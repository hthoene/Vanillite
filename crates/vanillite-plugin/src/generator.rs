//! Providers: replacing the `chunk-generator` slot.
//!
//! A provider is called from many worker threads at once and works with numeric block state
//! ids: resolve them once with [`Server::block_state_id`](crate::Server::block_state_id) when
//! the generator is created. Providers run outside the plugin lock (they must be thread-safe);
//! a panic in a provider stops the server in order, because a world cannot continue without
//! its generator.

use std::ffi::c_void;

use vanillite_abi::{self as abi, Status, status};

use crate::ffi::{guard, str};

/// The world a generator is created for.
#[derive(Clone, Debug)]
pub struct WorldInfo {
    /// Dimension key, e.g. `minecraft:overworld`.
    pub dimension: String,
    /// Lowest block Y.
    pub min_y: i32,
    /// Height in blocks.
    pub height: u32,
    /// World seed.
    pub seed: i64,
}

/// The blocks of one chunk being generated: a dense array, pre-filled with air.
#[derive(Debug)]
pub struct ChunkBlocks<'a> {
    x: i32,
    z: i32,
    min_y: i32,
    height: u32,
    blocks: &'a mut [u32],
}

impl ChunkBlocks<'_> {
    /// Chunk X.
    pub fn chunk_x(&self) -> i32 {
        self.x
    }

    /// Chunk Z.
    pub fn chunk_z(&self) -> i32 {
        self.z
    }

    /// Lowest block Y.
    pub fn min_y(&self) -> i32 {
        self.min_y
    }

    /// Height in blocks.
    pub fn height(&self) -> u32 {
        self.height
    }

    fn index(&self, x: u32, y: i32, z: u32) -> Option<usize> {
        let dy = u32::try_from(y.checked_sub(self.min_y)?).ok()?;
        (x < 16 && z < 16 && dy < self.height)
            .then(|| usize::try_from((dy * 16 + z) * 16 + x).ok())
            .flatten()
    }

    /// Sets a block (`x`, `z` in `0..16` within the chunk); outside positions are ignored.
    pub fn set(&mut self, x: u32, y: i32, z: u32, state: u32) {
        if let Some(i) = self.index(x, y, z)
            && let Some(slot) = self.blocks.get_mut(i)
        {
            *slot = state;
        }
    }

    /// The block at a position, air outside.
    pub fn get(&self, x: u32, y: i32, z: u32) -> u32 {
        self.index(x, y, z)
            .and_then(|i| self.blocks.get(i).copied())
            .unwrap_or(0)
    }

    /// Fills one whole layer.
    pub fn fill_layer(&mut self, y: i32, state: u32) {
        if let Some(start) = self.index(0, y, 0)
            && let Some(layer) = self.blocks.get_mut(start..start + 256)
        {
            layer.fill(state);
        }
    }
}

/// The generator of one world. Called from several threads at once.
pub trait ChunkGenerator: Send + Sync + 'static {
    /// Generates the terrain of a chunk (Vanilla's noise step: blocks only; the server lights
    /// and finishes it).
    fn generate(&self, chunk: &mut ChunkBlocks<'_>);
}

/// Creates the generator of each world.
pub trait ChunkGeneratorProvider: Send + Sync + 'static {
    /// The generator of a world; `None` keeps the built-in generator for that world.
    fn create(&self, world: &WorldInfo) -> Option<Box<dyn ChunkGenerator>>;
}

/// A generator instance with the world's lowest block.
struct Instance {
    generator: Box<dyn ChunkGenerator>,
    min_y: i32,
}

pub(crate) unsafe extern "C" fn create(
    user: *mut c_void,
    world: *const abi::WorldInfo,
    instance: *mut *mut c_void,
) -> Status {
    guard(|| {
        if world.is_null() || instance.is_null() {
            return status::INVALID_ARGUMENT;
        }
        // SAFETY: `user` is the leaked provider box of the registration; `world` is valid for
        // the call.
        let (provider, world) =
            unsafe { (&*user.cast::<Box<dyn ChunkGeneratorProvider>>(), &*world) };
        let info = WorldInfo {
            // SAFETY: the host's strings are valid for the call.
            dimension: unsafe { str(world.dimension) }.to_owned(),
            min_y: world.min_y,
            height: world.height,
            seed: world.seed,
        };
        let created = provider
            .create(&info)
            .map_or(std::ptr::null_mut(), |generator| {
                Box::into_raw(Box::new(Instance {
                    generator,
                    min_y: info.min_y,
                }))
                .cast()
            });
        // SAFETY: checked above.
        unsafe { *instance = created };
        status::OK
    })
}

pub(crate) unsafe extern "C" fn generate(
    instance: *mut c_void,
    chunks: *const abi::GenChunk,
    count: usize,
    stride: usize,
) -> Status {
    guard(|| {
        // SAFETY: `instance` comes from `create`; generators are `Sync`.
        let instance = unsafe { &*instance.cast::<Instance>() };
        for i in 0..count {
            // SAFETY: the host passes `count` records `stride` bytes apart, each with its own
            // block array of `len` entries, valid for the call.
            let chunk = unsafe { &*chunks.cast::<u8>().add(i * stride).cast::<abi::GenChunk>() };
            // SAFETY: as above; every chunk has its own array.
            let blocks = unsafe { std::slice::from_raw_parts_mut(chunk.blocks, chunk.len) };
            let height = u32::try_from(chunk.len / 256).unwrap_or(0);
            let mut view = ChunkBlocks {
                x: chunk.x,
                z: chunk.z,
                min_y: instance.min_y,
                height,
                blocks,
            };
            instance.generator.generate(&mut view);
        }
        status::OK
    })
}

pub(crate) unsafe extern "C" fn destroy(instance: *mut c_void) {
    let _ = guard(|| {
        // SAFETY: `instance` comes from `create` and is destroyed once.
        drop(unsafe { Box::from_raw(instance.cast::<Instance>()) });
        status::OK
    });
}
