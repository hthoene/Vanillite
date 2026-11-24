//! `ProtoChunk`: a chunk while it is being generated.
//!
//! Block and biome data live in [`vanillite_chunk::Section`]s, so a finished proto chunk turns
//! into a [`vanillite_chunk::Chunk`] without copying. Heightmaps are kept as plain `i32`s: up to
//! the terrain stage the two world generation heightmaps (`WORLD_SURFACE_WG`, `OCEAN_FLOOR_WG`)
//! follow block changes; after it they are frozen and the four final heightmaps follow instead
//! (`ChunkStatus.heightmapsAfter`).

use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};
use vanillite_base::{BlockPos, ChunkPos};
use vanillite_chunk::{BiomeId, Chunk, ChunkLight, HeightmapKind, Section, facts};
use vanillite_data::block::{BlockId, BlockState};
use vanillite_nbt::Compound;

/// A world generation heightmap (`Heightmap.Types.*_WG`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WgHeightmap {
    /// `WORLD_SURFACE_WG`: highest non-air block.
    WorldSurface,
    /// `OCEAN_FLOOR_WG`: highest block in `blocks_motion_in_heightmap`.
    OceanFloor,
}

impl WgHeightmap {
    /// The heightmap type.
    pub const fn kind(self) -> HeightmapKind {
        match self {
            Self::WorldSurface => HeightmapKind::WorldSurfaceWg,
            Self::OceanFloor => HeightmapKind::OceanFloorWg,
        }
    }
}

/// The heightmaps that follow block changes after the terrain stage (`Heightmap.Types` with
/// `Usage.LIVE_WORLD` and `WORLDGEN`, in `EnumSet` order).
const FINAL: [HeightmapKind; 4] = [
    HeightmapKind::WorldSurface,
    HeightmapKind::OceanFloor,
    HeightmapKind::MotionBlocking,
    HeightmapKind::MotionBlockingNoLeaves,
];

/// A tick scheduled during generation (`SavedTick` in `ProtoChunkTicks`): the delay is always 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProtoTick<T> {
    /// The block or fluid that ticks.
    pub kind: T,
    pub pos: BlockPos,
    /// `TickPriority` value (-3 extremely high .. 3 extremely low).
    pub priority: i8,
}

/// Ticks scheduled during generation, deduplicated by type and position (`ProtoChunkTicks`).
#[derive(Clone, Debug)]
pub struct ProtoTicks<T> {
    ticks: Vec<ProtoTick<T>>,
    unique: FxHashSet<(T, BlockPos)>,
}

impl<T> Default for ProtoTicks<T> {
    fn default() -> Self {
        Self {
            ticks: Vec::new(),
            unique: FxHashSet::default(),
        }
    }
}

impl<T: Copy + Eq + std::hash::Hash> ProtoTicks<T> {
    /// `schedule(tick)`: ignored if a tick of the same type is already at the position.
    pub fn schedule(&mut self, kind: T, pos: BlockPos, priority: i8) {
        if self.unique.insert((kind, pos)) {
            self.ticks.push(ProtoTick {
                kind,
                pos,
                priority,
            });
        }
    }

    /// `hasScheduledTick(pos, type)`.
    pub fn has(&self, kind: T, pos: BlockPos) -> bool {
        self.unique.contains(&(kind, pos))
    }

    /// The ticks in scheduling order.
    pub fn ticks(&self) -> &[ProtoTick<T>] {
        &self.ticks
    }
}

/// Fluid ids for scheduled fluid ticks: `minecraft:water` or `minecraft:lava`, as the fluid's
/// still/source block.
pub type FluidTickKind = BlockId;

