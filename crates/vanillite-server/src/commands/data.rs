//! What `data`, `execute store`/`if data` and `function ... with` read and write
//! (`DataAccessor`: `EntityDataAccessor`, `BlockDataAccessor`, `StorageDataAccessor`).

use vanillite_base::{BlockPos, Vec3};
use vanillite_command::coords::Coordinates;
use vanillite_command::error::CommandError;
use vanillite_command::nbt_path::NbtPath;
use vanillite_command::{Arg as ErrArg, ArgValue, Args};
use vanillite_data::entity::ids;
use vanillite_game::entities::Entities;
use vanillite_nbt::{Compound, List, Tag};
use vanillite_world::entity::EntityId;

use super::source::CommandSource;
use super::{Env, nbt_ops, select};

/// Where data lives.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum DataTarget {
    /// An entity of the command's world.
    Entity { uuid: u128, id: EntityId },
    /// A block entity.
    Block(BlockPos),
    /// Command storage.
    Storage(String),
}

/// `NbtTagArgument`-style type names (`Tag.getType().getName()`).
pub(crate) fn type_name(tag: &Tag) -> &'static str {
    match tag {
        Tag::Byte(_) => "BYTE",
        Tag::Short(_) => "SHORT",
        Tag::Int(_) => "INT",
        Tag::Long(_) => "LONG",
        Tag::Float(_) => "FLOAT",
        Tag::Double(_) => "DOUBLE",
        Tag::ByteArray(_) => "BYTE[]",
        Tag::String(_) => "STRING",
        Tag::List(_) => "LIST",
        Tag::Compound(_) => "COMPOUND",
        Tag::IntArray(_) => "INT[]",
        Tag::LongArray(_) => "LONG[]",
    }
}

/// The target named `name` in `args`: a single entity, a block position or a storage id.
pub(crate) fn target(
    args: &Args,
    name: &str,
    source: &CommandSource,
    env: &Env<'_>,
) -> Result<DataTarget, CommandError> {
    match args.get(name) {
        Some(ArgValue::Entity(selector)) => {
            let t = select::single(env, source, selector)?;
            let id = t
                .entity
                .ok_or_else(|| CommandError::new("argument.entity.notfound.entity", vec![]))?;
            Ok(DataTarget::Entity { uuid: t.uuid, id })
        }
        Some(ArgValue::Coordinates(c)) => {
            // `BlockPosArgument.getLoadedBlockPos`, then the block entity.
            let pos = block_pos(source, c);
            if !env.level.in_height(pos.y) {
                return Err(CommandError::new("argument.pos.outofworld", vec![]));
            }
            if source.world != env.world || !env.level.is_loaded(pos) {
                return Err(CommandError::new("argument.pos.unloaded", vec![]));
            }
            if env.level.block(pos).block().info().block_entity == u16::MAX {
                return Err(CommandError::new("commands.data.block.invalid", vec![]));
            }
            Ok(DataTarget::Block(pos))
        }
        Some(ArgValue::Identifier(id)) => Ok(DataTarget::Storage(id.clone())),
        _ => Err(CommandError::new("command.exception", vec![])),
    }
}

/// The block position of block coordinates for a source.
pub(crate) fn block_pos(source: &CommandSource, c: &Coordinates) -> BlockPos {
    let p = super::exec::resolve_position(source, c);
    BlockPos::new(floor(p.x), floor(p.y), floor(p.z))
}

fn floor(v: f64) -> i32 {
    let f = v.floor();
    if f >= f64::from(i32::MAX) {
        i32::MAX
    } else if f <= f64::from(i32::MIN) {
        i32::MIN
    } else {
        // In range after the checks above.
        format!("{f:.0}").parse().unwrap_or(0)
    }
}

