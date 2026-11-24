//! Parity of whole large structures (ocean monuments, woodland mansions, end cities) with
//! Vanilla, against `structure_large.json` recorded by `StructureGoldens.java` (format:
//! `tests/goldens/STRUCTURES.md`).
//!
//! - [`large_starts_match_vanilla`]: each recorded start is generated again in its chunk and
//!   compared with every piece and its NBT.
//! - [`large_decoration_matches_vanilla`]: every chunk a piece of the start intersects is
//!   decorated in the recorded order on fresh chunks and compared right after its decoration
//!   (blocks, heightmaps, post-processing, ticks, block entities with their NBT, the entities
//!   its decoration added).
//!
//! `VANILLITE_STRUCTURE` (e.g. `minecraft:mansion`) limits both tests to one structure. Starts
//! that other structures not ported in this tree may reach are reported as such (they still
//! fail).

mod common;

use std::collections::{BTreeSet, HashMap};
use std::panic::{AssertUnwindSafe, catch_unwind};

use serde_json::{Value, json};
use vanillite_base::math::java_string_hash;
use vanillite_base::{BlockPos, ChunkPos};
use vanillite_chunk::HeightmapKind;
use vanillite_nbt::{Compound, Tag};
use vanillite_worldgen::feature::region::entity_pos;
use vanillite_worldgen::proto::ProtoTick;
use vanillite_worldgen::structure::structure::StructureKind;
use vanillite_worldgen::structure::{
    GenerationContext, LevelHeight, StructurePiece, StructureStart, StructureState,
};
use vanillite_worldgen::template::BoundingBox;
use vanillite_worldgen::template::transform::direction_name;
use vanillite_worldgen::{
    DimensionSpec, NeighbourBiomes, NeighbourStarts, ProtoChunk, ReferencedStarts,
    TerrainGenerator, TerrainJob,
};

/// Differences listed per mismatching start.
const MAX_DIFFS: usize = 40;

fn int(v: &Value) -> i64 {
    v.as_i64()
        .unwrap_or_else(|| panic!("integer expected in golden, got {v}"))
}

fn list(v: &Value) -> &[Value] {
    v.as_array().map_or(&[], Vec::as_slice)
}

fn chunk_pos(v: &Value) -> ChunkPos {
    ChunkPos::new(int(&v[0]) as i32, int(&v[1]) as i32)
}

/// Whether `VANILLITE_STRUCTURE` (qualified or not) selects the structure.
fn selected(structure: &str) -> bool {
    match std::env::var("VANILLITE_STRUCTURE") {
        Ok(f) if f.contains(':') => f == structure,
        Ok(f) => structure.strip_prefix("minecraft:") == Some(f.as_str()),
        Err(_) => true,
    }
}

/// Generators per dimension and seed, built on first use.
#[derive(Default)]
struct Generators(HashMap<(String, i64), TerrainGenerator>);

impl Generators {
    /// The generator of a golden start (`dimension`, `seed`, `min_y`, `height`).
    fn get(&mut self, start: &Value) -> &TerrainGenerator {
        let dimension = start["dimension"].as_str().expect("dimension");
        let seed = int(&start["seed"]);
        self.0
            .entry((dimension.to_owned(), seed))
            .or_insert_with(|| {
                let source = match dimension {
                    "minecraft:end" => json!({"type": "minecraft:the_end"}),
                    "minecraft:nether" => {
                        json!({"type": "minecraft:multi_noise", "preset": "minecraft:nether"})
                    }
                    _ => json!({"type": "minecraft:multi_noise", "preset": "minecraft:overworld"}),
                };
                let spec = DimensionSpec {
                    noise_settings: dimension,
                    biome_source: &source,
                    min_y: int(&start["min_y"]) as i32,
                    height: int(&start["height"]) as i32,
                    has_sky_light: dimension == "minecraft:overworld",
                };
                TerrainGenerator::new(common::packs(), spec, seed).expect("generator")
            })
    }
}