/// A chunk during generation.
#[derive(Clone, Debug)]
pub struct ProtoChunk {
    pos: ChunkPos,
    min_y: i32,
    sections: Vec<Section>,
    /// First free Y above the highest matching block per column (`x + z * 16`), per heightmap,
    /// indexed by [`HeightmapKind::id`].
    heightmaps: [[i32; 256]; 6],
    /// Whether the terrain stage is done: block changes update the final heightmaps from then on.
    terrain_done: bool,
    /// Positions (`BlockPos.asLong` layout is not needed: x, y, z) marked for post-processing.
    post_processing: Vec<(i32, i32, i32)>,
    /// Block entity data by position (`setBlockEntityNbt`, `DUMMY` placeholders included).
    block_entities: FxHashMap<BlockPos, Compound>,
    block_ticks: ProtoTicks<BlockId>,
    fluid_ticks: ProtoTicks<FluidTickKind>,
}

impl ProtoChunk {
    /// An empty chunk of air with every biome set to `biome`.
    pub fn new(pos: ChunkPos, min_y: i32, height: i32, biome: BiomeId) -> Self {
        assert!(
            min_y % 16 == 0 && height % 16 == 0 && height > 0,
            "unaligned chunk height"
        );
        let air = BlockState::AIR.0;
        Self {
            pos,
            min_y,
            sections: (0..height / 16).map(|_| Section::new(air, biome)).collect(),
            heightmaps: [[min_y; 256]; 6],
            terrain_done: false,
            post_processing: Vec::new(),
            block_entities: FxHashMap::default(),
            block_ticks: ProtoTicks::default(),
            fluid_ticks: ProtoTicks::default(),
        }
    }

    /// The chunk position.
    pub fn pos(&self) -> ChunkPos {
        self.pos
    }

    /// Lowest block Y (`getMinY`).
    pub fn min_y(&self) -> i32 {
        self.min_y
    }

    /// Number of block rows (`getHeight`).
    pub fn height(&self) -> i32 {
        self.sections.len() as i32 * 16
    }

    /// Highest block Y (`getMaxY`).
    pub fn max_y(&self) -> i32 {
        self.min_y + self.height() - 1
    }

    /// `isOutsideBuildHeight(y)`.
    pub fn is_outside(&self, y: i32) -> bool {
        y < self.min_y || y > self.max_y()
    }

    /// `getSectionIndex(y)`.
    pub fn section_index(&self, y: i32) -> usize {
        ((y >> 4) - (self.min_y >> 4)) as usize
    }

    /// Sections, bottom first.
    pub fn sections(&self) -> &[Section] {
        &self.sections
    }

    /// Mutable section by index.
    pub fn section_mut(&mut self, index: usize) -> &mut Section {
        &mut self.sections[index]
    }

    /// `getHighestFilledSectionIndex()`: the highest section with a non-air block, or -1.
    pub fn highest_filled_section_index(&self) -> i32 {
        self.sections
            .iter()
            .rposition(|s| !s.summary().is_empty())
            .map_or(-1, |i| i as i32)
    }

    /// `getBlockState(x, y, z)` with world coordinates; air outside the height.
    #[inline]
    pub fn block(&self, x: i32, y: i32, z: i32) -> BlockState {
        if self.is_outside(y) {
            return BlockState::AIR;
        }
        BlockState(self.sections[self.section_index(y)].block(
            (x & 15) as u32,
            (y & 15) as u32,
            (z & 15) as u32,
        ))
    }

    /// `getBlockState(pos)` with Vanilla's `VOID_AIR` outside the build height.
    #[inline]
    pub fn block_or_void(&self, x: i32, y: i32, z: i32) -> BlockState {
        if self.is_outside(y) {
            return void_air();
        }
        self.block(x, y, z)
    }

    /// `ProtoChunk.setBlockState(pos, state)` before `INITIALIZE_LIGHT`: sets the block and
    /// updates the heightmaps of the current status. Returns the old state (`VOID_AIR` outside
    /// the build height).
    pub fn set_block(&mut self, x: i32, y: i32, z: i32, state: BlockState) -> BlockState {
        if self.is_outside(y) {
            return void_air();
        }
        let index = self.section_index(y);
        let section = &mut self.sections[index];
        if section.summary().is_empty() && state == BlockState::AIR {
            return state;
        }
        let (lx, lz) = ((x & 15) as u32, (z & 15) as u32);
        let old = BlockState(section.set_block(lx, (y & 15) as u32, lz, state.0));
        let (lx, lz) = (lx as usize, lz as usize);
        if self.terrain_done {
            for kind in FINAL {
                self.update_kind(kind, lx, y, lz, state);
            }
        } else {
            self.update_kind(HeightmapKind::WorldSurfaceWg, lx, y, lz, state);
            self.update_kind(HeightmapKind::OceanFloorWg, lx, y, lz, state);
        }
        old
    }

