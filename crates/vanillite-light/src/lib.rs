//! Built-in light engine: the `light-engine` slot with a breadth-first propagation that
//! reproduces Vanilla's final light levels.
//!
//! Light is the maximum over all sources of the source level minus the opacity along the
//! path, where a step into a block costs `max(1, dampening)` and faces closed by both blocks'
//! occlusion shapes stop it (Vanilla's `LightEngine`). Sky light sources are the cells at and
//! above each column's lowest sky source (`ChunkSkyLightSources`). Because light travels at
//! most 15 blocks, everything a chunk's sources reach lies within its 3 × 3 chunk area, and
//! everything that reaches a chunk comes from sources within its 5 × 5 area:
//!
//! - initial light propagates one chunk's own sources through its 3 × 3 area; results of
//!   different chunks combine by maximum, so neighbouring chunks light in parallel;
//! - after block changes, the light of the 3 × 3 area around a changed chunk is recomputed
//!   from all sources of its 5 × 5 area.
//!
//! Levels are processed from 15 down in buckets, so every cell is finalised once.
//! Not yet ported: Vanilla's exceptions for sections it keeps no light data for (huge empty
//! areas far from any block), and the incremental Starlight-style updates of section 11.

mod facts;

use std::sync::Arc;

use vanillite_base::{ChunkPos, Direction};
use vanillite_chunk::light::{ChunkLight, LightNibbles, NIBBLE_BYTES};
use vanillite_chunk::slots::{LightEngine, LightEngineProvider, Neighbours, WorldInfo};
use vanillite_chunk::{Chunk, ProtoChunk};

use crate::facts::LightFacts;

/// The built-in light engine of one world.
#[derive(Debug)]
pub struct BfsLightEngine {
    has_sky: bool,
}

impl BfsLightEngine {
    /// The engine of a world.
    pub fn new(world: &WorldInfo) -> Self {
        BfsLightEngine {
            has_sky: world.has_sky_light,
        }
    }
}

/// Provider of [`BfsLightEngine`].
#[derive(Debug, Default)]
pub struct BfsLightEngineProvider;

impl LightEngineProvider for BfsLightEngineProvider {
    fn create(&self, world: &WorldInfo) -> Arc<dyn LightEngine> {
        Arc::new(BfsLightEngine::new(world))
    }
}

impl LightEngine for BfsLightEngine {
    fn initialize_light(&self, targets: &mut [ProtoChunk]) {
        for target in targets {
            let chunk = &mut target.chunk;
            let highest = highest_filled_section(chunk);
            let light = chunk.light_mut();
            for (index, layer) in light.block_mut().iter_mut().enumerate() {
                *layer = if index == 0 {
                    LightNibbles::Absent
                } else {
                    LightNibbles::Uniform(0)
                };
            }
            let has_sky = self.has_sky;
            for (index, layer) in light.sky_mut().iter_mut().enumerate() {
                // Light layer `index` is section `index - 1`.
                let below_top = highest.is_some_and(|h| index >= 1 && index - 1 <= h);
                *layer = if has_sky && below_top {
                    LightNibbles::Uniform(0)
                } else {
                    LightNibbles::Absent
                };
            }
        }
    }

    fn light_chunks(&self, targets: &mut [ProtoChunk], neighbours: &mut Neighbours<'_>) {
        for target in targets {
            let center = target.pos;
            let origin = ChunkPos::new(center.x - 1, center.z - 1);
            let result = {
                let mut chunks = [None; 9];
                for (i, slot) in chunks.iter_mut().enumerate() {
                    let pos = grid_pos(origin, 3, i);
                    *slot = if pos == center {
                        Some(&target.chunk)
                    } else {
                        neighbours.get(pos).map(|c| &c.chunk)
                    };
                }
                let mut grid = Grid::new(origin, 3, &chunks);
                grid.fill_sky_sources(self.has_sky, |i| i == 4);
                grid.seed_block_sources(|i| i == 4);
                grid.run();
                grid.into_light()
            };
            for (i, light) in result.into_iter().enumerate() {
                let Some(light) = light else { continue };
                let pos = grid_pos(origin, 3, i);
                if pos == center {
                    target.chunk.light_mut().merge_max(&light);
                } else if let Some(chunk) = neighbours.get_mut(pos) {
                    chunk.chunk.light_mut().merge_max(&light);
                }
            }
            target.light_on = true;
        }
    }

