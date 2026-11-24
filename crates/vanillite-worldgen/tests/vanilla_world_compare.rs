//! Compares Vanillite's generation of an area with a world a Vanilla server generated from the
//! same seed (ignored by default; it needs that world).
//!
//! `VANILLITE_COMPARE_WORLD=<world>/dimensions/minecraft/overworld` (the folder with `region/`),
//! `VANILLITE_COMPARE_SEED=<seed>`, `VANILLITE_COMPARE_AREA=x0,z0,x1,z1` (chunk coordinates,
//! inclusive; every chunk of the area and one ring around it must be full in the Vanilla world),
//! `VANILLITE_COMPARE_VERBOSE=<n>` (print the first n differing blocks of each chunk).
//! Structures are generated unless `VANILLITE_COMPARE_STRUCTURES=0`; then chunks within one
//! chunk of a structure start or reference in the Vanilla world are skipped. Run with
//! `cargo test --release -p vanillite-worldgen --test vanilla_world_compare -- --ignored --nocapture`.

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use vanillite_base::ChunkPos;
use vanillite_chunk::slots::{self, ChunkGenerator, ChunkStatus, Neighbours, WorldInfo};
use vanillite_data::block::BlockState;
use vanillite_nbt::{Budget, Tag};
use vanillite_storage::region::RegionFile;
use vanillite_storage::{AnvilCodec, Compression};
use vanillite_worldgen::{DimensionSpec, TerrainGenerator, VanillaChunkGenerator};

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// Whether the stored Vanilla chunk has structure starts or references.
fn has_structures(dir: &std::path::Path, pos: ChunkPos) -> bool {
    let (rx, rz) = pos.region();
    let path = dir.join(format!("region/r.{rx}.{rz}.mca"));
    let Ok(Some(mut region)) = RegionFile::open(&path, false) else {
        return false;
    };
    let Ok(Some((compression, data))) = region.read(pos) else {
        return false;
    };
    let bytes = compression.decompress(&data).expect("decompress");
    let root =
        vanillite_nbt::read::read_named(&mut bytes.as_slice(), &mut Budget::file()).expect("nbt");
    let Some(structures) = root.get("structures").and_then(Tag::as_compound) else {
        return false;
    };
    ["starts", "References"].iter().any(|key| {
        structures
            .get(key)
            .and_then(Tag::as_compound)
            .is_some_and(|c| !c.is_empty())
    })
}

