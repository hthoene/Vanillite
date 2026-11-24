//! Mineshafts, strongholds and nether fortresses against the Vanilla structure goldens
//! (`StructureGoldens.java`, see `tests/goldens/STRUCTURES.md`).
//!
//! - [`procedural_starts_match_vanilla`]: every start of these structures in
//!   `structure_starts.json`: the start's box and each piece's type, box, orientation,
//!   generation depth, rotation, mirror and saved data, in order.
//! - [`procedural_decoration_matches_vanilla`]: the decorated areas of these structures in
//!   `structure_decoration.json` (blocks per section, heightmaps, post-processing marks, block
//!   entities, ticks). Entities (chest minecarts) are not spawned and not compared.

mod common;

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};

use serde_json::{Value, json};
use vanillite_base::math::java_string_hash;
use vanillite_base::{ChunkPos, Direction};
use vanillite_chunk::HeightmapKind;
use vanillite_nbt::{Compound, Tag};
use vanillite_worldgen::proto::ProtoTick;
use vanillite_worldgen::structure::StructurePiece;
use vanillite_worldgen::template::{BoundingBox, Mirror, Rotation};
use vanillite_worldgen::{
    DimensionSpec, NeighbourBiomes, NeighbourStarts, ProtoChunk, ReferencedStarts,
    TerrainGenerator, TerrainJob,
};

/// The structures this test covers.
const STRUCTURES: [&str; 4] = [
    "minecraft:mineshaft",
    "minecraft:mineshaft_mesa",
    "minecraft:stronghold",
    "minecraft:fortress",
];

fn int(v: &Value) -> i64 {
    v.as_i64().expect("integer in golden")
}

fn list(v: &Value) -> &[Value] {
    v.as_array().map_or(&[], Vec::as_slice)
}

/// Generators per dimension and seed, built on first use.
#[derive(Default)]
struct Generators(HashMap<(String, i64), TerrainGenerator>);

impl Generators {
    /// The generator of a golden world or area (`dimension`, `seed`, `min_y`, `height`).
    fn get(&mut self, world: &Value) -> &TerrainGenerator {
        let dimension = world["dimension"].as_str().expect("dimension");
        let seed = int(&world["seed"]);
        self.0
            .entry((dimension.to_owned(), seed))
            .or_insert_with(|| {
                let source = match dimension {
                    "minecraft:nether" => {
                        json!({"type": "minecraft:multi_noise", "preset": "minecraft:nether"})
                    }
                    "minecraft:end" => json!({"type": "minecraft:the_end"}),
                    _ => json!({"type": "minecraft:multi_noise", "preset": "minecraft:overworld"}),
                };
                let spec = DimensionSpec {
                    noise_settings: dimension,
                    biome_source: &source,
                    min_y: int(&world["min_y"]) as i32,
                    height: int(&world["height"]) as i32,
                    has_sky_light: dimension == "minecraft:overworld",
                };
                TerrainGenerator::new(common::packs(), spec, seed).expect("generator")
            })
    }
}

fn box_json(bb: &BoundingBox) -> Value {
    json!([
        bb.min_x(),
        bb.min_y(),
        bb.min_z(),
        bb.max_x(),
        bb.max_y(),
        bb.max_z()
    ])
}

fn direction_name(direction: Direction) -> &'static str {
    match direction {
        Direction::Down => "down",
        Direction::Up => "up",
        Direction::North => "north",
        Direction::South => "south",
        Direction::West => "west",
        Direction::East => "east",
    }
}

fn rotation_name(rotation: Rotation) -> &'static str {
    match rotation {
        Rotation::None => "NONE",
        Rotation::Clockwise90 => "CLOCKWISE_90",
        Rotation::Clockwise180 => "CLOCKWISE_180",
        Rotation::Counterclockwise90 => "COUNTERCLOCKWISE_90",
    }
}

fn mirror_name(mirror: Mirror) -> &'static str {
    match mirror {
        Mirror::None => "NONE",
        Mirror::LeftRight => "LEFT_RIGHT",
        Mirror::FrontBack => "FRONT_BACK",
    }
}

/// An NBT value as the goldens write it (numbers as JSON numbers, lists and arrays as arrays).
fn nbt_json(tag: &Tag) -> Value {
    match tag {
        Tag::Byte(v) => json!(v),
        Tag::Short(v) => json!(v),
        Tag::Int(v) => json!(v),
        Tag::Long(v) => json!(v),
        Tag::Float(v) => json!(v),
        Tag::Double(v) => json!(v),
        Tag::String(v) => json!(v),
        Tag::ByteArray(v) => json!(v),
        Tag::IntArray(v) => json!(v),
        Tag::LongArray(v) => json!(v),
        Tag::List(v) => Value::Array(v.0.iter().map(nbt_json).collect()),
        Tag::Compound(v) => compound_json(v),
    }
}

