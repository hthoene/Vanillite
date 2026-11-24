//! `/give`, `/clear`, `/item` and `/enchant` (Vanilla's `GiveCommand`,
//! `ClearInventoryCommands`, `ItemCommands` with its `ItemAccessor`s, `EnchantCommand`), and
//! item stacks from command arguments (`ItemInput`).
//!
//! Players in other worlds than the command's are changed on their world between ticks where
//! the outcome does not depend on their state (`give`); commands that count what they change
//! (`clear`, `item`, `enchant`) see the entities of the command's world only.

use vanillite_base::Vec3;
use vanillite_base::random::RandomSource as _;
use vanillite_command::error::CommandError;
use vanillite_command::{
    Arg as ErrArg, ArgValue, Args, GameValue, ItemArg, ItemPredicateArg, ResourceOrTag,
    SlotSourceArg,
};
use vanillite_data::entity::ids as entity_ids;
use vanillite_data::item::{ItemId, Rarity};
use vanillite_data::registry::{BLOCK, DATA_COMPONENT_TYPE, ITEM};
use vanillite_game::commands::ItemOp;
use vanillite_game::entities::Entities;
use vanillite_game::inventory::OFFHAND;
use vanillite_game::item::{self, EquipmentSlot, ItemStack};
use vanillite_game::level::Level;
use vanillite_nbt::{Compound, Tag};
use vanillite_protocol::codec::{DecodeLimited as _, Encode as _, Reader, write_varint};
use vanillite_protocol::components::ids as component_ids;
use vanillite_protocol::text::{Arg, Color, Component, HoverEvent, NamedColor, OpaqueNbt};
use vanillite_world::entity::EntityId;

use super::Env;
use super::source::CommandSource;
use super::tracker::Tracker;
use super::{dispatch, execute, select};
use select::Target;

fn missing(name: &str) -> CommandError {
    CommandError::new(
        "command.exception",
        vec![ErrArg::Str(format!("missing argument {name}"))],
    )
}

fn error(key: &'static str, args: Vec<ErrArg>) -> CommandError {
    CommandError::new(key, args)
}

/// What a system that does not exist yet answers (loot item modifiers, slot sources, item
/// component types without an encoder).
fn unsupported() -> CommandError {
    error("command.failed", vec![])
}

fn count_int(n: usize) -> i32 {
    i32::try_from(n).unwrap_or(i32::MAX)
}

fn targets(
    args: &Args,
    name: &str,
    source: &CommandSource,
    env: &Env<'_>,
) -> Result<Vec<Target>, CommandError> {
    let selector = args.selector(name).ok_or_else(|| missing(name))?;
    select::many(env, source, selector)
}

/// Runs `f` on a player's level and entity: now when it is in this world, otherwise on its
/// world between ticks. Returns whether it ran now.
pub(crate) fn on_player(
    env: &mut Env<'_>,
    target: &Target,
    f: impl FnOnce(&mut Level, EntityId) + Send + 'static,
) -> bool {
    if let Some(id) = target.entity {
        f(env.level, id);
        return true;
    }
    let uuid = target.uuid;
    env.global.run_in_world(target.world, move |world| {
        let level = world.level_mut();
        if let Some(id) = select::find_uuid(&level.entities, uuid) {
            f(level, id);
        }
    });
    false
}

/// `containerMenu.broadcastChanges` of a player.
fn sync_inventory(entities: &mut Entities, id: EntityId) {
    if let Some(player) = entities.player_mut(id) {
        let sink = std::sync::Arc::clone(&player.connection.sink);
        player.inventory.broadcast_changes(sink.as_ref());
    }
}

// --- Item stacks ------------------------------------------------------------------------------

/// The translation key of an item's default name (`Item.getDescriptionId`).
fn item_name_key(item: u16) -> String {
    let info = ItemId(item).info();
    let path = info.name.rsplit(':').next().unwrap_or("");
    let block_named = (info.block != u16::MAX && BLOCK.name(info.block) == Some(info.name))
        && !matches!(info.name, "minecraft:resin_clump" | "minecraft:nether_wart")
        || info.name == "minecraft:air";
    if block_named {
        format!("block.minecraft.{path}")
    } else {
        format!("item.minecraft.{path}")
    }
}

