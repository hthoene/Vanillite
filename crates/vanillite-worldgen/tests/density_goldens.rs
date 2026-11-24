//! Density function parity with Vanilla 26.3 (`DensityGoldens.java`): ranges, domain axes,
//! uncached block values, volumes with fresh caching contexts, and one shared caching context
//! that mixes volumes and single values like a `NoiseChunk`.

mod common;

use serde_json::Value;
use vanillite_worldgen::RandomState;
use vanillite_worldgen::density::{
    DensityCompiler, DensityFunction, DensityParser, FnId, Program, SamplerContext,
};
use vanillite_worldgen::settings::NoiseGeneratorSettings;
use vanillite_worldgen::volume::DensityVolume;

fn ints(v: &Value) -> Vec<i64> {
    v.as_array()
        .expect("array")
        .iter()
        .map(|x| x.as_i64().expect("int"))
        .collect()
}

fn pos(v: &Value) -> [i32; 3] {
    let p = ints(v);
    [p[0] as i32, p[1] as i32, p[2] as i32]
}

fn volume(v: &Value) -> DensityVolume {
    let a: Vec<i32> = ints(v).into_iter().map(|x| x as i32).collect();
    DensityVolume::with_step(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8])
}

/// The golden's 64-bit hash of all float bits: `hash = hash * 31 + bits`.
fn hash(values: &[f32]) -> i64 {
    values.iter().fold(0i64, |h, v| {
        h.wrapping_mul(31)
            .wrapping_add(i64::from(v.to_bits() as i32))
    })
}

/// Compares a sampled volume with a golden `{hash, samples}` entry.
fn check_volume(golden: &Value, values: &[f32], what: &str, failures: &mut Vec<String>) {
    for sample in golden["samples"].as_array().expect("samples") {
        let index = sample[0].as_u64().expect("index") as usize;
        let expected = common::f32_bits(&sample[1]);
        if values[index].to_bits() != expected.to_bits() {
            failures.push(format!(
                "{what}: index {index}: got {} expected {expected}",
                values[index]
            ));
            return;
        }
    }
    if hash(values) != golden["hash"].as_i64().expect("hash") {
        failures.push(format!("{what}: hash differs (samples match)"));
    }
}

/// The functions of a golden case, by golden name.
fn function(
    settings: &NoiseGeneratorSettings,
    parser: &mut DensityParser<'_>,
    inline: &[Value],
    name: &str,
) -> DensityFunction {
    if let Some(id) = name.strip_prefix("minecraft:") {
        return parser.reference(id).expect("registry function");
    }
    if let Some(index) = name.strip_prefix("inline/") {
        let json = inline[index.parse::<usize>().expect("inline index")]
            .as_str()
            .expect("inline function");
        let json: Value = serde_json::from_str(json).expect("inline function JSON");
        return parser.parse(&json).expect("inline function");
    }
    let r = &settings.router;
    let a = settings.aquifers.as_ref();
    match name {
        "router/temperature" => r.temperature.clone(),
        "router/vegetation" => r.vegetation.clone(),
        "router/continents" => r.continents.clone(),
        "router/erosion" => r.erosion.clone(),
        "router/depth" => r.depth.clone(),
        "router/ridges" => r.ridges.clone(),
        "router/chunk_surface_level" => r.chunk_surface_level.clone(),
        "router/final_density" => r.final_density.clone(),
        "aquifers/barrier" => a.expect("aquifers").barrier.clone(),
        "aquifers/fluid_level_floodedness" => a.expect("aquifers").fluid_level_floodedness.clone(),
        "aquifers/fluid_level_spread" => a.expect("aquifers").fluid_level_spread.clone(),
        "aquifers/lava" => a.expect("aquifers").lava.clone(),
        "aquifers/exclusion" => a.expect("aquifers").exclusion.clone(),
        "aquifers/surface_level" => a.expect("aquifers").surface_level.clone(),
        _ => panic!("unknown golden function {name}"),
    }
}

fn sample_volume(
    program: &Program,
    ctx: &mut SamplerContext,
    f: FnId,
    v: &DensityVolume,
) -> Vec<f32> {
    let mut out = vec![0.0; v.len()];
    program.sample_volume(ctx, f, &mut out, v);
    out
}

