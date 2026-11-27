//! Light sweep against Vanilla (#237): `cargo xtask light sweep` lets
//! `golden/LightSweep.java` generate, light and change areas in Vanilla; this test lights the
//! same blocks with [`BfsLightEngine`] as the world's chunk pipeline does (`initialize_light`
//! of the area plus two rings, `light_chunks` of the area plus one ring), replays the same block
//! changes batch by batch as the world does (`set_block`, then one `relight` of the changed
//! chunks; unloading drops the light the neighbours only kept because of the chunk and saves it
//! with the storage codec, loading reads it back) and compares
//! the light a client receives: the layers as the light data of a chunk packet sends them, read
//! as the client reads them (a missing sky layer continues the bottom row of the next one above,
//! 15 above all; a missing block layer is dark).
//!
//! `VANILLITE_SWEEP_DIR=<dir with *.bin.gz>` and `VANILLITE_SWEEP_REPORT=<markdown file>`.
//! Each snapshot is compared and then taken over as our light, so every batch is judged on its
//! own from Vanilla's light before it.
//!
//! `VANILLITE_SWEEP_LINES=<n>` lists n differing cells per area (default 60) and
//! `VANILLITE_SWEEP_FILTER=<text>` compares only the files whose name contains the text.
//! `VANILLITE_SWEEP_COLUMN=x,z[,y0,y1]` prints that column of both after every snapshot and the
//! block changes within 16 blocks of it.

mod common;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use vanillite_base::hash::FxHashMap;
use vanillite_base::{BlockPos, ChunkPos};
use vanillite_chunk::light::{ChunkLight, LightNibbles, NIBBLE_BYTES};
use vanillite_chunk::slots::{LightEngine, Neighbours};
use vanillite_chunk::{Chunk, ChunkStatus, PalettedContainer, ProtoChunk, Section, WorldInfo};
use vanillite_data::block::BlockState;
use vanillite_light::BfsLightEngine;
use vanillite_storage::{Codec, Compression};

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn max_lines() -> usize {
    env("VANILLITE_SWEEP_LINES").map_or(60, |n| n.parse().unwrap_or(60))
}

/// Cells listed per snapshot.
const LINES_PER_STAGE: usize = 6;

struct Reader {
    data: Vec<u8>,
    at: usize,
}

impl Reader {
    fn bytes(&mut self, n: usize) -> &[u8] {
        let s = &self.data[self.at..self.at + n];
        self.at += n;
        s
    }
    fn u8(&mut self) -> u8 {
        self.bytes(1)[0]
    }
    fn u16(&mut self) -> u16 {
        u16::from_be_bytes(self.bytes(2).try_into().unwrap())
    }
    fn i32(&mut self) -> i32 {
        i32::from_be_bytes(self.bytes(4).try_into().unwrap())
    }
    fn i64(&mut self) -> i64 {
        i64::from_be_bytes(self.bytes(8).try_into().unwrap())
    }
    fn utf(&mut self) -> String {
        let n = usize::from(self.u16());
        String::from_utf8_lossy(self.bytes(n)).into_owned()
    }
}

/// A light layer as the client receives it: not sent, or 2048 bytes (an "empty" layer is
/// zeros).
type ClientLayer = Option<Box<[u8; NIBBLE_BYTES]>>;

/// Vanilla's light of one chunk: sky then block layers, one per light section.
struct VanillaLight {
    pos: ChunkPos,
    sky: Vec<ClientLayer>,
    block: Vec<ClientLayer>,
}

fn read_snapshot(r: &mut Reader, layers: usize) -> Vec<VanillaLight> {
    let count = r.i32();
    (0..count)
        .map(|_| {
            let pos = ChunkPos::new(r.i32(), r.i32());
            let mut sky = Vec::with_capacity(layers);
            let mut block = Vec::with_capacity(layers);
            for _ in 0..layers {
                for out in [&mut sky, &mut block] {
                    out.push(match r.u8() {
                        0 => None,
                        1 => Some(Box::new([0; NIBBLE_BYTES])),
                        _ => Some(Box::new(r.bytes(NIBBLE_BYTES).try_into().unwrap())),
                    });
                }
            }
            VanillaLight { pos, sky, block }
        })
        .collect()
}

/// Our layers as the light data of a chunk packet sends them (`packet::encode_light_data`).
fn client_layers(layers: &[LightNibbles]) -> Vec<ClientLayer> {
    layers
        .iter()
        .map(|layer| match layer {
            LightNibbles::Absent => None,
            other => Some(Box::new(other.to_bytes())),
        })
        .collect()
}

