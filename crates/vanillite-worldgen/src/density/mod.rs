//! Density functions: parsing, the optimising compiler, the backend-neutral program (IR) and the
//! CPU interpreter.
//!
//! # Pipeline
//!
//! 1. [`DensityParser`] turns `worldgen/density_function` JSON into a [`DensityFunction`] tree;
//!    references to registry entries stay references (Vanilla's `HolderHolder`).
//! 2. [`DensityCompiler`] applies Vanilla's rewrite rules (`DensityFunctionCompiler`: inline
//!    references, deduplicate `cache` markers by input, slice uniform axes) and lowers every
//!    function into one shared [`Program`]: a flat list of typed instructions with explicit cache
//!    cells, noise handles and spline tables. The program is plain data and does not know how it
//!    is executed, so another backend (SIMD, a GPU shader) can be generated from it; the CPU
//!    interpreter here is the reference that parity goldens check any backend against.
//! 3. [`Program::sample_volume`] runs an instruction tree over a whole [`DensityVolume`] per
//!    instruction (batched interpretation, Vanilla 26.3's `sampleVolume`), and
//!    [`Program::sample_value`] runs it for one block (`sampleValue`). Both follow Vanilla's
//!    specialised samplers exactly, including where the two paths round differently.

use std::sync::Arc;

use serde_json::Value;
use vanillite_base::random::LegacyRandom;
use vanillite_datapack::LoadedPacks;

use crate::Error;
use crate::interval::Interval;
use crate::noise::{Noise, NormalNoise};
use crate::random::AnyRandom;
use crate::volume::DensityVolume;

/// A parsed density function (`DensityFunction`): immutable, cheap to clone, compared and hashed
/// structurally with references compared by id, like Vanilla's records and holders.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DensityFunction {
    _todo: Arc<()>,
}

impl DensityFunction {
    /// `range()`.
    pub fn range(&self) -> Interval {
        todo!()
    }
}

/// Parses density functions, resolving registry references against loaded data packs.
#[derive(Debug)]
pub struct DensityParser<'a> {
    _packs: &'a LoadedPacks,
}

impl<'a> DensityParser<'a> {
    /// A parser over the `worldgen/density_function` and `worldgen/noise` registries of `packs`.
    pub fn new(packs: &'a LoadedPacks) -> Self {
        Self { _packs: packs }
    }

    /// Parses an inline density function or a reference string (`DensityFunction.CODEC`).
    pub fn parse(&mut self, _json: &Value) -> Result<DensityFunction, Error> {
        todo!()
    }

    /// The registry entry `id` as a reference (`HolderHolder`).
    pub fn reference(&mut self, _id: &str) -> Result<DensityFunction, Error> {
        todo!()
    }
}

/// What compiling needs from the world (`DensityFunction.CompileContext`), implemented by
/// [`crate::RandomState`].
pub trait CompileContext {
    /// `createNoiseSampler(holder)`: the noise instance for a `worldgen/noise` id.
    fn create_noise(&self, id: &str) -> Arc<Noise>;
    /// The parameters of a `worldgen/noise` id (for ranges).
    fn noise_parameters(&self, id: &str) -> Option<&NormalNoise>;
    /// `createRandom(id)`.
    fn create_random(&self, id: &str) -> AnyRandom;
    /// `createEndIslandRandom()`.
    fn create_end_island_random(&self) -> LegacyRandom;
}

/// Handle of a compiled function inside a [`Program`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FnId(pub u32);

/// `DensityFunctionCompiler`: compiles functions into one shared [`Program`].
pub struct DensityCompiler<'a> {
    _ctx: &'a dyn CompileContext,
}

impl std::fmt::Debug for DensityCompiler<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DensityCompiler").finish_non_exhaustive()
    }
}

impl<'a> DensityCompiler<'a> {
    /// A compiler with an empty program.
    pub fn new(ctx: &'a dyn CompileContext) -> Self {
        Self { _ctx: ctx }
    }

    /// `getSampler(function)`: compiles `function` once; equal functions share one id.
    pub fn compile(&mut self, _function: &DensityFunction) -> FnId {
        todo!()
    }

    /// The finished program.
    pub fn finish(self) -> Program {
        todo!()
    }
}

/// The compiled, backend-neutral program of all density functions of a world.
#[derive(Debug)]
pub struct Program {
    _todo: (),
}

impl Program {
    /// `DensitySampler.sampleValue(context, x, y, z)`.
    pub fn sample_value(
        &self,
        _ctx: &mut SamplerContext,
        _f: FnId,
        _x: i32,
        _y: i32,
        _z: i32,
    ) -> f32 {
        todo!()
    }

    /// `DensitySampler.sampleVolume(context, buffer, volume)`; `out.len()` is `volume.len()`.
    pub fn sample_volume(
        &self,
        _ctx: &mut SamplerContext,
        _f: FnId,
        _out: &mut [f32],
        _volume: &DensityVolume,
    ) {
        todo!()
    }
}

/// `SamplerContext`: per-task cache cells and scratch buffers.
///
/// A context with caches enabled is created per chunk (Vanilla's `NoiseChunk`); cached values
/// depend on what was sampled before, exactly as in Vanilla, so the same calls must happen in
/// the same order.
#[derive(Debug)]
pub struct SamplerContext {
    _todo: (),
}

impl SamplerContext {
    /// A context with caches (`enableCaches()`) or without (`EMPTY_UNCACHED`).
    pub fn new(_caching: bool) -> Self {
        todo!()
    }
}
