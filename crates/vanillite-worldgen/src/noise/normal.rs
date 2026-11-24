//! `NormalNoise`: the `worldgen/noise` parameters, their octave layout and normalisation, and the
//! two ways Vanilla instantiates them.

use serde_json::{Map, Value};
use vanillite_base::random::{PositionalRandomFactory, RandomSource};

use super::perlin::PerlinNoise;
use super::stack::{Layer, NoiseStack, legacy_nether_biome_stack};
use super::{Kind, Noise};
use crate::Error;
use crate::interval::Interval;
use crate::random::AnyRandom;

/// `INPUT_FACTOR`: frequency ratio of the second Perlin stack.
const INPUT_FACTOR: f64 = 1.018_126_888_217_522_7;
/// `TARGET_DEVIATION`.
const TARGET_DEVIATION: f64 = 1.0 / 3.0;
/// `PerlinNoise.STANDARD_DEVIATION`.
const PERLIN_STANDARD_DEVIATION: f64 = 0.270_224_783_124_521_1;
/// `Codec.doubleRange(1.0E-5F, 1000000.0)`: the `float` bound widened to `double`.
const MIN_BASE_AMPLITUDE: f32 = 1.0e-5;
/// `MAX_AMPLITUDE`.
const MAX_AMPLITUDE: f64 = 1_000_000.0;
/// `MAX_OCTAVE_COUNT`.
const MAX_OCTAVE_COUNT: usize = 32;

/// `NormalNoise.Normalization`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Normalization {
    /// `"normalize": false`: amplitudes as given.
    Disabled,
    /// `"normalize": true` (the default): octave amplitudes scaled to sum up to the base amplitude.
    Enabled,
    /// The pre-1.18 normalisation (`"legacy"`).
    Legacy,
}

/// `NormalNoise.OctaveInfo`.
#[derive(Clone, Copy, Debug)]
struct OctaveInfo {
    octave_index: i32,
    frequency: f64,
    amplitude: f64,
}

impl OctaveInfo {
    fn abs_amplitude(&self) -> f64 {
        self.amplitude.abs()
    }
}

/// A `worldgen/noise` entry with its derived octaves (`NormalNoise`).
#[derive(Clone, Debug)]
pub struct NormalNoise {
    base_amplitude: f64,
    base_octave: i32,
    octave_count: i32,
    amplitude_modifiers: Vec<f64>,
    octaves: Vec<OctaveInfo>,
    normalization_factor: f64,
    range: Interval,
}

/// `DoubleStream.sum()`: Kahan summation with Java's final correction.
fn java_stream_sum(values: impl Iterator<Item = f64>) -> f64 {
    let (mut sum, mut compensation, mut simple) = (0.0_f64, 0.0_f64, 0.0_f64);
    for value in values {
        let tmp = value - compensation;
        let velvel = sum + tmp;
        compensation = (velvel - sum) - tmp;
        sum = velvel;
        simple += value;
    }
    let tmp = sum - compensation;
    if tmp.is_nan() && simple.is_infinite() {
        simple
    } else {
        tmp
    }
}

fn amplitude_modifier(modifiers: &[f64], index: i32) -> f64 {
    if modifiers.is_empty() {
        1.0
    } else {
        modifiers[index as usize]
    }
}

/// `buildOctaves(baseOctave, baseAmplitude, octaveCount, normalize, amplitudeModifiers)`.
fn build_octaves(
    base_octave: i32,
    base_amplitude: f64,
    octave_count: i32,
    normalize: bool,
    modifiers: &[f64],
) -> Vec<OctaveInfo> {
    let mut frequency = 2.0_f64.powf(f64::from(base_octave));
    let mut amplitude = base_amplitude;
    if normalize {
        amplitude = base_amplitude
            * (0.5_f64.powf(f64::from(-(octave_count - 1)))
                / (0.5_f64.powf(f64::from(-octave_count)) - 1.0));
    }
    let mut octaves = Vec::with_capacity(octave_count as usize);
    for i in 0..octave_count {
        let modifier = amplitude_modifier(modifiers, i);
        if modifier != 0.0 {
            octaves.push(OctaveInfo {
                octave_index: base_octave + i,
                frequency,
                amplitude: amplitude * modifier,
            });
        }
        frequency *= 2.0;
        amplitude *= 0.5;
    }
    octaves
}