#[inline]
fn nibble(data: &[u8; NIBBLE_BYTES], x: usize, y: usize, z: usize) -> u8 {
    let index = (y << 8) | (z << 4) | x;
    (data[index >> 1] >> ((index & 1) * 4)) & 15
}

/// The level the client reads at a cell of layer `layer`.
fn client_level(
    layers: &[ClientLayer],
    sky: bool,
    layer: usize,
    x: usize,
    y: usize,
    z: usize,
) -> u8 {
    if let Some(data) = &layers[layer] {
        return nibble(data, x, y, z);
    }
    if !sky {
        return 0;
    }
    layers[layer + 1..]
        .iter()
        .flatten()
        .next()
        .map_or(15, |data| nibble(data, x, 0, z))
}

fn state_name(s: u16) -> String {
    let st = BlockState(s);
    let props: Vec<String> = st.properties().map(|(k, v)| format!("{k}={v}")).collect();
    if props.is_empty() {
        st.block().name().to_owned()
    } else {
        format!("{}[{}]", st.block().name(), props.join(","))
    }
}

/// Differences of one area.
#[derive(Default)]
struct Area {
    chunks: u64,
    cells: u64,
    sky_diffs: u64,
    block_diffs: u64,
    /// Snapshots compared and those with differences, by stage label.
    stages: BTreeMap<String, (u32, u32)>,
    lines: Vec<String>,
}

impl Area {
    fn compare(
        &mut self,
        stage: &str,
        vanilla: &[VanillaLight],
        chunks: &mut FxHashMap<ChunkPos, Arc<ProtoChunk>>,
        info: &WorldInfo,
        batch: usize,
    ) {
        if let Some(column) = env("VANILLITE_SWEEP_COLUMN") {
            print_column(&column, stage, batch, vanilla, chunks, info);
        }
        let entry = self.stages.entry(stage.to_owned()).or_default();
        entry.0 += 1;
        let before = self.sky_diffs + self.block_diffs;
        let mut listed = 0;
        for v in vanilla {
            let ours = chunks.get_mut(&v.pos).expect("compared chunk loaded");
            self.chunks += 1;
            let light = ours.chunk.light();
            let kinds = [
                ('s', client_layers(light.sky()), &v.sky),
                ('b', client_layers(light.block()), &v.block),
            ];
            for (kind, mine, theirs) in kinds {
                if kind == 's' && !info.has_sky_light {
                    continue;
                }
                for layer in 0..mine.len() {
                    for y in 0..16 {
                        for z in 0..16 {
                            for x in 0..16 {
                                self.cells += 1;
                                let a = client_level(&mine, kind == 's', layer, x, y, z);
                                let b = client_level(theirs, kind == 's', layer, x, y, z);
                                if a == b {
                                    continue;
                                }
                                if kind == 's' {
                                    self.sky_diffs += 1;
                                } else {
                                    self.block_diffs += 1;
                                }
                                if self.lines.len() < max_lines() && listed < LINES_PER_STAGE {
                                    listed += 1;
                                    let wy = info.min_y + (layer as i32 - 1) * 16 + y as i32;
                                    let state = ours
                                        .chunk
                                        .block(x as u32, wy, z as u32)
                                        .map_or_else(|| "-".to_owned(), state_name);
                                    self.lines.push(format!(
                                        "{batch} {stage}: {kind} {} {wy} {}: ours {a} vanilla {b} ({state})",
                                        v.pos.x * 16 + x as i32,
                                        v.pos.z * 16 + z as i32,
                                    ));
                                }
                            }
                        }
                    }
                }
            }
            // Take Vanilla's light over.
            let light = Arc::make_mut(ours).chunk.light_mut();
            take_over(light.sky_mut(), &v.sky);
            take_over(light.block_mut(), &v.block);
        }
        if self.sky_diffs + self.block_diffs > before {
            entry.1 += 1;
        }
    }

    fn differs(&self) -> bool {
        self.sky_diffs + self.block_diffs > 0
    }
}

