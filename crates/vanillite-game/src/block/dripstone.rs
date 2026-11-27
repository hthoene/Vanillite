//! Pointed dripstone and sulfur spikes (`SpeleothemBlock`, `PointedDripstoneBlock`) and the
//! cauldrons they drip into (`AbstractCauldronBlock.tick`).
//!
//! A stalactite grows by random ticks while a water source sits on top of the block it hangs
//! from; it drips water or lava from above its root into a cauldron below its tip (a scheduled
//! cauldron tick fills it); an unsupported stalactite falls as `falling_block` entities.

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_worldgen::behaviour::{PosExt, StateExt, is_solid_render, tags};

use super::{falling, fluid};
use crate::level::Level;

/// `MAX_SEARCH_LENGTH_WHEN_CHECKING_DRIP_TYPE` and `..._BETWEEN_STALACTITE_TIP_AND_CAULDRON`.
const SEARCH: i32 = 11;
/// `WATER_TRANSFER_PROBABILITY_PER_RANDOM_TICK`.
const WATER_TRANSFER: f32 = 0.175_781_25;
/// `LAVA_TRANSFER_PROBABILITY_PER_RANDOM_TICK`.
const LAVA_TRANSFER: f32 = 0.058_593_75;
/// `GROWTH_PROBABILITY_PER_RANDOM_TICK`.
const GROWTH: f32 = 0.011_377_778;
/// `LevelEvent.DRIPSTONE_DRIP`.
const DRIP: i32 = 1504;
/// `LevelEvent.SOUND_DRIP_WATER_INTO_CAULDRON`.
const DRIP_WATER_INTO_CAULDRON: i32 = 1047;
/// `LevelEvent.SOUND_DRIP_LAVA_INTO_CAULDRON`.
const DRIP_LAVA_INTO_CAULDRON: i32 = 1046;

/// What drips from a stalactite.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Drip {
    Water,
    Lava,
    Other,
}

/// `isSpeleothemWithDirection`.
fn is_with_direction(state: BlockState, tip: Direction) -> bool {
    tags().blocks.speleothems.contains(state) && state.direction("vertical_direction") == Some(tip)
}

/// `isStalactite`.
fn is_stalactite(state: BlockState) -> bool {
    is_with_direction(state, Direction::Down)
}

/// `isTip(state, includeMergedTip)`.
fn is_tip(state: BlockState, include_merged: bool) -> bool {
    if !tags().blocks.speleothems.contains(state) {
        return false;
    }
    match state.value("thickness") {
        Some("tip") => true,
        Some("tip_merge") => include_merged,
        _ => false,
    }
}

/// `isFreeHangingStalactite`.
fn is_free_hanging_stalactite(state: BlockState) -> bool {
    is_stalactite(state) && state.value("thickness") == Some("tip") && !state.flag("waterlogged")
}

/// `isStalactiteStartPos`.
fn is_stalactite_start(level: &Level, state: BlockState, pos: BlockPos) -> bool {
    is_stalactite(state) && !level.block(pos.above()).is(state.block())
}

/// `findBlockVertical`: up to `max - 1` steps along Y while `path` holds, until `target`.
fn find_vertical(
    level: &Level,
    pos: BlockPos,
    up: bool,
    path: impl Fn(BlockPos, BlockState) -> bool,
    target: impl Fn(BlockState) -> bool,
    max: i32,
) -> Option<BlockPos> {
    let step = if up { 1 } else { -1 };
    let mut p = pos;
    for _ in 1..max {
        p = p.offset(0, step, 0);
        let state = level.block(p);
        if target(state) {
            return Some(p);
        }
        if !level.in_height(p.y) || !path(p, state) {
            return None;
        }
    }
    None
}

/// `findTip`.
fn find_tip(
    level: &Level,
    state: BlockState,
    pos: BlockPos,
    max: i32,
    include_merged: bool,
) -> Option<BlockPos> {
    if is_tip(state, include_merged) {
        return Some(pos);
    }
    let dir = state.direction("vertical_direction")?;
    let block = state.block();
    find_vertical(
        level,
        pos,
        dir == Direction::Up,
        |_, s| s.is(block) && s.direction("vertical_direction") == Some(dir),
        |s| is_tip(s, include_merged),
        max,
    )
}

/// `canDripThrough`.
fn can_drip_through(state: BlockState) -> bool {
    if state.is_air() {
        return true;
    }
    if is_solid_render(state) || !state.fluid_state().is_empty() {
        return false;
    }
    // `REQUIRED_SPACE_TO_DRIP_THROUGH_NON_SOLID_BLOCK`: the column 6..10 on X and Z.
    !state
        .collision_shape()
        .iter()
        .any(|b| b[0] < 10.0 / 16.0 && b[3] > 6.0 / 16.0 && b[2] < 10.0 / 16.0 && b[5] > 6.0 / 16.0)
}

