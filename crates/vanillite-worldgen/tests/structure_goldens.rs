//! Structure parity with Vanilla, against the goldens recorded by `StructureGoldens.java`
//! (format: `tests/goldens/STRUCTURES.md`). Each test mirrors one part of the Java harness:
//!
//! - [`starts_match_vanilla`]: `createStructures` on fresh chunks (each world's area, then its
//!   targeted chunks), compared start by start with every piece and its NBT, plus the
//!   stronghold ring positions and the dimension origin.
//! - [`references_match_vanilla`]: `createReferences` for every chunk of the reference areas, in
//!   Vanilla's `LongOpenHashSet` order.
//! - [`terrain_matches_vanilla`]: the chunk's references, its `Beardifier` and the three terrain
//!   steps (noise, surface, carvers) built with it.
//! - [`decoration_matches_vanilla`]: `applyBiomeDecoration` with structures for the 3×3 chunks
//!   around each area's centre, compared on all 5×5 chunks (blocks, heightmaps, post-processing,
//!   ticks, block entities with their NBT) and the entities added.
//!
//! Every test prints a summary per structure id ("N of M … match", the mismatching items and
//! their first difference) and fails if anything differs. Structures whose type is still a
//! stub (`structure/kinds/unported.rs`) are listed apart as "not ported in this tree"; they fail
//! too. Set `VANILLITE_STRUCTURE` (e.g. `minecraft:igloo`) to check one structure id.

mod common;

use std::collections::{BTreeMap, HashMap};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use serde_json::{Value, json};
use vanillite_base::math::java_string_hash;
use vanillite_base::{BlockPos, ChunkPos};
use vanillite_chunk::HeightmapKind;
use vanillite_nbt::{Compound, Tag};
use vanillite_worldgen::feature::region::entity_pos;
use vanillite_worldgen::proto::{ProtoTick, WgHeightmap};
use vanillite_worldgen::structure::structure::StructureKind;
use vanillite_worldgen::structure::{
    Beardifier, GenerationContext, LevelHeight, StructurePiece, StructurePlacement, StructureStart,
    StructureState, TerrainAdjustment,
};
use vanillite_worldgen::template::BoundingBox;
use vanillite_worldgen::template::transform::direction_name;
use vanillite_worldgen::{
    DimensionSpec, NeighbourBiomes, NeighbourStarts, ProtoChunk, ReferencedStarts,
    TerrainGenerator, TerrainJob, TerrainStep,
};

/// Differences listed per mismatching structure before the rest is summarised.
const MAX_DIFFS: usize = 8;

/// [`MAX_DIFFS`], or `VANILLITE_STRUCTURE_DIFFS` (a number) to see more.
fn max_diffs() -> usize {
    std::env::var("VANILLITE_STRUCTURE_DIFFS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(MAX_DIFFS)
}

/// The structures among `others` that are not ported and share a structure set with
/// `structure`, as a note: where Vanilla generated one of them, `createStructures` stopped
/// there, while here the stub fails and the set's selection goes on to `structure`.
fn displaced_by_unported<'a>(
    generator: &TerrainGenerator,
    structure: &str,
    others: impl IntoIterator<Item = &'a str>,
) -> Option<String> {
    let registry = generator.structures();
    let id = registry.structure_id(structure)?;
    let displacing: Vec<&str> = others
        .into_iter()
        .filter(|other| *other != structure && !is_ported(generator, other))
        .filter(|other| {
            registry.structure_id(other).is_some_and(|other| {
                registry.sets().iter().any(|set| {
                    set.structures.iter().any(|(s, _)| *s == id)
                        && set.structures.iter().any(|(s, _)| *s == other)
                })
            })
        })
        .collect();
    (!displacing.is_empty()).then(|| {
        format!(
            "Vanilla generated {} of the same structure set there (not ported in this tree)",
            displacing.join(", ")
        )
    })
}

/// Mismatching items named per structure before the rest is counted.
const MAX_ITEMS: usize = 10;

// -------------------------------------------------------------------------------------------
// Golden access and generators

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

fn biome_source(dimension: &str) -> Value {
    match dimension {
        "minecraft:nether" => {
            json!({"type": "minecraft:multi_noise", "preset": "minecraft:nether"})
        }
        "minecraft:end" => json!({"type": "minecraft:the_end"}),
        _ => json!({"type": "minecraft:multi_noise", "preset": "minecraft:overworld"}),
    }
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
                let source = biome_source(dimension);
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

/// `VANILLITE_STRUCTURE`, qualified.
fn structure_filter() -> Option<String> {
    std::env::var("VANILLITE_STRUCTURE").ok().map(|f| {
        if f.contains(':') {
            f
        } else {
            format!("minecraft:{f}")
        }
    })
}

fn selected(filter: &Option<String>, structure: &str) -> bool {
    filter.as_deref().is_none_or(|f| f == structure)
}

/// The registry name of a structure id.
fn structure_name(
    generator: &TerrainGenerator,
    id: vanillite_worldgen::structure::StructureId,
) -> &str {
    &generator.structures().structure(id).name
}

/// Whether the structure's type is ported (not a stub of `kinds::unported`); unknown names
/// count as ported so that they show up as plain failures.
fn is_ported(generator: &TerrainGenerator, name: &str) -> bool {
    generator
        .structures()
        .structure_id(name)
        .is_none_or(|id| generator.structures().structure(id).kind.is_ported())
}

/// Chunks after `Stage::StructureStarts`, generated fresh on demand.
struct Starts<'g> {
    generator: &'g TerrainGenerator,
    chunks: HashMap<ChunkPos, ProtoChunk>,
}

