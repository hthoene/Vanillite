//! Jigsaw structure starts (`JigsawPlacement`) against the Vanilla goldens of
//! `structure_starts.json`: for every chunk of the golden worlds, the starts of jigsaw
//! structures must match Vanilla's piece by piece (pool element, position, rotation, box,
//! ground level delta and junctions).
//! The decoration areas of `structure_decoration.json` centred on jigsaw structures must match
//! Vanilla block for block.

mod common;

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use serde_json::{Value, json};
use vanillite_base::ChunkPos;
use vanillite_base::math::java_string_hash;
use vanillite_chunk::HeightmapKind;
use vanillite_nbt::Tag;
use vanillite_worldgen::proto::ProtoTick;
use vanillite_worldgen::structure::StructurePiece;
use vanillite_worldgen::structure::pool::{PoolElementStructurePiece, Projection};
use vanillite_worldgen::structure::structure::StructureKind;
use vanillite_worldgen::template::{BoundingBox, Rotation};
use vanillite_worldgen::{
    DimensionSpec, NeighbourBiomes, NeighbourStarts, ProtoChunk, ReferencedStarts,
    TerrainGenerator, TerrainJob,
};

fn generator(dimension: &str, seed: i64) -> TerrainGenerator {
    let (biome_source, min_y, height) = match dimension {
        "minecraft:overworld" => (
            json!({"type": "minecraft:multi_noise", "preset": "minecraft:overworld"}),
            -64,
            384,
        ),
        "minecraft:nether" => (
            json!({"type": "minecraft:multi_noise", "preset": "minecraft:nether"}),
            0,
            256,
        ),
        _ => (json!({"type": "minecraft:the_end"}), 0, 256),
    };
    let spec = DimensionSpec {
        noise_settings: dimension,
        biome_source: &biome_source,
        min_y,
        height,
        has_sky_light: dimension == "minecraft:overworld",
    };
    TerrainGenerator::new(common::packs(), spec, seed).expect("generator")
}

fn bb_json(bb: &BoundingBox) -> Value {
    json!([
        bb.min_x(),
        bb.min_y(),
        bb.min_z(),
        bb.max_x(),
        bb.max_y(),
        bb.max_z()
    ])
}

fn rotation_name(rotation: Rotation) -> &'static str {
    match rotation {
        Rotation::None => "NONE",
        Rotation::Clockwise90 => "CLOCKWISE_90",
        Rotation::Clockwise180 => "CLOCKWISE_180",
        Rotation::Counterclockwise90 => "COUNTERCLOCKWISE_90",
    }
}

fn projection_name(projection: Projection) -> &'static str {
    match projection {
        Projection::Rigid => "rigid",
        Projection::TerrainMatching => "terrain_matching",
    }
}

/// A jigsaw piece in the golden's form (the parts the port reproduces).
fn piece_json(piece: &dyn StructurePiece) -> Value {
    let Some(pool) = <dyn StructurePiece>::downcast_ref::<PoolElementStructurePiece>(piece) else {
        return json!(["not a jigsaw piece", piece.base().kind]);
    };
    let junctions: Vec<Value> = pool
        .junctions
        .iter()
        .map(|j| {
            json!({
                "delta_y": j.delta_y,
                "dest_proj": projection_name(j.dest_projection),
                "source_ground_y": j.source_ground_y,
                "source_x": j.source_x,
                "source_z": j.source_z,
            })
        })
        .collect();
    let element = &pool.element;
    json!([
        bb_json(piece.bounding_box()),
        rotation_name(pool.rotation),
        pool.position.x,
        pool.position.y,
        pool.position.z,
        pool.ground_level_delta,
        junctions,
        element.element_type(),
        element.location(),
        projection_name(element.projection()),
    ])
}

/// The same parts of a golden piece.
fn golden_piece(piece: &Value) -> Value {
    let data = &piece[6];
    let element = &data["pool_element"];
    json!([
        piece[1],
        piece[4],
        data["PosX"],
        data["PosY"],
        data["PosZ"],
        data["ground_level_delta"],
        data["junctions"],
        element["element_type"],
        element.get("location"),
        element["projection"],
    ])
}

