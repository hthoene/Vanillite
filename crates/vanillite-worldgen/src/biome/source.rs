//! `BiomeSource` (`MultiNoiseBiomeSource`, `FixedBiomeSource`, `TheEndBiomeSource`), its
//! resolvers (`BiomeResolver`, `createResolver`, `createUncachedResolver`,
//! `createResolverForChunk`) and the biome step of chunk generation
//! (`ChunkGenerator.doCreateBiomes`, `ChunkAccess`/`LevelChunkSection.fillBiomesFromNoise`).

use serde_json::Value;
use vanillite_chunk::BiomeId;
use vanillite_datapack::LoadedPacks;

use super::BiomeRegistry;
use super::climate::{
    ClimateCache, ClimateSampler, ParameterList, ParameterPoint, SearchCache, target,
};
use super::preset::Preset;
use crate::Error;
use crate::density::{Program, SamplerContext};
use crate::proto::ProtoChunk;
use crate::volume::DensityVolume;

/// The parameter list registry (`worldgen/multi_noise_biome_source_parameter_list`).
const PARAMETER_LIST_REGISTRY: &str = "minecraft:worldgen/multi_noise_biome_source_parameter_list";

/// `BiomeResolver`: the noise biome at quart coordinates.
///
/// Implemented by the resolvers of [`BiomeSource`] and by every
/// `FnMut(quart_x, quart_y, quart_z) -> BiomeId` closure (e.g. a lookup in generated chunks).
pub trait BiomeResolver {
    /// `getNoiseBiome(quartX, quartY, quartZ)`.
    fn noise_biome(&mut self, quart_x: i32, quart_y: i32, quart_z: i32) -> BiomeId;
}

impl<F: FnMut(i32, i32, i32) -> BiomeId> BiomeResolver for F {
    #[inline]
    fn noise_biome(&mut self, quart_x: i32, quart_y: i32, quart_z: i32) -> BiomeId {
        self(quart_x, quart_y, quart_z)
    }
}

/// The five biomes of `TheEndBiomeSource`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EndBiomes {
    pub end: BiomeId,
    pub highlands: BiomeId,
    pub midlands: BiomeId,
    pub islands: BiomeId,
    pub barrens: BiomeId,
}

/// A dimension's biome source (`BiomeSource`).
#[derive(Debug)]
pub enum BiomeSource {
    /// `minecraft:fixed`: one biome everywhere.
    Fixed(BiomeId),
    /// `minecraft:multi_noise`: the closest climate region (a preset or a direct list).
    MultiNoise(ParameterList<BiomeId>),
    /// `minecraft:the_end`: the central island and the erosion-based outer islands.
    TheEnd(EndBiomes),
}

