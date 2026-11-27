//! The world side of the plugin API: player properties, entities, effects, items, block
//! entities and advancements of the world a plugin call runs on. Each function works like the
//! Vanilla command that does the same (`/gamemode`, `/tp`, `/summon`, `/effect`, `/give`,
//! `/item`, `/data`, `/advancement`) without its feedback.

use std::borrow::Cow;
use std::ops::RangeInclusive;

use vanillite_abi::{effect_flag, game_mode, player_prop, status};
use vanillite_base::{BlockPos, Vec3};
use vanillite_command::argument::ParseCtx;
use vanillite_command::reader::StringReader;
use vanillite_command::{ArgValue, ArgumentType, GameValue, Limits};
use vanillite_config::GameMode;
use vanillite_data::entity::EntityTypeId;
use vanillite_game::advancements::Grant;
use vanillite_game::effect::{EffectInstance, MobEffect};
use vanillite_game::entities::Entities;
use vanillite_game::item::ItemStack;
use vanillite_game::survival::player::{Abilities, MAX_AIR};
use vanillite_nbt::Compound;
use vanillite_plugin_host::Location;
use vanillite_world::entity::{EntityFlags, EntityId};

use super::PluginWorld;
use crate::commands::exec;
use crate::commands::items::{self, EntitySlot};
use crate::commands::{CommandSource, Env, SourceKind};
use crate::world::WorldId;

type Result<T> = std::result::Result<T, i32>;

/// Vanilla's default flying speed (`Abilities.flyingSpeed`); it cannot be changed yet.
const FLY_SPEED: f64 = 0.05;
/// Vanilla's default walking speed (`Abilities.walkingSpeed`); it cannot be changed yet.
const WALK_SPEED: f64 = 0.1;
/// `FoodData`'s exhaustion cap.
const MAX_EXHAUSTION: f32 = 40.0;

/// A player or another entity of this world by UUID.
pub(super) fn any_entity(entities: &Entities, uuid: u128) -> Result<EntityId> {
    entities
        .player_by_uuid(uuid)
        .or_else(|| crate::commands::find_uuid(entities, uuid))
        .ok_or(status::NOT_FOUND)
}

/// A player of this world by UUID.
fn player_id(entities: &Entities, uuid: u128) -> Result<EntityId> {
    entities.player_by_uuid(uuid).ok_or(status::NOT_FOUND)
}

/// A namespaced id as Vanilla's `ResourceLocation` reads it: `zombie` is `minecraft:zombie`.
pub(super) fn qualified(id: &str) -> Cow<'_, str> {
    if id.contains(':') {
        Cow::Borrowed(id)
    } else {
        Cow::Owned(format!("minecraft:{id}"))
    }
}

/// A position with finite coordinates.
pub(super) fn position([x, y, z]: [f64; 3]) -> Result<Vec3> {
    if x.is_finite() && y.is_finite() && z.is_finite() {
        Ok(Vec3::new(x, y, z))
    } else {
        Err(status::INVALID_ARGUMENT)
    }
}

/// A whole number within `range`.
#[allow(clippy::as_conversions, clippy::cast_possible_truncation)]
pub(super) fn whole(value: f64, range: RangeInclusive<i32>) -> Result<i32> {
    let inside = value >= f64::from(*range.start()) && value <= f64::from(*range.end());
    if value.fract() == 0.0 && inside {
        Ok(value as i32)
    } else {
        Err(status::INVALID_ARGUMENT)
    }
}

/// A float within `range`.
#[allow(clippy::as_conversions, clippy::cast_possible_truncation)]
pub(super) fn float(value: f64, range: RangeInclusive<f32>) -> Result<f32> {
    let value = value as f32;
    if range.contains(&value) {
        Ok(value)
    } else {
        Err(status::INVALID_ARGUMENT)
    }
}

/// The command environment of the plugin's world.
fn env<'a>(w: &'a mut PluginWorld<'_>) -> Env<'a> {
    Env {
        world: w.0.id(),
        level: w.0.level_mut(),
        global: w.1,
        others: Vec::new(),
    }
}

