//! Every structure and structure set of the Vanilla data pack parses for all three dimensions,
//! and the structure stages (starts, references, terrain with the `Beardifier`, features with
//! structure placement) run on small areas without panicking.

mod common;

use std::collections::HashMap;

use serde_json::json;
use vanillite_base::ChunkPos;
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
    TerrainGenerator::new(common::packs(), spec, seed).expect("generator with structures")
}

/// Chunks of one dimension, generated stage by stage on demand.
struct World<'g> {
    generator: &'g TerrainGenerator,
    /// Chunks past the structure starts stage.
    starts: HashMap<ChunkPos, ProtoChunk>,
}

impl<'g> World<'g> {
    fn new(generator: &'g TerrainGenerator) -> Self {
        Self {
            generator,
            starts: HashMap::new(),
        }
    }

    /// Generates the structure starts of every chunk within `radius` of `center`.
    fn ensure_starts(&mut self, center: ChunkPos, radius: i32) {
        let mut batch = Vec::new();
        for dz in -radius..=radius {
            for dx in -radius..=radius {
                let pos = ChunkPos::new(center.x + dx, center.z + dz);
                if !self.starts.contains_key(&pos) {
                    batch.push(self.generator.new_chunk(pos));
                }
            }
        }
        self.generator.generate_structure_starts(&mut batch);
        for chunk in batch {
            self.starts.insert(chunk.pos(), chunk);
        }
    }

    /// Decorates `center` (features with structures) after generating everything it needs;
    /// returns the decorated 3×3 area.
    fn decorate(&mut self, center: ChunkPos) -> Vec<ProtoChunk> {
        self.decorate_with(center, true)
    }

    /// [`Self::decorate`], with or without placing structures (the terrain keeps its
    /// `Beardifier` either way).
    fn decorate_with(&mut self, center: ChunkPos, place_structures: bool) -> Vec<ProtoChunk> {
        self.ensure_starts(center, 9);
        let generator = self.generator;
        // References for the 3×3 area, biomes for 5×5.
        let mut chunks: HashMap<ChunkPos, ProtoChunk> = HashMap::new();
        for dz in -2..=2 {
            for dx in -2..=2 {
                let pos = ChunkPos::new(center.x + dx, center.z + dz);
                let mut chunk = self.starts[&pos].clone();
                if dx.abs() <= 1 && dz.abs() <= 1 {
                    let starts = NeighbourStarts::collect(pos, |ox, oz| {
                        &self.starts[&ChunkPos::new(pos.x + ox, pos.z + oz)]
                    });
                    generator.generate_structure_references(&mut chunk, &starts);
                }
                chunks.insert(pos, chunk);
            }
        }
        let mut all: Vec<ProtoChunk> = chunks.into_values().collect();
        generator.generate_biomes(&mut all);
        let chunks: HashMap<ChunkPos, ProtoChunk> = all.into_iter().map(|c| (c.pos(), c)).collect();
        let mut jobs = Vec::new();
        for dz in -1..=1 {
            for dx in -1..=1 {
                let pos = ChunkPos::new(center.x + dx, center.z + dz);
                let chunk = chunks[&pos].clone();
                let neighbours = NeighbourBiomes::collect(|ox, oz| {
                    &chunks[&ChunkPos::new(pos.x + ox, pos.z + oz)]
                });
                let structures = ReferencedStarts::collect(&chunk, |p| &self.starts[&p]);
                jobs.push(TerrainJob {
                    chunk,
                    neighbours,
                    structures,
                });
            }
        }
        generator.generate_terrain(&mut jobs);
        let mut area: Vec<ProtoChunk> = jobs.into_iter().map(|j| j.chunk).collect();
        let structures = if place_structures {
            ReferencedStarts::collect(&area[4], |p| &self.starts[&p])
        } else {
            ReferencedStarts::empty()
        };
        let region: [&mut ProtoChunk; 9] = area
            .iter_mut()
            .collect::<Vec<_>>()
            .try_into()
            .expect("nine chunks");
        generator.generate_features(region, &structures);
        area
    }
}

#[test]
fn all_dimensions_parse_and_generate() {
    for dimension in ["minecraft:overworld", "minecraft:nether", "minecraft:end"] {
        let generator = generator(dimension, 12345);
        assert!(
            generator.structures().structures().len() >= 50,
            "{dimension}: structures missing"
        );
        assert!(
            !generator.structures().possible_sets().is_empty(),
            "{dimension}"
        );
        let mut world = World::new(&generator);
        world.decorate(ChunkPos::new(0, 0));
    }
}

