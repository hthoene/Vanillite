//! The compiled, backend-neutral density program: a flat list of typed instructions.
//!
//! Each [`Op`] mirrors one of Vanilla 26.3's specialised `DensitySampler` classes and refers to
//! its inputs by instruction index. Everything is plain data (numbers, indices, flat tables and
//! shared noise instances), so other backends can be generated from a [`Program`]; the CPU
//! interpreter in `exec.rs` is the reference implementation.

use std::sync::Arc;

use super::function::{Axis, DistanceMetric, RoundMode};
use super::spline::SplineProgram;
use crate::noise::Noise;

/// Index of an instruction in [`Program::ops`].
pub type OpIndex = u32;

/// A value a sampling context can bind (`ContextBoundSampler` keys). Without a binding the
/// instruction yields its fallback constant, as Vanilla does with an empty `ContextMap`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ContextField {
    /// `Blender.ALPHA_KEY` (`blend_alpha`, fallback 1).
    BlendAlpha,
    /// `Blender.OFFSET_KEY` (`blend_offset`, fallback 0).
    BlendOffset,
    /// `Beardifier.CONTEXT_KEY` (`beardifier`, fallback 0).
    Beardifier,
}

/// How a gradient maps its coordinate (`GradientFunction` samplers).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GradientTiling {
    /// `ClampedSampler`: the coordinate is clamped to `[min, max]`.
    Clamped { min: i32, max: i32 },
    /// `RepeatSampler`: `floorMod(coordinate - from, range)`.
    Repeat { range: i32 },
    /// `MirroredRepeatSampler`.
    MirroredRepeat { range: i32 },
}

/// `IntervalSelectFunction.Sampler` with more than one threshold.
#[derive(Clone, Debug, PartialEq)]
pub struct IntervalSelectTable {
    /// Ascending thresholds; `functions.len() == thresholds.len() + 1`.
    pub thresholds: Vec<f32>,
    pub functions: Vec<OpIndex>,
}