/// `DataAccessor.getData`.
pub(crate) fn read(target: &DataTarget, env: &Env<'_>) -> Result<Compound, CommandError> {
    match target {
        DataTarget::Storage(id) => Ok(env
            .global
            .systems
            .storage()
            .get(id)
            .cloned()
            .unwrap_or_default()),
        DataTarget::Entity { id, .. } => entity_nbt(&env.level.entities, *id)
            .ok_or_else(|| CommandError::new("argument.entity.notfound.entity", vec![])),
        DataTarget::Block(pos) => env
            .level
            .block_entity_nbt(*pos)
            .ok_or_else(|| CommandError::new("commands.data.block.invalid", vec![])),
    }
}

/// `DataAccessor.setData`.
pub(crate) fn write(
    target: &DataTarget,
    env: &mut Env<'_>,
    data: Compound,
) -> Result<(), CommandError> {
    match target {
        DataTarget::Storage(id) => {
            env.global.systems.storage().set(id.clone(), data);
            Ok(())
        }
        DataTarget::Entity { id, .. } => apply_entity_nbt(&mut env.level.entities, *id, &data),
        DataTarget::Block(pos) => {
            if env.level.load_block_entity_nbt(*pos, &data) {
                Ok(())
            } else {
                Err(CommandError::new("commands.data.block.invalid", vec![]))
            }
        }
    }
}

/// The compound of the `source` argument of `function ... with`.
pub(crate) fn read_source(
    args: &Args,
    source: &CommandSource,
    env: &Env<'_>,
) -> Result<Compound, CommandError> {
    let t = target(args, "source", source, env)?;
    read(&t, env)
}

/// `DataCommands.getSingleTag`.
pub(crate) fn single_tag(path: &NbtPath, root: &Tag) -> Result<Tag, CommandError> {
    let mut found = nbt_ops::get(path, root)?;
    if found.len() != 1 {
        return Err(CommandError::new("commands.data.get.multiple", vec![]));
    }
    found
        .pop()
        .ok_or_else(|| CommandError::new("commands.data.get.multiple", vec![]))
}

/// A UUID as Vanilla's int array.
fn uuid_tag(uuid: u128) -> Tag {
    Tag::IntArray(vanillite_nbt::snbt::uuid_to_ints(uuid).to_vec())
}

fn vec3_list(v: Vec3) -> Tag {
    Tag::List(List(vec![
        Tag::Double(v.x),
        Tag::Double(v.y),
        Tag::Double(v.z),
    ]))
}