/// Each ported structure type, found at a known start of a known seed, changes the blocks
/// of the chunk holding its first piece's centre when the features stage places it.
#[test]
fn ported_structures_place() {
    let cases: [(&str, i64, &str, ChunkPos, &str); 12] = [
        (
            "minecraft:overworld",
            42,
            "minecraft:desert_pyramid",
            ChunkPos::new(-86, -110),
            "minecraft:orange_terracotta",
        ),
        (
            "minecraft:overworld",
            3,
            "minecraft:jungle_pyramid",
            ChunkPos::new(-25, 1),
            "minecraft:mossy_cobblestone",
        ),
        (
            "minecraft:overworld",
            42,
            "minecraft:swamp_hut",
            ChunkPos::new(-51, -23),
            "minecraft:spruce_planks",
        ),
        (
            "minecraft:overworld",
            12345,
            "minecraft:igloo",
            ChunkPos::new(36, 41),
            "",
        ),
        (
            "minecraft:overworld",
            12345,
            "minecraft:buried_treasure",
            ChunkPos::new(46, 64),
            "minecraft:chest",
        ),
        (
            "minecraft:overworld",
            12345,
            "minecraft:shipwreck",
            ChunkPos::new(-9, 35),
            "",
        ),
        (
            "minecraft:overworld",
            42,
            "minecraft:shipwreck_beached",
            ChunkPos::new(40, 55),
            "",
        ),
        (
            "minecraft:overworld",
            12345,
            "minecraft:ocean_ruin_cold",
            ChunkPos::new(-16, 5),
            "",
        ),
        (
            "minecraft:overworld",
            1,
            "minecraft:ocean_ruin_warm",
            ChunkPos::new(-57, 48),
            "",
        ),
        (
            "minecraft:overworld",
            12345,
            "minecraft:ruined_portal",
            ChunkPos::new(-40, -59),
            "",
        ),
        (
            "minecraft:nether",
            12345,
            "minecraft:nether_fossil",
            ChunkPos::new(-32, -6),
            "minecraft:bone_block",
        ),
        (
            "minecraft:nether",
            12345,
            "minecraft:ruined_portal_nether",
            ChunkPos::new(0, -18),
            "",
        ),
    ];
    let mut generators: HashMap<(&str, i64), TerrainGenerator> = HashMap::new();
    for (dimension, seed, name, start_pos, expected) in cases {
        let generator = generators
            .entry((dimension, seed))
            .or_insert_with(|| generator(dimension, seed));
        let mut world = World::new(generator);
        world.ensure_starts(start_pos, 0);
        let id = generator
            .structures()
            .structure_id(name)
            .expect("structure");
        let start = world.starts[&start_pos]
            .start_for_structure(id)
            .unwrap_or_else(|| panic!("no {name} start in {start_pos:?} of seed {seed}"))
            .clone();
        let center = start.pieces()[0].bounding_box().center();
        let chunk = ChunkPos::new(center.x >> 4, center.z >> 4);
        let without = world.decorate_with(chunk, false);
        let with = world.decorate_with(chunk, true);
        let mut changed = Vec::new();
        let (min_y, max_y) = (with[4].min_y(), with[4].max_y());
        for y in min_y..=max_y {
            for z in chunk.min_block_z()..chunk.min_block_z() + 16 {
                for x in chunk.min_block_x()..chunk.min_block_x() + 16 {
                    let (a, b) = (without[4].block(x, y, z), with[4].block(x, y, z));
                    if a != b {
                        changed.push(b);
                    }
                }
            }
        }
        assert!(!changed.is_empty(), "{name} in {chunk:?} placed nothing");
        if !expected.is_empty() {
            assert!(
                changed.iter().any(|s| s.block().name() == expected),
                "{name} in {chunk:?} placed no {expected}"
            );
        }
    }
}

/// `getBaseHeight` agrees with the world surface heightmap of the terrain stage's noise step.
#[test]
fn base_height_matches_noise_fill() {
    use vanillite_chunk::HeightmapKind;
    use vanillite_worldgen::TerrainStep;
    use vanillite_worldgen::proto::WgHeightmap;
    use vanillite_worldgen::structure::LevelHeight;
    let generator = generator("minecraft:overworld", 1);
    let level = LevelHeight {
        min_y: -64,
        height: 384,
    };
    for center in [
        ChunkPos::new(0, 0),
        ChunkPos::new(-20, 37),
        ChunkPos::new(53, 41),
    ] {
        let mut biomes: Vec<ProtoChunk> = (0..9)
            .map(|i| generator.new_chunk(ChunkPos::new(center.x + i % 3 - 1, center.z + i / 3 - 1)))
            .collect();
        generator.generate_biomes(&mut biomes);
        let neighbours =
            NeighbourBiomes::collect(|dx, dz| &biomes[((dz + 1) * 3 + dx + 1) as usize]);
        let mut job = TerrainJob {
            chunk: biomes[4].clone(),
            neighbours,
            structures: ReferencedStarts::empty(),
        };
        let mut heights = Vec::new();
        generator.generate_terrain_observed(&mut job, |step, chunk| {
            if step == TerrainStep::Noise {
                for (x, z) in [(0, 0), (5, 11), (15, 15), (8, 3)] {
                    heights.push((
                        x,
                        z,
                        chunk.height_at(WgHeightmap::WorldSurface, x, z) + 1,
                        chunk.height_at(WgHeightmap::OceanFloor, x, z) + 1,
                    ));
                }
            }
        });
        for (x, z, surface, floor) in heights {
            let (bx, bz) = (
                center.min_block_x() + x as i32,
                center.min_block_z() + z as i32,
            );
            assert_eq!(
                generator.base_height(bx, bz, HeightmapKind::WorldSurfaceWg, level),
                surface,
                "surface at {bx} {bz}"
            );
            assert_eq!(
                generator.base_height(bx, bz, HeightmapKind::OceanFloorWg, level),
                floor,
                "floor at {bx} {bz}"
            );
        }
    }
}
