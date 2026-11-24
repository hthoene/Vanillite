//! Features stage parity with Vanilla, against the goldens recorded by `FeatureGoldens.java`.
//!
//! - [`placed_features_match_vanilla`]: every placed feature of the Vanilla pack, run once (as
//!   `FeaturePlacer` runs it) on the 3×3 terrain chunks of its area. Set `VANILLITE_FEATURE`
//!   (e.g. `minecraft:ore_iron_upper`) to run one feature. Prints how many match and what
//!   differs for the others.
//! - [`decoration_matches_vanilla`]: `applyBiomeDecoration` for the 3×3 chunks around a centre in
//!   row-major order, so writes across chunk borders and their order are covered.
//!
//! Compared per chunk: block hashes per section, the four final heightmaps, post-processing
//! marks, block entities (position and id) and scheduled block and fluid ticks.

mod common;

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};

use serde_json::{Value, json};
use vanillite_base::math::java_string_hash;
use vanillite_base::random::DecorationRandom;
use vanillite_base::{BlockPos, ChunkPos};
use vanillite_chunk::HeightmapKind;
use vanillite_nbt::Tag;
use vanillite_worldgen::proto::ProtoTick;
use vanillite_worldgen::{
    DimensionSpec, NeighbourBiomes, ProtoChunk, TerrainGenerator, TerrainJob,
};

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

/// Differences listed per failing feature or area before the rest is summarised.
const MAX_DIFFS: usize = 12;

fn biome_source(dimension: &str) -> Value {
    match dimension {
        "minecraft:nether" => {
            json!({"type": "minecraft:multi_noise", "preset": "minecraft:nether"})
        }
        "minecraft:end" => json!({"type": "minecraft:the_end"}),
        _ => json!({"type": "minecraft:multi_noise", "preset": "minecraft:overworld"}),
    }
}

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
    /// The generator of a golden area (`dimension`, `seed`, `min_y`, `height`).
    fn get(&mut self, area: &Value) -> &TerrainGenerator {
        let dimension = area["dimension"].as_str().expect("dimension");
        let seed = int(&area["seed"]);
        self.0
            .entry((dimension.to_owned(), seed))
            .or_insert_with(|| {
                let source = biome_source(dimension);
                let spec = DimensionSpec {
                    noise_settings: dimension,
                    biome_source: &source,
                    min_y: int(&area["min_y"]) as i32,
                    height: int(&area["height"]) as i32,
                    has_sky_light: dimension == "minecraft:overworld",
                };
                TerrainGenerator::new(common::packs(), spec, seed).expect("generator")
            })
    }
}

/// Terrain for the chunks within `radius` of `center` (row-major, `z` outer), with biomes one
/// chunk further out.
fn terrain(generator: &TerrainGenerator, center: ChunkPos, radius: i32) -> Vec<ProtoChunk> {
    let outer = radius + 1;
    let side = 2 * outer + 1;
    let mut biomes: Vec<ProtoChunk> = (0..side * side)
        .map(|i| {
            generator.new_chunk(ChunkPos::new(
                center.x + i % side - outer,
                center.z + i / side - outer,
            ))
        })
        .collect();
    generator.generate_biomes(&mut biomes);
    let at = |dx: i32, dz: i32| ((dz + outer) * side + dx + outer) as usize;
    let mut jobs = Vec::new();
    for dz in -radius..=radius {
        for dx in -radius..=radius {
            let neighbours = NeighbourBiomes::collect(|ox, oz| &biomes[at(dx + ox, dz + oz)]);
            jobs.push(TerrainJob {
                chunk: biomes[at(dx, dz)].clone(),
                neighbours,
            });
        }
    }
    generator.generate_terrain(&mut jobs);
    jobs.into_iter().map(|j| j.chunk).collect()
}

/// The chunks of a row-major area within one chunk of `center`, row-major.
fn region_chunks(chunks: &mut [ProtoChunk], center: ChunkPos) -> Vec<&mut ProtoChunk> {
    chunks
        .iter_mut()
        .filter(|c| (c.pos().x - center.x).abs() <= 1 && (c.pos().z - center.z).abs() <= 1)
        .collect()
}