/// A silent source of the server at `pos` in the plugin's world, for the command code that
/// needs one.
fn server_source(w: &PluginWorld<'_>, pos: Vec3) -> Result<CommandSource> {
    let global = w.1.arc().ok_or(status::ERROR)?;
    let mut source = CommandSource::new(
        global,
        SourceKind::Server,
        4,
        w.0.id(),
        pos,
        (0.0, 0.0),
        None,
    );
    source.silent = true;
    Ok(source)
}

pub(super) fn player_get(w: &PluginWorld<'_>, player: u128, prop: u32) -> Result<f64> {
    let entities = w.0.entities();
    let p = entities
        .player(player_id(entities, player)?)
        .ok_or(status::NOT_FOUND)?;
    let s = &p.survival;
    let flag = |b: bool| if b { 1.0 } else { 0.0 };
    Ok(match prop {
        player_prop::HEALTH => f64::from(s.health),
        player_prop::MAX_HEALTH => f64::from(s.max_health()),
        player_prop::FOOD => f64::from(s.food.food_level),
        player_prop::SATURATION => f64::from(s.food.saturation),
        player_prop::EXHAUSTION => f64::from(s.food.exhaustion),
        player_prop::XP_LEVEL => f64::from(s.xp.level),
        player_prop::XP_PROGRESS => f64::from(s.xp.progress),
        player_prop::XP_TOTAL => f64::from(s.xp.total),
        player_prop::GAME_MODE => f64::from(super::plugin_game_mode(p.game_mode)),
        player_prop::FLYING => flag(s.flying),
        player_prop::MAY_FLY => flag(Abilities::of(p.game_mode).mayfly),
        player_prop::FLY_SPEED => FLY_SPEED,
        player_prop::WALK_SPEED => WALK_SPEED,
        player_prop::SELECTED_SLOT => f64::from(u32::try_from(p.inventory.selected).unwrap_or(0)),
        player_prop::AIR => f64::from(s.air),
        player_prop::FIRE_TICKS => f64::from(s.fire_ticks),
        _ => return Err(status::INVALID_ARGUMENT),
    })
}

/// Sets a player property. Health 0 kills as `/kill` does (Vanilla's `setHealth(0)` would
/// leave the player dying without `die`); the client learns health, food and experience
/// with the player's next tick as in Vanilla.
pub(super) fn player_set(
    w: &mut PluginWorld<'_>,
    player: u128,
    prop: u32,
    value: f64,
) -> Result<()> {
    let id = player_id(w.0.entities(), player)?;
    match prop {
        player_prop::GAME_MODE => return set_game_mode(w, id, value),
        player_prop::HEALTH if value == 0.0 => {
            let dimension = w.0.dimension().to_owned();
            crate::commands::kill_any(w.0.level_mut(), w.1, &dimension, id);
            return Ok(());
        }
        player_prop::MAX_HEALTH | player_prop::XP_TOTAL => return Err(status::INVALID_ARGUMENT),
        // The abilities follow the game mode; they have no state of their own yet.
        player_prop::MAY_FLY | player_prop::FLY_SPEED | player_prop::WALK_SPEED => {
            return Err(status::UNSUPPORTED);
        }
        _ => {}
    }
    let entities = w.0.entities_mut();
    let p = entities.player_mut(id).ok_or(status::NOT_FOUND)?;
    let s = &mut p.survival;
    // Whether the synched entity data (health, air) changed.
    let mut data_changed = false;
    match prop {
        player_prop::HEALTH => {
            s.health = float(value, 0.0..=s.max_health())?;
            data_changed = true;
        }
        player_prop::FOOD => {
            s.food.food_level = whole(value, 0..=vanillite_game::survival::food::MAX_FOOD)?;
        }
        player_prop::SATURATION => {
            // `FoodData.setSaturation`'s cap is the food level.
            #[allow(clippy::as_conversions, clippy::cast_precision_loss)]
            let max = s.food.food_level as f32;
            s.food.saturation = float(value, 0.0..=max)?;
        }
        player_prop::EXHAUSTION => s.food.exhaustion = float(value, 0.0..=MAX_EXHAUSTION)?,
        player_prop::XP_LEVEL => {
            s.xp.level = whole(value, 0..=i32::MAX)?;
            s.resend_experience();
        }
        player_prop::XP_PROGRESS => {
            s.xp.progress = float(value, 0.0..=1.0)?;
            s.resend_experience();
        }
        player_prop::FLYING => {
            let flying = whole(value, 0..=1)? == 1;
            if flying && !Abilities::of(p.game_mode).mayfly {
                return Err(status::INVALID_ARGUMENT);
            }
            s.flying = flying;
            vanillite_game::player::join::send_abilities(
                p.connection.sink.as_ref(),
                p.game_mode,
                flying,
            );
        }
        player_prop::SELECTED_SLOT => {
            let slot = whole(value, 0..=8)?;
            p.inventory.selected = usize::try_from(slot).map_err(|_| status::INVALID_ARGUMENT)?;
            p.connection
                .sink
                .send(&vanillite_protocol::packets::play::clientbound::SetHeldSlot { slot });
        }
        player_prop::AIR => {
            s.air = whole(value, i32::MIN..=MAX_AIR)?;
            data_changed = true;
        }
        player_prop::FIRE_TICKS => s.fire_ticks = whole(value, i32::MIN..=i32::MAX)?,
        _ => return Err(status::INVALID_ARGUMENT),
    }
    if data_changed && let Some(mut entity) = entities.store.get_mut(id) {
        entity.mark_data_dirty();
    }
    Ok(())
}