impl<'g> Starts<'g> {
    fn new(generator: &'g TerrainGenerator) -> Self {
        Self {
            generator,
            chunks: HashMap::new(),
        }
    }

    /// Generates the starts of every chunk within `radius` of `center` that has none yet.
    fn ensure(&mut self, center: ChunkPos, radius: i32) {
        let mut batch = Vec::new();
        for dz in -radius..=radius {
            for dx in -radius..=radius {
                let pos = ChunkPos::new(center.x + dx, center.z + dz);
                if !self.chunks.contains_key(&pos) {
                    batch.push(self.generator.new_chunk(pos));
                }
            }
        }
        self.generator.generate_structure_starts(&mut batch);
        for chunk in batch {
            self.chunks.insert(chunk.pos(), chunk);
        }
    }

    fn get(&self, pos: ChunkPos) -> &ProtoChunk {
        self.chunks
            .get(&pos)
            .unwrap_or_else(|| panic!("no structure starts generated for {pos:?}"))
    }

    /// A copy of the chunk at `pos` after `createReferences` (starts within 8 must exist).
    fn with_references(&self, pos: ChunkPos) -> ProtoChunk {
        let mut chunk = self.get(pos).clone();
        let starts = NeighbourStarts::collect(pos, |dx, dz| {
            self.get(ChunkPos::new(pos.x + dx, pos.z + dz))
        });
        self.generator
            .generate_structure_references(&mut chunk, &starts);
        chunk
    }

    /// `StructureManager.startsForStructure` over the chunk's references.
    fn referenced(&self, chunk: &ProtoChunk) -> ReferencedStarts {
        ReferencedStarts::collect(chunk, |p| self.get(p))
    }
}

// -------------------------------------------------------------------------------------------
// Summary

/// Results of one structure id.
#[derive(Default)]
struct Tally {
    total: usize,
    matched: usize,
    /// Mismatching items with their differences.
    failures: Vec<(String, Vec<String>)>,
    /// A note on why the item may differ (unported structures nearby).
    notes: Vec<String>,
}

/// Per structure id results of one test, printed as a summary.
struct Summary {
    /// What is counted ("starts", "chunk reference lists", ...).
    unit: &'static str,
    ported: BTreeMap<String, Tally>,
    unported: BTreeMap<String, Tally>,
    /// Failures not tied to one structure (ring positions, origin, ...).
    general: Vec<String>,
}

impl Summary {
    fn new(unit: &'static str) -> Self {
        Self {
            unit,
            ported: BTreeMap::new(),
            unported: BTreeMap::new(),
            general: Vec::new(),
        }
    }

    /// Records one compared item of `structure`: `diffs` empty means it matches. `ported` is
    /// false when the structure (or one that reaches the item) is not ported in this tree;
    /// `note` says why.
    fn record(
        &mut self,
        structure: &str,
        ported: bool,
        item: String,
        diffs: Vec<String>,
        note: Option<String>,
    ) {
        let map = if ported {
            &mut self.ported
        } else {
            &mut self.unported
        };
        let tally = map.entry(structure.to_owned()).or_default();
        tally.total += 1;
        if diffs.is_empty() {
            tally.matched += 1;
        } else {
            tally.failures.push((item, diffs));
            if let Some(note) = note
                && !tally.notes.contains(&note)
            {
                tally.notes.push(note);
            }
        }
    }

    fn failures(map: &BTreeMap<String, Tally>) -> usize {
        map.values().map(|t| t.failures.len()).sum()
    }

    fn write_section(
        &self,
        out: &mut String,
        title: &str,
        map: &BTreeMap<String, Tally>,
        details: bool,
    ) {
        if map.is_empty() {
            return;
        }
        let total: usize = map.values().map(|t| t.total).sum();
        let matched: usize = map.values().map(|t| t.matched).sum();
        out.push_str(&format!(
            "\n== {title}: {matched} of {total} {} match\n",
            self.unit
        ));
        for (name, tally) in map {
            out.push_str(&format!(
                "  {name}: {} of {} {} match",
                tally.matched, tally.total, self.unit
            ));
            if tally.failures.is_empty() {
                out.push('\n');
                continue;
            }
            let items: Vec<&str> = tally
                .failures
                .iter()
                .take(MAX_ITEMS)
                .map(|(i, _)| i.as_str())
                .collect();
            let more = tally.failures.len().saturating_sub(MAX_ITEMS);
            let more = if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            };
            out.push_str(&format!("; mismatching: {}{more}\n", items.join(", ")));
            for note in &tally.notes {
                out.push_str(&format!("      note: {note}\n"));
            }
            let shown = if details { tally.failures.len() } else { 1 };
            for (item, diffs) in tally.failures.iter().take(shown) {
                out.push_str(&format!("      {item}: first difference: {}\n", diffs[0]));
                if details {
                    for diff in diffs.iter().skip(1).take(max_diffs() - 1) {
                        out.push_str(&format!("          {diff}\n"));
                    }
                    if diffs.len() > max_diffs() {
                        out.push_str(&format!(
                            "          ... {} more\n",
                            diffs.len() - max_diffs()
                        ));
                    }
                }
            }
        }
    }

    /// Prints the summary and fails if anything differs.
    fn finish(self, what: &str) {
        let mut out = what.to_owned();
        self.write_section(&mut out, "ported in this tree", &self.ported, true);
        self.write_section(
            &mut out,
            "not ported in this tree (stub in structure/kinds/unported.rs, or reached by one)",
            &self.unported,
            // All details when one structure is checked.
            structure_filter().is_some(),
        );
        if !self.general.is_empty() {
            out.push_str("\n== other differences\n");
            for line in &self.general {
                out.push_str(&format!("  {line}\n"));
            }
        }
        let (ported, unported) = (Self::failures(&self.ported), Self::failures(&self.unported));
        out.push_str(&format!(
            "\n{what}: {ported} mismatching {} of ported structures, {unported} of structures not ported in this tree, {} other differences\n",
            self.unit,
            self.general.len()
        ));
        println!("{out}");
        assert!(
            self.ported.values().map(|t| t.total).sum::<usize>()
                + self.unported.values().map(|t| t.total).sum::<usize>()
                > 0
                || !self.general.is_empty(),
            "{what}: nothing compared (check VANILLITE_STRUCTURE)"
        );
        assert!(
            ported == 0 && unported == 0 && self.general.is_empty(),
            "{what}: {ported} mismatching {unit} of ported structures, {unported} of structures not ported in this tree, {} other differences (see the summary above)",
            self.general.len(),
            unit = self.unit,
        );
    }
}

