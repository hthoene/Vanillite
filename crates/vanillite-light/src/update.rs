//! Light after block changes: only the cells a change can reach are recomputed.
//!
//! Light is the largest fixed point of "source level, or the best neighbour minus the cost of
//! the step". A change at a block can only alter cells that light passing through it reaches:
//! with `L` the highest level stored at the block and its six neighbours (or the level the new
//! block emits), those lie within `L - 1` steps. Such a box is reset and propagated again from
//! the sources inside it and from the stored levels of the cells around it, which the change
//! cannot reach and which therefore stay right. The result equals a recomputation of the whole
//! area ([`crate::BfsLightEngine::relight_area`]); boxes of several changes are processed one
//! after another, each on the result of the one before.
//!
//! Sky light sources are the cells at and above a column's lowest source
//! (`ChunkSkyLightSources`). Only sources hold level 15, so the lowest source before the change
//! is read from the stored light, the one after it from the blocks; the cells between them
//! switch and get a box as well. Columns of chunks without a change keep their sources.

use std::sync::Arc;

use vanillite_base::hash::FxHashMap;
use vanillite_base::{BlockPos, ChunkPos, Direction};
use vanillite_chunk::light::{ChunkLight, LightNibbles, NIBBLE_BYTES};
use vanillite_chunk::slots::{ChunkStatus, Neighbours, ProtoChunk};

use crate::facts::LightFacts;
use crate::{highest_filled_section, lowest_source};

/// Marks a level that is not recomputed: the cells around the box and cells without a chunk.
const FIXED: u8 = 0x80;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Sky,
    Block,
}

/// A box of cells, inclusive: x and z in blocks, y in cells above the bottom of the world.
#[derive(Clone, Copy, Debug)]
struct Region {
    min: [i32; 3],
    max: [i32; 3],
}

impl Region {
    fn volume(&self) -> i64 {
        (0..3)
            .map(|axis| i64::from(self.max[axis] - self.min[axis] + 1))
            .product()
    }

    fn union(&self, other: &Region) -> Region {
        Region {
            min: [0, 1, 2].map(|axis| self.min[axis].min(other.min[axis])),
            max: [0, 1, 2].map(|axis| self.max[axis].max(other.max[axis])),
        }
    }
}

/// Adds a box, joined with the boxes it shares enough cells with that one box is less work than
/// two.
fn add_region(regions: &mut Vec<Region>, mut region: Region) {
    while let Some(index) = regions
        .iter()
        .position(|other| other.union(&region).volume() <= other.volume() + region.volume())
    {
        region = regions.swap_remove(index).union(&region);
    }
    regions.push(region);
}

/// A light layer for reading.
enum View<'a> {
    Uniform(u8),
    Data(&'a [u8; NIBBLE_BYTES]),
    /// An absent sky layer below a data layer repeats that layer's bottom row.
    Row(&'a [u8; NIBBLE_BYTES]),
}

impl View<'_> {
    #[inline]
    fn get(&self, x: usize, y: usize, z: usize) -> u8 {
        let nibble = |data: &[u8; NIBBLE_BYTES], y: usize| {
            let index = (y << 8) | (z << 4) | x;
            (data[index >> 1] >> ((index & 1) * 4)) & 15
        };
        match self {
            View::Uniform(level) => *level,
            View::Data(data) => nibble(data, y),
            View::Row(data) => nibble(data, 0),
        }
    }

    fn to_bytes(&self) -> [u8; NIBBLE_BYTES] {
        match self {
            View::Uniform(level) => [level * 0x11; NIBBLE_BYTES],
            View::Data(data) => **data,
            View::Row(data) => {
                let mut bytes = [0; NIBBLE_BYTES];
                for row in bytes.as_chunks_mut::<128>().0 {
                    row.copy_from_slice(&data[..128]);
                }
                bytes
            }
        }
    }
}