/// Whether two structures (registry names) are in one structure set.
fn shares_set(generator: &TerrainGenerator, a: &str, b: &str) -> bool {
    let registry = generator.structures();
    let (Some(a), Some(b)) = (registry.structure_id(a), registry.structure_id(b)) else {
        return false;
    };
    a != b
        && registry.sets().iter().any(|set| {
            set.structures.iter().any(|(id, _)| *id == a)
                && set.structures.iter().any(|(id, _)| *id == b)
        })
}

#[derive(Debug)]
struct Start {
    structure: String,
    bb: Value,
    pieces: Vec<Value>,
}

#[test]
fn jigsaw_starts_match_vanilla() {
    let golden = common::golden("structure_starts");
    let worlds = golden["worlds"].as_array().expect("worlds");
    // Golden jigsaw starts per world and chunk, by structure name.
    let mut expected: HashMap<(usize, ChunkPos), BTreeMap<String, Start>> = HashMap::new();
    // Every golden start's structure per world and chunk (any type).
    let mut golden_structures: HashMap<(usize, ChunkPos), Vec<String>> = HashMap::new();
    for start in golden["starts"].as_array().expect("starts") {
        golden_structures
            .entry((
                start["w"].as_u64().expect("world") as usize,
                ChunkPos::new(
                    start["chunk"][0].as_i64().expect("x") as i32,
                    start["chunk"][1].as_i64().expect("z") as i32,
                ),
            ))
            .or_default()
            .push(start["structure"].as_str().expect("structure").to_owned());
    }
    let mut displaced = Vec::new();
    for start in golden["starts"].as_array().expect("starts") {
        let pieces = start["pieces"].as_array().expect("pieces");
        if pieces.first().is_none_or(|p| p[0] != "minecraft:jigsaw") {
            continue;
        }
        let w = start["w"].as_u64().expect("world") as usize;
        let chunk = ChunkPos::new(
            start["chunk"][0].as_i64().expect("x") as i32,
            start["chunk"][1].as_i64().expect("z") as i32,
        );
        let structure = start["structure"].as_str().expect("structure").to_owned();
        expected.entry((w, chunk)).or_default().insert(
            structure.clone(),
            Start {
                structure,
                bb: start["bb"].clone(),
                pieces: pieces.iter().map(golden_piece).collect(),
            },
        );
    }
    let mut matched = 0;
    let mut failures = Vec::new();
    let mut per_structure: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for (w, world) in worlds.iter().enumerate() {
        let dimension = world["dimension"].as_str().expect("dimension");
        let generator = generator(dimension, world["seed"].as_i64().expect("seed"));
        let area: Vec<i32> = world["area"]
            .as_array()
            .expect("area")
            .iter()
            .map(|v| v.as_i64().expect("int") as i32)
            .collect();
        let mut chunks = Vec::new();
        for z in area[1]..area[1] + area[3] {
            for x in area[0]..area[0] + area[2] {
                chunks.push(ChunkPos::new(x, z));
            }
        }
        for chunk in world["chunks"].as_array().expect("chunks") {
            chunks.push(ChunkPos::new(
                chunk[0].as_i64().expect("x") as i32,
                chunk[1].as_i64().expect("z") as i32,
            ));
        }
        let actual: Mutex<Vec<(ChunkPos, BTreeMap<String, Start>)>> = Mutex::new(Vec::new());
        let next = std::sync::atomic::AtomicUsize::new(0);
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(16));
        std::thread::scope(|scope| {
            for _ in 0..threads {
                scope.spawn(|| {
                    loop {
                        let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(&pos) = chunks.get(index) else {
                            break;
                        };
                        let mut batch = [generator.new_chunk(pos)];
                        generator.generate_structure_starts(&mut batch);
                        let mut starts = BTreeMap::new();
                        for (id, start) in batch[0].structure_starts() {
                            let structure = generator.structures().structure(*id);
                            if !matches!(structure.kind, StructureKind::Jigsaw(_)) {
                                continue;
                            }
                            let pieces = start.pieces();
                            starts.insert(
                                structure.name.clone(),
                                Start {
                                    structure: structure.name.clone(),
                                    bb: bb_json(&start.bounding_box()),
                                    pieces: pieces.iter().map(|p| piece_json(&**p)).collect(),
                                },
                            );
                        }
                        if !starts.is_empty() {
                            actual.lock().unwrap().push((pos, starts));
                        }
                    }
                });
            }
        });
        let mut actual: HashMap<ChunkPos, BTreeMap<String, Start>> =
            actual.into_inner().unwrap().into_iter().collect();
        let mut keys: Vec<ChunkPos> = expected
            .keys()
            .filter(|(ew, _)| *ew == w)
            .map(|(_, c)| *c)
            .chain(actual.keys().copied())
            .collect();
        keys.sort_by_key(|c| (c.z, c.x));
        keys.dedup();
        for chunk in keys {
            let empty = BTreeMap::new();
            let want = expected.get(&(w, chunk)).unwrap_or(&empty);
            let got = actual.remove(&chunk).unwrap_or_default();
            let names: std::collections::BTreeSet<&String> =
                want.keys().chain(got.keys()).collect();
            for name in names {
                let counts = per_structure.entry(name.clone()).or_default();
                counts.1 += 1;
                match (want.get(name), got.get(name)) {
                    (Some(want), Some(got)) => {
                        let first_diff = want
                            .pieces
                            .iter()
                            .zip(&got.pieces)
                            .position(|(a, b)| a != b);
                        if want.bb == got.bb
                            && want.pieces.len() == got.pieces.len()
                            && first_diff.is_none()
                        {
                            matched += 1;
                            counts.0 += 1;
                        } else {
                            let detail = match first_diff {
                                Some(i) => format!(
                                    "piece {i}:\n  want {}\n  got  {}",
                                    want.pieces[i], got.pieces[i]
                                ),
                                None => format!(
                                    "bb want {} got {}, pieces want {} got {}",
                                    want.bb,
                                    got.bb,
                                    want.pieces.len(),
                                    got.pieces.len()
                                ),
                            };
                            failures.push(format!(
                                "world {w} {} at {chunk:?}: {detail}",
                                want.structure
                            ));
                        }
                    }
                    (Some(want), None) => failures.push(format!(
                        "world {w} {} at {chunk:?}: missing",
                        want.structure
                    )),
                    (None, Some(got)) => {
                        // A structure of the same set that Vanilla generated here but the port
                        // does not generate yet lets the set try this one instead.
                        let shares_set = golden_structures.get(&(w, chunk)).is_some_and(|names| {
                            names
                                .iter()
                                .any(|other| shares_set(&generator, other, &got.structure))
                        });
                        if shares_set {
                            counts.1 -= 1;
                            displaced.push(format!("world {w} {} at {chunk:?}", got.structure));
                        } else {
                            failures.push(format!(
                                "world {w} {} at {chunk:?}: unexpected start",
                                got.structure
                            ));
                        }
                    }
                    (None, None) => unreachable!(),
                }
            }
        }
    }
    let total: usize = per_structure.values().map(|c| c.1).sum();
    for (name, (ok, all)) in &per_structure {
        eprintln!("{name}: {ok}/{all}");
    }
    eprintln!("jigsaw starts matching Vanilla: {matched}/{total}");
    if !displaced.is_empty() {
        eprintln!(
            "not compared (Vanilla generated another structure of the set there, which is not \
             ported): {}",
            displaced.join(", ")
        );
    }
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// Decoration areas whose sections near jigsaw pieces differ only because of a structure that is
/// not ported yet (checked block by block against a Vanilla dump of the area): there the
/// differences are reported, not failed. Remove an entry once its area matches.
const KNOWN_FOREIGN_DIFFERENCES: [(&str, &str); 2] = [
    (
        "minecraft:trial_chambers",
        "the mineshaft of chunk [-19, -22] crosses the chambers at y 8 to 29",
    ),
    (
        "minecraft:abandoned_camp_birch_forest",
        "the corridors of the mineshaft of chunk [-53, 95] change the air exposure draws of \
         ore_coal_lower, which moves coal blobs below the camp",
    ),
];

