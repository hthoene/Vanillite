//! Plant growth.
use vanillite_worldgen::behaviour::{PosExt, StateExt};
use vanillite_base::BlockPos;
use vanillite_data::block::{BlockId, BlockState};
use crate::level::Level;
pub(super) fn cactus_random_tick(_l: &mut Level, _s: BlockState, _p: BlockPos) {}
pub(super) fn sugar_cane_random_tick(_l: &mut Level, _s: BlockState, _p: BlockPos) {}
pub(super) fn bamboo_random_tick(_l: &mut Level, _s: BlockState, _p: BlockPos) {}
pub(super) fn bamboo_sapling_random_tick(_l: &mut Level, _s: BlockState, _p: BlockPos) {}
pub(super) fn growing_plant_head_random_tick(_l: &mut Level, _s: BlockState, _p: BlockPos) {}
pub(super) fn crop_random_tick(_l: &mut Level, _s: BlockState, _p: BlockPos) {}
pub(super) fn stem_random_tick(_l: &mut Level, _s: BlockState, _p: BlockPos) {}
pub(super) fn nether_wart_random_tick(_l: &mut Level, _s: BlockState, _p: BlockPos) {}
pub(super) fn cocoa_random_tick(_l: &mut Level, _s: BlockState, _p: BlockPos) {}
pub(super) fn farmland_tick(_l: &mut Level, _s: BlockState, _p: BlockPos) {}
pub(super) fn farmland_random_tick(_l: &mut Level, _s: BlockState, _p: BlockPos) {}
pub(super) fn path_tick(_l: &mut Level, _s: BlockState, _p: BlockPos) {}
pub(super) fn sweet_berry_random_tick(_l: &mut Level, _s: BlockState, _p: BlockPos) {}
pub(super) fn sapling_random_tick(_l: &mut Level, _s: BlockState, _p: BlockPos) {}
pub(super) fn budding_amethyst_random_tick(_l: &mut Level, _s: BlockState, _p: BlockPos) {}
pub(super) fn is_sapling(_b: BlockId) -> bool { false }