/// `/gamemode` on one player of this world, with the plugins' change event: `ERROR` when a
/// plugin refused the change.
fn set_game_mode(w: &mut PluginWorld<'_>, id: EntityId, value: f64) -> Result<()> {
    let mode = match u32::try_from(whole(value, 0..=3)?) {
        Ok(game_mode::SURVIVAL) => GameMode::Survival,
        Ok(game_mode::CREATIVE) => GameMode::Creative,
        Ok(game_mode::ADVENTURE) => GameMode::Adventure,
        Ok(game_mode::SPECTATOR) => GameMode::Spectator,
        _ => return Err(status::INVALID_ARGUMENT),
    };
    let target =
        crate::commands::local_target(w.0.entities(), w.0.id(), id).ok_or(status::NOT_FOUND)?;
    if target.game_mode == mode || exec::enforce_game_mode(w.0.entities_mut(), w.1, &target, mode) {
        Ok(())
    } else {
        Err(status::ERROR)
    }
}

pub(super) fn entity_location(w: &PluginWorld<'_>, entity: u128) -> Result<Location> {
    let entities = w.0.entities();
    let e = entities
        .store
        .get(any_entity(entities, entity)?)
        .ok_or(status::NOT_FOUND)?;
    let (pos, rot) = (e.pos(), e.rot());
    Ok(Location {
        world: u32::from(w.0.id().0),
        pos: [pos.x, pos.y, pos.z],
        yaw: rot.y_rot,
        pitch: rot.x_rot,
    })
}

pub(super) fn entity_type(w: &PluginWorld<'_>, entity: u128) -> Result<String> {
    let entities = w.0.entities();
    let e = entities
        .store
        .get(any_entity(entities, entity)?)
        .ok_or(status::NOT_FOUND)?;
    Ok(e.kind().name().to_owned())
}

/// `/tp`: within this world at once; players to another world between ticks, as `/tp` moves
/// targets of another world (like through a portal).
pub(super) fn teleport(w: &mut PluginWorld<'_>, entity: u128, to: Location) -> Result<()> {
    let pos = position(to.pos)?;
    if !to.yaw.is_finite() || !to.pitch.is_finite() || !exec::in_spawnable_bounds(pos) {
        return Err(status::INVALID_ARGUMENT);
    }
    let world = w.0.id();
    let id = any_entity(w.0.entities(), entity)?;
    let target =
        crate::commands::local_target(w.0.entities(), world, id).ok_or(status::NOT_FOUND)?;
    if to.world == u32::from(world.0) {
        return exec::teleport_local(
            w.0.level_mut(),
            w.1,
            &target,
            pos,
            0,
            to.yaw,
            to.pitch,
            vanillite_abi::teleport_cause::PLUGIN,
        )
        .map_err(|_| status::INVALID_ARGUMENT);
    }
    let destination = u16::try_from(to.world)
        .ok()
        .filter(|&i| usize::from(i) < w.1.dimensions.len())
        .ok_or(status::INVALID_ARGUMENT)?;
    if !target.is_player() {
        return Err(status::UNSUPPORTED);
    }
    exec::move_to_world(
        w.1,
        &target,
        WorldId(destination),
        pos,
        Some((to.yaw, to.pitch)),
        Some(vanillite_abi::teleport_cause::PLUGIN),
    );
    Ok(())
}

