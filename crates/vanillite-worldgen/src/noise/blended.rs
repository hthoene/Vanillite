//! The noise part of `BlendedNoise` (the legacy `old_blended_noise` terrain density): its three
//! smeared Perlin stacks and their range.

use super::perlin::SmearedPerlinNoise;
use super::stack::{Layer, NoiseStack};
use super::{Kind, Noise};
use crate::interval::Interval;
use crate::random::AnyRandom;

/// `BlendedNoise.BASE_SCALE`.
const BASE_SCALE: f64 = 684.412;
/// `BlendedNoise.LIMIT_FACTOR` (a `float` constant widened to `double`).
const LIMIT_FACTOR: f32 = 0.999_984_74;
/// `BlendedNoise.MAIN_FACTOR`.
const MAIN_FACTOR: f64 = 12.75;
/// `BlendedNoise.LIMIT_FIRST_OCTAVE`.
const LIMIT_FIRST_OCTAVE: i32 = -15;
/// `BlendedNoise.MAIN_FIRST_OCTAVE`.
const MAIN_FIRST_OCTAVE: i32 = -7;

/// The three noise stacks of the legacy terrain noise (`BlendedNoise.FbmSet`).
#[derive(Clone, Debug)]
pub struct BlendedFbmSet {
    pub min_limit: Noise,
    pub max_limit: Noise,
    pub main: Noise,
}

/// `BlendedNoise.createFbm(random, firstOctave, smearScaleY, valueFactor)`.
fn create_fbm(
    random: &mut AnyRandom,
    first_octave: i32,
    smear_scale_y: f64,
    mut value_factor: f64,
) -> Noise {
    assert!(first_octave <= 0, "firstOctave>0");
    let octaves = -first_octave + 1;
    let mut factor = 1.0;
    value_factor /= 2.0_f64.powf(f64::from(octaves)) - 1.0;
    let mut layers = Vec::with_capacity(octaves as usize);
    for _ in 0..octaves {
        layers.push(Layer {
            noise: SmearedPerlinNoise::new(random, smear_scale_y * factor),
            frequency: factor,
            amplitude: value_factor as f32,
        });
        factor /= 2.0;
        value_factor *= 2.0;
    }
    Noise(Kind::SmearedPerlin(NoiseStack::new(layers)))
}

/// `BlendedNoise.createFbmSet(random)` for the given `y_scale` and smear parameters.
pub fn blended_fbm_set(
    y_scale: f64,
    y_factor: f64,
    smear_scale_multiplier: f64,
    random: &mut AnyRandom,
) -> BlendedFbmSet {
    let limit_smear_scale_y = BASE_SCALE * y_scale * smear_scale_multiplier;
    let main_smear_scale_y = limit_smear_scale_y / y_factor;
    let limit_factor = f64::from(LIMIT_FACTOR);
    BlendedFbmSet {
        min_limit: create_fbm(
            random,
            LIMIT_FIRST_OCTAVE,
            limit_smear_scale_y,
            limit_factor,
        ),
        max_limit: create_fbm(
            random,
            LIMIT_FIRST_OCTAVE,
            limit_smear_scale_y,
            limit_factor,
        ),
        main: create_fbm(random, MAIN_FIRST_OCTAVE, main_smear_scale_y, MAIN_FACTOR),
    }
}

/// `BlendedNoise.range()` (`computeFbmRange` of the limit noises).
pub fn blended_noise_range(y_scale: f64, smear_scale_multiplier: f64) -> Interval {
    let smear_scale_y = BASE_SCALE * y_scale * smear_scale_multiplier;
    let octaves = -LIMIT_FIRST_OCTAVE + 1;
    let mut factor = 1.0;
    let mut value_factor = f64::from(LIMIT_FACTOR) / (2.0_f64.powf(f64::from(octaves)) - 1.0);
    let mut range = Interval::of_exact(0.0);
    for _ in 0..octaves {
        let layer = SmearedPerlinNoise::range_of(smear_scale_y * factor)
            * Interval::of_exact(value_factor as f32);
        range = range + layer;
        factor /= 2.0;
        value_factor *= 2.0;
    }
    range
}
