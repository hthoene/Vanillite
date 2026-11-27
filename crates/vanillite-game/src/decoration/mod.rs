//! Decorations and technical entities of Vanilla 26.3: item frames and glow item frames,
//! paintings and cushions (`BlockAttachedEntity`), markers, interactions and the block, item
//! and text displays. Their data lives in [`Entities::decor`]; armor stands and mannequins are
//! living entities and live in the mob store as the kinds of the decor family ([`stand`]).
//!
//! Everything here runs in the entity tick (`level` readable, the entities taken out): the
//! block-attached entities check their support every 100 ticks, framed maps reach the players
//! every 10 ticks. Hits, interactions, drops and sounds use the same [`Env`] as the mobs.

pub mod cushion;
pub mod display;
pub mod hanging;
pub mod matrix;
pub mod stand;

use vanillite_base::{BlockPos, Direction, Vec3};
use vanillite_data::entity::{EntityTypeId, ids};
use vanillite_nbt::{Compound, List, Tag};
use vanillite_protocol::packets::play::clientbound::SoundSource;
use vanillite_protocol::packets::play::serverbound::Hand;
use vanillite_world::entity::{EntityFlags, EntityId, NetworkIds, NewEntity, Rotation};

use crate::entities::Entities;
use crate::item::ItemStack;
use crate::level::Level;
use crate::mob::MobContext;
use crate::mob::animal::Interaction;
use crate::player::encode::DataWriter;
use crate::survival::damage::{DamageSource, tag};
use crate::survival::player::Env;

/// The kind data of a decoration entity.
#[derive(Clone, Debug, PartialEq)]
pub enum DecorData {
    /// `item_frame` and `glow_item_frame`.
    Frame(Box<hanging::Frame>),
    Painting(hanging::Painting),
    Cushion(cushion::Cushion),
    /// `Marker`: no data, never sent to clients.
    Marker,
    Interaction(Box<display::Interaction>),
    /// `block_display`, `item_display` and `text_display`.
    Display(Box<display::Display>),
}

/// A decoration entity: its kind data and the `Entity` fields it keeps.
#[derive(Clone, Debug)]
pub struct Decor {
    pub data: DecorData,
    /// `getCustomName` as network NBT.
    pub name: Option<Box<[u8]>>,
    /// `isCustomNameVisible`.
    pub name_visible: bool,
    /// `isSilent`.
    pub silent: bool,
    /// `isInvulnerable`.
    pub invulnerable: bool,
    /// `tickCount`.
    pub tick_count: i32,
    /// The stored fields the game does not model (custom data, fire, …), written back.
    pub extra: Compound,
    /// The entity's own random (`Entity.random`, seeded from the entity seed stream).
    pub random: vanillite_base::random::LegacyRandom,
}

impl Decor {
    /// A decoration with the `Entity` defaults (`new Entity`: a fresh random).
    pub fn new(data: DecorData) -> Decor {
        Decor {
            data,
            name: None,
            name_visible: false,
            silent: false,
            invulnerable: false,
            tick_count: 0,
            extra: Compound::new(),
            random: vanillite_base::random::LegacyRandom::new(crate::mob::new_entity_seed()),
        }
    }
}

impl Decor {
    /// [`Decor::new`] without drawing a seed (the data of an existing entity).
    pub(crate) fn new_without_seed(data: DecorData) -> Decor {
        Decor {
            data,
            name: None,
            name_visible: false,
            silent: false,
            invulnerable: false,
            tick_count: 0,
            extra: Compound::new(),
            random: vanillite_base::random::LegacyRandom::new(0),
        }
    }
}

/// `spawnAtLocation` / `new ItemEntity(level, x, y, z, stack)` with the default pickup delay:
/// the item draws its yaw and motion from its own random.
pub(crate) fn spawn_item(env: &mut Env<'_>, pos: Vec3, stack: ItemStack) {
    if stack.is_empty() {
        return;
    }
    let (uuid, y_rot, velocity, item_random) = crate::mob::new_item_entity();
    env.out.push(crate::survival::Effect::DropItem {
        random: item_random,
        uuid,
        pos,
        velocity,
        y_rot,
        pickup_delay: 10,
        stack,
    });
}

