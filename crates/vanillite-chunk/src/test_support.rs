//! Synthetic tables for unit tests.

use crate::facts::{self, Tables};

pub const AIR: u16 = 0;
pub const STONE: u16 = 1;
pub const WATER: u16 = 2;
pub const TICKING: u16 = 3;
pub const LEAVES: u16 = 4;
pub const FLOWER: u16 = 5;

/// Same size as the 26.3 block state registry, so direct containers use 16 bits.
pub const STATE_COUNT: usize = 35_723;

static FLAGS: [u8; STATE_COUNT] = {
    let mut flags = [facts::BLOCKS_MOTION | facts::BLOCKS_MOTION_NO_LEAVES; STATE_COUNT];
    flags[AIR as usize] = facts::AIR;
    flags[WATER as usize] = facts::FLUID | facts::FLUID_RANDOM_TICKING;
    flags[TICKING as usize] |= facts::RANDOM_TICKING;
    flags[LEAVES as usize] = facts::BLOCKS_MOTION | facts::RANDOM_TICKING;
    flags[FLOWER as usize] = 0;
    flags
};

pub fn install() {
    facts::install(Tables {
        block_flags: &FLAGS,
        biome_count: 65,
    });
}