    /// Ends the terrain stage: primes the final heightmaps from the blocks
    /// (`Heightmap.primeHeightmaps`); from now on block changes update them instead of the
    /// world generation heightmaps.
    pub fn finish_terrain(&mut self) {
        self.terrain_done = true;
        let top = (self.highest_filled_section_index() + 1) * 16 + self.min_y;
        for column in 0..256 {
            let (x, z) = ((column & 15) as i32, (column >> 4) as i32);
            for kind in FINAL {
                self.heightmaps[kind.id() as usize][column] = self.min_y;
            }
            let mut remaining = FINAL.len();
            let mut done = [false; 4];
            for y in (self.min_y..top).rev() {
                let state = self.block(x, y, z);
                if state == BlockState::AIR {
                    continue;
                }
                let flags = facts::block_flags(state.0);
                for (i, kind) in FINAL.into_iter().enumerate() {
                    if !done[i] && kind.matches(flags) {
                        done[i] = true;
                        remaining -= 1;
                        self.heightmaps[kind.id() as usize][column] = y + 1;
                    }
                }
                if remaining == 0 {
                    break;
                }
            }
        }
    }

    /// Whether [`Self::finish_terrain`] ran.
    pub fn terrain_done(&self) -> bool {
        self.terrain_done
    }

    /// `LevelChunkSection.setBlockState` without heightmap updates (as `doFill` writes).
    #[inline]
    pub fn set_block_raw(&mut self, x: i32, y: i32, z: i32, state: BlockState) {
        let index = self.section_index(y);
        self.sections[index].set_block((x & 15) as u32, (y & 15) as u32, (z & 15) as u32, state.0);
    }

    /// `getHeight(type, x, z)` of a world generation heightmap: the highest matching block Y,
    /// or `min_y - 1`. Local x/z.
    #[inline]
    pub fn height_at(&self, map: WgHeightmap, x: usize, z: usize) -> i32 {
        self.heightmap(map.kind(), x, z)
    }

    /// `getHeight(type, x, z)`: the highest matching block Y, or `min_y - 1`. Local x/z. The
    /// final heightmaps are only valid after [`Self::finish_terrain`].
    #[inline]
    pub fn heightmap(&self, kind: HeightmapKind, x: usize, z: usize) -> i32 {
        self.heightmaps[kind.id() as usize][x + z * 16] - 1
    }

    /// `Heightmap.update(x, y, z, state)` of a world generation heightmap with local x/z and
    /// world y.
    pub fn update_heightmap(
        &mut self,
        map: WgHeightmap,
        x: usize,
        y: i32,
        z: usize,
        state: BlockState,
    ) {
        self.update_kind(map.kind(), x, y, z, state);
    }

    fn update_kind(&mut self, kind: HeightmapKind, x: usize, y: i32, z: usize, state: BlockState) {
        let index = kind.id() as usize;
        let column = x + z * 16;
        let first_available = self.heightmaps[index][column];
        if y <= first_available - 2 {
            return;
        }
        if kind.matches(facts::block_flags(state.0)) {
            if y >= first_available {
                self.heightmaps[index][column] = y + 1;
            }
        } else if first_available - 1 == y {
            let mut found = self.min_y;
            for below in (self.min_y..y).rev() {
                if kind.matches(facts::block_flags(self.block(x as i32, below, z as i32).0)) {
                    found = below + 1;
                    break;
                }
            }
            self.heightmaps[index][column] = found;
        }
    }

    /// Block entity data at a position.
    pub fn block_entity(&self, pos: BlockPos) -> Option<&Compound> {
        self.block_entities.get(&pos)
    }

