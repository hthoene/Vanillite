//! The CPU interpreter of a [`Program`] (the reference backend) and its [`SamplerContext`].
//!
//! [`Program::sample_volume`] runs one instruction over a whole [`DensityVolume`] per call
//! (Vanilla 26.3's `sampleVolume` of each sampler), [`Program::sample_value`] runs it for one
//! block (`sampleValue`). Both follow the Vanilla samplers line by line, including the places
//! where the two paths compute differently (interpolation, short-circuits, cache cells).

use std::sync::Arc;

use vanillite_base::math::{floor_div, floor_mod};
use vanillite_base::pos::BlockPos;

use super::function::{Axis, DistanceMetric, leaky_relu, squeeze};
use super::program::{ContextField, GradientTiling, Op, OpIndex, Program};
use super::spline::SplineProgram;
use crate::mth::{
    clamp_f, cube_f, floor_f, length3_f, lerp_f, lerp3_f, max_f, min_f, signum_f, sqrt_f, square_f,
};
use crate::noise::Noise;
use crate::volume::DensityVolume;

/// One cache cell (`SamplerContext.CacheCell`).
#[derive(Debug)]
struct CacheCell {
    /// The volume `buffer` holds, if any.
    volume: Option<DensityVolume>,
    buffer: Vec<f32>,
    /// With `partial`: the points of `volume` that `buffer` holds (only for
    /// `Program::partial_cells`); else it holds all of them.
    computed: Vec<bool>,
    partial: bool,
    value_key: i64,
    value: f32,
}

impl Default for CacheCell {
    fn default() -> Self {
        Self {
            volume: None,
            buffer: Vec::new(),
            computed: Vec::new(),
            partial: false,
            value_key: 0,
            value: f32::NAN,
        }
    }
}

impl CacheCell {
    fn reset(&mut self) {
        self.volume = None;
        self.partial = false;
        self.value_key = 0;
        self.value = f32::NAN;
    }
}

/// A pooled scratch buffer: the first `len` values of `buf`. Pooled buffers keep their full
/// length, so handing one out again never clears it.
#[derive(Debug, Default)]
struct Scratch {
    buf: Vec<f32>,
    len: usize,
}

impl std::ops::Deref for Scratch {
    type Target = [f32];

    #[inline]
    fn deref(&self) -> &[f32] {
        &self.buf[..self.len]
    }
}

impl std::ops::DerefMut for Scratch {
    #[inline]
    fn deref_mut(&mut self) -> &mut [f32] {
        &mut self.buf[..self.len]
    }
}

/// `SamplerContext`: per-task cache cells and scratch buffers.
///
/// A context with caches enabled is created per chunk (Vanilla's `NoiseChunk`); cached values
/// depend on what was sampled before, exactly as in Vanilla, so the same calls must happen in
/// the same order. Buffers are pooled, so sampling does not allocate once the pool is warm.
#[derive(Debug, Default)]
pub struct SamplerContext {
    caching: bool,
    cells: Vec<CacheCell>,
    /// Free scratch buffers (`DensityBufferPool`).
    pool: Vec<Vec<f32>>,
    /// Buffers in use by spline and interval-select instructions, as a stack.
    slots: Vec<Scratch>,
    /// Per-point spline coordinate values, as a stack.
    scratch: Vec<f32>,
    /// The bound `beardifier` field, if any.
    beardifier: Option<Arc<dyn ContextSampler>>,
}

impl SamplerContext {
    /// A context with caches (`enableCaches()`) or without (`EMPTY_UNCACHED`).
    pub fn new(caching: bool) -> Self {
        Self {
            caching,
            ..Self::default()
        }
    }

    /// Empties every cache cell, as if the context were new, but keeps the allocations.
    pub fn reset(&mut self) {
        for cell in &mut self.cells {
            cell.reset();
        }
    }

    /// `acquireBuffer(volume)`: a scratch buffer of `len` values (contents unspecified).
    fn acquire(&mut self, len: usize) -> Scratch {
        let best = self
            .pool
            .iter()
            .enumerate()
            .filter(|(_, b)| b.len() >= len)
            .min_by_key(|(_, b)| b.len())
            .map(|(i, _)| i);
        let mut buf = match best {
            Some(i) => self.pool.swap_remove(i),
            None => self.pool.pop().unwrap_or_default(),
        };
        if buf.len() < len {
            buf.resize(len, 0.0);
        }
        Scratch { buf, len }
    }

    /// Returns a buffer to the pool (`ScopedDensityBuffer.close()`).
    fn release(&mut self, buffer: Scratch) {
        if !buffer.buf.is_empty() {
            self.pool.push(buffer.buf);
        }
    }

    fn cell(&mut self, id: u32) -> &mut CacheCell {
        let id = id as usize;
        if id >= self.cells.len() {
            self.cells.resize_with(id + 1, CacheCell::default);
        }
        &mut self.cells[id]
    }

    /// Binds the `beardifier` field (`Beardifier.CONTEXT_KEY`) for the chunk being generated;
    /// `None` leaves the fallback (Vanilla's empty context, or `Beardifier.EMPTY`, which is
    /// zero everywhere as well).
    pub fn set_beardifier(&mut self, beardifier: Option<Arc<dyn ContextSampler>>) {
        self.beardifier = beardifier;
    }

    /// `sampleValue` of a context-bound field (`ContextBoundSampler`).
    ///
    /// Only `beardifier` can be bound; `blend_alpha` and `blend_offset` (old-chunk blending)
    /// yield their fallbacks and `blend_density` is the identity, as in Vanilla with an empty
    /// `Blender`.
    #[inline]
    fn context_value(&self, field: ContextField, fallback: f32, x: i32, y: i32, z: i32) -> f32 {
        match (field, &self.beardifier) {
            (ContextField::Beardifier, Some(beardifier)) => beardifier.sample_value(x, y, z),
            _ => fallback,
        }
    }

    /// `sampleVolume` of a context-bound field.
    fn context_volume(
        &self,
        field: ContextField,
        fallback: f32,
        out: &mut [f32],
        v: &DensityVolume,
    ) {
        match (field, &self.beardifier) {
            (ContextField::Beardifier, Some(beardifier)) => beardifier.sample_volume(out, v),
            _ => out.fill(fallback),
        }
    }
}

/// A sampler a context binds for a context-bound field (Vanilla's `DensitySampler` stored in
/// the `SamplerContext` user fields), such as the structures' `Beardifier`.
pub trait ContextSampler: Send + Sync + std::fmt::Debug {
    /// `sampleValue(context, blockX, blockY, blockZ)`.
    fn sample_value(&self, x: i32, y: i32, z: i32) -> f32;
    /// `sampleVolume(context, outputBuffer, volume)`: fills `out` in the volume's index order.
    fn sample_volume(&self, out: &mut [f32], volume: &DensityVolume);
}

/// Java `Math.pow`: unlike C's `pow`, a NaN exponent and `(±1) ^ ±∞` give NaN.
#[inline]
fn java_pow(base: f64, exponent: f64) -> f64 {
    if exponent.is_nan() || (exponent.is_infinite() && base.abs() == 1.0) {
        f64::NAN
    } else {
        base.powf(exponent)
    }
}

/// `DistanceMetric.compute(dx, dy, dz)`.
#[inline]
fn distance(metric: DistanceMetric, dx: f32, dy: f32, dz: f32) -> f32 {
    match metric {
        DistanceMetric::Euclidean => length3_f(dx, dy, dz),
        DistanceMetric::EuclideanSquared => dx * dx + dy * dy + dz * dz,
        DistanceMetric::Manhattan => dx.abs() + dy.abs() + dz.abs(),
        DistanceMetric::Chebyshev => max_f(max_f(dx.abs(), dy.abs()), dz.abs()),
    }
}

