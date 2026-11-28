//! Carvers and value/height providers against Vanilla 26.3 (`CarverGoldens.java`).

mod common;

use serde_json::Value;
use vanillite_base::ChunkPos;
use vanillite_base::random::LegacyRandom;
use vanillite_worldgen::carver::providers::{FloatProvider, HeightProvider, IntProvider};
use vanillite_worldgen::carver::{
    CarverId, CarverPlans, CarverRandom, CarverRegistry, WorldGenerationContext, carve_chunk,
};

fn int(v: &Value) -> i64 {
    v.as_i64().expect("int")
}

fn context(json: &Value) -> WorldGenerationContext {
    WorldGenerationContext::new(
        int(&json["min_gen_y"]) as i32,
        int(&json["gen_depth"]) as i32,
        int(&json["sea_level"]) as i32,
    )
}

fn parse(json: &Value) -> Value {
    serde_json::from_str(json.as_str().expect("provider json")).expect("provider JSON")
}

#[test]
fn carving_masks_match_vanilla() {
    let golden = common::golden("carver");
    let registry = CarverRegistry::load(common::packs()).expect("carver registry");
    let sets: Vec<(String, Vec<CarverId>, WorldGenerationContext)> = ["overworld", "nether"]
        .iter()
        .map(|name| {
            let names: Vec<String> = golden["carver_sets"][name]
                .as_array()
                .unwrap()
                .iter()
                .map(|n| n.as_str().unwrap().to_owned())
                .collect();
            let ids = registry.ids(&names).expect("carver ids");
            ((*name).to_owned(), ids, context(&golden[*name]))
        })
        .collect();

    let mut failures = Vec::new();
    for run in golden["runs"].as_array().unwrap() {
        let set = run["set"].as_str().unwrap();
        let (_, ids, ctx) = sets.iter().find(|(n, _, _)| n == set).unwrap();
        let seed = int(&run["seed"]);
        let pos = ChunkPos::new(int(&run["x"]) as i32, int(&run["z"]) as i32);
        let lookup = |_: ChunkPos| ids.as_slice();
        let mask = carve_chunk(&registry, &lookup, seed, pos, ctx, None);
        // The same from the shared per-source plans.
        let plans = CarverPlans::default();
        assert_eq!(
            carve_chunk(&registry, &lookup, seed, pos, ctx, Some(&plans)),
            mask,
            "{set} seed {seed} chunk {pos:?}: planned carving differs"
        );

        assert_eq!(i64::from(mask.min_y()), int(&run["min_y"]));
        assert_eq!(i64::from(mask.max_y()), int(&run["max_y"]));
        let bit_hash = mask
            .set_bits()
            .fold(0_i64, |h, i| h.wrapping_mul(31).wrapping_add(i as i64));
        let mut visit_hash = 0_i64;
        let mut segments = Vec::new();
        mask.visit(|x, z, bottom, top| {
            for v in [x, z, bottom, top] {
                visit_hash = visit_hash.wrapping_mul(31).wrapping_add(i64::from(v));
            }
            if segments.len() < 6 {
                segments.push(Value::from(vec![x, z, bottom, top]));
            }
        });
        let ok = mask.count() as i64 == int(&run["count"])
            && bit_hash == int(&run["bit_hash"])
            && visit_hash == int(&run["visit_hash"])
            && Value::from(segments.clone()) == run["segments"];
        if !ok {
            failures.push(format!(
                "{set} seed {seed} chunk {pos:?}: count {} vs {}, segments {:?} vs {}",
                mask.count(),
                run["count"],
                segments,
                run["segments"]
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

fn seeds() -> [i64; 3] {
    [0, 42, -7302402383914416]
}

#[test]
fn int_providers_match_vanilla() {
    let golden = common::golden("carver");
    for entry in golden["providers"]["int"].as_array().unwrap() {
        let provider = IntProvider::from_json(&parse(&entry["json"])).expect("int provider");
        assert_eq!(
            i64::from(provider.min_inclusive()),
            int(&entry["min"]),
            "{}",
            entry["json"]
        );
        assert_eq!(
            i64::from(provider.max_inclusive()),
            int(&entry["max"]),
            "{}",
            entry["json"]
        );
        let mut samples = Vec::new();
        for seed in seeds() {
            let mut random = LegacyRandom::new(seed);
            for _ in 0..24 {
                samples.push(Value::from(provider.sample(&mut random)));
            }
        }
        assert_eq!(Value::from(samples), entry["samples"], "{}", entry["json"]);
    }
}

#[test]
fn float_providers_match_vanilla() {
    let golden = common::golden("carver");
    for entry in golden["providers"]["float"].as_array().unwrap() {
        let provider = FloatProvider::from_json(&parse(&entry["json"])).expect("float provider");
        assert_eq!(
            provider.min().to_bits(),
            common::f32_bits(&entry["min"]).to_bits()
        );
        assert_eq!(
            provider.max().to_bits(),
            common::f32_bits(&entry["max"]).to_bits()
        );
        let mut samples = Vec::new();
        for seed in seeds() {
            let mut random = LegacyRandom::new(seed);
            for _ in 0..24 {
                samples.push(Value::from(provider.sample(&mut random).to_bits() as i32));
            }
        }
        assert_eq!(Value::from(samples), entry["samples"], "{}", entry["json"]);
    }
}

#[test]
fn height_providers_match_vanilla() {
    let golden = common::golden("carver");
    let contexts = [context(&golden["overworld"]), context(&golden["nether"])];
    for entry in golden["providers"]["height"].as_array().unwrap() {
        let provider = HeightProvider::from_json(&parse(&entry["json"])).expect("height provider");
        let mut samples = Vec::new();
        for ctx in &contexts {
            for seed in seeds() {
                let mut random = LegacyRandom::new(seed);
                for _ in 0..24 {
                    samples.push(Value::from(provider.sample(&mut random, ctx)));
                }
            }
        }
        assert_eq!(Value::from(samples), entry["samples"], "{}", entry["json"]);
    }
}

#[test]
fn worldgen_random_keeps_its_gaussian_cache() {
    let golden = common::golden("carver");
    let normal = FloatProvider::from_json(&parse(&golden["providers"]["float"][4]["json"]))
        .expect("clamped normal");
    let mut samples = Vec::new();
    for seed in seeds() {
        let mut random = CarverRandom::new();
        for i in 0..8 {
            random.set_large_feature_seed(seed + i, 3, -4);
            samples.push(Value::from(normal.sample(&mut random).to_bits() as i32));
            if i % 3 == 0 {
                samples.push(Value::from(normal.sample(&mut random).to_bits() as i32));
            }
        }
    }
    assert_eq!(
        Value::from(samples),
        golden["providers"]["worldgen_random_gaussian"]
    );
}
