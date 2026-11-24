//! Noise generators (`net.minecraft.world.level.levelgen.synth`): gradient (Perlin/simplex)
//! noise, noise stacks, `NormalNoise` parameters and the legacy blended terrain noise.
//!
//! Every function keeps Java's operation order and `float`/`double` conversions, so values match
//! Vanilla bit for bit.

use serde_json::Value;

use crate::Error;
use crate::interval::Interval;
use crate::random::AnyRandom;
use crate::volume::DensityVolume;

/// A sampled noise (`net.minecraft.world.level.levelgen.synth.Noise`).
#[derive(Debug)]
pub struct Noise {
    _todo: (),
}

impl Noise {
    /// `range()`.
    pub fn range(&self) -> Interval {
        todo!()
    }

    /// `get(x, y)`.
    pub fn get2(&self, _x: f64, _y: f64) -> f32 {
        todo!()
    }

    /// `get(x, y, z)`.
    pub fn get(&self, _x: f64, _y: f64, _z: f64) -> f32 {
        todo!()
    }

    /// `addToVolume(buffer, volume, xzScale, yScale, amplitude)`.
    pub fn add_to_volume(
        &self,
        _buffer: &mut [f32],
        _volume: &DensityVolume,
        _xz_scale: f64,
        _y_scale: f64,
        _amplitude: f32,
    ) {
        todo!()
    }
}

/// `NormalNoise` parameters from `worldgen/noise` (`NormalNoise.Parameters`).
#[derive(Debug)]
pub struct NormalNoise {
    _todo: (),
}

impl NormalNoise {
    /// Parses a `worldgen/noise` entry.
    pub fn from_json(_json: &Value) -> Result<Self, Error> {
        todo!()
    }

    /// `range()`.
    pub fn range(&self) -> Interval {
        todo!()
    }

    /// `create(random)`.
    pub fn create(&self, _random: &mut AnyRandom) -> Noise {
        todo!()
    }

    /// `createForLegacyNetherBiome(random)`.
    pub fn create_for_legacy_nether_biome(&self, _random: &mut AnyRandom) -> Noise {
        todo!()
    }
}

impl Noise {
    /// `new SimplexNoise(random, discardNoiseOffset)` (the end islands use it with `true`).
    pub fn simplex(_random: &mut AnyRandom, _discard_noise_offset: bool) -> Noise {
        todo!()
    }
}

/// The three noise stacks of the legacy terrain noise (`BlendedNoise.FbmSet`).
#[derive(Debug)]
pub struct BlendedFbmSet {
    pub min_limit: Noise,
    pub max_limit: Noise,
    pub main: Noise,
}

/// `BlendedNoise.createFbmSet(random)` for the given `y_scale` and smear parameters.
pub fn blended_fbm_set(
    _y_scale: f64,
    _y_factor: f64,
    _smear_scale_multiplier: f64,
    _random: &mut AnyRandom,
) -> BlendedFbmSet {
    todo!()
}

/// `BlendedNoise.range()`.
pub fn blended_noise_range(_y_scale: f64, _smear_scale_multiplier: f64) -> Interval {
    todo!()
}