/// Whether entities of `kind` keep their data in the decor store.
#[inline]
pub fn is_decor_kind(kind: EntityTypeId) -> bool {
    kind == ids::ITEM_FRAME
        || kind == ids::GLOW_ITEM_FRAME
        || kind == ids::PAINTING
        || kind == ids::CUSHION
        || kind == ids::MARKER
        || kind == ids::INTERACTION
        || display::is_display_kind(kind)
}

/// Whether the decor entities of `kind` tick (block-attached entities check their support).
#[inline]
pub fn ticks(kind: EntityTypeId) -> bool {
    kind == ids::ITEM_FRAME
        || kind == ids::GLOW_ITEM_FRAME
        || kind == ids::PAINTING
        || kind == ids::CUSHION
}

/// Adds a decoration entity with its bounding box (`setPos` of the kind).
pub fn spawn(
    ents: &mut Entities,
    network_ids: &NetworkIds,
    kind: EntityTypeId,
    uuid: u128,
    pos: Vec3,
    rot: Rotation,
    decor: Decor,
) -> EntityId {
    let mut new = NewEntity::new(kind, network_ids.next(), uuid, pos);
    new.rot = rot;
    let id = ents.spawn_decor(new, decor);
    refresh_box(ents, id);
    id
}

/// `recalculateBoundingBox` / `refreshDimensions` after the kind data changed.
pub fn refresh_box(ents: &mut Entities, id: EntityId) {
    let Some(e) = ents.store.get(id) else {
        return;
    };
    let kind = e.kind();
    let pos = e.pos();
    let Some(d) = ents.decor(id) else {
        return;
    };
    match &d.data {
        DecorData::Frame(_) | DecorData::Painting(_) => {
            let Some(aabb) = hanging::bounding_box(kind, &d.data) else {
                return;
            };
            if let Some(mut e) = ents.store.get_mut(id) {
                let c = aabb.center();
                e.set_pos(c);
                e.set_aabb(aabb);
            }
        }
        DecorData::Cushion(_) => {
            let dims = vanillite_world::entity::Dimensions::of(kind);
            if let Some(mut e) = ents.store.get_mut(id) {
                e.set_dims(dims);
                e.set_pos(pos);
            }
        }
        DecorData::Interaction(i) => {
            let dims = vanillite_world::entity::Dimensions {
                width: i.width,
                height: i.height,
            };
            if let Some(mut e) = ents.store.get_mut(id) {
                e.set_dims(dims);
            }
        }
        DecorData::Marker | DecorData::Display(_) => {
            let dims = vanillite_world::entity::Dimensions {
                width: 0.0,
                height: 0.0,
            };
            if let Some(mut e) = ents.store.get_mut(id) {
                e.set_dims(dims);
            }
        }
    }
}

/// One tick of a decoration (`BlockAttachedEntity.tick`, `ServerEntity` map updates of item
/// frames); a removed one queues itself in [`MobContext::discarded`].
pub fn tick(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    id: EntityId,
) {
    let Some(d) = ents.decor_mut(id) else {
        return;
    };
    d.tick_count = d.tick_count.wrapping_add(1);
    let tick = d.tick_count;
    match &d.data {
        DecorData::Frame(_) | DecorData::Painting(_) | DecorData::Cushion(_) => {}
        _ => return,
    }
    // `checkBelowWorld`.
    if ents
        .store
        .get(id)
        .is_some_and(|e| e.pos().y < f64::from(env.min_y - 64))
    {
        cx.discarded.push(id);
        return;
    }
    if matches!(ents.decor(id).map(|d| &d.data), Some(DecorData::Frame(_))) && (tick - 1) % 10 == 0
    {
        hanging::tick_framed_map(level, ents, env, id);
    }
    let check = match ents.decor_mut(id).map(|d| &mut d.data) {
        Some(DecorData::Frame(f)) => bump(&mut f.attached.checks),
        Some(DecorData::Painting(p)) => bump(&mut p.attached.checks),
        Some(DecorData::Cushion(c)) => bump(&mut c.attached.checks),
        _ => false,
    };
    if !check {
        return;
    }
    if ents.store.get(id).is_some_and(|e| e.kind() == ids::CUSHION) {
        cushion::tick_at_check_interval(level, ents, env, cx, id);
        if cx.discarded.contains(&id) {
            return;
        }
    }
    if !survives(level, ents, id) {
        kill(level, ents, env, cx, id);
        drop_item(level, ents, env, cx, id, None);
    }
}