/// `GradientSampler.compute(coordinate)`.
#[inline]
fn gradient(
    tiling: GradientTiling,
    from: i32,
    from_value: f32,
    factor: f32,
    coordinate: i32,
) -> f32 {
    match tiling {
        GradientTiling::Clamped { min, max } => {
            let clamped = if coordinate < min {
                min
            } else {
                coordinate.min(max)
            };
            from_value + clamped.wrapping_sub(from) as f32 * factor
        }
        GradientTiling::Repeat { range } => {
            from_value + floor_mod(coordinate.wrapping_sub(from), range) as f32 * factor
        }
        GradientTiling::MirroredRepeat { range } => {
            let relative = coordinate.wrapping_sub(from);
            let tile = floor_div(relative, range);
            let local = relative.wrapping_sub(tile.wrapping_mul(range));
            if tile & 1 == 0 {
                from_value + local as f32 * factor
            } else {
                from_value + range.wrapping_sub(local) as f32 * factor
            }
        }
    }
}

/// `EndIslandFunction.getHeightValue(islandNoise, sectionX, sectionZ)`.
fn end_island_height(noise: &Noise, section_x: i32, section_z: i32) -> f32 {
    let chunk_x = section_x / 2;
    let chunk_z = section_z / 2;
    let sub_x = section_x % 2;
    let sub_z = section_z % 2;
    let mut doffs = -100.0f32;
    for xo in -12..=12i32 {
        for zo in -12..=12i32 {
            let total_x = i64::from(chunk_x.wrapping_add(xo));
            let total_z = i64::from(chunk_z.wrapping_add(zo));
            if total_x * total_x + total_z * total_z > 4096
                && noise.get2(total_x as f64, total_z as f64) < -0.9
            {
                let island_size =
                    ((total_x as f32).abs() * 3439.0 + (total_z as f32).abs() * 147.0) % 13.0 + 9.0;
                let xd = sub_x.wrapping_sub(xo * 2) as f32;
                let zd = sub_z.wrapping_sub(zo * 2) as f32;
                let new_doffs = 100.0 - sqrt_f(xd * xd + zd * zd) * island_size;
                doffs = max_f(doffs, clamp_f(new_doffs, -100.0, 80.0));
            }
        }
    }
    doffs
}

/// `EndIslandFunction.Sampler.sampleValue`.
fn end_islands(noise: &Noise, x: i32, z: i32) -> f32 {
    (end_island_height(noise, x / 8, z / 8) - 8.0) / 128.0
}

/// `RangeChoiceFunction`'s range test.
#[inline]
fn in_range(input: f32, min_inclusive: f32, max_exclusive: f32) -> bool {
    input >= min_inclusive && input < max_exclusive
}

/// `IntervalSelectFunction.Sampler.selectSamplerIndex(input)`.
#[inline]
fn select_index(thresholds: &[f32], input: f32) -> usize {
    thresholds
        .iter()
        .position(|&t| input < t)
        .unwrap_or(thresholds.len())
}

/// The points of a volume whose values are needed (`None`: all).
type Mask<'a> = Option<&'a [bool]>;

/// Whether point `index` is needed.
#[inline(always)]
fn needed(mask: Mask<'_>, index: usize) -> bool {
    mask.is_none_or(|mask| mask[index])
}

/// The needed points of `mask` where `values` satisfies `test`.
fn refine(mask: Mask<'_>, values: &[f32], test: impl Fn(f32) -> bool) -> Vec<bool> {
    match mask {
        Some(mask) => mask
            .iter()
            .zip(values)
            .map(|(&m, &x)| m && test(x))
            .collect(),
        None => values.iter().map(|&x| test(x)).collect(),
    }
}

/// The lerp of `LerpFunction`'s samplers with their `alpha == 0` / `alpha == 1` shortcuts.
#[inline]
fn lerp_select(alpha: f32, first: f32, second: f32) -> f32 {
    if alpha == 0.0 {
        first
    } else if alpha == 1.0 {
        second
    } else {
        lerp_f(alpha, first, second)
    }
}

/// Applies `f` to every value of `out` in place.
#[inline]
fn map_in_place(out: &mut [f32], f: impl Fn(f32) -> f32) {
    for v in out {
        *v = f(*v);
    }
}

impl Program {
    /// If `f` is a range choice on a Y gradient with a constant outside the range (as ore vein
    /// densities are), the contiguous block Y range `[min, max)` it chooses `when_in_range`
    /// for and the constant it has everywhere else.
    pub fn y_range_choice(&self, f: super::FnId) -> Option<(i32, i32, f32)> {
        let (input, min_inclusive, max_exclusive, outside) = match self.ops[f.0 as usize] {
            Op::RangeChoice {
                input,
                min_inclusive,
                max_exclusive,
                when_out_of_range,
                ..
            } => match self.ops[when_out_of_range as usize] {
                Op::Constant { value } => (input, min_inclusive, max_exclusive, value),
                _ => return None,
            },
            Op::RangeChoiceConst {
                input,
                min_inclusive,
                max_exclusive,
                when_out_of_range,
                ..
            } => (input, min_inclusive, max_exclusive, when_out_of_range),
            _ => return None,
        };
        let Op::Gradient {
            axis: Axis::Y,
            tiling,
            from_coordinate,
            from_value,
            factor,
        } = self.ops[input as usize]
        else {
            return None;
        };
        let in_range = |y| {
            let v = gradient(tiling, from_coordinate, from_value, factor, y);
            v >= min_inclusive && v < max_exclusive
        };
        // Every Y a dimension can have (`DimensionType.MIN_Y..=MAX_Y`).
        let ys = -2032..=2031;
        let min = ys.clone().find(|&y| in_range(y))?;
        let max = ys.clone().rev().find(|&y| in_range(y))? + 1;
        (min..max).all(in_range).then_some((min, max, outside))
    }

    /// The least common multiple of the Y cell sizes of all interpolated functions (1 without
    /// any): a volume starting at a multiple of it interpolates exactly like any larger volume
    /// containing it.
    pub fn interpolation_y_period(&self) -> i32 {
        fn gcd(a: i32, b: i32) -> i32 {
            if b == 0 { a } else { gcd(b, a % b) }
        }
        self.ops.iter().fold(1, |period, op| match *op {
            Op::Interpolated { cell_size_y, .. } if cell_size_y > 0 => {
                period / gcd(period, cell_size_y) * cell_size_y
            }
            _ => period,
        })
    }

    /// `DensitySampler.sampleValue(context, x, y, z)` of function `f`.
    pub fn sample_value(
        &self,
        ctx: &mut SamplerContext,
        f: super::FnId,
        x: i32,
        y: i32,
        z: i32,
    ) -> f32 {
        self.value(ctx, f.0, x, y, z)
    }

    /// `DensitySampler.sampleVolume(context, buffer, volume)` of function `f`;
    /// `out.len()` must be `volume.len()`.
    pub fn sample_volume(
        &self,
        ctx: &mut SamplerContext,
        f: super::FnId,
        out: &mut [f32],
        volume: &DensityVolume,
    ) {
        assert_eq!(out.len(), volume.len(), "buffer size must match the volume");
        self.volume(ctx, f.0, out, volume);
    }

    fn noise(&self, index: u32) -> &Noise {
        &self.noises[index as usize]
    }