/// `ItemStack.getHoverName`: the custom name or the item's name.
fn hover_name(stack: &ItemStack) -> (Component, bool) {
    if let Some(bytes) = stack.component(component_ids::CUSTOM_NAME)
        && let Ok(name) = Component::decode_limited(&mut Reader::new(bytes), bytes.len())
    {
        return (name, true);
    }
    (
        Component::translatable(item_name_key(stack.item), vec![]),
        false,
    )
}

/// `ItemStack.getRarity`: enchanted items are one step rarer.
fn rarity_color(stack: &ItemStack) -> NamedColor {
    let enchanted = !item::enchantments(stack).is_empty();
    match (ItemId(stack.item).info().rarity, enchanted) {
        (Rarity::Common, false) => NamedColor::White,
        (Rarity::Uncommon, false) => NamedColor::Yellow,
        (Rarity::Common | Rarity::Uncommon | Rarity::Rare, true) | (Rarity::Rare, false) => {
            NamedColor::Aqua
        }
        (Rarity::Epic, _) => NamedColor::LightPurple,
    }
}

/// `ItemStack.getDisplayName`: the name in brackets in the rarity colour, showing the item on
/// hover; `components` is the component patch as NBT when known.
pub(crate) fn item_display(stack: &ItemStack, components: Option<&Compound>) -> Component {
    let (name, custom) = hover_name(stack);
    let mut name = Component::empty().append(name);
    if custom {
        name = name.italic(true);
    }
    let mut out = Component::translatable("chat.square_brackets", vec![name]);
    if !stack.is_empty() {
        out = out
            .color(Color::Named(rarity_color(stack)))
            .hover(HoverEvent::ShowItem {
                id: ITEM.name(stack.item).unwrap_or("minecraft:air").to_owned(),
                count: stack.count,
                components: components.map(|c| OpaqueNbt(Tag::Compound(c.clone()))),
            });
    }
    out
}

fn qualify(id: &str) -> String {
    if id.contains(':') {
        id.to_owned()
    } else {
        format!("minecraft:{id}")
    }
}

/// The wire value of a component written in an item argument. Only the common component
/// types have an encoder; the others fail with [`unsupported`].
fn encode_component(name: &str, value: &Tag, env: &Env<'_>) -> Result<Vec<u8>, CommandError> {
    let malformed = || {
        error(
            "arguments.item.component.malformed",
            vec![
                ErrArg::Str(name.to_owned()),
                ErrArg::Str(vanillite_nbt::snbt::to_string(value)),
            ],
        )
    };
    let int = |tag: &Tag, min: i32, max: i32| {
        tag.as_i64()
            .and_then(|v| i32::try_from(v).ok())
            .filter(|v| (min..=max).contains(v))
            .ok_or_else(malformed)
    };
    let component = |tag: &Tag| Component::try_from_nbt(tag).map_err(|_| malformed());
    let mut out = Vec::new();
    match name {
        "minecraft:max_stack_size" => write_varint(&mut out, int(value, 1, 99)?),
        "minecraft:max_damage" => write_varint(&mut out, int(value, 1, i32::MAX)?),
        "minecraft:damage" | "minecraft:repair_cost" => {
            write_varint(&mut out, int(value, 0, i32::MAX)?);
        }
        "minecraft:unbreakable" => {
            if !matches!(value, Tag::Compound(c) if c.is_empty()) {
                return Err(malformed());
            }
        }
        "minecraft:enchantment_glint_override" => out.push(u8::from(int(value, 0, 1)? == 1)),
        "minecraft:custom_name" | "minecraft:item_name" => component(value)?.encode(&mut out),
        "minecraft:lore" => {
            let Tag::List(lines) = value else {
                return Err(malformed());
            };
            if lines.0.len() > 256 {
                return Err(malformed());
            }
            write_varint(&mut out, count_int(lines.0.len()));
            for line in &lines.0 {
                component(line)?.encode(&mut out);
            }
        }
        "minecraft:enchantments" | "minecraft:stored_enchantments" => {
            let Tag::Compound(levels) = value else {
                return Err(malformed());
            };
            write_varint(&mut out, count_int(levels.len()));
            for (enchantment, level) in levels.iter() {
                let id = env
                    .global
                    .game
                    .enchantments
                    .get(&qualify(enchantment))
                    .and_then(|id| i32::try_from(*id).ok())
                    .ok_or_else(malformed)?;
                write_varint(&mut out, id);
                write_varint(&mut out, int(level, 1, 255)?);
            }
        }
        "minecraft:rarity" => {
            let rarity = ["common", "uncommon", "rare", "epic"]
                .iter()
                .position(|r| value.as_str() == Some(*r))
                .ok_or_else(malformed)?;
            write_varint(&mut out, count_int(rarity));
        }
        "minecraft:custom_data" => {
            if !matches!(value, Tag::Compound(_)) {
                return Err(malformed());
            }
            vanillite_nbt::write::write_network(value, &mut out).map_err(|_| malformed())?;
        }
        _ => return Err(unsupported()),
    }
    let type_id = DATA_COMPONENT_TYPE.id(name).ok_or_else(malformed)?;
    vanillite_protocol::components::validate(
        type_id,
        &out,
        &vanillite_protocol::item::InstalledRegistries,
    )
    .map_err(|_| malformed())?;
    Ok(out)
}

