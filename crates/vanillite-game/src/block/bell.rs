//! Bells (`BellBlock`, `BellBlockEntity`): they ring when they get a signal or are hit on the
//! right side. Raids do not exist yet, so a ring makes no raider glow.

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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Bell {
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
    let Some(BlockEntity::Bell(bell)) = level.block_entity_mut(pos) else {
        return false;
    };
    if b0 != EVENT_BELL_RING {
        return false;
    }
    bell.resonation_ticks = 0;
    bell.click_direction = Some(from_3d(b1));
    bell.ticks = 0;
    bell.shaking = true;
    true
}

/// `BellBlockEntity.serverTick`.
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
}