/// Layer `layer` of a chunk's light. An absent block light layer is dark; an absent sky light
/// layer continues the data above it and is full sky light above all data, as on the client.
fn view(light: &ChunkLight, kind: Kind, layer: usize) -> View<'_> {
    let layers = match kind {
        Kind::Sky => light.sky(),
        Kind::Block => light.block(),
    };
    match (&layers[layer], kind) {
        (LightNibbles::Uniform(level), _) => View::Uniform(*level),
        (LightNibbles::Data(data), _) => View::Data(data),
        (LightNibbles::Absent, Kind::Block) => View::Uniform(0),
        (LightNibbles::Absent, Kind::Sky) => {
            for above in &layers[layer + 1..] {
                match above {
                    LightNibbles::Absent => {}
                    LightNibbles::Uniform(level) => return View::Uniform(*level),
                    LightNibbles::Data(data) => return View::Row(data),
                }
            }
            View::Uniform(15)
        }
    }
}

/// The layer for `bytes`: without an array when every level is the same.
fn layer_of(bytes: [u8; NIBBLE_BYTES]) -> LightNibbles {
    let first = bytes[0];
    if first & 15 == first >> 4 && bytes.iter().all(|&byte| byte == first) {
        LightNibbles::Uniform(first & 15)
    } else {
        LightNibbles::Data(Arc::new(bytes))
    }
}

/// Chunks whose light layers exist: light is read from and written into them.
fn usable(chunk: &ProtoChunk) -> bool {
    chunk.status >= ChunkStatus::InitializeLight
}

/// The light of the chunks whose light changes by the block changes at `changes`.
pub(crate) fn update(
    has_sky: bool,
    changes: &[BlockPos],
    area: &Neighbours<'_>,
) -> Vec<(ChunkPos, ChunkLight)> {
    let Some(any) = area.iter().next() else {
        return Vec::new();
    };
    let mut update = Update {
        area,
        facts: LightFacts::get(),
        min_y: any.chunk.min_y(),
        height: any.chunk.sections().len() as i32 * 16,
        light: FxHashMap::default(),
        lowest: FxHashMap::default(),
        scratch: Scratch::default(),
    };
    let mut changes: Vec<BlockPos> = changes
        .iter()
        .copied()
        .filter(|pos| {
            (0..update.height).contains(&(pos.y - update.min_y))
                && area.get(pos.chunk()).is_some_and(usable)
        })
        .collect();
    changes.sort_unstable_by_key(|pos| (pos.x, pos.z, pos.y));
    changes.dedup();
    let mut chunks: Vec<ChunkPos> = changes.iter().map(|pos| pos.chunk()).collect();
    chunks.sort_unstable();
    chunks.dedup();

    let mut sky = Vec::new();
    let mut block = Vec::new();
    if has_sky {
        for &pos in &chunks {
            update.sky_columns(pos, &mut sky);
        }
    }
    for &pos in &changes {
        let cell = [pos.x, pos.y - update.min_y, pos.z];
        let emission = update
            .state(cell)
            .map_or(0, |state| LightFacts::light(state).0);
        for (kind, regions, emitted) in [
            (Kind::Sky, &mut sky, 0),
            (Kind::Block, &mut block, emission),
        ] {
            if kind == Kind::Sky && !has_sky {
                continue;
            }
            let mut level = emitted.max(update.stored(kind, cell));
            for dir in Direction::ALL {
                let [dx, dy, dz] = dir.normal();
                level = level.max(update.stored(kind, [cell[0] + dx, cell[1] + dy, cell[2] + dz]));
            }
            if level > 0 {
                let reach = i32::from(level) - 1;
                add_region(
                    regions,
                    Region {
                        min: cell.map(|c| c - reach),
                        max: cell.map(|c| c + reach),
                    },
                );
            }
        }
    }
    for (kind, regions) in [(Kind::Sky, sky), (Kind::Block, block)] {
        for region in regions {
            update.recompute(kind, region);
        }
    }
    if has_sky {
        for &pos in &chunks {
            update.fit_sky_layers(pos);
        }
    }
    update.light.into_iter().collect()
}