/// Debugging: `VANILLITE_SWEEP_COLUMN=x,z[,y0,y1]` prints that column of both after every
/// snapshot that holds its chunk (sky/block, ours/Vanilla, block state).
fn print_column(
    column: &str,
    stage: &str,
    batch: usize,
    vanilla: &[VanillaLight],
    chunks: &FxHashMap<ChunkPos, Arc<ProtoChunk>>,
    info: &WorldInfo,
) {
    let v: Vec<i32> = column
        .split(',')
        .map(|n| n.trim().parse().unwrap())
        .collect();
    let (x, z) = (v[0], v[1]);
    let (y0, y1) = (
        v.get(2).copied().unwrap_or(info.min_y - 16),
        v.get(3).copied().unwrap_or(info.min_y + 48),
    );
    let pos = ChunkPos::new(x >> 4, z >> 4);
    let none = VanillaLight {
        pos,
        sky: vec![None; info.height as usize / 16 + 2],
        block: vec![None; info.height as usize / 16 + 2],
    };
    let theirs = vanilla.iter().find(|l| l.pos == pos).unwrap_or(&none);
    let Some(ours) = chunks.get(&pos) else {
        return;
    };
    let light = ours.chunk.light();
    let (mine_sky, mine_block) = (client_layers(light.sky()), client_layers(light.block()));
    let shown = vanilla.iter().any(|l| l.pos == pos);
    println!(
        "column {x} {z} after {batch} {stage}{}:",
        if shown {
            ""
        } else {
            " (not in Vanilla's snapshot)"
        }
    );
    for y in (y0..=y1).rev() {
        let layer = ((y - info.min_y + 16) >> 4) as usize;
        let ly = ((y - info.min_y) & 15) as usize;
        let (lx, lz) = ((x & 15) as usize, (z & 15) as usize);
        let state = ours
            .chunk
            .block(lx as u32, y, lz as u32)
            .map_or_else(|| "-".to_owned(), state_name);
        let present = |l: &[ClientLayer]| if l[layer].is_some() { "" } else { "~" };
        println!(
            "  y {y}: sky {}{} / {}{}  block {} / {}  {state}",
            client_level(&mine_sky, true, layer, lx, ly, lz),
            present(&mine_sky),
            client_level(&theirs.sky, true, layer, lx, ly, lz),
            present(&theirs.sky),
            client_level(&mine_block, false, layer, lx, ly, lz),
            client_level(&theirs.block, false, layer, lx, ly, lz),
        );
    }
}

/// Vanilla's layers as ours: an absent layer stays absent, one level is uniform.
fn take_over(layers: &mut [LightNibbles], theirs: &[ClientLayer]) {
    for (layer, theirs) in layers.iter_mut().zip(theirs) {
        *layer = match theirs {
            None => LightNibbles::Absent,
            Some(bytes)
                if bytes.iter().all(|&b| b == bytes[0]) && bytes[0] >> 4 == bytes[0] & 15 =>
            {
                LightNibbles::Uniform(bytes[0] & 15)
            }
            Some(bytes) => LightNibbles::Data(Arc::new(**bytes)),
        };
    }
}

fn world_info(kind: &str, min_y: i32, height: i32, has_sky: bool) -> WorldInfo {
    let dimension = match kind {
        "nether" => "minecraft:the_nether",
        "end" => "minecraft:the_end",
        _ => "minecraft:overworld",
    };
    WorldInfo {
        dimension: dimension.to_owned(),
        dir: PathBuf::new(),
        min_y,
        height: height as u32,
        seed: 0,
        has_sky_light: has_sky,
        default_biome: 0,
        biomes: Arc::from(Vec::<String>::new()),
        air: 0,
    }
}

/// The world's `relight` of the changes of a batch, applied like `Chunks::apply_relit`.
fn relight(
    engine: &BfsLightEngine,
    changes: &[BlockPos],
    chunks: &mut FxHashMap<ChunkPos, Arc<ProtoChunk>>,
) {
    if changes.is_empty() {
        return;
    }
    let mut read: Vec<Arc<ProtoChunk>> = Vec::new();
    for pos in changes {
        let center = pos.chunk();
        for dz in -1..=1 {
            for dx in -1..=1 {
                let at = ChunkPos::new(center.x + dx, center.z + dz);
                if let Some(chunk) = chunks.get(&at)
                    && !read.iter().any(|c| c.pos == at)
                {
                    read.push(Arc::clone(chunk));
                }
            }
        }
    }
    let light: Vec<(ChunkPos, ChunkLight)> = engine.relight(changes, &Neighbours::new(&mut read));
    for (pos, light) in light {
        if let Some(chunk) = chunks.get_mut(&pos) {
            *Arc::make_mut(chunk).chunk.light_mut() = light;
        }
    }
}