    /// `sampleValue` of instruction `op`.
    fn value(&self, ctx: &mut SamplerContext, op: OpIndex, x: i32, y: i32, z: i32) -> f32 {
        match self.ops[op as usize] {
            Op::Constant { value } => value,
            Op::ContextBound { field, fallback } => ctx.context_value(field, fallback, x, y, z),
            Op::Noise {
                noise,
                xz_scale,
                y_scale,
            } => self.noise(noise).get(
                f64::from(x) * xz_scale,
                f64::from(y) * y_scale,
                f64::from(z) * xz_scale,
            ),
            Op::ShiftedNoiseXz {
                shift_x,
                shift_z,
                noise,
                xz_scale,
                y_scale,
            } => {
                let nx = f64::from(x) * xz_scale + f64::from(self.value(ctx, shift_x, x, y, z));
                let ny = f64::from(y) * y_scale;
                let nz = f64::from(z) * xz_scale + f64::from(self.value(ctx, shift_z, x, y, z));
                self.noise(noise).get(nx, ny, nz)
            }
            Op::ShiftedNoiseXyz {
                shift_x,
                shift_y,
                shift_z,
                noise,
                xz_scale,
                y_scale,
            } => {
                let nx = f64::from(x) * xz_scale + f64::from(self.value(ctx, shift_x, x, y, z));
                let ny = f64::from(y) * y_scale + f64::from(self.value(ctx, shift_y, x, y, z));
                let nz = f64::from(z) * xz_scale + f64::from(self.value(ctx, shift_z, x, y, z));
                self.noise(noise).get(nx, ny, nz)
            }
            Op::ShiftB { noise } => {
                self.noise(noise)
                    .get(f64::from(z) * 0.25, f64::from(x) * 0.25, 0.0)
                    * 4.0
            }
            Op::EndIslands { noise } => end_islands(self.noise(noise), x, z),
            Op::DistanceToPoint {
                x: px,
                y: py,
                z: pz,
                metric,
            } => distance(
                metric,
                px.wrapping_sub(x) as f32,
                py.wrapping_sub(y) as f32,
                pz.wrapping_sub(z) as f32,
            ),
            Op::Gradient {
                axis,
                tiling,
                from_coordinate,
                from_value,
                factor,
            } => gradient(
                tiling,
                from_coordinate,
                from_value,
                factor,
                axis.choose(x, y, z),
            ),
            Op::Add { left, right } => {
                self.value(ctx, left, x, y, z) + self.value(ctx, right, x, y, z)
            }
            Op::Sub { left, right } => {
                self.value(ctx, left, x, y, z) - self.value(ctx, right, x, y, z)
            }
            Op::Mul { left, right } => {
                let left = self.value(ctx, left, x, y, z);
                if left == 0.0 {
                    0.0
                } else {
                    left * self.value(ctx, right, x, y, z)
                }
            }
            Op::Div { left, right } => {
                let left = self.value(ctx, left, x, y, z);
                if left == 0.0 {
                    0.0
                } else {
                    left / self.value(ctx, right, x, y, z)
                }
            }
            Op::Min {
                left,
                right,
                right_min,
            } => {
                let left = self.value(ctx, left, x, y, z);
                if left <= right_min {
                    left
                } else {
                    min_f(left, self.value(ctx, right, x, y, z))
                }
            }
            Op::Max {
                left,
                right,
                right_max,
            } => {
                let left = self.value(ctx, left, x, y, z);
                if left >= right_max {
                    left
                } else {
                    max_f(left, self.value(ctx, right, x, y, z))
                }
            }
            Op::ConstAdd { input, value } => self.value(ctx, input, x, y, z) + value,
            Op::ConstSub { value, input } => value - self.value(ctx, input, x, y, z),
            Op::ConstMul { input, value } => self.value(ctx, input, x, y, z) * value,
            Op::ConstDiv { value, input } => value / self.value(ctx, input, x, y, z),
            Op::ConstMin { input, value } => min_f(self.value(ctx, input, x, y, z), value),
            Op::ConstMax { input, value } => max_f(self.value(ctx, input, x, y, z), value),
            Op::Abs { input } => self.value(ctx, input, x, y, z).abs(),
            Op::Square { input } => square_f(self.value(ctx, input, x, y, z)),
            Op::Cube { input } => cube_f(self.value(ctx, input, x, y, z)),
            Op::Sqrt { input } => sqrt_f(self.value(ctx, input, x, y, z)),
            Op::LeakyRelu {
                input,
                negative_factor,
            } => leaky_relu(negative_factor, self.value(ctx, input, x, y, z)),
            Op::Reciprocal { input } => 1.0 / self.value(ctx, input, x, y, z),
            Op::Negate { input } => -self.value(ctx, input, x, y, z),
            Op::Squeeze { input } => squeeze(self.value(ctx, input, x, y, z)),
            Op::Log { input } => f64::from(self.value(ctx, input, x, y, z)).ln() as f32,
            Op::Sign { input } => signum_f(self.value(ctx, input, x, y, z)),
            Op::RoundToInteger { mode, input } => {
                mode.round_to_integer(self.value(ctx, input, x, y, z))
            }
            Op::Round {
                mode,
                input,
                multiple,
            } => {
                let input = self.value(ctx, input, x, y, z);
                let multiple = self.value(ctx, multiple, x, y, z);
                round_multiple(mode, input, multiple)
            }
            Op::Pow { base, exponent } => {
                let base = self.value(ctx, base, x, y, z);
                let exponent = self.value(ctx, exponent, x, y, z);
                java_pow(f64::from(base), f64::from(exponent)) as f32
            }
            Op::PowConstBase { base, exponent } => {
                java_pow(base, f64::from(self.value(ctx, exponent, x, y, z))) as f32
            }
            Op::PowConstExponent { base, exponent } => {
                java_pow(f64::from(self.value(ctx, base, x, y, z)), exponent) as f32
            }
            Op::Lerp {
                alpha,
                first,
                second,
            } => {
                let alpha = self.value(ctx, alpha, x, y, z);
                if alpha == 0.0 {
                    self.value(ctx, first, x, y, z)
                } else if alpha == 1.0 {
                    self.value(ctx, second, x, y, z)
                } else {
                    let first = self.value(ctx, first, x, y, z);
                    let second = self.value(ctx, second, x, y, z);
                    lerp_f(alpha, first, second)
                }
            }
            Op::LerpConstFirst {
                alpha,
                first,
                second,
            } => {
                let alpha = self.value(ctx, alpha, x, y, z);
                if alpha == 0.0 {
                    first
                } else if alpha == 1.0 {
                    self.value(ctx, second, x, y, z)
                } else {
                    lerp_f(alpha, first, self.value(ctx, second, x, y, z))
                }
            }
            Op::LerpConstSecond {
                alpha,
                first,
                second,
            } => {
                let alpha = self.value(ctx, alpha, x, y, z);
                if alpha == 0.0 {
                    self.value(ctx, first, x, y, z)
                } else if alpha == 1.0 {
                    second
                } else {
                    lerp_f(alpha, self.value(ctx, first, x, y, z), second)
                }
            }
            Op::Clamp { input, min, max } => clamp_f(self.value(ctx, input, x, y, z), min, max),
            Op::RangeChoice {
                input,
                min_inclusive,
                max_exclusive,
                when_in_range,
                when_out_of_range,
            } => {
                let input = self.value(ctx, input, x, y, z);
                if in_range(input, min_inclusive, max_exclusive) {
                    self.value(ctx, when_in_range, x, y, z)
                } else {
                    self.value(ctx, when_out_of_range, x, y, z)
                }
            }
            Op::RangeChoiceConst {
                input,
                min_inclusive,
                max_exclusive,
                when_in_range,
                when_out_of_range,
            } => {
                if in_range(
                    self.value(ctx, input, x, y, z),
                    min_inclusive,
                    max_exclusive,
                ) {
                    when_in_range
                } else {
                    when_out_of_range
                }
            }
            Op::IntervalSelectSingle {
                input,
                threshold,
                if_below,
                if_above,
            } => {
                if self.value(ctx, input, x, y, z) < threshold {
                    self.value(ctx, if_below, x, y, z)
                } else {
                    self.value(ctx, if_above, x, y, z)
                }
            }
            Op::IntervalSelect { input, table } => {
                let input = self.value(ctx, input, x, y, z);
                let table = &self.interval_selects[table as usize];
                let selected = table.functions[select_index(&table.thresholds, input)];
                self.value(ctx, selected, x, y, z)
            }
            Op::Cached { cell, input } => self.value_cached(ctx, cell, input, x, y, z),
            // No blender is bound (see `SamplerContext::context_field`): the identity.
            Op::BlendDensity { input } => self.value(ctx, input, x, y, z),
            Op::Interpolated {
                input,
                cell_size_xz,
                cell_size_y,
                ..
            } => self.interpolated_value(ctx, input, cell_size_xz, cell_size_y, x, y, z),
            Op::SliceX { input, x: sx } => self.value(ctx, input, sx, y, z),
            Op::SliceY { input, y: sy } => self.value(ctx, input, x, sy, z),
            Op::SliceZ { input, z: sz } => self.value(ctx, input, x, y, sz),
            Op::SliceXz {
                input,
                x: sx,
                z: sz,
            } => self.value(ctx, input, sx, y, sz),
            Op::FindTopSurface {
                density,
                upper_bound,
                lower_bound,
                cell_height,
            } => {
                let upper = self.value(ctx, upper_bound, x, y, z);
                self.find_surface(ctx, density, lower_bound, cell_height, x, z, upper)
            }
            Op::Spline { spline } => {
                self.spline_value(ctx, &self.splines[spline as usize], x, y, z)
            }
        }
    }

