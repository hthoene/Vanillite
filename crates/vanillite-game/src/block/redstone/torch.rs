//! Redstone torches and wall torches (`RedstoneTorchBlock`, `RedstoneWallTorchBlock`) with
//! burnout: a torch that turns off 8 times within 60 ticks stays off for 160 ticks.

use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_worldgen::behaviour::{PosExt, StateExt};

use crate::level::Level;
use crate::level::signal::DIRECTIONS;

/// `RECENT_TOGGLE_TIMER`.
const RECENT_TOGGLE_TIMER: i64 = 60;
/// `MAX_RECENT_TOGGLES`.
const MAX_RECENT_TOGGLES: usize = 8;
/// `RESTART_DELAY`.
const RESTART_DELAY: i32 = 160;
/// `TOGGLE_DELAY`.
const TOGGLE_DELAY: i32 = 2;

fn lit(state: BlockState) -> bool {
    state.flag("lit")
}

fn is_wall(state: BlockState) -> bool {
    state.is(ids::REDSTONE_WALL_TORCH)
}

/// `ownSignal`.
fn own_signal(state: BlockState) -> i32 {
    if lit(state) { 15 } else { 0 }
}

/// `getSignal`: everywhere but into the block it is attached to.
pub(super) fn signal(_: &Level, state: BlockState, _: BlockPos, direction: Direction) -> i32 {
    let attached = if is_wall(state) {
        state.direction("facing").unwrap_or(Direction::North)
    } else {
        Direction::Up
    };
    if direction != attached {
        own_signal(state)
    } else {
        0
    }
}

/// `getDirectSignal`: only into the block above.
pub(super) fn direct_signal(
    level: &Level,
    state: BlockState,
    pos: BlockPos,
    direction: Direction,
) -> i32 {
    if direction == Direction::Down {
        signal(level, state, pos, direction)
    } else {
        0
    }
}

/// `hasNeighborSignal`: the block the torch is attached to is powered.
fn has_neighbor_signal(level: &Level, pos: BlockPos, state: BlockState) -> bool {
    if is_wall(state) {
        let opposite = state
            .direction("facing")
            .unwrap_or(Direction::North)
            .opposite();
        level.has_signal(pos.relative(opposite), opposite)
    } else {
        level.has_signal(pos.below(), Direction::Down)
    }
}

/// `notifyNeighbors`.
fn notify_neighbors(level: &mut Level, pos: BlockPos, state: BlockState) {
    for direction in DIRECTIONS {
        level.update_neighbors_at(pos.relative(direction), state.block());
    }
}

/// `onPlace`.
pub(super) fn on_place(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    _: BlockState,
    _: bool,
) {
    notify_neighbors(level, pos, state);
}

/// `affectNeighborsAfterRemoval`.
pub(super) fn affect_neighbors_after_removal(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    moved: bool,
) {
    if !moved {
        notify_neighbors(level, pos, state);
    }
}

/// `neighborChanged`.
pub(super) fn neighbor_changed(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    _: BlockId,
    _: bool,
) {
    if lit(state) == has_neighbor_signal(level, pos, state)
        && !level.chunks.block_tick_runs_this_tick(pos, state.block().0)
    {
        level.schedule_block_tick(pos, state.block(), TOGGLE_DELAY);
    }
}

/// `tick`.
pub(super) fn tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    let signal = has_neighbor_signal(level, pos, state);
    let now = level.game_time();
    let expired = level
        .torch_toggles
        .iter()
        .take_while(|(_, when)| now - when > RECENT_TOGGLE_TIMER)
        .count();
    level.torch_toggles.drain(..expired);
    if lit(state) {
        if signal {
            level.set_block_and_update(pos, state.set_flag("lit", false));
            if toggled_too_frequently(level, pos, true) {
                level.level_event(None, 1502, pos, 0);
                let block = level.block(pos).block();
                level.schedule_block_tick(pos, block, RESTART_DELAY);
            }
        }
    } else if !signal && !toggled_too_frequently(level, pos, false) {
        level.set_block_and_update(pos, state.set_flag("lit", true));
    }
}

/// `isToggledTooFrequently`.
fn toggled_too_frequently(level: &mut Level, pos: BlockPos, add: bool) -> bool {
    if add {
        let now = level.game_time();
        level.torch_toggles.push((pos, now));
    }
    level
        .torch_toggles
        .iter()
        .filter(|(p, _)| *p == pos)
        .take(MAX_RECENT_TOGGLES)
        .count()
        >= MAX_RECENT_TOGGLES
}