/// `ticksSinceLastCheck++ >= 100`, resetting it.
fn bump(checks: &mut i32) -> bool {
    let due = *checks >= 100;
    *checks = if due { 0 } else { *checks + 1 };
    due
}

/// `survives` of a block-attached entity.
pub fn survives(level: &Level, ents: &Entities, id: EntityId) -> bool {
    let Some(e) = ents.store.get(id) else {
        return false;
    };
    let kind = e.kind();
    let aabb = e.aabb();
    match ents.decor(id).map(|d| &d.data) {
        Some(data @ (DecorData::Frame(_) | DecorData::Painting(_))) => {
            hanging::survives(level, ents, Some(id), kind, data)
        }
        Some(DecorData::Cushion(_)) => cushion::would_survive_at(level, &aabb),
        _ => true,
    }
}

/// `kill(level, attributedTo)` of a block-attached entity: `onKilled` (a framed map forgets
/// the frame), then the removal.
pub fn kill(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    id: EntityId,
) {
    if cx.discarded.contains(&id) {
        return;
    }
    if let Some(DecorData::Frame(f)) = ents.decor(id).map(|d| &d.data) {
        let stack = f.item.clone();
        hanging::remove_framed_map(level, ents, id, &stack);
    }
    let _ = env;
    cx.discarded.push(id);
}

/// `dropItem(level, causedBy)` of a block-attached entity.
pub fn drop_item(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    id: EntityId,
    caused_by: Option<EntityId>,
) {
    let Some(kind) = ents.store.get(id).map(|e| e.kind()) else {
        return;
    };
    if kind == ids::CUSHION {
        cushion::drop_item(ents, env, id, caused_by);
    } else if kind == ids::PAINTING || kind == ids::ITEM_FRAME || kind == ids::GLOW_ITEM_FRAME {
        hanging::drop_item(level, ents, env, cx, id, caused_by);
    }
}

/// Whether `player` is a player with infinite materials (`hasInfiniteMaterials`).
pub(crate) fn creative(ents: &Entities, player: Option<EntityId>) -> bool {
    player
        .and_then(|p| ents.player(p))
        .is_some_and(|p| p.game_mode == vanillite_config::GameMode::Creative)
}

/// `isInvulnerableToBase(source)`.
fn invulnerable_to_base(ents: &Entities, env: &Env<'_>, d: &Decor, source: &DamageSource) -> bool {
    d.invulnerable
        && !env.types.is(source.kind, tag::BYPASSES_INVULNERABILITY)
        && !creative(ents, source.cause)
}

/// `hurtServer` of a decoration; whether it was hurt.
pub fn hurt(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    id: EntityId,
    source: &DamageSource,
) -> bool {
    if cx.discarded.contains(&id) {
        return false;
    }
    let Some(d) = ents.decor(id) else {
        return false;
    };
    let kind = match &d.data {
        DecorData::Frame(_) => 0,
        DecorData::Painting(_) => 1,
        DecorData::Cushion(_) => 2,
        // Markers, interactions and displays take no damage.
        _ => return false,
    };
    if kind == 2 && !cushion::may_break(ents, source) {
        return false;
    }
    if kind == 0 {
        return hanging::hurt_frame(level, ents, env, cx, id, source);
    }
    hurt_attached(level, ents, env, cx, id, source)
}