/// The message of a caught panic.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic".to_owned())
}

// -------------------------------------------------------------------------------------------
// NBT and pieces

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

/// Differences between an NBT value and its JSON form in a golden (`StructureGoldens.nbt`):
/// numbers compare in the tag's type (floats as `f32`), compounds key by key. With `lenient`,
/// keys whose value is empty on one side and absent on the other are ignored.
fn nbt_diff(path: &str, tag: &Tag, golden: &Value, lenient: bool, out: &mut Vec<String>) {
    let same = match tag {
        Tag::Byte(v) => golden.as_i64() == Some(i64::from(*v)),
        Tag::Short(v) => golden.as_i64() == Some(i64::from(*v)),
        Tag::Int(v) => golden.as_i64() == Some(i64::from(*v)),
        Tag::Long(v) => golden.as_i64() == Some(*v),
        Tag::Float(v) => golden.as_f64().is_some_and(|g| (g as f32) == *v),
        Tag::Double(v) => golden.as_f64() == Some(*v),
        Tag::String(v) => golden.as_str() == Some(v.as_str()),
        Tag::ByteArray(v) => {
            list(golden).iter().map(Value::as_i64).collect::<Vec<_>>()
                == v.iter().map(|&b| Some(i64::from(b))).collect::<Vec<_>>()
                && golden.is_array()
        }
        Tag::IntArray(v) => {
            list(golden).iter().map(Value::as_i64).collect::<Vec<_>>()
                == v.iter().map(|&b| Some(i64::from(b))).collect::<Vec<_>>()
                && golden.is_array()
        }
        Tag::LongArray(v) => {
            list(golden).iter().map(Value::as_i64).collect::<Vec<_>>()
                == v.iter().map(|&b| Some(b)).collect::<Vec<_>>()
                && golden.is_array()
        }
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
        out.push(format!("{path}: {}, expected {golden}", tag_text(tag)));
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
            None => out.push(format!(
                "{path}{sep}{key}: {} not expected",
                tag_text(value)
            )),
        }
    }
    for (key, g) in expected {
        let present = compound.get(key).is_some() && !skip.contains(&key.as_str());
        if !present && !(lenient && is_empty_json(g)) {
            out.push(format!("{path}{sep}{key}: missing, expected {g}"));
        }
    }
}

fn tag_text(tag: &Tag) -> String {
    match tag {
        Tag::String(s) => format!("{s:?}"),
        Tag::Compound(_) => "compound".to_owned(),
        Tag::List(l) => format!("list of {}", l.0.len()),
        other => format!("{other:?}"),
    }
}

/// Differences of one piece to its golden entry
/// `[type id, bb, orientation, gen_depth, rotation, mirror, data]`.
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
    // Vanilla's `null` rotation/mirror (pieces that never got an orientation) is `NONE` here.
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
        None => out.push(
            "piece NBT not written (StructurePiece::additional_save_data not implemented)"
                .to_owned(),
        ),
    }
}

/// Differences of a start to its golden entry.
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

// -------------------------------------------------------------------------------------------
// Part 1: starts

