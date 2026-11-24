//! Every placement modifier, block predicate, block state provider and value provider that a
//! Vanilla feature, placed feature or block state provider JSON contains parses with the ported
//! types, wherever it is nested.

mod common;

use serde_json::Value;
use vanillite_worldgen::RandomState;
use vanillite_worldgen::biome::BiomeRegistry;
use vanillite_worldgen::carver::providers::{FloatProvider, HeightProvider, IntProvider};
use vanillite_worldgen::feature::{BlockPredicate, BlockStateProvider, FeatureParser};

const PREDICATES: &[&str] = &[
    "matching_blocks",
    "matching_block_tag",
    "matching_fluids",
    "matching_biomes",
    "has_sturdy_face",
    "solid",
    "replaceable",
    "would_survive",
    "inside_world_bounds",
    "any_of",
    "all_of",
    "not",
    "true",
    "unobstructed",
    "volume_match",
];

const PROVIDERS: &[&str] = &[
    "copy_properties",
    "dual_noise",
    "noise",
    "noise_threshold",
    "random_block",
    "randomized_int",
    "rotated",
    "rule_based",
    "simple",
    "weighted",
];

const PLACEMENTS: &[&str] = &[
    "block_predicate_filter",
    "rarity_filter",
    "random_chance",
    "surface_relative_threshold_filter",
    "surface_water_depth_filter",
    "biome",
    "count",
    "noise_based_count",
    "noise_threshold_count",
    "count_on_every_layer",
    "cuboid",
    "environment_scan",
    "heightmap",
    "in_square",
    "offset",
    "randomly_selected",
    "fixed_placement",
];

const VALUE_PROVIDERS: &[&str] = &[
    "constant",
    "uniform",
    "biased_to_bottom",
    "very_biased_to_bottom",
    "clamped",
    "weighted_list",
    "clamped_normal",
    "trapezoid",
];

#[derive(Default)]
struct Counts {
    predicates: usize,
    providers: usize,
    placements: usize,
    values: usize,
}

fn walk(json: &Value, parser: &mut FeatureParser<'_>, file: &str, counts: &mut Counts) {
    match json {
        Value::Object(map) => {
            if let Some(kind) = map.get("type").and_then(Value::as_str) {
                let kind = kind.strip_prefix("minecraft:").unwrap_or(kind);
                let result = if kind == "height_range" {
                    if map.contains_key("height") {
                        counts.placements += 1;
                        parser.placement(json).map(drop)
                    } else {
                        counts.predicates += 1;
                        BlockPredicate::parse(json, parser).map(drop)
                    }
                } else if PREDICATES.contains(&kind) {
                    counts.predicates += 1;
                    BlockPredicate::parse(json, parser).map(drop)
                } else if PROVIDERS.contains(&kind) {
                    counts.providers += 1;
                    BlockStateProvider::parse(json, parser).map(drop)
                } else if PLACEMENTS.contains(&kind) {
                    counts.placements += 1;
                    parser.placement(json).map(drop)
                } else if VALUE_PROVIDERS.contains(&kind) {
                    counts.values += 1;
                    IntProvider::from_json(json)
                        .map(drop)
                        .or_else(|_| HeightProvider::from_json(json).map(drop))
                        .or_else(|_| FloatProvider::from_json(json).map(drop))
                } else {
                    Ok(())
                };
                if let Err(e) = result {
                    panic!("{file}: {kind} {json}: {e}");
                }
            }
            for value in map.values() {
                walk(value, parser, file, counts);
            }
        }
        Value::Array(items) => {
            for item in items {
                walk(item, parser, file, counts);
            }
        }
        _ => {}
    }
}

#[test]
fn every_nested_part_parses() {
    let packs = common::packs();
    let biomes = BiomeRegistry::load(packs).expect("biomes");
    let random_state = RandomState::new(packs, 12345, false).expect("random state");
    let mut parser = FeatureParser::new(packs, &biomes, &random_state);
    let mut counts = Counts::default();
    for registry in [
        "minecraft:worldgen/feature",
        "minecraft:worldgen/placed_feature",
        "minecraft:worldgen/block_state_provider",
    ] {
        for entry in packs.registry(registry).expect(registry).entries() {
            let json: Value = serde_json::from_str(entry.json.get()).expect("json");
            walk(&json, &mut parser, &entry.id, &mut counts);
        }
    }
    for entry in packs
        .registry("minecraft:worldgen/block_state_provider")
        .expect("providers")
        .entries()
    {
        parser
            .state_provider(&Value::String(entry.id.clone()))
            .unwrap_or_else(|e| panic!("{}: {e}", entry.id));
    }
    assert!(counts.predicates > 500, "{}", counts.predicates);
    assert!(counts.providers > 50, "{}", counts.providers);
    assert!(counts.placements > 1000, "{}", counts.placements);
    assert!(counts.values > 400, "{}", counts.values);
}