impl BiomeSource {
    /// Parses a `biome_source` object of a chunk generator (`BiomeSource.CODEC`).
    pub fn from_json(
        json: &Value,
        packs: &LoadedPacks,
        biomes: &BiomeRegistry,
    ) -> Result<Self, Error> {
        let kind = json
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::new("biome source: type missing"))?;
        match kind.strip_prefix("minecraft:").unwrap_or(kind) {
            "fixed" => {
                let biome = json
                    .get("biome")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::new("fixed biome source: biome missing"))?;
                Ok(Self::Fixed(biomes.require(biome)?))
            }
            "the_end" => Ok(Self::TheEnd(EndBiomes {
                end: biomes.require("minecraft:the_end")?,
                highlands: biomes.require("minecraft:end_highlands")?,
                midlands: biomes.require("minecraft:end_midlands")?,
                islands: biomes.require("minecraft:small_end_islands")?,
                barrens: biomes.require("minecraft:end_barrens")?,
            })),
            "multi_noise" => {
                if let Some(preset) = json.get("preset") {
                    Self::from_preset(preset_of(preset, packs)?, biomes)
                } else {
                    let list = json
                        .get("biomes")
                        .and_then(Value::as_array)
                        .filter(|l| !l.is_empty())
                        .ok_or_else(|| Error::new("multi noise biome source: no biomes"))?;
                    let values =
                        list.iter()
                            .map(|entry| {
                                let biome = entry.get("biome").and_then(Value::as_str).ok_or_else(
                                    || Error::new("multi noise entry: biome missing"),
                                )?;
                                let point = ParameterPoint::from_json(
                                    entry.get("parameters").unwrap_or(&Value::Null),
                                )?;
                                Ok((point, biomes.require(biome)?))
                            })
                            .collect::<Result<Vec<_>, Error>>()?;
                    Ok(Self::MultiNoise(ParameterList::new(values)?))
                }
            }
            other => Err(Error::new(format!("unsupported biome source {other}"))),
        }
    }

    /// `MultiNoiseBiomeSource.createFromPreset(preset)` with the biomes of `biomes`.
    pub fn from_preset(preset: Preset, biomes: &BiomeRegistry) -> Result<Self, Error> {
        let values = preset
            .entries()
            .iter()
            .map(|(point, name)| Ok((*point, biomes.require(name)?)))
            .collect::<Result<Vec<_>, Error>>()?;
        Ok(Self::MultiNoise(ParameterList::new(values)?))
    }

    /// `possibleBiomes()`: every biome the source can return, distinct, in first-seen order.
    pub fn possible_biomes(&self) -> Vec<BiomeId> {
        let all: Vec<BiomeId> = match self {
            Self::Fixed(b) => vec![*b],
            Self::MultiNoise(list) => list.values().iter().map(|v| v.1).collect(),
            Self::TheEnd(e) => vec![e.end, e.highlands, e.midlands, e.islands, e.barrens],
        };
        let mut out = Vec::new();
        for b in all {
            if !out.contains(&b) {
                out.push(b);
            }
        }
        out
    }

    /// `createResolver(sampler)` with a sampler context of the caller's choice.
    pub fn create_resolver<'a>(
        &'a self,
        program: &'a Program,
        sampler: &'a ClimateSampler,
        ctx: SamplerContext,
    ) -> Resolver<'a> {
        Resolver {
            source: self,
            program,
            sampler,
            ctx,
            cache: SearchCache::default(),
        }
    }

    /// `createUncachedResolver(randomState)`: a resolver with an uncaching context, as the
    /// carvers use it.
    pub fn create_uncached_resolver<'a>(
        &'a self,
        program: &'a Program,
        sampler: &'a ClimateSampler,
    ) -> Resolver<'a> {
        self.create_resolver(program, sampler, SamplerContext::new(false))
    }

    /// `createCachingResolver(randomState)`.
    pub fn create_caching_resolver<'a>(
        &'a self,
        program: &'a Program,
        sampler: &'a ClimateSampler,
    ) -> Resolver<'a> {
        self.create_resolver(program, sampler, SamplerContext::new(true))
    }

    /// `createResolverForChunk(sampler, minQuart..., quartSize...)`: the multi-noise source
    /// samples the six climate functions over the whole quart grid first (`sampleVolume`, in
    /// `Climate.Sampler` order), then answers per quart from the buffers; the other sources
    /// resolve per quart.
    #[allow(clippy::too_many_arguments)]
    pub fn create_resolver_for_chunk<'a>(
        &'a self,
        program: &'a Program,
        sampler: &'a ClimateSampler,
        mut ctx: SamplerContext,
        min_quart: (i32, i32, i32),
        quart_size: (i32, i32, i32),
    ) -> ChunkResolver<'a> {
        let Self::MultiNoise(list) = self else {
            return ChunkResolver::PerQuart(self.create_resolver(program, sampler, ctx));
        };
        let volume = DensityVolume::with_step(
            quart_size.0,
            quart_size.1,
            quart_size.2,
            min_quart.0 << 2,
            min_quart.1 << 2,
            min_quart.2 << 2,
            4,
            4,
            4,
        );
        let buffers = sampler.functions().map(|f| {
            let mut buffer = vec![0.0f32; volume.len()];
            program.sample_volume(&mut ctx, f, &mut buffer, &volume);
            buffer
        });
        ChunkResolver::Sampled(SampledResolver {
            list,
            volume,
            min_quart,
            buffers,
            cache: SearchCache::default(),
        })
    }

    /// `ChunkGenerator.doCreateBiomes`: fills every noise biome of `chunk` from this source
    /// (with a caching sampler context; the blender and below-zero retrogen decorations are the
    /// identity for newly generated chunks).
    pub fn create_biomes(
        &self,
        chunk: &mut ProtoChunk,
        program: &Program,
        sampler: &ClimateSampler,
    ) {
        let pos = chunk.pos();
        let min_quart = (pos.x << 2, chunk.min_y() >> 2, pos.z << 2);
        let quart_size = (4, chunk.height() >> 2, 4);
        let mut resolver = self.create_resolver_for_chunk(
            program,
            sampler,
            SamplerContext::new(true),
            min_quart,
            quart_size,
        );
        fill_biomes_from_noise(chunk, &mut resolver);
    }

    /// `getNoiseBiome(quartX, quartY, quartZ)` through `sampler` with the caller's search cache.
    fn noise_biome(
        &self,
        program: &Program,
        sampler: &ClimateSampler,
        ctx: &mut SamplerContext,
        cache: &mut SearchCache,
        quart: (i32, i32, i32),
    ) -> BiomeId {
        let (quart_x, quart_y, quart_z) = quart;
        match self {
            Self::Fixed(biome) => *biome,
            Self::MultiNoise(list) => {
                let target = sampler.sample(program, ctx, quart_x, quart_y, quart_z);
                *list.find_value(&target, cache)
            }
            Self::TheEnd(biomes) => {
                let (block_x, block_y, block_z) = (quart_x << 2, quart_y << 2, quart_z << 2);
                let (chunk_x, chunk_z) = (block_x >> 4, block_z >> 4);
                let (cx, cz) = (i64::from(chunk_x), i64::from(chunk_z));
                if cx * cx + cz * cz <= 4096 {
                    return biomes.end;
                }
                let weird_x = chunk_x.wrapping_mul(2).wrapping_add(1).wrapping_mul(8);
                let weird_z = chunk_z.wrapping_mul(2).wrapping_add(1).wrapping_mul(8);
                let height = f64::from(program.sample_value(
                    ctx,
                    sampler.erosion,
                    weird_x,
                    block_y,
                    weird_z,
                ));
                if height > 0.25 {
                    biomes.highlands
                } else if height >= -0.0625 {
                    biomes.midlands
                } else if height < -0.21875 {
                    biomes.islands
                } else {
                    biomes.barrens
                }
            }
        }
    }
}