/// An item argument (`ItemInput`): a stack of one with its components, and the components as
/// NBT for the hover text.
pub(crate) struct ItemInput {
    stack: ItemStack,
    components: Option<Compound>,
}

impl ItemInput {
    /// `ItemArgument.getItem`.
    pub(crate) fn of(args: &Args, name: &str, env: &Env<'_>) -> Result<ItemInput, CommandError> {
        let Some(GameValue::Item(arg)) = args.game(name) else {
            return Err(missing(name));
        };
        Self::from_arg(arg, env)
    }

    fn from_arg(arg: &ItemArg, env: &Env<'_>) -> Result<ItemInput, CommandError> {
        let item = ITEM.id(&arg.item).ok_or_else(|| {
            error(
                "argument.item.id.invalid",
                vec![ErrArg::Str(arg.item.clone())],
            )
        })?;
        let mut stack = ItemStack::new(item, 1);
        let mut nbt = Compound::new();
        for (component, value) in &arg.components {
            let type_id = DATA_COMPONENT_TYPE.id(component).ok_or_else(unsupported)?;
            match value {
                Some(value) => {
                    let bytes = encode_component(component, value, env)?;
                    stack.patch_mut().set(type_id, bytes.into());
                    nbt.insert(component.clone(), value.clone());
                }
                None => {
                    let patch = stack.patch_mut();
                    patch.unset(type_id);
                    if let Err(i) = patch.removed.binary_search(&type_id) {
                        patch.removed.insert(i, type_id);
                    }
                    nbt.insert(format!("!{component}"), Compound::new());
                }
            }
        }
        Ok(ItemInput {
            stack,
            components: (!nbt.is_empty()).then_some(nbt),
        })
    }

    /// `createItemStack(count)`.
    fn create(&self, count: i32) -> Result<ItemStack, CommandError> {
        let max = item::max_stack_size(&self.stack);
        if count > max {
            return Err(error(
                "arguments.item.overstacked",
                vec![
                    ErrArg::Str(ITEM.name(self.stack.item).unwrap_or("").to_owned()),
                    ErrArg::Int(max),
                ],
            ));
        }
        Ok(self.stack.with_count(count))
    }

    fn display(&self, stack: &ItemStack) -> Component {
        item_display(stack, self.components.as_ref())
    }
}

// --- give ---------------------------------------------------------------------------------------

/// `LivingEntity.createItemStackToDrop(stack, false, false)` with `setNoPickUpDelay` and the
/// player as the only one to pick it up (`GiveCommand`).
fn drop_for(level: &mut Level, id: EntityId, stack: ItemStack) {
    let Some((pos, rot, uuid)) = level
        .entities
        .store
        .get(id)
        .map(|e| (e.pos(), e.rot(), e.uuid()))
    else {
        return;
    };
    let eye = f64::from(entity_ids::PLAYER.info().eye_height);
    let deg = std::f64::consts::PI / 180.0;
    let (sin_x, cos_x) = (
        vanillite_base::math::sin(f64::from(rot.x_rot) * deg),
        vanillite_base::math::cos(f64::from(rot.x_rot) * deg),
    );
    let (sin_y, cos_y) = (
        vanillite_base::math::sin(f64::from(rot.y_rot) * deg),
        vanillite_base::math::cos(f64::from(rot.y_rot) * deg),
    );
    let random = level.random();
    let dir = random.next_float() * std::f32::consts::TAU;
    let pow2 = 0.02 * random.next_float();
    let spread = random.next_float() - random.next_float();
    let velocity = Vec3::new(
        f64::from(-sin_y * cos_x * 0.3) + f64::from(dir).cos() * f64::from(pow2),
        f64::from(-sin_x * 0.3 + 0.1 + spread * 0.1),
        f64::from(cos_y * cos_x * 0.3) + f64::from(dir).sin() * f64::from(pow2),
    );
    let at = Vec3::new(pos.x, pos.y + eye - 0.3, pos.z);
    if let Some(item) =
        vanillite_game::block::drops::spawn_item(level, at, stack, velocity, 0, None)
        && let Some(data) = level.entities.item_mut(item)
    {
        data.target = Some(uuid);
    }
}

