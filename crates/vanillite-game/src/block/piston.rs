//! Pistons and sticky pistons (`PistonBaseBlock`, `PistonHeadBlock`, `MovingPistonBlock`,
//! `PistonMovingBlockEntity`, `PistonStructureResolver`).

use vanillite_base::BlockPos;
use vanillite_data::block::{BlockId, BlockState};

use super::BlockBehaviour;
use crate::level::Level;

/// The data of a `PistonMovingBlockEntity`.
#[derive(Clone, Debug, PartialEq)]
pub struct MovingPiston {}

/// Fills the piston behaviours of `block`.
pub(super) fn fill(_block: BlockId, _b: &mut BlockBehaviour) {}

/// `PistonMovingBlockEntity.tick`.
pub fn moving_piston_tick(_level: &mut Level, _pos: BlockPos, _state: BlockState) {}