/// `BlockAttachedEntity.hurtServer`.
pub(crate) fn hurt_attached(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    id: EntityId,
    source: &DamageSource,
) -> bool {
    let Some(d) = ents.decor(id) else {
        return false;
    };
    if invulnerable_to_base(ents, env, d, source) {
        return false;
    }
    let mob_cause = source.cause.is_some_and(|c| ents.mob(c).is_some());
    if mob_cause && !env.rules.bool(crate::survival::rules::Rule::MOB_GRIEFING) {
        return false;
    }
    kill(level, ents, env, cx, id);
    drop_item(level, ents, env, cx, id, source.cause);
    true
}

/// `thunderHit`: cushions break, the other decorations ignore lightning.
pub fn thunder_hit(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    id: EntityId,
    bolt: Option<EntityId>,
) {
    if ents.store.get(id).is_some_and(|e| e.kind() == ids::CUSHION) && !cx.discarded.contains(&id) {
        kill(level, ents, env, cx, id);
        cushion::drop_item(ents, env, id, bolt);
    }
}

/// The result of a player's attack on a decoration (`skipAttackInteraction`): `true` when the
/// attack ends there.
pub fn attack(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    player: EntityId,
    id: EntityId,
) -> bool {
    match ents.decor(id).map(|d| &d.data) {
        Some(DecorData::Interaction(_)) => display::attack_interaction(ents, env, player, id),
        Some(DecorData::Frame(_) | DecorData::Painting(_) | DecorData::Cushion(_)) => {
            let source =
                crate::mob::combat::entity_source(ents, env.types.player_attack, player, player);
            hurt(level, ents, env, cx, id, &source);
            true
        }
        _ => true,
    }
}

/// `interact(player, hand, location)` of a decoration.
#[allow(clippy::too_many_arguments)]
pub fn interact(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    player: EntityId,
    id: EntityId,
    hand: Hand,
    sneaking: bool,
) -> Interaction {
    match ents.decor(id).map(|d| &d.data) {
        Some(DecorData::Frame(_)) => {
            hanging::interact_frame(level, ents, env, cx, player, id, hand)
        }
        Some(DecorData::Cushion(_)) => cushion::interact(ents, env, player, id, sneaking),
        Some(DecorData::Interaction(_)) => display::interact_interaction(ents, env, player, id),
        _ => Interaction::Pass,
    }
}

/// `getPickResult` of a decoration (`None` for those without one).
pub fn pick_result(ents: &Entities, id: EntityId) -> Option<ItemStack> {
    let kind = ents.store.get(id)?.kind();
    match &ents.decor(id)?.data {
        DecorData::Frame(f) => Some(if f.item.is_empty() {
            hanging::frame_item(kind)
        } else {
            f.item.clone()
        }),
        DecorData::Painting(_) => stack_of("minecraft:painting"),
        DecorData::Cushion(c) => stack_of(&cushion::item_name(c.color)),
        _ => None,
    }
}

/// One item of `name`.
pub(crate) fn stack_of(name: &str) -> Option<ItemStack> {
    vanillite_data::registry::ITEM
        .id(name)
        .map(|i| ItemStack::new(i, 1))
}

/// `item.set(CUSTOM_NAME, getCustomName())` of a dropped item.
pub(crate) fn with_name(mut stack: ItemStack, name: Option<&[u8]>) -> ItemStack {
    if let Some(name) = name {
        stack.patch_mut().set(
            vanillite_protocol::components::ids::CUSTOM_NAME,
            name.into(),
        );
    }
    stack
}

/// `Entity.playSound(sound, volume, pitch)`: silent entities make none.
pub(crate) fn play_sound(
    ents: &Entities,
    env: &mut Env<'_>,
    id: EntityId,
    sound: &str,
    volume: f32,
    pitch: f32,
) {
    if ents.decor(id).is_some_and(|d| d.silent) {
        return;
    }
    let Some(pos) = ents.store.get(id).map(|e| e.pos()) else {
        return;
    };
    crate::survival::play_sound(
        env.out,
        None,
        pos,
        sound,
        SoundSource::Neutral,
        volume,
        pitch,
        crate::survival::sound_seed(),
    );
}

