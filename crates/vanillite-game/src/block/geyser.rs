//! Potent sulfur geysers (`PotentSulfurBlock`, `PotentSulfurBlockEntity`): under water a wet
//! block gives nausea to the living entities in the gas above it; on a periodic source below
//! it erupts and rests in turns, on a continuous source it erupts all the time, launching the
//! entities in the water column upwards.

use vanillite_base::random::{PositionalRandomFactory, RandomSource, XoroshiroRandom};
use vanillite_base::{Aabb, BlockPos, Vec3};
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_world::entity::EntityId;
use vanillite_worldgen::behaviour::{PosExt, StateExt};

use super::{BlockBehaviour, sound_id};
use crate::level::game_event::{GameEventContext, events};
use crate::level::{BlockEntity, Level, SOUND_BLOCKS};

const STATE: &str = "potent_sulfur_state";
/// `GEYSER_SALT`.
const GEYSER_SALT: i64 = -904_011_478;

pub(super) fn fill(block: BlockId, b: &mut BlockBehaviour) {
    if block == ids::POTENT_SULFUR {
        b.on_place = Some(on_place);
        // `triggerEvent` sets the client's eruption start; the event reaches the clients.
        b.trigger_event = Some(|_, _, _, _, _| true);
    }
}

/// `onPlace`: an erupting geyser announces its start.
fn on_place(level: &mut Level, state: BlockState, pos: BlockPos, _old: BlockState, _: bool) {
    let continuous = match state.value(STATE) {
        Some("erupting") => false,
        Some("continuous") => true,
        _ => return,
    };
    level.block_event(pos, ids::POTENT_SULFUR, 0, 0);
    let sound = if continuous {
        "block.potent_sulfur.geyser_continuous_eruption"
    } else {
        "block.potent_sulfur.geyser_eruption"
    };
    level.play_block_sound(None, pos, sound_id(sound), SOUND_BLOCKS, 1.0, 1.0);
    let context = GameEventContext::with_state(None, state);
    level.game_event_block(events().block_activate, pos, context);
}

/// The ticker of the block entity by the block's state (`getTicker`).
pub fn tick(level: &mut Level, pos: BlockPos, state: BlockState) {
    match state.value(STATE) {
        Some("wet") => nausea_tick(level, pos),
        Some("dormant") => {
            countdown_tick(level, pos, state);
            nausea_tick(level, pos);
        }
        Some("erupting") => {
            launch_tick(level, pos);
            countdown_tick(level, pos, state);
        }
        Some("continuous") => launch_tick(level, pos),
        _ => {}
    }
}

/// `isGeyserPassableBlock`: air, water and blocks without collision.
fn passable(state: BlockState) -> bool {
    state.is_air() || state.is(ids::WATER) || state.collision_shape().is_empty()
}

/// `fluid.isSourceOfType(WATER)`.
fn water_source(level: &Level, pos: BlockPos) -> bool {
    let fluid = level.fluid(pos);
    !fluid.is_empty() && fluid.info().source && super::sculk_catalyst::is_water(level.block(pos))
}

/// `findNoxiousGasSourceBlock`: the first passable non-water block above the water column (at
/// most 4 water blocks high).
fn gas_source(level: &Level, origin: BlockPos) -> Option<BlockPos> {
    let max_y = origin.y + 4 + 1;
    let mut pos = origin.above();
    while pos.y <= max_y {
        let state = level.block(pos);
        let waterlogged = water_source(level, pos);
        if !waterlogged || (!state.is(ids::WATER) && !passable(state)) {
            if state.is_air() || passable(state) {
                return Some(pos);
            }
            break;
        }
        pos = pos.above();
    }
    None
}

/// `SERVER_NAUSEA_EFFECT_TICKER`: every 10 ticks the living entities in reach of the gas get
/// nausea for 4 seconds.
fn nausea_tick(level: &mut Level, pos: BlockPos) {
    if level.game_time() % 10 != 0 {
        return;
    }
    let Some(source) = gas_source(level, pos) else {
        return;
    };
    let (x, y, z) = (
        f64::from(source.x),
        f64::from(source.y),
        f64::from(source.z),
    );
    let area = Aabb::new(x - 2.5, y, z - 2.5, x + 3.5, y + 1.0, z + 3.5);
    let mut found: Vec<EntityId> = Vec::new();
    level
        .entities
        .store
        .for_each_in(&area, false, |id| found.push(id));
    for id in found {
        let Some(eye) = living_eye(level, id) else {
            continue;
        };
        if can_be_reached(level, source, eye) {
            let effect = crate::effect::EffectInstance::with_flags(
                crate::effect::MobEffect::NAUSEA,
                80,
                0,
                true,
                true,
                true,
            );
            level.entities.add_effect(id, effect, true);
        }
    }
}

/// The eye of a living entity that is alive and no spectator.
fn living_eye(level: &Level, id: EntityId) -> Option<Vec3> {
    let e = level.entities.store.get(id)?;
    let pos = e.pos();
    if let Some(m) = level.entities.mob(id) {
        return m
            .is_alive()
            .then(|| Vec3::new(pos.x, pos.y + crate::mob::eye_height(m), pos.z));
    }
    let p = level.entities.player(id)?;
    if p.game_mode == vanillite_config::GameMode::Spectator || p.survival.health <= 0.0 {
        return None;
    }
    let eye = crate::survival::player::eye_height(e.dims().height);
    Some(Vec3::new(pos.x, pos.y + eye, pos.z))
}

