//! Noise generators against Vanilla goldens (`NoiseGoldens.java`): `NormalNoise` instances of a
//! `RandomState`, the legacy nether biome noises, the end island simplex noise and the
//! `BlendedNoise` fbm stacks. Every value is compared bit for bit.

mod common;

use common::{f32_bits, f64_bits, golden, packs};
use serde_json::Value;
use vanillite_base::random::RandomSource;
use vanillite_worldgen::RandomState;
use vanillite_worldgen::density::CompileContext;
use vanillite_worldgen::interval::Interval;
use vanillite_worldgen::noise::{Noise, blended_fbm_set, blended_noise_range};
use vanillite_worldgen::random::AnyRandom;
use vanillite_worldgen::volume::DensityVolume;

/// Sample points and volumes shared by every noise of the golden.
struct Setup {
    points: Vec<[f64; 3]>,
    volumes: Vec<(DensityVolume, f64, f64)>,
    amplitude: f32,
}

impl Setup {
    fn load(root: &Value) -> Self {
        let points = root["points"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| [f64_bits(&p[0]), f64_bits(&p[1]), f64_bits(&p[2])])
            .collect();
        let volumes = root["volumes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| {
                let d: Vec<i32> = v["volume"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|c| c.as_i64().unwrap() as i32)
                    .collect();
                let volume =
                    DensityVolume::with_step(d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7], d[8]);
                (volume, f64_bits(&v["xz_scale"]), f64_bits(&v["y_scale"]))
            })
            .collect();
        Self {
            points,
            volumes,
            amplitude: f32_bits(&root["amplitude"]),
        }
    }
}

/// Collects mismatches so one run reports all of them.
#[derive(Default)]
struct Report {
    checked: usize,
    failures: Vec<String>,
}

impl Report {
    fn f32(&mut self, what: impl FnOnce() -> String, actual: f32, expected: &Value) {
        self.checked += 1;
        let expected = f32_bits(expected);
        if actual.to_bits() != expected.to_bits() {
            self.failures
                .push(format!("{}: {actual:e} != {expected:e}", what()));
        }
    }

    fn range(&mut self, what: &str, actual: Interval, expected: &Value) {
        self.f32(|| format!("{what} range min"), actual.min(), &expected[0]);
        self.f32(|| format!("{what} range max"), actual.max(), &expected[1]);
    }

    /// Range, `get`, `get2` and `add_to_volume` of `noise` against `Noise.sample` in Java.
    fn noise(&mut self, setup: &Setup, what: &str, noise: &Noise, expected: &Value) {
        self.range(what, noise.range(), &expected["range"]);
        for (i, p) in setup.points.iter().enumerate() {
            self.f32(
                || format!("{what} get{p:?}"),
                noise.get(p[0], p[1], p[2]),
                &expected["get3"][i],
            );
            self.f32(
                || format!("{what} get2({}, {})", p[0], p[2]),
                noise.get2(p[0], p[2]),
                &expected["get2"][i],
            );
        }
        for (v, (volume, xz_scale, y_scale)) in setup.volumes.iter().enumerate() {
            let mut buffer: Vec<f32> = (0..volume.len())
                .map(|j| (j % 7) as f32 * 0.125 - 0.375)
                .collect();
            noise.add_to_volume(&mut buffer, volume, *xz_scale, *y_scale, setup.amplitude);
            for (j, value) in buffer.iter().enumerate() {
                self.f32(
                    || format!("{what} volume {v} [{j}]"),
                    *value,
                    &expected["volumes"][v][j],
                );
            }
        }
    }

    fn finish(self) {
        assert!(self.checked > 0, "nothing checked");
        assert!(
            self.failures.is_empty(),
            "{} of {} values differ:\n{}",
            self.failures.len(),
            self.checked,
            self.failures[..self.failures.len().min(40)].join("\n")
        );
    }
}

fn seed_of(v: &Value) -> i64 {
    v["seed"].as_i64().unwrap()
}