/// `getFluidAboveStalactite`: the position above the stalactite's root, what drips from it and
/// the block there.
fn fluid_above(
    level: &Level,
    pos: BlockPos,
    state: BlockState,
) -> Option<(BlockPos, Drip, BlockState)> {
    if !is_stalactite(state) {
        return None;
    }
    let block = state.block();
    // `findRootBlock`.
    let root = find_vertical(
        level,
        pos,
        true,
        |_, s| s.is(block) && s.direction("vertical_direction") == Some(Direction::Down),
        |s| !s.is(block),
        SEARCH,
    )?;
    let above = root.above();
    let above_state = level.block(above);
    let drip = if above_state.is(ids::MUD) && !level.settings.water_evaporates {
        Drip::Water
    } else {
        let f = level.fluid(above);
        if f.is_empty() {
            Drip::Other
        } else if f.info().fluid == fluid::WATER {
            Drip::Water
        } else if f.info().fluid == fluid::LAVA {
            Drip::Lava
        } else {
            Drip::Other
        }
    };
    Some((above, drip, above_state))
}

/// `AbstractCauldronBlock.canReceiveStalactiteDrip`.
fn cauldron_receives(state: BlockState, drip: Drip) -> bool {
    if state.is(ids::CAULDRON) {
        true
    } else if state.is(ids::WATER_CAULDRON) {
        drip == Drip::Water
    } else {
        false
    }
}

/// `SpeleothemBlock.randomTick` and `PointedDripstoneBlock.randomTick`.
pub(super) fn random_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    let dripstone = state.is(ids::POINTED_DRIPSTONE);
    if dripstone {
        let value = level.random().next_float();
        maybe_transfer_fluid(level, state, pos, value);
    }
    if level.random().next_float() < GROWTH && is_stalactite_start(level, state, pos) {
        grow_if_possible(level, state, pos);
    }
}

/// `PointedDripstoneBlock.maybeTransferFluid`.
fn maybe_transfer_fluid(level: &mut Level, state: BlockState, pos: BlockPos, value: f32) {
    // Above both transfer probabilities (the water one is the larger) nothing drips.
    if value > WATER_TRANSFER {
        return;
    }
    if !is_stalactite_start(level, state, pos) {
        return;
    }
    let Some((source, drip, source_state)) = fluid_above(level, pos, state) else {
        return;
    };
    let probability = match drip {
        Drip::Water => WATER_TRANSFER,
        Drip::Lava => LAVA_TRANSFER,
        Drip::Other => return,
    };
    if value >= probability {
        return;
    }
    let Some(tip) = find_tip(level, state, pos, SEARCH, false) else {
        return;
    };
    if source_state.is(ids::MUD) && drip == Drip::Water {
        let clay = ids::CLAY.default_state();
        level.set_block_and_update(source, clay);
        level.game_event_block(
            crate::level::game_event::events().block_change,
            source,
            crate::level::game_event::GameEventContext::with_state(None, clay),
        );
        level.level_event(None, DRIP, tip, 0);
        return;
    }
    // `findFillableCauldronBelowStalactiteTip`.
    let cauldron = find_vertical(
        level,
        tip,
        false,
        |_, s| can_drip_through(s),
        |s| cauldron_receives(s, drip),
        SEARCH,
    );
    if let Some(cauldron) = cauldron {
        level.level_event(None, DRIP, tip, 0);
        let delay = 50 + tip.y - cauldron.y;
        let block = level.block(cauldron).block();
        level.schedule_block_tick(cauldron, block, delay);
    }
}

/// `growStalactiteOrStalagmiteIfPossible`.
fn grow_if_possible(level: &mut Level, state: BlockState, pos: BlockPos) {
    let block = state.block();
    let (grows_on, max_length) = if block == ids::POINTED_DRIPSTONE {
        (ids::DRIPSTONE_BLOCK, 7)
    } else {
        (ids::SULFUR, 2)
    };
    // `canGrow`: the block to grow on above; dripstone also needs a water source on top of it.
    if !level.block(pos.above()).is(grows_on) {
        return;
    }
    if block == ids::POINTED_DRIPSTONE {
        let f = level.block(pos.offset(0, 2, 0)).fluid_state();
        if f.info().fluid != fluid::WATER || !f.info().source {
            return;
        }
    }
    let Some(tip) = find_tip(level, state, pos, max_length, false) else {
        return;
    };
    let tip_state = level.block(tip);
    if is_free_hanging_stalactite(tip_state) && can_tip_grow(level, block, tip_state, tip) {
        if level.random().next_boolean() {
            grow(level, block, tip, Direction::Down);
        } else {
            grow_stalagmite_below(level, block, tip);
        }
    }
}

/// `canTipGrow`.
fn can_tip_grow(level: &Level, block: BlockId, tip_state: BlockState, tip: BlockPos) -> bool {
    let Some(dir) = tip_state.direction("vertical_direction") else {
        return false;
    };
    let at = level.block(tip.relative(dir));
    if !at.fluid_state().is_empty() {
        return false;
    }
    at.is_air() || is_unmerged_tip(at, block, dir.opposite())
}