/// `setDeltaMovement` with `hurtMarked`: the tracker sends the motion to everyone, the entity
/// itself too.
pub(super) fn set_velocity(w: &mut PluginWorld<'_>, entity: u128, v: [f64; 3]) -> Result<()> {
    let velocity = position(v)?;
    let entities = w.0.entities_mut();
    let id = any_entity(entities, entity)?;
    let mut e = entities.store.get_mut(id).ok_or(status::NOT_FOUND)?;
    e.set_velocity(velocity);
    e.set_flags(EntityFlags::HURT_MARKED, true);
    Ok(())
}

pub(super) fn spawn_entity(
    w: &mut PluginWorld<'_>,
    kind: &str,
    pos: [f64; 3],
    nbt: &Compound,
) -> Result<u128> {
    let pos = position(pos)?;
    let source = server_source(w, pos)?;
    crate::commands::summon_for(
        &mut env(w),
        &source,
        &qualified(kind),
        pos,
        nbt,
        vanillite_abi::spawn_reason::PLUGIN,
    )
    .map(|target| target.uuid)
    .map_err(|_| status::INVALID_ARGUMENT)
}

/// `Entity.discard`; players cannot be removed.
pub(super) fn remove_entity(w: &mut PluginWorld<'_>, entity: u128) -> Result<()> {
    let entities = w.0.entities_mut();
    let id = any_entity(entities, entity)?;
    if entities.player(id).is_some() {
        return Err(status::INVALID_ARGUMENT);
    }
    entities.despawn(id);
    Ok(())
}

/// The entities whose position lies within `radius` of `center`, nearest first, through the
/// section index; nothing for an unknown type or a radius that is no distance.
pub(super) fn entities_near(
    w: &PluginWorld<'_>,
    center: [f64; 3],
    radius: f64,
    kind: Option<&str>,
) -> Vec<u128> {
    let Ok(center) = position(center) else {
        return Vec::new();
    };
    if !radius.is_finite() || radius < 0.0 {
        return Vec::new();
    }
    let kind = match kind {
        Some(name) => match EntityTypeId::by_name(&qualified(name)) {
            Some(kind) => Some(kind),
            None => return Vec::new(),
        },
        None => None,
    };
    let store = &w.0.entities().store;
    let area = vanillite_base::Aabb::new(
        center.x - radius,
        center.y - radius,
        center.z - radius,
        center.x + radius,
        center.y + radius,
        center.z + radius,
    );
    let mut found = Vec::new();
    store.for_each_in(&area, false, |id| {
        if let Some(e) = store.get(id)
            && kind.is_none_or(|k| e.kind() == k)
        {
            let distance = e.pos().distance_to_sqr(center);
            if distance <= radius * radius {
                found.push((distance, e.uuid()));
            }
        }
    });
    found.sort_by(|a, b| a.0.total_cmp(&b.0));
    found.into_iter().map(|(_, uuid)| uuid).collect()
}

pub(super) fn entity_nbt(w: &PluginWorld<'_>, entity: u128) -> Result<Compound> {
    let entities = w.0.entities();
    crate::commands::entity_nbt(entities, any_entity(entities, entity)?).ok_or(status::NOT_FOUND)
}

/// `/data merge entity`; players refuse it as in Vanilla.
pub(super) fn merge_entity_nbt(
    w: &mut PluginWorld<'_>,
    entity: u128,
    nbt: &Compound,
) -> Result<()> {
    let entities = w.0.entities_mut();
    let id = any_entity(entities, entity)?;
    if entities.player(id).is_some() {
        return Err(status::INVALID_ARGUMENT);
    }
    let old = crate::commands::entity_nbt(entities, id).ok_or(status::NOT_FOUND)?;
    let mut merged = old.clone();
    crate::commands::merge_compound(&mut merged, nbt);
    if merged == old {
        return Ok(());
    }
    crate::commands::apply_entity_nbt(entities, id, &merged).map_err(|_| status::INVALID_ARGUMENT)
}

