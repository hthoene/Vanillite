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

/// `BubbleColumnBlock.entityInside` for an entity inside the column at `pos` (`isPrecise`):
/// whether it is at the column's top (nothing collides and no fluid above: `onAboveBubbleColumn`)
/// and the vertical speed the column gives it (`handleOnAboveBubbleColumn` /
/// `handleOnInsideBubbleColumn`).
pub fn drag(above: BlockState, state: BlockState, y: f64) -> (bool, f64) {
    let top = above.collision_shape().is_empty() && above.fluid_state().is_empty();
    let drag_down = state.value("drag") == Some("true");
    let y = match (top, drag_down) {
        (true, true) => (y - 0.03).max(-0.9),
        (true, false) => (y + 0.1).min(1.8),
        (false, true) => (y - 0.03).max(-0.3),
        (false, false) => (y + 0.06).min(0.7),
    };
    (top, y)
}

/// `Entity.sendBubbleColumnParticles`: splashes and bubbles at random spots on the top of the
/// column at `pos`, drawn from the level random.
pub fn send_particles(level: &mut Level, pos: BlockPos) {
    use crate::level::particles::simple;
    use vanillite_base::Vec3;
    use vanillite_base::random::RandomSource;
    let (x, y, z) = (f64::from(pos.x), f64::from(pos.y + 1), f64::from(pos.z));
    for _ in 0..2 {
        let at = Vec3::new(
            x + level.random().next_double(),
            y,
            z + level.random().next_double(),
        );
        level.send_particles(simple("minecraft:splash"), at, 1, Vec3::ZERO, 1.0);
        let at = Vec3::new(
            x + level.random().next_double(),
            y,
            z + level.random().next_double(),
        );
        level.send_particles(
            simple("minecraft:bubble"),
            at,
            1,
            Vec3::new(0.0, 0.01, 0.0),
            0.2,
        );
    }
}

/// The bubble columns on the way of an entity of `dims` that end inside it (`entityInside`
/// with `isPrecise`), applied to its speed `v`: the particles of the column tops it is above,
/// and whether it was inside a column below its top (which resets the fall distance).
pub fn along(
    level: &mut Level,
    dims: vanillite_world::entity::Dimensions,
    movements: &[crate::survival::inside::Movement],
    v: &mut vanillite_base::Vec3,
) -> bool {
    let mut cells = [None; 4];
    let mut count = 0;
    {
        let view = level.view();
        let walker = crate::survival::inside::Walker {
            blocks: &view,
            facts: None,
            dims,
            fast_fall: false,
        };
        walker.walk(movements, |cell| {
            if cell.in_block
                && cell.precise
                && cell.state.is(ids::BUBBLE_COLUMN)
                && let Some(slot) = cells.get_mut(count)
            {
                *slot = Some((cell.pos, cell.state));
                count += 1;
            }
            true
        });
    }
    let mut inside = false;
    for (pos, state) in cells.into_iter().flatten() {
        let (top, y) = drag(level.block(pos.above()), state, v.y);
        v.y = y;
        if top {
            send_particles(level, pos);
        } else {
            inside = true;
        }
    }
    inside
}