/// The `AddEntity` data and position of a decoration (`getAddEntityPacket`): hanging
/// entities send their direction and block position.
pub fn add_entity_packet(ents: &Entities, id: EntityId) -> Option<(i32, Vec3)> {
    match &ents.decor(id)?.data {
        DecorData::Frame(f) => Some((
            i32::from(f.attached.direction as u8),
            block_vec(f.attached.pos),
        )),
        DecorData::Painting(p) => Some((
            i32::from(p.attached.direction as u8),
            block_vec(p.attached.pos),
        )),
        _ => None,
    }
}

fn block_vec(p: BlockPos) -> Vec3 {
    Vec3::new(f64::from(p.x), f64::from(p.y), f64::from(p.z))
}

/// The synched data of a decoration after the entity's own (ids from 8 on).
pub fn entity_data(w: &mut DataWriter<'_>, d: &Decor) {
    match &d.data {
        DecorData::Frame(f) => hanging::frame_data(w, f),
        DecorData::Painting(p) => hanging::painting_data(w, p),
        DecorData::Cushion(c) => cushion::entity_data(w, c),
        DecorData::Interaction(i) => display::interaction_data(w, i),
        DecorData::Display(disp) => display::entity_data(w, disp),
        DecorData::Marker => {}
    }
}

/// `CustomNameVisible` of a decoration or decor family mob.
pub fn name_visible(ents: &Entities, id: EntityId) -> bool {
    if let Some(d) = ents.decor(id) {
        return d.name_visible;
    }
    ents.mob(id)
        .and_then(stand::state)
        .is_some_and(|s| s.name_visible)
}

/// The custom name of a decoration.
pub fn custom_name(ents: &Entities, id: EntityId) -> Option<&[u8]> {
    ents.decor(id)?.name.as_deref()
}

// --- Storage -----------------------------------------------------------------------------

/// The keys of `Entity.saveWithoutId` the decorations model.
const BASE_KEYS: [&str; 12] = [
    "id",
    "Pos",
    "Motion",
    "Rotation",
    "UUID",
    "OnGround",
    "NoGravity",
    "CustomName",
    "CustomNameVisible",
    "Silent",
    "Invulnerable",
    "Glowing",
];

/// `Entity.saveWithoutId` of a decoration (`None` for other entities).
pub fn save(ents: &Entities, id: EntityId) -> Option<Compound> {
    let e = ents.store.get(id)?;
    let d = ents.decor(id)?;
    let mut out = d.extra.clone();
    let base = crate::mob::nbt::save_entity_base(&e);
    for (k, v) in base.iter() {
        out.insert(k, v.clone());
    }
    or_default(&mut out, "fall_distance", Tag::Double(0.0));
    or_default(&mut out, "Fire", Tag::Short(0));
    or_default(&mut out, "Air", Tag::Short(300));
    or_default(&mut out, "PortalCooldown", Tag::Int(0));
    out.insert("Invulnerable", d.invulnerable);
    if let Some(name) = d.name.as_deref().and_then(crate::mob::nbt::component_tag) {
        out.insert("CustomName", name);
    }
    if d.name_visible {
        out.insert("CustomNameVisible", true);
    }
    if d.silent {
        out.insert("Silent", true);
    }
    if e.flags().contains(EntityFlags::GLOWING) {
        out.insert("Glowing", true);
    }
    let invisible = e.flags().contains(EntityFlags::INVISIBLE);
    match &d.data {
        DecorData::Frame(f) => hanging::save_frame(f, invisible, &mut out),
        DecorData::Painting(p) => hanging::save_painting(p, &mut out),
        DecorData::Cushion(c) => cushion::save(c, &mut out),
        DecorData::Interaction(i) => display::save_interaction(i, &mut out),
        DecorData::Display(disp) => display::save(disp, &mut out),
        DecorData::Marker => {}
    }
    Some(out)
}

