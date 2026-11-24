//! Ice, snow, weathering.
use vanillite_worldgen::behaviour::{PosExt, StateExt};
use vanillite_base::BlockPos;
use vanillite_data::block::{BlockId, BlockState};
use crate::level::Level;
pub fn tick_precipitation(_l: &mut Level, _p: BlockPos) {}
pub(super) fn snow_random_tick(_l: &mut Level, _s: BlockState, _p: BlockPos) {}
pub(super) fn ice_random_tick(_l: &mut Level, _s: BlockState, _p: BlockPos) {}
pub(super) fn redstone_ore_random_tick(_l: &mut Level, _s: BlockState, _p: BlockPos) {}
pub(super) fn copper_random_tick(_l: &mut Level, _s: BlockState, _p: BlockPos) {}
pub(super) fn weathers(_b: BlockId) -> bool { false }