/// The preset of a `preset` field: a parameter list registry id or an inline
/// `{"preset": ...}` (`MultiNoiseBiomeSourceParameterList.CODEC`).
fn preset_of(json: &Value, packs: &LoadedPacks) -> Result<Preset, Error> {
    let inline;
    let entry = match json {
        Value::String(id) => {
            let entry = packs
                .registry(PARAMETER_LIST_REGISTRY)
                .and_then(|r| r.get(id))
                .ok_or_else(|| Error::new(format!("unknown parameter list {id}")))?;
            inline = serde_json::from_str::<Value>(entry.json.get())
                .map_err(|e| Error::new(format!("parameter list {id}: {e}")))?;
            &inline
        }
        other => other,
    };
    let name = entry
        .get("preset")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::new("parameter list: preset missing"))?;
    Preset::by_id(name).ok_or_else(|| Error::new(format!("Unknown preset: {name}")))
}

/// A resolver of a [`BiomeSource`] that samples the climate per quart (`createResolver`).
///
/// It owns its sampler context and its R-tree [`SearchCache`].
#[derive(Debug)]
pub struct Resolver<'a> {
    source: &'a BiomeSource,
    program: &'a Program,
    sampler: &'a ClimateSampler,
    ctx: SamplerContext,
    cache: SearchCache,
}

impl Resolver<'_> {
    /// `getNoiseBiome(quartX, 0, quartZ)` with the climate of the column taken from `climates`
    /// (sampled and stored there on a miss). The climate is a pure function of the position,
    /// so the result, and the search cache state it leaves, equal [`BiomeResolver::noise_biome`].
    pub fn noise_biome_at_zero(
        &mut self,
        quart_x: i32,
        quart_z: i32,
        climates: &ClimateCache,
    ) -> BiomeId {
        let BiomeSource::MultiNoise(list) = self.source else {
            return self.noise_biome(quart_x, 0, quart_z);
        };
        let (sampler, program, ctx) = (self.sampler, self.program, &mut self.ctx);
        let index = climates.search_at(
            quart_x,
            quart_z,
            self.cache.0,
            || sampler.sample(program, ctx, quart_x, 0, quart_z),
            |target, hint| list.search_index(target, hint),
        );
        self.cache.0 = Some(index);
        *list.value_at(index)
    }
}

