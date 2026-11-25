//! Bells (`BellBlock`, `BellBlockEntity`): they ring when they get a signal or are hit on the
//! right side. Living entities within 32 blocks hear the ring (`HEARD_BELL_TIME`); raiders near
//! a ringing bell make it resonate and then glow.

use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_world::entity::EntityId;
use vanillite_worldgen::behaviour::StateExt;

use super::{BlockBehaviour, Interaction, UseContext, sound_id};
use crate::level::game_event::events;
use crate::level::{BlockEntity, Level, SOUND_BLOCKS};

/// `BellBlock.EVENT_BELL_RING`.
const EVENT_BELL_RING: i32 = 1;

/// The data of a `BellBlockEntity`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bell {
    /// `lastRingTimestamp`.
    pub last_ring: i64,
    /// `nearbyEntities`: the living entities within 48 blocks at the last search.
    pub nearby: Option<Vec<EntityId>>,
    /// `resonating`.
    pub resonating: bool,
    /// `ticks`: ticks since the last ring while shaking.
    pub ticks: i32,
    /// `shaking`.
    pub shaking: bool,
    /// `clickDirection`.
    pub click_direction: Option<Direction>,
    /// `resonationTicks`.
    pub resonation_ticks: i32,
}

/// Fills the bell behaviours.
pub(super) fn fill(block: BlockId, b: &mut BlockBehaviour) {
    if block == ids::BELL {
        b.neighbor_changed = Some(neighbor_changed);
        b.trigger_event = Some(trigger_event);
        b.use_without_item = Some(use_without_item);
    }
}

/// `Direction.from3DDataValue(value)`.
fn from_3d(value: i32) -> Direction {
    const ALL: [Direction; 6] = [
        Direction::Down,
        Direction::Up,
        Direction::North,
        Direction::South,
        Direction::West,
        Direction::East,
    ];
    ALL[value.rem_euclid(6) as usize]
}

/// `BellBlock.neighborChanged`: a rising signal rings the bell.
fn neighbor_changed(level: &mut Level, state: BlockState, pos: BlockPos, _: BlockId, _: bool) {
    let signal = level.has_neighbor_signal(pos);
    if signal != state.flag("powered") {
        if signal {
            attempt_to_ring(level, None, pos, None);
        }
        level.set_block_and_update(pos, state.set_flag("powered", signal));
    }
}

/// `BellBlock.useWithoutItem`: a hit on the right side rings it.
fn use_without_item(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    context: &UseContext,
) -> Interaction {
    let click_y = context.hit.y - f64::from(pos.y);
    if on_hit(
        level,
        state,
        pos,
        context.face,
        click_y,
        Some(context.player),
    ) {
        Interaction::Success
    } else {
        Interaction::Pass
    }
}

/// `BellBlock.onHit(level, state, hitResult, player, requireHitFromCorrectSide = true)`: also
/// what projectiles call (`onProjectileHit`). Returns whether the hit was on a ringing side.
pub fn on_hit(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    face: Direction,
    click_y: f64,
    player: Option<EntityId>,
) -> bool {
    if is_proper_hit(state, face, click_y) {
        attempt_to_ring(level, player, pos, Some(face));
        true
    } else {
        false
    }
}

/// `BellBlock.isProperHit`.
fn is_proper_hit(state: BlockState, face: Direction, click_y: f64) -> bool {
    let vertical = |d: Direction| matches!(d, Direction::Up | Direction::Down);
    let x_axis = |d: Direction| matches!(d, Direction::West | Direction::East);
    if vertical(face) || click_y > f64::from(0.8124f32) {
        return false;
    }
    let facing = state.direction("facing").unwrap_or(Direction::North);
    let same_axis = x_axis(facing) == x_axis(face);
    match state.value("attachment") {
        Some("floor") => same_axis,
        Some("single_wall" | "double_wall") => !same_axis,
        Some("ceiling") => true,
        _ => false,
    }
}

/// `BellBlock.attemptToRing(ringingEntity, level, pos, direction)`: also what explosions call
/// (`onExplosionHit`, when the explosion triggers blocks).
pub fn attempt_to_ring(
    level: &mut Level,
    entity: Option<EntityId>,
    pos: BlockPos,
    direction: Option<Direction>,
) -> bool {
    let state = level.block(pos);
    let Some(BlockEntity::Bell(bell)) = level.block_entity_mut(pos) else {
        return false;
    };
    let direction = direction
        .or_else(|| state.direction("facing"))
        .unwrap_or(Direction::North);
    // `BellBlockEntity.onHit`.
    bell.click_direction = Some(direction);
    if bell.shaking {
        bell.ticks = 0;
    } else {
        bell.shaking = true;
    }
    level.block_event(pos, state.block(), EVENT_BELL_RING, direction as i32);
    level.play_block_sound(
        None,
        pos,
        sound_id("block.bell.use"),
        SOUND_BLOCKS,
        2.0,
        1.0,
    );
    level.game_event_at(entity, events().block_change, pos);
    true
}