    /// `SamplerContext.sampleValueCached`.
    fn value_cached(
        &self,
        ctx: &mut SamplerContext,
        cell_id: u32,
        input: OpIndex,
        x: i32,
        y: i32,
        z: i32,
    ) -> f32 {
        if !ctx.caching {
            return self.value(ctx, input, x, y, z);
        }
        let key = BlockPos::new(x, y, z).as_long();
        let cell = ctx.cell(cell_id);
        if cell.value_key == key && !cell.value.is_nan() {
            return cell.value;
        }
        if !cell.partial
            && let Some(index) = cell.volume.and_then(|v| v.index_of_block(x, y, z))
        {
            return cell.buffer[index];
        }
        let value = self.value(ctx, input, x, y, z);
        let cell = ctx.cell(cell_id);
        cell.value_key = key;
        cell.value = value;
        value
    }

    /// `InterpolatedFunction.Sampler.sampleValue`: trilinear interpolation of the cell corners,
    /// sampled as a 2×2×2 volume.
    #[allow(clippy::too_many_arguments)]
    fn interpolated_value(
        &self,
        ctx: &mut SamplerContext,
        input: OpIndex,
        cell_xz: i32,
        cell_y: i32,
        x: i32,
        y: i32,
        z: i32,
    ) -> f32 {
        let x_in_cell = floor_mod(x, cell_xz);
        let y_in_cell = floor_mod(y, cell_y);
        let z_in_cell = floor_mod(z, cell_xz);
        if x_in_cell == 0 && y_in_cell == 0 && z_in_cell == 0 {
            return self.value(ctx, input, x, y, z);
        }
        let volume = DensityVolume::with_step(
            2,
            2,
            2,
            x - x_in_cell,
            y - y_in_cell,
            z - z_in_cell,
            cell_xz,
            cell_y,
            cell_xz,
        );
        let mut buffer = ctx.acquire(volume.len());
        self.volume(ctx, input, &mut buffer, &volume);
        let at = |x, y, z| buffer[volume.index(x, y, z)];
        let value = lerp3_f(
            x_in_cell as f32 / cell_xz as f32,
            y_in_cell as f32 / cell_y as f32,
            z_in_cell as f32 / cell_xz as f32,
            at(0, 0, 0),
            at(1, 0, 0),
            at(0, 1, 0),
            at(1, 1, 0),
            at(0, 0, 1),
            at(1, 0, 1),
            at(0, 1, 1),
            at(1, 1, 1),
        );
        ctx.release(buffer);
        value
    }

    /// `FindTopSurfaceFunction.Sampler.findSurfaceFrom`.
    #[allow(clippy::too_many_arguments)]
    fn find_surface(
        &self,
        ctx: &mut SamplerContext,
        density: OpIndex,
        lower_bound: i32,
        cell_height: i32,
        x: i32,
        z: i32,
        upper_bound: f32,
    ) -> f32 {
        let top_y = floor_f(upper_bound / cell_height as f32).wrapping_mul(cell_height);
        if top_y <= lower_bound {
            return lower_bound as f32;
        }
        let mut probe_y = top_y;
        while probe_y >= lower_bound {
            if self.value(ctx, density, x, probe_y, z) > 0.0 {
                return probe_y as f32;
            }
            probe_y = probe_y.wrapping_sub(cell_height);
        }
        lower_bound as f32
    }

    /// `SplineFunction.Sampler.sampleValue` with a `PointSplineInput`: each coordinate is
    /// sampled at most once (NaN marks "not sampled yet", as in Vanilla).
    fn spline_value(
        &self,
        ctx: &mut SamplerContext,
        spline: &SplineProgram,
        x: i32,
        y: i32,
        z: i32,
    ) -> f32 {
        let base = ctx.scratch.len();
        ctx.scratch
            .resize(base + spline.coordinates.len(), f32::NAN);
        let value = spline.sample(spline.root, &mut |slot| {
            let cached = ctx.scratch[base + slot as usize];
            if !cached.is_nan() {
                return cached;
            }
            let value = self.value(ctx, spline.coordinates[slot as usize], x, y, z);
            ctx.scratch[base + slot as usize] = value;
            value
        });
        ctx.scratch.truncate(base);
        value
    }

    /// `sampleVolume` of instruction `op`.
    fn volume(&self, ctx: &mut SamplerContext, op: OpIndex, out: &mut [f32], v: &DensityVolume) {
        self.volume_masked(ctx, op, out, v, None);
    }