fn compound_json(tag: &Compound) -> Value {
    Value::Object(
        tag.iter()
            .map(|(k, v)| (k.to_owned(), nbt_json(v)))
            .collect(),
    )
}

/// A piece as a golden piece entry: `[type, bb, orientation, gen_depth, rotation, mirror,
/// data]`.
fn piece_json(piece: &dyn StructurePiece) -> Value {
    let data = piece.additional_save_data().unwrap_or_default();
    let orientation = piece.orientation();
    let (rotation, mirror) = match orientation {
        Some(_) => (
            json!(rotation_name(piece.rotation())),
            json!(mirror_name(piece.mirror())),
        ),
        None => (Value::Null, Value::Null),
    };
    json!([
        piece.base().kind,
        box_json(piece.bounding_box()),
        orientation.map(direction_name),
        piece.gen_depth(),
        rotation,
        mirror,
        compound_json(&data),
    ])
}

#[test]
fn procedural_starts_match_vanilla() {
    let golden = common::golden("structure_starts");
    let worlds = list(&golden["worlds"]);
    let mut generators = Generators::default();
    let mut total = 0;
    let mut failures = Vec::new();
    let mut per_structure: HashMap<String, (usize, usize)> = HashMap::new();
    for start in list(&golden["starts"]) {
        let structure = start["structure"].as_str().unwrap_or_default();
        if !STRUCTURES.contains(&structure) {
            continue;
        }
        total += 1;
        let world = &worlds[int(&start["w"]) as usize];
        let generator = generators.get(world);
        let pos = ChunkPos::new(
            int(&start["chunk"][0]) as i32,
            int(&start["chunk"][1]) as i32,
        );
        let label = format!(
            "{structure} in world {} chunk [{}, {}]",
            int(&start["w"]),
            pos.x,
            pos.z
        );
        let id = generator
            .structures()
            .structure_id(structure)
            .expect("structure id");
        let mut batch = vec![generator.new_chunk(pos)];
        generator.generate_structure_starts(&mut batch);
        let entry = per_structure.entry(structure.to_owned()).or_default();
        entry.1 += 1;
        let Some(actual) = batch[0].start_for_structure(id) else {
            failures.push(format!("{label}: no start"));
            continue;
        };
        let mut diffs = Vec::new();
        if box_json(&actual.bounding_box()) != start["bb"] {
            diffs.push(format!(
                "start box {} expected {}",
                box_json(&actual.bounding_box()),
                start["bb"]
            ));
        }
        let pieces = actual.pieces();
        let expected = list(&start["pieces"]);
        if pieces.len() != expected.len() {
            diffs.push(format!(
                "{} pieces, expected {}",
                pieces.len(),
                expected.len()
            ));
        }
        if let Some((index, (a, e))) = pieces
            .iter()
            .map(|p| piece_json(&**p))
            .zip(expected)
            .enumerate()
            .find(|(_, (a, e))| a != *e)
        {
            diffs.push(format!("first differing piece {index}: {a}, expected {e}"));
        }
        if diffs.is_empty() {
            entry.0 += 1;
        } else {
            failures.push(format!("{label}: {}", diffs.join("; ")));
        }
    }
    let mut summary: Vec<_> = per_structure.into_iter().collect();
    summary.sort();
    for (structure, (ok, all)) in summary {
        println!("{structure}: {ok} of {all} starts match");
    }
    for failure in &failures {
        println!("{failure}");
    }
    assert!(total > 0, "no starts of {STRUCTURES:?} in the golden");
    assert!(
        failures.is_empty(),
        "{} of {total} starts differ from Vanilla",
        failures.len()
    );
}

/// The recorded heightmaps, in golden order.
const HEIGHTMAPS: [HeightmapKind; 4] = [
    HeightmapKind::WorldSurface,
    HeightmapKind::OceanFloor,
    HeightmapKind::MotionBlocking,
    HeightmapKind::MotionBlockingNoLeaves,
];

/// What the goldens record of one chunk (as `feature_goldens`).
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

    fn diff(&self, expected: &Self, pos: ChunkPos, min_y: i32, out: &mut Vec<String>) {
        let at = format!("chunk [{}, {}]", pos.x, pos.z);
        for (index, (a, e)) in self.sections.iter().zip(&expected.sections).enumerate() {
            if a != e {
                out.push(format!(
                    "{at} section y {}: blocks differ",
                    min_y + index as i32 * 16
                ));
            }
        }
        if self.heightmaps != expected.heightmaps {
            out.push(format!("{at}: heightmaps differ"));
        }
        for (index, (a, e)) in self
            .post_processing
            .iter()
            .zip(&expected.post_processing)
            .enumerate()
        {
            if a != e {
                out.push(format!(
                    "{at} section y {}: post-processing marks differ ({} marks, expected {})",
                    min_y + index as i32 * 16,
                    a.0,
                    e.0
                ));
            }
        }
        if self.block_entities != expected.block_entities {
            out.push(format!(
                "{at}: block entities {:?}, expected {:?}",
                self.block_entities, expected.block_entities
            ));
        }
        if self.block_ticks != expected.block_ticks {
            out.push(format!(
                "{at}: block ticks differ ({}, expected {})",
                self.block_ticks.0, expected.block_ticks.0
            ));
        }
        if self.fluid_ticks != expected.fluid_ticks {
            out.push(format!(
                "{at}: fluid ticks differ ({}, expected {})",
                self.fluid_ticks.0, expected.fluid_ticks.0
            ));
        }
    }
}