/// The recorded heightmaps, in golden order.
const HEIGHTMAPS: [HeightmapKind; 4] = [
    HeightmapKind::WorldSurface,
    HeightmapKind::OceanFloor,
    HeightmapKind::MotionBlocking,
    HeightmapKind::MotionBlockingNoLeaves,
];

fn int(v: &Value) -> i64 {
    v.as_i64().expect("integer in golden")
}

fn list(v: &Value) -> &[Value] {
    v.as_array().map_or(&[], Vec::as_slice)
}

/// What `structure_decoration.json` records of one chunk (as in `feature_goldens.rs`): block
/// hashes per section, heightmap hashes, post-processing marks, block entity positions and
/// types, tick counts and hashes.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Snapshot {
    sections: Vec<i64>,
    heightmaps: [i64; 4],
    post_processing: Vec<(i64, i64)>,
    block_entities: Vec<(i32, i32, i32, String)>,
    block_ticks: (i64, i64),
    fluid_ticks: (i64, i64),
}

impl Snapshot {
    fn of(chunk: &ProtoChunk) -> Self {
        let sections = chunk
            .sections()
            .iter()
            .map(|section| {
                let mut hash = 0_i64;
                for y in 0..16 {
                    for z in 0..16 {
                        for x in 0..16 {
                            hash = hash
                                .wrapping_mul(31)
                                .wrapping_add(i64::from(section.block(x, y, z)));
                        }
                    }
                }
                hash
            })
            .collect();
        let heightmaps = HEIGHTMAPS.map(|kind| {
            let mut hash = 0_i64;
            for z in 0..16 {
                for x in 0..16 {
                    hash = hash
                        .wrapping_mul(31)
                        .wrapping_add(i64::from(chunk.heightmap(kind, x, z)));
                }
            }
            hash
        });
        let mut post_processing = vec![(0_i64, 0_i64); chunk.sections().len()];
        for &(x, y, z) in chunk.post_processing() {
            let packed = ((x & 15) | ((y & 15) << 4) | ((z & 15) << 8)) as i16;
            let entry = &mut post_processing[chunk.section_index(y)];
            entry.0 += 1;
            entry.1 = entry.1.wrapping_mul(31).wrapping_add(i64::from(packed));
        }
        let mut block_entities: Vec<(i32, i32, i32, String)> = chunk
            .block_entities()
            .iter()
            .map(|(pos, tag)| {
                let id = match tag.get("id") {
                    Some(Tag::String(id)) => id.clone(),
                    _ => String::new(),
                };
                (pos.x, pos.y, pos.z, id)
            })
            .collect();
        block_entities.sort_by_key(|&(x, y, z, _)| (y, z, x));
        Self {
            sections,
            heightmaps,
            post_processing,
            block_entities,
            block_ticks: tick_hash(chunk.block_ticks().ticks(), |b| b.name()),
            fluid_ticks: tick_hash(chunk.fluid_ticks().ticks(), |f| {
                vanillite_data::registry::FLUID.name(f).unwrap_or("")
            }),
        }
    }