    /// [`Self::volume`] where only the points of `mask` (if any) must be right: the others may
    /// hold anything. Every instruction is pointwise except those that read other points
    /// (interpolation, slices, surface search), which sample their inputs fully; so the masked
    /// points get exactly the values of the full sampling, and branches (range choices, interval
    /// selects, lerps) only sample their inputs where they are chosen.
    fn volume_masked(
        &self,
        ctx: &mut SamplerContext,
        op: OpIndex,
        out: &mut [f32],
        v: &DensityVolume,
        mask: Mask<'_>,
    ) {
        match self.ops[op as usize] {
            Op::Constant { value } => out.fill(value),
            Op::ContextBound { field, fallback } => ctx.context_volume(field, fallback, out, v),
            Op::Noise {
                noise,
                xz_scale,
                y_scale,
            } => {
                out.fill(0.0);
                let noise = self.noise(noise);
                match mask {
                    Some(mask) => noise.add_to_volume_masked(out, v, xz_scale, y_scale, 1.0, mask),
                    None => noise.add_to_volume(out, v, xz_scale, y_scale, 1.0),
                }
            }
            Op::ShiftedNoiseXz {
                shift_x,
                shift_z,
                noise,
                xz_scale,
                y_scale,
            } => {
                self.volume_masked(ctx, shift_x, out, v, mask);
                let mut shift_z_buffer = ctx.acquire(out.len());
                self.volume_masked(ctx, shift_z, &mut shift_z_buffer, v, mask);
                let noise = self.noise(noise);
                let mut index = 0;
                for z in 0..v.size_z {
                    let base_z = f64::from(v.block_z(z)) * xz_scale;
                    for x in 0..v.size_x {
                        let base_x = f64::from(v.block_x(x)) * xz_scale;
                        for y in 0..v.size_y {
                            if !needed(mask, index) {
                                index += 1;
                                continue;
                            }
                            let nx = base_x + f64::from(out[index]);
                            let ny = f64::from(v.block_y(y)) * y_scale;
                            let nz = base_z + f64::from(shift_z_buffer[index]);
                            out[index] = noise.get(nx, ny, nz);
                            index += 1;
                        }
                    }
                }
                ctx.release(shift_z_buffer);
            }
            Op::ShiftedNoiseXyz {
                shift_x,
                shift_y,
                shift_z,
                noise,
                xz_scale,
                y_scale,
            } => {
                self.volume_masked(ctx, shift_x, out, v, mask);
                let mut shift_y_buffer = ctx.acquire(out.len());
                self.volume_masked(ctx, shift_y, &mut shift_y_buffer, v, mask);
                let mut shift_z_buffer = ctx.acquire(out.len());
                self.volume_masked(ctx, shift_z, &mut shift_z_buffer, v, mask);
                let noise = self.noise(noise);
                let mut index = 0;
                for z in 0..v.size_z {
                    let base_z = f64::from(v.block_z(z)) * xz_scale;
                    for x in 0..v.size_x {
                        let base_x = f64::from(v.block_x(x)) * xz_scale;
                        for y in 0..v.size_y {
                            if !needed(mask, index) {
                                index += 1;
                                continue;
                            }
                            let nx = base_x + f64::from(out[index]);
                            let ny = f64::from(v.block_y(y)) * y_scale
                                + f64::from(shift_y_buffer[index]);
                            let nz = base_z + f64::from(shift_z_buffer[index]);
                            out[index] = noise.get(nx, ny, nz);
                            index += 1;
                        }
                    }
                }
                ctx.release(shift_z_buffer);
                ctx.release(shift_y_buffer);
            }
            Op::ShiftB { noise } => {
                // Sampled as a (z, x) plane so the noise's Y axis runs along block X.
                let transposed = DensityVolume::with_step(
                    v.size_z, v.size_x, 1, v.min_z, v.min_x, 0, v.step_z, v.step_x, 1,
                );
                let mut buffer = ctx.acquire(transposed.len());
                buffer.fill(0.0);
                self.noise(noise)
                    .add_to_volume(&mut buffer, &transposed, 0.25, 0.25, 4.0);
                let size_y = v.size_y as usize;
                for z in 0..v.size_z {
                    for x in 0..v.size_x {
                        let value = buffer[transposed.index(z, x, 0)];
                        let start = v.index(x, 0, z);
                        out[start..start + size_y].fill(value);
                    }
                }
                ctx.release(buffer);
            }
            Op::EndIslands { noise } => {
                let noise = self.noise(noise);
                let size_y = v.size_y as usize;
                for z in 0..v.size_z {
                    let block_z = v.block_z(z);
                    for x in 0..v.size_x {
                        let value = end_islands(noise, v.block_x(x), block_z);
                        let start = v.index(x, 0, z);
                        out[start..start + size_y].fill(value);
                    }
                }
            }
            Op::DistanceToPoint {
                x: px,
                y: py,
                z: pz,
                metric,
            } => {
                let mut index = 0;
                for z in 0..v.size_z {
                    let dz = pz.wrapping_sub(v.block_z(z)) as f32;
                    for x in 0..v.size_x {
                        let dx = px.wrapping_sub(v.block_x(x)) as f32;
                        for y in 0..v.size_y {
                            let dy = py.wrapping_sub(v.block_y(y)) as f32;
                            out[index] = distance(metric, dx, dy, dz);
                            index += 1;
                        }
                    }
                }
            }
            Op::Gradient {
                axis,
                tiling,
                from_coordinate,
                from_value,
                factor,
            } => {
                let compute = |c| gradient(tiling, from_coordinate, from_value, factor, c);
                let size_y = v.size_y as usize;
                match axis {
                    Axis::X => {
                        for x in 0..v.size_x {
                            let value = compute(v.block_x(x));
                            for z in 0..v.size_z {
                                let start = v.index(x, 0, z);
                                out[start..start + size_y].fill(value);
                            }
                        }
                    }
                    Axis::Y => {
                        for y in 0..v.size_y {
                            let value = compute(v.block_y(y));
                            for z in 0..v.size_z {
                                for x in 0..v.size_x {
                                    out[v.index(x, y, z)] = value;
                                }
                            }
                        }
                    }
                    Axis::Z => {
                        let plane = (v.size_x * v.size_y) as usize;
                        for z in 0..v.size_z {
                            let value = compute(v.block_z(z));
                            let start = v.index(0, 0, z);
                            out[start..start + plane].fill(value);
                        }
                    }
                }
            }
            Op::Add { left, right } => {
                self.binary_volume(ctx, left, right, out, v, mask, |l, r| l + r);
            }
            Op::Sub { left, right } => {
                self.binary_volume(ctx, left, right, out, v, mask, |l, r| l + -r);
            }
            Op::Mul { left, right } => {
                self.binary_volume(ctx, left, right, out, v, mask, |l, r| l * r);
            }
            Op::Div { left, right } => {
                self.binary_volume(ctx, left, right, out, v, mask, |l, r| l / r);
            }
            Op::Min { left, right, .. } => {
                self.binary_volume(
                    ctx,
                    left,
                    right,
                    out,
                    v,
                    mask,
                    |l, r| if r < l { r } else { l },
                );
            }
            Op::Max { left, right, .. } => {
                self.binary_volume(
                    ctx,
                    left,
                    right,
                    out,
                    v,
                    mask,
                    |l, r| if r > l { r } else { l },
                );
            }
            Op::ConstAdd { input, value } => {
                self.volume_masked(ctx, input, out, v, mask);
                map_in_place(out, |x| x + value);
            }
            Op::ConstSub { value, input } => {
                self.volume_masked(ctx, input, out, v, mask);
                map_in_place(out, |x| value - x);
            }
            Op::ConstMul { input, value } => {
                self.volume_masked(ctx, input, out, v, mask);
                map_in_place(out, |x| x * value);
            }
            Op::ConstDiv { value, input } => {
                self.volume_masked(ctx, input, out, v, mask);
                map_in_place(out, |x| value / x);
            }
            Op::ConstMin { input, value } => {
                self.volume_masked(ctx, input, out, v, mask);
                map_in_place(out, |x| if value < x { value } else { x });
            }
            Op::ConstMax { input, value } => {
                self.volume_masked(ctx, input, out, v, mask);
                map_in_place(out, |x| if value > x { value } else { x });
            }
            Op::Abs { input } => self.unary_volume(ctx, input, out, v, mask, f32::abs),
            Op::Square { input } => self.unary_volume(ctx, input, out, v, mask, square_f),
            Op::Cube { input } => self.unary_volume(ctx, input, out, v, mask, cube_f),
            Op::Sqrt { input } => self.unary_volume(ctx, input, out, v, mask, sqrt_f),
            Op::LeakyRelu {
                input,
                negative_factor,
            } => self.unary_volume(ctx, input, out, v, mask, |x| leaky_relu(negative_factor, x)),
            Op::Reciprocal { input } => self.unary_volume(ctx, input, out, v, mask, |x| 1.0 / x),
            Op::Negate { input } => self.unary_volume(ctx, input, out, v, mask, |x| -x),
            Op::Squeeze { input } => self.unary_volume(ctx, input, out, v, mask, squeeze),
            Op::Log { input } => {
                self.unary_volume(ctx, input, out, v, mask, |x| f64::from(x).ln() as f32);
            }
            Op::Sign { input } => self.unary_volume(ctx, input, out, v, mask, signum_f),
            Op::RoundToInteger { mode, input } => {
                self.unary_volume(ctx, input, out, v, mask, |x| mode.round_to_integer(x));
            }
            Op::Round {
                mode,
                input,
                multiple,
            } => {
                self.binary_volume(ctx, input, multiple, out, v, mask, |i, m| {
                    round_multiple(mode, i, m)
                });
            }
            Op::Pow { base, exponent } => {
                self.binary_volume(ctx, base, exponent, out, v, mask, |b, e| {
                    java_pow(f64::from(b), f64::from(e)) as f32
                });
            }
            Op::PowConstBase { base, exponent } => {
                self.unary_volume(ctx, exponent, out, v, mask, |e| {
                    java_pow(base, f64::from(e)) as f32
                });
            }
            Op::PowConstExponent { base, exponent } => {
                self.unary_volume(ctx, base, out, v, mask, |b| {
                    java_pow(f64::from(b), exponent) as f32
                });
            }
            Op::Lerp {
                alpha,
                first,
                second,
            } => {
                self.volume_masked(ctx, alpha, out, v, mask);
                // `first` is read where alpha != 1, `second` where alpha != 0.
                let mut first_buffer = ctx.acquire(out.len());
                self.volume_branch(ctx, first, &mut first_buffer, v, mask, out, |a| a != 1.0);
                let mut second_buffer = ctx.acquire(out.len());
                self.volume_branch(ctx, second, &mut second_buffer, v, mask, out, |a| a != 0.0);
                for ((a, &f), &s) in out
                    .iter_mut()
                    .zip(first_buffer.iter())
                    .zip(second_buffer.iter())
                {
                    *a = lerp_select(*a, f, s);
                }
                ctx.release(second_buffer);
                ctx.release(first_buffer);
            }
            Op::LerpConstFirst {
                alpha,
                first,
                second,
            } => {
                // `second` is read where alpha != 0.
                self.volume_masked(ctx, alpha, out, v, mask);
                let mut second_buffer = ctx.acquire(out.len());
                self.volume_branch(ctx, second, &mut second_buffer, v, mask, out, |a| a != 0.0);
                for (a, &s) in out.iter_mut().zip(second_buffer.iter()) {
                    *a = lerp_select(*a, first, s);
                }
                ctx.release(second_buffer);
            }
            Op::LerpConstSecond {
                alpha,
                first,
                second,
            } => {
                // `first` is read where alpha != 1.
                self.volume_masked(ctx, alpha, out, v, mask);
                let mut first_buffer = ctx.acquire(out.len());
                self.volume_branch(ctx, first, &mut first_buffer, v, mask, out, |a| a != 1.0);
                for (a, &f) in out.iter_mut().zip(first_buffer.iter()) {
                    *a = lerp_select(*a, f, second);
                }
                ctx.release(first_buffer);
            }
            Op::Clamp { input, min, max } => {
                self.unary_volume(ctx, input, out, v, mask, |x| clamp_f(x, min, max));
            }
            Op::RangeChoice {
                input,
                min_inclusive,
                max_exclusive,
                when_in_range,
                when_out_of_range,
            } => {
                let chosen = |x| in_range(x, min_inclusive, max_exclusive);
                let mut input_buffer = ctx.acquire(out.len());
                if self.maskable[when_in_range as usize]
                    && !self.other_volumes[when_in_range as usize]
                    && !self.other_volumes[input as usize]
                {
                    // The input first, so `when_in_range` is only sampled where it is chosen.
                    // Both sample one volume only, so their caches end up the same.
                    self.volume_masked(ctx, input, &mut input_buffer, v, mask);
                    self.volume_branch(ctx, when_in_range, out, v, mask, &input_buffer, chosen);
                } else {
                    self.volume_masked(ctx, when_in_range, out, v, mask);
                    self.volume_masked(ctx, input, &mut input_buffer, v, mask);
                }
                let mut out_of_range = ctx.acquire(out.len());
                self.volume_branch(
                    ctx,
                    when_out_of_range,
                    &mut out_of_range,
                    v,
                    mask,
                    &input_buffer,
                    |x| !chosen(x),
                );
                for i in 0..out.len() {
                    if !in_range(input_buffer[i], min_inclusive, max_exclusive) {
                        out[i] = out_of_range[i];
                    }
                }
                ctx.release(out_of_range);
                ctx.release(input_buffer);
            }
            Op::RangeChoiceConst {
                input,
                min_inclusive,
                max_exclusive,
                when_in_range,
                when_out_of_range,
            } => {
                self.unary_volume(ctx, input, out, v, mask, |x| {
                    if in_range(x, min_inclusive, max_exclusive) {
                        when_in_range
                    } else {
                        when_out_of_range
                    }
                });
            }
            Op::IntervalSelectSingle {
                input,
                threshold,
                if_below,
                if_above,
            } => {
                self.volume_masked(ctx, input, out, v, mask);
                let mut below = ctx.acquire(out.len());
                self.volume_branch(ctx, if_below, &mut below, v, mask, out, |x| x < threshold);
                let mut above = ctx.acquire(out.len());
                // The `else` of `x < threshold`, NaN included.
                self.volume_branch(ctx, if_above, &mut above, v, mask, out, |x| {
                    x >= threshold || x.is_nan()
                });
                for i in 0..out.len() {
                    out[i] = if out[i] < threshold {
                        below[i]
                    } else {
                        above[i]
                    };
                }
                ctx.release(above);
                ctx.release(below);
            }
            Op::IntervalSelect { input, table } => {
                self.volume_masked(ctx, input, out, v, mask);
                let table = &self.interval_selects[table as usize];
                let base = ctx.slots.len();
                for (index, &function) in table.functions.iter().enumerate() {
                    let mut buffer = ctx.acquire(out.len());
                    self.volume_branch(ctx, function, &mut buffer, v, mask, out, |x| {
                        select_index(&table.thresholds, x) == index
                    });
                    ctx.slots.push(buffer);
                }
                for (i, value) in out.iter_mut().enumerate() {
                    *value = ctx.slots[base + select_index(&table.thresholds, *value)][i];
                }
                while ctx.slots.len() > base {
                    let buffer = ctx.slots.pop().expect("slot");
                    ctx.release(buffer);
                }
            }
            Op::Cached { cell, input } => match mask {
                Some(mask)
                    if ctx.caching
                        && self
                            .partial_cells
                            .get(cell as usize)
                            .copied()
                            .unwrap_or(false) =>
                {
                    self.volume_cached_masked(ctx, cell, input, out, v, mask);
                }
                _ => self.volume_cached(ctx, cell, input, out, v),
            },
            // No blender is bound (see `SamplerContext::context_field`): the identity.
            Op::BlendDensity { input } => self.volume_masked(ctx, input, out, v, mask),
            Op::Interpolated {
                input,
                cell_size_xz,
                cell_size_y,
                cell_size_xz_inv,
                cell_size_y_inv,
            } => {
                let cell = InterpolationCell {
                    input,
                    size_xz: cell_size_xz,
                    size_y: cell_size_y,
                    size_xz_inv: cell_size_xz_inv,
                    size_y_inv: cell_size_y_inv,
                };
                self.interpolated_volume(ctx, &cell, out, v);
            }
            Op::SliceX { input, x: sx } => {
                if v.size_x == 1 && v.min_x == sx {
                    return self.volume_masked(ctx, input, out, v, mask);
                }
                let input_volume = DensityVolume::with_step(
                    1, v.size_y, v.size_z, sx, v.min_y, v.min_z, v.step_x, v.step_y, v.step_z,
                );
                let mut buffer = ctx.acquire(input_volume.len());
                self.volume(ctx, input, &mut buffer, &input_volume);
                let mut index = 0;
                for z in 0..v.size_z {
                    for _ in 0..v.size_x {
                        for y in 0..v.size_y {
                            out[index] = buffer[input_volume.index(0, y, z)];
                            index += 1;
                        }
                    }
                }
                ctx.release(buffer);
            }
            Op::SliceY { input, y: sy } => {
                if v.size_y == 1 && v.min_y == sy {
                    return self.volume_masked(ctx, input, out, v, mask);
                }
                let input_volume = DensityVolume::with_step(
                    v.size_x, 1, v.size_z, v.min_x, sy, v.min_z, v.step_x, v.step_y, v.step_z,
                );
                let mut buffer = ctx.acquire(input_volume.len());
                self.volume(ctx, input, &mut buffer, &input_volume);
                let size_y = v.size_y as usize;
                for z in 0..v.size_z {
                    for x in 0..v.size_x {
                        let value = buffer[input_volume.index(x, 0, z)];
                        let start = v.index(x, 0, z);
                        out[start..start + size_y].fill(value);
                    }
                }
                ctx.release(buffer);
            }
            Op::SliceZ { input, z: sz } => {
                if v.size_z == 1 && v.min_z == sz {
                    return self.volume_masked(ctx, input, out, v, mask);
                }
                let input_volume = DensityVolume::with_step(
                    v.size_x, v.size_y, 1, v.min_x, v.min_y, sz, v.step_x, v.step_y, v.step_z,
                );
                let mut buffer = ctx.acquire(input_volume.len());
                self.volume(ctx, input, &mut buffer, &input_volume);
                let mut index = 0;
                for _ in 0..v.size_z {
                    for x in 0..v.size_x {
                        for y in 0..v.size_y {
                            out[index] = buffer[input_volume.index(x, y, 0)];
                            index += 1;
                        }
                    }
                }
                ctx.release(buffer);
            }
            Op::SliceXz {
                input,
                x: sx,
                z: sz,
            } => {
                if v.size_x == 1 && v.size_z == 1 && v.min_x == sx && v.min_z == sz {
                    return self.volume_masked(ctx, input, out, v, mask);
                }
                let input_volume = DensityVolume::with_step(
                    1, v.size_y, 1, sx, v.min_y, sz, v.step_x, v.step_y, v.step_z,
                );
                let mut buffer = ctx.acquire(input_volume.len());
                self.volume(ctx, input, &mut buffer, &input_volume);
                let size_y = v.size_y as usize;
                for y in 0..v.size_y {
                    let value = buffer[input_volume.index(0, y, 0)];
                    let mut index = v.index(0, y, 0);
                    for _ in 0..v.size_z * v.size_x {
                        out[index] = value;
                        index += size_y;
                    }
                }
                ctx.release(buffer);
            }
            Op::FindTopSurface {
                density,
                upper_bound,
                lower_bound,
                cell_height,
            } => {
                assert_eq!(
                    v.size_y, 1,
                    "find_top_surface cannot sample with size_y != 1"
                );
                self.volume(ctx, upper_bound, out, v);
                let mut index = 0;
                for z in 0..v.size_z {
                    let block_z = v.block_z(z);
                    for x in 0..v.size_x {
                        let upper = out[index];
                        out[index] = self.find_surface(
                            ctx,
                            density,
                            lower_bound,
                            cell_height,
                            v.block_x(x),
                            block_z,
                            upper,
                        );
                        index += 1;
                    }
                }
            }
            Op::Spline { spline } => {
                self.spline_volume(ctx, &self.splines[spline as usize], out, v, mask);
            }
        }
    }

