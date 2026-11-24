//! Biomes for generation: the biome registry data world generation reads, the multi-noise biome
//! source (`Climate`, `MultiNoiseBiomeSource`) and the zoomed biome lookup (`BiomeManager`).
//!
//! Ports `net.minecraft.world.level.biome`: [`climate`] (`Climate`), `rtree` (`Climate.RTree`),
//! [`preset`] (`MultiNoiseBiomeSourceParameterList`), [`source`] (`BiomeSource` with its
//! multi-noise, fixed and end variants, `BiomeResolver`, `ChunkGenerator.doCreateBiomes` and
//! `fillBiomesFromNoise`), [`manager`] (`BiomeManager`) and the temperature part of `Biome`.

pub mod climate;
pub mod manager;
pub mod preset;
mod rtree;
pub mod source;
mod temperature;

pub use climate::{
    ClimateSampler, Parameter, ParameterList, ParameterPoint, SearchCache, TargetPoint,
    quantize_coord, target, unquantize_coord,
};
pub use manager::{BiomeManager, obfuscate_seed};
pub use preset::Preset;
pub use source::{BiomeResolver, BiomeSource, fill_biomes_from_noise};

use serde_json::Value;
use vanillite_chunk::BiomeId;
use vanillite_datapack::LoadedPacks;

use crate::Error;

/// The biome registry.
const BIOME_REGISTRY: &str = "minecraft:worldgen/biome";
/// The carver registry (for `#tag` carver lists).
const CARVER_REGISTRY: &str = "minecraft:worldgen/carver";

/// What world generation needs of one `worldgen/biome` entry.
#[derive(Clone, Debug)]
pub struct BiomeData {
    /// Registry id, e.g. `minecraft:plains`.
    pub name: String,
    /// Protocol id (index in the registry) = the id stored in chunks.
    pub id: BiomeId,
    /// `climate_settings.temperature`.
    pub temperature: f32,
    /// `climate_settings.downfall`.
    pub downfall: f32,
    /// `climate_settings.has_precipitation`.
    pub has_precipitation: bool,
    /// `climate_settings.temperature_modifier` is `frozen`.
    pub frozen_temperature_modifier: bool,
    /// Carver ids (`worldgen/carver`) in order.
    pub carvers: Vec<String>,
    /// The full entry, for data this struct does not model.
    pub json: Value,
}

impl BiomeData {
    /// Parses a biome entry (`Biome.DIRECT_CODEC`: the climate settings are inlined at the top
    /// level, `carvers` is a carver holder set).
    fn from_json(name: &str, id: BiomeId, json: Value, packs: &LoadedPacks) -> Result<Self, Error> {
        let err = |what: &str| Error::new(format!("biome {name}: {what}"));
        let float = |field: &str| {
            json.get(field)
                .and_then(climate::json_f32)
                .ok_or_else(|| err(&format!("{field} missing")))
        };
        let temperature = float("temperature")?;
        let downfall = float("downfall")?;
        let has_precipitation = json
            .get("has_precipitation")
            .and_then(Value::as_bool)
            .ok_or_else(|| err("has_precipitation missing"))?;
        let frozen_temperature_modifier = match json.get("temperature_modifier") {
            None => false,
            Some(v) => match v.as_str() {
                Some("none") => false,
                Some("frozen") => true,
                _ => return Err(err(&format!("unknown temperature_modifier {v}"))),
            },
        };
        let carvers = carver_list(
            json.get("carvers").ok_or_else(|| err("carvers missing"))?,
            packs,
        )
        .map_err(|e| err(&e.to_string()))?;
        Ok(Self {
            name: name.to_owned(),
            id,
            temperature,
            downfall,
            has_precipitation,
            frozen_temperature_modifier,
            carvers,
            json,
        })
    }
}

/// A carver holder set: an id, a `#tag` or a list of ids.
fn carver_list(json: &Value, packs: &LoadedPacks) -> Result<Vec<String>, Error> {
    match json {
        Value::String(s) => match s.strip_prefix('#') {
            Some(tag) => packs
                .tags(CARVER_REGISTRY)
                .and_then(|tags| tags.get(tag))
                .cloned()
                .ok_or_else(|| Error::new(format!("unknown carver tag #{tag}"))),
            None => Ok(vec![s.clone()]),
        },
        Value::Array(list) => list
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| Error::new(format!("carvers: expected an id, got {v}")))
            })
            .collect(),
        _ => Err(Error::new(format!("carvers: invalid {json}"))),
    }
}

/// All biomes of the loaded data packs, indexed by protocol id.
#[derive(Clone, Debug)]
pub struct BiomeRegistry {
    biomes: Vec<BiomeData>,
}

impl BiomeRegistry {
    /// Reads `minecraft:worldgen/biome` of `packs`.
    pub fn load(packs: &LoadedPacks) -> Result<Self, Error> {
        let registry = packs
            .registry(BIOME_REGISTRY)
            .ok_or_else(|| Error::new("no biome registry"))?;
        let biomes = registry
            .entries()
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let id = BiomeId::try_from(index)
                    .map_err(|_| Error::new("too many biomes for a biome id"))?;
                let json: Value = serde_json::from_str(entry.json.get())
                    .map_err(|e| Error::new(format!("biome {}: {e}", entry.id)))?;
                BiomeData::from_json(&entry.id, id, json, packs)
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { biomes })
    }

    /// Biome by protocol id.
    pub fn get(&self, id: BiomeId) -> &BiomeData {
        &self.biomes[usize::from(id)]
    }

    /// Protocol id of a registry id.
    pub fn id_of(&self, name: &str) -> Option<BiomeId> {
        self.biomes
            .iter()
            .position(|b| b.name == name)
            .map(|i| i as BiomeId)
    }

    /// Protocol id of a registry id, or an error naming it (Vanilla's `getOrThrow`).
    pub fn require(&self, name: &str) -> Result<BiomeId, Error> {
        self.id_of(name)
            .ok_or_else(|| Error::new(format!("unknown biome {name}")))
    }

    /// All biomes in protocol id order.
    pub fn all(&self) -> &[BiomeData] {
        &self.biomes
    }
}