    /// Light is a function of each state's emission, dampening and occlusion shape and of
    /// which sections are empty, so a change that keeps all of them keeps every light level.
    fn changes_light(&self, old: u16, new: u16) -> bool {
        let air =
            |state| vanillite_chunk::facts::block_flags(state) & vanillite_chunk::facts::AIR != 0;
        let facts = LightFacts::get();
        LightFacts::light(old) != LightFacts::light(new)
            || air(old) != air(new)
            || facts.shape_of(old) != facts.shape_of(new)
    }

    fn relight(&self, changed: &[ChunkPos], area: &mut Neighbours<'_>) {
        for &center in changed {
            let origin = ChunkPos::new(center.x - 2, center.z - 2);
            let result = {
                let mut chunks = [None; 25];
                for (i, slot) in chunks.iter_mut().enumerate() {
                    *slot = area.get(grid_pos(origin, 5, i)).map(|c| &c.chunk);
                }
                let mut grid = Grid::new(origin, 5, &chunks);
                grid.fill_sky_sources(self.has_sky, |_| true);
                grid.seed_block_sources(|_| true);
                grid.run();
                grid.into_light()
            };
            for (i, light) in result.into_iter().enumerate() {
                let pos = grid_pos(origin, 5, i);
                if pos.chessboard_distance(center) > 1 {
                    continue;
                }
                let (Some(light), Some(chunk)) = (light, area.get_mut(pos)) else {
                    continue;
                };
                let highest = highest_filled_section(&chunk.chunk);
                let target = chunk.chunk.light_mut();
                for (index, layer) in light.block().iter().enumerate().skip(1) {
                    if index <= target.block().len() - 2 {
                        target.block_mut()[index] = layer.clone();
                    }
                }
                if self.has_sky {
                    for (index, layer) in light.sky().iter().enumerate().skip(1) {
                        if index > target.sky().len() - 2 {
                            continue;
                        }
                        let above_top = highest.is_none_or(|h| index - 1 > h);
                        target.sky_mut()[index] = if above_top {
                            LightNibbles::Absent
                        } else {
                            layer.clone()
                        };
                    }
                }
            }
        }
    }
}

/// Position of grid cell `i` (row-major, z then x).
#[inline]
fn grid_pos(origin: ChunkPos, size: i32, i: usize) -> ChunkPos {
    let i = i as i32;
    ChunkPos::new(origin.x + i % size, origin.z + i / size)
}

/// Index of the highest section with a non-air block.
fn highest_filled_section(chunk: &Chunk) -> Option<usize> {
    chunk
        .sections()
        .iter()
        .rposition(|section| !section.summary().is_empty())
}

/// Light buffers of a square of chunk columns; one byte per cell.
struct Grid<'a> {
    origin: ChunkPos,
    size: usize,
    width: usize,
    /// Cells of the grid in y: from the bottom of the world to the section above the highest
    /// non-air section of the area.
    height: usize,
    /// Sections of the world.
    sections: usize,
    min_y: i32,
    chunks: &'a [Option<&'a Chunk>],
    facts: &'static LightFacts,
    sky: Vec<u8>,
    block: Vec<u8>,
    sky_queue: [Vec<u32>; 16],
    block_queue: [Vec<u32>; 16],
}

