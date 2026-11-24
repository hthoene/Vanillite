//! The position-dependent temperature of `Biome`: `getTemperature` with the height adjustment,
//! the `frozen` temperature modifier and their static noises, and the checks built on it
//! (`warmEnoughToRain`, `coldEnoughToSnow`, `shouldMeltFrozenOceanIcebergSlightly`).
//!
//! Vanilla memoizes `getTemperature` per thread and biome in a 1024-entry map keyed by the block
//! position only; the computation is a pure function of the position (and the sea level, which
//! is constant per dimension), so the port computes it directly.

use std::sync::OnceLock;

use vanillite_base::random::LegacyRandom;

use super::BiomeData;
use crate::noise::Noise;
use crate::random::AnyRandom;

/// `Biome.TEMPERATURE_NOISE`, `Biome.FROZEN_TEMPERATURE_NOISE` (three octaves with frequency and
/// amplitude) and `Biome.BIOME_INFO_NOISE`.
struct Noises {
    temperature: Noise,
    frozen: [(Noise, f64, f32); 3],
    biome_info: Noise,
}

fn noises() -> &'static Noises {
    static NOISES: OnceLock<Noises> = OnceLock::new();
    NOISES.get_or_init(|| {
        let simplex = |random: &mut AnyRandom| Noise::simplex(random, true);
        let temperature = simplex(&mut AnyRandom::Legacy(LegacyRandom::new(1234)));
        let mut random = AnyRandom::Legacy(LegacyRandom::new(3456));
        let frozen = [
            (simplex(&mut random), 1.0, 0.142_857_15),
            (simplex(&mut random), 0.5, 0.285_714_3),
            (simplex(&mut random), 0.25, 0.571_428_6),
        ];
        let biome_info = simplex(&mut AnyRandom::Legacy(LegacyRandom::new(2345)));
        Noises {
            temperature,
            frozen,
            biome_info,
        }
    })
}

/// `Biome.BIOME_INFO_NOISE.get(x, z)`: the simplex noise seeded with `2345` (legacy random)
/// that flower placement (`noise_based_count`, `noise_threshold_count`) and the frozen
/// temperature modifier read.
pub fn biome_info_noise(x: f64, z: f64) -> f32 {
    noises().biome_info.get2(x, z)
}

/// `NoiseStack.get(x, y)` of `FROZEN_TEMPERATURE_NOISE`.
fn frozen_temperature_noise(x: f64, z: f64) -> f32 {
    let mut value = 0.0f32;
    for (noise, frequency, amplitude) in &noises().frozen {
        value += amplitude * noise.get2(x * frequency, z * frequency);
    }
    value
}

/// `TemperatureModifier.FROZEN.modifyTemperature(pos, baseTemperature)`.
fn frozen_modifier(x: i32, z: i32, base_temperature: f32) -> f32 {
    let (x, z) = (f64::from(x), f64::from(z));
    let large_variation = f64::from(frozen_temperature_noise(x * 0.05, z * 0.05) * 7.0);
    let edge_variation = f64::from(noises().biome_info.get2(x * 0.2, z * 0.2));
    let ice_patches = large_variation + edge_variation;
    if ice_patches < 0.3 {
        let small_variation = f64::from(noises().biome_info.get2(x * 0.09, z * 0.09));
        if small_variation < 0.8 {
            return 0.2;
        }
    }
    base_temperature
}

impl BiomeData {
    /// `getTemperature(pos, seaLevel)` (= `getHeightAdjustedTemperature`): the base temperature
    /// after the temperature modifier, lowered with noise above `seaLevel + 17`.
    pub fn temperature_at(&self, x: i32, y: i32, z: i32, sea_level: i32) -> f32 {
        let adjusted = if self.frozen_temperature_modifier {
            frozen_modifier(x, z, self.temperature)
        } else {
            self.temperature
        };
        let snow_level = sea_level.wrapping_add(17);
        if y > snow_level {
            let v = noises()
                .temperature
                .get2(f64::from(x as f32 / 8.0), f64::from(z as f32 / 8.0))
                * 8.0;
            adjusted - (v + y as f32 - snow_level as f32) * 0.05 / 40.0
        } else {
            adjusted
        }
    }

    /// `warmEnoughToRain(pos, seaLevel)`.
    pub fn warm_enough_to_rain(&self, x: i32, y: i32, z: i32, sea_level: i32) -> bool {
        self.temperature_at(x, y, z, sea_level) >= 0.15
    }

    /// `coldEnoughToSnow(pos, seaLevel)` (the material rules' `TemperatureCondition`).
    pub fn cold_enough_to_snow(&self, x: i32, y: i32, z: i32, sea_level: i32) -> bool {
        !self.warm_enough_to_rain(x, y, z, sea_level)
    }

    /// `shouldMeltFrozenOceanIcebergSlightly(pos, seaLevel)` (the frozen ocean extension of the
    /// material system).
    pub fn should_melt_frozen_ocean_iceberg_slightly(
        &self,
        x: i32,
        y: i32,
        z: i32,
        sea_level: i32,
    ) -> bool {
        self.temperature_at(x, y, z, sea_level) > 0.1
    }
}