/// `EntityType.create` + `load` of stored decoration data; `None` for other kinds.
pub fn load(
    tag: &Compound,
) -> Option<(
    EntityTypeId,
    crate::mob::nbt::LoadedEntity,
    Decor,
    EntityFlags,
)> {
    let kind = crate::mob::nbt::type_of(tag)?;
    if !is_decor_kind(kind) {
        return None;
    }
    let base = crate::mob::nbt::load_entity_base(tag)?;
    let mut flags = EntityFlags::EMPTY;
    let data = read_data(kind, tag, base.pos, &mut flags)?;
    let mut d = Decor::new(data);
    read_base(&mut d, tag, &mut flags);
    d.extra = tag.clone();
    for k in BASE_KEYS.iter().chain(kind_keys(kind)) {
        d.extra.remove(k);
    }
    d.extra.remove("Tags");
    Some((kind, base, d, flags))
}

/// The kind data of `kind` read from `tag` (`readAdditionalSaveData`).
fn read_data(
    kind: EntityTypeId,
    tag: &Compound,
    pos: Vec3,
    flags: &mut EntityFlags,
) -> Option<DecorData> {
    Some(if kind == ids::ITEM_FRAME || kind == ids::GLOW_ITEM_FRAME {
        let (frame, invisible) = hanging::load_frame(tag, pos);
        flags.set(EntityFlags::INVISIBLE, invisible);
        DecorData::Frame(Box::new(frame))
    } else if kind == ids::PAINTING {
        DecorData::Painting(hanging::load_painting(tag, pos))
    } else if kind == ids::CUSHION {
        DecorData::Cushion(cushion::load(tag, pos))
    } else if kind == ids::MARKER {
        DecorData::Marker
    } else if kind == ids::INTERACTION {
        DecorData::Interaction(Box::new(display::load_interaction(tag)))
    } else {
        DecorData::Display(Box::new(display::load(kind, tag)?))
    })
}

fn read_base(d: &mut Decor, tag: &Compound, flags: &mut EntityFlags) {
    d.name = tag
        .get("CustomName")
        .and_then(crate::mob::nbt::tag_component);
    d.name_visible = flag(tag, "CustomNameVisible");
    d.silent = flag(tag, "Silent");
    d.invulnerable = flag(tag, "Invulnerable");
    flags.set(EntityFlags::GLOWING, flag(tag, "Glowing"));
    flags.set(EntityFlags::NO_GRAVITY, flag(tag, "NoGravity"));
    flags.set(EntityFlags::ON_GROUND, flag(tag, "OnGround"));
}

/// The kind keys `load` reads for `kind` (the others stay in [`Decor::extra`]).
fn kind_keys(kind: EntityTypeId) -> &'static [&'static str] {
    if kind == ids::ITEM_FRAME || kind == ids::GLOW_ITEM_FRAME {
        &[
            "block_pos",
            "Item",
            "ItemRotation",
            "ItemDropChance",
            "Facing",
            "Invisible",
            "Fixed",
        ]
    } else if kind == ids::PAINTING {
        &["block_pos", "facing", "variant"]
    } else if kind == ids::CUSHION {
        &["block_pos", "color"]
    } else if kind == ids::INTERACTION {
        &["width", "height", "attack", "interaction", "response"]
    } else if display::is_display_kind(kind) {
        display::KEYS
    } else {
        &[]
    }
}