    fn from_golden(chunk: &Value) -> Self {
        let pair = |v: &Value| (int(&v[0]), int(&v[1]));
        Self {
            sections: list(&chunk["sections"]).iter().map(int).collect(),
            heightmaps: std::array::from_fn(|i| int(&chunk["heightmaps"][i])),
            post_processing: list(&chunk["post_processing"]).iter().map(pair).collect(),
            block_entities: list(&chunk["block_entities"])
                .iter()
                .map(|v| {
                    (
                        int(&v[0]) as i32,
                        int(&v[1]) as i32,
                        int(&v[2]) as i32,
                        v[3].as_str().unwrap_or_default().to_owned(),
                    )
                })
                .collect(),
            block_ticks: chunk.get("block_ticks").map_or((0, 0), pair),
            fluid_ticks: chunk.get("fluid_ticks").map_or((0, 0), pair),
        }
    }

    /// Differences to `expected` for the chunk at `pos`, each with the region it concerns.
    fn diff(&self, expected: &Self, pos: ChunkPos, min_y: i32, out: &mut Vec<Diff>) {
        let at = format!("chunk [{}, {}]", pos.x, pos.z);
        let (x, z) = (pos.min_block_x(), pos.min_block_z());
        let max_y = min_y + self.sections.len() as i32 * 16 - 1;
        let column = BoundingBox::new(x, min_y, z, x + 15, max_y, z + 15);
        let section = |index: usize| {
            let y = min_y + index as i32 * 16;
            BoundingBox::new(x, y, z, x + 15, y + 15, z + 15)
        };
        for (index, (a, e)) in self.sections.iter().zip(&expected.sections).enumerate() {
            if a != e {
                out.push(Diff {
                    text: format!(
                        "{at} section {index} (y {}): blocks differ",
                        min_y + index as i32 * 16
                    ),
                    region: section(index),
                });
            }
        }
        if self.heightmaps != expected.heightmaps {
            out.push(Diff {
                text: format!("{at}: heightmaps differ"),
                region: column,
            });
        }
        for (index, (a, e)) in self
            .post_processing
            .iter()
            .zip(&expected.post_processing)
            .enumerate()
        {
            if a != e {
                out.push(Diff {
                    text: format!("{at} section {index}: post-processing marks differ"),
                    region: section(index),
                });
            }
        }
        if self.block_entities != expected.block_entities {
            let mut region: Option<BoundingBox> = None;
            for (x, y, z, _) in self
                .block_entities
                .iter()
                .filter(|e| !expected.block_entities.contains(e))
                .chain(
                    expected
                        .block_entities
                        .iter()
                        .filter(|e| !self.block_entities.contains(e)),
                )
            {
                let pos = BoundingBox::new(*x, *y, *z, *x, *y, *z);
                region.get_or_insert(pos).encapsulate(&pos);
            }
            out.push(Diff {
                text: format!(
                    "{at}: block entities differ: {:?}, expected {:?}",
                    self.block_entities, expected.block_entities
                ),
                region: region.unwrap_or(column),
            });
        }
        if self.block_ticks != expected.block_ticks {
            out.push(Diff {
                text: format!(
                    "{at}: block ticks differ ({} ticks, expected {})",
                    self.block_ticks.0, expected.block_ticks.0
                ),
                region: column,
            });
        }
        if self.fluid_ticks != expected.fluid_ticks {
            out.push(Diff {
                text: format!(
                    "{at}: fluid ticks differ ({} ticks, expected {})",
                    self.fluid_ticks.0, expected.fluid_ticks.0
                ),
                region: column,
            });
        }
    }
}

