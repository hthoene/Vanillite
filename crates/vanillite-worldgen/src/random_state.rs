//! `RandomState`: everything a world's generator derives from the seed (noise instances and
//! positional random factories), shared read-only by all generation tasks.

use std::sync::Arc;

use rustc_hash::FxHashMap;
use serde_json::Value;
use vanillite_base::random::{LegacyRandom, PositionalRandomFactory, RandomSource};
use vanillite_datapack::LoadedPacks;

use crate::Error;
use crate::density::CompileContext;
use crate::noise::{Noise, NormalNoise};
use crate::random::{AnyPositional, AnyRandom};

/// `Noises.TEMPERATURE_NETHER`: created from a legacy random for the old nether biome layout.
const TEMPERATURE_NETHER: &str = "minecraft:nether/temperature";
/// `Noises.VEGETATION_NETHER`.
const VEGETATION_NETHER: &str = "minecraft:nether/vegetation";
/// `BlendedNoise.NOISE_SEED`.
const BLENDED_NOISE_SEED: &str = "minecraft:terrain";

/// Seed-derived state of one world's generator (`RandomState`).
///
/// All noises of the data pack are instantiated up front: each is a pure function of the seed
/// and its id, so creating them eagerly gives the same values as Vanilla's lazy cache and needs
/// no locking afterwards.
#[derive(Debug)]
pub struct RandomState {
    seed: i64,
    legacy: bool,
    random: AnyPositional,
    parameters: FxHashMap<String, NormalNoise>,
    noises: FxHashMap<String, Arc<Noise>>,
}

impl RandomState {
    /// The state for `seed` without noises: enough for work that only draws positional
    /// randoms (template placement on a live world).
    pub fn without_noises(seed: i64, legacy_random_source: bool) -> Self {
        Self {
            seed,
            legacy: legacy_random_source,
            random: AnyRandom::new(legacy_random_source, seed).fork_positional(),
            parameters: FxHashMap::default(),
            noises: FxHashMap::default(),
        }
    }

    /// The state for `seed` with the noise parameters of `packs`.
    pub fn new(packs: &LoadedPacks, seed: i64, legacy_random_source: bool) -> Result<Self, Error> {
        let random = AnyRandom::new(legacy_random_source, seed).fork_positional();
        let mut parameters = FxHashMap::default();
        if let Some(registry) = packs.registry("minecraft:worldgen/noise") {
            for entry in registry.entries() {
                let json: Value = serde_json::from_str(entry.json.get())
                    .map_err(|e| Error::new(format!("noise {}: {e}", entry.id)))?;
                parameters.insert(entry.id.clone(), NormalNoise::from_json(&json)?);
            }
        }
        let mut noises = FxHashMap::default();
        for (id, params) in &parameters {
            let noise = match id.as_str() {
                TEMPERATURE_NETHER => params.create_for_legacy_nether_biome(
                    &mut AnyRandom::Legacy(LegacyRandom::new(seed)),
                ),
                VEGETATION_NETHER => params.create_for_legacy_nether_biome(&mut AnyRandom::Legacy(
                    LegacyRandom::new(seed.wrapping_add(1)),
                )),
                _ => params.create(&mut random.with_hash_of(id)),
            };
            noises.insert(id.clone(), Arc::new(noise));
        }
        Ok(Self {
            seed,
            legacy: legacy_random_source,
            random,
            parameters,
            noises,
        })
    }

    /// The world seed.
    pub fn seed(&self) -> i64 {
        self.seed
    }

    /// Whether the legacy LCG is the world generation RNG.
    pub fn legacy_random_source(&self) -> bool {
        self.legacy
    }

    /// The positional factory forked from the seed (`RandomState.random`).
    pub fn random(&self) -> &AnyPositional {
        &self.random
    }

    /// `getOrCreateNoise(id)`. Panics for ids that are not in the data pack (Vanilla throws).
    pub fn noise(&self, id: &str) -> Arc<Noise> {
        self.noises
            .get(id)
            .cloned()
            .unwrap_or_else(|| panic!("unknown noise {id}"))
    }

    /// `getOrCreateRandomFactory(name)`: `random.fromHashOf(name).forkPositional()`.
    pub fn random_factory(&self, name: &str) -> AnyPositional {
        self.random.with_hash_of(name).fork_positional()
    }

    /// The parameters of a `worldgen/noise` id.
    pub fn noise_parameters(&self, id: &str) -> Option<&NormalNoise> {
        self.parameters.get(id)
    }

    /// `CompileContext.createRandom(id)`: the legacy terrain noise seeds from the world seed,
    /// everything else from the positional factory.
    pub fn create_random(&self, id: &str) -> AnyRandom {
        if self.legacy && id == BLENDED_NOISE_SEED {
            AnyRandom::Legacy(LegacyRandom::new(self.seed))
        } else {
            self.random.with_hash_of(id)
        }
    }

    /// `CompileContext.createEndIslandRandom()`.
    pub fn create_end_island_random(&self) -> LegacyRandom {
        LegacyRandom::new(self.seed)
    }
}

impl CompileContext for RandomState {
    fn create_noise(&self, id: &str) -> Arc<Noise> {
        self.noise(id)
    }

    fn noise_parameters(&self, id: &str) -> Option<&NormalNoise> {
        RandomState::noise_parameters(self, id)
    }

    fn create_random(&self, id: &str) -> AnyRandom {
        RandomState::create_random(self, id)
    }

    fn create_end_island_random(&self) -> LegacyRandom {
        RandomState::create_end_island_random(self)
    }
}