#[test]
fn starts_match_vanilla() {
    let golden = common::golden("structure_starts");
    let filter = structure_filter();
    let mut generators = Generators::default();
    let mut summary = Summary::new("starts");

    // Golden starts by world and chunk.
    let mut expected: HashMap<(usize, ChunkPos), Vec<&Value>> = HashMap::new();
    for start in list(&golden["starts"]) {
        expected
            .entry((int(&start["w"]) as usize, chunk_pos(&start["chunk"])))
            .or_default()
            .push(start);
    }

    for (w, world) in list(&golden["worlds"]).iter().enumerate() {
        let generator = generators.get(world);
        let label = format!(
            "world {w} ({} seed {})",
            world["dimension"].as_str().unwrap_or_default(),
            int(&world["seed"])
        );
        let state = StructureState::new(generator);

        // `getDimensionOrigin()`.
        let origin = state.dimension_origin();
        if json!([origin.x, origin.z]) != world["origin"] {
            summary.general.push(format!(
                "{label}: dimension origin [{}, {}], expected {}",
                origin.x, origin.z, world["origin"]
            ));
        }

        // `getRingPositionsFor` of the (last) concentric rings placement among the possible sets.
        if selected(&filter, "minecraft:stronghold") {
            let rings = generator
                .structures()
                .possible_sets()
                .iter()
                .filter_map(|&id| match &generator.structures().set(id).placement {
                    StructurePlacement::ConcentricRings { rings, .. } => Some(rings),
                    _ => None,
                })
                .last();
            let positions = rings.and_then(|rings| state.ring_positions_for(rings));
            match (positions, world["stronghold_rings"].as_array()) {
                (None, None) => {}
                (Some(actual), Some(golden_rings)) => {
                    let matching = actual
                        .iter()
                        .zip(golden_rings)
                        .filter(|(p, g)| ChunkPos::new(p.x, p.z) == chunk_pos(g))
                        .count();
                    if matching != golden_rings.len() || actual.len() != golden_rings.len() {
                        let first = actual
                            .iter()
                            .zip(golden_rings)
                            .position(|(p, g)| *p != chunk_pos(g));
                        summary.general.push(format!(
                            "{label}: stronghold ring positions: {matching} of {} match ({} computed); first difference at index {first:?}",
                            golden_rings.len(),
                            actual.len()
                        ));
                    } else {
                        println!(
                            "{label}: stronghold ring positions: {matching} of {} match",
                            golden_rings.len()
                        );
                    }
                }
                (actual, _) => summary.general.push(format!(
                    "{label}: stronghold ring positions {}, expected {}",
                    if actual.is_some() {
                        "present"
                    } else {
                        "absent"
                    },
                    world["stronghold_rings"]
                )),
            }
        }

        // The area (z outer, x inner), then the targeted chunks.
        let area = &world["area"];
        let (min_x, min_z) = (int(&area[0]) as i32, int(&area[1]) as i32);
        let (size_x, size_z) = (int(&area[2]) as i32, int(&area[3]) as i32);
        let mut order: Vec<ChunkPos> = Vec::new();
        for z in min_z..min_z + size_z {
            for x in min_x..min_x + size_x {
                order.push(ChunkPos::new(x, z));
            }
        }
        order.extend(list(&world["chunks"]).iter().map(chunk_pos));

        for pos in order {
            let mut chunk = generator.new_chunk(pos);
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                generator.generate_structure_starts(std::slice::from_mut(&mut chunk));
            }));
            let item = format!("w{w} chunk [{}, {}]", pos.x, pos.z);
            if let Err(payload) = outcome {
                summary.general.push(format!(
                    "{item}: createStructures panicked: {}",
                    panic_message(&*payload)
                ));
                continue;
            }
            let mut actual: BTreeMap<&str, &Arc<StructureStart>> = chunk
                .structure_starts()
                .iter()
                .map(|(id, start)| (structure_name(generator, *id), start))
                .collect();
            for golden_start in expected.get(&(w, pos)).map_or(&[][..], Vec::as_slice) {
                let name = golden_start["structure"].as_str().expect("structure");
                if !selected(&filter, name) {
                    actual.remove(name);
                    continue;
                }
                let diffs = match actual.remove(name) {
                    Some(start) => start_diff(start, golden_start),
                    None => vec!["no start generated".to_owned()],
                };
                summary.record(name, is_ported(generator, name), item.clone(), diffs, None);
            }
            let golden_names = expected
                .get(&(w, pos))
                .map_or(&[][..], Vec::as_slice)
                .iter()
                .filter_map(|start| start["structure"].as_str());
            let golden_names: Vec<&str> = golden_names.collect();
            for (name, _) in actual {
                if selected(&filter, name) {
                    let note = displaced_by_unported(generator, name, golden_names.iter().copied());
                    summary.record(
                        name,
                        is_ported(generator, name) && note.is_none(),
                        item.clone(),
                        vec!["start generated where Vanilla has none".to_owned()],
                        note,
                    );
                }
            }
        }
    }
    summary.finish("structure starts");
}

// -------------------------------------------------------------------------------------------
// Part 2: references

/// The chunk's references as structure id → start chunks (set order), sorted by id.
fn references_of(
    generator: &TerrainGenerator,
    chunk: &ProtoChunk,
) -> BTreeMap<String, Vec<ChunkPos>> {
    chunk
        .structure_references()
        .iter()
        .map(|(id, set)| {
            (
                structure_name(generator, *id).to_owned(),
                set.iter().map(ChunkPos::from_long).collect(),
            )
        })
        .collect()
}

fn golden_references(v: &Value) -> BTreeMap<String, Vec<ChunkPos>> {
    v.as_object()
        .map(|o| {
            o.iter()
                .map(|(k, v)| (k.clone(), list(v).iter().map(chunk_pos).collect()))
                .collect()
        })
        .unwrap_or_default()
}

fn positions_text(list: &[ChunkPos]) -> String {
    let items: Vec<String> = list.iter().map(|p| format!("[{}, {}]", p.x, p.z)).collect();
    format!("[{}]", items.join(", "))
}

/// Records the per structure comparison of a chunk's references. `golden_starts` gives the
/// structures Vanilla started in a chunk (`structure_starts.json`).
fn record_references<'g>(
    summary: &mut Summary,
    generator: &TerrainGenerator,
    filter: &Option<String>,
    item: &str,
    actual: &BTreeMap<String, Vec<ChunkPos>>,
    expected: &BTreeMap<String, Vec<ChunkPos>>,
    golden_starts: &dyn Fn(ChunkPos) -> Vec<&'g str>,
) {
    let mut names: Vec<&String> = actual.keys().chain(expected.keys()).collect();
    names.sort();
    names.dedup();
    for name in names {
        if !selected(filter, name) {
            continue;
        }
        let (a, e) = (
            actual.get(name).map_or(&[][..], Vec::as_slice),
            expected.get(name).map_or(&[][..], Vec::as_slice),
        );
        let diffs = if a == e {
            Vec::new()
        } else {
            vec![format!(
                "references {}, expected {}",
                positions_text(a),
                positions_text(e)
            )]
        };
        // Start chunks on one side only: did an unported structure of the set start there?
        let note = if diffs.is_empty() {
            None
        } else {
            let others = a
                .iter()
                .filter(|p| !e.contains(p))
                .chain(e.iter().filter(|p| !a.contains(p)))
                .flat_map(|&p| golden_starts(p));
            displaced_by_unported(generator, name, others)
        };
        summary.record(
            name,
            is_ported(generator, name) && note.is_none(),
            item.to_owned(),
            diffs,
            note,
        );
    }
}