    /// Samples a branch that is read where `test(values[i])` holds (within `mask`): not at all
    /// if nowhere, else masked to those points when that saves work (see `Program::maskable`).
    #[allow(clippy::too_many_arguments)]
    fn volume_branch(
        &self,
        ctx: &mut SamplerContext,
        op: OpIndex,
        out: &mut [f32],
        v: &DensityVolume,
        mask: Mask<'_>,
        values: &[f32],
        test: impl Fn(f32) -> bool,
    ) {
        if !self.maskable[op as usize] {
            let any = match mask {
                Some(mask) => mask.iter().zip(values).any(|(&m, &x)| m && test(x)),
                None => values.iter().any(|&x| test(x)),
            };
            if any {
                self.volume_masked(ctx, op, out, v, mask);
            }
            return;
        }
        let chosen = refine(mask, values, test);
        let count = chosen.iter().filter(|&&c| c).count();
        if count == 0 {
            return;
        }
        let all = match mask {
            Some(mask) => count == mask.iter().filter(|&&m| m).count(),
            None => count == chosen.len(),
        };
        if all {
            self.volume_masked(ctx, op, out, v, mask);
        } else {
            self.volume_masked(ctx, op, out, v, Some(&chosen));
        }
    }

    /// Samples `left` into `out` and `right` into a scratch buffer, then combines them.
    #[allow(clippy::too_many_arguments)]
    fn binary_volume(
        &self,
        ctx: &mut SamplerContext,
        left: OpIndex,
        right: OpIndex,
        out: &mut [f32],
        v: &DensityVolume,
        mask: Mask<'_>,
        f: impl Fn(f32, f32) -> f32,
    ) {
        self.volume_masked(ctx, left, out, v, mask);
        let mut right_buffer = ctx.acquire(out.len());
        self.volume_masked(ctx, right, &mut right_buffer, v, mask);
        for (l, &r) in out.iter_mut().zip(right_buffer.iter()) {
            *l = f(*l, r);
        }
        ctx.release(right_buffer);
    }