/// `Entity.saveWithoutId` for the data the server keeps for every entity, with the kind's own
/// data for players and items.
pub(crate) fn entity_nbt(entities: &Entities, id: EntityId) -> Option<Compound> {
    let e = entities.store.get(id)?;
    // Decorations, mobs (armor stands and mannequins too) and minecarts with all their
    // stored data.
    let full = vanillite_game::decoration::save(entities, id)
        .or_else(|| {
            entities
                .mob(id)
                .map(|m| vanillite_game::mob::nbt::save_mob(&e, m))
        })
        .or_else(|| {
            entities
                .minecart(id)
                .map(|m| vanillite_game::mob::nbt::save_minecart(&e, m))
        });
    if let Some(mut full) = full {
        full.remove("id");
        let tags = entities.tags_of(id);
        if !tags.is_empty() {
            full.insert(
                "Tags",
                Tag::List(List(tags.iter().map(|t| Tag::String(t.clone())).collect())),
            );
        }
        if let Some(data) = entities.custom_data(id) {
            full.insert("data", data.clone());
        }
        return Some(full);
    }
    let mut out = Compound::new();
    out.insert("Pos", vec3_list(e.pos()));
    out.insert("Motion", vec3_list(e.velocity()));
    let rot = e.rot();
    out.insert(
        "Rotation",
        Tag::List(List(vec![Tag::Float(rot.y_rot), Tag::Float(rot.x_rot)])),
    );
    out.insert("fall_distance", Tag::Double(0.0));
    out.insert("Fire", Tag::Short(-1));
    out.insert("Air", Tag::Short(300));
    out.insert(
        "OnGround",
        Tag::Byte(i8::from(
            e.flags()
                .contains(vanillite_world::entity::EntityFlags::ON_GROUND),
        )),
    );
    out.insert("Invulnerable", Tag::Byte(0));
    out.insert("PortalCooldown", Tag::Int(0));
    out.insert("UUID", uuid_tag(e.uuid()));
    let tags = entities.tags_of(id);
    if !tags.is_empty() {
        out.insert(
            "Tags",
            Tag::List(List(tags.iter().map(|t| Tag::String(t.clone())).collect())),
        );
    }
    if e.kind() == ids::PLAYER
        && let Some(p) = entities.player(id)
    {
        p.survival.save(&mut out);
        p.inventory.save(&mut out);
        out.insert("playerGameType", Tag::Int(game_type(p.game_mode)));
        if let Some(prev) = p.previous_game_mode {
            out.insert("previousPlayerGameType", Tag::Int(game_type(prev)));
        }
        // `ServerPlayer.addAdditionalSaveData`'s other live state.
        out.insert(
            "FallFlying",
            Tag::Byte(i8::from(
                e.flags()
                    .contains(vanillite_world::entity::EntityFlags::FALL_FLYING),
            )),
        );
        out.insert(
            "abilities",
            Tag::Compound(crate::players::abilities_nbt(
                p.game_mode,
                p.survival.flying,
            )),
        );
        out.insert("recipeBook", p.recipe_book.save());
        out.insert(
            "post_effects",
            Tag::List(List(
                p.post_effects
                    .iter()
                    .map(|e| Tag::String(e.clone()))
                    .collect(),
            )),
        );
        // `NbtPredicate.getEntityTagToCompare`: the selected item of a player.
        let selected = p.inventory.selected_item();
        if !selected.is_empty() {
            out.insert(
                "SelectedItem",
                Tag::Compound(vanillite_game::inventory::stack_to_nbt(selected)),
            );
        }
    } else if e.kind() == ids::ITEM
        && let Some(item) = entities.item(id)
    {
        out.insert(
            "Item",
            Tag::Compound(vanillite_game::inventory::stack_to_nbt(&item.stack)),
        );
        out.insert("Age", Tag::Short(item.age));
        out.insert("PickupDelay", Tag::Short(item.pickup_delay));
        if let Some(owner) = item.target {
            out.insert("Owner", uuid_tag(owner));
        }
        if let Some(thrower) = item.thrower {
            out.insert("Thrower", uuid_tag(thrower));
        }
    }
    Some(out)
}

fn game_type(mode: vanillite_config::GameMode) -> i32 {
    match mode {
        vanillite_config::GameMode::Survival => 0,
        vanillite_config::GameMode::Creative => 1,
        vanillite_config::GameMode::Adventure => 2,
        vanillite_config::GameMode::Spectator => 3,
    }
}

fn read_vec3(tag: Option<&Tag>) -> Option<Vec3> {
    match tag? {
        Tag::List(List(v)) => match v.as_slice() {
            [Tag::Double(x), Tag::Double(y), Tag::Double(z)] => Some(Vec3::new(*x, *y, *z)),
            _ => None,
        },
        _ => None,
    }
}

