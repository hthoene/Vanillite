//! `NoiseGeneratorSettings` and `NoiseRouter` from `worldgen/noise_settings`.

use serde_json::Value;
use vanillite_data::block::BlockState;
use vanillite_datapack::LoadedPacks;

use crate::Error;
use crate::blocks::parse_block_state;
use crate::density::{DensityFunction, DensityParser};

/// `NoiseRouter`: the density functions the generator samples.
#[derive(Clone, Debug)]
pub struct NoiseRouter {
    pub temperature: DensityFunction,
    pub vegetation: DensityFunction,
    pub continents: DensityFunction,
    pub erosion: DensityFunction,
    pub depth: DensityFunction,
    pub ridges: DensityFunction,
    pub chunk_surface_level: DensityFunction,
    pub final_density: DensityFunction,
}

/// `Aquifer.Config`.
#[derive(Clone, Debug)]
pub struct AquiferConfig {
    pub barrier: DensityFunction,
    pub fluid_level_floodedness: DensityFunction,
    pub fluid_level_spread: DensityFunction,
    pub lava: DensityFunction,
    pub exclusion: DensityFunction,
    pub surface_level: DensityFunction,
}

/// `NoiseGeneratorSettings` (debug functions and spawn targets are not needed for generation).
#[derive(Clone, Debug)]
pub struct NoiseGeneratorSettings {
    /// `noise.min_y`.
    pub min_y: i32,
    /// `noise.height`.
    pub height: i32,
    pub default_block: BlockState,
    pub default_fluid: BlockState,
    pub router: NoiseRouter,
    /// The `material_rule` value: a `worldgen/material_rule` id or an inline rule.
    pub material_rule: Value,
    pub sea_level: i32,
    pub aquifers: Option<AquiferConfig>,
    pub legacy_random_source: bool,
}

fn field<'v>(json: &'v Value, name: &str) -> Result<&'v Value, Error> {
    json.get(name)
        .ok_or_else(|| Error::new(format!("noise settings: missing field {name}")))
}

fn int(json: &Value, name: &str) -> Result<i32, Error> {
    field(json, name)?
        .as_i64()
        .and_then(|v| i32::try_from(v).ok())
        .ok_or_else(|| Error::new(format!("noise settings: {name} is not an int")))
}

fn boolean(json: &Value, name: &str) -> Result<bool, Error> {
    field(json, name)?
        .as_bool()
        .ok_or_else(|| Error::new(format!("noise settings: {name} is not a boolean")))
}

impl NoiseGeneratorSettings {
    /// The `worldgen/noise_settings` entry `id` of `packs`.
    pub fn load(packs: &LoadedPacks, id: &str) -> Result<Self, Error> {
        let entry = packs
            .registry("minecraft:worldgen/noise_settings")
            .and_then(|r| r.get(id))
            .ok_or_else(|| Error::new(format!("unknown noise settings {id}")))?;
        let json: Value = serde_json::from_str(entry.json.get())
            .map_err(|e| Error::new(format!("noise settings {id}: {e}")))?;
        Self::from_json(&json, &mut DensityParser::new(packs))
    }

    /// Parses inline noise settings.
    pub fn from_json(json: &Value, parser: &mut DensityParser<'_>) -> Result<Self, Error> {
        let noise = field(json, "noise")?;
        let router = field(json, "noise_router")?;
        let mut df = |json: &Value, name: &str| parser.parse(field(json, name)?);
        let router = NoiseRouter {
            temperature: df(router, "temperature")?,
            vegetation: df(router, "vegetation")?,
            continents: df(router, "continents")?,
            erosion: df(router, "erosion")?,
            depth: df(router, "depth")?,
            ridges: df(router, "ridges")?,
            chunk_surface_level: df(router, "chunk_surface_level")?,
            final_density: df(router, "final_density")?,
        };
        let aquifers = match json.get("aquifers") {
            None => None,
            Some(a) => Some(AquiferConfig {
                barrier: df(a, "barrier")?,
                fluid_level_floodedness: df(a, "fluid_level_floodedness")?,
                fluid_level_spread: df(a, "fluid_level_spread")?,
                lava: df(a, "lava")?,
                exclusion: df(a, "exclusion")?,
                surface_level: df(a, "surface_level")?,
            }),
        };
        let min_y = int(noise, "min_y")?;
        let height = int(noise, "height")?;
        if height % 16 != 0 || min_y % 16 != 0 || height < 0 {
            return Err(Error::new(
                "noise settings: min_y and height must be multiples of 16",
            ));
        }
        Ok(Self {
            min_y,
            height,
            default_block: parse_block_state(field(json, "default_block")?)?,
            default_fluid: parse_block_state(field(json, "default_fluid")?)?,
            router,
            material_rule: field(json, "material_rule")?.clone(),
            sea_level: int(json, "sea_level")?,
            aquifers,
            legacy_random_source: boolean(json, "legacy_random_source")?,
        })
    }
}