    /// Samples `input` into `out`, then maps every value.
    fn unary_volume(
        &self,
        ctx: &mut SamplerContext,
        input: OpIndex,
        out: &mut [f32],
        v: &DensityVolume,
        mask: Mask<'_>,
        f: impl Fn(f32) -> f32,
    ) {
        self.volume_masked(ctx, input, out, v, mask);
        map_in_place(out, f);
    }

    /// `volume_cached` of a cell only the masked-cache root reads, at the points of `mask`:
    /// the cell keeps which points it holds and samples only those it lacks.
    fn volume_cached_masked(
        &self,
        ctx: &mut SamplerContext,
        cell_id: u32,
        input: OpIndex,
        out: &mut [f32],
        v: &DensityVolume,
        mask: &[bool],
    ) {
        let cell = ctx.cell(cell_id);
        if cell.volume == Some(*v) {
            if !cell.partial || mask.iter().zip(&cell.computed).all(|(&m, &c)| !m || c) {
                out.copy_from_slice(&cell.buffer);
                return;
            }
            // Sample the missing points and add them to the cell.
            let missing: Vec<bool> = mask
                .iter()
                .zip(&cell.computed)
                .map(|(&m, &c)| m && !c)
                .collect();
            self.volume_masked(ctx, input, out, v, Some(&missing));
            let cell = ctx.cell(cell_id);
            for (i, &m) in missing.iter().enumerate() {
                if m {
                    cell.buffer[i] = out[i];
                    cell.computed[i] = true;
                }
            }
            out.copy_from_slice(&cell.buffer);
            return;
        }
        self.volume_masked(ctx, input, out, v, Some(mask));
        let cell = ctx.cell(cell_id);
        cell.buffer.clear();
        cell.buffer.extend_from_slice(out);
        cell.computed.clear();
        cell.computed.extend_from_slice(mask);
        cell.volume = Some(*v);
        cell.partial = true;
    }

    /// `SamplerContext.sampleVolumeCached`.
    fn volume_cached(
        &self,
        ctx: &mut SamplerContext,
        cell_id: u32,
        input: OpIndex,
        out: &mut [f32],
        v: &DensityVolume,
    ) {
        if !ctx.caching {
            return self.volume(ctx, input, out, v);
        }
        let cell = ctx.cell(cell_id);
        if cell.volume != Some(*v) || cell.partial {
            cell.volume = None;
            cell.partial = false;
            let mut buffer = std::mem::take(&mut cell.buffer);
            if buffer.len() >= out.len() {
                buffer.truncate(out.len());
            } else {
                buffer.resize(out.len(), 0.0);
            }
            self.volume(ctx, input, &mut buffer, v);
            let cell = ctx.cell(cell_id);
            cell.buffer = buffer;
            cell.volume = Some(*v);
        }
        out.copy_from_slice(&ctx.cells[cell_id as usize].buffer);
    }