#[test]
fn normal_noise_parameter_ranges() {
    let root = golden("noise");
    let state = RandomState::new(packs(), 0, false).unwrap();
    let mut report = Report::default();
    for (id, expected) in root["parameter_ranges"].as_object().unwrap() {
        let params = state
            .noise_parameters(id)
            .unwrap_or_else(|| panic!("missing noise {id}"));
        report.range(id, params.range(), expected);
    }
    report.finish();
}

#[test]
fn random_state_noises() {
    let root = golden("noise");
    let setup = Setup::load(&root);
    let mut report = Report::default();
    for entry in root["random_states"].as_array().unwrap() {
        let seed = seed_of(entry);
        let legacy = entry["legacy"].as_bool().unwrap();
        let state = RandomState::new(packs(), seed, legacy).unwrap();
        for (id, expected) in entry["noises"].as_object().unwrap() {
            let what = format!("{id} seed {seed} legacy {legacy}");
            report.noise(&setup, &what, &state.noise(id), expected);
        }
    }
    report.finish();
}

#[test]
fn legacy_nether_biome_noises() {
    let root = golden("noise");
    let setup = Setup::load(&root);
    let mut report = Report::default();
    for entry in root["legacy_nether_biome"].as_array().unwrap() {
        let seed = seed_of(entry);
        // Which random source the overworld/nether state uses does not matter for these two.
        let state = RandomState::new(packs(), seed, false).unwrap();
        for name in ["temperature", "vegetation"] {
            let id = format!("minecraft:nether/{name}");
            let what = format!("{id} seed {seed}");
            report.noise(&setup, &what, &state.noise(&id), &entry[name]);
        }
    }
    report.finish();
}

#[test]
fn end_island_simplex() {
    let root = golden("noise");
    let setup = Setup::load(&root);
    let mut report = Report::default();
    for entry in root["end_islands"].as_array().unwrap() {
        let seed = seed_of(entry);
        let state = RandomState::new(packs(), seed, true).unwrap();
        let mut random = state.create_end_island_random();
        random.consume_count(17292);
        let noise = Noise::simplex(&mut AnyRandom::Legacy(random), true);
        let what = format!("end islands seed {seed}");
        report.noise(&setup, &what, &noise, entry);
        let mut i = 0;
        for cx in (-30_i64..=30).step_by(7) {
            for cz in (-1_900_000_i64..=1_900_000).step_by(475_000) {
                let (x, z) = (cx * 13 + cz, cz - cx * 61);
                report.f32(
                    || format!("{what} get2({x}, {z})"),
                    noise.get2(x as f64, z as f64),
                    &entry["islands"][i],
                );
                i += 1;
            }
        }
    }
    report.finish();
}

#[test]
fn blended_noise_fbm_sets() {
    let root = golden("noise");
    let setup = Setup::load(&root);
    let mut report = Report::default();
    for entry in root["blended"].as_array().unwrap() {
        let seed = seed_of(entry);
        let legacy = entry["legacy"].as_bool().unwrap();
        let p: Vec<f64> = entry["params"]
            .as_array()
            .unwrap()
            .iter()
            .map(f64_bits)
            .collect();
        let (y_scale, y_factor, smear) = (p[1], p[3], p[4]);
        let state = RandomState::new(packs(), seed, legacy).unwrap();
        let mut random = state.create_random("minecraft:terrain");
        let set = blended_fbm_set(y_scale, y_factor, smear, &mut random);
        let what = format!("blended {p:?} seed {seed} legacy {legacy}");
        report.range(&what, blended_noise_range(y_scale, smear), &entry["range"]);
        for (name, noise) in [
            ("min_limit", &set.min_limit),
            ("max_limit", &set.max_limit),
            ("main", &set.main),
        ] {
            report.noise(&setup, &format!("{what} {name}"), noise, &entry[name]);
        }
    }
    report.finish();
}
