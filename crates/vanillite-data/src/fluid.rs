//! Fluid states.

pub use crate::generated::fluids::FLUID_STATES;

use crate::block::BlockState;

/// A fluid state (Vanilla's global fluid state id; 0 is empty).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct FluidState(pub u8);

/// Static facts of a fluid state.
#[derive(Debug)]
pub struct FluidStateInfo {
    /// Registry id of `minecraft:fluid`.
    pub fluid: u16,
    pub amount: u8,
    pub source: bool,
    pub falling: bool,
    pub own_height: f32,
    pub random_ticks: bool,
    pub explosion_resistance: f32,
    /// The block state Vanilla places for this fluid state.
    pub block_state: u16,
}

impl FluidState {
    pub const EMPTY: Self = Self(0);

    #[inline]
    pub fn info(self) -> &'static FluidStateInfo {
        &FLUID_STATES[usize::from(self.0)]
    }

    #[inline]
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    #[inline]
    pub fn block_state(self) -> BlockState {
        BlockState(self.info().block_state)
    }
}