/// `BellBlock.triggerEvent` → `BellBlockEntity.triggerEvent`.
fn trigger_event(level: &mut Level, _: BlockState, pos: BlockPos, b0: i32, b1: i32) -> bool {
    if !matches!(level.block_entity(pos), Some(BlockEntity::Bell(_))) {
        return false;
    }
    if b0 != EVENT_BELL_RING {
        return false;
    }
    update_entities(level, pos);
    let Some(BlockEntity::Bell(bell)) = level.block_entity_mut(pos) else {
        return false;
    };
    bell.resonation_ticks = 0;
    bell.click_direction = Some(from_3d(b1));
    bell.ticks = 0;
    bell.shaking = true;
    true
}

/// `BellBlockEntity.serverTick`: shaking, then the resonance that makes raiders glow.
pub fn tick(level: &mut Level, pos: BlockPos, _: BlockState) {
    let Some(BlockEntity::Bell(bell)) = level.block_entity_mut(pos) else {
        return;
    };
    if bell.shaking {
        bell.ticks += 1;
    }
    if bell.ticks >= 50 {
        bell.shaking = false;
        bell.ticks = 0;
    }
    let (ticks, resonation) = (bell.ticks, bell.resonation_ticks);
    let nearby = bell.nearby.clone().unwrap_or_default();
    if ticks >= 5 && resonation == 0 && raiders_near(level, pos, &nearby, 32.0) {
        if let Some(BlockEntity::Bell(bell)) = level.block_entity_mut(pos) {
            bell.resonating = true;
        }
        level.play_block_sound(
            None,
            pos,
            sound_id("block.bell.resonate"),
            SOUND_BLOCKS,
            1.0,
            1.0,
        );
    }
    let Some(BlockEntity::Bell(bell)) = level.block_entity_mut(pos) else {
        return;
    };
    if bell.resonating {
        if bell.resonation_ticks < 40 {
            bell.resonation_ticks += 1;
        } else {
            bell.resonating = false;
            make_raiders_glow(level, pos, &nearby);
        }
    }
}

/// `BlockPos.closerToCenterThan(pos, dist)`.
fn closer_to_center(level: &Level, pos: BlockPos, id: EntityId, dist: f64) -> bool {
    level.entities.store.get(id).is_some_and(|e| {
        let p = e.pos();
        let c = vanillite_base::Vec3::new(
            f64::from(pos.x) + 0.5,
            f64::from(pos.y) + 0.5,
            f64::from(pos.z) + 0.5,
        );
        c.distance_to_sqr(p) < dist * dist
    })
}

/// A living raider (`#raiders`) of the bell's list within `dist`.
fn is_raider_near(level: &Level, pos: BlockPos, id: EntityId, dist: f64) -> bool {
    level
        .entities
        .mob(id)
        .is_some_and(|m| m.is_alive() && !m.removed && m.kind.is_raider())
        && closer_to_center(level, pos, id, dist)
}

/// `areRaidersNearby`.
fn raiders_near(level: &Level, pos: BlockPos, nearby: &[EntityId], dist: f64) -> bool {
    nearby
        .iter()
        .any(|&id| is_raider_near(level, pos, id, dist))
}

/// `makeRaidersGlow`: Glowing for 3 seconds on the raiders within 48 blocks.
fn make_raiders_glow(level: &mut Level, pos: BlockPos, nearby: &[EntityId]) {
    for &id in nearby {
        if is_raider_near(level, pos, id, 48.0) {
            crate::effect::living::add_detached(
                &mut level.entities,
                id,
                crate::effect::EffectInstance::new(crate::effect::MobEffect::GLOWING, 60, 0),
            );
        }
    }
}

/// `updateEntities`: the living entities within 48 blocks (searched again at most every 3
/// seconds); those within 32 blocks that think with a brain remember hearing the bell.
fn update_entities(level: &mut Level, pos: BlockPos) {
    let now = level.game_time();
    let Some(BlockEntity::Bell(bell)) = level.block_entity(pos) else {
        return;
    };
    if now > bell.last_ring + 60 || bell.nearby.is_none() {
        let area = vanillite_base::Aabb::new(
            f64::from(pos.x) - 48.0,
            f64::from(pos.y) - 48.0,
            f64::from(pos.z) - 48.0,
            f64::from(pos.x) + 49.0,
            f64::from(pos.y) + 49.0,
            f64::from(pos.z) + 49.0,
        );
        let ents = &level.entities;
        let mut found = Vec::new();
        ents.store.for_each_in(&area, false, |id| {
            if ents.mob(id).is_some() || ents.player(id).is_some() {
                found.push(id);
            }
        });
        if let Some(BlockEntity::Bell(bell)) = level.block_entity_mut(pos) {
            bell.last_ring = now;
            bell.nearby = Some(found);
        }
    }
    let nearby = match level.block_entity(pos) {
        Some(BlockEntity::Bell(bell)) => bell.nearby.clone().unwrap_or_default(),
        _ => return,
    };
    for id in nearby {
        if !closer_to_center(level, pos, id, 32.0) {
            continue;
        }
        if let Some(m) = level.entities.mob_mut(id)
            && m.is_alive()
            && !m.removed
            && let Some(brain) = m.brain.as_deref_mut()
            && brain.is_registered(crate::mob::brain::MemoryType::HeardBellTime)
        {
            brain.set(
                crate::mob::brain::MemoryType::HeardBellTime,
                crate::mob::brain::MemValue::Long(now),
            );
        }
    }
}
