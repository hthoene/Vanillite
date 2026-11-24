//! The `chunk-generator` slot backed by Vanilla's noise generator.
//!
//! The chunk runtime drives the status pyramid; this adapter runs the [`Stage`](crate::Stage)s of
//! a [`TerrainGenerator`] for the statuses they produce. Blocks and biomes live in the runtime's
//! chunk between the steps; what else a [`ProtoChunk`] carries from the terrain stage on (world
//! generation heightmaps, block entity data, scheduled ticks) stays with the chunk as its
//! [`GenerationState`] until it becomes `Full`; scheduled ticks and post-processing marks are
//! also copied into the runtime's chunk after every step, where storage and the live world find
//! them.
//!
//! Structure data is kept in the chunk's stored tags ([`slots::ProtoChunk::extra`]) as Vanilla
//! saves it: `structures` (`starts` with their pieces, `References`) and the `entities` that
//! generation added. Starts are shared between the chunks they reach, and pieces remember what
//! they did while they were placed, so the generator keeps every start it made (or regenerated
//! for a chunk from storage; starts are a function of the seed) by chunk.

use std::sync::{Arc, Mutex, PoisonError};

use rustc_hash::FxHashMap;
use vanillite_nbt::{Budget, Compound, List, Tag};

use vanillite_base::{BlockPos, ChunkPos};
use vanillite_chunk::slots::{
    self, BlockEntityData, ChunkGenerator, ChunkStatus, GenerationState, Neighbours, SavedTick,
    WorldInfo,
};
use vanillite_chunk::{Chunk, Section};
use vanillite_data::block::BlockId;
use vanillite_data::registry::BLOCK_ENTITY_TYPE;

use crate::proto::ProtoTick;
use crate::structure::long_set::LongOpenHashSet;
use crate::structure::start::ChunkStarts;
use crate::structure::{NeighbourStarts, ReferencedStarts, StructureRegistry};
use crate::{NeighbourBiomes, ProtoChunk, TerrainGenerator, TerrainJob};

/// Chunks whose starts the generator keeps at most; beyond, it forgets them all and regenerates
/// them when they are needed again.
const MAX_KEPT_STARTS: usize = 1 << 18;

/// Vanilla's `NoiseBasedChunkGenerator` in the `chunk-generator` slot.
#[derive(Debug)]
pub struct VanillaChunkGenerator {
    generator: TerrainGenerator,
    world: WorldInfo,
    /// `WorldOptions.generateStructures`.
    structures: bool,
    /// The structure starts of chunks past `StructureStarts`, by chunk.
    starts: Mutex<FxHashMap<ChunkPos, ChunkStarts>>,
}

impl VanillaChunkGenerator {
    /// The slot implementation for one world; `generator` must be built for the same dimension
    /// height and biome registry as `world`. Structures are generated.
    pub fn new(generator: TerrainGenerator, world: WorldInfo) -> Self {
        Self {
            generator,
            world,
            structures: true,
            starts: Mutex::default(),
        }
    }

    /// Whether structures are generated (`generate_structures` of the world).
    pub fn with_structures(mut self, enabled: bool) -> Self {
        self.structures = enabled;
        self
    }

    /// The wrapped generator.
    pub fn generator(&self) -> &TerrainGenerator {
        &self.generator
    }
}

/// The generation state of a runtime chunk.
fn state(chunk: &slots::ProtoChunk) -> Option<&ProtoChunk> {
    chunk
        .generation
        .as_ref()
        .and_then(|g| g.0.downcast_ref::<ProtoChunk>())
}

/// The stored tags of a chunk ([`slots::ProtoChunk::extra`]) as a compound.
fn extra(chunk: &slots::ProtoChunk) -> Compound {
    chunk
        .extra
        .as_ref()
        .and_then(|bytes| {
            vanillite_nbt::read::read_network(&mut &bytes[..], &mut Budget::file()).ok()
        })
        .and_then(|tag| match tag {
            Some(Tag::Compound(c)) => Some(c),
            _ => None,
        })
        .unwrap_or_default()
}