/// `/data` and `/summon` on a live decoration: the stored data is read again from `tag` (the
/// whole compound, as `Entity.load`), the entity keeps its id.
pub fn apply_nbt(ents: &mut Entities, id: EntityId, tag: &Compound) -> bool {
    let Some(kind) = ents.store.get(id).map(|e| e.kind()) else {
        return false;
    };
    if ents.mob(id).is_some() {
        return stand::apply_nbt(ents, id, tag);
    }
    if !is_decor_kind(kind) {
        return false;
    }
    let Some(pos) = ents.store.get(id).map(|e| e.pos()) else {
        return false;
    };
    let pos = crate::mob::nbt::pos_of(tag).unwrap_or(pos);
    let mut flags = EntityFlags::EMPTY;
    let Some(data) = read_data(kind, tag, pos, &mut flags) else {
        return false;
    };
    let Some(d) = ents.decor_mut(id) else {
        return false;
    };
    let old_frame_item = match &d.data {
        DecorData::Frame(f) => Some(f.item.clone()),
        _ => None,
    };
    let tick = d.tick_count;
    let random = d.random.clone();
    let mut fresh = Decor {
        random,
        ..Decor::new_without_seed(data)
    };
    read_base(&mut fresh, tag, &mut flags);
    fresh.tick_count = tick;
    fresh.extra = tag.clone();
    for k in BASE_KEYS.iter().chain(kind_keys(kind)) {
        fresh.extra.remove(k);
    }
    fresh.extra.remove("Tags");
    // Interaction records survive a load that does not name them.
    *d = fresh;
    let _ = old_frame_item;
    let rot = match ents.decor(id).map(|d| &d.data) {
        Some(DecorData::Frame(f)) => Some(hanging::rotation_of(kind, f.attached.direction)),
        Some(DecorData::Painting(p)) => Some(hanging::rotation_of(kind, p.attached.direction)),
        _ => crate::mob::nbt::load_entity_base(tag).map(|b| b.rot),
    };
    if let Some(mut e) = ents.store.get_mut(id) {
        for f in [
            EntityFlags::GLOWING,
            EntityFlags::NO_GRAVITY,
            EntityFlags::INVISIBLE,
        ] {
            e.set_flags(f, flags.contains(f));
        }
        e.set_pos(pos);
        if let Some(rot) = rot.filter(|_| tag.get("Rotation").is_some() || kind != ids::MARKER) {
            e.set_rot(rot);
        }
        e.mark_data_dirty();
    }
    refresh_box(ents, id);
    true
}

fn or_default(nbt: &mut Compound, name: &str, value: Tag) {
    if nbt.get(name).is_none() {
        nbt.insert(name, value);
    }
}

pub(crate) fn flag(nbt: &Compound, name: &str) -> bool {
    nbt.get(name).and_then(Tag::as_i64).is_some_and(|v| v != 0)
}

pub(crate) fn int(nbt: &Compound, name: &str) -> Option<i32> {
    nbt.get(name)
        .and_then(Tag::as_i64)
        .and_then(|v| i32::try_from(v).ok())
}

pub(crate) fn float(nbt: &Compound, name: &str) -> Option<f32> {
    match nbt.get(name) {
        Some(Tag::Float(v)) => Some(*v),
        #[allow(clippy::cast_possible_truncation)]
        Some(Tag::Double(v)) => Some(*v as f32),
        Some(t) => t.as_i64().map(|v| {
            #[allow(clippy::cast_precision_loss)]
            let f = v as f32;
            f
        }),
        None => None,
    }
}

/// `BlockPos.CODEC` (three ints) under `key`.
pub(crate) fn block_pos_of(nbt: &Compound, key: &str) -> Option<BlockPos> {
    match nbt.get(key) {
        Some(Tag::IntArray(v)) if v.len() == 3 => Some(BlockPos::new(v[0], v[1], v[2])),
        Some(Tag::List(List(v))) if v.len() == 3 => {
            let i = |t: &Tag| t.as_i64().and_then(|v| i32::try_from(v).ok());
            Some(BlockPos::new(i(&v[0])?, i(&v[1])?, i(&v[2])?))
        }
        _ => None,
    }
}

pub(crate) fn block_pos_tag(p: BlockPos) -> Tag {
    Tag::IntArray(vec![p.x, p.y, p.z])
}

/// `Direction.get2DDataValue` (south, west, north, east).
pub(crate) fn data_2d(d: Direction) -> i32 {
    match d {
        Direction::South => 0,
        Direction::West => 1,
        Direction::North => 2,
        Direction::East => 3,
        _ => -1,
    }
}

/// `Direction.from2DDataValue`.
pub(crate) fn from_2d(v: i32) -> Direction {
    match v.rem_euclid(4) {
        0 => Direction::South,
        1 => Direction::West,
        2 => Direction::North,
        _ => Direction::East,
    }
}