/// `GiveCommand.giveItem` for one player: stacks into the inventory, the rest dropped.
fn give_to(level: &mut Level, id: EntityId, prototype: &ItemStack, count: i32, max: i32) {
    let mut remaining = count;
    while remaining > 0 {
        let size = max.min(remaining).max(1);
        remaining = remaining.saturating_sub(size);
        let mut stack = prototype.with_count(size);
        let Some(player) = level.entities.player_mut(id) else {
            return;
        };
        let infinite = player.game_mode == vanillite_config::GameMode::Creative;
        let added = player.inventory.add(None, &mut stack, infinite);
        if !(added && stack.is_empty()) {
            drop_for(level, id, stack);
        }
    }
    sync_inventory(&mut level.entities, id);
}

/// `/give <targets> <item> [<count>]`.
pub(crate) fn give(
    args: &Args,
    source: &CommandSource,
    env: &mut Env<'_>,
) -> Result<i32, CommandError> {
    let players = targets(args, "targets", source, env)?;
    let input = ItemInput::of(args, "item", env)?;
    let count = args.int("count").copied().unwrap_or(1);
    let prototype = input.create(1)?;
    let display = input.display(&prototype);
    let max = item::max_stack_size(&prototype);
    let allowed = max.saturating_mul(100);
    if count > allowed {
        return Err(error(
            "commands.give.failed.toomanyitems",
            vec![ErrArg::Int(allowed), ErrArg::Component(Box::new(display))],
        ));
    }
    let mut tracker = Tracker::<Component>::default();
    for player in &players {
        let stack = prototype.clone();
        on_player(env, player, move |level, id| {
            give_to(level, id, &stack, count, max)
        });
        tracker.track(&player.display(), 1);
    }
    tracker.feedback(source, env, true, false, None, |one, n, _| match one {
        Some(player) => Component::translatable_args(
            "commands.give.success.single",
            vec![
                Arg::Int(count),
                Arg::from(display),
                Arg::from(player.clone()),
            ],
        ),
        None => Component::translatable_args(
            "commands.give.success.multiple",
            vec![Arg::Int(count), Arg::from(display), Arg::Int(n)],
        ),
    })
}

// --- clear --------------------------------------------------------------------------------------

/// The items an item predicate accepts (`None`: all). Predicates with component tests are
/// not supported.
fn accepted_items(args: &Args, env: &Env<'_>) -> Result<Option<Vec<u16>>, CommandError> {
    let Some(GameValue::ItemPredicate(predicate)) = args.game("item") else {
        return Ok(None);
    };
    let ItemPredicateArg { item, tests } = predicate.as_ref();
    if !tests.is_empty() {
        return Err(unsupported());
    }
    Ok(match item {
        None => None,
        Some(ResourceOrTag::Resource(id)) => Some(ITEM.id(id).into_iter().collect()),
        Some(ResourceOrTag::Tag(tag)) => Some(
            env.global
                .systems
                .tags()
                .get("minecraft:item")
                .and_then(|t| t.get(tag))
                .map(|entries| entries.iter().filter_map(|e| ITEM.id(e)).collect())
                .unwrap_or_default(),
        ),
    })
}