/// A `minecraft:mob_effect` by id.
fn mob_effect(name: &str) -> Result<MobEffect> {
    vanillite_data::registry::MOB_EFFECT
        .id(&qualified(name))
        .and_then(|id| MobEffect::from_id(i32::from(id)))
        .ok_or(status::INVALID_ARGUMENT)
}

/// `/effect give`: `INVALID_ARGUMENT` for an entity that cannot have the effect. A weaker
/// effect than the one it has changes nothing, as in Vanilla.
pub(super) fn add_effect(
    w: &mut PluginWorld<'_>,
    entity: u128,
    effect: &str,
    duration: i32,
    amplifier: i32,
    flags: u32,
) -> Result<()> {
    use vanillite_game::effect::instance::INFINITE;
    let known = effect_flag::AMBIENT | effect_flag::HIDE_PARTICLES | effect_flag::HIDE_ICON;
    if (duration <= 0 && duration != INFINITE)
        || !(0..=255).contains(&amplifier)
        || flags & !known != 0
    {
        return Err(status::INVALID_ARGUMENT);
    }
    let effect = mob_effect(effect)?;
    let entities = w.0.entities_mut();
    let id = any_entity(entities, entity)?;
    let kind = entities.store.get(id).ok_or(status::NOT_FOUND)?.kind();
    if entities.effects(id).is_none()
        || !vanillite_game::effect::can_be_affected(kind, effect)
        || vanillite_game::effect::refuses_all_effects(kind)
    {
        return Err(status::INVALID_ARGUMENT);
    }
    let instance = EffectInstance::with_flags(
        effect,
        duration,
        amplifier,
        flags & effect_flag::AMBIENT != 0,
        flags & effect_flag::HIDE_PARTICLES == 0,
        flags & effect_flag::HIDE_ICON == 0,
    );
    vanillite_game::effect::living::add_queued(entities, id, instance);
    Ok(())
}

/// `/effect clear`: one effect or all of them; removing what the entity does not have is no
/// error.
pub(super) fn remove_effect(
    w: &mut PluginWorld<'_>,
    entity: u128,
    effect: Option<&str>,
) -> Result<()> {
    let only = effect.map(mob_effect).transpose()?;
    let entities = w.0.entities_mut();
    let id = any_entity(entities, entity)?;
    let effects = entities.effects_mut(id).ok_or(status::INVALID_ARGUMENT)?;
    match only {
        Some(effect) => effects.remove(effect),
        None => effects.clear(),
    };
    Ok(())
}

/// An item stack from its NBT form; `INVALID_ARGUMENT` for an unknown item.
fn stack(item: &Compound) -> Result<ItemStack> {
    let stack = vanillite_game::inventory::stack_from_nbt(item);
    if stack.is_empty() {
        Err(status::INVALID_ARGUMENT)
    } else {
        Ok(stack)
    }
}

/// `/give`: the count may go up to a hundred stacks; what does not fit drops for the player.
pub(super) fn give_item(w: &mut PluginWorld<'_>, player: u128, item: &Compound) -> Result<()> {
    let prototype = stack(item)?.with_count(1);
    let count = match item.get("count") {
        None => 1,
        Some(tag) => tag
            .as_i64()
            .and_then(|n| i32::try_from(n).ok())
            .ok_or(status::INVALID_ARGUMENT)?,
    };
    let max = vanillite_game::item::max_stack_size(&prototype);
    if count < 1 || count > max.saturating_mul(100) {
        return Err(status::INVALID_ARGUMENT);
    }
    let level = w.0.level_mut();
    let id = player_id(&level.entities, player)?;
    items::give_to(level, id, &prototype, count, max);
    Ok(())
}

