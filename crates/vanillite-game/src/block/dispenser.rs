//! Dispensers and droppers (`DispenserBlock`, `DropperBlock`).
//!
//! A rising signal at the block or the block above sets `triggered` and schedules a tick in 4
//! ticks; the tick dispenses one item from a random non-empty slot. Containers do not exist yet:
//! every dispenser is empty, so the tick plays the "dispense failed" click
//! (`LevelEvent.SOUND_DISPENSER_FAIL`). [`dispense_slot`] is the seam the container
//! implementation fills; the item behaviours (`DispenseItemBehavior`, TNT, projectiles, buckets,
//! …) and the dropper's transfer into containers follow it.

use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_worldgen::behaviour::{PosExt, StateExt};

use super::BlockBehaviour;
use crate::level::{Level, update};

/// `LevelEvent.SOUND_DISPENSER_FAIL`.
const DISPENSE_FAIL: i32 = 1001;
/// `DispenserBlock.TRIGGER_DURATION`.
const TRIGGER_DURATION: i32 = 4;

/// Fills the dispenser and dropper behaviours of `block`.
pub(super) fn fill(block: BlockId, b: &mut BlockBehaviour) {
    if block == ids::DISPENSER || block == ids::DROPPER {
        b.neighbor_changed = Some(neighbor_changed);
        b.tick = Some(tick);
        b.affect_neighbors_after_removal = Some(container_removed);
        b.analog_output = Some(container_signal);
    }
}

/// `DispenserBlock.neighborChanged`.
fn neighbor_changed(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    _source: BlockId,
    _moved: bool,
) {
    let should_trigger = level.has_neighbor_signal(pos) || level.has_neighbor_signal(pos.above());
    let triggered = state.flag("triggered");
    if should_trigger && !triggered {
        level.schedule_block_tick(pos, state.block(), TRIGGER_DURATION);
        level.set_block(pos, state.set_flag("triggered", true), update::CLIENTS);
    } else if !should_trigger && triggered {
        level.set_block(pos, state.set_flag("triggered", false), update::CLIENTS);
    }
}

/// `DispenserBlock.tick` / `DropperBlock.dispenseFrom`.
fn tick(level: &mut Level, _state: BlockState, pos: BlockPos) {
    // The item behaviours come with containers.
    if dispense_slot(level, pos).is_none() {
        level.level_event(None, DISPENSE_FAIL, pos, 0);
    }
}

/// `DispenserBlockEntity.getRandomSlot(level.getRandom())`: a random non-empty slot (one
/// `nextInt` per non-empty slot), `None` if the dispenser is empty. Seam for the containers:
/// dispensers have no inventory yet and are always empty.
pub fn dispense_slot(level: &mut Level, pos: BlockPos) -> Option<usize> {
    let _ = (level, pos);
    None
}

/// `Containers.updateNeighboursAfterDestroy`: comparators re-read the removed container.
pub fn container_removed(level: &mut Level, state: BlockState, pos: BlockPos, _moved: bool) {
    level.update_neighbour_for_output_signal(pos, state.block());
}

/// `AbstractContainerMenu.getRedstoneSignalFromBlockEntity`: the fill level of the container
/// for comparators. Seam for the containers: 0 while containers have no items.
pub fn container_signal(
    _level: &Level,
    _state: BlockState,
    _pos: BlockPos,
    _direction: Direction,
) -> i32 {
    0
}