/// `Inventory.clearOrCountMatchingItems`: the inventory, the crafting grid and the carried
/// stack; `max` < 0 removes all.
fn clear_player(
    entities: &mut Entities,
    id: EntityId,
    accepted: Option<&[u16]>,
    counting: bool,
    max: i32,
) -> i32 {
    let Some(player) = entities.player_mut(id) else {
        return 0;
    };
    let mut count = 0i32;
    let mut take = |stack: &mut ItemStack| {
        if stack.is_empty() || accepted.is_some_and(|a| !a.contains(&stack.item)) {
            return;
        }
        if counting {
            count = count.saturating_add(stack.count);
            return;
        }
        let left = max.saturating_sub(count);
        let removed = if left < 0 {
            stack.count
        } else {
            left.min(stack.count)
        };
        stack.shrink(removed);
        count = count.saturating_add(removed);
    };
    let inventory = &mut player.inventory;
    for index in 0..=OFFHAND {
        if let Some(stack) = inventory.get_mut(index) {
            take(stack);
        }
    }
    inventory.craft.iter_mut().for_each(&mut take);
    take(&mut inventory.carried);
    if !counting {
        sync_inventory(entities, id);
    }
    count
}

/// `/clear [<targets> [<item> [<maxCount>]]]`.
pub(crate) fn clear(
    args: &Args,
    source: &CommandSource,
    env: &mut Env<'_>,
) -> Result<i32, CommandError> {
    let players = if args.has("targets") {
        targets(args, "targets", source, env)?
    } else {
        if source.uuid().is_none() {
            return Err(error("permissions.requires.player", vec![]));
        }
        vec![dispatch::self_target(env, source)?]
    };
    let accepted = accepted_items(args, env)?;
    let max = args.int("maxCount").copied().unwrap_or(-1);
    let counting = max == 0;
    let mut tracker = Tracker::<Component>::default();
    for player in &players {
        let count = match player.entity {
            Some(id) => clear_player(
                &mut env.level.entities,
                id,
                accepted.as_deref(),
                counting,
                max,
            ),
            None => {
                if !counting {
                    let accepted = accepted.clone();
                    on_player(env, player, move |level, id| {
                        clear_player(&mut level.entities, id, accepted.as_deref(), false, max);
                    });
                }
                0
            }
        };
        tracker.track(&player.display(), count);
    }
    if tracker.total() == 0 {
        return Err(match tracker.pick(true) {
            (Some(player), _) => error(
                "clear.failed.single",
                vec![ErrArg::Component(Box::new(player.clone()))],
            ),
            (None, n) => error("clear.failed.multiple", vec![ErrArg::Int(n)]),
        });
    }
    let key = if counting { "test" } else { "success" };
    tracker.feedback(source, env, true, false, None, |one, n, total| match one {
        Some(player) => Component::translatable_args(
            format!("commands.clear.{key}.single"),
            vec![Arg::Int(total), Arg::from(player.clone())],
        ),
        None => Component::translatable_args(
            format!("commands.clear.{key}.multiple"),
            vec![Arg::Int(total), Arg::Int(n)],
        ),
    })
}

// --- item ---------------------------------------------------------------------------------------

/// A slot of an entity (`SlotProvider.getSlot`): player inventory indices 0–40, the cursor,
/// the crafting grid, or the stack of an item entity.
#[derive(Clone, Copy, Debug)]
enum EntitySlot {
    Inventory(usize),
    Cursor,
    Craft(usize),
    ItemEntity,
}

/// The slot `index` of an entity, if it has it. Mobs keep no equipment yet.
fn entity_slot(entities: &Entities, id: EntityId, index: i32) -> Option<EntitySlot> {
    let kind = entities.store.get(id)?.kind();
    if kind == entity_ids::ITEM {
        return (index == 0).then_some(EntitySlot::ItemEntity);
    }
    let player = entities.player(id)?;
    let at = |base: i32| usize::try_from(index.saturating_sub(base)).ok();
    Some(match index {
        0..=35 => EntitySlot::Inventory(at(0)?),
        98 => EntitySlot::Inventory(player.inventory.selected),
        99 => EntitySlot::Inventory(OFFHAND),
        100..=103 => EntitySlot::Inventory(at(100)?.saturating_add(36)),
        499 => EntitySlot::Cursor,
        500..=503 => EntitySlot::Craft(at(500)?),
        _ => return None,
    })
}