/// `Direction.from3DDataValue`.
pub(crate) fn from_3d(v: i32) -> Direction {
    Direction::ALL[usize::try_from(v.rem_euclid(6)).unwrap_or(0)]
}

/// `Direction.getCounterClockWise` (around Y).
pub(crate) fn counter_clockwise(d: Direction) -> Direction {
    match d {
        Direction::North => Direction::West,
        Direction::West => Direction::South,
        Direction::South => Direction::East,
        Direction::East => Direction::North,
        v => v,
    }
}

/// Whether the direction is horizontal.
pub(crate) fn horizontal(d: Direction) -> bool {
    !matches!(d, Direction::Up | Direction::Down)
}

/// `Direction.fromYRot`.
pub(crate) fn from_y_rot(y_rot: f32) -> Direction {
    from_2d(vanillite_base::math::floor(f64::from(y_rot) / 90.0 + 0.5) & 3)
}

/// Whether `pos` is inside the build height of the level.
pub(crate) fn in_build_height(level: &Level, pos: BlockPos) -> bool {
    level.in_height(pos.y)
}

/// `BlockAttachedEntity.move` / `push` outside the entity tick (pistons): the entity breaks
/// and drops its item without a cause. Fixed item frames stay.
pub fn break_in_level(level: &mut Level, id: EntityId) {
    let Some(e) = level.entities.store.get(id) else {
        return;
    };
    let (kind, pos) = (e.kind(), e.pos());
    let Some(d) = level.entities.decor(id) else {
        return;
    };
    let name = d.name.clone();
    let silent = d.silent;
    let (sound, items, direction) = match &d.data {
        DecorData::Frame(f) => {
            if f.fixed {
                return;
            }
            let mut items = vec![with_name(hanging::frame_item(kind), name.as_deref())];
            if !f.item.is_empty() {
                items.push(f.item.clone());
            }
            let s = if kind == ids::GLOW_ITEM_FRAME {
                "minecraft:entity.glow_item_frame.break"
            } else {
                "minecraft:entity.item_frame.break"
            };
            (s, items, Some(f.attached.direction))
        }
        DecorData::Painting(p) => (
            "minecraft:entity.painting.break",
            stack_of("minecraft:painting")
                .map(|s| with_name(s, name.as_deref()))
                .into_iter()
                .collect(),
            Some(p.attached.direction),
        ),
        DecorData::Cushion(c) => (
            "minecraft:entity.cushion.break",
            stack_of(&cushion::item_name(c.color))
                .map(|s| with_name(s, name.as_deref()))
                .into_iter()
                .collect(),
            None,
        ),
        _ => return,
    };
    if let Some(DecorData::Frame(f)) = level.entities.decor(id).map(|d| &d.data) {
        let stack = f.item.clone();
        hanging::remove_framed_map(level, &level.entities, id, &stack);
    }
    let drops = level.settings.entity_drops;
    let roll = match level.entities.decor_mut(id) {
        Some(d) => match &d.data {
            DecorData::Frame(f) => {
                use vanillite_base::random::RandomSource;
                let chance = f.drop_chance;
                Some(d.random.next_float() < chance)
            }
            _ => None,
        },
        None => None,
    };
    level.entities.despawn(id);
    if !(silent || kind == ids::PAINTING && !drops) {
        level.play_sound(
            None,
            pos.x,
            pos.y,
            pos.z,
            hanging::sound_id(sound),
            crate::level::SOUND_NEUTRAL,
            1.0,
            1.0,
        );
    }
    if !drops {
        return;
    }
    let at = match direction {
        Some(d) => {
            let [x, _, z] = d.normal();
            Vec3::new(
                pos.x + f64::from(x as f32 * 0.15),
                pos.y,
                pos.z + f64::from(z as f32 * 0.15),
            )
        }
        None => pos,
    };
    for (i, stack) in items.into_iter().enumerate() {
        if i == 1 && roll == Some(false) {
            continue;
        }
        crate::block::drops::spawn_item_moving(level, at, stack, None, 10, None);
    }
}