/// `canBeReachedByNoxiousGas`: the eye is in a passable block within 3 blocks of the gas, with
/// water under it in sight of the water under the gas.
fn can_be_reached(level: &Level, source: BlockPos, eye: Vec3) -> bool {
    let block = BlockPos::containing(eye.x, eye.y, eye.z);
    if !passable(level.block(block)) {
        return false;
    }
    let center = |p: BlockPos| {
        Vec3::new(
            f64::from(p.x) + 0.5,
            f64::from(p.y) + 0.5,
            f64::from(p.z) + 0.5,
        )
    };
    if eye.distance_to_sqr(center(source)) > 9.0 {
        return false;
    }
    let below_source = center(source.below());
    let below = Vec3::new(eye.x, eye.y - 1.0, eye.z);
    water_source(level, BlockPos::containing(below.x, below.y, below.z))
        && crate::mob::physics::clip(&level.view(), below_source, below).is_none()
}

/// `geyserPositional(level, pos)`.
fn positional(level: &Level, pos: BlockPos) -> XoroshiroRandom {
    XoroshiroRandom::new(level.seed() ^ GEYSER_SALT)
        .fork_positional()
        .at(pos.x, pos.y, pos.z)
}

/// `SERVER_WAITING_COUNTDOWN_TICKER`: every second the countdown runs down; at its end a
/// dormant geyser erupts and an erupting one rests.
fn countdown_tick(level: &mut Level, pos: BlockPos, state: BlockState) {
    if level.game_time() % 20 != 0 {
        return;
    }
    let Some(source) = gas_source(level, pos) else {
        return;
    };
    let dormant = state.value(STATE) == Some("dormant");
    let Some(mut countdown) = (match level.block_entity(pos) {
        Some(BlockEntity::PotentSulfur { countdown }) => Some(*countdown),
        _ => None,
    }) else {
        return;
    };
    if countdown <= 0 {
        let water_blocks = source.y - pos.y - 1;
        let mut random = positional(level, pos);
        countdown = if dormant {
            10 * (water_blocks - 1) + random.next_int_between_inclusive(15, 30)
        } else {
            let _ = random.next_int();
            water_blocks - 1 + random.next_int_between_inclusive(1, 2)
        };
    }
    if countdown > 0 {
        countdown -= 1;
    }
    if let Some(BlockEntity::PotentSulfur { countdown: c }) = level.block_entity_mut(pos) {
        *c = countdown;
    }
    if countdown == 0 {
        let next = if dormant { "erupting" } else { "dormant" };
        level.set_block_and_update(pos, state.set(STATE, next));
        if !dormant {
            let context = GameEventContext::with_state(None, state);
            level.game_event_block(events().block_deactivate, pos, context);
        }
    }
}

/// `LAUNCH_ENTITY_TICKER`: the entities in the free column above the block (6 blocks per
/// water block) are pushed up until they rise fast enough.
fn launch_tick(level: &mut Level, pos: BlockPos) {
    let Some(source) = gas_source(level, pos) else {
        return;
    };
    let water_blocks = source.y - pos.y - 1;
    // `getUnobstructedBlockCount`.
    let limit = 6 * water_blocks;
    let start = pos.above();
    let height = (0..limit)
        .find(|&i| !passable(level.block(start.offset(0, i, 0))))
        .unwrap_or(limit);
    let (x, y, z) = (f64::from(start.x), f64::from(start.y), f64::from(start.z));
    // `new AABB(pos.above()).expandTowards(0, height - 1, 0)`.
    let grow = f64::from(height - 1);
    let area = Aabb::new(
        x,
        y + grow.min(0.0),
        z,
        x + 1.0,
        y + 1.0 + grow.max(0.0),
        z + 1.0,
    );
    let mut found: Vec<EntityId> = Vec::new();
    level
        .entities
        .store
        .for_each_in(&area, false, |id| found.push(id));
    let threshold = f64::from(0.3_f32) + f64::from(water_blocks) * 0.1;
    let immune = |kind| {
        crate::player::play::game_data().is_some_and(|d| {
            crate::predicate::EntityTags::entity_in_tag(
                &*d,
                kind,
                "minecraft:not_affected_by_geysers",
            )
        })
    };
    for id in found {
        let Some(kind) = level.entities.store.get(id).map(|e| e.kind()) else {
            continue;
        };
        if level.entities.vehicle_of(id).is_some() || immune(kind) {
            continue;
        }
        let velocity = match level.entities.store.get(id) {
            Some(e) => e.velocity(),
            None => continue,
        };
        // `checkFallDistanceAccumulation`.
        if let Some(m) = level.entities.mob_mut(id) {
            if velocity.y > -0.5 && m.fall_distance > 1.0 {
                m.fall_distance = 1.0;
            }
            if !m.is_alive() {
                continue;
            }
        }
        if let Some(p) = level.entities.player(id) {
            // Clients move their players (`canSimulateMovement`); flying players stay.
            if !p.survival.simulate_movement
                || p.survival.flying
                || p.game_mode == vanillite_config::GameMode::Spectator
            {
                continue;
            }
        }
        if velocity.y < threshold
            && let Some(mut e) = level.entities.store.get_mut(id)
        {
            e.set_velocity(velocity + Vec3::new(0.0, f64::from(0.2_f32), 0.0));
        }
    }
}