/// Mismatching starts with their differences; prints them and fails if there are any.
fn report(what: &str, total: usize, failures: &[(String, Vec<String>)]) {
    let mut text = format!("{what}: {} of {total} match", total - failures.len());
    for (name, diffs) in failures {
        text += &format!("\n{name}:");
        for diff in diffs.iter().take(MAX_DIFFS) {
            text += &format!("\n    {diff}");
        }
        if diffs.len() > MAX_DIFFS {
            text += &format!("\n    ... {} more", diffs.len() - MAX_DIFFS);
        }
    }
    println!("{text}");
    assert!(total > 0, "no golden start selected");
    assert!(
        failures.is_empty(),
        "{} of {total} {what} differ",
        failures.len()
    );
}

fn label(start: &Value) -> String {
    format!(
        "{} at [{}, {}]",
        start["structure"].as_str().unwrap_or("?"),
        start["chunk"][0],
        start["chunk"][1]
    )
}

// -------------------------------------------------------------------------------------------
// NBT

/// Whether a value is an empty compound or list (Vanilla writes some defaults that way, e.g.
/// `components: {}`, which Vanillite's block entity tags leave out).
fn is_empty_json(v: &Value) -> bool {
    v.as_object().is_some_and(serde_json::Map::is_empty) || v.as_array().is_some_and(Vec::is_empty)
}

fn is_empty_tag(t: &Tag) -> bool {
    match t {
        Tag::Compound(c) => c.is_empty(),
        Tag::List(l) => l.0.is_empty(),
        _ => false,
    }
}

/// Differences between an NBT value and its JSON form (`StructureGoldens.nbt`): numbers in the
/// tag's type (floats as `f32`), compounds key by key. With `lenient`, keys that are empty on
/// one side and absent on the other are ignored.
fn nbt_diff(path: &str, tag: &Tag, golden: &Value, lenient: bool, out: &mut Vec<String>) {
    let ints = |values: Vec<i64>| {
        golden.is_array()
            && list(golden)
                .iter()
                .map(Value::as_i64)
                .eq(values.into_iter().map(Some))
    };
    let same = match tag {
        Tag::Byte(v) => golden.as_i64() == Some(i64::from(*v)),
        Tag::Short(v) => golden.as_i64() == Some(i64::from(*v)),
        Tag::Int(v) => golden.as_i64() == Some(i64::from(*v)),
        Tag::Long(v) => golden.as_i64() == Some(*v),
        Tag::Float(v) => golden.as_f64().is_some_and(|g| (g as f32) == *v),
        Tag::Double(v) => golden.as_f64() == Some(*v),
        Tag::String(v) => golden.as_str() == Some(v.as_str()),
        Tag::ByteArray(v) => ints(v.iter().map(|&b| i64::from(b)).collect()),
        Tag::IntArray(v) => ints(v.iter().map(|&b| i64::from(b)).collect()),
        Tag::LongArray(v) => ints(v.clone()),
        Tag::List(items) => {
            let Some(expected) = golden.as_array() else {
                out.push(format!("{path}: list, expected {golden}"));
                return;
            };
            if items.0.len() != expected.len() {
                out.push(format!(
                    "{path}: {} elements, expected {}",
                    items.0.len(),
                    expected.len()
                ));
                return;
            }
            for (i, (t, g)) in items.0.iter().zip(expected).enumerate() {
                nbt_diff(&format!("{path}[{i}]"), t, g, lenient, out);
            }
            return;
        }
        Tag::Compound(compound) => {
            compound_diff(path, compound, golden, lenient, &[], out);
            return;
        }
    };
    if !same {
        out.push(format!("{path}: {tag:?}, expected {golden}"));
    }
}

/// [`nbt_diff`] for a compound, ignoring the keys in `skip` on the Rust side.
fn compound_diff(
    path: &str,
    compound: &Compound,
    golden: &Value,
    lenient: bool,
    skip: &[&str],
    out: &mut Vec<String>,
) {
    let Some(expected) = golden.as_object() else {
        out.push(format!("{path}: compound, expected {golden}"));
        return;
    };
    let sep = if path.is_empty() { "" } else { "." };
    for (key, value) in compound.iter() {
        if skip.contains(&key) {
            continue;
        }
        match expected.get(key) {
            Some(g) => nbt_diff(&format!("{path}{sep}{key}"), value, g, lenient, out),
            None if lenient && is_empty_tag(value) => {}
            None => out.push(format!("{path}{sep}{key}: {value:?} not expected")),
        }
    }
    for (key, g) in expected {
        let present = compound.get(key).is_some() && !skip.contains(&key.as_str());
        if !present && !(lenient && is_empty_json(g)) {
            out.push(format!("{path}{sep}{key}: missing, expected {g}"));
        }
    }
}

