//! The `chunk-generator` slot backed by Vanilla's noise generator.
//!
//! The chunk runtime drives the status pyramid; this adapter runs the [`Stage`](crate::Stage)s of
//! a [`TerrainGenerator`] for the statuses they produce. Chunks move between the runtime's
//! [`slots::ProtoChunk`] and this crate's [`ProtoChunk`] by handing over their sections.

use vanillite_base::ChunkPos;
use vanillite_chunk::slots::{self, ChunkGenerator, ChunkStatus, Neighbours, WorldInfo};
use vanillite_chunk::{Chunk, Section};

use crate::{NeighbourBiomes, ProtoChunk, TerrainGenerator, TerrainJob};

/// Vanilla's `NoiseBasedChunkGenerator` in the `chunk-generator` slot.
#[derive(Debug)]
pub struct VanillaChunkGenerator {
    generator: TerrainGenerator,
    world: WorldInfo,
}

impl VanillaChunkGenerator {
    /// The slot implementation for one world; `generator` must be built for the same dimension
    /// height and biome registry as `world`.
    pub fn new(generator: TerrainGenerator, world: WorldInfo) -> Self {
        Self { generator, world }
    }

    /// The wrapped generator.
    pub fn generator(&self) -> &TerrainGenerator {
        &self.generator
    }

    /// The chunk as this crate's proto chunk; it must hold no blocks yet.
    fn take(chunk: &slots::ProtoChunk) -> ProtoChunk {
        let sections = chunk
            .chunk
            .sections()
            .iter()
            .map(|s| Section::clone(s))
            .collect();
        ProtoChunk::from_sections(chunk.pos, chunk.chunk.min_y(), sections)
    }

    /// The biomes of the 3×3 chunks around `target`, which the runtime provides at
    /// [`ChunkStatus::Biomes`] or later.
    fn neighbour_biomes(
        target: &slots::ProtoChunk,
        neighbours: &Neighbours<'_>,
    ) -> NeighbourBiomes {
        let chunks = std::array::from_fn(|i| {
            let (dx, dz) = (i as i32 % 3 - 1, i as i32 / 3 - 1);
            let chunk: &Chunk = if (dx, dz) == (0, 0) {
                &target.chunk
            } else {
                let pos = ChunkPos::new(target.pos.x + dx, target.pos.z + dz);
                &neighbours
                    .get(pos)
                    .unwrap_or_else(|| {
                        panic!("terrain of {:?} needs biomes of {pos:?}", target.pos)
                    })
                    .chunk
            };
            chunk
                .sections()
                .iter()
                .map(|s| s.biomes().clone())
                .collect()
        });
        NeighbourBiomes::new(target.chunk.min_section(), chunks)
    }
}

impl ChunkGenerator for VanillaChunkGenerator {
    fn new_chunk(&self, pos: ChunkPos) -> slots::ProtoChunk {
        slots::ProtoChunk::empty(pos, &self.world)
    }

    fn generate(
        &self,
        status: ChunkStatus,
        targets: &mut [slots::ProtoChunk],
        neighbours: &mut Neighbours<'_>,
    ) {
        match status {
            ChunkStatus::Biomes => {
                let mut batch: Vec<ProtoChunk> = targets.iter().map(Self::take).collect();
                self.generator.generate_biomes(&mut batch);
                for (target, chunk) in targets.iter_mut().zip(batch) {
                    target.chunk = chunk.into_chunk();
                }
            }
            ChunkStatus::Terrain => {
                let mut batch: Vec<TerrainJob> = targets
                    .iter()
                    .map(|target| TerrainJob {
                        chunk: Self::take(target),
                        neighbours: Self::neighbour_biomes(target, neighbours),
                    })
                    .collect();
                self.generator.generate_terrain(&mut batch);
                for (target, job) in targets.iter_mut().zip(batch) {
                    target.chunk = job.chunk.into_chunk();
                }
            }
            // Structure starts, structure references and features (milestone 2, #22) plug in
            // here; until then these statuses leave the chunks as they are.
            _ => {}
        }
    }
}
