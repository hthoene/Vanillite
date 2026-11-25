//! The `chunk-generator` slot filled by a plugin: an adapter that implements the slot's Rust
//! trait over the provider's C table.
//!
//! The provider fills the terrain step (Vanilla's noise step) of a batch of chunks in one call,
//! as dense arrays of numeric block state ids; the adapter writes them into the chunks. The
//! other steps are left to the server (light, finishing). A world for which the provider
//! creates no generator keeps the built-in one.

use std::sync::{Arc, PoisonError};

use vanillite_abi::{self as abi, status};
use vanillite_base::ChunkPos;
use vanillite_chunk::slots::{
    ChunkGenerator, ChunkGeneratorProvider, ChunkStatus, Neighbours, ProtoChunk, WorldInfo,
};

use crate::{Inner, Plugin, PluginHost};

/// The provider of a plugin, with the built-in provider for the worlds it leaves alone.
pub struct PluginGeneratorProvider {
    host: Arc<Inner>,
    plugin: Arc<Plugin>,
    table: abi::ChunkGeneratorProvider,
    fallback: Box<dyn ChunkGeneratorProvider>,
}

// SAFETY: the table holds the plugin's functions and user data, which providers declare
// thread-safe by contract.
unsafe impl Send for PluginGeneratorProvider {}
// SAFETY: as above.
unsafe impl Sync for PluginGeneratorProvider {}

impl std::fmt::Debug for PluginGeneratorProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginGeneratorProvider")
            .field("plugin", &self.plugin.id())
            .finish_non_exhaustive()
    }
}

impl PluginHost {
    /// The plugin that fills the `chunk-generator` slot, if any: its id and the provider,
    /// which falls back to `built_in` for worlds the plugin leaves alone.
    pub fn take_chunk_generator(
        &self,
        built_in: impl FnOnce() -> Box<dyn ChunkGeneratorProvider>,
    ) -> Option<(String, PluginGeneratorProvider)> {
        let (local, table) = self
            .inner
            .generator
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()?;
        let plugin = self.inner.plugin(local)?;
        Some((
            plugin.id().to_owned(),
            PluginGeneratorProvider {
                host: Arc::clone(&self.inner),
                plugin,
                table,
                fallback: built_in(),
            },
        ))
    }
}

impl PluginGeneratorProvider {
    /// A provider failed: the world cannot continue without it, so the server saves and stops.
    fn failed(&self, what: &str) {
        let reason = format!(
            "the chunk generator of plugin {} failed ({what})",
            self.plugin.id()
        );
        log::error!("{reason}; stopping the server");
        if let Some(server) = self.host.server() {
            server.shutdown(&reason);
        }
    }
}

impl ChunkGeneratorProvider for PluginGeneratorProvider {
    fn create(&self, world: &WorldInfo) -> Result<Arc<dyn ChunkGenerator>, String> {
        let info = abi::WorldInfo {
            size: abi::size_of::<abi::WorldInfo>(),
            dimension: abi::Str::new(&world.dimension),
            min_y: world.min_y,
            height: world.height,
            seed: world.seed,
        };
        let mut instance = std::ptr::null_mut();
        let s = self.plugin.call_provider(|| {
            // SAFETY: the provider's function and user data live as long as its library; the
            // record and output are valid for the call.
            unsafe { (self.table.create)(self.table.user, &info, &mut instance) }
        });
        if s != status::OK {
            return Err(format!(
                "plugin {} could not create a generator (status {s})",
                self.plugin.id()
            ));
        }
        if instance.is_null() {
            log::info!(
                "Plugin {} keeps the built-in generator for {}",
                self.plugin.id(),
                world.dimension
            );
            return self.fallback.create(world);
        }
        log::info!("Plugin {} generates {}", self.plugin.id(), world.dimension);
        Ok(Arc::new(PluginGenerator {
            provider: PluginGeneratorProvider {
                host: Arc::clone(&self.host),
                plugin: Arc::clone(&self.plugin),
                table: self.table,
                fallback: Box::new(NoFallback),
            },
            instance: crate::SendPtr(instance),
            world: world.clone(),
        }))
    }
}

/// Placeholder fallback of a created generator (it never creates anything).
#[derive(Debug)]
struct NoFallback;

impl ChunkGeneratorProvider for NoFallback {
    fn create(&self, _: &WorldInfo) -> Result<Arc<dyn ChunkGenerator>, String> {
        Err("no fallback".into())
    }
}

/// The generator of one world, created by a plugin.
#[derive(Debug)]
struct PluginGenerator {
    provider: PluginGeneratorProvider,
    instance: crate::SendPtr,
    world: WorldInfo,
}

impl ChunkGenerator for PluginGenerator {
    fn new_chunk(&self, pos: ChunkPos) -> ProtoChunk {
        ProtoChunk::empty(pos, &self.world)
    }

    fn generate(&self, status: ChunkStatus, targets: &mut [ProtoChunk], _: &mut Neighbours<'_>) {
        if status != ChunkStatus::Terrain || targets.is_empty() {
            return;
        }
        let air = u32::from(self.world.air);
        let len = 256 * self.world.height as usize;
        let mut buffers: Vec<Vec<u32>> = targets.iter().map(|_| vec![air; len]).collect();
        let chunks: Vec<abi::GenChunk> = targets
            .iter()
            .zip(buffers.iter_mut())
            .map(|(t, b)| abi::GenChunk {
                x: t.pos.x,
                z: t.pos.z,
                blocks: b.as_mut_ptr(),
                len: b.len(),
            })
            .collect();
        let table = &self.provider.table;
        let s = self.provider.plugin.call_provider(|| {
            // SAFETY: the instance comes from `create`; the records and arrays are valid for
            // the call.
            unsafe {
                (table.generate)(
                    self.instance.0,
                    chunks.as_ptr(),
                    chunks.len(),
                    size_of::<abi::GenChunk>(),
                )
            }
        });
        if s != status::OK {
            self.provider.failed(if s == status::PANICKED {
                "it panicked"
            } else {
                "it returned an error"
            });
            return;
        }
        for (target, blocks) in targets.iter_mut().zip(&buffers) {
            for (i, &state) in blocks.iter().enumerate() {
                if state == air {
                    continue;
                }
                let Ok(id) = u16::try_from(state) else {
                    continue;
                };
                if !vanillite_data::block::BlockState(id).is_valid() {
                    continue;
                }
                let (x, z) = ((i % 16) as u32, ((i / 16) % 16) as u32);
                let y = self.world.min_y + (i / 256) as i32;
                target.chunk.set_block(x, y, z, id);
            }
        }
    }
}

impl Drop for PluginGenerator {
    fn drop(&mut self) {
        let table = &self.provider.table;
        self.provider.plugin.call_provider(|| {
            // SAFETY: the instance comes from `create` and is destroyed once.
            unsafe { (table.destroy)(self.instance.0) };
            status::OK
        });
    }
}