/// `computeNormalizationFactor(targetAmplitude, octaves)`.
fn normalization_factor(target_amplitude: f64, octaves: &[OctaveInfo]) -> f64 {
    let mut variance = 0.0;
    for octave in octaves {
        let deviation = PERLIN_STANDARD_DEVIATION * octave.abs_amplitude();
        variance += deviation * deviation;
    }
    let input_deviation = variance.sqrt();
    if input_deviation == 0.0 {
        0.0
    } else {
        let input_sum_deviation = input_deviation * 2.0_f64.sqrt();
        target_amplitude * TARGET_DEVIATION / input_sum_deviation
    }
}

/// `computeParityNormalizationFactor(baseAmplitude, octaveCount, amplitudeModifiers)`.
fn parity_normalization_factor(base_amplitude: f64, octave_count: i32, modifiers: &[f64]) -> f64 {
    let mut min_octave = i32::MAX;
    let mut max_octave = i32::MIN;
    for i in 0..octave_count {
        if amplitude_modifier(modifiers, i) != 0.0 {
            min_octave = min_octave.min(i);
            max_octave = max_octave.max(i);
        }
    }
    let span = max_octave.wrapping_sub(min_octave);
    // `parityExpectedDeviation(span)`
    let expected = 0.1 * (1.0 + 1.0 / f64::from(span.wrapping_add(1)));
    base_amplitude * 0.5 * TARGET_DEVIATION / expected
}

fn field_f64(map: &Map<String, Value>, key: &str, default: f64) -> Result<f64, Error> {
    match map.get(key) {
        None => Ok(default),
        Some(v) => v
            .as_f64()
            .ok_or_else(|| Error::new(format!("noise: {key} is not a number"))),
    }
}

/// `Codec.INT` on a JSON number (`Number.intValue()`).
fn as_int(value: &Value) -> Option<i32> {
    value
        .as_i64()
        .map(|v| v as i32)
        .or_else(|| value.as_f64().map(|v| v as i32))
}

fn field_int(map: &Map<String, Value>, key: &str, default: Option<i32>) -> Result<i32, Error> {
    match (map.get(key), default) {
        (None, Some(default)) => Ok(default),
        (None, None) => Err(Error::new(format!("noise: missing {key}"))),
        (Some(v), _) => {
            as_int(v).ok_or_else(|| Error::new(format!("noise: {key} is not a number")))
        }
    }
}

fn check_range<T: PartialOrd + std::fmt::Display>(
    key: &str,
    value: T,
    min: T,
    max: T,
) -> Result<T, Error> {
    if value >= min && value <= max {
        Ok(value)
    } else {
        Err(Error::new(format!(
            "noise: {key} {value} outside of [{min}; {max}]"
        )))
    }
}

impl NormalNoise {
    /// Parses a `worldgen/noise` entry (`NormalNoise.Parameters.CODEC`).
    pub fn from_json(json: &Value) -> Result<Self, Error> {
        let map = json
            .as_object()
            .ok_or_else(|| Error::new("noise: not an object"))?;
        let base_amplitude = check_range(
            "base_amplitude",
            field_f64(map, "base_amplitude", 1.0)?,
            f64::from(MIN_BASE_AMPLITUDE),
            MAX_AMPLITUDE,
        )?;
        let base_octave =
            check_range("base_octave", field_int(map, "base_octave", None)?, -32, 32)?;
        let octave_count = check_range(
            "octave_count",
            field_int(map, "octave_count", Some(1))?,
            1,
            MAX_OCTAVE_COUNT as i32,
        )?;
        let normalize = match map.get("normalize") {
            None => Normalization::Enabled,
            Some(Value::Bool(true)) => Normalization::Enabled,
            Some(Value::Bool(false)) => Normalization::Disabled,
            // `Codec.BOOL` on a number: `byteValue() != 0`.
            Some(v @ Value::Number(_)) if as_int(v).is_some_and(|i| i as i8 != 0) => {
                Normalization::Enabled
            }
            Some(Value::Number(_)) => Normalization::Disabled,
            Some(Value::String(s)) if s == "legacy" => Normalization::Legacy,
            Some(other) => {
                return Err(Error::new(format!(
                    "noise: invalid normalization type {other}"
                )));
            }
        };
        let amplitude_modifiers = match map.get("amplitude_modifiers") {
            None => Vec::new(),
            Some(Value::Array(list)) => {
                if list.len() > MAX_OCTAVE_COUNT {
                    return Err(Error::new("noise: too many amplitude_modifiers"));
                }
                list.iter()
                    .map(|v| {
                        let value = v.as_f64().ok_or_else(|| {
                            Error::new("noise: amplitude modifier is not a number")
                        })?;
                        check_range("amplitude modifier", value, 0.0, MAX_AMPLITUDE)
                    })
                    .collect::<Result<_, _>>()?
            }
            Some(_) => return Err(Error::new("noise: amplitude_modifiers is not a list")),
        };
        if !amplitude_modifiers.is_empty() && amplitude_modifiers.len() != octave_count as usize {
            return Err(Error::new(format!(
                "amplitude_modifiers had size {}, but octave_count was {octave_count}",
                amplitude_modifiers.len()
            )));
        }
        Ok(Self::new(
            base_amplitude,
            base_octave,
            octave_count,
            normalize,
            amplitude_modifiers,
        ))
    }