/// A difference to the golden and the blocks it concerns.
struct Diff {
    text: String,
    region: BoundingBox,
}

/// Count and hash of ticks as `FeatureGoldens.tickHash`: type name, x, y, z, priority.
fn tick_hash<T: Copy>(ticks: &[ProtoTick<T>], name: impl Fn(T) -> &'static str) -> (i64, i64) {
    let mut hash = 0_i64;
    for tick in ticks {
        for v in [
            i64::from(java_string_hash(name(tick.kind))),
            i64::from(tick.pos.x),
            i64::from(tick.pos.y),
            i64::from(tick.pos.z),
            i64::from(tick.priority),
        ] {
            hash = hash.wrapping_mul(31).wrapping_add(v);
        }
    }
    (ticks.len() as i64, hash)
}

/// The decoration areas of `structure_decoration.json` centred on jigsaw structures: steps 1–4
/// for the 5×5 chunks (starts within 10), then the features stage with structures for the 3×3
/// chunks in row-major order. Set `VANILLITE_STRUCTURE` (e.g. `village_plains`) to run one.
///
/// Structures that are not ported yet (mineshafts, strongholds, ...) also reach some areas, so
/// only differences in a section within a block of a jigsaw piece fail the test; the others are
/// counted. [`KNOWN_FOREIGN_DIFFERENCES`] lists the areas where such sections also differ
/// because of an unported structure.
#[test]
fn jigsaw_decoration_matches_vanilla() {
    let golden = common::golden("structure_decoration");
    let filter = std::env::var("VANILLITE_STRUCTURE").ok().map(|f| {
        if f.contains(':') {
            f
        } else {
            format!("minecraft:{f}")
        }
    });
    let mut generators: HashMap<(String, i64), TerrainGenerator> = HashMap::new();
    let mut total = 0;
    let mut exact = 0;
    let mut foreign = 0;
    let mut failures: Vec<(String, Vec<String>)> = Vec::new();
    for area in list(&golden["areas"]) {
        let name = area["structure"].as_str().expect("structure");
        if filter.as_ref().is_some_and(|f| f != name) {
            continue;
        }
        let dimension = area["dimension"].as_str().expect("dimension");
        let seed = int(&area["seed"]);
        let generator = generators
            .entry((dimension.to_owned(), seed))
            .or_insert_with(|| generator(dimension, seed));
        let Some(id) = generator.structures().structure_id(name) else {
            continue;
        };
        if !matches!(
            generator.structures().structure(id).kind,
            StructureKind::Jigsaw(_)
        ) {
            continue;
        }
        total += 1;
        let center = ChunkPos::new(int(&area["x"]) as i32, int(&area["z"]) as i32);
        let min_y = int(&area["min_y"]) as i32;
        let (jigsaw, other): (Vec<_>, Vec<_>) =
            decorate_area(generator, center, min_y, list(&area["chunks"]))
                .into_iter()
                .partition(|(_, near_jigsaw)| *near_jigsaw);
        eprintln!(
            "{name} around [{}, {}]: {}",
            center.x,
            center.z,
            match (jigsaw.is_empty(), other.is_empty()) {
                (true, true) => "match".to_owned(),
                (true, false) => format!(
                    "jigsaw pieces match; {} differences away from them (other structures)",
                    other.len()
                ),
                _ => format!("{} differences at jigsaw pieces", jigsaw.len()),
            }
        );
        if other.is_empty() && jigsaw.is_empty() {
            exact += 1;
        }
        let known = KNOWN_FOREIGN_DIFFERENCES.iter().find(|(n, _)| *n == name);
        if let (Some((_, reason)), false) = (known, jigsaw.is_empty()) {
            eprintln!("  known: {reason}");
            foreign += 1;
            continue;
        }
        if !jigsaw.is_empty() {
            failures.push((
                format!("{name} around [{}, {}]", center.x, center.z),
                jigsaw.into_iter().map(|(text, _)| text).collect(),
            ));
        }
    }
    eprintln!(
        "jigsaw decoration areas: {exact}/{total} match exactly, {}/{total} match in every \
         section near a jigsaw piece, {foreign} differ there because of unported structures",
        total - failures.len() - foreign
    );
    let mut text = String::new();
    for (label, diffs) in &failures {
        text += &format!("\n{label}:");
        for diff in diffs.iter().take(12) {
            text += &format!("\n    {diff}");
        }
        if diffs.len() > 12 {
            text += &format!("\n    ... {} more", diffs.len() - 12);
        }
    }
    assert!(
        failures.is_empty(),
        "{} areas differ:{text}",
        failures.len()
    );
}