// -------------------------------------------------------------------------------------------
// Starts

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

/// Differences of one piece to `[type id, bb, orientation, gen_depth, rotation, mirror, data]`.
fn piece_diff(piece: &dyn StructurePiece, golden: &Value, out: &mut Vec<String>) {
    let kind = piece.base().kind;
    if kind != golden[0].as_str().unwrap_or_default() {
        out.push(format!("type {kind}, expected {}", golden[0]));
        return;
    }
    let bb = box_json(piece.bounding_box());
    if bb != golden[1] {
        out.push(format!("bb {bb}, expected {}", golden[1]));
    }
    let orientation = piece.orientation().map(direction_name);
    if orientation != golden[2].as_str() {
        out.push(format!(
            "orientation {orientation:?}, expected {}",
            golden[2]
        ));
    }
    if i64::from(piece.gen_depth()) != int(&golden[3]) {
        out.push(format!(
            "gen depth {}, expected {}",
            piece.gen_depth(),
            golden[3]
        ));
    }
    let rotation = piece.rotation().legacy_name();
    if rotation != golden[4].as_str().unwrap_or("NONE") {
        out.push(format!("rotation {rotation}, expected {}", golden[4]));
    }
    let mirror = piece.mirror().legacy_name();
    if mirror != golden[5].as_str().unwrap_or("NONE") {
        out.push(format!("mirror {mirror}, expected {}", golden[5]));
    }
    match piece.additional_save_data() {
        Some(data) => compound_diff("data", &data, &golden[6], false, &[], out),
        None => out.push("piece NBT not written (additional_save_data)".to_owned()),
    }
}

fn start_diff(start: &StructureStart, golden: &Value) -> Vec<String> {
    let mut diffs = Vec::new();
    if i64::from(start.references()) != int(&golden["references"]) {
        diffs.push(format!(
            "references {}, expected {}",
            start.references(),
            golden["references"]
        ));
    }
    let bb = box_json(&start.bounding_box());
    if bb != golden["bb"] {
        diffs.push(format!("bb {bb}, expected {}", golden["bb"]));
    }
    let pieces = start.pieces();
    let expected = list(&golden["pieces"]);
    if pieces.len() != expected.len() {
        diffs.push(format!(
            "{} pieces, expected {}",
            pieces.len(),
            expected.len()
        ));
    }
    for (index, (piece, golden)) in pieces.iter().zip(expected).enumerate() {
        let mut piece_diffs = Vec::new();
        piece_diff(&**piece, golden, &mut piece_diffs);
        diffs.extend(
            piece_diffs
                .into_iter()
                .map(|d| format!("piece {index} ({}): {d}", golden[0].as_str().unwrap_or("?"))),
        );
    }
    diffs
}

#[test]
fn large_starts_match_vanilla() {
    let golden = common::golden("structure_large");
    let mut generators = Generators::default();
    let (mut total, mut failures) = (0, Vec::new());
    for start in list(&golden["starts"]) {
        let name = start["structure"].as_str().expect("structure");
        if !selected(name) {
            continue;
        }
        total += 1;
        let generator = generators.get(start);
        let id = generator
            .structures()
            .structure_id(name)
            .expect("structure in registry");
        let mut batch = vec![generator.new_chunk(chunk_pos(&start["chunk"]))];
        generator.generate_structure_starts(&mut batch);
        let diffs = match batch[0].start_for_structure(id) {
            Some(generated) => start_diff(generated, start),
            None => vec!["no start generated".to_owned()],
        };
        if !diffs.is_empty() {
            failures.push((label(start), diffs));
        }
    }
    report("large structure starts", total, &failures);
}

// -------------------------------------------------------------------------------------------
// Decoration

/// The recorded heightmaps, in golden order.
const HEIGHTMAPS: [HeightmapKind; 4] = [
    HeightmapKind::WorldSurface,
    HeightmapKind::OceanFloor,
    HeightmapKind::MotionBlocking,
    HeightmapKind::MotionBlockingNoLeaves,
];

