//! `NoiseStack` (octaves of one noise kind summed with frequency and amplitude) and
//! `LegacyFbmInitializer` (the octave layout of the old nether biome noises).
//!
//! Vanilla's `NoiseStack.Builder.build()` picks `NoiseStack.Perlin` when every layer is a
//! `PerlinNoise`, `NoiseStack.SmearedPerlin` when every layer is a `SmearedPerlinNoise`, and the
//! plain `NoiseStack` otherwise; all three compute the same values. Here the layer type is a
//! type parameter, so each stack runs its octaves' loops without dynamic dispatch.

use vanillite_base::random::RandomSource;

use super::perlin::PerlinNoise;
use crate::interval::Interval;
use crate::volume::DensityVolume;

/// One noise kind that can be a layer of a [`NoiseStack`] (the `Noise` interface).
pub(super) trait Octave {
    /// `range()`.
    fn range(&self) -> Interval;
    /// `get(x, y, z)`.
    fn get(&self, x: f64, y: f64, z: f64) -> f32;
    /// `get(x, y)`.
    fn get2(&self, x: f64, y: f64) -> f32;
    /// `addToVolume(buffer, volume, xzScale, yScale, amplitude)`.
    fn add_to_volume(
        &self,
        buffer: &mut [f32],
        volume: &DensityVolume,
        xz_scale: f64,
        y_scale: f64,
        amplitude: f32,
    );
}

/// `NoiseStack.Layer`.
#[derive(Clone, Debug)]
pub(super) struct Layer<N> {
    pub noise: N,
    pub frequency: f64,
    pub amplitude: f32,
}

/// Layers summed in order (`NoiseStack`).
#[derive(Clone, Debug)]
pub(super) struct NoiseStack<N> {
    layers: Box<[Layer<N>]>,
    range: Interval,
}

impl<N: Octave> NoiseStack<N> {
    /// `NoiseStack.Builder.build()` of the given layers.
    pub fn new(layers: Vec<Layer<N>>) -> Self {
        let mut range = Interval::of_exact(0.0);
        for layer in &layers {
            range = range + layer.noise.range() * Interval::of_exact(layer.amplitude);
        }
        Self {
            layers: layers.into_boxed_slice(),
            range,
        }
    }

    /// The layers with frequency and amplitude scaled (`Builder.addStack(stack, frequency,
    /// amplitude)`).
    pub fn into_scaled(self, frequency: f64, amplitude: f32) -> impl Iterator<Item = Layer<N>> {
        self.layers.into_iter().map(move |layer| Layer {
            noise: layer.noise,
            frequency: layer.frequency * frequency,
            amplitude: layer.amplitude * amplitude,
        })
    }
}

impl<N: Octave> Octave for NoiseStack<N> {
    fn range(&self) -> Interval {
        self.range
    }

    fn get(&self, x: f64, y: f64, z: f64) -> f32 {
        let mut value = 0.0_f32;
        for layer in &self.layers {
            let f = layer.frequency;
            value += layer.amplitude * layer.noise.get(x * f, y * f, z * f);
        }
        value
    }

    fn get2(&self, x: f64, y: f64) -> f32 {
        let mut value = 0.0_f32;
        for layer in &self.layers {
            let f = layer.frequency;
            value += layer.amplitude * layer.noise.get2(x * f, y * f);
        }
        value
    }

    fn add_to_volume(
        &self,
        buffer: &mut [f32],
        volume: &DensityVolume,
        xz_scale: f64,
        y_scale: f64,
        amplitude: f32,
    ) {
        for layer in &self.layers {
            let f = layer.frequency;
            layer.noise.add_to_volume(
                buffer,
                volume,
                xz_scale * f,
                y_scale * f,
                amplitude * layer.amplitude,
            );
        }
    }
}

/// `LegacyFbmInitializer.createForLegacyNetherBiome(random, firstOctave, amplitudes)`.
///
/// Panics where Vanilla throws (amplitudes with positive octaves).
pub(super) fn legacy_nether_biome_stack(
    random: &mut impl RandomSource,
    first_octave: i32,
    amplitudes: &[f64],
) -> NoiseStack<PerlinNoise> {
    let octaves = amplitudes.len() as i32;
    let zero_octave_index = -first_octave;
    let mut levels: Vec<Option<PerlinNoise>> = (0..octaves).map(|_| None).collect();
    let zero_octave = PerlinNoise::new(random);
    if (0..octaves).contains(&zero_octave_index) && amplitudes[zero_octave_index as usize] != 0.0 {
        levels[zero_octave_index as usize] = Some(zero_octave);
    }
    for i in (0..zero_octave_index).rev() {
        if i < octaves && amplitudes[i as usize] != 0.0 {
            levels[i as usize] = Some(PerlinNoise::new(random));
        } else {
            // `skipOctave`
            random.consume_count(262);
        }
    }
    let created = levels.iter().filter(|l| l.is_some()).count();
    let non_zero = amplitudes.iter().filter(|&&a| a != 0.0).count();
    assert!(
        created == non_zero,
        "Failed to create correct number of noise levels for given non-zero amplitudes"
    );
    assert!(
        zero_octave_index >= octaves - 1,
        "Positive octaves are temporarily disabled"
    );
    let mut factor = 2.0_f64.powf(f64::from(-zero_octave_index));
    let mut value_factor =
        2.0_f64.powf(f64::from(octaves - 1)) / (2.0_f64.powf(f64::from(octaves)) - 1.0);
    let mut layers = Vec::with_capacity(non_zero);
    for (level, &amplitude) in levels.into_iter().zip(amplitudes) {
        if let Some(noise) = level {
            layers.push(Layer {
                noise,
                frequency: factor,
                amplitude: (value_factor * amplitude) as f32,
            });
        }
        factor *= 2.0;
        value_factor /= 2.0;
    }
    NoiseStack::new(layers)
}