#[derive(Default)]
struct Scratch {
    state: Vec<u16>,
    old: Vec<u8>,
    new: Vec<u8>,
    queue: [Vec<u32>; 16],
}

struct Update<'a> {
    area: &'a Neighbours<'a>,
    facts: &'static LightFacts,
    min_y: i32,
    /// Cells of the world in y.
    height: i32,
    /// The new light of the chunks written so far.
    light: FxHashMap<ChunkPos, ChunkLight>,
    /// The lowest sky source of every column of the chunks with changes, from their blocks.
    lowest: FxHashMap<ChunkPos, Box<[u16; 256]>>,
    scratch: Scratch,
}

impl<'a> Update<'a> {
    fn chunk(&self, pos: ChunkPos) -> Option<&'a ProtoChunk> {
        self.area.get(pos).filter(|chunk| usable(chunk))
    }

    fn light_of(&self, chunk: &'a ProtoChunk) -> &ChunkLight {
        self.light
            .get(&chunk.pos)
            .unwrap_or_else(|| chunk.chunk.light())
    }

    fn state(&self, cell: [i32; 3]) -> Option<u16> {
        if !(0..self.height).contains(&cell[1]) {
            return None;
        }
        let chunk = self.chunk(ChunkPos::new(cell[0] >> 4, cell[2] >> 4))?;
        let section = &chunk.chunk.sections()[(cell[1] >> 4) as usize];
        Some(section.block(
            (cell[0] & 15) as u32,
            (cell[1] & 15) as u32,
            (cell[2] & 15) as u32,
        ))
    }

    /// The stored level of a cell, 0 outside the chunks.
    fn stored(&self, kind: Kind, cell: [i32; 3]) -> u8 {
        if !(0..self.height).contains(&cell[1]) {
            return 0;
        }
        let Some(chunk) = self.chunk(ChunkPos::new(cell[0] >> 4, cell[2] >> 4)) else {
            return 0;
        };
        view(self.light_of(chunk), kind, (cell[1] >> 4) as usize + 1).get(
            (cell[0] & 15) as usize,
            (cell[1] & 15) as usize,
            (cell[2] & 15) as usize,
        )
    }

    /// Compares the lowest sky source of every column of a chunk with changes before (from the
    /// stored light) and after (from the blocks), and adds a box around the cells that switch.
    /// All columns: a section that becomes or stops being empty moves sources of other columns.
    fn sky_columns(&mut self, pos: ChunkPos, regions: &mut Vec<Region>) {
        let Some(proto) = self.chunk(pos) else {
            return;
        };
        let chunk = &proto.chunk;
        let sections = chunk.sections().len();
        let light = chunk.light();
        let views: Vec<View<'_>> = (0..sections)
            .map(|section| view(light, Kind::Sky, section + 1))
            .collect();
        let mut lowest = Box::new([0u16; 256]);
        for z in 0..16usize {
            for x in 0..16usize {
                let new = lowest_source(self.facts, chunk, x as u32, z as u32) as i32;
                lowest[z * 16 + x] = new as u16;
                let mut old = self.height;
                'scan: for section in (0..sections).rev() {
                    match &views[section] {
                        View::Uniform(15) => old = section as i32 * 16,
                        View::Uniform(_) => break,
                        layer => {
                            for y in (0..16).rev() {
                                if layer.get(x, y, z) != 15 {
                                    break 'scan;
                                }
                                old = section as i32 * 16 + y as i32;
                            }
                        }
                    }
                }
                if old == new {
                    continue;
                }
                let (wx, wz) = (pos.x * 16 + x as i32, pos.z * 16 + z as i32);
                add_region(
                    regions,
                    Region {
                        min: [wx - 14, old.min(new) - 14, wz - 14],
                        max: [wx + 14, old.max(new) - 1 + 14, wz + 14],
                    },
                );
            }
        }
        self.lowest.insert(pos, lowest);
    }

    /// Resets the cells of a box and propagates their light again from the sources inside it
    /// and the stored levels around it.
    fn recompute(&mut self, kind: Kind, mut region: Region) {
        region.min[1] = region.min[1].max(0);
        region.max[1] = region.max[1].min(self.height - 1);
        if region.min[1] > region.max[1] {
            return;
        }
        // The buffer: the box and one cell around it.
        let origin = region.min.map(|c| c - 1);
        let size = [0, 1, 2].map(|axis| (region.max[axis] - region.min[axis] + 3) as usize);
        let cells = size[0] * size[1] * size[2];
        let index = |x: usize, y: usize, z: usize| (y * size[2] + z) * size[0] + x;
        let inside = |x: usize, y: usize, z: usize| {
            x > 0 && x < size[0] - 1 && y > 0 && y < size[1] - 1 && z > 0 && z < size[2] - 1
        };
        let mut scratch = std::mem::take(&mut self.scratch);
        scratch.state.clear();
        scratch.state.resize(cells, 0);
        scratch.old.clear();
        scratch.old.resize(cells, 0);
        scratch.new.clear();
        scratch.new.resize(cells, FIXED);
        let Scratch {
            state,
            old,
            new,
            queue,
        } = &mut scratch;

        // The part of the buffer in each section of each chunk: (chunk, section, cell ranges).
        let end = [0, 1, 2].map(|axis| origin[axis] + size[axis] as i32 - 1);
        let y_range = origin[1].max(0)..=end[1].min(self.height - 1);
        let mut parts = Vec::new();
        for cz in origin[2] >> 4..=end[2] >> 4 {
            for cx in origin[0] >> 4..=end[0] >> 4 {
                let Some(chunk) = self.chunk(ChunkPos::new(cx, cz)) else {
                    continue;
                };
                for section in *y_range.start() >> 4..=*y_range.end() >> 4 {
                    let low = [cx * 16, section * 16, cz * 16];
                    let from = [0, 1, 2].map(|axis| low[axis].max(origin[axis]));
                    let to = [0, 1, 2].map(|axis| (low[axis] + 15).min(end[axis]));
                    parts.push((chunk, section as usize, low, from, to));
                }
            }
        }

        for &(chunk, section, low, from, to) in &parts {
            let blocks = &chunk.chunk.sections()[section];
            // Every kind of air is the same to light.
            let empty = blocks.summary().is_empty();
            let layer = view(self.light_of(chunk), kind, section + 1);
            let lowest = self.lowest.get(&chunk.pos);
            for wy in from[1]..=to[1] {
                for wz in from[2]..=to[2] {
                    for wx in from[0]..=to[0] {
                        let (lx, ly, lz) = (
                            (wx - low[0]) as usize,
                            (wy - low[1]) as usize,
                            (wz - low[2]) as usize,
                        );
                        let (bx, by, bz) = (
                            (wx - origin[0]) as usize,
                            (wy - origin[1]) as usize,
                            (wz - origin[2]) as usize,
                        );
                        let cell = index(bx, by, bz);
                        let block = if empty {
                            0
                        } else {
                            blocks.block(lx as u32, ly as u32, lz as u32)
                        };
                        let stored = layer.get(lx, ly, lz);
                        state[cell] = block;
                        old[cell] = stored;
                        let level = if !inside(bx, by, bz) {
                            new[cell] = FIXED | stored;
                            if stored > 1 {
                                queue[usize::from(stored)].push(cell as u32);
                            }
                            continue;
                        } else {
                            match kind {
                                Kind::Block => LightFacts::light(block).0,
                                Kind::Sky => {
                                    let source = match lowest {
                                        Some(lowest) => wy >= i32::from(lowest[lz * 16 + lx]),
                                        None => stored == 15,
                                    };
                                    if source { 15 } else { 0 }
                                }
                            }
                        };
                        new[cell] = level;
                        if level > 1 {
                            queue[usize::from(level)].push(cell as u32);
                        }
                    }
                }
            }
        }

        // `propagateIncrease`, highest level first. Fixed cells are never raised: their marked
        // level is above every level, and only cells of the box are not fixed.
        let offsets = Direction::ALL.map(|dir| {
            let [dx, dy, dz] = dir.normal();
            dx as isize + (dz as isize + dy as isize * size[2] as isize) * size[0] as isize
        });
        for level in (2..=15u8).rev() {
            while let Some(cell) = queue[usize::from(level)].pop() {
                let cell = cell as usize;
                let own = new[cell];
                if own & !FIXED != level {
                    continue;
                }
                let from = state[cell];
                for (dir, offset) in Direction::ALL.into_iter().zip(offsets) {
                    // A cell around the box has neighbours outside the buffer.
                    let Some(target) = cell
                        .checked_add_signed(offset)
                        .filter(|&target| own & FIXED == 0 || target < cells)
                    else {
                        continue;
                    };
                    let current = new[target];
                    if level - 1 <= current {
                        continue;
                    }
                    let to = state[target];
                    let reached = level.saturating_sub(LightFacts::opacity(to));
                    if reached <= current || self.facts.occludes(from, to, dir) {
                        continue;
                    }
                    new[target] = reached;
                    if reached > 1 {
                        queue[usize::from(reached)].push(target as u32);
                    }
                }
            }
        }

        for &(chunk, section, low, from, to) in &parts {
            let from = [0, 1, 2].map(|axis| from[axis].max(region.min[axis]));
            let to = [0, 1, 2].map(|axis| to[axis].min(region.max[axis]));
            if (0..3).any(|axis| from[axis] > to[axis]) {
                continue;
            }
            let cell_of = |wx: i32, wy: i32, wz: i32| {
                index(
                    (wx - origin[0]) as usize,
                    (wy - origin[1]) as usize,
                    (wz - origin[2]) as usize,
                )
            };
            let changed = (from[1]..=to[1]).any(|wy| {
                (from[2]..=to[2]).any(|wz| {
                    let row = cell_of(from[0], wy, wz);
                    let len = (to[0] - from[0] + 1) as usize;
                    new[row..row + len] != old[row..row + len]
                })
            });
            if !changed {
                continue;
            }
            let mut bytes = view(self.light_of(chunk), kind, section + 1).to_bytes();
            for wy in from[1]..=to[1] {
                for wz in from[2]..=to[2] {
                    for wx in from[0]..=to[0] {
                        let level = new[cell_of(wx, wy, wz)];
                        let nibble =
                            (((wy - low[1]) << 8) | ((wz - low[2]) << 4) | (wx - low[0])) as usize;
                        let shift = (nibble & 1) * 4;
                        let byte = &mut bytes[nibble >> 1];
                        *byte = (*byte & !(15 << shift)) | (level << shift);
                    }
                }
            }
            let light = self
                .light
                .entry(chunk.pos)
                .or_insert_with(|| chunk.chunk.light().clone());
            let layers = match kind {
                Kind::Sky => light.sky_mut(),
                Kind::Block => light.block_mut(),
            };
            layers[section + 1] = layer_of(bytes);
        }
        self.scratch = scratch;
    }

    /// Sky light layers above the highest section with blocks are absent (full sky light), the
    /// ones at and below it hold data: follows a change of the highest section.
    fn fit_sky_layers(&mut self, pos: ChunkPos) {
        let Some(chunk) = self.chunk(pos) else {
            return;
        };
        let highest = highest_filled_section(&chunk.chunk);
        for section in 0..chunk.chunk.sections().len() {
            let above = highest.is_none_or(|highest| section > highest);
            let current = self.light_of(chunk);
            let absent = current.sky()[section + 1] == LightNibbles::Absent;
            if above == absent {
                continue;
            }
            let layer = if above {
                LightNibbles::Absent
            } else {
                layer_of(view(current, Kind::Sky, section + 1).to_bytes())
            };
            self.light
                .entry(pos)
                .or_insert_with(|| chunk.chunk.light().clone())
                .sky_mut()[section + 1] = layer;
        }
    }
}