#[test]
fn density_goldens() {
    let packs = common::packs();
    let golden = common::golden("density");
    let points: Vec<[i32; 3]> = golden["points"]
        .as_array()
        .expect("points")
        .iter()
        .map(pos)
        .collect();
    let inline = golden["inline"].as_array().expect("inline functions");
    let mut failures = Vec::new();
    let mut checked = 0usize;

    for case in golden["cases"].as_array().expect("cases") {
        let settings_id = case["settings"].as_str().expect("settings");
        let seed = case["seed"].as_i64().expect("seed");
        let label = format!("{settings_id} seed {seed}");
        let settings = NoiseGeneratorSettings::load(packs, settings_id).expect("noise settings");
        let state =
            RandomState::new(packs, seed, settings.legacy_random_source).expect("random state");
        let mut parser = DensityParser::new(packs);
        let mut compiler = DensityCompiler::new(&state);
        let entries = case["functions"].as_array().expect("functions");
        let mut ids = Vec::new();
        for entry in entries {
            let name = entry["name"].as_str().expect("name");
            let f = function(&settings, &mut parser, inline, name);
            let id = compiler.compile(&f);
            ids.push((name, f, id));
        }
        let program = compiler.finish();
        let volumes: Vec<DensityVolume> = case["volumes"]
            .as_array()
            .expect("volumes")
            .iter()
            .map(volume)
            .collect();

        for (entry, (name, f, id)) in entries.iter().zip(&ids) {
            let what = format!("{label} {name}");
            let range = f.range();
            let expected_range = (
                common::f32_bits(&entry["range"][0]),
                common::f32_bits(&entry["range"][1]),
            );
            if range.min().to_bits() != expected_range.0.to_bits()
                || range.max().to_bits() != expected_range.1.to_bits()
            {
                failures.push(format!(
                    "{what}: range [{}, {}] expected [{}, {}]",
                    range.min(),
                    range.max(),
                    expected_range.0,
                    expected_range.1
                ));
            }
            if i64::from(f.domain_axes()) != entry["axes"].as_i64().expect("axes") {
                failures.push(format!("{what}: domain axes {}", f.domain_axes()));
            }
            let mut uncached = SamplerContext::new(false);
            for (p, expected) in points
                .iter()
                .zip(entry["values"].as_array().expect("values"))
            {
                let value = program.sample_value(&mut uncached, *id, p[0], p[1], p[2]);
                let expected = common::f32_bits(expected);
                checked += 1;
                if value.to_bits() != expected.to_bits() {
                    failures.push(format!(
                        "{what}: value at {p:?}: got {value} expected {expected}"
                    ));
                }
            }
            for (v, expected) in volumes
                .iter()
                .zip(entry["volumes"].as_array().expect("volumes"))
            {
                let mut ctx = SamplerContext::new(true);
                let values = sample_volume(&program, &mut ctx, *id, v);
                checked += 1;
                check_volume(
                    expected,
                    &values,
                    &format!("{what} volume {v:?}"),
                    &mut failures,
                );
            }
        }

        // One shared caching context, like a `NoiseChunk`.
        let chunk = volume(&case["sequence_volume"]);
        let sequence_points: Vec<[i32; 3]> = case["sequence_points"]
            .as_array()
            .expect("points")
            .iter()
            .map(pos)
            .collect();
        let mut shared = SamplerContext::new(true);
        let min_y = settings.min_y;
        for (step, (name, _, id)) in case["sequence"]
            .as_array()
            .expect("sequence")
            .iter()
            .zip(&ids)
        {
            let what = format!("{label} sequence {name}");
            let before = program.sample_value(&mut shared, *id, 41, min_y + 33, -41);
            if before.to_bits() != common::f32_bits(&step["before"][0]).to_bits() {
                failures.push(format!("{what}: value before the volume: {before}"));
            }
            let values = sample_volume(&program, &mut shared, *id, &chunk);
            check_volume(
                &step["volume"],
                &values,
                &format!("{what} volume"),
                &mut failures,
            );
            for (p, expected) in sequence_points
                .iter()
                .zip(step["after"].as_array().expect("after"))
            {
                let value = program.sample_value(&mut shared, *id, p[0], p[1], p[2]);
                let expected = common::f32_bits(expected);
                checked += 1;
                if value.to_bits() != expected.to_bits() {
                    failures.push(format!(
                        "{what}: value at {p:?}: got {value} expected {expected}"
                    ));
                }
            }
        }
    }

    assert!(
        failures.is_empty(),
        "{} of {checked} density checks failed:\n{}",
        failures.len(),
        failures
            .iter()
            .take(60)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
