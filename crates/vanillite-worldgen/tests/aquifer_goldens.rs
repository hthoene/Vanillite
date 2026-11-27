//! The noise-based aquifer against Vanilla 26.3 (`AquiferGoldens.java`): `NoiseChunk` for whole
//! overworld chunks, `computeSubstance` in `doFill` order, then a carver-style pass.

mod common;

use serde_json::Value;
use vanillite_data::block::BlockState;
use vanillite_worldgen::RandomState;
use vanillite_worldgen::aquifer::{Aquifer, AquiferFunctions, GlobalFluidPicker};
use vanillite_worldgen::density::{DensityCompiler, SamplerContext};
use vanillite_worldgen::settings::NoiseGeneratorSettings;
use vanillite_worldgen::volume::DensityVolume;

fn int(v: &Value) -> i64 {
    v.as_i64().expect("int")
}

fn id(state: Option<BlockState>) -> i64 {
    state.map_or(-1, |s| i64::from(s.0))
}

#[test]
fn aquifer_matches_vanilla() {
    let golden = common::golden("aquifer");
    let packs = common::packs();
    let settings = NoiseGeneratorSettings::load(packs, golden["settings"].as_str().unwrap())
        .expect("noise settings");
    let picker = GlobalFluidPicker::from_settings(&settings);
    let stride = int(&golden["sample_stride"]);
    let carve_top = int(&golden["carve_top"]) as i32;
    let carve_bottom = int(&golden["carve_bottom"]) as i32;
    let carve_columns: Vec<(i32, i32)> = golden["carve_columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (int(&c[0]) as i32, int(&c[1]) as i32))
        .collect();

    let mut failures = Vec::new();
    let mut current_seed = None;
    let mut world = None;
    for chunk in golden["chunks"].as_array().unwrap() {
        let seed = int(&chunk["seed"]);
        if current_seed != Some(seed) {
            current_seed = Some(seed);
            let random_state =
                RandomState::new(packs, seed, settings.legacy_random_source).unwrap();
            let mut compiler = DensityCompiler::new(&random_state);
            let final_density = compiler.compile(&settings.router.final_density);
            let functions =
                AquiferFunctions::compile(settings.aquifers.as_ref().unwrap(), &mut compiler);
            let program = compiler.finish();
            world = Some((random_state, program, final_density, functions));
        }
        let (random_state, program, final_density, functions) = world.as_ref().unwrap();
        let final_density = *final_density;

        let (chunk_x, chunk_z) = (int(&chunk["x"]) as i32, int(&chunk["z"]) as i32);
        let volume = DensityVolume::new(
            16,
            settings.height,
            16,
            chunk_x << 4,
            settings.min_y,
            chunk_z << 4,
        );
        // `new NoiseChunk(…)`: the aquifer samples the surface first, then `doFill` samples the
        // final density over the volume, all in the chunk's caching context.
        let mut ctx = SamplerContext::new(true);
        let mut aquifer = Aquifer::for_chunk(
            Some(functions),
            random_state,
            &volume,
            picker,
            program,
            &mut ctx,
            None,
        );
        let mut density = vec![0.0_f32; volume.len()];
        program.sample_volume(&mut ctx, final_density, &mut density, &volume);

        let mut fill_hash = 0_i64;
        let (mut solid, mut scheduled, mut index) = (0_i64, 0_i64, 0_i64);
        let mut samples = Vec::new();
        for z in 0..volume.size_z {
            for x in 0..volume.size_x {
                for y in (0..volume.size_y).rev() {
                    let (bx, by, bz) = (volume.block_x(x), volume.block_y(y), volume.block_z(z));
                    let d = density[volume.index(x, y, z)];
                    let state =
                        aquifer.compute_substance(program, &mut ctx, bx, by, bz, f64::from(d));
                    let schedule = aquifer.should_schedule_fluid_update();
                    fill_hash = fill_hash
                        .wrapping_mul(31)
                        .wrapping_add(id(state))
                        .wrapping_mul(31)
                        .wrapping_add(i64::from(schedule));
                    solid += i64::from(state.is_none());
                    scheduled += i64::from(schedule);
                    if index % stride == 0 {
                        samples.push(Value::from(vec![
                            Value::from(bx),
                            Value::from(by),
                            Value::from(bz),
                            Value::from(d.to_bits() as i32),
                            Value::from(id(state)),
                            Value::from(schedule),
                        ]));
                    }
                    index += 1;
                }
            }
        }

        let mut carve_hash = 0_i64;
        for &(cx, cz) in &carve_columns {
            let (bx, bz) = ((chunk_x << 4) + cx, (chunk_z << 4) + cz);
            for by in (carve_bottom..=carve_top).rev() {
                let state = aquifer.compute_substance(program, &mut ctx, bx, by, bz, 0.0);
                carve_hash = carve_hash
                    .wrapping_mul(31)
                    .wrapping_add(id(state))
                    .wrapping_mul(31)
                    .wrapping_add(i64::from(aquifer.should_schedule_fluid_update()));
            }
        }

        let ok = fill_hash == int(&chunk["fill_hash"])
            && solid == int(&chunk["solid"])
            && scheduled == int(&chunk["scheduled"])
            && Value::from(samples.clone()) == chunk["samples"]
            && carve_hash == int(&chunk["carve_hash"]);
        if !ok {
            let first_bad = samples
                .iter()
                .zip(chunk["samples"].as_array().unwrap())
                .find(|(a, b)| a != b);
            failures.push(format!(
                "seed {seed} chunk ({chunk_x}, {chunk_z}): solid {solid} vs {}, scheduled \
                 {scheduled} vs {}, carve hash {}, first differing sample (ours, Vanilla) {:?}",
                chunk["solid"],
                chunk["scheduled"],
                carve_hash == int(&chunk["carve_hash"]),
                first_bad
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