/// Count and hash of ticks as `FeatureGoldens.tickHash`: type name, x, y, z, priority.
fn tick_hash<T>(ticks: &[ProtoTick<T>], name: impl Fn(&T) -> &str) -> (i64, i64) {
    let mut hash = 0_i64;
    for tick in ticks {
        for v in [
            i64::from(java_string_hash(name(&tick.kind))),
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

/// Differences of a decorated chunk to its golden entry.
fn chunk_diff(chunk: &ProtoChunk, golden: &Value, out: &mut Vec<String>) {
    let pos = chunk.pos();
    let at = format!("chunk [{}, {}]", pos.x, pos.z);
    for (index, (section, expected)) in chunk
        .sections()
        .iter()
        .zip(list(&golden["sections"]))
        .enumerate()
    {
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
        if hash != int(expected) {
            out.push(format!(
                "{at} section {index} (y {}): blocks differ",
                chunk.min_y() + index as i32 * 16
            ));
        }
    }
    for (i, kind) in HEIGHTMAPS.iter().enumerate() {
        let mut hash = 0_i64;
        for z in 0..16 {
            for x in 0..16 {
                hash = hash
                    .wrapping_mul(31)
                    .wrapping_add(i64::from(chunk.heightmap(*kind, x, z)));
            }
        }
        if hash != int(&golden["heightmaps"][i]) {
            out.push(format!("{at}: heightmap {kind:?} differs"));
        }
    }
    let mut marks = vec![(0_i64, 0_i64); chunk.sections().len()];
    for &(x, y, z) in chunk.post_processing() {
        let packed = ((x & 15) | ((y & 15) << 4) | ((z & 15) << 8)) as i16;
        let entry = &mut marks[chunk.section_index(y)];
        entry.0 += 1;
        entry.1 = entry.1.wrapping_mul(31).wrapping_add(i64::from(packed));
    }
    let pair = |v: &Value| (int(&v[0]), int(&v[1]));
    for (index, (actual, expected)) in marks
        .iter()
        .zip(list(&golden["post_processing"]))
        .enumerate()
    {
        if *actual != pair(expected) {
            out.push(format!(
                "{at} section {index}: post-processing marks differ ({} marks, expected {})",
                actual.0, expected[0]
            ));
        }
    }
    let block_ticks = tick_hash(chunk.block_ticks().ticks(), |b| b.name());
    if block_ticks != pair(&golden["block_ticks"]) {
        out.push(format!(
            "{at}: block ticks differ ({} ticks, expected {})",
            block_ticks.0, golden["block_ticks"][0]
        ));
    }
    let fluid_ticks = tick_hash(chunk.fluid_ticks().ticks(), |f| {
        vanillite_data::registry::FLUID.name(*f).unwrap_or("")
    });
    if fluid_ticks != pair(&golden["fluid_ticks"]) {
        out.push(format!(
            "{at}: fluid ticks differ ({} ticks, expected {})",
            fluid_ticks.0, golden["fluid_ticks"][0]
        ));
    }
    block_entities_diff(chunk, list(&golden["block_entities"]), &at, out);
}

/// Block entities: positions and type ids, then the NBT where the golden has it.
fn block_entities_diff(chunk: &ProtoChunk, golden: &[Value], at: &str, out: &mut Vec<String>) {
    let id = |tag: &Compound| match tag.get("id") {
        Some(Tag::String(id)) => id.clone(),
        _ => String::new(),
    };
    let mut expected: HashMap<BlockPos, &Value> = golden
        .iter()
        .map(|e| {
            let pos = BlockPos::new(int(&e[0]) as i32, int(&e[1]) as i32, int(&e[2]) as i32);
            (pos, e)
        })
        .collect();
    let mut actual: Vec<(&BlockPos, &Compound)> = chunk.block_entities().iter().collect();
    actual.sort_by_key(|(p, _)| (p.y, p.z, p.x));
    for (pos, tag) in actual {
        let at_pos = format!("{at}: block entity at {} {} {}", pos.x, pos.y, pos.z);
        let Some(e) = expected.remove(pos) else {
            out.push(format!("{at_pos} ({}) not expected", id(tag)));
            continue;
        };
        let expected_id = e[3].as_str().unwrap_or_default();
        if id(tag) != expected_id {
            out.push(format!(
                "{at_pos}: type {}, expected {expected_id}",
                id(tag)
            ));
        } else if !e[4].is_null() {
            let mut diffs = Vec::new();
            let skip = ["id", "x", "y", "z", "keepPacked"];
            compound_diff("", tag, &e[4], true, &skip, &mut diffs);
            out.extend(diffs.into_iter().map(|d| format!("{at_pos}: {d}")));
        }
    }
    let mut missing: Vec<(&BlockPos, &&Value)> = expected.iter().collect();
    missing.sort_by_key(|(p, _)| (p.y, p.z, p.x));
    for (pos, e) in missing {
        out.push(format!(
            "{at}: block entity at {} {} {} missing, expected {}",
            pos.x, pos.y, pos.z, e[3]
        ));
    }
}

/// An entity as `[type id, x, y, z]`.
fn entity_json(tag: &Compound) -> Value {
    let kind = match tag.get("id") {
        Some(Tag::String(id)) => id.as_str(),
        _ => "",
    };
    let [x, y, z] = entity_pos(tag);
    json!([kind, x, y, z])
}

/// The entities one chunk's decoration added (Vanillite stores them per chunk, so the order of
/// Vanilla's `addFreshEntity` calls is compared within each chunk only).
fn entities_diff(mut actual: Vec<Value>, golden: &[Value], at: &str, out: &mut Vec<String>) {
    for e in golden {
        match actual.iter().position(|a| a == e) {
            Some(i) => {
                actual.remove(i);
            }
            None => out.push(format!("{at}: entity {e} not added")),
        }
    }
    for a in actual {
        out.push(format!("{at}: entity {a} added, Vanilla has none"));
    }
}

/// All chunks within `radius` of a chunk of `chunks`, row-major (z, then x).
fn around(chunks: &[ChunkPos], radius: i32) -> BTreeSet<(i32, i32)> {
    let mut out = BTreeSet::new();
    for c in chunks {
        for dz in -radius..=radius {
            for dx in -radius..=radius {
                out.insert((c.z + dz, c.x + dx));
            }
        }
    }
    out
}

/// Decorates `decorated` in order on fresh chunks (each with Vanilla's dependencies: starts,
/// references, biomes, terrain) and compares each chunk right after its decoration.
fn decorate(
    generator: &TerrainGenerator,
    decorated: &[ChunkPos],
    golden: &[&Value],
) -> Vec<String> {
    let pos = |(z, x): (i32, i32)| ChunkPos::new(x, z);
    let mut starts: Vec<ProtoChunk> = around(decorated, 10)
        .into_iter()
        .map(|p| generator.new_chunk(pos(p)))
        .collect();
    generator.generate_structure_starts(&mut starts);
    let starts: HashMap<ChunkPos, ProtoChunk> = starts.into_iter().map(|c| (c.pos(), c)).collect();
    let start_at = |p: ChunkPos| {
        starts
            .get(&p)
            .unwrap_or_else(|| panic!("no structure starts generated for {p:?}"))
    };

    // References and biomes within 2, terrain within 1 of the decorated chunks.
    let mut biomes: Vec<ProtoChunk> = around(decorated, 2)
        .into_iter()
        .map(|p| {
            let p = pos(p);
            let mut chunk = start_at(p).clone();
            let neighbours =
                NeighbourStarts::collect(p, |dx, dz| start_at(ChunkPos::new(p.x + dx, p.z + dz)));
            generator.generate_structure_references(&mut chunk, &neighbours);
            chunk
        })
        .collect();
    generator.generate_biomes(&mut biomes);
    let biomes: HashMap<ChunkPos, ProtoChunk> = biomes.into_iter().map(|c| (c.pos(), c)).collect();
    let mut jobs: Vec<TerrainJob> = around(decorated, 1)
        .into_iter()
        .map(|p| {
            let p = pos(p);
            let chunk = biomes[&p].clone();
            let neighbours =
                NeighbourBiomes::collect(|dx, dz| &biomes[&ChunkPos::new(p.x + dx, p.z + dz)]);
            let structures = ReferencedStarts::collect(&chunk, start_at);
            TerrainJob {
                chunk,
                neighbours,
                structures,
            }
        })
        .collect();
    generator.generate_terrain(&mut jobs);
    // Row-major, so that the 3×3 region of a chunk comes out in Vanilla's order.
    let mut chunks: Vec<ProtoChunk> = jobs.into_iter().map(|j| j.chunk).collect();

    let mut diffs = Vec::new();
    for (&center, expected) in decorated.iter().zip(golden) {
        let index = chunks
            .iter()
            .position(|c| c.pos() == center)
            .expect("decorated chunk");
        let structures = ReferencedStarts::collect(&chunks[index], start_at);
        let before: Vec<usize> = chunks.iter().map(|c| c.entities().len()).collect();
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            let region: [&mut ProtoChunk; 9] = chunks
                .iter_mut()
                .filter(|c| (c.pos().x - center.x).abs() <= 1 && (c.pos().z - center.z).abs() <= 1)
                .collect::<Vec<_>>()
                .try_into()
                .expect("nine chunks");
            generator.generate_features(region, &structures);
        }));
        let at = format!("chunk [{}, {}]", center.x, center.z);
        if let Err(payload) = outcome {
            let message = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                .unwrap_or_default();
            diffs.push(format!("{at}: decoration panicked: {message}"));
            break;
        }
        let added: Vec<Value> = chunks
            .iter()
            .zip(&before)
            .flat_map(|(c, &n)| c.entities()[n..].iter().map(entity_json))
            .collect();
        chunk_diff(&chunks[index], expected, &mut diffs);
        entities_diff(added, list(&expected["entities"]), &at, &mut diffs);
    }
    diffs
}

