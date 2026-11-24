//! `LiquidBlockContainer.placeLiquid` for the blocks templates waterlog.

use std::sync::OnceLock;

use vanillite_base::BlockPos;
use vanillite_data::block::{BlockState, ids};
use vanillite_data::fluid::FluidState;
use vanillite_data::registry::Registry;

use crate::feature::region::{WorldGenRegion, flags};

/// `fluidState.isSource()`.
#[inline]
pub fn is_source(fluid: FluidState) -> bool {
    !fluid.is_empty() && fluid.info().source
}

/// `fluidState.is(Fluids.WATER)`: still (source) water.
pub fn is_water(fluid: FluidState) -> bool {
    static WATER: OnceLock<Option<u16>> = OnceLock::new();
    let water = *WATER
        .get_or_init(|| Registry::by_name("minecraft:fluid").and_then(|r| r.id("minecraft:water")));
    !fluid.is_empty() && Some(fluid.info().fluid) == water
}

/// `state.getBlock() instanceof LiquidBlockContainer`: every `SimpleWaterloggedBlock` (the
/// blocks with a `waterlogged` property) plus kelp and seagrass.
pub fn is_liquid_block_container(state: BlockState) -> bool {
    let block = state.block();
    block == ids::KELP
        || block == ids::KELP_PLANT
        || block == ids::SEAGRASS
        || block == ids::TALL_SEAGRASS
        || state.value("waterlogged").is_some()
}

/// `((LiquidBlockContainer) state.getBlock()).placeLiquid(level, pos, state, fluidState)`:
/// waterlogs the block in water (`SimpleWaterloggedBlock`, with the overrides of campfires,
/// candles and double slabs; kelp and seagrass take nothing).
pub fn place_liquid(
    level: &mut WorldGenRegion<'_>,
    pos: BlockPos,
    state: BlockState,
    fluid: FluidState,
) -> bool {
    let block = state.block();
    if block == ids::KELP
        || block == ids::KELP_PLANT
        || block == ids::SEAGRASS
        || block == ids::TALL_SEAGRASS
    {
        return false;
    }
    if state.value("type") == Some("double") {
        // `SlabBlock.placeLiquid`.
        return false;
    }
    if state.value("waterlogged") != Some("false") || !is_water(fluid) {
        return false;
    }
    let waterlogged = state.with("waterlogged", "true").unwrap_or(state);
    let name = block.name();
    let new_state = if block == ids::CAMPFIRE || block == ids::SOUL_CAMPFIRE {
        // `CampfireBlock.placeLiquid`: always unlit.
        waterlogged.with("lit", "false").unwrap_or(waterlogged)
    } else if name.ends_with("candle") && state.value("lit") == Some("true") {
        // `CandleBlock.placeLiquid` → `AbstractCandleBlock.extinguish` (`setBlock` flags 11).
        waterlogged.with("lit", "false").unwrap_or(waterlogged)
    } else {
        waterlogged
    };
    level.set_block(pos, new_state, flags::ALL);
    level.schedule_fluid_tick(pos, ids::WATER);
    true
}