fn slot_get(entities: &Entities, id: EntityId, slot: EntitySlot) -> ItemStack {
    let stack = match slot {
        EntitySlot::ItemEntity => entities.item(id).map(|i| &i.stack),
        EntitySlot::Inventory(i) => entities.player(id).map(|p| p.inventory.get(i)),
        EntitySlot::Cursor => entities.player(id).map(|p| &p.inventory.carried),
        EntitySlot::Craft(i) => entities.player(id).and_then(|p| p.inventory.craft.get(i)),
    };
    stack.cloned().unwrap_or_default()
}

/// `SlotAccess.set`: armour slots below the head take only their own armour.
fn slot_set(entities: &mut Entities, id: EntityId, slot: EntitySlot, stack: ItemStack) -> bool {
    if let EntitySlot::ItemEntity = slot {
        return entities.item_mut(id).is_some_and(|i| {
            i.stack = stack;
            true
        });
    }
    let Some(player) = entities.player_mut(id) else {
        return false;
    };
    let inventory = &mut player.inventory;
    let target = match slot {
        EntitySlot::Inventory(i @ 36..=38) => {
            let wanted = match i {
                36 => EquipmentSlot::Feet,
                37 => EquipmentSlot::Legs,
                _ => EquipmentSlot::Chest,
            };
            if !stack.is_empty() && item::equipment_slot(&stack) != wanted {
                return false;
            }
            inventory.get_mut(i)
        }
        EntitySlot::Inventory(i) => inventory.get_mut(i),
        EntitySlot::Cursor => Some(&mut inventory.carried),
        EntitySlot::Craft(i) => inventory.craft.get_mut(i),
        EntitySlot::ItemEntity => None,
    };
    target.is_some_and(|t| {
        *t = stack;
        true
    })
}

/// The slot range of a slot source argument: its name and indices. Slot sources from the
/// registry or inline need the loot context and are not supported.
fn slot_range(args: &Args, name: &str) -> Result<(String, Vec<i32>), CommandError> {
    match args.game(name) {
        Some(GameValue::SlotSource(SlotSourceArg::Range(range, slots))) => {
            Ok((range.clone(), slots.clone()))
        }
        Some(GameValue::SlotSource(SlotSourceArg::Source(_))) => Err(unsupported()),
        _ => Err(missing(name)),
    }
}

/// The entities of an item accessor; a block must be loaded and is never a container (block
/// entities have no inventories yet).
fn item_entities(
    args: &Args,
    name: &str,
    source: &CommandSource,
    env: &Env<'_>,
    target: bool,
) -> Result<Vec<Target>, CommandError> {
    if let Some(ArgValue::Coordinates(_)) = args.get(name) {
        let pos = execute::loaded_block_pos(args, name, source, env)?;
        let key = if target {
            "commands.item.target.not_a_container"
        } else {
            "commands.item.source.not_a_container"
        };
        return Err(error(
            key,
            vec![ErrArg::Int(pos.x), ErrArg::Int(pos.y), ErrArg::Int(pos.z)],
        ));
    }
    targets(args, name, source, env)
}

/// `ItemProvider` of the distributions: `of`, `cycle`, `of(...).orElseProvide(EMPTY)`.
struct Provider {
    items: Vec<ItemStack>,
    index: usize,
    op: ItemOp,
}

impl Provider {
    fn has_next(&self) -> bool {
        match self.op {
            ItemOp::Fill => !self.items.is_empty(),
            ItemOp::Override => true,
            _ => self.index < self.items.len(),
        }
    }

    fn next(&mut self) -> ItemStack {
        let i = match self.op {
            ItemOp::Fill => self.index.checked_rem(self.items.len()).unwrap_or(0),
            _ => self.index,
        };
        self.index = self.index.saturating_add(1);
        self.items.get(i).cloned().unwrap_or_default()
    }
}