/// Structures not ported in this tree whose placement allows a start within 10 chunks of the
/// decorated chunks with a fitting biome (Vanillite cannot know their extent without starts,
/// so this only labels mismatches).
fn unported_nearby(generator: &TerrainGenerator, decorated: &[ChunkPos]) -> Vec<String> {
    let registry = generator.structures();
    let state = StructureState::new(generator);
    let level = LevelHeight {
        min_y: generator.min_y(),
        height: generator.gen_depth(),
    };
    let mut found = BTreeSet::new();
    for &set_id in registry.possible_sets() {
        let set = registry.set(set_id);
        for (id, _) in &set.structures {
            let structure = registry.structure(*id);
            if structure.kind.is_ported() {
                continue;
            }
            // Mineshafts stay within 80 blocks of their start; the rest within 8 chunks.
            let reach = if matches!(structure.kind, StructureKind::Mineshaft(_)) {
                6
            } else {
                10
            };
            for (z, x) in around(decorated, reach) {
                if !set.placement.is_structure_chunk(&state, x, z) {
                    continue;
                }
                let pos = ChunkPos::new(x, z);
                let ctx = GenerationContext::new(generator, pos, level, &structure.settings.biomes);
                let (bx, bz) = (pos.min_block_x() + 8, pos.min_block_z() + 8);
                if ctx.could_structure_exist_in_column(bx, bz, level.min_y, level.max_y()) {
                    found.insert(format!("{} at [{x}, {z}]", structure.name));
                }
            }
        }
    }
    found.into_iter().collect()
}

#[test]
fn large_decoration_matches_vanilla() {
    let golden = common::golden("structure_large");
    let chunks = list(&golden["chunks"]);
    let mut generators = Generators::default();
    let (mut total, mut failures) = (0, Vec::new());
    for (index, start) in list(&golden["starts"]).iter().enumerate() {
        if !selected(start["structure"].as_str().expect("structure")) {
            continue;
        }
        total += 1;
        let generator = generators.get(start);
        let decorated: Vec<ChunkPos> = list(&start["decorated"]).iter().map(chunk_pos).collect();
        let expected: Vec<&Value> = chunks
            .iter()
            .filter(|c| int(&c["s"]) == index as i64)
            .collect();
        let mut diffs = decorate(generator, &decorated, &expected);
        if !diffs.is_empty() {
            let unported = unported_nearby(generator, &decorated);
            if !unported.is_empty() {
                diffs.insert(
                    0,
                    format!(
                        "note: structures not ported in this tree may reach the start: {}",
                        unported.join(", ")
                    ),
                );
            }
            failures.push((label(start), diffs));
        }
    }
    report("large structure decorations", total, &failures);
}
