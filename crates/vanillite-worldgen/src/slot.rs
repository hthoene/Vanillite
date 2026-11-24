//! The `chunk-generator` slot backed by Vanilla's noise generator.
//!
//! The chunk runtime drives the status pyramid; this adapter runs the [`Stage`](crate::Stage)s of
//! a [`TerrainGenerator`] for the statuses they produce. Blocks and biomes live in the runtime's
//! chunk between the steps; what else a [`ProtoChunk`] carries from the terrain stage on (world
//! generation heightmaps, block entity data, scheduled ticks) stays with the chunk as its
//! [`GenerationState`] until it becomes `Full`; scheduled ticks and post-processing marks are
//! also copied into the runtime's chunk after every step, where storage and the live world find
//! them.

use std::sync::Arc;

use vanillite_base::{BlockPos, ChunkPos};
use vanillite_chunk::slots::{
    self, BlockEntityData, ChunkGenerator, ChunkStatus, GenerationState, Neighbours, SavedTick,
    WorldInfo,
};
use vanillite_chunk::{Chunk, Section};
use vanillite_data::block::BlockId;
use vanillite_data::registry::BLOCK_ENTITY_TYPE;

use crate::proto::ProtoTick;
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
}

/// The runtime's chunk as a proto chunk: its sections plus the generation state it carries, or
/// (for chunks that come from storage) one derived from its blocks.
fn take(chunk: &slots::ProtoChunk) -> ProtoChunk {
    let sections: Vec<Section> = chunk
        .chunk
        .sections()
        .iter()
        .map(|s| Section::clone(s))
        .collect();
    match chunk
        .generation
        .as_ref()
        .and_then(|g| g.0.downcast_ref::<ProtoChunk>())
    {
        Some(state) => {
            let mut proto = state.clone();
            proto.set_sections(sections);
            proto
        }
        None => {
            let mut proto = ProtoChunk::from_sections(
                chunk.pos,
                chunk.chunk.min_y(),
                sections,
                chunk.status >= ChunkStatus::Terrain,
            );
            for tick in &chunk.block_ticks {
                proto
                    .block_ticks_mut()
                    .schedule(BlockId(tick.kind), tick.pos, tick.priority);
            }
            for tick in &chunk.fluid_ticks {
                proto
                    .fluid_ticks_mut()
                    .schedule(tick.kind, tick.pos, tick.priority);
            }
            for pos in &chunk.post_processing {
                proto.mark_post_processing(pos.x, pos.y, pos.z);
            }
            proto
        }
    }
}

/// Hands a proto chunk back to the runtime's chunk: the sections (light kept), its block
/// entities and the rest as generation state.
fn put(chunk: &mut slots::ProtoChunk, mut proto: ProtoChunk) {
    let sections = proto.take_sections().into_iter().map(Arc::new).collect();
    chunk.chunk = Chunk::from_sections(
        chunk.chunk.min_section(),
        sections,
        chunk.chunk.light().clone(),
    );
    for (&pos, data) in proto.block_entities() {
        let Some(kind) = data
            .get("id")
            .and_then(|id| id.as_str())
            .and_then(|id| BLOCK_ENTITY_TYPE.id(id))
        else {
            continue;
        };
        let mut nbt = Vec::new();
        if vanillite_nbt::write::write_network_compound(data, &mut nbt).is_err() {
            continue;
        }
        chunk.block_entities.retain(|b| b.pos != pos);
        chunk.block_entities.push(BlockEntityData {
            pos,
            kind: Some(kind),
            nbt: nbt.into(),
        });
    }
    // Generation ticks are stored with delay 0 (`ProtoChunkTicks`).
    fn saved<T>(kind: u16, tick: &ProtoTick<T>) -> SavedTick {
        SavedTick {
            kind,
            pos: tick.pos,
            delay: 0,
            priority: tick.priority,
        }
    }
    chunk.block_ticks = proto
        .block_ticks()
        .ticks()
        .iter()
        .map(|t| saved(t.kind.0, t))
        .collect();
    chunk.fluid_ticks = proto
        .fluid_ticks()
        .ticks()
        .iter()
        .map(|t| saved(t.kind, t))
        .collect();
    chunk.post_processing = proto
        .post_processing()
        .iter()
        .map(|&(x, y, z)| BlockPos::new(x, y, z))
        .collect();
    chunk.generation = Some(GenerationState(Arc::new(proto)));
}

/// The biomes of the 3×3 chunks around `target`, which the runtime provides at
/// [`ChunkStatus::Biomes`] or later.
fn neighbour_biomes(target: &slots::ProtoChunk, neighbours: &Neighbours<'_>) -> NeighbourBiomes {
    let chunks = std::array::from_fn(|i| {
        let (dx, dz) = (i as i32 % 3 - 1, i as i32 / 3 - 1);
        let chunk: &Chunk = if (dx, dz) == (0, 0) {
            &target.chunk
        } else {
            let pos = ChunkPos::new(target.pos.x + dx, target.pos.z + dz);
            &neighbours
                .get(pos)
                .unwrap_or_else(|| panic!("terrain of {:?} needs biomes of {pos:?}", target.pos))
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
                let mut batch: Vec<ProtoChunk> = targets.iter().map(take).collect();
                self.generator.generate_biomes(&mut batch);
                for (target, mut chunk) in targets.iter_mut().zip(batch) {
                    let sections = chunk.take_sections().into_iter().map(Arc::new).collect();
                    target.chunk = Chunk::from_sections(
                        target.chunk.min_section(),
                        sections,
                        target.chunk.light().clone(),
                    );
                }
            }
            ChunkStatus::Terrain => {
                let mut batch: Vec<TerrainJob> = targets
                    .iter()
                    .map(|target| TerrainJob {
                        chunk: take(target),
                        neighbours: neighbour_biomes(target, neighbours),
                    })
                    .collect();
                self.generator.generate_terrain(&mut batch);
                for (target, job) in targets.iter_mut().zip(batch) {
                    put(target, job.chunk);
                }
            }
            ChunkStatus::Features => {
                // One target at a time: the features of a chunk write into its 3×3 area.
                for target in targets.iter_mut() {
                    self.decorate(target, neighbours);
                }
            }
            // Structure starts and references come with structures; until then these statuses
            // leave the chunks as they are.
            _ => {}
        }
    }
}

impl VanillaChunkGenerator {
    /// `applyBiomeDecoration` for `target` with its 3×3 area.
    fn decorate(&self, target: &mut slots::ProtoChunk, neighbours: &mut Neighbours<'_>) {
        let center = target.pos;
        let positions: [ChunkPos; 9] = std::array::from_fn(|i| {
            ChunkPos::new(center.x + i as i32 % 3 - 1, center.z + i as i32 / 3 - 1)
        });
        let mut area: [ProtoChunk; 9] =
            std::array::from_fn(|i| {
                if i == 4 {
                    take(target)
                } else {
                    take(neighbours.get(positions[i]).unwrap_or_else(|| {
                        panic!("features of {center:?} need {:?}", positions[i])
                    }))
                }
            });
        let [a0, a1, a2, a3, a4, a5, a6, a7, a8] = &mut area;
        self.generator
            .generate_features([a0, a1, a2, a3, a4, a5, a6, a7, a8]);
        for (i, proto) in area.into_iter().enumerate() {
            if i == 4 {
                put(target, proto);
            } else if let Some(chunk) = neighbours.get_mut(positions[i]) {
                put(chunk, proto);
            }
        }
    }
}