/// What the goldens record of one chunk.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Snapshot {
    sections: Vec<i64>,
    heightmaps: [i64; 4],
    /// Per section: number of marks and their hash.
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
        let heightmaps = HEIGHTMAPS.map(|(kind, _)| {
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

    /// A chunk as recorded in a golden (ticks and block entities empty unless present).
    fn from_golden(chunk: &Value) -> Self {
        let pair = |v: &Value| (int(&v[0]), int(&v[1]));
        Self {
            sections: list(&chunk["sections"]).iter().map(int).collect(),
            heightmaps: std::array::from_fn(|i| int(&chunk["heightmaps"][i])),
            post_processing: list(&chunk["post_processing"]).iter().map(pair).collect(),
            block_entities: list(&chunk["block_entities"])
                .iter()
                .map(block_entity)
                .collect(),
            block_ticks: chunk.get("block_ticks").map_or((0, 0), pair),
            fluid_ticks: chunk.get("fluid_ticks").map_or((0, 0), pair),
        }
    }

    /// Readable differences to `expected` for the chunk at `pos`.
    fn diff(&self, expected: &Self, pos: ChunkPos, min_y: i32, out: &mut Vec<String>) {
        let at = format!("chunk [{}, {}]", pos.x, pos.z);
        for (index, (a, e)) in self.sections.iter().zip(&expected.sections).enumerate() {
            if a != e {
                out.push(format!(
                    "{at} section {index} (y {}): blocks differ",
                    min_y + index as i32 * 16
                ));
            }
        }
        for (i, (_, name)) in HEIGHTMAPS.iter().enumerate() {
            if self.heightmaps[i] != expected.heightmaps[i] {
                out.push(format!("{at}: heightmap {name} differs"));
            }
        }
        for (index, (a, e)) in self
            .post_processing
            .iter()
            .zip(&expected.post_processing)
            .enumerate()
        {
            if a != e {
                out.push(format!(
                    "{at} section {index}: post-processing marks differ ({} marks, expected {})",
                    a.0, e.0
                ));
            }
        }
        if self.block_entities != expected.block_entities {
            out.push(format!(
                "{at}: block entities differ: {:?}, expected {:?}",
                self.block_entities, expected.block_entities
            ));
        }
        if self.block_ticks != expected.block_ticks {
            out.push(format!(
                "{at}: block ticks differ ({} ticks, expected {})",
                self.block_ticks.0, expected.block_ticks.0
            ));
        }
        if self.fluid_ticks != expected.fluid_ticks {
            out.push(format!(
                "{at}: fluid ticks differ ({} ticks, expected {})",
                self.fluid_ticks.0, expected.fluid_ticks.0
            ));
        }
    }
}

fn block_entity(v: &Value) -> (i32, i32, i32, String) {
    (
        int(&v[0]) as i32,
        int(&v[1]) as i32,
        int(&v[2]) as i32,
        v[3].as_str().unwrap_or_default().to_owned(),
    )
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

/// The message of a caught panic.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic".to_owned())
}

/// Prints a summary and fails if anything differs.
fn report(what: &str, total: usize, skipped: &[String], failures: &[(String, Vec<String>)]) {
    let mut text = format!(
        "{what}: {} of {} match",
        total - skipped.len() - failures.len(),
        total - skipped.len()
    );
    if !skipped.is_empty() {
        text += &format!(
            " ({} not run: Vanilla could not run them in the harness: {})",
            skipped.len(),
            skipped.join(", ")
        );
    }
    if !failures.is_empty() {
        text += &format!(
            "\nmismatching ({}): {}",
            failures.len(),
            failures
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        for (name, diffs) in failures {
            text += &format!("\n{name}:");
            for diff in diffs.iter().take(MAX_DIFFS) {
                text += &format!("\n    {diff}");
            }
            if diffs.len() > MAX_DIFFS {
                text += &format!("\n    ... {} more", diffs.len() - MAX_DIFFS);
            }
        }
    }
    println!("{text}");
    assert!(
        failures.is_empty(),
        "{} of {} {what} differ from Vanilla (see the summary above)",
        failures.len(),
        total - skipped.len()
    );
}

#[test]
fn placed_features_match_vanilla() {
    let golden = common::golden("feature");
    let filter = std::env::var("VANILLITE_FEATURE").ok().map(|f| {
        if f.contains(':') {
            f
        } else {
            format!("minecraft:{f}")
        }
    });
    let areas = list(&golden["areas"]);
    let mut generators = Generators::default();
    // Rust terrain per area, built on first use, and whether it matches the golden terrain.
    let mut terrains: Vec<Option<(Vec<ProtoChunk>, Vec<String>)>> = vec![None; areas.len()];
    let mut total = 0;
    let mut skipped = Vec::new();
    let mut failures = Vec::new();
    for feature in list(&golden["features"]) {
        let name = feature["name"].as_str().expect("name").to_owned();
        if filter.as_ref().is_some_and(|f| *f != name) {
            continue;
        }
        total += 1;
        if let Some(error) = feature.get("error") {
            skipped.push(format!("{name} ({})", error.as_str().unwrap_or_default()));
            continue;
        }
        let index = int(&feature["area"]) as usize;
        let area = &areas[index];
        let generator = generators.get(area);
        let center = ChunkPos::new(int(&area["x"]) as i32, int(&area["z"]) as i32);
        let min_y = int(&area["min_y"]) as i32;
        let expected_terrain: Vec<Snapshot> = list(&area["chunks"])
            .iter()
            .map(Snapshot::from_golden)
            .collect();
        let (chunks, terrain_diffs) = terrains[index].get_or_insert_with(|| {
            let chunks = terrain(generator, center, 1);
            let mut diffs = Vec::new();
            for (chunk, expected) in chunks.iter().zip(&expected_terrain) {
                Snapshot::of(chunk).diff(expected, chunk.pos(), min_y, &mut diffs);
            }
            (
                chunks,
                diffs.into_iter().map(|d| format!("terrain: {d}")).collect(),
            )
        });

        let Some(id) = generator.features().env.registry.placed_id(&name) else {
            failures.push((name, vec!["not in the feature registry".to_owned()]));
            continue;
        };
        let origin = &feature["origin"];
        let origin = BlockPos::new(
            int(&origin[0]) as i32,
            int(&origin[1]) as i32,
            int(&origin[2]) as i32,
        );
        let biome_check = feature["biome_check"].as_bool().expect("biome_check");
        let (seed_index, step) = (
            int(&feature["feature_seed"][0]) as i32,
            int(&feature["feature_seed"][1]) as i32,
        );

        let mut placed = chunks.clone();
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            let mut random = DecorationRandom::new(0);
            let decoration_seed = random.set_decoration_seed(
                int(&area["seed"]),
                center.min_block_x(),
                center.min_block_z(),
            );
            random.set_feature_seed(decoration_seed, seed_index, step);
            let mut region = generator.feature_region(region_chunks(&mut placed, center));
            generator
                .features()
                .place_one(&mut region, id, origin, &mut random, biome_check)
        }));
        let mut diffs = terrain_diffs.clone();
        match outcome {
            Err(payload) => diffs.push(format!("panicked: {}", panic_message(&*payload))),
            Ok(result) => {
                let expected_result = feature["result"].as_bool().expect("result");
                if result != expected_result {
                    diffs.push(format!("returned {result}, expected {expected_result}"));
                }
                let mut expected = expected_terrain.clone();
                for e in list(&feature["sections"]) {
                    expected[int(&e[0]) as usize].sections[int(&e[1]) as usize] = int(&e[2]);
                }
                for e in list(&feature["heightmaps"]) {
                    expected[int(&e[0]) as usize].heightmaps[int(&e[1]) as usize] = int(&e[2]);
                }
                for e in list(&feature["post_processing"]) {
                    expected[int(&e[0]) as usize].post_processing[int(&e[1]) as usize] =
                        (int(&e[2]), int(&e[3]));
                }
                for e in list(&feature["block_ticks"]) {
                    expected[int(&e[0]) as usize].block_ticks = (int(&e[1]), int(&e[2]));
                }
                for e in list(&feature["fluid_ticks"]) {
                    expected[int(&e[0]) as usize].fluid_ticks = (int(&e[1]), int(&e[2]));
                }
                for e in list(&feature["block_entities"]) {
                    let entity = block_entity(e);
                    let chunk = ChunkPos::new(entity.0 >> 4, entity.2 >> 4);
                    let i = ((chunk.z - center.z + 1) * 3 + chunk.x - center.x + 1) as usize;
                    expected[i].block_entities.push(entity);
                }
                for (chunk, expected) in placed.iter().zip(&expected) {
                    Snapshot::of(chunk).diff(expected, chunk.pos(), min_y, &mut diffs);
                }
            }
        }
        if !diffs.is_empty() {
            failures.push((name, diffs));
        }
    }
    assert!(
        total > 0,
        "no placed feature named {}",
        filter.unwrap_or_default()
    );
    report("placed features", total, &skipped, &failures);
}