#[test]
fn references_match_vanilla() {
    let golden = common::golden("structure_references");
    let starts_golden = common::golden("structure_starts");
    // Vanilla's starts by dimension, seed and chunk.
    let mut golden_starts: HashMap<(&str, i64, ChunkPos), Vec<&str>> = HashMap::new();
    let worlds = list(&starts_golden["worlds"]);
    for start in list(&starts_golden["starts"]) {
        let world = &worlds[int(&start["w"]) as usize];
        golden_starts
            .entry((
                world["dimension"].as_str().unwrap_or_default(),
                int(&world["seed"]),
                chunk_pos(&start["chunk"]),
            ))
            .or_default()
            .push(start["structure"].as_str().unwrap_or_default());
    }
    let filter = structure_filter();
    let mut generators = Generators::default();
    let mut summary = Summary::new("chunk reference lists");
    let areas = list(&golden["areas"]);
    let mut starts: Vec<Starts<'_>> = Vec::new();
    // Generators first (the starts borrow them).
    for area in areas {
        generators.get(area);
    }
    for area in areas {
        let dimension = area["dimension"].as_str().unwrap_or_default();
        let generator = &generators.0[&(dimension.to_owned(), int(&area["seed"]))];
        let mut cache = Starts::new(generator);
        let a = &area["area"];
        let (min_x, min_z) = (int(&a[0]) as i32, int(&a[1]) as i32);
        let (size_x, size_z) = (int(&a[2]) as i32, int(&a[3]) as i32);
        let center = ChunkPos::new(min_x + size_x / 2, min_z + size_z / 2);
        // Every chunk of the area reads the starts within 8.
        cache.ensure(center, size_x.max(size_z) / 2 + 9);
        starts.push(cache);
    }
    for chunk in list(&golden["chunks"]) {
        let a = int(&chunk["a"]) as usize;
        let cache = &starts[a];
        let pos = chunk_pos(&chunk["chunk"]);
        let item = format!("a{a} chunk [{}, {}]", pos.x, pos.z);
        let actual = references_of(cache.generator, &cache.with_references(pos));
        let area = &areas[a];
        let world = (
            area["dimension"].as_str().unwrap_or_default(),
            int(&area["seed"]),
        );
        let starts_at = |p: ChunkPos| {
            golden_starts
                .get(&(world.0, world.1, p))
                .cloned()
                .unwrap_or_default()
        };
        record_references(
            &mut summary,
            cache.generator,
            &filter,
            &item,
            &actual,
            &golden_references(&chunk["references"]),
            &starts_at,
        );
    }
    summary.finish("structure references");
}

// -------------------------------------------------------------------------------------------
// Part 3: terrain with the Beardifier

/// Differences between a chunk and a `chunk.json` style snapshot (section hashes, the two
/// worldgen heightmaps, post-processing marks).
fn terrain_snapshot_diff(chunk: &ProtoChunk, golden: &Value, out: &mut Vec<String>, step: &str) {
    for (index, (section, expected)) in chunk
        .sections()
        .iter()
        .zip(list(&golden["sections"]))
        .enumerate()
    {
        if section_hash(section) != int(expected) {
            out.push(format!(
                "after {step}: section {index} (y {}) differs",
                chunk.min_y() + index as i32 * 16
            ));
        }
    }
    for (map, key) in [
        (WgHeightmap::WorldSurface, "world_surface_wg"),
        (WgHeightmap::OceanFloor, "ocean_floor_wg"),
    ] {
        let mut hash = 0_i64;
        for z in 0..16 {
            for x in 0..16 {
                hash = hash
                    .wrapping_mul(31)
                    .wrapping_add(i64::from(chunk.height_at(map, x, z)));
            }
        }
        if hash != int(&golden[key]) {
            out.push(format!("after {step}: heightmap {key} differs"));
        }
    }
    let marks = post_processing(chunk);
    for (index, (actual, expected)) in marks
        .iter()
        .zip(list(&golden["post_processing"]))
        .enumerate()
    {
        if *actual != (int(&expected[0]), int(&expected[1])) {
            out.push(format!(
                "after {step}: post-processing of section {index}: {} marks, expected {}",
                actual.0, expected[0]
            ));
        }
    }
}

fn section_hash(section: &vanillite_chunk::Section) -> i64 {
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
}

/// Per section: number of post-processing marks and their hash.
fn post_processing(chunk: &ProtoChunk) -> Vec<(i64, i64)> {
    let mut lists = vec![(0_i64, 0_i64); chunk.sections().len()];
    for &(x, y, z) in chunk.post_processing() {
        let packed = ((x & 15) | ((y & 15) << 4) | ((z & 15) << 8)) as i16;
        let entry = &mut lists[chunk.section_index(y)];
        entry.0 += 1;
        entry.1 = entry.1.wrapping_mul(31).wrapping_add(i64::from(packed));
    }
    lists
}