/// `/item (replace|fill|override|modify) ...`.
pub(crate) fn item(
    op: ItemOp,
    args: &Args,
    source: &CommandSource,
    env: &mut Env<'_>,
) -> Result<i32, CommandError> {
    let targets = item_entities(args, "target", source, env, true)?;
    let (slots_name, slots) = slot_range(args, "slots")?;
    if op == ItemOp::Modify {
        // Loot item modifiers do not exist yet.
        return Err(unsupported());
    }
    let (items, known) = if args.has("item") {
        let input = ItemInput::of(args, "item", env)?;
        let stack = input.create(args.int("count").copied().unwrap_or(1))?;
        let display = input.display(&stack);
        (vec![stack], Some(display))
    } else {
        let sources = item_entities(args, "source", source, env, false)?;
        let (source_name, source_slots) = slot_range(args, "sourceSlots")?;
        let entities = &env.level.entities;
        let items: Vec<ItemStack> = sources
            .iter()
            .filter_map(|s| s.entity)
            .flat_map(|id| {
                source_slots
                    .iter()
                    .filter_map(move |&i| entity_slot(entities, id, i))
                    .map(move |slot| slot_get(entities, id, slot))
            })
            .collect();
        if items.is_empty() {
            return Err(error(
                "commands.item.source.no_such_slot",
                vec![ErrArg::Str(source_name)],
            ));
        }
        if args.has("modifier") {
            return Err(unsupported());
        }
        (items, None)
    };
    let mut provider = Provider {
        items,
        index: 0,
        op,
    };
    let mut selected = 0usize;
    let mut tracker = Tracker::<Component>::default();
    for target in &targets {
        let Some(id) = target.entity else {
            continue;
        };
        provider.index = 0;
        let entities = &mut env.level.entities;
        let here: Vec<EntitySlot> = slots
            .iter()
            .filter_map(|&i| entity_slot(entities, id, i))
            .collect();
        let mut changed = 0i32;
        for slot in here {
            if !provider.has_next() {
                break;
            }
            selected = selected.saturating_add(1);
            if slot_set(entities, id, slot, provider.next()) {
                changed = changed.saturating_add(1);
            }
        }
        if changed > 0 {
            sync_inventory(entities, id);
        }
        tracker.track(&target.display(), changed);
    }
    if selected == 0 {
        return Err(error(
            "commands.item.target.no_such_slot",
            vec![ErrArg::Str(slots_name)],
        ));
    }
    match known {
        Some(item) => tracker.feedback(
            source,
            env,
            true,
            false,
            Some(error(
                "commands.item.target.failed.known_item",
                vec![ErrArg::Component(Box::new(item.clone()))],
            )),
            |one, n, total| match one {
                Some(entity) => Component::translatable_args(
                    "commands.item.entity.replace.success.single.known_item",
                    vec![
                        Arg::Int(total),
                        Arg::from(entity.clone()),
                        Arg::from(item.clone()),
                    ],
                ),
                None => Component::translatable_args(
                    "commands.item.entity.replace.success.multiple.known_item",
                    vec![Arg::Int(n), Arg::from(item.clone())],
                ),
            },
        ),
        None => tracker.feedback(
            source,
            env,
            true,
            false,
            Some(error("commands.item.target.failed", vec![])),
            |one, n, total| match one {
                Some(entity) => Component::translatable_args(
                    "commands.item.entity.replace.success.single",
                    vec![Arg::Int(total), Arg::from(entity.clone())],
                ),
                None => Component::translatable_args(
                    "commands.item.entity.replace.success.multiple",
                    vec![Arg::Int(n)],
                ),
            },
        ),
    }
}

// --- enchant ------------------------------------------------------------------------------------

/// Enchantments of an `ItemEnchantments` wire value: `(id, level)`.
fn read_enchantments(data: &[u8]) -> Vec<(u32, i32)> {
    let mut r = Reader::new(data);
    let mut out = Vec::new();
    let Ok(n) = r.varint() else {
        return out;
    };
    for _ in 0..n.clamp(0, 256) {
        let (Ok(id), Ok(level)) = (r.varint(), r.varint()) else {
            break;
        };
        if let Ok(id) = u32::try_from(id) {
            out.push((id, level));
        }
    }
    out
}

/// `Enchantment.getFullname`: the description in red (curses) or gray, with the level.
fn full_name(
    env: &Env<'_>,
    name: &str,
    description: &Component,
    max_level: i32,
    level: i32,
) -> Component {
    let curse = env
        .global
        .systems
        .in_tag("minecraft:enchantment", "minecraft:curse", name);
    let mut out = description.clone();
    if out.style.color.is_none() {
        out.style.color = Some(Color::Named(if curse {
            NamedColor::Red
        } else {
            NamedColor::Gray
        }));
    }
    if level != 1 || max_level != 1 {
        out = out
            .append(Component::text(" "))
            .append(Component::translatable(
                format!("enchantment.level.{level}"),
                vec![],
            ));
    }
    out
}

