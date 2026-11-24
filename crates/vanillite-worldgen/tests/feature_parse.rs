//! Every feature and placed feature of the Vanilla data pack parses, for all three dimensions,
//! and one area of each dimension decorates without panicking.

mod common;

use serde_json::json;
use vanillite_base::ChunkPos;
use vanillite_worldgen::{
    DimensionSpec, NeighbourBiomes, ProtoChunk, TerrainGenerator, TerrainJob,
};

fn generator(
    noise_settings: &str,
    biome_source: serde_json::Value,
    min_y: i32,
    height: i32,
) -> TerrainGenerator {
    let spec = DimensionSpec {
        noise_settings,
        biome_source: &biome_source,
        min_y,
        height,
        has_sky_light: noise_settings == "minecraft:overworld",
    };
    TerrainGenerator::new(common::packs(), spec, 12345).expect("generator with features")
}

/// Generates the 5×5 chunks around `center` up to terrain and decorates the middle chunk.
fn decorate_one(generator: &TerrainGenerator, center: ChunkPos) {
    let mut biomes: Vec<ProtoChunk> = (0..49)
        .map(|i| generator.new_chunk(ChunkPos::new(center.x + i % 7 - 3, center.z + i / 7 - 3)))
        .collect();
    generator.generate_biomes(&mut biomes);
    let at = |dx: i32, dz: i32| ((dz + 3) * 7 + dx + 3) as usize;
    let mut terrain = Vec::new();
    for dz in -1..=1 {
        for dx in -1..=1 {
            let neighbours = NeighbourBiomes::collect(|ox, oz| &biomes[at(dx + ox, dz + oz)]);
            terrain.push(TerrainJob {
                chunk: biomes[at(dx, dz)].clone(),
                neighbours,
            });
        }
    }
    generator.generate_terrain(&mut terrain);
    let mut chunks: Vec<ProtoChunk> = terrain.into_iter().map(|j| j.chunk).collect();
    let area: [&mut ProtoChunk; 9] = chunks
        .iter_mut()
        .collect::<Vec<_>>()
        .try_into()
        .expect("nine chunks");
    generator.generate_features(area);
}

#[test]
fn all_dimensions_parse_and_decorate() {
    let overworld = generator(
        "minecraft:overworld",
        json!({"type": "minecraft:multi_noise", "preset": "minecraft:overworld"}),
        -64,
        384,
    );
    decorate_one(&overworld, ChunkPos::new(0, 0));
    let nether = generator(
        "minecraft:nether",
        json!({"type": "minecraft:multi_noise", "preset": "minecraft:nether"}),
        0,
        256,
    );
    decorate_one(&nether, ChunkPos::new(0, 0));
    let end = generator(
        "minecraft:end",
        json!({"type": "minecraft:the_end"}),
        0,
        256,
    );
    decorate_one(&end, ChunkPos::new(0, 0));
}
