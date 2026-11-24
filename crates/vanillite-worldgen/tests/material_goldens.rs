//! Parity of the material system with Vanilla: clay bands, vertical anchors and the world
//! generation context, and the noise based per-column helpers of `MaterialSystem`.

mod common;

use serde_json::Value;
use vanillite_base::random::{PositionalRandomFactory, RandomSource as _};
use vanillite_worldgen::RandomState;
use vanillite_worldgen::biome::BiomeRegistry;
use vanillite_worldgen::density::DensityCompiler;
use vanillite_worldgen::material::{
    MaterialSystem, VerticalAnchor, WorldGenerationContext, generate_bands,
};
use vanillite_worldgen::random::AnyRandom;
use vanillite_worldgen::settings::NoiseGeneratorSettings;

fn int(v: &Value) -> i32 {
    v.as_i64().expect("int") as i32
}

#[test]
fn clay_bands() {
    let golden = common::golden("material");
    for case in golden["clay_bands"].as_array().unwrap() {
        let seed = case["seed"].as_i64().unwrap();
        let legacy = case["legacy"].as_bool().unwrap();
        let mut random = AnyRandom::new(legacy, seed)
            .fork_positional()
            .with_hash_of("minecraft:clay_bands");
        let bands = generate_bands(&mut random);
        let names: Vec<&str> = bands.iter().map(|s| s.block().name()).collect();
        let expected: Vec<&str> = case["bands"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(names, expected, "seed {seed}, legacy {legacy}");
    }
}

#[test]
fn anchors_and_contexts() {
    let golden = common::golden("material");
    let contexts: Vec<WorldGenerationContext> = golden["contexts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            let context = WorldGenerationContext::new(
                int(&c["generator_min_y"]),
                int(&c["generator_height"]),
                int(&c["sea_level"]),
                int(&c["level_min_y"]),
                int(&c["level_height"]),
            );
            assert_eq!(context.min_gen_y(), int(&c["min_gen_y"]), "{c}");
            assert_eq!(context.gen_depth(), int(&c["gen_depth"]), "{c}");
            assert_eq!(context.sea_level(), int(&c["context_sea_level"]), "{c}");
            context
        })
        .collect();
    for case in golden["anchors"].as_array().unwrap() {
        let json = &case["json"];
        match (VerticalAnchor::parse(json), case.get("resolved")) {
            (Ok(anchor), Some(resolved)) => {
                let ys: Vec<i32> = contexts.iter().map(|c| anchor.resolve_y(c)).collect();
                let expected: Vec<i32> = resolved.as_array().unwrap().iter().map(int).collect();
                assert_eq!(ys, expected, "{json}");
            }
            (Err(_), None) => {}
            (result, expected) => panic!("{json}: got {result:?}, Vanilla {expected:?}"),
        }
    }
}

#[test]
#[ignore = "needs the noise, density and biome modules"]
fn column_helpers() {
    let packs = common::packs();
    let golden = common::golden("material");
    let settings = NoiseGeneratorSettings::load(packs, "minecraft:overworld").unwrap();
    let biomes = BiomeRegistry::load(packs).unwrap();
    for seed in [0_i64, 42] {
        let random_state = RandomState::new(packs, seed, settings.legacy_random_source).unwrap();
        let mut compiler = DensityCompiler::new(&random_state);
        let system =
            MaterialSystem::new(packs, &settings, &random_state, &biomes, &mut compiler).unwrap();
        for case in golden["columns"].as_array().unwrap() {
            if case["seed"].as_i64() != Some(seed) {
                continue;
            }
            let (x, z) = (int(&case["x"]), int(&case["z"]));
            assert_eq!(
                system.surface_depth(x, z),
                int(&case["surface_depth"]),
                "{case}"
            );
            assert_eq!(
                system.surface_secondary(x, z).to_bits(),
                common::f64_bits(&case["surface_secondary"]).to_bits(),
                "{case}"
            );
            let bands: Vec<&str> = [-64, 0, 63, 100, 255]
                .iter()
                .map(|&y| system.band(x, y, z).block().name())
                .collect();
            let expected: Vec<&str> = case["bands_at_y_-64_0_63_100_255"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            assert_eq!(bands, expected, "{case}");
        }
    }
}
