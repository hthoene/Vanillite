//! Biomes for generation: the biome registry data world generation reads, the multi-noise biome
//! source (`Climate`, `MultiNoiseBiomeSource`) and the zoomed biome lookup (`BiomeManager`).

use serde_json::Value;
use vanillite_chunk::BiomeId;
use vanillite_datapack::LoadedPacks;

use crate::Error;

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

/// All biomes of the loaded data packs, indexed by protocol id.
#[derive(Clone, Debug)]
pub struct BiomeRegistry {
    biomes: Vec<BiomeData>,
}

impl BiomeRegistry {
    /// Reads `minecraft:worldgen/biome` of `packs`.
    pub fn load(_packs: &LoadedPacks) -> Result<Self, Error> {
        todo!()
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

    /// All biomes in protocol id order.
    pub fn all(&self) -> &[BiomeData] {
        &self.biomes
    }
}