    /// Mutable block entity data at a position.
    pub fn block_entity_mut(&mut self, pos: BlockPos) -> Option<&mut Compound> {
        self.block_entities.get_mut(&pos)
    }

    /// `setBlockEntityNbt`: replaces the block entity data at a position.
    pub fn set_block_entity(&mut self, pos: BlockPos, data: Compound) {
        self.block_entities.insert(pos, data);
    }

    /// `removeBlockEntity(pos)`.
    pub fn remove_block_entity(&mut self, pos: BlockPos) {
        self.block_entities.remove(&pos);
    }

    /// All block entities.
    pub fn block_entities(&self) -> &FxHashMap<BlockPos, Compound> {
        &self.block_entities
    }

    /// Scheduled block ticks.
    pub fn block_ticks(&self) -> &ProtoTicks<BlockId> {
        &self.block_ticks
    }

    /// Scheduled block ticks, mutable.
    pub fn block_ticks_mut(&mut self) -> &mut ProtoTicks<BlockId> {
        &mut self.block_ticks
    }

    /// Scheduled fluid ticks.
    pub fn fluid_ticks(&self) -> &ProtoTicks<FluidTickKind> {
        &self.fluid_ticks
    }

    /// Scheduled fluid ticks, mutable.
    pub fn fluid_ticks_mut(&mut self) -> &mut ProtoTicks<FluidTickKind> {
        &mut self.fluid_ticks
    }

    /// Sets a block in its section only, as `BulkSectionAccess` writes (no heightmap updates).
    #[inline]
    pub fn set_block_in_section(
        &mut self,
        x: i32,
        y: i32,
        z: i32,
        state: BlockState,
    ) -> BlockState {
        let index = self.section_index(y);
        BlockState(self.sections[index].set_block(
            (x & 15) as u32,
            (y & 15) as u32,
            (z & 15) as u32,
            state.0,
        ))
    }

    /// `markPosForPostProcessing(pos)`; positions outside the build height are ignored.
    pub fn mark_post_processing(&mut self, x: i32, y: i32, z: i32) {
        if !self.is_outside(y) {
            self.post_processing.push((x, y, z));
        }
    }

    /// Positions marked for post-processing, in marking order (duplicates possible).
    pub fn post_processing(&self) -> &[(i32, i32, i32)] {
        &self.post_processing
    }

    /// Noise biome at quart coordinates of this chunk (`ChunkAccess.getNoiseBiome`): y is clamped
    /// to the chunk, x and z are taken modulo the chunk.
    pub fn noise_biome(&self, quart_x: i32, quart_y: i32, quart_z: i32) -> BiomeId {
        let min_quart = self.min_y >> 2;
        let max_quart = min_quart + (self.height() >> 2) - 1;
        let qy = quart_y.clamp(min_quart, max_quart);
        let section = self.section_index(qy << 2);
        self.sections[section].biome((quart_x & 3) as u32, (qy & 3) as u32, (quart_z & 3) as u32)
    }

    /// Sets the noise biome at quart coordinates inside the chunk.
    pub fn set_noise_biome(&mut self, quart_x: i32, quart_y: i32, quart_z: i32, biome: BiomeId) {
        let section = self.section_index(quart_y << 2);
        self.sections[section].biomes_mut().set_at(
            (quart_x & 3) as u32,
            (quart_y & 3) as u32,
            (quart_z & 3) as u32,
            biome,
        );
    }

    /// The finished chunk; its heightmaps are primed from the blocks.
    pub fn into_chunk(self) -> Chunk {
        let count = self.sections.len();
        Chunk::from_sections(
            self.min_y >> 4,
            self.sections.into_iter().map(Arc::new).collect(),
            ChunkLight::new(count),
        )
    }
}

/// `Blocks.VOID_AIR.defaultBlockState()`.
pub fn void_air() -> BlockState {
    use std::sync::OnceLock;
    static VOID_AIR: OnceLock<BlockState> = OnceLock::new();
    *VOID_AIR.get_or_init(|| {
        BlockId::by_name("minecraft:void_air")
            .expect("void_air")
            .default_state()
    })
}