#[test]
#[ignore = "needs a Vanilla world (VANILLITE_COMPARE_WORLD)"]
fn compare_with_vanilla_world() {
    let Some(dir) = env("VANILLITE_COMPARE_WORLD").map(PathBuf::from) else {
        return;
    };
    let seed: i64 = env("VANILLITE_COMPARE_SEED")
        .expect("VANILLITE_COMPARE_SEED")
        .parse()
        .unwrap();
    let area: Vec<i32> = env("VANILLITE_COMPARE_AREA")
        .unwrap_or_else(|| "-8,-8,7,7".to_owned())
        .split(',')
        .map(|v| v.trim().parse().unwrap())
        .collect();
    let (x0, z0, x1, z1) = (area[0], area[1], area[2], area[3]);

    let packs = common::packs();
    let source: serde_json::Value = serde_json::json!({
        "type": "minecraft:multi_noise",
        "preset": "minecraft:overworld"
    });
    let (min_y, height) = (-64, 384);
    let spec = DimensionSpec {
        noise_settings: "minecraft:overworld",
        biome_source: &source,
        min_y,
        height,
        has_sky_light: true,
    };
    let terrain = TerrainGenerator::new(packs, spec, seed).expect("generator");
    let biomes: Arc<[String]> = terrain
        .biomes()
        .all()
        .iter()
        .map(|b| b.name.clone())
        .collect();
    let world = WorldInfo {
        dimension: "minecraft:overworld".to_owned(),
        dir: dir.clone(),
        min_y,
        height: height as u32,
        seed,
        has_sky_light: true,
        default_biome: terrain.biomes().id_of("minecraft:plains").unwrap(),
        biomes,
        air: 0,
    };
    let structures = env("VANILLITE_COMPARE_STRUCTURES").as_deref() != Some("0");
    let generator = VanillaChunkGenerator::new(terrain, world.clone()).with_structures(structures);

    // Generated area: features for the compared area plus one ring, terrain one more,
    // references for those, and structure starts and biomes 8 chunks further out.
    const MARGIN: i32 = 11;
    let (gx0, gz0, gx1, gz1) = (x0 - MARGIN, z0 - MARGIN, x1 + MARGIN, z1 + MARGIN);
    let width = (gx1 - gx0 + 1) as usize;
    let index = |p: ChunkPos| (p.z - gz0) as usize * width + (p.x - gx0) as usize;
    let mut chunks: Vec<Arc<slots::ProtoChunk>> = (gz0..=gz1)
        .flat_map(|z| (gx0..=gx1).map(move |x| ChunkPos::new(x, z)))
        .map(|p| Arc::new(generator.new_chunk(p)))
        .collect();
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());

    let step = |chunks: &mut Vec<Arc<slots::ProtoChunk>>, status: ChunkStatus, ring: i32| {
        let todo: Vec<ChunkPos> = chunks
            .iter()
            .map(|c| c.pos)
            .filter(|p| {
                p.x >= gx0 + ring && p.x <= gx1 - ring && p.z >= gz0 + ring && p.z <= gz1 - ring
            })
            .collect();
        let snapshot: Vec<Arc<slots::ProtoChunk>> = chunks.clone();
        let results: Vec<slots::ProtoChunk> = std::thread::scope(|s| {
            let handles: Vec<_> = todo
                .chunks(todo.len().div_ceil(threads))
                .map(|part| {
                    let snapshot = &snapshot;
                    let generator = &generator;
                    s.spawn(move || {
                        part.iter()
                            .map(|&p| {
                                let mut target = [slots::ProtoChunk::clone(&snapshot[index(p)])];
                                let mut neighbours: Vec<Arc<slots::ProtoChunk>> = (-8..=8)
                                    .flat_map(|dz| (-8..=8).map(move |dx| (dx, dz)))
                                    .filter(|&(dx, dz)| {
                                        (dx, dz) != (0, 0)
                                            && (gx0..=gx1).contains(&(p.x + dx))
                                            && (gz0..=gz1).contains(&(p.z + dz))
                                    })
                                    .map(|(dx, dz)| {
                                        Arc::clone(
                                            &snapshot[index(ChunkPos::new(p.x + dx, p.z + dz))],
                                        )
                                    })
                                    .collect();
                                generator.generate(
                                    status,
                                    &mut target,
                                    &mut Neighbours::new(&mut neighbours),
                                );
                                let [mut t] = target;
                                t.status = status;
                                t
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|h| h.join().unwrap())
                .collect()
        });
        for chunk in results {
            let i = index(chunk.pos);
            chunks[i] = Arc::new(chunk);
        }
    };
    let generate = env("VANILLITE_COMPARE_OTHER").is_none();
    if generate {
        step(&mut chunks, ChunkStatus::StructureStarts, 0);
        step(&mut chunks, ChunkStatus::StructureReferences, 8);
        step(&mut chunks, ChunkStatus::Biomes, 8);
        step(&mut chunks, ChunkStatus::Terrain, 9);
        // Features one chunk at a time (they write into the 3×3 area), row by row.
        for z in gz0 + MARGIN - 1..=gz1 - MARGIN + 1 {
            for x in gx0 + MARGIN - 1..=gx1 - MARGIN + 1 {
                let p = ChunkPos::new(x, z);
                let mut target = [slots::ProtoChunk::clone(&chunks[index(p)])];
                let mut neighbours: Vec<Arc<slots::ProtoChunk>> = (-8..=8)
                    .flat_map(|dz| (-8..=8).map(move |dx| (dx, dz)))
                    .filter(|&d| d != (0, 0))
                    .map(|(dx, dz)| Arc::clone(&chunks[index(ChunkPos::new(x + dx, z + dz))]))
                    .collect();
                generator.generate(
                    ChunkStatus::Features,
                    &mut target,
                    &mut Neighbours::new(&mut neighbours),
                );
                let [t] = target;
                chunks[index(p)] = Arc::new(t);
                for n in neighbours {
                    let i = index(n.pos);
                    chunks[i] = n;
                }
            }
        }
    }

    let storage = (
        world.dir.clone(),
        AnvilCodec::new(&world, Compression::Deflate),
    );
    // A second Vanilla world instead of Vanillite's generation (how much two Vanilla runs differ).
    let other = env("VANILLITE_COMPARE_OTHER").map(|dir| {
        let info = WorldInfo {
            dir: PathBuf::from(dir),
            ..world.clone()
        };
        (
            info.dir.clone(),
            AnvilCodec::new(&info, Compression::Deflate),
        )
    });
    let load = |(dir, codec): &(PathBuf, AnvilCodec), pos: ChunkPos| -> Option<slots::ProtoChunk> {
        use vanillite_chunk::slots::{ChunkCodec, RecordMeta};
        let (rx, rz) = pos.region();
        let path = dir.join(format!("region/r.{rx}.{rz}.mca"));
        let region = RegionFile::open(&path, false).expect("open")?;
        let record = region.read_record(pos).expect("read")?;
        Some(
            codec
                .decode(pos, &RecordMeta::default(), &record)
                .expect("decode"),
        )
    };
    let mut pairs: BTreeMap<(String, String), u64> = BTreeMap::new();
    let (mut total, mut equal, mut compared, mut identical, mut skipped) = (0u64, 0u64, 0, 0, 0);
    let mut worst: Vec<(u64, ChunkPos)> = Vec::new();
    for z in z0..=z1 {
        for x in x0..=x1 {
            let pos = ChunkPos::new(x, z);
            if !structures
                && (-1..=1).any(|dz| {
                    (-1..=1).any(|dx| has_structures(&dir, ChunkPos::new(x + dx, z + dz)))
                })
            {
                skipped += 1;
                continue;
            }
            let vanilla = load(&storage, pos).expect("chunk in Vanilla world");
            assert_eq!(
                vanilla.status,
                ChunkStatus::Full,
                "{pos:?} not full in Vanilla"
            );
            let loaded;
            let ours = match &other {
                Some(other) => {
                    loaded = load(other, pos).expect("chunk in other world");
                    &loaded
                }
                None => &*chunks[index(pos)],
            };
            let mut diff = 0u64;
            for (si, (a, b)) in ours
                .chunk
                .sections()
                .iter()
                .zip(vanilla.chunk.sections())
                .enumerate()
            {
                for y in 0..16 {
                    for lz in 0..16 {
                        for lx in 0..16 {
                            let (sa, sb) = (a.block(lx, y, lz), b.block(lx, y, lz));
                            total += 1;
                            if sa == sb {
                                equal += 1;
                                continue;
                            }
                            diff += 1;
                            let name = |s: u16| {
                                let st = BlockState(s);
                                let props: Vec<String> =
                                    st.properties().map(|(k, v)| format!("{k}={v}")).collect();
                                format!("{}[{}]", st.block().name(), props.join(","))
                            };
                            *pairs.entry((name(sa), name(sb))).or_default() += 1;
                            if env("VANILLITE_COMPARE_VERBOSE")
                                .is_some_and(|n| diff <= n.parse().unwrap_or(3))
                            {
                                println!(
                                    "{pos:?} {} {} {}: ours {} vanilla {}",
                                    x * 16 + lx as i32,
                                    min_y + si as i32 * 16 + y as i32,
                                    z * 16 + lz as i32,
                                    name(sa),
                                    name(sb)
                                );
                            }
                        }
                    }
                }
            }
            compared += 1;
            if diff == 0 {
                identical += 1;
            } else {
                worst.push((diff, pos));
            }
        }
    }
    worst.sort_by(|a, b| b.cmp(a));
    println!(
        "compared {compared} chunks ({skipped} skipped near structures): {identical} identical, \
         {:.4} % of blocks equal ({} differ)",
        equal as f64 * 100.0 / total.max(1) as f64,
        total - equal
    );
    println!("most different chunks: {:?}", &worst[..worst.len().min(15)]);
    let mut pairs: Vec<_> = pairs.into_iter().collect();
    pairs.sort_by_key(|a| std::cmp::Reverse(a.1));
    for ((ours, vanilla), n) in pairs.iter().take(
        env("VANILLITE_COMPARE_PAIRS").map_or(40, |n| n.parse().expect("VANILLITE_COMPARE_PAIRS")),
    ) {
        println!("{n:8}  ours {ours}  vanilla {vanilla}");
    }
}