/// Differences of the chunk's `Beardifier` to the golden's `beardifier` member.
fn beardifier_diff(
    generator: &TerrainGenerator,
    starts: &ReferencedStarts,
    pos: ChunkPos,
    golden: &Value,
    out: &mut Vec<String>,
) {
    let registry = generator.structures();
    let mut adapted: Vec<Value> = starts
        .all()
        .iter()
        .filter(|(id, _)| registry.structure(*id).terrain_adaptation() != TerrainAdjustment::None)
        .map(|(id, start)| {
            json!([
                structure_name(generator, *id),
                start.chunk_pos().x,
                start.chunk_pos().z
            ])
        })
        .collect();
    adapted.sort_by_key(|v| {
        (
            v[0].as_str().unwrap_or_default().to_owned(),
            int(&v[1]),
            int(&v[2]),
        )
    });
    if Value::Array(adapted.clone()) != golden["starts"] {
        out.push(format!(
            "beardifier starts {}, expected {}",
            Value::Array(adapted),
            golden["starts"]
        ));
    }
    let beardifier = Beardifier::for_structures_in_chunk(registry, starts, pos);
    let (rigids, junctions, affected) = beardifier.as_ref().map_or((0, 0, Value::Null), |b| {
        (
            b.rigid_count(),
            b.junction_count(),
            box_json(&b.affected_box()),
        )
    });
    let actual = json!({"rigids": rigids, "junctions": junctions, "affected_box": affected});
    for key in ["rigids", "junctions", "affected_box"] {
        if actual[key] != golden[key] {
            out.push(format!(
                "beardifier {key} {}, expected {}",
                actual[key], golden[key]
            ));
        }
    }
}

/// Names of the unported structures among `names`.
fn unported_among<'a>(
    generator: &TerrainGenerator,
    names: impl IntoIterator<Item = &'a String>,
) -> Vec<String> {
    let mut out: Vec<String> = names
        .into_iter()
        .filter(|n| !is_ported(generator, n))
        .cloned()
        .collect();
    out.sort();
    out.dedup();
    out
}

#[test]
fn terrain_matches_vanilla() {
    let golden = common::golden("structure_terrain");
    let filter = structure_filter();
    let mut generators = Generators::default();
    let cases = list(&golden["chunks"]);
    for case in cases {
        generators.get(case);
    }
    let mut summary = Summary::new("terrain chunks");
    // Starts per world, shared by the targets like the Java pipelines (terrain does not change
    // the starts).
    let mut caches: HashMap<(String, i64), Starts<'_>> = HashMap::new();
    for case in cases {
        let structure = case["structure"].as_str().expect("structure");
        if !selected(&filter, structure) {
            continue;
        }
        let key = (
            case["dimension"].as_str().unwrap_or_default().to_owned(),
            int(&case["seed"]),
        );
        let generator = &generators.0[&key];
        let cache = caches.entry(key).or_insert_with(|| Starts::new(generator));
        let pos = ChunkPos::new(int(&case["x"]) as i32, int(&case["z"]) as i32);
        let item = format!("chunk [{}, {}]", pos.x, pos.z);
        cache.ensure(pos, 9);

        let mut diffs = Vec::new();
        let target = cache.with_references(pos);
        let expected_references = golden_references(&case["references"]);
        let actual_references = references_of(generator, &target);
        if actual_references != expected_references {
            for (name, starts) in &expected_references {
                let actual = actual_references.get(name).map_or(&[][..], Vec::as_slice);
                if actual != starts.as_slice() {
                    diffs.push(format!(
                        "references of {name} {}, expected {}",
                        positions_text(actual),
                        positions_text(starts)
                    ));
                }
            }
            for (name, starts) in &actual_references {
                if !expected_references.contains_key(name) {
                    diffs.push(format!(
                        "references of {name} {}, expected none",
                        positions_text(starts)
                    ));
                }
            }
        }
        let referenced = cache.referenced(&target);
        beardifier_diff(generator, &referenced, pos, &case["beardifier"], &mut diffs);

        // Biomes of the 3×3 chunks (the target keeps its references).
        let mut area: Vec<ProtoChunk> = (0..9)
            .map(|i| {
                let p = ChunkPos::new(pos.x + i % 3 - 1, pos.z + i / 3 - 1);
                if p == pos {
                    target.clone()
                } else {
                    cache.get(p).clone()
                }
            })
            .collect();
        generator.generate_biomes(&mut area);
        let neighbours = NeighbourBiomes::collect(|dx, dz| &area[((dz + 1) * 3 + dx + 1) as usize]);
        let mut job = TerrainJob {
            chunk: area.swap_remove(4),
            neighbours,
            structures: referenced,
        };
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            let mut step_diffs = Vec::new();
            generator.generate_terrain_observed(&mut job, |step, chunk| {
                let key = match step {
                    TerrainStep::Noise => "noise",
                    TerrainStep::Surface => "surface",
                    TerrainStep::Carvers => "carvers",
                };
                terrain_snapshot_diff(chunk, &case[key], &mut step_diffs, key);
            });
            step_diffs
        }));
        match outcome {
            Ok(step_diffs) => diffs.extend(step_diffs),
            Err(payload) => diffs.push(format!("terrain panicked: {}", panic_message(&*payload))),
        }

        let unported = unported_among(
            generator,
            expected_references
                .keys()
                .chain(std::iter::once(&structure.to_owned())),
        );
        let note = (!unported.is_empty()).then(|| {
            format!(
                "the chunk references unported structures: {}",
                unported.join(", ")
            )
        });
        summary.record(structure, unported.is_empty(), item, diffs, note);
    }
    summary.finish("structure terrain");
}

// -------------------------------------------------------------------------------------------
// Part 4: decoration with structures

