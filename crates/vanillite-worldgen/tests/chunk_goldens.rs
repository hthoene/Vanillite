//! Per-stage chunk parity with Vanilla: biomes, then the three steps of the terrain stage (noise
//! fill with aquifers, material rules, carvers), compared by per-section hashes recorded by
//! `ChunkGoldens.java`.

mod common;

use std::collections::HashMap;

use serde_json::{Value, json};
use vanillite_base::ChunkPos;
use vanillite_base::math::java_string_hash;
use vanillite_worldgen::proto::WgHeightmap;
use vanillite_worldgen::{
    DimensionSpec, NeighbourBiomes, ProtoChunk, TerrainGenerator, TerrainJob, TerrainStep,
};

fn biome_source(dimension: &str) -> Value {
    match dimension {
        "minecraft:nether" => {
            json!({"type": "minecraft:multi_noise", "preset": "minecraft:nether"})
        }
        "minecraft:end" => json!({"type": "minecraft:the_end"}),
        _ => json!({"type": "minecraft:multi_noise", "preset": "minecraft:overworld"}),
    }
}

fn biome_hash(generator: &TerrainGenerator, chunk: &ProtoChunk) -> i64 {
    let mut hash = 0_i64;
    let pos = chunk.pos();
    let min_quart = chunk.min_y() >> 2;
    for qy in min_quart..min_quart + (chunk.height() >> 2) {
        for qz in 0..4 {
            for qx in 0..4 {
                let biome = chunk.noise_biome((pos.x << 2) + qx, qy, (pos.z << 2) + qz);
                let name = &generator.biomes().get(biome).name;
                hash = hash
                    .wrapping_mul(31)
                    .wrapping_add(i64::from(java_string_hash(name)));
            }
        }
    }
    hash
}

/// Differences between a chunk and a golden snapshot, as readable lines.
fn compare(chunk: &ProtoChunk, golden: &Value) -> Vec<String> {
    let mut diffs = Vec::new();
    for (index, (section, expected)) in chunk
        .sections()
        .iter()
        .zip(golden["sections"].as_array().unwrap())
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
        if hash != expected.as_i64().unwrap() {
            diffs.push(format!(
                "section {index} (y {})",
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
        if hash != golden[key].as_i64().unwrap() {
            diffs.push(format!("heightmap {key}"));
        }
    }
    let mut lists: Vec<Vec<i16>> = vec![Vec::new(); chunk.sections().len()];
    for &(x, y, z) in chunk.post_processing() {
        let packed = (x & 15) | ((y & 15) << 4) | ((z & 15) << 8);
        lists[chunk.section_index(y)].push(packed as i16);
    }
    for (index, (list, expected)) in lists
        .iter()
        .zip(golden["post_processing"].as_array().unwrap())
        .enumerate()
    {
        let hash = list
            .iter()
            .fold(0_i64, |h, &v| h.wrapping_mul(31).wrapping_add(i64::from(v)));
        if list.len() as i64 != expected[0].as_i64().unwrap()
            || hash != expected[1].as_i64().unwrap()
        {
            diffs.push(format!(
                "post processing of section {index}: {} marks, expected {}",
                list.len(),
                expected[0]
            ));
        }
    }
    diffs
}

#[test]
fn chunks_match_vanilla() {
    let packs = common::packs();
    let golden = common::golden("chunk");
    let mut generators: HashMap<(String, i64), TerrainGenerator> = HashMap::new();
    let mut failures = Vec::new();
    for case in golden["chunks"].as_array().unwrap() {
        let dimension = case["dimension"].as_str().unwrap();
        let seed = case["seed"].as_i64().unwrap();
        let generator = generators
            .entry((dimension.to_owned(), seed))
            .or_insert_with(|| {
                let source = biome_source(dimension);
                let spec = DimensionSpec {
                    noise_settings: dimension,
                    biome_source: &source,
                    min_y: case["min_y"].as_i64().unwrap() as i32,
                    height: case["height"].as_i64().unwrap() as i32,
                };
                TerrainGenerator::new(packs, spec, seed).expect("generator")
            });
        let center = ChunkPos::new(
            case["x"].as_i64().unwrap() as i32,
            case["z"].as_i64().unwrap() as i32,
        );
        let label = format!("{dimension} seed {seed} chunk {} {}", center.x, center.z);

        let mut area: Vec<ProtoChunk> = (0..9)
            .map(|i| generator.new_chunk(ChunkPos::new(center.x + i % 3 - 1, center.z + i / 3 - 1)))
            .collect();
        generator.generate_biomes(&mut area);
        if biome_hash(generator, &area[4]) != case["biomes"].as_i64().unwrap() {
            failures.push(format!("{label}: biomes differ"));
            continue;
        }
        let neighbours = NeighbourBiomes::collect(|dx, dz| &area[((dz + 1) * 3 + dx + 1) as usize]);
        let mut job = TerrainJob {
            chunk: area.swap_remove(4),
            neighbours,
        };
        let mut step_failures = Vec::new();
        generator.generate_terrain_observed(&mut job, |step, chunk| {
            let key = match step {
                TerrainStep::Noise => "noise",
                TerrainStep::Surface => "surface",
                TerrainStep::Carvers => "carvers",
            };
            for diff in compare(chunk, &case[key]) {
                step_failures.push(format!("{label}: after {key}: {diff}"));
            }
        });
        failures.extend(step_failures);
    }
    assert!(
        failures.is_empty(),
        "{} differences:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
