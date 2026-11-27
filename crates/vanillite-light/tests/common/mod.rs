//! Lights chunks of a Vanilla world from their blocks and compares the result with the light
//! Vanilla stored.

#![allow(dead_code)]

use std::fmt;
use std::path::Path;
use std::sync::{Arc, Once};

use vanillite_base::ChunkPos;
use vanillite_chunk::light::LightNibbles;
use vanillite_chunk::slots::{DimensionId, LightEngine, Neighbours, ProtoChunk, WorldInfo};
use vanillite_data::block::BlockState;
use vanillite_light::BfsLightEngine;
use vanillite_storage::{
    AnvilCodec, Compression, IoPool, RegionStore, StoreSettings, WorldFormat, blocking,
};

/// Overworld height range.
pub const MIN_Y: i32 = -64;
pub const SECTIONS: usize = 24;

pub fn install() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        vanillite_chunk::facts::install_vanilla(&[], &[], 1);
    });
}

pub fn world_info(dir: &Path) -> WorldInfo {
    WorldInfo {
        dimension: "minecraft:overworld".to_owned(),
        dir: dir.to_owned(),
        min_y: MIN_Y,
        height: (SECTIONS * 16) as u32,
        seed: 0,
        has_sky_light: true,
        default_biome: 0,
        biomes: Arc::from(Vec::<String>::new()),
        air: 0,
    }
}

/// A differing cell.
#[derive(Debug)]
pub struct Diff {
    /// `s`ky or `b`lock light.
    pub kind: char,
    pub pos: [i32; 3],
    pub ours: u8,
    pub vanilla: u8,
    pub state: u16,
}

impl fmt::Display for Diff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = BlockState(self.state);
        let props: Vec<String> = state
            .properties()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        write!(
            f,
            "{} {} {} {}: ours {} vanilla {} ({}[{}])",
            self.kind,
            self.pos[0],
            self.pos[1],
            self.pos[2],
            self.ours,
            self.vanilla,
            state.block().name(),
            props.join(",")
        )
    }
}

/// Compared and differing cells.
#[derive(Debug, Default)]
pub struct Stats {
    pub chunks: u64,
    pub identical: u64,
    pub sky: u64,
    pub sky_differ: u64,
    pub block: u64,
    pub block_differ: u64,
}

impl Stats {
    pub fn differ(&self) -> u64 {
        self.sky_differ + self.block_differ
    }

    pub fn add(&mut self, other: &Stats) {
        self.chunks += other.chunks;
        self.identical += other.identical;
        self.sky += other.sky;
        self.sky_differ += other.sky_differ;
        self.block += other.block;
        self.block_differ += other.block_differ;
    }
}

impl fmt::Display for Stats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let rate = |n: u64, d: u64| 100.0 - d as f64 * 100.0 / n.max(1) as f64;
        write!(
            f,
            "{} chunks, {} identical; sky {:.5} % equal ({} of {} differ), block {:.5} % equal \
             ({} of {} differ)",
            self.chunks,
            self.identical,
            rate(self.sky, self.sky_differ),
            self.sky_differ,
            self.sky,
            rate(self.block, self.block_differ),
            self.block_differ,
            self.block
        )
    }
}

/// How the compared chunk is lit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// `BfsLightEngine::relight_area` of the center (the reference of light after block changes).
    Relight,
    /// `initialize_light` and `light_chunks` of the whole area (world generation).
    Generation,
}

/// Where chunks come from.
pub struct World {
    load: Box<dyn Fn(ChunkPos) -> Option<ProtoChunk>>,
}

impl World {
    /// The chunks of an Anvil world.
    pub fn open(dir: &Path) -> World {
        install();
        let info = world_info(dir);
        let codec = AnvilCodec::new(&info, Compression::Deflate);
        let pool = IoPool::new(1).expect("IO thread");
        let store = RegionStore::new(
            info.dir.clone(),
            WorldFormat::Anvil,
            StoreSettings::default(),
            pool,
        );
        World {
            load: Box::new(move |pos| {
                blocking::load(&store, &codec, DimensionId(0), &[pos])
                    .pop()
                    .unwrap()
                    .expect("read chunk")
            }),
        }
    }

    /// Chunks from a function.
    pub fn from_fn(load: impl Fn(ChunkPos) -> Option<ProtoChunk> + 'static) -> World {
        install();
        World {
            load: Box::new(load),
        }
    }