impl<'a> Grid<'a> {
    fn new(origin: ChunkPos, size: usize, chunks: &'a [Option<&'a Chunk>]) -> Self {
        let any = chunks.iter().flatten().next();
        let (min_y, sections) = any.map_or((0, 0), |c| (c.min_y(), c.sections().len()));
        // Above the highest non-air section of the area every cell is a sky source and no block
        // light reaches past the section above it: the grid ends there.
        let top = chunks
            .iter()
            .flatten()
            .filter_map(|c| highest_filled_section(c))
            .max();
        let height = top.map_or(0, |t| (t + 2).min(sections)) * 16;
        let width = size * 16;
        let cells = width * width * height;
        Grid {
            origin,
            size,
            width,
            height,
            sections,
            min_y,
            chunks,
            facts: LightFacts::get(),
            sky: vec![0; cells],
            block: vec![0; cells],
            sky_queue: Default::default(),
            block_queue: Default::default(),
        }
    }

    #[inline]
    fn index(&self, x: usize, y: usize, z: usize) -> usize {
        (y * self.width + z) * self.width + x
    }

    #[inline]
    fn coords(&self, index: usize) -> (usize, usize, usize) {
        let x = index % self.width;
        let z = (index / self.width) % self.width;
        (x, index / (self.width * self.width), z)
    }

    /// The chunk containing grid column x/z.
    #[inline]
    fn chunk(&self, x: usize, z: usize) -> Option<&'a Chunk> {
        self.chunks[(z >> 4) * self.size + (x >> 4)]
    }

    /// Block state at grid coordinates; `None` in missing chunks (they block light like
    /// Vanilla's bedrock stand-in).
    #[inline]
    fn state(&self, x: usize, y: usize, z: usize) -> Option<u16> {
        let chunk = self.chunk(x, z)?;
        let section = &chunk.sections()[y >> 4];
        Some(section.block((x & 15) as u32, (y & 15) as u32, (z & 15) as u32))
    }

    /// `ChunkSkyLightSources.findLowestSourceY` relative to the grid's bottom: cells at and
    /// above it are sky sources. 0 means the whole column, `height` none.
    fn lowest_source(&self, chunk: &Chunk, x: usize, z: usize) -> usize {
        let Some(top) = highest_filled_section(chunk) else {
            return 0;
        };
        let (lx, lz) = ((x & 15) as u32, (z & 15) as u32);
        let air = 0u16;
        let mut top_state = air;
        let mut top_y = (top + 1) * 16;
        for index in (0..=top).rev() {
            let section = &chunk.sections()[index];
            if section.summary().is_empty() {
                top_state = air;
                top_y = index * 16;
                continue;
            }
            for local in (0..16u32).rev() {
                let bottom = section.block(lx, local, lz);
                if self.facts.edge_occluded(top_state, bottom) {
                    return top_y;
                }
                top_state = bottom;
                top_y = index * 16 + local as usize;
            }
        }
        0
    }

    /// Sets the sky sources of all present chunks to 15 and queues those of the chunks
    /// `seed(i)` selects, where light can leave them (Vanilla's `propagateLightSources`).
    fn fill_sky_sources(&mut self, has_sky: bool, seed: impl Fn(usize) -> bool) {
        if !has_sky || self.height == 0 {
            return;
        }
        let width = self.width;
        let mut lowest = vec![self.height; width * width];
        for z in 0..width {
            for x in 0..width {
                if let Some(chunk) = self.chunk(x, z) {
                    lowest[z * width + x] = self.lowest_source(chunk, x, z);
                }
            }
        }
        for z in 0..width {
            for x in 0..width {
                let low = lowest[z * width + x];
                let seeded = seed((z >> 4) * self.size + (x >> 4));
                // A missing neighbour column counts as having sources everywhere
                // below the world (no sideways propagation into it).
                let side = |dx: isize, dz: isize| {
                    let nx = x.checked_add_signed(dx).filter(|&v| v < width);
                    let nz = z.checked_add_signed(dz).filter(|&v| v < width);
                    match (nx, nz) {
                        (Some(nx), Some(nz)) if self.chunk(nx, nz).is_some() => {
                            lowest[nz * width + nx]
                        }
                        _ => 0,
                    }
                };
                let neighbour_low = side(-1, 0).max(side(1, 0)).max(side(0, -1)).max(side(0, 1));
                for y in low..self.height {
                    let index = self.index(x, y, z);
                    self.sky[index] = 15;
                    if seeded && (y == low || y < neighbour_low) {
                        self.sky_queue[15].push(index as u32);
                    }
                }
            }
        }
    }

    /// Sets and queues the block light sources of the chunks `seed(i)` selects.
    fn seed_block_sources(&mut self, seed: impl Fn(usize) -> bool) {
        for i in 0..self.chunks.len() {
            let Some(chunk) = self.chunks[i] else {
                continue;
            };
            if !seed(i) {
                continue;
            }
            let base_x = (i % self.size) * 16;
            let base_z = (i / self.size) * 16;
            for (index, section) in chunk.sections().iter().enumerate() {
                if section.summary().is_empty() {
                    continue;
                }
                let mut emits = false;
                section.blocks().for_each_count(|state, _| {
                    emits |= LightFacts::light(state).0 > 0;
                });
                if !emits {
                    continue;
                }
                for y in 0..16u32 {
                    for z in 0..16u32 {
                        for x in 0..16u32 {
                            let emission = LightFacts::light(section.block(x, y, z)).0;
                            if emission == 0 {
                                continue;
                            }
                            let cell = self.index(
                                base_x + x as usize,
                                index * 16 + y as usize,
                                base_z + z as usize,
                            );
                            if self.block[cell] < emission {
                                self.block[cell] = emission;
                                self.block_queue[usize::from(emission)].push(cell as u32);
                            }
                        }
                    }
                }
            }
        }
    }

    fn run(&mut self) {
        let mut sky = std::mem::take(&mut self.sky);
        let mut queue = std::mem::take(&mut self.sky_queue);
        self.propagate(&mut sky, &mut queue);
        self.sky = sky;
        let mut block = std::mem::take(&mut self.block);
        let mut queue = std::mem::take(&mut self.block_queue);
        self.propagate(&mut block, &mut queue);
        self.block = block;
    }

    /// Propagates queued levels, highest first (`propagateIncrease`).
    fn propagate(&self, light: &mut [u8], queue: &mut [Vec<u32>; 16]) {
        let (width, height) = (self.width, self.height);
        for level in (2..=15u8).rev() {
            while let Some(cell) = queue[usize::from(level)].pop() {
                let cell = cell as usize;
                if light[cell] != level {
                    continue;
                }
                let (x, y, z) = self.coords(cell);
                let Some(from) = self.state(x, y, z) else {
                    continue;
                };
                for dir in Direction::ALL {
                    let [dx, dy, dz] = dir.normal();
                    let (Some(tx), Some(ty), Some(tz)) = (
                        x.checked_add_signed(dx as isize).filter(|&v| v < width),
                        y.checked_add_signed(dy as isize).filter(|&v| v < height),
                        z.checked_add_signed(dz as isize).filter(|&v| v < width),
                    ) else {
                        continue;
                    };
                    let to_cell = self.index(tx, ty, tz);
                    let current = light[to_cell];
                    if level - 1 <= current {
                        continue;
                    }
                    let Some(to) = self.state(tx, ty, tz) else {
                        continue;
                    };
                    let new = level.saturating_sub(LightFacts::opacity(to));
                    if new <= current || self.facts.occludes(from, to, dir) {
                        continue;
                    }
                    light[to_cell] = new;
                    if new > 1 {
                        queue[usize::from(new)].push(to_cell as u32);
                    }
                }
            }
        }
    }

    /// The computed light per present chunk, in the chunk's layout (layer `i + 1` is section
    /// `i`; the border layers stay absent). Sections without light are `Uniform(0)`; above the
    /// grid sky light is absent (full) and block light 0.
    fn into_light(self) -> Vec<Option<ChunkLight>> {
        let grid_sections = self.height / 16;
        (0..self.chunks.len())
            .map(|i| {
                self.chunks[i]?;
                let base_x = (i % self.size) * 16;
                let base_z = (i / self.size) * 16;
                let mut light = ChunkLight::new(self.sections);
                for section in 0..self.sections {
                    if section < grid_sections {
                        light.sky_mut()[section + 1] =
                            self.layer(&self.sky, base_x, base_z, section);
                        light.block_mut()[section + 1] =
                            self.layer(&self.block, base_x, base_z, section);
                    } else {
                        light.block_mut()[section + 1] = LightNibbles::Uniform(0);
                    }
                }
                Some(light)
            })
            .collect()
    }

    fn layer(&self, buffer: &[u8], base_x: usize, base_z: usize, section: usize) -> LightNibbles {
        let mut bytes = [0u8; NIBBLE_BYTES];
        let (mut min, mut max) = (15u8, 0u8);
        for y in 0..16 {
            for z in 0..16 {
                let row = self.index(base_x, section * 16 + y, base_z + z);
                let levels = &buffer[row..row + 16];
                // Nibble `(y << 8) | (z << 4) | x`: even x in the low half of a byte.
                let out = &mut bytes[((y << 8) | (z << 4)) >> 1..][..8];
                for (byte, &[low, high]) in out.iter_mut().zip(levels.as_chunks::<2>().0) {
                    *byte = low | (high << 4);
                }
                for &level in levels {
                    min = min.min(level);
                    max = max.max(level);
                }
            }
        }
        if min == max {
            LightNibbles::Uniform(min)
        } else {
            LightNibbles::Data(Arc::new(bytes))
        }
    }
}

impl std::fmt::Debug for Grid<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Grid")
            .field("origin", &self.origin)
            .field("size", &self.size)
            .field("min_y", &self.min_y)
            .finish_non_exhaustive()
    }
}
