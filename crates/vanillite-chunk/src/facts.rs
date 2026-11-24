//! Per-state facts and global palette sizes the chunk format depends on.
//!
//! The tables are installed once at startup. Every block state has one flag byte; the flags drive
//! section summaries and heightmaps.

use std::sync::OnceLock;

/// Global block state id (Vanilla's `Block.BLOCK_STATE_REGISTRY` id).
pub type BlockStateId = u16;

/// Biome id in the server's biome registry (dynamic, fixed after the registries are loaded).
pub type BiomeId = u16;

/// `BlockState.isAir()`.
pub const AIR: u8 = 1 << 0;
/// `BlockState.isRandomlyTicking()`.
pub const RANDOM_TICKING: u8 = 1 << 1;
/// The state's fluid state is not empty.
pub const FLUID: u8 = 1 << 2;
/// The state's fluid state is randomly ticking.
pub const FLUID_RANDOM_TICKING: u8 = 1 << 3;
/// The collision shape is neither empty nor a full cube, or depends on context.
pub const SPECIAL_COLLISION: u8 = 1 << 4;
/// The block is in the `blocks_motion_in_heightmap` tag.
pub const BLOCKS_MOTION: u8 = 1 << 5;
/// The block is in the `blocks_motion_in_heightmap_no_leaves` tag.
pub const BLOCKS_MOTION_NO_LEAVES: u8 = 1 << 6;

/// Tables handed to [`install`].
#[derive(Debug, Clone, Copy)]
pub struct Tables {
    /// One flag byte per block state, indexed by [`BlockStateId`]. Its length is the size of the
    /// global block state palette.
    pub block_flags: &'static [u8],
    /// Number of entries in the biome registry.
    pub biome_count: u32,
}

#[derive(Debug)]
struct Installed {
    block_flags: &'static [u8],
    block_bits: u8,
    biome_count: u32,
    biome_bits: u8,
}

static INSTALLED: OnceLock<Installed> = OnceLock::new();

/// Installs the tables. Returns `false` if tables were installed before (the first ones stay).
pub fn install(tables: Tables) -> bool {
    let block_count = u32::try_from(tables.block_flags.len()).unwrap_or(u32::MAX);
    assert!(
        block_count <= 1 << 16,
        "block state ids must fit in 16 bits"
    );
    assert!(
        tables.biome_count <= 1 << 16,
        "biome ids must fit in 16 bits"
    );
    INSTALLED
        .set(Installed {
            block_flags: tables.block_flags,
            block_bits: ceil_log2(block_count),
            biome_count: tables.biome_count,
            biome_bits: ceil_log2(tables.biome_count),
        })
        .is_ok()
}

#[inline]
fn installed() -> &'static Installed {
    INSTALLED
        .get()
        .expect("vanillite_chunk::facts::install must be called at startup")
}

/// Flag byte of a block state; unknown ids have no flags.
#[inline]
pub fn block_flags(state: BlockStateId) -> u8 {
    installed()
        .block_flags
        .get(usize::from(state))
        .copied()
        .unwrap_or(0)
}

/// Size of the global block state palette.
#[inline]
pub fn block_state_count() -> u32 {
    // The length was checked to fit in 16 bits at install.
    installed().block_flags.len() as u32
}

/// Bits per entry of the direct (global) block state palette.
#[inline]
pub fn block_global_bits() -> u8 {
    installed().block_bits
}

/// Size of the biome registry.
#[inline]
pub fn biome_count() -> u32 {
    installed().biome_count
}

/// Bits per entry of the direct (global) biome palette.
#[inline]
pub fn biome_global_bits() -> u8 {
    installed().biome_bits
}

/// Installs the Vanilla tables: the per-state facts from `vanillite-data` plus the two heightmap
/// block tags (`#minecraft:blocks_motion_in_heightmap` and `…_no_leaves`, as block registry ids)
/// that come from the data packs. Returns `false` if tables were installed before.
pub fn install_vanilla(
    motion_blocking: &[u32],
    motion_blocking_no_leaves: &[u32],
    biome_count: u32,
) -> bool {
    use vanillite_data::block::BlockState;
    use vanillite_data::block::{BLOCKS, STATE_COUNT, flags};

    let mut in_tag = vec![0u8; BLOCKS.len()];
    for (ids, flag) in [
        (motion_blocking, BLOCKS_MOTION),
        (motion_blocking_no_leaves, BLOCKS_MOTION_NO_LEAVES),
    ] {
        for &id in ids {
            if let Some(entry) = usize::try_from(id).ok().and_then(|id| in_tag.get_mut(id)) {
                *entry |= flag;
            }
        }
    }
    let table: Vec<u8> = (0..STATE_COUNT)
        .map(|index| {
            let state = BlockState(u16::try_from(index).unwrap_or(u16::MAX));
            let mut value = in_tag[usize::from(state.block().0)];
            if state.is_air() {
                value |= AIR;
            }
            if state.random_ticks() {
                value |= RANDOM_TICKING;
            }
            let fluid = state.fluid_state();
            if !fluid.is_empty() {
                value |= FLUID;
                if fluid.info().random_ticks {
                    value |= FLUID_RANDOM_TICKING;
                }
            }
            let collision = state.collision_shape();
            if state.block().info().dynamic_shape
                || state.has(flags::LARGE_COLLISION_SHAPE)
                || (!collision.is_empty() && !state.has(flags::COLLISION_FULL_BLOCK))
            {
                value |= SPECIAL_COLLISION;
            }
            value
        })
        .collect();
    install(Tables {
        block_flags: Box::leak(table.into_boxed_slice()),
        biome_count,
    })
}

/// `Mth.ceillog2`: bits needed for `count` distinct values (0 for 0 or 1).
pub const fn ceil_log2(count: u32) -> u8 {
    if count <= 1 {
        0
    } else {
        (32 - (count - 1).leading_zeros()) as u8
    }
}