/// The slot of an entity named as `/item` names it (`SlotArgument`: one slot only).
fn entity_slot(w: &PluginWorld<'_>, id: EntityId, name: &str) -> Result<EntitySlot> {
    let source = server_source(w, Vec3::new(0.0, 0.0, 0.0))?;
    let limits = Limits::default();
    let mut reader = StringReader::new(name);
    let parsed = ArgumentType::ItemSlot.parse(
        &mut reader,
        ParseCtx {
            source: &source,
            limits: &limits,
        },
    );
    match parsed {
        Ok(ArgValue::Game(GameValue::Slot(index))) if !reader.can_read() => {
            items::entity_slot(w.0.entities(), id, index).ok_or(status::INVALID_ARGUMENT)
        }
        _ => Err(status::INVALID_ARGUMENT),
    }
}

pub(super) fn item(w: &PluginWorld<'_>, entity: u128, slot: &str) -> Result<Compound> {
    let id = any_entity(w.0.entities(), entity)?;
    let slot = entity_slot(w, id, slot)?;
    let stack = items::slot_get(w.0.entities(), id, slot);
    Ok(if stack.is_empty() {
        Compound::new()
    } else {
        vanillite_game::inventory::stack_to_nbt(&stack)
    })
}

/// `/item replace entity`: `INVALID_ARGUMENT` when the slot does not take the item (armour of
/// another kind).
pub(super) fn set_item(
    w: &mut PluginWorld<'_>,
    entity: u128,
    slot: &str,
    item: &Compound,
) -> Result<()> {
    let id = any_entity(w.0.entities(), entity)?;
    let slot = entity_slot(w, id, slot)?;
    let stack = if item.is_empty() {
        ItemStack::EMPTY
    } else {
        stack(item)?
    };
    let entities = w.0.entities_mut();
    if !items::slot_set(entities, id, slot, stack) {
        return Err(status::INVALID_ARGUMENT);
    }
    items::sync_inventory(entities, id);
    Ok(())
}

pub(super) fn block_entity_nbt(w: &PluginWorld<'_>, [x, y, z]: [i32; 3]) -> Result<Compound> {
    w.0.level()
        .block_entity_nbt(BlockPos::new(x, y, z))
        .ok_or(status::NOT_FOUND)
}

/// `/data merge block`.
pub(super) fn merge_block_entity_nbt(
    w: &mut PluginWorld<'_>,
    [x, y, z]: [i32; 3],
    nbt: &Compound,
) -> Result<()> {
    let pos = BlockPos::new(x, y, z);
    let level = w.0.level_mut();
    let old = level.block_entity_nbt(pos).ok_or(status::NOT_FOUND)?;
    let mut merged = old.clone();
    crate::commands::merge_compound(&mut merged, nbt);
    if merged == old || level.load_block_entity_nbt(pos, &merged) {
        Ok(())
    } else {
        Err(status::INVALID_ARGUMENT)
    }
}

/// `/advancement (grant|revoke) <player> only <advancement> [<criterion>]`, with the rewards
/// of what it completes.
pub(super) fn set_advancement(
    w: &mut PluginWorld<'_>,
    player: u128,
    advancement: &str,
    criterion: Option<&str>,
    grant: bool,
) -> Result<()> {
    let set = w.1.systems.advancements();
    let index = set
        .index(&qualified(advancement))
        .ok_or(status::INVALID_ARGUMENT)?;
    let criterion = match criterion {
        Some(name) => Some(
            set.get(index)
                .and_then(|a| a.criterion(name))
                .ok_or(status::INVALID_ARGUMENT)?,
        ),
        None => None,
    };
    let action = if grant { Grant::Grant } else { Grant::Revoke };
    let mut env = env(w);
    let id = player_id(&env.level.entities, player)?;
    crate::commands::perform_advancement(env.level, id, action, &[index], criterion, true);
    crate::progress::grant_rewards(&mut env, id);
    Ok(())
}

pub(super) fn advancement_done(
    w: &PluginWorld<'_>,
    player: u128,
    advancement: &str,
) -> Result<bool> {
    let entities = w.0.entities();
    let p = entities
        .player(player_id(entities, player)?)
        .ok_or(status::NOT_FOUND)?;
    let index = p
        .advancements
        .advancements()
        .index(&qualified(advancement))
        .ok_or(status::INVALID_ARGUMENT)?;
    Ok(p.advancements.is_done(index))
}