/// One instruction: a Vanilla sampler with its inputs as instruction indices.
#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    /// `ConstantFunction.Sampler`.
    Constant { value: f32 },
    /// `ContextBoundSampler(key, ConstantFunction.Sampler(fallback))`.
    ContextBound { field: ContextField, fallback: f32 },
    /// `NoiseFunction.Sampler`: `noise.get(x * xz_scale, y * y_scale, z * xz_scale)`.
    Noise {
        noise: u32,
        xz_scale: f64,
        y_scale: f64,
    },
    /// `NoiseFunction.ShiftedXzSampler`.
    ShiftedNoiseXz {
        shift_x: OpIndex,
        shift_z: OpIndex,
        noise: u32,
        xz_scale: f64,
        y_scale: f64,
    },
    /// `NoiseFunction.ShiftedXyzSampler`.
    ShiftedNoiseXyz {
        shift_x: OpIndex,
        shift_y: OpIndex,
        shift_z: OpIndex,
        noise: u32,
        xz_scale: f64,
        y_scale: f64,
    },
    /// `ShiftNoiseFunction.ShiftB`'s sampler: `noise.get(z / 4, x / 4, 0) * 4`, sampled over a
    /// transposed volume.
    ShiftB { noise: u32 },
    /// `EndIslandFunction.Sampler` over a simplex noise.
    EndIslands { noise: u32 },
    /// `DistanceToPointFunction.Sampler`.
    DistanceToPoint {
        x: i32,
        y: i32,
        z: i32,
        metric: DistanceMetric,
    },
    /// `GradientFunction` samplers: `from_value + relative * factor`.
    Gradient {
        axis: Axis,
        tiling: GradientTiling,
        from_coordinate: i32,
        from_value: f32,
        factor: f32,
    },
    /// `BinaryFunction.AddSampler`.
    Add { left: OpIndex, right: OpIndex },
    /// `BinaryFunction.SubSampler`.
    Sub { left: OpIndex, right: OpIndex },
    /// `BinaryFunction.MulSampler` (per point: `0` when `left == 0` without sampling `right`).
    Mul { left: OpIndex, right: OpIndex },
    /// `BinaryFunction.DivSampler` (per point: `0` when `left == 0` without sampling `right`).
    Div { left: OpIndex, right: OpIndex },
    /// `BinaryFunction.MinSampler`: per point, `right` is skipped when `left <= right_min`.
    Min {
        left: OpIndex,
        right: OpIndex,
        right_min: f32,
    },
    /// `BinaryFunction.MaxSampler`: per point, `right` is skipped when `left >= right_max`.
    Max {
        left: OpIndex,
        right: OpIndex,
        right_max: f32,
    },
    /// `BinaryFunction.ConstAddSampler`: `input + value`.
    ConstAdd { input: OpIndex, value: f32 },
    /// `BinaryFunction.ConstSubSampler`: `value - input`.
    ConstSub { value: f32, input: OpIndex },
    /// `BinaryFunction.ConstMulSampler`: `input * value`.
    ConstMul { input: OpIndex, value: f32 },
    /// `BinaryFunction.ConstDivSampler`: `value / input`.
    ConstDiv { value: f32, input: OpIndex },
    /// `BinaryFunction.ConstMinSampler`.
    ConstMin { input: OpIndex, value: f32 },
    /// `BinaryFunction.ConstMaxSampler`.
    ConstMax { input: OpIndex, value: f32 },
    /// `UnaryFunction.AbsSampler`.
    Abs { input: OpIndex },
    /// `UnaryFunction.SquareSampler`.
    Square { input: OpIndex },
    /// `UnaryFunction.CubeSampler`.
    Cube { input: OpIndex },
    /// `UnaryFunction.SqrtSampler`.
    Sqrt { input: OpIndex },
    /// `UnaryFunction.LeakyReLUSampler` (`half_negative`, `quarter_negative`).
    LeakyRelu {
        input: OpIndex,
        negative_factor: f32,
    },
    /// `UnaryFunction.ReciprocalSampler`.
    Reciprocal { input: OpIndex },
    /// `UnaryFunction.NegateSampler`.
    Negate { input: OpIndex },
    /// `UnaryFunction.SqueezeSampler`.
    Squeeze { input: OpIndex },
    /// `UnaryFunction.LogSampler`.
    Log { input: OpIndex },
    /// `UnaryFunction.SignSampler`.
    Sign { input: OpIndex },
    /// `RoundFunction.IntegerMultipleSampler`.
    RoundToInteger { mode: RoundMode, input: OpIndex },
    /// `RoundFunction.Sampler`.
    Round {
        mode: RoundMode,
        input: OpIndex,
        multiple: OpIndex,
    },
    /// `PowFunction.Sampler`.
    Pow { base: OpIndex, exponent: OpIndex },
    /// `PowFunction.ConstBaseSampler`.
    PowConstBase { base: f64, exponent: OpIndex },
    /// `PowFunction.ConstExponentSampler`.
    PowConstExponent { base: OpIndex, exponent: f64 },
    /// `LerpFunction.Sampler`.
    Lerp {
        alpha: OpIndex,
        first: OpIndex,
        second: OpIndex,
    },
    /// `LerpFunction.ConstFirstSampler`.
    LerpConstFirst {
        alpha: OpIndex,
        first: f32,
        second: OpIndex,
    },
    /// `LerpFunction.ConstSecondSampler`.
    LerpConstSecond {
        alpha: OpIndex,
        first: OpIndex,
        second: f32,
    },
    /// `ClampFunction.Sampler`.
    Clamp { input: OpIndex, min: f32, max: f32 },
    /// `RangeChoiceFunction.Sampler`.
    RangeChoice {
        input: OpIndex,
        min_inclusive: f32,
        max_exclusive: f32,
        when_in_range: OpIndex,
        when_out_of_range: OpIndex,
    },
    /// `RangeChoiceFunction.ConstSampler`.
    RangeChoiceConst {
        input: OpIndex,
        min_inclusive: f32,
        max_exclusive: f32,
        when_in_range: f32,
        when_out_of_range: f32,
    },
    /// `IntervalSelectFunction.SingleThresholdSampler`.
    IntervalSelectSingle {
        input: OpIndex,
        threshold: f32,
        if_below: OpIndex,
        if_above: OpIndex,
    },
    /// `IntervalSelectFunction.Sampler`: `table` indexes [`Program::interval_selects`].
    IntervalSelect { input: OpIndex, table: u32 },
    /// `CachingDensitySampler`: cache cell `cell` of the sampling context.
    Cached { cell: u32, input: OpIndex },
    /// `BlendDensityFunction.Sampler`: the identity without a bound blender.
    BlendDensity { input: OpIndex },
    /// `InterpolatedFunction.Sampler`.
    Interpolated {
        input: OpIndex,
        cell_size_xz: i32,
        cell_size_y: i32,
        cell_size_xz_inv: f32,
        cell_size_y_inv: f32,
    },
    /// `SliceFunction.XSampler`.
    SliceX { input: OpIndex, x: i32 },
    /// `SliceFunction.YSampler`.
    SliceY { input: OpIndex, y: i32 },
    /// `SliceFunction.ZSampler`.
    SliceZ { input: OpIndex, z: i32 },
    /// `SliceFunction.XzSampler`.
    SliceXz { input: OpIndex, x: i32, z: i32 },
    /// `FindTopSurfaceFunction.Sampler` (always wrapped in a [`Op::SliceY`] at 0); volumes must
    /// have a Y size of 1.
    FindTopSurface {
        density: OpIndex,
        upper_bound: OpIndex,
        lower_bound: i32,
        cell_height: i32,
    },
    /// `SplineFunction.Sampler`: `spline` indexes [`Program::splines`].
    Spline { spline: u32 },
}

/// The compiled, backend-neutral program of all density functions of a world.
///
/// Built by [`super::DensityCompiler`]; immutable and shareable between threads. Sampling state
/// lives in a [`super::SamplerContext`].
#[derive(Debug, Default)]
pub struct Program {
    pub(super) ops: Vec<Op>,
    pub(super) noises: Vec<Arc<Noise>>,
    pub(super) splines: Vec<SplineProgram>,
    pub(super) interval_selects: Vec<IntervalSelectTable>,
    pub(super) cache_cells: u32,
}

impl Program {
    /// All instructions; a [`super::FnId`] is the index of its root instruction.
    pub fn ops(&self) -> &[Op] {
        &self.ops
    }

    /// The noise instances instructions refer to.
    pub fn noises(&self) -> &[Arc<Noise>] {
        &self.noises
    }

    /// The splines of [`Op::Spline`].
    pub fn splines(&self) -> &[SplineProgram] {
        &self.splines
    }

    /// The tables of [`Op::IntervalSelect`].
    pub fn interval_selects(&self) -> &[IntervalSelectTable] {
        &self.interval_selects
    }

    /// Number of cache cells ([`Op::Cached`] cells are below this).
    pub fn cache_cell_count(&self) -> u32 {
        self.cache_cells
    }

    pub(super) fn push(&mut self, op: Op) -> OpIndex {
        self.ops.push(op);
        (self.ops.len() - 1) as OpIndex
    }
}
