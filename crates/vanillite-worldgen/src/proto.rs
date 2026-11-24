//! `ProtoChunk`: a chunk while it is being generated.
//!
//! Block and biome data live in [`vanillite_chunk::Section`]s, so a finished proto chunk turns
//! into a [`vanillite_chunk::Chunk`] without copying. During generation only the two world
//! generation heightmaps exist (`WORLD_SURFACE_WG`, `OCEAN_FLOOR_WG`), kept as plain `i32`s.

use std::sync::Arc;

use vanillite_base::ChunkPos;
use vanillite_chunk::{BiomeId, Chunk, ChunkLight, Section, facts};
use vanillite_data::block::BlockState;

/// A world generation heightmap (`Heightmap.Types.*_WG`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WgHeightmap {
    /// `WORLD_SURFACE_WG`: highest non-air block.
    WorldSurface,
    /// `OCEAN_FLOOR_WG`: highest block in `blocks_motion_in_heightmap`.
    OceanFloor,
}

impl WgHeightmap {
    fn index(self) -> usize {
        self as usize
    }

    fn matches(self, state: BlockState) -> bool {
        let flags = facts::block_flags(state.0);
        match self {
            Self::WorldSurface => flags & facts::AIR == 0,
            Self::OceanFloor => flags & facts::BLOCKS_MOTION != 0,
        }
    }
}

/// A chunk during generation.
#[derive(Clone, Debug)]
pub struct ProtoChunk {
    pos: ChunkPos,
    min_y: i32,
    sections: Vec<Section>,
    /// First free Y above the highest matching block per column (`x + z * 16`), per heightmap.
    heightmaps: [[i32; 256]; 2],
    /// Positions (`BlockPos.asLong` layout is not needed: x, y, z) marked for post-processing.
    post_processing: Vec<(i32, i32, i32)>,
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
            heightmaps: [[min_y; 256]; 2],
            post_processing: Vec::new(),
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

    /// `ProtoChunk.setBlockState(pos, state)` before `INITIALIZE_LIGHT`: sets the block and
    /// updates the world generation heightmaps. Returns the old state.
    pub fn set_block(&mut self, x: i32, y: i32, z: i32, state: BlockState) -> BlockState {
        if self.is_outside(y) {
            return BlockState::AIR;
        }
        let index = self.section_index(y);
        let section = &mut self.sections[index];
        if section.summary().is_empty() && state == BlockState::AIR {
            return state;
        }
        let (lx, lz) = ((x & 15) as u32, (z & 15) as u32);
        let old = BlockState(section.set_block(lx, (y & 15) as u32, lz, state.0));
        self.update_heightmap(
            WgHeightmap::WorldSurface,
            lx as usize,
            y,
            lz as usize,
            state,
        );
        self.update_heightmap(WgHeightmap::OceanFloor, lx as usize, y, lz as usize, state);
        old
    }

    /// `LevelChunkSection.setBlockState` without heightmap updates (as `doFill` writes).
    #[inline]
    pub fn set_block_raw(&mut self, x: i32, y: i32, z: i32, state: BlockState) {
        let index = self.section_index(y);
        self.sections[index].set_block((x & 15) as u32, (y & 15) as u32, (z & 15) as u32, state.0);
    }

    /// `getHeight(type, x, z)`: the highest matching block Y, or `min_y - 1`. Local x/z.
    #[inline]
    pub fn height_at(&self, map: WgHeightmap, x: usize, z: usize) -> i32 {
        self.heightmaps[map.index()][x + z * 16] - 1
    }

    /// `Heightmap.update(x, y, z, state)` with local x/z and world y.
    pub fn update_heightmap(
        &mut self,
        map: WgHeightmap,
        x: usize,
        y: i32,
        z: usize,
        state: BlockState,
    ) {
        let column = x + z * 16;
        let first_available = self.heightmaps[map.index()][column];
        if y <= first_available - 2 {
            return;
        }
        if map.matches(state) {
            if y >= first_available {
                self.heightmaps[map.index()][column] = y + 1;
            }
        } else if first_available - 1 == y {
            let mut found = self.min_y;
            for below in (self.min_y..y).rev() {
                if map.matches(self.block(x as i32, below, z as i32)) {
                    found = below + 1;
                    break;
                }
            }
            self.heightmaps[map.index()][column] = found;
        }
    }

    /// `markPosForPostProcessing(pos)`.
    pub fn mark_post_processing(&mut self, x: i32, y: i32, z: i32) {
        self.post_processing.push((x, y, z));
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