impl BiomeResolver for Resolver<'_> {
    #[inline]
    fn noise_biome(&mut self, quart_x: i32, quart_y: i32, quart_z: i32) -> BiomeId {
        self.source.noise_biome(
            self.program,
            self.sampler,
            &mut self.ctx,
            &mut self.cache,
            (quart_x, quart_y, quart_z),
        )
    }
}

/// The multi-noise resolver of `createResolverForChunk`: climate buffers over the chunk's quarts.
#[derive(Debug)]
pub struct SampledResolver<'a> {
    list: &'a ParameterList<BiomeId>,
    volume: DensityVolume,
    min_quart: (i32, i32, i32),
    /// Temperature, vegetation, continents, erosion, depth, ridges.
    buffers: [Vec<f32>; 6],
    cache: SearchCache,
}

impl BiomeResolver for SampledResolver<'_> {
    /// Only quarts inside the sampled grid are valid (Vanilla indexes its buffers unchecked).
    #[inline]
    fn noise_biome(&mut self, quart_x: i32, quart_y: i32, quart_z: i32) -> BiomeId {
        let index = self.volume.index(
            quart_x - self.min_quart.0,
            quart_y - self.min_quart.1,
            quart_z - self.min_quart.2,
        );
        let [t, h, c, e, d, w] = &self.buffers;
        let target = target(t[index], h[index], c[index], e[index], d[index], w[index]);
        *self.list.find_value(&target, &mut self.cache)
    }
}

/// The resolver `createResolverForChunk` returns.
#[derive(Debug)]
pub enum ChunkResolver<'a> {
    /// Multi-noise: pre-sampled climate buffers.
    Sampled(SampledResolver<'a>),
    /// Other sources: `createResolver(sampler)`.
    PerQuart(Resolver<'a>),
}

impl BiomeResolver for ChunkResolver<'_> {
    #[inline]
    fn noise_biome(&mut self, quart_x: i32, quart_y: i32, quart_z: i32) -> BiomeId {
        match self {
            Self::Sampled(r) => r.noise_biome(quart_x, quart_y, quart_z),
            Self::PerQuart(r) => r.noise_biome(quart_x, quart_y, quart_z),
        }
    }
}

/// `ChunkAccess.fillBiomesFromNoise(resolver)`: every section bottom to top, each in
/// `LevelChunkSection.fillBiomesFromNoise` order (x, then y, then z innermost). The order is
/// part of the result, as it drives the R-tree search cache.
pub fn fill_biomes_from_noise(chunk: &mut ProtoChunk, resolver: &mut impl BiomeResolver) {
    let pos = chunk.pos();
    let (quart_min_x, quart_min_z) = (pos.x << 2, pos.z << 2);
    let min_section = chunk.min_y() >> 4;
    let sections = chunk.sections().len() as i32;
    for section_y in min_section..min_section + sections {
        let quart_min_y = section_y << 2;
        for x in 0..4 {
            for y in 0..4 {
                for z in 0..4 {
                    let (qx, qy, qz) = (quart_min_x + x, quart_min_y + y, quart_min_z + z);
                    let biome = resolver.noise_biome(qx, qy, qz);
                    chunk.set_noise_biome(qx, qy, qz, biome);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use vanillite_base::ChunkPos;

    use super::*;

    #[test]
    fn fill_order_and_positions() {
        crate::test_support::packs();
        let mut chunk = ProtoChunk::new(ChunkPos { x: -3, z: 5 }, -64, 384, 0);
        let mut calls = Vec::new();
        let biome_of = |x: i32, y: i32, z: i32| ((x * 7 + y * 3 + z) & 31) as BiomeId;
        let mut resolver = |x: i32, y: i32, z: i32| {
            calls.push((x, y, z));
            biome_of(x, y, z)
        };
        fill_biomes_from_noise(&mut chunk, &mut resolver);
        assert_eq!(calls.len(), 4 * 96 * 4);
        assert_eq!(
            &calls[..3],
            &[(-12, -16, 20), (-12, -16, 21), (-12, -16, 22)]
        );
        assert_eq!(calls[4], (-12, -15, 20));
        assert_eq!(calls[16], (-11, -16, 20));
        assert_eq!(calls[64], (-12, -12, 20));
        for &(x, y, z) in &calls {
            assert_eq!(chunk.noise_biome(x, y, z), biome_of(x, y, z));
        }
    }
}
