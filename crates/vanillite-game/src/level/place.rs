//! `/place` on the live world: the live chunks around a placement are copied into generation
//! chunks, the generation runs there with its `setBlock` calls recorded, and the record is
//! replayed on the level (every `setBlock` in call order with its flags), followed by the block
//! entity data, the entities and the scheduled ticks the generation left. Chunks of the area
//! that are not loaded take part as empty chunks and keep nothing.

use vanillite_base::{BlockPos, ChunkPos};
use vanillite_data::block::BlockState;
use vanillite_nbt::Tag;
use vanillite_worldgen::feature::WorldGenRegion;
use vanillite_worldgen::generator::TerrainGenerator;
use vanillite_worldgen::proto::ProtoChunk;

use super::{Level, update};

/// A square of chunks copied from a level for one placement.
#[derive(Debug)]
pub struct PlaceArea {
    radius: i32,
    /// Row-major (`dz` outer, `dx` inner) over `-radius..=radius` around the centre.
    chunks: Vec<ProtoChunk>,
    /// Which chunks are live in the level.
    live: Vec<bool>,
    writes: Vec<(BlockPos, BlockState, i32)>,
}

impl PlaceArea {
    /// Copies the chunks of the square around `min..=max` (chunk corners).
    pub fn copy(level: &Level, generator: &TerrainGenerator, min: ChunkPos, max: ChunkPos) -> Self {
        Self::copy_with(level, min, max, |pos| generator.new_chunk(pos))
    }

    /// Like [`Self::copy`] without a generator: chunks that are not loaded are empty.
    pub fn copy_plain(level: &Level, min: ChunkPos, max: ChunkPos) -> Self {
        let height = level.max_y() - level.min_y() + 1;
        Self::copy_with(level, min, max, |pos| {
            ProtoChunk::new(pos, level.min_y(), height, 0)
        })
    }

    fn copy_with(
        level: &Level,
        min: ChunkPos,
        max: ChunkPos,
        empty: impl Fn(ChunkPos) -> ProtoChunk,
    ) -> Self {
        let center = ChunkPos::new(min.x + (max.x - min.x) / 2, min.z + (max.z - min.z) / 2);
        let radius = (center.x - min.x)
            .max(max.x - center.x)
            .max(center.z - min.z)
            .max(max.z - center.z)
            .max(0);
        let mut chunks = Vec::new();
        let mut live = Vec::new();
        for dz in -radius..=radius {
            for dx in -radius..=radius {
                let pos = ChunkPos::new(center.x + dx, center.z + dz);
                match level.chunks.chunk(pos) {
                    Some(stored) => {
                        let sections = stored
                            .chunk
                            .sections()
                            .iter()
                            .map(|s| (**s).clone())
                            .collect();
                        chunks.push(ProtoChunk::from_sections(
                            pos,
                            level.min_y(),
                            sections,
                            true,
                        ));
                        live.push(true);
                    }
                    None => {
                        chunks.push(empty(pos));
                        live.push(false);
                    }
                }
            }
        }
        Self {
            radius,
            chunks,
            live,
            writes: Vec::new(),
        }
    }

    /// Runs generation on the copy, recording its writes.
    pub fn run<R>(
        &mut self,
        generator: &TerrainGenerator,
        f: impl FnOnce(&mut WorldGenRegion<'_>) -> R,
    ) -> R {
        let mut region = generator.region(self.chunks.iter_mut().collect(), self.radius);
        region.record_writes();
        let result = f(&mut region);
        self.writes.extend(region.take_writes());
        result
    }

    /// Runs `f` on the copy as a plain region of the level's seed (no generator: template
    /// placement), recording its writes.
    pub fn run_plain<R>(&mut self, seed: i64, f: impl FnOnce(&mut WorldGenRegion<'_>) -> R) -> R {
        let random_state =
            vanillite_worldgen::random_state::RandomState::without_noises(seed, false);
        let mut region = WorldGenRegion::new(
            self.chunks.iter_mut().collect(),
            self.radius,
            self.radius,
            &random_state,
            63,
        );
        region.record_writes();
        let result = f(&mut region);
        self.writes.extend(region.take_writes());
        result
    }

    /// Replays what generation did on the level; returns whether anything was written.
    pub fn apply(self, level: &mut Level) -> bool {
        let live: vanillite_base::hash::FxHashSet<ChunkPos> = self
            .chunks
            .iter()
            .zip(&self.live)
            .filter(|(_, live)| **live)
            .map(|(c, _)| c.pos())
            .collect();
        let mut any = false;
        for (pos, state, flags) in self.writes {
            if live.contains(&pos.chunk()) {
                level.set_block(pos, state, u32::try_from(flags).unwrap_or(update::ALL));
                any = true;
            }
        }
        for (chunk, _) in self.chunks.iter().zip(&self.live).filter(|(_, l)| **l) {
            for (&pos, nbt) in chunk.block_entities() {
                let dummy = matches!(nbt.get("id"), Some(Tag::String(id)) if id == "DUMMY");
                if !dummy && level.block_entity_nbt(pos).is_some() {
                    level.load_block_entity_nbt(pos, nbt);
                }
            }
            for entity in chunk.entities() {
                level.load_new_entity(entity.clone(), vanillite_abi::spawn_reason::STRUCTURE);
            }
            for tick in chunk.block_ticks().ticks() {
                level.schedule_block_tick(tick.pos, tick.kind, 0);
            }
            for tick in chunk.fluid_ticks().ticks() {
                level.schedule_fluid_tick(tick.pos, tick.kind, 0);
            }
        }
        any
    }
}
