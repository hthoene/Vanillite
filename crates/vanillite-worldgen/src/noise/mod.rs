//! Noise generators (`net.minecraft.world.level.levelgen.synth`): gradient (Perlin/simplex)
//! noise, noise stacks, `NormalNoise` parameters and the legacy blended terrain noise.
//!
//! Ports `GradientNoise`, `PerlinNoise`, `SmearedPerlinNoise`, `SimplexNoise`, `NoiseStack`,
//! `NormalNoise`, `LegacyFbmInitializer` and the noise part of `BlendedNoise`
//! (`createFbmSet`, `createFbm`, `computeFbmRange`). Every function keeps Java's operation order
//! and `float`/`double` conversions, so values match Vanilla bit for bit. `NoiseUtils` has no
//! users in Vanilla outside debug output and is not ported.
//!
//! A [`Noise`] is a closed set of concrete kinds: the kind is matched once per call, and the
//! batched [`Noise::add_to_volume`] loops run monomorphised inside.

mod blended;
mod gradient;
mod normal;
mod perlin;
mod simplex;
mod stack;

pub use blended::{BlendedFbmSet, blended_fbm_set, blended_noise_range};
pub use normal::{NormalNoise, Normalization};

use crate::interval::Interval;
use crate::random::AnyRandom;
use crate::volume::DensityVolume;
use perlin::{PerlinNoise, SmearedPerlinNoise};
use simplex::SimplexNoise;
use stack::{NoiseStack, Octave};

/// A sampled noise (`net.minecraft.world.level.levelgen.synth.Noise`).
#[derive(Clone, Debug)]
pub struct Noise(Kind);

/// The noise kinds Vanilla instantiates.
#[derive(Clone, Debug)]
enum Kind {
    /// A stack of Perlin octaves (`NormalNoise`, legacy nether biome noise).
    Perlin(NoiseStack<PerlinNoise>),
    /// A stack of smeared Perlin octaves (`BlendedNoise`).
    SmearedPerlin(NoiseStack<SmearedPerlinNoise>),
    /// A single simplex noise (end islands); boxed, it is much larger than a stack handle.
    Simplex(Box<SimplexNoise>),
}

macro_rules! dispatch {
    ($self:expr, $n:ident => $e:expr) => {
        match &$self.0 {
            Kind::Perlin($n) => $e,
            Kind::SmearedPerlin($n) => $e,
            Kind::Simplex($n) => $e,
        }
    };
}

impl Noise {
    /// `range()`.
    pub fn range(&self) -> Interval {
        dispatch!(self, n => n.range())
    }

    /// `get(x, y)`.
    pub fn get2(&self, x: f64, y: f64) -> f32 {
        dispatch!(self, n => n.get2(x, y))
    }

    /// `get(x, y, z)`.
    pub fn get(&self, x: f64, y: f64, z: f64) -> f32 {
        dispatch!(self, n => n.get(x, y, z))
    }

    /// `addToVolume(buffer, volume, xzScale, yScale, amplitude)`: adds `amplitude` times the
    /// noise at every point of `volume` (block coordinates scaled by `xz_scale`/`y_scale`) to
    /// `buffer`, in the volume's index order. `buffer` must hold at least `volume.len()` values.
    pub fn add_to_volume(
        &self,
        buffer: &mut [f32],
        volume: &DensityVolume,
        xz_scale: f64,
        y_scale: f64,
        amplitude: f32,
    ) {
        dispatch!(self, n => n.add_to_volume(buffer, volume, xz_scale, y_scale, amplitude));
    }

    /// [`Self::add_to_volume`] where only the points of `mask` (same index order as `buffer`)
    /// matter: they get the same values as unmasked, the others may or may not be added to.
    pub fn add_to_volume_masked(
        &self,
        buffer: &mut [f32],
        volume: &DensityVolume,
        xz_scale: f64,
        y_scale: f64,
        amplitude: f32,
        mask: &[bool],
    ) {
        match &self.0 {
            Kind::Perlin(n) => {
                n.add_to_volume_masked(buffer, volume, xz_scale, y_scale, amplitude, mask)
            }
            Kind::SmearedPerlin(n) => {
                n.add_to_volume_masked(buffer, volume, xz_scale, y_scale, amplitude, mask);
            }
            Kind::Simplex(n) => n.add_to_volume(buffer, volume, xz_scale, y_scale, amplitude),
        }
    }

    /// `new SimplexNoise(random, discardNoiseOffset)` (the end islands use it with `true`).
    pub fn simplex(random: &mut AnyRandom, discard_noise_offset: bool) -> Noise {
        Noise(Kind::Simplex(Box::new(SimplexNoise::new(
            random,
            discard_noise_offset,
        ))))
    }
}