/// Generates and decorates the area around `center`; the differences to the golden chunks,
/// each with whether it touches a jigsaw piece.
fn decorate_area(
    generator: &TerrainGenerator,
    center: ChunkPos,
    min_y: i32,
    expected: &[Value],
) -> Vec<(String, bool)> {
    // Starts within 10 chunks (in parallel).
    let mut positions = Vec::new();
    for dz in -10..=10 {
        for dx in -10..=10 {
            positions.push(ChunkPos::new(center.x + dx, center.z + dz));
        }
    }
    let starts: Mutex<HashMap<ChunkPos, ProtoChunk>> = Mutex::new(HashMap::new());
    let next = std::sync::atomic::AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(16));
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(&pos) = positions.get(index) else {
                        break;
                    };
                    let mut batch = [generator.new_chunk(pos)];
                    generator.generate_structure_starts(&mut batch);
                    let [chunk] = batch;
                    starts.lock().unwrap().insert(pos, chunk);
                }
            });
        }
    });
    let starts = starts.into_inner().unwrap();
    // References within 2, biomes within 3, terrain within 2.
    let mut biomes = Vec::new();
    for dz in -3..=3 {
        for dx in -3..=3 {
            let pos = ChunkPos::new(center.x + dx, center.z + dz);
            let mut chunk = starts[&pos].clone();
            if dx.abs() <= 2 && dz.abs() <= 2 {
                let neighbours = NeighbourStarts::collect(pos, |ox, oz| {
                    &starts[&ChunkPos::new(pos.x + ox, pos.z + oz)]
                });
                generator.generate_structure_references(&mut chunk, &neighbours);
            }
            biomes.push(chunk);
        }
    }
    generator.generate_biomes(&mut biomes);
    let at = |dx: i32, dz: i32| ((dz + 3) * 7 + dx + 3) as usize;
    let mut jobs = Vec::new();
    for dz in -2..=2 {
        for dx in -2..=2 {
            let chunk = biomes[at(dx, dz)].clone();
            let neighbours = NeighbourBiomes::collect(|ox, oz| &biomes[at(dx + ox, dz + oz)]);
            let structures = ReferencedStarts::collect(&chunk, |p| &starts[&p]);
            jobs.push(TerrainJob {
                chunk,
                neighbours,
                structures,
            });
        }
    }
    generator.generate_terrain(&mut jobs);
    let mut chunks: Vec<ProtoChunk> = jobs.into_iter().map(|j| j.chunk).collect();
    for dz in -1..=1 {
        for dx in -1..=1 {
            let pos = ChunkPos::new(center.x + dx, center.z + dz);
            let structures = {
                let chunk = chunks.iter().find(|c| c.pos() == pos).expect("chunk");
                ReferencedStarts::collect(chunk, |p| &starts[&p])
            };
            let region: [&mut ProtoChunk; 9] = chunks
                .iter_mut()
                .filter(|c| (c.pos().x - pos.x).abs() <= 1 && (c.pos().z - pos.z).abs() <= 1)
                .collect::<Vec<_>>()
                .try_into()
                .expect("nine chunks");
            generator.generate_features(region, &structures);
        }
    }
    let mut diffs = Vec::new();
    for (chunk, expected) in chunks.iter().zip(expected) {
        Snapshot::of(chunk).diff(
            &Snapshot::from_golden(expected),
            chunk.pos(),
            min_y,
            &mut diffs,
        );
    }
    // The boxes of every jigsaw piece of the starts that can reach the area.
    let mut jigsaw_boxes = Vec::new();
    for chunk in starts.values() {
        for (id, start) in chunk.structure_starts() {
            if matches!(
                generator.structures().structure(*id).kind,
                StructureKind::Jigsaw(_)
            ) {
                for piece in start.pieces().iter() {
                    jigsaw_boxes.push(piece.bounding_box().inflated_by(1, 1, 1));
                }
            }
        }
    }
    diffs
        .into_iter()
        .map(|d| {
            let near_jigsaw = jigsaw_boxes.iter().any(|b| b.intersects(&d.region));
            (d.text, near_jigsaw)
        })
        .collect()
}
