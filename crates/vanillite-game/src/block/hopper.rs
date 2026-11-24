//! Hoppers (`HopperBlock`, `HopperBlockEntity`).
//!
//! A hopper is enabled while it receives no signal. Its block entity ticks every tick: the
//! transfer cooldown counts down and, once it is over, the hopper tries to push one item out and
//! pull one in, waiting 8 ticks after a transfer. Containers do not exist yet: [`transfer`] is
//! the seam that moves items and moves none for now.

use vanillite_base::BlockPos;
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_worldgen::behaviour::StateExt;

use super::{BlockBehaviour, dispenser};
use crate::level::{BlockEntity, Level, update};

/// The transfer timing of a hopper block entity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hopper {
    /// `cooldownTime`: ticks until the next transfer attempt (`TransferCooldown`).
    pub cooldown_time: i32,
    /// `tickedGameTime`: the game time of the last tick.
    pub ticked_game_time: i64,
}

impl Default for Hopper {
    fn default() -> Self {
        Hopper {
            cooldown_time: -1,
            ticked_game_time: 0,
        }
    }
}

/// `HopperBlockEntity.MOVE_ITEM_SPEED`.
const MOVE_ITEM_SPEED: i32 = 8;

/// Fills the hopper behaviours of `block`.
pub(super) fn fill(block: BlockId, b: &mut BlockBehaviour) {
    if block == ids::HOPPER {
        b.on_place = Some(on_place);
        b.neighbor_changed = Some(neighbor_changed);
        b.affect_neighbors_after_removal = Some(dispenser::container_removed);
        b.analog_output = Some(dispenser::container_signal);
    }
}

/// `HopperBlock.onPlace`.
fn on_place(level: &mut Level, state: BlockState, pos: BlockPos, old: BlockState, _moved: bool) {
    if old.block() != state.block() {
        check_powered_state(level, pos, state);
    }
}

/// `HopperBlock.neighborChanged`.
fn neighbor_changed(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    _source: BlockId,
    _moved: bool,
) {
    check_powered_state(level, pos, state);
}

/// `HopperBlock.checkPoweredState`: enabled without a signal.
fn check_powered_state(level: &mut Level, pos: BlockPos, state: BlockState) {
    let should_be_on = !level.has_neighbor_signal(pos);
    if should_be_on != state.flag("enabled") {
        level.set_block(
            pos,
            state.set_flag("enabled", should_be_on),
            update::CLIENTS,
        );
    }
}

/// `HopperBlockEntity.pushItemsTick`.
pub fn push_items_tick(level: &mut Level, pos: BlockPos, state: BlockState) {
    let game_time = level.game_time();
    let Some(BlockEntity::Hopper(hopper)) = level.block_entity_mut(pos) else {
        return;
    };
    hopper.cooldown_time -= 1;
    hopper.ticked_game_time = game_time;
    if hopper.cooldown_time > 0 {
        return;
    }
    hopper.cooldown_time = 0;
    // `tryMoveItems`.
    if state.flag("enabled") && transfer(level, pos, state) {
        if let Some(BlockEntity::Hopper(hopper)) = level.block_entity_mut(pos) {
            hopper.cooldown_time = MOVE_ITEM_SPEED;
        }
        // `setChanged`: comparators re-read the hopper.
        level.update_neighbour_for_output_signal(pos, state.block());
    }
}

/// `ejectItems` and `suckInItems` of `tryMoveItems`: whether an item moved. Seam for the
/// containers (which also set the cooldown of a receiving hopper, `8 - skipTickCount`): hoppers
/// hold no items yet, so nothing moves.
pub fn transfer(level: &mut Level, pos: BlockPos, state: BlockState) -> bool {
    let _ = (level, pos, state);
    false
}