/// The recorded heightmaps, in golden order.
const HEIGHTMAPS: [(HeightmapKind, &str); 4] = [
    (HeightmapKind::WorldSurface, "WORLD_SURFACE"),
    (HeightmapKind::OceanFloor, "OCEAN_FLOOR"),
    (HeightmapKind::MotionBlocking, "MOTION_BLOCKING"),
    (
        HeightmapKind::MotionBlockingNoLeaves,
        "MOTION_BLOCKING_NO_LEAVES",
    ),
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

/// Differences of a decorated chunk to its golden entry (`feature.json` decoration format plus
/// block entities with NBT).
fn decorated_chunk_diff(chunk: &ProtoChunk, golden: &Value, out: &mut Vec<String>) {
    let pos = chunk.pos();
    let at = format!("chunk [{}, {}]", pos.x, pos.z);
    for (index, (section, expected)) in chunk
        .sections()
        .iter()
        .zip(list(&golden["sections"]))
        .enumerate()
    {
        if section_hash(section) != int(expected) {
            out.push(format!(
                "{at} section {index} (y {}): blocks differ",
                chunk.min_y() + index as i32 * 16
            ));
        }
    }
    for (i, (kind, name)) in HEIGHTMAPS.iter().enumerate() {
        let mut hash = 0_i64;
        for z in 0..16 {
            for x in 0..16 {
                hash = hash
                    .wrapping_mul(31)
                    .wrapping_add(i64::from(chunk.heightmap(*kind, x, z)));
            }
        }
        if hash != int(&golden["heightmaps"][i]) {
            out.push(format!("{at}: heightmap {name} differs"));
        }
    }
    for (index, (actual, expected)) in post_processing(chunk)
        .iter()
        .zip(list(&golden["post_processing"]))
        .enumerate()
    {
        if *actual != (int(&expected[0]), int(&expected[1])) {
            out.push(format!(
                "{at} section {index}: post-processing marks differ ({} marks, expected {})",
                actual.0, expected[0]
            ));
        }
    }
    let pair = |v: &Value| (int(&v[0]), int(&v[1]));
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

/// Block entities: positions and type ids, then the NBT where the golden has it
/// (`saveWithoutMetadata`; empty default compounds and lists of Vanilla are not required).
fn block_entities_diff(chunk: &ProtoChunk, golden: &[Value], at: &str, out: &mut Vec<String>) {
    let mut actual: Vec<(BlockPos, &Compound)> = chunk
        .block_entities()
        .iter()
        .map(|(p, t)| (*p, t))
        .collect();
    actual.sort_by_key(|(p, _)| (p.y, p.z, p.x));
    let id = |tag: &Compound| match tag.get("id") {
        Some(Tag::String(id)) => id.clone(),
        _ => String::new(),
    };
    let mut expected: HashMap<BlockPos, &Value> = golden
        .iter()
        .map(|e| {
            (
                BlockPos::new(int(&e[0]) as i32, int(&e[1]) as i32, int(&e[2]) as i32),
                e,
            )
        })
        .collect();
    for (pos, tag) in &actual {
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
            continue;
        }
        if !e[4].is_null() {
            let mut diffs = Vec::new();
            compound_diff(
                "",
                tag,
                &e[4],
                true,
                &["id", "x", "y", "z", "keepPacked"],
                &mut diffs,
            );
            out.extend(
                diffs
                    .into_iter()
                    .map(|d| format!("{at_pos} ({expected_id}): {d}")),
            );
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

/// Unported structures that may reach the decorated area: for chunks within `radius` of
/// `center` whose placement selects a set with an unported structure that may have a valid
/// biome in that chunk's middle column. Vanillite has no starts for them, so it cannot know
/// their extent; this is only used to label mismatches.
fn unported_nearby(generator: &TerrainGenerator, center: ChunkPos, radius: i32) -> Vec<String> {
    let registry = generator.structures();
    let state = StructureState::new(generator);
    let level = LevelHeight {
        min_y: generator.min_y(),
        height: generator.gen_depth(),
    };
    let mut found = Vec::new();
    for &set_id in registry.possible_sets() {
        let set = registry.set(set_id);
        let unported: Vec<_> = set
            .structures
            .iter()
            .filter(|(id, _)| !registry.structure(*id).kind.is_ported())
            .collect();
        if unported.is_empty() {
            continue;
        }
        for dz in -radius..=radius {
            for dx in -radius..=radius {
                let pos = ChunkPos::new(center.x + dx, center.z + dz);
                if !set.placement.is_structure_chunk(&state, pos.x, pos.z) {
                    continue;
                }
                for (id, _) in &unported {
                    let structure = registry.structure(*id);
                    // Mineshafts stay within 80 blocks of their start; the rest within the
                    // reference range.
                    let reach = if matches!(structure.kind, StructureKind::Mineshaft(_)) {
                        6
                    } else {
                        radius
                    };
                    if dx.abs() > reach + 2 || dz.abs() > reach + 2 {
                        continue;
                    }
                    let ctx =
                        GenerationContext::new(generator, pos, level, &structure.settings.biomes);
                    let (x, z) = (pos.min_block_x() + 8, pos.min_block_z() + 8);
                    if ctx.could_structure_exist_in_column(x, z, level.min_y, level.max_y()) {
                        found.push(format!("{} at [{}, {}]", structure.name, pos.x, pos.z));
                    }
                }
            }
        }
    }
    found
}

/// An entity added during decoration: decorated chunk index (0–8), type id and position.
#[derive(Clone, Debug, PartialEq, PartialOrd)]
struct Entity {
    index: i64,
    kind: String,
    pos: [f64; 3],
}

impl Entity {
    fn of(index: i64, tag: &Compound) -> Self {
        let kind = match tag.get("id") {
            Some(Tag::String(id)) => id.clone(),
            _ => String::new(),
        };
        Self {
            index,
            kind,
            pos: entity_pos(tag),
        }
    }

    fn from_golden(v: &Value) -> Self {
        let f = |v: &Value| v.as_f64().expect("entity coordinate");
        Self {
            index: int(&v[0]),
            kind: v[1].as_str().unwrap_or_default().to_owned(),
            pos: [f(&v[2]), f(&v[3]), f(&v[4])],
        }
    }

    fn text(&self) -> String {
        format!(
            "{} at {} {} {} (decorating chunk {})",
            self.kind, self.pos[0], self.pos[1], self.pos[2], self.index
        )
    }
}

/// Entities by decorated chunk, type and position (within one decorated chunk the order of
/// Vanilla's `addFreshEntity` calls is not compared: Vanillite stores entities per chunk).
fn entities_diff(actual: &[Entity], golden: &[Value], out: &mut Vec<String>) {
    let sort = |list: &mut Vec<Entity>| {
        list.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    };
    let mut actual = actual.to_vec();
    let mut expected: Vec<Entity> = golden.iter().map(Entity::from_golden).collect();
    sort(&mut actual);
    sort(&mut expected);
    for e in &expected {
        if let Some(i) = actual.iter().position(|a| a == e) {
            actual.remove(i);
        } else {
            out.push(format!("entity {} not added", e.text()));
        }
    }
    for a in actual {
        out.push(format!("entity {} added, Vanilla has none", a.text()));
    }
}

/// The chunks of `chunks` (a row-major area) within one chunk of `center`, row-major.
fn region_chunks(chunks: &mut [ProtoChunk], center: ChunkPos) -> Vec<&mut ProtoChunk> {
    chunks
        .iter_mut()
        .filter(|c| (c.pos().x - center.x).abs() <= 1 && (c.pos().z - center.z).abs() <= 1)
        .collect()
}

#[test]
fn decoration_matches_vanilla() {
    let golden = common::golden("structure_decoration");
    let filter = structure_filter();
    let mut generators = Generators::default();
    let mut summary = Summary::new("decorated areas");
    for area in list(&golden["areas"]) {
        let structure = area["structure"].as_str().expect("structure");
        if !selected(&filter, structure) {
            continue;
        }
        let generator = generators.get(area);
        let center = ChunkPos::new(int(&area["x"]) as i32, int(&area["z"]) as i32);
        let item = format!("around chunk [{}, {}]", center.x, center.z);
        let mut diffs = Vec::new();

        // Fresh chunks: starts within 10, references within 2, biomes within 3, terrain
        // within 2 (row-major, z outer), then the features of the 3×3 chunks in row-major
        // order.
        let mut starts = Starts::new(generator);
        starts.ensure(center, 10);
        let mut biomes: Vec<ProtoChunk> = Vec::with_capacity(49);
        for dz in -3..=3 {
            for dx in -3..=3 {
                let pos = ChunkPos::new(center.x + dx, center.z + dz);
                biomes.push(if dx.abs() <= 2 && dz.abs() <= 2 {
                    starts.with_references(pos)
                } else {
                    starts.get(pos).clone()
                });
            }
        }
        generator.generate_biomes(&mut biomes);
        let at = |dx: i32, dz: i32| ((dz + 3) * 7 + dx + 3) as usize;
        let mut jobs = Vec::with_capacity(25);
        for dz in -2..=2 {
            for dx in -2..=2 {
                let chunk = biomes[at(dx, dz)].clone();
                let structures = starts.referenced(&chunk);
                let neighbours = NeighbourBiomes::collect(|ox, oz| &biomes[at(dx + ox, dz + oz)]);
                jobs.push(TerrainJob {
                    chunk,
                    neighbours,
                    structures,
                });
            }
        }
        generator.generate_terrain(&mut jobs);
        let mut chunks: Vec<ProtoChunk> = jobs.into_iter().map(|j| j.chunk).collect();

        // Entities added while decorating each of the 3×3 chunks (row-major index 0–8).
        let mut added: Vec<Entity> = Vec::new();
        'decorate: for dz in -1..=1 {
            for dx in -1..=1 {
                let pos = ChunkPos::new(center.x + dx, center.z + dz);
                let index = ((dz + 2) * 5 + dx + 2) as usize;
                let before: Vec<usize> = chunks.iter().map(|c| c.entities().len()).collect();
                let structures = starts.referenced(&chunks[index]);
                let outcome = catch_unwind(AssertUnwindSafe(|| {
                    let region: [&mut ProtoChunk; 9] = region_chunks(&mut chunks, pos)
                        .try_into()
                        .expect("nine chunks");
                    generator.generate_features(region, &structures);
                }));
                if let Err(payload) = outcome {
                    diffs.push(format!(
                        "decorating chunk [{}, {}] panicked: {}",
                        pos.x,
                        pos.z,
                        panic_message(&*payload)
                    ));
                    break 'decorate;
                }
                let decorated = ((dz + 1) * 3 + dx + 1) as i64;
                for (chunk, before) in chunks.iter().zip(&before) {
                    added.extend(
                        chunk.entities()[*before..]
                            .iter()
                            .map(|e| Entity::of(decorated, e)),
                    );
                }
            }
        }
        if diffs.is_empty() {
            for (chunk, expected) in chunks.iter().zip(list(&area["chunks"])) {
                decorated_chunk_diff(chunk, expected, &mut diffs);
            }
            entities_diff(&added, list(&area["entities"]), &mut diffs);
        }

        let mut unported = Vec::new();
        if !is_ported(generator, structure) {
            unported.push(format!("{structure} is not ported"));
        } else if !diffs.is_empty() {
            unported = unported_nearby(generator, center, 10);
        }
        let note = (!unported.is_empty()).then(|| {
            format!(
                "unported structures may reach the area: {}",
                unported.join(", ")
            )
        });
        summary.record(structure, unported.is_empty(), item, diffs, note);
    }
    summary.finish("structure decoration");
}