/// Count and hash of ticks as `FeatureGoldens.tickHash`.
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

/// Steps 1–5 of the decoration golden around `center`: the 5×5 decorated area (row-major).
fn decorate(generator: &TerrainGenerator, center: ChunkPos) -> Vec<ProtoChunk> {
    let at = |dx: i32, dz: i32| ChunkPos::new(center.x + dx, center.z + dz);
    // Starts within 10.
    let mut batch: Vec<ProtoChunk> = (-10..=10)
        .flat_map(|dz| (-10..=10).map(move |dx| (dx, dz)))
        .map(|(dx, dz)| generator.new_chunk(at(dx, dz)))
        .collect();
    generator.generate_structure_starts(&mut batch);
    let starts: HashMap<ChunkPos, ProtoChunk> = batch.into_iter().map(|c| (c.pos(), c)).collect();
    // References within 2, biomes within 3.
    let mut biomes: Vec<ProtoChunk> = (-3..=3)
        .flat_map(|dz| (-3..=3).map(move |dx| (dx, dz)))
        .map(|(dx, dz)| {
            let pos = at(dx, dz);
            let mut chunk = starts[&pos].clone();
            if dx.abs() <= 2 && dz.abs() <= 2 {
                let neighbours = NeighbourStarts::collect(pos, |ox, oz| {
                    &starts[&ChunkPos::new(pos.x + ox, pos.z + oz)]
                });
                generator.generate_structure_references(&mut chunk, &neighbours);
            }
            chunk
        })
        .collect();
    generator.generate_biomes(&mut biomes);
    let index = |dx: i32, dz: i32| ((dz + 3) * 7 + dx + 3) as usize;
    // Terrain within 2.
    let mut jobs = Vec::new();
    for dz in -2..=2 {
        for dx in -2..=2 {
            let chunk = biomes[index(dx, dz)].clone();
            let neighbours = NeighbourBiomes::collect(|ox, oz| &biomes[index(dx + ox, dz + oz)]);
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
    // Decoration of the 3×3 around the centre, row-major.
    for dz in -1..=1 {
        for dx in -1..=1 {
            let pos = at(dx, dz);
            let structures =
                ReferencedStarts::collect(&chunks[((dz + 2) * 5 + dx + 2) as usize], |p| {
                    &starts[&p]
                });
            let region: [&mut ProtoChunk; 9] = chunks
                .iter_mut()
                .filter(|c| (c.pos().x - pos.x).abs() <= 1 && (c.pos().z - pos.z).abs() <= 1)
                .collect::<Vec<_>>()
                .try_into()
                .expect("nine chunks");
            generator.generate_features(region, &structures);
        }
    }
    chunks
}

#[test]
fn procedural_decoration_matches_vanilla() {
    let golden = common::golden("structure_decoration");
    let mut generators = Generators::default();
    let mut total = 0;
    let mut failures = Vec::new();
    for area in list(&golden["areas"]) {
        let structure = area["structure"].as_str().unwrap_or_default();
        if !STRUCTURES.contains(&structure) {
            continue;
        }
        total += 1;
        let generator = generators.get(area);
        let center = ChunkPos::new(int(&area["x"]) as i32, int(&area["z"]) as i32);
        let min_y = int(&area["min_y"]) as i32;
        let label = format!("{structure} around chunk [{}, {}]", center.x, center.z);
        let mut diffs = Vec::new();
        match catch_unwind(AssertUnwindSafe(|| decorate(generator, center))) {
            Ok(chunks) => {
                for (chunk, expected) in chunks.iter().zip(list(&area["chunks"])) {
                    Snapshot::of(chunk).diff(
                        &Snapshot::from_golden(expected),
                        chunk.pos(),
                        min_y,
                        &mut diffs,
                    );
                }
            }
            Err(_) => diffs.push("decoration panicked".to_owned()),
        }
        if diffs.is_empty() {
            println!("{label}: matches");
        } else {
            println!("{label}: {} differences", diffs.len());
            for diff in diffs.iter().take(20) {
                println!("    {diff}");
            }
            failures.push(label);
        }
    }
    println!(
        "{} of {total} decorated areas match",
        total - failures.len()
    );
    assert!(total > 0, "no decorated areas of {STRUCTURES:?}");
    assert!(
        failures.is_empty(),
        "decorated areas differ from Vanilla: {failures:?}"
    );
}