    /// `SplineFunction.Sampler.sampleVolume` with a `BufferSplineInput`: each coordinate is
    /// sampled over the whole volume when a point first needs it.
    fn spline_volume(
        &self,
        ctx: &mut SamplerContext,
        spline: &SplineProgram,
        out: &mut [f32],
        v: &DensityVolume,
        mask: Mask<'_>,
    ) {
        let base = ctx.slots.len();
        ctx.slots
            .resize_with(base + spline.coordinates.len(), Scratch::default);
        let len = out.len();
        for (i, value) in out.iter_mut().enumerate() {
            if !needed(mask, i) {
                continue;
            }
            *value = spline.sample(spline.root, &mut |slot| {
                let slot_index = base + slot as usize;
                if ctx.slots[slot_index].is_empty() {
                    let mut buffer = ctx.acquire(len);
                    let coordinate = spline.coordinates[slot as usize];
                    self.volume_masked(ctx, coordinate, &mut buffer, v, mask);
                    ctx.slots[slot_index] = buffer;
                }
                ctx.slots[slot_index][i]
            });
        }
        while ctx.slots.len() > base {
            let buffer = ctx.slots.pop().expect("slot");
            ctx.release(buffer);
        }
    }

    /// `InterpolatedFunction.Sampler.sampleVolume`.
    fn interpolated_volume(
        &self,
        ctx: &mut SamplerContext,
        cell: &InterpolationCell,
        out: &mut [f32],
        v: &DensityVolume,
    ) {
        let (xz, y) = (cell.size_xz, cell.size_y);
        if (v.step_x == xz || v.size_x == 1)
            && (v.step_y == y || v.size_y == 1)
            && (v.step_z == xz || v.size_z == 1)
            && floor_mod(v.min_x, xz) == 0
            && floor_mod(v.min_y, y) == 0
            && floor_mod(v.min_z, xz) == 0
        {
            self.volume(ctx, cell.input, out, v);
        } else if v.step_x == 1 && v.step_y == 1 && v.step_z == 1 {
            self.interpolate_block_step(ctx, cell, out, v);
        } else {
            // `sampleWithNonBlockStep`: interpolate every block, then pick the volume's points.
            let block_volume = DensityVolume::new(
                v.size_x * v.step_x,
                v.size_y * v.step_y,
                v.size_z * v.step_z,
                v.min_x,
                v.min_y,
                v.min_z,
            );
            let mut buffer = ctx.acquire(block_volume.len());
            self.interpolate_block_step(ctx, cell, &mut buffer, &block_volume);
            for z in 0..v.size_z {
                for x in 0..v.size_x {
                    for y in 0..v.size_y {
                        out[v.index(x, y, z)] =
                            buffer[block_volume.index(x * v.step_x, y * v.step_y, z * v.step_z)];
                    }
                }
            }
            ctx.release(buffer);
        }
    }

    /// `InterpolatedFunction.Sampler.sampleWithBlockStep`: samples the cell corners covering
    /// the volume, then fills each cell.
    fn interpolate_block_step(
        &self,
        ctx: &mut SamplerContext,
        cell: &InterpolationCell,
        out: &mut [f32],
        v: &DensityVolume,
    ) {
        let (xz, cy) = (cell.size_xz, cell.size_y);
        let min_cell_x = floor_div(v.min_x, xz);
        let min_cell_y = floor_div(v.min_y, cy);
        let min_cell_z = floor_div(v.min_z, xz);
        let max_cell_x = floor_div(v.max_x(), xz);
        let max_cell_y = floor_div(v.max_y(), cy);
        let max_cell_z = floor_div(v.max_z(), xz);
        let count_x = max_cell_x - min_cell_x + 1;
        let count_y = max_cell_y - min_cell_y + 1;
        let count_z = max_cell_z - min_cell_z + 1;
        let extra = |max: i32, size: i32, count: i32| {
            if floor_mod(max, size) == 0 {
                count
            } else {
                count + 1
            }
        };
        let cells = DensityVolume::with_step(
            extra(v.max_x(), xz, count_x),
            extra(v.max_y(), cy, count_y),
            extra(v.max_z(), xz, count_z),
            min_cell_x * xz,
            min_cell_y * cy,
            min_cell_z * xz,
            xz,
            cy,
            xz,
        );
        let mut corners = ctx.acquire(cells.len());
        self.volume(ctx, cell.input, &mut corners, &cells);
        for cell_z in 0..count_z {
            let next_z = (cell_z + 1).min(cells.size_z - 1);
            for cell_x in 0..count_x {
                let next_x = (cell_x + 1).min(cells.size_x - 1);
                let mut v000 = corners[cells.index(cell_x, 0, cell_z)];
                let mut v100 = corners[cells.index(next_x, 0, cell_z)];
                let mut v001 = corners[cells.index(cell_x, 0, next_z)];
                let mut v101 = corners[cells.index(next_x, 0, next_z)];
                for cell_y in 0..count_y {
                    let next_y = (cell_y + 1).min(cells.size_y - 1);
                    let v010 = corners[cells.index(cell_x, next_y, cell_z)];
                    let v110 = corners[cells.index(next_x, next_y, cell_z)];
                    let v011 = corners[cells.index(cell_x, next_y, next_z)];
                    let v111 = corners[cells.index(next_x, next_y, next_z)];
                    fill_cell(
                        cell,
                        out,
                        v,
                        &cells,
                        [cell_x, cell_y, cell_z],
                        [v000, v100, v010, v110, v001, v101, v011, v111],
                    );
                    v000 = v010;
                    v100 = v110;
                    v001 = v011;
                    v101 = v111;
                }
            }
        }
        ctx.release(corners);
    }
}

/// The parameters of an [`Op::Interpolated`].
struct InterpolationCell {
    input: OpIndex,
    size_xz: i32,
    size_y: i32,
    size_xz_inv: f32,
    size_y_inv: f32,
}

/// `InterpolatedFunction.Sampler.fillCell`: the part of one cell inside the output volume,
/// interpolated in Z, then X, then stepped along Y (`value += valueStep`).
fn fill_cell(
    cell: &InterpolationCell,
    out: &mut [f32],
    v: &DensityVolume,
    cells: &DensityVolume,
    [cell_x, cell_y, cell_z]: [i32; 3],
    [v000, v100, v010, v110, v001, v101, v011, v111]: [f32; 8],
) {
    let out_x = cells.block_x(cell_x) - v.min_x;
    let out_y = cells.block_y(cell_y) - v.min_y;
    let out_z = cells.block_z(cell_z) - v.min_z;
    let x0 = 0.max(-out_x);
    let y0 = 0.max(-out_y);
    let z0 = 0.max(-out_z);
    let x1 = cell.size_xz.min(v.size_x - out_x) - 1;
    let y1 = cell.size_y.min(v.size_y - out_y) - 1;
    let z1 = cell.size_xz.min(v.size_z - out_z) - 1;
    for z in z0..=z1 {
        let output_z = out_z + z;
        let alpha_z = z as f32 * cell.size_xz_inv;
        let v00 = lerp_f(alpha_z, v000, v001);
        let v01 = lerp_f(alpha_z, v010, v011);
        let v10 = lerp_f(alpha_z, v100, v101);
        let v11 = lerp_f(alpha_z, v110, v111);
        for x in x0..=x1 {
            let output_x = out_x + x;
            let alpha_x = x as f32 * cell.size_xz_inv;
            let v0 = lerp_f(alpha_x, v00, v10);
            let v1 = lerp_f(alpha_x, v01, v11);
            let step = (v1 - v0) * cell.size_y_inv;
            let mut value = v0 + step * y0 as f32;
            if y1 >= y0 {
                let start = v.index(output_x, out_y + y0, output_z);
                for slot in &mut out[start..=start + (y1 - y0) as usize] {
                    *slot = value;
                    value += step;
                }
            }
        }
    }
}

/// `RoundFunction.Sampler.apply(input, multiple)`.
#[inline]
fn round_multiple(mode: super::function::RoundMode, input: f32, multiple: f32) -> f32 {
    if multiple == 0.0 {
        input
    } else {
        mode.round_to_integer(input / multiple) * multiple
    }
}