/// `Entity.load` of what the server keeps: position, motion, rotation and tags. Players
/// cannot be changed (`commands.data.entity.invalid`).
pub(crate) fn apply_entity_nbt(
    entities: &mut Entities,
    id: EntityId,
    data: &Compound,
) -> Result<(), CommandError> {
    let Some(e) = entities.store.get(id) else {
        return Err(CommandError::new("argument.entity.notfound.entity", vec![]));
    };
    if e.kind() == ids::PLAYER {
        return Err(CommandError::new("commands.data.entity.invalid", vec![]));
    }
    let tags: Vec<String> = match data.get("Tags") {
        Some(Tag::List(List(l))) => l
            .iter()
            .filter_map(|t| t.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    };
    entities.set_tags(id, tags);
    // `Entity.load`: the custom data (`data`).
    if let Some(Tag::Compound(c)) = data.get("data") {
        entities.set_custom_data(id, Some(c.clone()));
    }
    if vanillite_game::decoration::apply_nbt(entities, id, data) {
        return Ok(());
    }
    // `readAdditionalSaveData` of mobs and minecarts.
    let (game_time, day_time) = (entities.game_time, entities.day_time);
    if let Some(m) = entities.mob_mut(id) {
        vanillite_game::mob::nbt::apply_mob(m, data, game_time, day_time);
    } else if let Some(m) = entities.minecart_mut(id) {
        vanillite_game::mob::nbt::apply_minecart(m, data);
    }
    // `Entity.load`: head and body face the yaw.
    let yaw = match data.get("Rotation") {
        Some(Tag::List(List(r))) => match r.as_slice() {
            [Tag::Float(y), Tag::Float(_)] => Some(*y),
            _ => None,
        },
        _ => None,
    };
    if let Some(yaw) = yaw
        && let Some(m) = entities.mob_mut(id)
    {
        m.y_body_rot = yaw;
    }
    if let Some(mut e) = entities.store.get_mut(id) {
        if let Some(pos) = read_vec3(data.get("Pos"))
            .filter(|p| p.x.is_finite() && p.y.is_finite() && p.z.is_finite())
        {
            e.set_pos(pos);
        }
        if let Some(motion) = read_vec3(data.get("Motion")) {
            let clamp = |v: f64| if v.abs() > 10.0 { 0.0 } else { v };
            e.set_velocity(Vec3::new(clamp(motion.x), clamp(motion.y), clamp(motion.z)));
        }
        if let Some(Tag::List(List(r))) = data.get("Rotation")
            && let [Tag::Float(y), Tag::Float(x)] = r.as_slice()
        {
            let mut rot = e.as_ref().rot();
            rot.y_rot = *y;
            rot.x_rot = *x;
            rot.y_head_rot = *y;
            e.set_rot(rot);
        }
        if let Some(v) = data.get("NoGravity").and_then(Tag::as_i64) {
            e.set_flags(vanillite_world::entity::EntityFlags::NO_GRAVITY, v != 0);
        }
        e.mark_data_dirty();
    }
    if e_kind_is_item(entities, id)
        && let Some(item) = entities.item_mut(id)
    {
        if let Some(age) = data.get("Age").and_then(Tag::as_i64) {
            item.age = i16::try_from(age).unwrap_or(item.age);
        }
        if let Some(delay) = data.get("PickupDelay").and_then(Tag::as_i64) {
            item.pickup_delay = i16::try_from(delay).unwrap_or(item.pickup_delay);
        }
        if let Some(Tag::Compound(c)) = data.get("Item") {
            item.stack = vanillite_game::inventory::stack_from_nbt(c);
        }
        if let Some(v) = data.get("Invulnerable").and_then(Tag::as_i64) {
            item.invulnerable = v != 0;
        }
    }
    Ok(())
}

fn e_kind_is_item(entities: &Entities, id: EntityId) -> bool {
    entities
        .store
        .get(id)
        .is_some_and(|e| e.kind() == ids::ITEM)
}

/// Writes a number at a path of a target (`execute store ... <path> <type> <scale>`).
pub(crate) fn store_number(
    target: &DataTarget,
    path: &NbtPath,
    tag: Tag,
    env: &mut Env<'_>,
) -> Result<(), CommandError> {
    let data = read(target, env)?;
    let mut root = Tag::Compound(data);
    nbt_ops::set(path, &mut root, &tag)?;
    if let Tag::Compound(c) = root {
        write(target, env, c)?;
    }
    Ok(())
}

/// The error of a value that is no number (`commands.data.get.invalid`).
pub(crate) fn not_numeric(path: &NbtPath) -> CommandError {
    CommandError::new(
        "commands.data.get.invalid",
        vec![ErrArg::Str(path.text.clone())],
    )
}