/// `/enchant <targets> <enchantment> [<level>]`.
pub(crate) fn enchant(
    args: &Args,
    source: &CommandSource,
    env: &mut Env<'_>,
) -> Result<i32, CommandError> {
    let selector = args.selector("targets").ok_or_else(|| missing("targets"))?;
    let targets = select::many(env, source, selector)?;
    let name = args
        .identifier("enchantment")
        .ok_or_else(|| missing("enchantment"))?
        .clone();
    let game = std::sync::Arc::clone(&env.global.game);
    let (Some(&id), Some(info)) = (
        game.enchantments.get(&name),
        game.enchantment_info.get(&name),
    ) else {
        return Err(error(
            "argument.resource.not_found",
            vec![
                ErrArg::Str(name),
                ErrArg::Str("minecraft:enchantment".to_owned()),
            ],
        ));
    };
    let level = args.int("level").copied().unwrap_or(1);
    if level > info.max_level {
        return Err(error(
            "commands.enchant.failed.level",
            vec![ErrArg::Int(level), ErrArg::Int(info.max_level)],
        ));
    }
    let single = targets.len() == 1;
    let book = ITEM.id("minecraft:enchanted_book");
    let stored = DATA_COMPONENT_TYPE.id("minecraft:stored_enchantments");
    let mut tracker = Tracker::<Component>::default();
    for target in &targets {
        let display = target.display();
        if target.kind.info().attributes.is_empty() {
            if single {
                return Err(error(
                    "commands.enchant.failed.entity",
                    vec![ErrArg::Str(display.plain())],
                ));
            }
            continue;
        }
        // Only players of this world hold items.
        let entities = &mut env.level.entities;
        let Some(player) = target.entity.and_then(|id| entities.player_mut(id)) else {
            if single && target.entity.is_some() {
                return Err(error(
                    "commands.enchant.failed.itemless",
                    vec![ErrArg::Str(display.plain())],
                ));
            }
            continue;
        };
        let Some(stack) = player
            .inventory
            .selected_item_mut()
            .filter(|s| !s.is_empty())
        else {
            if single {
                return Err(error(
                    "commands.enchant.failed.itemless",
                    vec![ErrArg::Str(display.plain())],
                ));
            }
            continue;
        };
        let component = if Some(stack.item) == book {
            stored.unwrap_or(component_ids::ENCHANTMENTS)
        } else {
            component_ids::ENCHANTMENTS
        };
        let existing = stack
            .component(component)
            .map(read_enchantments)
            .unwrap_or_default();
        let compatible = existing.iter().all(|&(other, _)| {
            let other_name = game
                .enchantments
                .iter()
                .find(|(_, v)| **v == other)
                .map(|(k, _)| k.as_str())
                .unwrap_or("");
            other != id
                && !info.exclusive_set.iter().any(|e| e == other_name)
                && !game
                    .enchantment_info
                    .get(other_name)
                    .is_some_and(|o| o.exclusive_set.contains(&name))
        });
        if !info.supported_items.contains(&stack.item) || !compatible {
            if single {
                return Err(error(
                    "commands.enchant.failed.incompatible",
                    vec![ErrArg::Str(hover_name(stack).0.plain())],
                ));
            }
            continue;
        }
        let removed = stack.patch().is_some_and(|p| p.is_removed(component));
        if level > 0 && !removed {
            let mut levels = existing;
            levels.push((id, level.min(255)));
            let mut bytes = Vec::new();
            write_varint(&mut bytes, count_int(levels.len()));
            for (e, l) in levels {
                write_varint(&mut bytes, i32::try_from(e).unwrap_or(0));
                write_varint(&mut bytes, l);
            }
            stack.patch_mut().set(component, bytes.into());
        }
        if let Some(id) = target.entity {
            sync_inventory(entities, id);
        }
        tracker.track(&display, 1);
    }
    let full = full_name(env, &name, &info.description, info.max_level, level);
    tracker.feedback(
        source,
        env,
        true,
        false,
        Some(error("commands.enchant.failed", vec![])),
        |one, n, _| match one {
            Some(entity) => Component::translatable_args(
                "commands.enchant.success.single",
                vec![Arg::from(full), Arg::from(entity.clone())],
            ),
            None => Component::translatable_args(
                "commands.enchant.success.multiple",
                vec![Arg::from(full), Arg::Int(n)],
            ),
        },
    )
}
