//! Smoke test: every tree-like feature of the Vanilla pack parses and places on a flat area
//! without panicking, and the common trees actually grow.

use serde_json::Value;
use vanillite_base::random::DecorationRandom;
use vanillite_base::{BlockPos, ChunkPos};
use vanillite_data::block::{BlockState, ids};

use crate::anchor::WorldGenerationContext;
use crate::biome::BiomeRegistry;
use crate::blocks::default_state;
use crate::feature::region::WorldGenRegion;
use crate::feature::{FeatureContext, FeatureEnv, FeatureId, FeatureParser};
use crate::proto::ProtoChunk;
use crate::random_state::RandomState;
use crate::test_support::packs;

use super::{FLUID_FLOWING_WATER, FLUID_WATER, shuffle};
use vanillite_base::random::RandomSource;

const TREE_LIKE: [&str; 7] = [
    "minecraft:tree",
    "minecraft:fallen_tree",
    "minecraft:huge_brown_mushroom",
    "minecraft:huge_red_mushroom",
    "minecraft:huge_fungus",
    "minecraft:coral_tree",
    "minecraft:coral_claw",
];

/// 3×3 chunks around chunk (0, 0): stone below y = 60, dirt up to 63, the surface block at 64;
/// a 4×4 pond of water at the surface in the north-west corner of the middle chunk.
fn flat_area(biomes: &BiomeRegistry, surface: BlockState) -> Vec<ProtoChunk> {
    let plains = biomes.require("minecraft:plains").expect("plains");
    let (stone, dirt) = (
        default_state("minecraft:stone"),
        default_state("minecraft:dirt"),
    );
    let mut chunks = Vec::new();
    for dz in -1..=1 {
        for dx in -1..=1 {
            let mut chunk = ProtoChunk::new(ChunkPos::new(dx, dz), -64, 384, plains);
            for x in 0..16 {
                for z in 0..16 {
                    let (wx, wz) = (dx * 16 + x, dz * 16 + z);
                    for y in -64..64 {
                        chunk.set_block(wx, y, wz, if y < 60 { stone } else { dirt });
                    }
                    let pond = dx == 0 && dz == 0 && x < 4 && z < 4;
                    let top = if pond {
                        ids::WATER.default_state()
                    } else {
                        surface
                    };
                    chunk.set_block(wx, 64, wz, top);
                }
            }
            chunk.finish_terrain();
            chunks.push(chunk);
        }
    }
    chunks
}

#[test]
fn tree_like_features_place_on_flat_ground() {
    let packs = packs();
    let biomes = BiomeRegistry::load(packs).expect("biomes");
    let random_state = RandomState::new(packs, 12345, false).expect("random state");
    let mut parser = FeatureParser::new(packs, &biomes, &random_state);
    let registry = packs
        .registry("minecraft:worldgen/feature")
        .expect("feature registry");
    let mut features: Vec<(String, FeatureId)> = Vec::new();
    for entry in registry.entries() {
        let json: Value = serde_json::from_str(entry.json.get()).expect("feature json");
        let kind = json.get("type").and_then(Value::as_str).unwrap_or_default();
        if TREE_LIKE.contains(&kind) {
            let id = parser
                .feature(&Value::String(entry.id.clone()))
                .unwrap_or_else(|e| panic!("{}: {e}", entry.id));
            features.push((entry.id.clone(), id));
        }
    }
    let env = FeatureEnv {
        registry: parser.finish(),
        context: WorldGenerationContext::new(-64, 384, 63),
        biome_feature_sets: Vec::new(),
    };
    let grass = flat_area(&biomes, default_state("minecraft:grass_block"));
    let crimson = flat_area(&biomes, default_state("minecraft:crimson_nylium"));
    let warped = flat_area(&biomes, default_state("minecraft:warped_nylium"));
    let mut grown = 0;
    for (name, id) in &features {
        let template = match name.as_str() {
            n if n.contains("warped") => &warped,
            n if n.contains("crimson") => &crimson,
            _ => &grass,
        };
        for (seed, (x, z)) in [(1_i64, (8, 8)), (2, (2, 2)), (3, (13, 5))] {
            let mut area = template.clone();
            let mut region =
                WorldGenRegion::new(area.iter_mut().collect(), 1, 1, &random_state, 63);
            let mut random = DecorationRandom::new(seed);
            let mut ctx = FeatureContext {
                level: &mut region,
                env: &env,
                random: &mut random,
            };
            if ctx.place_feature(*id, BlockPos::new(x, 65, z)) {
                grown += 1;
            }
        }
    }
    assert!(
        grown > features.len(),
        "only {grown} of {} placements succeeded",
        features.len() * 3
    );
}

#[test]
fn shuffle_matches_util_shuffle() {
    // Util.shuffle swaps i - 1 with nextInt(i) for i = size..2.
    let mut random = DecorationRandom::new(42);
    let mut reference = DecorationRandom::new(42);
    let mut list = [0, 1, 2, 3, 4, 5];
    shuffle(&mut list, &mut random);
    let mut expected = [0, 1, 2, 3, 4, 5];
    for i in (2..=6).rev() {
        let j = reference.next_int_bounded(i) as usize;
        expected.swap(i as usize - 1, j);
    }
    assert_eq!(list, expected);
}

#[test]
fn fluid_ids_match_registry() {
    let fluids = vanillite_data::registry::Registry::by_name("minecraft:fluid").unwrap();
    assert_eq!(fluids.id("minecraft:water"), Some(FLUID_WATER));
    assert_eq!(
        fluids.id("minecraft:flowing_water"),
        Some(FLUID_FLOWING_WATER)
    );
}