/// The light the neighbours of an unloaded chunk keep, as `Chunks::forget_unkept_light`.
fn forget_unkept(
    chunks: &FxHashMap<ChunkPos, Arc<ProtoChunk>>,
    unloaded: &ProtoChunk,
) -> Vec<(ChunkPos, ChunkLight)> {
    let pos = unloaded.pos;
    let mut out = Vec::new();
    for dz in -1..=1 {
        for dx in -1..=1 {
            let center = ChunkPos::new(pos.x + dx, pos.z + dz);
            let Some(chunk) = chunks.get(&center).filter(|_| center != pos) else {
                continue;
            };
            let area: [Option<&Chunk>; 9] = std::array::from_fn(|i| {
                let at = ChunkPos::new(center.x + i as i32 % 3 - 1, center.z + i as i32 / 3 - 1);
                if at == pos {
                    Some(&unloaded.chunk)
                } else {
                    chunks.get(&at).map(|c| &c.chunk)
                }
            });
            let index = ((pos.z - center.z + 1) * 3 + (pos.x - center.x + 1)) as usize;
            let mut light = chunk.chunk.light().clone();
            if vanillite_chunk::light::forget_unkept_light(&mut light, &area, index) {
                out.push((center, light));
            }
        }
    }
    out
}

/// Runs one area; its report row and details.
fn run_area(file: &Path) -> (String, Area) {
    let data = Compression::Gzip
        .decompress(&std::fs::read(file).expect("read"))
        .expect("gunzip");
    let mut r = Reader { data, at: 0 };
    assert_eq!(r.i32(), 0x4C53_5750, "{}: magic", file.display());
    assert_eq!(r.i32(), 1, "{}: format", file.display());
    let kind = r.utf();
    let seed = r.i64();
    let (min_y, height) = (r.i32(), r.i32());
    let has_sky = r.u8() != 0;
    let (x0, z0, size) = (r.i32(), r.i32(), r.i32());
    let label = format!("{kind} seed {seed} chunks {x0},{z0} +{size}");
    let info = world_info(&kind, min_y, height, has_sky);
    let sections = (height / 16) as usize;
    let layers = sections + 2;
    let in_ring = |pos: ChunkPos, ring: i32| {
        pos.x >= x0 - ring
            && pos.x < x0 + size + ring
            && pos.z >= z0 - ring
            && pos.z < z0 + size + ring
    };
    let ring = |ring: i32| -> Vec<ChunkPos> {
        (z0 - ring..z0 + size + ring)
            .flat_map(|z| (x0 - ring..x0 + size + ring).map(move |x| ChunkPos::new(x, z)))
            .collect()
    };

    // Blocks of the area plus two rings.
    let mut chunks: FxHashMap<ChunkPos, Arc<ProtoChunk>> = FxHashMap::default();
    let mut values = vec![0u16; 4096];
    for pos in ring(2) {
        let mut parts = Vec::with_capacity(sections);
        for _ in 0..sections {
            for v in &mut values {
                *v = r.u16();
            }
            parts.push(Arc::new(Section::from_containers(
                PalettedContainer::from_values(&values),
                PalettedContainer::new(0),
            )));
        }
        let mut chunk = ProtoChunk::empty(pos, &info);
        chunk.chunk = Chunk::from_sections(min_y >> 4, parts, ChunkLight::new(sections));
        chunk.status = ChunkStatus::Features;
        chunks.insert(pos, Arc::new(chunk));
    }

    // Generation: `initialize_light` of every chunk, `light_chunks` of the area plus one ring.
    let engine = BfsLightEngine::new(&info);
    for pos in ring(2) {
        let chunk = chunks.get_mut(&pos).unwrap();
        engine.initialize_light(std::slice::from_mut(Arc::make_mut(chunk)));
    }
    for pos in ring(1) {
        let mut target = [ProtoChunk::clone(&chunks[&pos])];
        let mut neighbours: Vec<Arc<ProtoChunk>> = (-1..=1)
            .flat_map(|dz| (-1..=1).map(move |dx| ChunkPos::new(pos.x + dx, pos.z + dz)))
            .filter(|&p| p != pos)
            .filter_map(|p| chunks.get(&p).cloned())
            .collect();
        engine.light_chunks(&mut target, &mut Neighbours::new(&mut neighbours));
        for neighbour in neighbours {
            chunks.insert(neighbour.pos, neighbour);
        }
        let [mut target] = target;
        target.status = ChunkStatus::Full;
        chunks.insert(pos, Arc::new(target));
    }

    let mut area = Area::default();
    let vanilla = read_snapshot(&mut r, layers);
    area.compare("generation", &vanilla, &mut chunks, &info, 0);

    // Block changes.
    let codec = Codec::new(&info);
    let mut saved = FxHashMap::default();
    let batches = r.i32();
    for batch in 1..=batches as usize {
        let stage = r.utf();
        let ops = r.i32();
        let mut changes = Vec::new();
        for _ in 0..ops {
            match r.u8() {
                0 => {
                    let pos = BlockPos::new(r.i32(), r.i32(), r.i32());
                    let state = r.i32() as u16;
                    if let Some(column) = env("VANILLITE_SWEEP_COLUMN") {
                        let v: Vec<i32> = column
                            .split(',')
                            .map(|n| n.trim().parse().unwrap())
                            .collect();
                        if (pos.x - v[0]).abs() <= 16 && (pos.z - v[1]).abs() <= 16 {
                            println!(
                                "batch {batch} {stage}: set {} {} {} {}",
                                pos.x,
                                pos.y,
                                pos.z,
                                state_name(state)
                            );
                        }
                    }
                    let Some(chunk) = chunks.get_mut(&pos.chunk()) else {
                        continue;
                    };
                    let old = Arc::make_mut(chunk).chunk.set_block(
                        (pos.x & 15) as u32,
                        pos.y,
                        (pos.z & 15) as u32,
                        state,
                    );
                    if let Some(old) = old
                        && engine.changes_light(old, state)
                    {
                        changes.push(pos);
                    }
                }
                op => {
                    let pos = ChunkPos::new(r.i32(), r.i32());
                    if op == 1 {
                        let chunk = chunks.remove(&pos).expect("unloaded chunk loaded");
                        forget_unkept(&chunks, &chunk)
                            .into_iter()
                            .for_each(|(at, light)| {
                                *Arc::make_mut(chunks.get_mut(&at).unwrap())
                                    .chunk
                                    .light_mut() = light;
                            });
                        saved.insert(pos, codec.write(&chunk).expect("save chunk"));
                    } else {
                        let root = saved.remove(&pos).expect("loaded chunk saved");
                        chunks.insert(pos, Arc::new(codec.read(pos, root).expect("load chunk")));
                    }
                }
            }
        }
        // A tick's changes are relit at its end; chunks outside the area are not changed.
        changes.retain(|pos| in_ring(pos.chunk(), 0));
        relight(&engine, &changes, &mut chunks);
        let vanilla = read_snapshot(&mut r, layers);
        area.compare(&stage, &vanilla, &mut chunks, &info, batch);
    }
    (label, area)
}

