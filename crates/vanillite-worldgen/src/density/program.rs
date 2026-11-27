//! The compiled, backend-neutral density program: a flat list of typed instructions.
//!
//! Each [`Op`] mirrors one of Vanilla 26.3's specialised `DensitySampler` classes and refers to
//! its inputs by instruction index. Everything is plain data (numbers, indices, flat tables and
//! shared noise instances), so other backends can be generated from a [`Program`]; the CPU
//! interpreter in `exec.rs` is the reference implementation.

use std::sync::Arc;

use super::function::{Axis, DistanceMetric, RoundMode};
use super::spline::SplineProgram;
use crate::interval::Interval;
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
    /// A number no other finished program of the process has (0 until then): the key of
    /// caches that outlive a sampling context.
    pub(super) id: u64,
    pub(super) ops: Vec<Op>,
    pub(super) noises: Vec<Arc<Noise>>,
    pub(super) splines: Vec<SplineProgram>,
    pub(super) interval_selects: Vec<IntervalSelectTable>,
    pub(super) cache_cells: u32,
    /// Per instruction: whether sampling it at only some points of a volume saves work, i.e.
    /// it reads a noise, a spline or an interpolation other than through a slice.
    pub(super) maskable: Vec<bool>,
    /// Per cache cell: whether only the function given to [`Self::set_masked_cache_root`]
    /// reads it, so its volume cache may hold values for only some points (no other function
    /// samples it point by point and could see that cache).
    pub(super) partial_cells: Vec<bool>,
    /// Per instruction: whether its value depends on the position other than through noises
    /// and splines (a gradient, a slice, a bound field): only then can bounds at a position be
    /// tighter than its static range.
    pub(super) positional: Vec<bool>,
    /// Per instruction: how many noise instructions it reads, counting shared ones once per
    /// reader (saturating): a measure of what sampling it costs.
    pub(super) noise_ops: Vec<u16>,
    /// Per instruction: the static range of its function where the compiler knows it
    /// (`DensityFunction.range()`), else not an interval.
    pub(super) ranges: Vec<Interval>,
    /// The function given to [`Self::set_masked_cache_root`] with the cell sizes (XZ, Y) of its
    /// interpolations, if it may be sampled over only the lower part of a volume: its
    /// interpolations share one lattice, and every cache cell that would then hold another
    /// volume (those not behind a Y slice) is a partial cell, which nothing else reads.
    pub(super) root_lattice: Option<(super::FnId, i32, i32)>,
    /// Per instruction: whether leaving out its sampling (or sampling it at only some points)
    /// cannot be noticed by anything but its own reader: every cache cell it reads is a
    /// partial cell (see `partial_cells`), or it reads none.
    pub(super) skippable: Vec<bool>,
    /// Per instruction: whether it samples an input over another volume than its own
    /// (interpolation, slices, surface search, `shift_b`), so its cache cells may end up
    /// holding other volumes depending on the order of evaluation.
    pub(super) other_volumes: Vec<bool>,
}