#[test]
fn decoration_matches_vanilla() {
    let golden = common::golden("feature");
    let mut generators = Generators::default();
    let decorations = list(&golden["decorations"]);
    let mut failures = Vec::new();
    for area in decorations {
        let generator = generators.get(area);
        let center = ChunkPos::new(int(&area["x"]) as i32, int(&area["z"]) as i32);
        let min_y = int(&area["min_y"]) as i32;
        // Chunks within `radius` are decorated row by row, or in the reverse order.
        let radius = area["radius"].as_i64().unwrap_or(1) as i32;
        let sign = if area["reversed"].as_bool().unwrap_or(false) {
            -1
        } else {
            1
        };
        let label = format!(
            "{} seed {} around chunk [{}, {}], radius {radius}, order {sign}",
            area["dimension"].as_str().unwrap_or_default(),
            int(&area["seed"]),
            center.x,
            center.z
        );
        let mut chunks = terrain(generator, center, radius + 1);
        let mut diffs = Vec::new();
        'decorate: for dz in -radius..=radius {
            for dx in -radius..=radius {
                let pos = ChunkPos::new(center.x + sign * dx, center.z + sign * dz);
                let outcome = catch_unwind(AssertUnwindSafe(|| {
                    let region: [&mut ProtoChunk; 9] = region_chunks(&mut chunks, pos)
                        .try_into()
                        .expect("nine chunks");
                    generator.generate_features(region);
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
            }
        }
        if diffs.is_empty() {
            for (chunk, expected) in chunks.iter().zip(list(&area["chunks"])) {
                Snapshot::of(chunk).diff(
                    &Snapshot::from_golden(expected),
                    chunk.pos(),
                    min_y,
                    &mut diffs,
                );
            }
        }
        if !diffs.is_empty() {
            failures.push((label, diffs));
        }
    }
    report("decorated areas", decorations.len(), &[], &failures);
}

/// The `chunk-generator` slot, driven like the chunk runtime drives it (biomes, terrain and
/// features as separate steps on snapshots, the neighbours' changes taken back), decorates the
/// first golden area like Vanilla.
#[test]
fn slot_decoration_matches_vanilla() {
    use std::sync::Arc;
    use vanillite_chunk::slots::{self, ChunkGenerator, ChunkStatus, Neighbours, WorldInfo};
    use vanillite_worldgen::VanillaChunkGenerator;

    let golden = common::golden("feature");
    let area = &list(&golden["decorations"])[0];
    let dimension = area["dimension"].as_str().expect("dimension");
    let seed = int(&area["seed"]);
    let (min_y, height) = (int(&area["min_y"]) as i32, int(&area["height"]) as i32);
    let source = biome_source(dimension);
    let has_sky_light = dimension == "minecraft:overworld";
    let spec = DimensionSpec {
        noise_settings: dimension,
        biome_source: &source,
        min_y,
        height,
        has_sky_light,
    };
    let generator = TerrainGenerator::new(common::packs(), spec, seed).expect("generator");
    let world = WorldInfo {
        dimension: dimension.to_owned(),
        dir: std::env::temp_dir(),
        min_y,
        height: height as u32,
        seed,
        has_sky_light,
        default_biome: generator.biomes().id_of("minecraft:plains").unwrap_or(0),
        biomes: generator
            .biomes()
            .all()
            .iter()
            .map(|b| b.name.clone())
            .collect(),
        air: 0,
    };
    let slot = VanillaChunkGenerator::new(generator, world);
    let center = ChunkPos::new(int(&area["x"]) as i32, int(&area["z"]) as i32);
    let mut chunks: HashMap<ChunkPos, Arc<slots::ProtoChunk>> = HashMap::new();
    let mut step = |status: ChunkStatus, radius: i32| {
        for dz in -radius..=radius {
            for dx in -radius..=radius {
                let pos = ChunkPos::new(center.x + dx, center.z + dz);
                let mut target = [chunks
                    .get(&pos)
                    .map_or_else(|| slot.new_chunk(pos), |c| (**c).clone())];
                let mut neighbours: Vec<Arc<slots::ProtoChunk>> = chunks
                    .iter()
                    .filter(|(p, _)| {
                        **p != pos && (p.x - pos.x).abs() <= 1 && (p.z - pos.z).abs() <= 1
                    })
                    .map(|(_, c)| Arc::clone(c))
                    .collect();
                slot.generate(status, &mut target, &mut Neighbours::new(&mut neighbours));
                let [mut target] = target;
                target.status = status;
                for n in neighbours {
                    chunks.insert(n.pos, n);
                }
                chunks.insert(pos, Arc::new(target));
            }
        }
    };
    step(ChunkStatus::Biomes, 3);
    step(ChunkStatus::Terrain, 2);
    step(ChunkStatus::Features, 1);

    let mut diffs = Vec::new();
    let mut index = 0;
    for dz in -2..=2 {
        for dx in -2..=2 {
            let chunk = &chunks[&ChunkPos::new(center.x + dx, center.z + dz)];
            let mut proto = chunk
                .generation
                .as_ref()
                .and_then(|g| g.0.downcast_ref::<ProtoChunk>())
                .expect("generation state")
                .clone();
            proto.set_sections(
                chunk
                    .chunk
                    .sections()
                    .iter()
                    .map(|s| (**s).clone())
                    .collect(),
            );
            Snapshot::of(&proto).diff(
                &Snapshot::from_golden(&list(&area["chunks"])[index]),
                proto.pos(),
                min_y,
                &mut diffs,
            );
            index += 1;
        }
    }
    assert!(diffs.is_empty(), "{}", diffs.join("\n"));
}