    /// Lights `center` from the blocks of its 3 × 3 area (which holds every source that reaches
    /// it) and compares it with the stored light.
    pub fn compare(&self, center: ChunkPos, mode: Mode, mut on_diff: impl FnMut(Diff)) -> Stats {
        let mut area: Vec<Arc<ProtoChunk>> = (-1..=1)
            .flat_map(|dz| (-1..=1).map(move |dx| ChunkPos::new(center.x + dx, center.z + dz)))
            .map(|pos| Arc::new((self.load)(pos).unwrap_or_else(|| panic!("{pos:?} missing"))))
            .collect();
        let vanilla = Arc::clone(&area[4]);
        assert!(vanilla.light_on, "{center:?} has no light");
        let engine = BfsLightEngine::new(&world_info(Path::new("")));
        match mode {
            Mode::Relight => engine.relight_area(center, &mut Neighbours::new(&mut area)),
            Mode::Generation => {
                // Like generation: every chunk gets fresh layers, then each chunk propagates its
                // own sources through its 3 × 3 area.
                for chunk in &mut area {
                    let chunk = Arc::make_mut(chunk);
                    engine.initialize_light(std::slice::from_mut(chunk));
                }
                for i in 0..area.len() {
                    let mut target = [ProtoChunk::clone(&area[i])];
                    let mut others: Vec<Arc<ProtoChunk>> = area
                        .iter()
                        .enumerate()
                        .filter(|&(j, _)| j != i)
                        .map(|(_, c)| Arc::clone(c))
                        .collect();
                    engine.light_chunks(&mut target, &mut Neighbours::new(&mut others));
                    let [target] = target;
                    for other in others {
                        let j = area.iter().position(|c| c.pos == other.pos).unwrap();
                        area[j] = other;
                    }
                    area[i] = Arc::new(target);
                }
            }
        }
        let neighbours = Neighbours::new(&mut area);
        // Vanilla keeps light data for sections within one section of a non-air one.
        let filled = |pos: ChunkPos, section: i32| {
            (0..SECTIONS as i32).contains(&section)
                && !neighbours.get(pos).unwrap().chunk.sections()[section as usize]
                    .summary()
                    .is_empty()
        };
        let storing = |section: i32| {
            (-1..=1).any(|dz| {
                (-1..=1).any(|dx| {
                    (-1..=1)
                        .any(|dy| filled(ChunkPos::new(center.x + dx, center.z + dz), section + dy))
                })
            })
        };
        let ours = neighbours.get(center).unwrap();
        let mut stats = Stats {
            chunks: 1,
            ..Stats::default()
        };
        let top = ours
            .chunk
            .sections()
            .iter()
            .rposition(|s| !s.summary().is_empty());
        for section in 0..SECTIONS {
            if !storing(section as i32) {
                continue;
            }
            let layer = section + 1;
            let blocks = &ours.chunk.sections()[section];
            for (kind, mine, theirs) in [
                (
                    's',
                    &ours.chunk.light().sky()[layer],
                    &vanilla.chunk.light().sky()[layer],
                ),
                (
                    'b',
                    &ours.chunk.light().block()[layer],
                    &vanilla.chunk.light().block()[layer],
                ),
            ] {
                let above_top = top.is_none_or(|t| section > t);
                for y in 0..16 {
                    for z in 0..16 {
                        for x in 0..16 {
                            let a = match mine {
                                LightNibbles::Absent if kind == 's' && above_top => 15,
                                other => other.get(x, y, z),
                            };
                            // Vanilla saves no sky layer for sections that kept no light
                            // data while a neighbour was unloaded; loaded again, the layer
                            // repeats the bottom row of the next layer above (15 above all).
                            let b = if kind == 's' && *theirs == LightNibbles::Absent && a != 0 {
                                vanilla.chunk.light().sky()[layer + 1..]
                                    .iter()
                                    .find(|l| **l != LightNibbles::Absent)
                                    .map_or(15, |l| l.get(x, 0, z))
                            } else {
                                theirs.get(x, y, z)
                            };
                            if kind == 's' {
                                stats.sky += 1;
                            } else {
                                stats.block += 1;
                            }
                            if a == b {
                                continue;
                            }
                            if kind == 's' {
                                stats.sky_differ += 1;
                            } else {
                                stats.block_differ += 1;
                            }
                            on_diff(Diff {
                                kind,
                                pos: [
                                    center.x * 16 + x as i32,
                                    MIN_Y + section as i32 * 16 + y as i32,
                                    center.z * 16 + z as i32,
                                ],
                                ours: a,
                                vanilla: b,
                                state: blocks.block(x, y, z),
                            });
                        }
                    }
                }
            }
        }
        if stats.differ() == 0 {
            stats.identical = 1;
        }
        stats
    }
}
