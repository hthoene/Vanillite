//! Density functions: parsing, the optimising compiler, the backend-neutral program (IR) and the
//! CPU interpreter (Vanilla 26.3 `net.minecraft.world.level.levelgen.densityfunction`).
//!
//! # Pipeline
//!
//! 1. [`DensityParser`] turns `worldgen/density_function` JSON into a [`DensityFunction`] tree;
//!    references to registry entries stay references (Vanilla's `HolderHolder`).
//! 2. [`DensityCompiler`] applies Vanilla's rewrite rules (`DensityFunctionCompiler`: inline
//!    references, deduplicate `cache` markers by input, slice uniform axes) and lowers every
//!    function into one shared [`Program`]: a flat list of typed instructions ([`Op`]) with
//!    explicit cache cells, noise handles and spline tables. The program is plain data and does
//!    not know how it is executed, so another backend (SIMD, a GPU shader) can be generated from
//!    it; the CPU interpreter here is the reference that parity goldens check any backend
//!    against.
//! 3. [`Program::sample_volume`] runs an instruction tree over a whole [`DensityVolume`] per
//!    instruction (batched interpretation, Vanilla 26.3's `sampleVolume`), and
//!    [`Program::sample_value`] runs it for one block (`sampleValue`). Both follow Vanilla's
//!    specialised samplers exactly, including where the two paths round differently.
//!
//! # Context-bound functions
//!
//! `blend_alpha`, `blend_offset` and `beardifier` read per-chunk data in Vanilla (old-chunk
//! blending, structures) and `blend_density` applies the blender. The terrain stage binds the
//! structures' `Beardifier` with [`SamplerContext::set_beardifier`] (a [`ContextSampler`]);
//! old-chunk blending is not ported, so `blend_alpha`/`blend_offset` evaluate to their
//! fallbacks (1, 0) and `blend_density` is the identity.
//!
//! [`DensityVolume`]: crate::volume::DensityVolume

mod compile;
mod exec;
mod function;
mod parse;
mod program;
mod spline;

use std::sync::Arc;

use vanillite_base::random::LegacyRandom;

use crate::noise::{Noise, NormalNoise};
use crate::random::AnyRandom;

pub use compile::DensityCompiler;
pub use exec::{ContextSampler, SamplerContext};
pub use function::{Axis, DensityFunction, DistanceMetric, RoundMode};
pub use parse::DensityParser;
pub use program::{ContextField, GradientTiling, IntervalSelectTable, Op, OpIndex, Program};
pub use spline::{SplineMultipoint, SplinePoint, SplineProgram, SplineValue};

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

/// Handle of a compiled function inside a [`Program`]: the index of its root instruction in
/// [`Program::ops`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FnId(pub u32);