#[test]
#[ignore = "needs Vanilla sweep files (cargo xtask light sweep)"]
fn light_sweep_matches_vanilla() {
    let Some(dir) = env("VANILLITE_SWEEP_DIR").map(PathBuf::from) else {
        return;
    };
    common::install();
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("sweep dir")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().ends_with(".bin.gz"))
        .filter(|p| env("VANILLITE_SWEEP_FILTER").is_none_or(|f| p.to_string_lossy().contains(&f)))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no *.bin.gz in {}", dir.display());
    let next = AtomicUsize::new(0);
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(files.len());
    let mut results: Vec<(usize, String, Area)> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut out = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(file) = files.get(i) else { break };
                        let (label, area) = run_area(file);
                        out.push((i, label, area));
                    }
                    out
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("area thread"))
            .collect()
    });
    results.sort_by_key(|r| r.0);

    let mut report = String::from(
        "| area | chunk snapshots | cells | sky differ | block differ | stages with differences |\n|---|---|---|---|---|---|\n",
    );
    let mut details = String::new();
    let mut failed = 0;
    for (_, label, area) in &results {
        let stages: Vec<String> = area
            .stages
            .iter()
            .filter(|(_, (_, bad))| *bad > 0)
            .map(|(name, (all, bad))| format!("{name} {bad}/{all}"))
            .collect();
        let _ = writeln!(
            report,
            "| {label} | {} | {} | {} | {} | {} |",
            area.chunks,
            area.cells,
            area.sky_diffs,
            area.block_diffs,
            if stages.is_empty() {
                "-".to_owned()
            } else {
                stages.join(", ")
            }
        );
        if area.differs() {
            failed += 1;
            let _ = writeln!(details, "\n### {label}\n");
            for line in &area.lines {
                let _ = writeln!(details, "- {line}");
            }
        }
    }
    let summary = format!(
        "# Light sweep against Vanilla\n\n{} areas, {failed} with differences.\n\n{report}{}",
        results.len(),
        if details.is_empty() {
            String::new()
        } else {
            format!("\n## Differences\n{details}")
        }
    );
    println!("{summary}");
    if let Some(path) = env("VANILLITE_SWEEP_REPORT") {
        std::fs::write(&path, &summary).expect("write report");
    }
    assert_eq!(failed, 0, "{failed} areas differ from Vanilla");
}