/// `isUnmergedTipWithDirection`.
fn is_unmerged_tip(state: BlockState, block: BlockId, dir: Direction) -> bool {
    is_tip(state, false) && state.direction("vertical_direction") == Some(dir) && state.is(block)
}

/// `grow(level, growFromPos, growToDirection)`.
fn grow(level: &mut Level, block: BlockId, from: BlockPos, dir: Direction) {
    let target = from.relative(dir);
    let existing = level.block(target);
    if is_unmerged_tip(existing, block, dir.opposite()) {
        // `createMergedTips`.
        let (stalactite, stalagmite) =
            if existing.direction("vertical_direction") == Some(Direction::Up) {
                (target.above(), target)
            } else {
                (target, target.below())
            };
        create(level, block, stalactite, Direction::Down, "tip_merge");
        create(level, block, stalagmite, Direction::Up, "tip_merge");
    } else if existing.is_air() || existing.is(ids::WATER) {
        create(level, block, target, dir, "tip");
    }
}

/// `createSpeleothem`.
fn create(level: &mut Level, block: BlockId, pos: BlockPos, dir: Direction, thickness: &str) {
    let waterlogged = level.fluid(pos).info().fluid == fluid::WATER;
    let state = block
        .default_state()
        .set_direction("vertical_direction", dir)
        .set("thickness", thickness)
        .set_flag("waterlogged", waterlogged);
    level.set_block_and_update(pos, state);
}

/// `isValidSpeleothemPlacement`.
fn is_valid_placement(level: &Level, block: BlockId, pos: BlockPos, tip: Direction) -> bool {
    let behind = level.block(pos.relative(tip.opposite()));
    super::is_face_sturdy(behind, tip) || is_with_direction(behind, tip) && behind.is(block)
}

/// `growStalagmiteBelow`.
fn grow_stalagmite_below(level: &mut Level, block: BlockId, above: BlockPos) {
    let mut pos = above;
    for _ in 0..10 {
        pos = pos.below();
        let state = level.block(pos);
        if !state.fluid_state().is_empty() {
            return;
        }
        if is_unmerged_tip(state, block, Direction::Up) && can_tip_grow(level, block, state, pos) {
            grow(level, block, pos, Direction::Up);
            return;
        }
        if is_valid_placement(level, block, pos, Direction::Up)
            && !fluid::is_water(level.fluid(pos.below()))
        {
            grow(level, block, pos.below(), Direction::Up);
            return;
        }
        // `blocksStalagmiteScan`: only pointed dripstone stops at blocks it cannot drip through.
        if block == ids::POINTED_DRIPSTONE && !can_drip_through(state) {
            return;
        }
    }
}

/// `SpeleothemBlock.tick`: an unsupported stalagmite breaks, a stalactite falls.
pub(super) fn tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    if is_with_direction(state, Direction::Up) && !level.can_survive(state, pos) {
        level.destroy_block(pos, true, None);
        return;
    }
    // `spawnFallingStalactite`: the tip hurts what it lands on, more the longer it was.
    let mut p = pos;
    let mut s = state;
    while is_stalactite(s) {
        let id = falling::fall(level, p, s, true);
        if is_tip(s, true) {
            let size = (1 + pos.y - p.y).max(6);
            #[allow(clippy::cast_precision_loss)]
            falling::set_hurts_entities(level, id, size as f32, 40);
            break;
        }
        p = p.below();
        s = level.block(p);
    }
}

/// `AbstractCauldronBlock.tick`: a stalactite above drips its fluid in.
pub(super) fn cauldron_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    // `findStalactiteTipAboveCauldron`.
    let Some(tip) = find_vertical(
        level,
        pos,
        true,
        |_, s| can_drip_through(s),
        is_free_hanging_stalactite,
        SEARCH,
    ) else {
        return;
    };
    // `getCauldronFillFluidType`.
    let drip = match fluid_above(level, tip, level.block(tip)) {
        Some((_, d @ (Drip::Water | Drip::Lava), _)) => d,
        _ => return,
    };
    if !cauldron_receives(state, drip) {
        return;
    }
    let block_change = crate::level::game_event::events().block_change;
    let (new, event) = if state.is(ids::CAULDRON) {
        match drip {
            Drip::Water => (
                ids::WATER_CAULDRON.default_state(),
                DRIP_WATER_INTO_CAULDRON,
            ),
            _ => (ids::LAVA_CAULDRON.default_state(), DRIP_LAVA_INTO_CAULDRON),
        }
    } else {
        // A water cauldron fills up by one level.
        let fill = state.int("level").unwrap_or(3);
        if fill >= 3 {
            return;
        }
        (state.set_int("level", fill + 1), DRIP_WATER_INTO_CAULDRON)
    };
    level.set_block_and_update(pos, new);
    level.game_event_block(
        block_change,
        pos,
        crate::level::game_event::GameEventContext::with_state(None, new),
    );
    level.level_event(None, event, pos, 0);
}
