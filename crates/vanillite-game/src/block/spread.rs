//! Spreading and decaying blocks.
use vanillite_worldgen::behaviour::{PosExt, StateExt};
use vanillite_base::BlockPos;
use vanillite_data::block::BlockState;
use crate::level::Level;
pub(super) fn leaves_tick(_l: &mut Level, _s: BlockState, _p: BlockPos) {}
pub(super) fn leaves_random_tick(_l: &mut Level, _s: BlockState, _p: BlockPos) {}
pub(super) fn mushroom_random_tick(_l: &mut Level, _s: BlockState, _p: BlockPos) {}
pub(super) fn coral_on_place(_l: &mut Level, _s: BlockState, _p: BlockPos, _o: BlockState, _m: bool) {}
pub(super) fn coral_tick(_l: &mut Level, _s: BlockState, _p: BlockPos) {}
pub(super) fn spreading_snowy_dirt_random_tick(_l: &mut Level, _s: BlockState, _p: BlockPos) {}
pub(super) fn nylium_random_tick(_l: &mut Level, _s: BlockState, _p: BlockPos) {}