/// The runtime's chunk as a proto chunk: its sections plus the generation state it carries, or
/// (for chunks that come from storage) one derived from its blocks. Without the starts and
/// references of chunks from storage; [`VanillaChunkGenerator::take`] adds them.
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

/// `packStructureData`: the chunk's `structures` tag.
fn structure_data(registry: &StructureRegistry, proto: &ProtoChunk) -> Compound {
    let mut starts = Compound::new();
    for (structure, start) in proto.structure_starts() {
        starts.insert(
            registry.structure(*structure).name.as_str(),
            start.create_tag(registry),
        );
    }
    let mut references = Compound::new();
    for (structure, set) in proto.structure_references() {
        if !set.is_empty() {
            references.insert(
                registry.structure(*structure).name.as_str(),
                set.iter().collect::<Vec<i64>>(),
            );
        }
    }
    Compound::new()
        .with("starts", starts)
        .with("References", references)
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

impl VanillaChunkGenerator {
    /// [`take`] plus the starts and references of a chunk from storage.
    fn take(&self, chunk: &slots::ProtoChunk) -> ProtoChunk {
        let mut proto = take(chunk);
        if state(chunk).is_none() && chunk.status >= ChunkStatus::StructureStarts {
            for (structure, start) in self.starts_of(chunk) {
                proto.set_start_for_structure(structure, start);
            }
            if chunk.status >= ChunkStatus::StructureReferences {
                self.read_references(chunk, &mut proto);
            }
        }
        proto
    }

    /// [`put`] plus the chunk's structure data and generation entities in its stored tags.
    fn put(&self, chunk: &mut slots::ProtoChunk, proto: ProtoChunk) {
        let mut tags = extra(chunk);
        tags.insert(
            "structures",
            structure_data(self.generator.structures(), &proto),
        );
        if !proto.entities().is_empty() {
            let entities = proto
                .entities()
                .iter()
                .cloned()
                .map(Tag::Compound)
                .collect();
            tags.insert("entities", Tag::List(List(entities)));
        }
        let mut bytes = Vec::new();
        if vanillite_nbt::write::write_network_compound(&tags, &mut bytes).is_ok() {
            chunk.extra = Some(bytes.into());
        }
        put(chunk, proto);
    }

    /// `unpackStructureReferences`: the stored references of a chunk from storage.
    fn read_references(&self, chunk: &slots::ProtoChunk, proto: &mut ProtoChunk) {
        let tags = extra(chunk);
        let Some(references) = tags
            .get("structures")
            .and_then(Tag::as_compound)
            .and_then(|s| s.get("References"))
            .and_then(Tag::as_compound)
        else {
            return;
        };
        let registry = self.generator.structures();
        for (name, value) in references.iter() {
            let (Some(structure), Tag::LongArray(keys)) = (registry.structure_id(name), value)
            else {
                continue;
            };
            let keys: Vec<i64> = keys
                .iter()
                .copied()
                .filter(|&key| ChunkPos::from_long(key).chessboard_distance(chunk.pos) <= 8)
                .collect();
            if !keys.is_empty() {
                proto.set_references(structure, LongOpenHashSet::from_array(&keys));
            }
        }
    }

    /// The structure starts of a chunk past `StructureStarts`: from its generation state, kept
    /// from earlier, or regenerated (chunks from storage that saved starts).
    fn starts_of(&self, chunk: &slots::ProtoChunk) -> ChunkStarts {
        if !self.structures || chunk.status < ChunkStatus::StructureStarts {
            return ChunkStarts::new();
        }
        if let Some(state) = state(chunk) {
            return state.structure_starts().clone();
        }
        if let Some(starts) = self.kept_starts().get(&chunk.pos) {
            return starts.clone();
        }
        let stored = extra(chunk)
            .get("structures")
            .and_then(Tag::as_compound)
            .and_then(|s| s.get("starts"))
            .and_then(Tag::as_compound)
            .is_some_and(|starts| !starts.is_empty());
        let starts = if stored {
            let mut proto = self.generator.new_chunk(chunk.pos);
            self.generator
                .generate_structure_starts(std::slice::from_mut(&mut proto));
            proto.structure_starts().clone()
        } else {
            ChunkStarts::new()
        };
        self.keep_starts(chunk.pos, starts)
    }

    fn kept_starts(&self) -> std::sync::MutexGuard<'_, FxHashMap<ChunkPos, ChunkStarts>> {
        self.starts.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Keeps the starts of a chunk; returns the kept ones (another job may have been first).
    fn keep_starts(&self, pos: ChunkPos, starts: ChunkStarts) -> ChunkStarts {
        let mut kept = self.kept_starts();
        if kept.len() >= MAX_KEPT_STARTS {
            kept.clear();
        }
        kept.entry(pos).or_insert(starts).clone()
    }

    /// The start of `structure` in the chunk at `pos`, which is `target` or one of the
    /// neighbours.
    fn start_at(
        &self,
        target: &slots::ProtoChunk,
        neighbours: &Neighbours<'_>,
        pos: ChunkPos,
        structure: crate::structure::StructureId,
    ) -> Option<Arc<crate::structure::StructureStart>> {
        let chunk = if pos == target.pos {
            target
        } else {
            neighbours.get(pos)?
        };
        self.starts_of(chunk)
            .into_iter()
            .find(|(s, _)| *s == structure)
            .map(|(_, start)| start)
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
            ChunkStatus::StructureStarts => {
                for target in targets.iter_mut() {
                    let mut proto = self.take(target);
                    if self.structures {
                        self.generator
                            .generate_structure_starts(std::slice::from_mut(&mut proto));
                        let starts = proto.structure_starts().clone();
                        if !starts.is_empty() {
                            self.keep_starts(target.pos, starts);
                        }
                    }
                    self.put(target, proto);
                }
            }
            ChunkStatus::StructureReferences => {
                for target in targets.iter_mut() {
                    let center = target.pos;
                    let starts = NeighbourStarts::from_starts(center, |dx, dz| {
                        let pos = ChunkPos::new(center.x + dx, center.z + dz);
                        match (dx, dz) {
                            (0, 0) => self.starts_of(target),
                            _ => neighbours
                                .get(pos)
                                .map_or_else(ChunkStarts::new, |chunk| self.starts_of(chunk)),
                        }
                    });
                    let mut proto = self.take(target);
                    self.generator
                        .generate_structure_references(&mut proto, &starts);
                    self.put(target, proto);
                }
            }
            ChunkStatus::Terrain => {
                let mut batch: Vec<TerrainJob> = targets
                    .iter()
                    .map(|target| {
                        let chunk = self.take(target);
                        let structures = ReferencedStarts::collect_with(&chunk, |pos, s| {
                            self.start_at(target, neighbours, pos, s)
                        });
                        TerrainJob {
                            chunk,
                            neighbours: neighbour_biomes(target, neighbours),
                            structures,
                        }
                    })
                    .collect();
                self.generator.generate_terrain(&mut batch);
                for (target, job) in targets.iter_mut().zip(batch) {
                    self.put(target, job.chunk);
                }
            }
            ChunkStatus::Features => {
                // One target at a time: the features of a chunk write into its 3×3 area.
                for target in targets.iter_mut() {
                    self.decorate(target, neighbours);
                }
            }
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
                    self.take(target)
                } else {
                    self.take(neighbours.get(positions[i]).unwrap_or_else(|| {
                        panic!("features of {center:?} need {:?}", positions[i])
                    }))
                }
            });
        let structures = ReferencedStarts::collect_with(&area[4], |pos, s| {
            self.start_at(target, neighbours, pos, s)
        });
        let [a0, a1, a2, a3, a4, a5, a6, a7, a8] = &mut area;
        self.generator
            .generate_features([a0, a1, a2, a3, a4, a5, a6, a7, a8], &structures);
        for (i, proto) in area.into_iter().enumerate() {
            if i == 4 {
                self.put(target, proto);
            } else if let Some(chunk) = neighbours.get_mut(positions[i]) {
                self.put(chunk, proto);
            }
        }
    }
}
