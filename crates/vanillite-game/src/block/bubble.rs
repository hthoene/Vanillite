//! Bubble columns (`BubbleColumnBlock`) and the water above soul sand and magma that grows them
//! (`LiquidBlock.onPlace`, `neighborChanged` and `tick`).
//!
//! A full water source whose block below enables bubble columns schedules a block tick in 20
//! ticks; the tick turns it and the sources above into bubble columns (`updateColumn`). A bubble
//! column checks itself again 5 ticks after a change below it.

use vanillite_base::BlockPos;
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_worldgen::behaviour::{Class, PosExt, StateExt, class_of, tags};

use crate::level::{Level, update};

/// `LiquidBlock.BUBBLE_COLUMN_CHECK_DELAY`.
const CHECK_DELAY: i32 = 20;

/// `LiquidBlock.shouldBubbleColumnOccupy`: a full source of a fluid bubble columns occupy.
fn should_occupy(state: BlockState) -> bool {
    let fluid = state.fluid_state();
    tags().fluids.bubble_column_can_occupy.contains(fluid)
        && fluid.info().source
        && fluid.info().amount == 8
}

/// Whether the block below makes a bubble column (`ENABLES_BUBBLE_COLUMN_*`).
fn enables_column(below: BlockState) -> bool {
    let t = &tags().blocks;
    t.enables_bubble_column_drag_down.contains(below)
        || t.enables_bubble_column_push_up.contains(below)
}

/// `LiquidBlock.tryScheduleBubbleBlockColumn`.
fn try_schedule(level: &mut Level, pos: BlockPos, block: BlockId) {
    if enables_column(level.block(pos.below())) {
        level.schedule_block_tick(pos, block, CHECK_DELAY);
    }
}

/// The bubble column part of `LiquidBlock.onPlace` and `neighborChanged`.
pub(super) fn liquid_check(level: &mut Level, state: BlockState, pos: BlockPos) {
    if should_occupy(state) {
        try_schedule(level, pos, state.block());
    }
}

/// `LiquidBlock.tick`: the source becomes a bubble column if the block below makes one.
pub(super) fn liquid_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    if should_occupy(state) {
        let below = level.block(pos.below());
        update_column(level, pos, state, below);
    }
}

/// `BubbleColumnBlock.tick`.
pub(super) fn column_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    let below = level.block(pos.below());
    update_column(level, pos, state, below);
}

/// `BubbleColumnBlock.canOccupy`.
fn can_occupy(state: BlockState) -> bool {
    if state.is(ids::BUBBLE_COLUMN) {
        return true;
    }
    let fluid = state.fluid_state();
    tags().fluids.bubble_column_can_occupy.contains(fluid)
        && class_of(state) == Class::Liquid
        && fluid.info().source
        && fluid.info().amount >= 8
}

/// `BubbleColumnBlock.getColumnState`.
fn column_state(below: BlockState, occupy: BlockState) -> BlockState {
    let t = &tags().blocks;
    if below.is(ids::BUBBLE_COLUMN) {
        below
    } else if t.enables_bubble_column_push_up.contains(below) {
        ids::BUBBLE_COLUMN.default_state().set_flag("drag", false)
    } else if t.enables_bubble_column_drag_down.contains(below) {
        ids::BUBBLE_COLUMN.default_state().set_flag("drag", true)
    } else if occupy.is(ids::BUBBLE_COLUMN) {
        ids::WATER.default_state()
    } else {
        occupy
    }
}

/// `BubbleColumnBlock.updateColumn(level, occupyAt, occupyState, belowState)`: the column from
/// `pos` up to the last source it can occupy.
pub fn update_column(level: &mut Level, pos: BlockPos, occupy: BlockState, below: BlockState) {
    if !can_occupy(occupy) {
        return;
    }
    let column = column_state(below, occupy);
    level.set_block(pos, column, update::CLIENTS);
    let mut p = pos.above();
    while can_occupy(level.block(p)) {
        if !level.set_block(p, column, update::CLIENTS) {
            return;
        }
        p = p.above();
    }
}