    /// `new NormalNoise(new Parameters(...))`.
    pub fn new(
        base_amplitude: f64,
        base_octave: i32,
        octave_count: i32,
        normalize: Normalization,
        amplitude_modifiers: Vec<f64>,
    ) -> Self {
        let octaves = build_octaves(
            base_octave,
            base_amplitude,
            octave_count,
            normalize != Normalization::Disabled,
            &amplitude_modifiers,
        );
        let mut target_amplitude = java_stream_sum(octaves.iter().map(OctaveInfo::abs_amplitude));
        let mut factor = normalization_factor(target_amplitude, &octaves);
        if normalize == Normalization::Legacy && factor != 0.0 {
            let parity =
                parity_normalization_factor(base_amplitude, octave_count, &amplitude_modifiers);
            target_amplitude *= parity / factor;
            factor = parity;
        }
        Self {
            base_amplitude,
            base_octave,
            octave_count,
            amplitude_modifiers,
            octaves,
            normalization_factor: factor,
            range: Interval::of_symmetric((target_amplitude * TARGET_DEVIATION * 6.0) as f32),
        }
    }

    /// `range()`.
    pub fn range(&self) -> Interval {
        self.range
    }

    /// `create(random)`: two Perlin stacks seeded per octave from positional forks of `random`.
    pub fn create(&self, random: &mut AnyRandom) -> Noise {
        let first = random.fork_positional();
        let second = random.fork_positional();
        let mut layers = Vec::with_capacity(self.octaves.len() * 2);
        for octave in &self.octaves {
            let seed = format!("octave_{}", octave.octave_index);
            let first_noise = PerlinNoise::new(&mut first.with_hash_of(&seed));
            let second_noise = PerlinNoise::new(&mut second.with_hash_of(&seed));
            let value_factor = (self.normalization_factor * octave.amplitude) as f32;
            layers.push(Layer {
                noise: first_noise,
                frequency: octave.frequency,
                amplitude: value_factor,
            });
            layers.push(Layer {
                noise: second_noise,
                frequency: octave.frequency * INPUT_FACTOR,
                amplitude: value_factor,
            });
        }
        Noise(Kind::Perlin(NoiseStack::new(layers)))
    }

    /// `createForLegacyNetherBiome(random)`: the octave layout of the pre-1.18 nether biome noises.
    ///
    /// Panics where Vanilla throws (parameters with positive octaves).
    pub fn create_for_legacy_nether_biome(&self, random: &mut AnyRandom) -> Noise {
        let ones;
        let amplitudes = if self.amplitude_modifiers.is_empty() {
            ones = vec![1.0; self.octave_count as usize];
            &ones
        } else {
            &self.amplitude_modifiers
        };
        let first = legacy_nether_biome_stack(random, self.base_octave, amplitudes);
        let second = legacy_nether_biome_stack(random, self.base_octave, amplitudes);
        let value_factor = (self.normalization_factor * self.base_amplitude) as f32;
        let layers = first
            .into_scaled(1.0, value_factor)
            .chain(second.into_scaled(INPUT_FACTOR, value_factor))
            .collect();
        Noise(Kind::Perlin(NoiseStack::new(layers)))
    }
}