impl Program {
    /// A number no other finished program of the process has.
    pub fn id(&self) -> u64 {
        self.id
    }

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
        let maskable = self.is_maskable(&op);
        let other_volumes = match op {
            Op::Interpolated { .. }
            | Op::SliceX { .. }
            | Op::SliceY { .. }
            | Op::SliceZ { .. }
            | Op::SliceXz { .. }
            | Op::FindTopSurface { .. }
            | Op::ShiftB { .. } => true,
            _ => {
                let mut inputs = Vec::new();
                self.inputs_of(&op, &mut inputs);
                inputs.iter().any(|&input| {
                    self.other_volumes
                        .get(input as usize)
                        .copied()
                        .unwrap_or(true)
                })
            }
        };
        let skippable = self.is_skippable(&op);
        let positional = match op {
            Op::Gradient { .. }
            | Op::SliceX { .. }
            | Op::SliceY { .. }
            | Op::SliceZ { .. }
            | Op::SliceXz { .. }
            | Op::ShiftB { .. }
            | Op::EndIslands { .. }
            | Op::DistanceToPoint { .. }
            | Op::FindTopSurface { .. }
            | Op::ContextBound { .. } => true,
            _ => {
                let mut inputs = Vec::new();
                self.inputs_of(&op, &mut inputs);
                inputs
                    .iter()
                    .any(|&input| self.positional.get(input as usize).copied().unwrap_or(true))
            }
        };
        self.positional.push(positional);
        let own = u16::from(matches!(
            op,
            Op::Noise { .. }
                | Op::ShiftedNoiseXz { .. }
                | Op::ShiftedNoiseXyz { .. }
                | Op::ShiftB { .. }
                | Op::EndIslands { .. }
        ));
        let mut inputs = Vec::new();
        self.inputs_of(&op, &mut inputs);
        let noise_ops = inputs.iter().fold(own, |sum, &input| {
            sum.saturating_add(
                self.noise_ops
                    .get(input as usize)
                    .copied()
                    .unwrap_or(u16::MAX),
            )
        });
        self.noise_ops.push(noise_ops);
        self.ops.push(op);
        self.maskable.push(maskable);
        self.other_volumes.push(other_volumes);
        self.skippable.push(skippable);
        self.ranges.push(Interval::NAI);
        (self.ops.len() - 1) as OpIndex
    }

    /// Records the static range of the function instruction `op` computes.
    pub(super) fn set_range(&mut self, op: OpIndex, range: Interval) {
        if self.ranges[op as usize].is_nai() {
            self.ranges[op as usize] = range;
        }
    }

    /// [`Self::skippable`] of an instruction whose inputs have theirs.
    fn is_skippable(&self, op: &Op) -> bool {
        if let Op::Cached { cell, .. } = *op
            && !self
                .partial_cells
                .get(cell as usize)
                .copied()
                .unwrap_or(false)
        {
            return false;
        }
        let mut inputs = Vec::new();
        self.inputs_of(op, &mut inputs);
        inputs
            .iter()
            .all(|&input| self.skippable.get(input as usize).copied().unwrap_or(false))
    }

    /// Lets the volume caches of the cells only `root` reads hold partial volumes (sampled
    /// where a branch of `root` needs them). Call it with the function that is only ever
    /// sampled as whole volumes (the final density).
    pub fn set_masked_cache_root(&mut self, root: super::FnId) {
        let count = self.ops.len();
        let mut from_root = vec![false; count];
        let mut stack = vec![root.0];
        let mut inputs = Vec::new();
        while let Some(op) = stack.pop() {
            if std::mem::replace(&mut from_root[op as usize], true) {
                continue;
            }
            self.inputs(op, &mut inputs);
            stack.append(&mut inputs);
        }
        let mut shared = vec![false; count];
        let mut stack: Vec<OpIndex> = (0..count as OpIndex)
            .filter(|&op| !from_root[op as usize])
            .collect();
        while let Some(op) = stack.pop() {
            self.inputs(op, &mut inputs);
            for input in inputs.drain(..) {
                if !std::mem::replace(&mut shared[input as usize], true) {
                    stack.push(input);
                }
            }
        }
        let mut partial = vec![true; self.cache_cells as usize];
        let mut seen = vec![false; self.cache_cells as usize];
        for (op, instruction) in self.ops.iter().enumerate() {
            if let Op::Cached { cell, .. } = *instruction {
                let cell = cell as usize;
                if cell >= partial.len() {
                    partial.resize(cell + 1, true);
                    seen.resize(cell + 1, false);
                }
                seen[cell] = true;
                partial[cell] &= from_root[op] && !shared[op];
            }
        }
        for (partial, seen) in partial.iter_mut().zip(seen) {
            *partial &= seen;
        }
        self.partial_cells = partial;
        // Inputs come before the instructions that read them.
        for op in 0..count {
            self.skippable[op] = self.is_skippable(&self.ops[op]);
        }
        self.root_lattice = self.lattice_of(root).map(|(xz, y)| (root, xz, y));
    }

    /// Whether the cache cells the functions `roots` read are read by nothing else: sampling
    /// those functions, or not, then changes nothing for any other function.
    pub fn cells_private_to(&self, roots: &[super::FnId]) -> bool {
        let count = self.ops.len();
        let mut inside = vec![false; count];
        let mut stack: Vec<OpIndex> = roots.iter().map(|f| f.0).collect();
        let mut inputs = Vec::new();
        while let Some(op) = stack.pop() {
            if std::mem::replace(&mut inside[op as usize], true) {
                continue;
            }
            self.inputs(op, &mut inputs);
            stack.append(&mut inputs);
        }
        // Inputs come before the instructions that read them.
        let mut reads_cell = vec![false; count];
        for op in 0..count {
            self.inputs(op as OpIndex, &mut inputs);
            reads_cell[op] = matches!(self.ops[op], Op::Cached { .. })
                || inputs.iter().any(|&input| reads_cell[input as usize]);
            if !inside[op]
                && inputs
                    .iter()
                    .any(|&input| inside[input as usize] && reads_cell[input as usize])
            {
                return false;
            }
            inputs.clear();
        }
        true
    }

    /// The cell sizes (XZ, Y) of the interpolations `f` reads, if they all have the same.
    pub(super) fn interpolation_lattice(&self, f: super::FnId) -> Option<(i32, i32)> {
        let mut lattice = None;
        let mut seen = vec![false; self.ops.len()];
        let mut stack = vec![f.0];
        let mut inputs = Vec::new();
        while let Some(op) = stack.pop() {
            if std::mem::replace(&mut seen[op as usize], true) {
                continue;
            }
            if let Op::Interpolated {
                cell_size_xz,
                cell_size_y,
                ..
            } = self.ops[op as usize]
                && *lattice.get_or_insert((cell_size_xz, cell_size_y))
                    != (cell_size_xz, cell_size_y)
            {
                return None;
            }
            self.inputs(op, &mut inputs);
            stack.append(&mut inputs);
        }
        lattice.filter(|&(xz, y)| xz > 0 && y > 0)
    }

    /// See [`Self::root_lattice`].
    fn lattice_of(&self, root: super::FnId) -> Option<(i32, i32)> {
        let mut lattice = None;
        let mut seen = vec![false; self.ops.len()];
        let mut stack = vec![root.0];
        let mut inputs = Vec::new();
        while let Some(op) = stack.pop() {
            if std::mem::replace(&mut seen[op as usize], true) {
                continue;
            }
            match self.ops[op as usize] {
                // Sampled over a layer that does not depend on the volume's height.
                Op::SliceY { .. } => continue,
                Op::FindTopSurface { .. } => return None,
                Op::Cached { cell, .. }
                    if !self
                        .partial_cells
                        .get(cell as usize)
                        .copied()
                        .unwrap_or(false) =>
                {
                    return None;
                }
                Op::Interpolated {
                    cell_size_xz,
                    cell_size_y,
                    ..
                } if *lattice.get_or_insert((cell_size_xz, cell_size_y))
                    != (cell_size_xz, cell_size_y) =>
                {
                    return None;
                }
                _ => {}
            }
            self.inputs(op, &mut inputs);
            stack.append(&mut inputs);
        }
        // Cut at a multiple of every interpolation's Y cell size, the volume interpolates
        // as the whole one does.
        lattice.filter(|&(xz, y)| xz > 0 && y > 0 && y == self.interpolation_y_period())
    }

    /// Whether sampling `f` at a point gives the same value whatever the cache cells of the
    /// context hold: nothing it reads is interpolated, the one instruction whose values over a
    /// volume (which a cell may hold) differ from its values at single points. Such a function
    /// may be sampled at any time and in any order.
    pub fn is_point_exact(&self, f: super::FnId) -> bool {
        let mut seen = vec![false; self.ops.len()];
        let mut stack = vec![f.0];
        let mut inputs = Vec::new();
        while let Some(op) = stack.pop() {
            if std::mem::replace(&mut seen[op as usize], true) {
                continue;
            }
            if matches!(self.ops[op as usize], Op::Interpolated { .. }) {
                return false;
            }
            self.inputs(op, &mut inputs);
            stack.append(&mut inputs);
        }
        true
    }

    /// Whether the value of `f` at a block depends on the block alone: it is point exact
    /// ([`Self::is_point_exact`]) and reads nothing a context binds per chunk (the
    /// `beardifier`; the blend fields are never bound). Such values may be kept across chunks.
    pub fn is_position_only(&self, f: super::FnId) -> bool {
        let mut seen = vec![false; self.ops.len()];
        let mut stack = vec![f.0];
        let mut inputs = Vec::new();
        while let Some(op) = stack.pop() {
            if std::mem::replace(&mut seen[op as usize], true) {
                continue;
            }
            if matches!(
                self.ops[op as usize],
                Op::Interpolated { .. }
                    | Op::ContextBound {
                        field: ContextField::Beardifier,
                        ..
                    }
            ) {
                return false;
            }
            self.inputs(op, &mut inputs);
            stack.append(&mut inputs);
        }
        true
    }

    /// The instructions `op` reads.
    fn inputs(&self, op: OpIndex, out: &mut Vec<OpIndex>) {
        self.inputs_of(&self.ops[op as usize], out);
    }

    /// The instructions `op` reads.
    fn inputs_of(&self, op: &Op, out: &mut Vec<OpIndex>) {
        match *op {
            Op::Constant { .. }
            | Op::ContextBound { .. }
            | Op::Noise { .. }
            | Op::ShiftB { .. }
            | Op::EndIslands { .. }
            | Op::DistanceToPoint { .. }
            | Op::Gradient { .. } => {}
            Op::ShiftedNoiseXz {
                shift_x, shift_z, ..
            } => out.extend([shift_x, shift_z]),
            Op::ShiftedNoiseXyz {
                shift_x,
                shift_y,
                shift_z,
                ..
            } => out.extend([shift_x, shift_y, shift_z]),
            Op::Add { left, right }
            | Op::Sub { left, right }
            | Op::Mul { left, right }
            | Op::Div { left, right }
            | Op::Min { left, right, .. }
            | Op::Max { left, right, .. }
            | Op::Round {
                input: left,
                multiple: right,
                ..
            }
            | Op::Pow {
                base: left,
                exponent: right,
            }
            | Op::LerpConstFirst {
                alpha: left,
                second: right,
                ..
            }
            | Op::LerpConstSecond {
                alpha: left,
                first: right,
                ..
            }
            | Op::FindTopSurface {
                density: left,
                upper_bound: right,
                ..
            } => out.extend([left, right]),
            Op::ConstAdd { input, .. }
            | Op::ConstSub { input, .. }
            | Op::ConstMul { input, .. }
            | Op::ConstDiv { input, .. }
            | Op::ConstMin { input, .. }
            | Op::ConstMax { input, .. }
            | Op::Abs { input }
            | Op::Square { input }
            | Op::Cube { input }
            | Op::Sqrt { input }
            | Op::LeakyRelu { input, .. }
            | Op::Reciprocal { input }
            | Op::Negate { input }
            | Op::Squeeze { input }
            | Op::Log { input }
            | Op::Sign { input }
            | Op::RoundToInteger { input, .. }
            | Op::PowConstBase {
                exponent: input, ..
            }
            | Op::PowConstExponent { base: input, .. }
            | Op::Clamp { input, .. }
            | Op::RangeChoiceConst { input, .. }
            | Op::Cached { input, .. }
            | Op::BlendDensity { input }
            | Op::Interpolated { input, .. }
            | Op::SliceX { input, .. }
            | Op::SliceY { input, .. }
            | Op::SliceZ { input, .. }
            | Op::SliceXz { input, .. } => out.push(input),
            Op::Lerp {
                alpha,
                first,
                second,
            } => out.extend([alpha, first, second]),
            Op::RangeChoice {
                input,
                when_in_range,
                when_out_of_range,
                ..
            } => out.extend([input, when_in_range, when_out_of_range]),
            Op::IntervalSelectSingle {
                input,
                if_below,
                if_above,
                ..
            } => out.extend([input, if_below, if_above]),
            Op::IntervalSelect { input, table } => {
                out.push(input);
                out.extend_from_slice(&self.interval_selects[table as usize].functions);
            }
            Op::Spline { spline } => {
                out.extend_from_slice(&self.splines[spline as usize].coordinates);
            }
        }
    }

    /// [`Self::maskable`] of a new instruction (unknown inputs count as maskable).
    fn is_maskable(&self, op: &Op) -> bool {
        let m = |input: OpIndex| self.maskable.get(input as usize).copied().unwrap_or(true);
        match *op {
            // An interpolation fills only the cells with needed points and asks its input
            // for their corners only.
            Op::Noise { .. }
            | Op::ShiftedNoiseXz { .. }
            | Op::ShiftedNoiseXyz { .. }
            | Op::Spline { .. }
            | Op::Interpolated { .. } => true,
            Op::Constant { .. }
            | Op::ContextBound { .. }
            | Op::ShiftB { .. }
            | Op::EndIslands { .. }
            | Op::DistanceToPoint { .. }
            | Op::Gradient { .. }
            | Op::SliceX { .. }
            | Op::SliceY { .. }
            | Op::SliceZ { .. }
            | Op::SliceXz { .. }
            | Op::FindTopSurface { .. } => false,
            Op::Add { left, right }
            | Op::Sub { left, right }
            | Op::Mul { left, right }
            | Op::Div { left, right }
            | Op::Min { left, right, .. }
            | Op::Max { left, right, .. }
            | Op::Round {
                input: left,
                multiple: right,
                ..
            }
            | Op::Pow {
                base: left,
                exponent: right,
            }
            | Op::LerpConstFirst {
                alpha: left,
                second: right,
                ..
            }
            | Op::LerpConstSecond {
                alpha: left,
                first: right,
                ..
            } => m(left) || m(right),
            Op::ConstAdd { input, .. }
            | Op::ConstSub { input, .. }
            | Op::ConstMul { input, .. }
            | Op::ConstDiv { input, .. }
            | Op::ConstMin { input, .. }
            | Op::ConstMax { input, .. }
            | Op::Abs { input }
            | Op::Square { input }
            | Op::Cube { input }
            | Op::Sqrt { input }
            | Op::LeakyRelu { input, .. }
            | Op::Reciprocal { input }
            | Op::Negate { input }
            | Op::Squeeze { input }
            | Op::Log { input }
            | Op::Sign { input }
            | Op::RoundToInteger { input, .. }
            | Op::PowConstBase {
                exponent: input, ..
            }
            | Op::PowConstExponent { base: input, .. }
            | Op::Clamp { input, .. }
            | Op::RangeChoiceConst { input, .. }
            | Op::Cached { input, .. }
            | Op::BlendDensity { input } => m(input),
            Op::Lerp {
                alpha,
                first,
                second,
            } => m(alpha) || m(first) || m(second),
            Op::RangeChoice {
                input,
                when_in_range,
                when_out_of_range,
                ..
            } => m(input) || m(when_in_range) || m(when_out_of_range),
            Op::IntervalSelectSingle {
                input,
                if_below,
                if_above,
                ..
            } => m(input) || m(if_below) || m(if_above),
            Op::IntervalSelect { input, table } => {
                m(input)
                    || self
                        .interval_selects
                        .get(table as usize)
                        .is_none_or(|t| t.functions.iter().any(|&f| m(f)))
            }
        }
    }
}
