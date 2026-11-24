//! The density function tree: Vanilla's `DensityFunction` records (`DensityFunctions`, `op/*`,
//! `generator/*`, `BlendedNoise`, `DensityFunctions.HolderHolder`), their `range()`,
//! `domainAxes()` and `rewriteChildren(rule)`.
//!
//! Nodes are immutable and shared. Each node stores its range, domain axes and structural hash,
//! computed once when it is built (Vanilla recomputes them on every call; the values are the
//! same because nodes never change).

use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use rustc_hash::FxHasher;

use super::spline::Spline;
use crate::interval::Interval;
use crate::mth::{cube_f, max_f};
use crate::noise::{NormalNoise, blended_noise_range};

/// `DensityFunction.AXIS_X`.
pub(crate) const AXIS_X: u8 = 1;
/// `DensityFunction.AXIS_Y`.
pub(crate) const AXIS_Y: u8 = 2;
/// `DensityFunction.AXIS_Z`.
pub(crate) const AXIS_Z: u8 = 4;
/// `DensityFunction.ALL_AXES`.
pub(crate) const ALL_AXES: u8 = 7;

/// A `float` compared and hashed by its bits, like the `float` components of Java records.
#[derive(Clone, Copy, Debug)]
pub(crate) struct F32(pub f32);

impl PartialEq for F32 {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}

impl Eq for F32 {}

impl Hash for F32 {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.to_bits().hash(state);
    }
}

/// A `double` compared and hashed by its bits.
#[derive(Clone, Copy, Debug)]
pub(crate) struct F64(pub f64);

impl PartialEq for F64 {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}

impl Eq for F64 {}

impl Hash for F64 {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.to_bits().hash(state);
    }
}

/// `Direction.Axis`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Axis {
    X,
    Y,
    Z,
}

impl Axis {
    /// `DensityFunction.axesFrom(axis)`.
    pub(crate) fn bit(self) -> u8 {
        match self {
            Axis::X => AXIS_X,
            Axis::Y => AXIS_Y,
            Axis::Z => AXIS_Z,
        }
    }

    /// `Axis.choose(x, y, z)`.
    #[inline]
    pub fn choose(self, x: i32, y: i32, z: i32) -> i32 {
        match self {
            Axis::X => x,
            Axis::Y => y,
            Axis::Z => z,
        }
    }
}

/// `TilingMode` of a `gradient`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum TilingMode {
    ClampToEdge,
    Repeat,
    MirroredRepeat,
}

/// `DistanceMetric`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DistanceMetric {
    Euclidean,
    EuclideanSquared,
    Manhattan,
    Chebyshev,
}

/// The context-bound functions (`SimpleDensityFunction`); see `ContextField` in the program.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum SimpleKind {
    BlendAlpha,
    BlendOffset,
    Beardifier,
}

/// `ShiftNoiseFunction` variants.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ShiftKind {
    /// `shift`: all three axes.
    Shift,
    /// `shift_a`: `(x, 0, z)`.
    ShiftA,
    /// `shift_b`: `(z, x, 0)`.
    ShiftB,
}

/// `UnaryFunction.Type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum UnaryOp {
    Abs,
    Square,
    Cube,
    Sqrt,
    HalfNegative,
    QuarterNegative,
    Reciprocal,
    Negate,
    Squeeze,
    Log,
    Sign,
}

/// `RoundFunction.Type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RoundMode {
    Floor,
    Round,
    Ceil,
    Truncate,
}

impl RoundMode {
    /// `RoundFunction.roundToInteger(input, type)`.
    #[inline]
    pub fn round_to_integer(self, input: f32) -> f32 {
        match self {
            RoundMode::Floor => input.floor(),
            RoundMode::Round => crate::mth::round_f(input) as f32,
            RoundMode::Ceil => input.ceil(),
            RoundMode::Truncate => {
                if input > 0.0 {
                    input.floor()
                } else {
                    input.ceil()
                }
            }
        }
    }
}

/// `BinaryFunction.Type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Min,
    Max,
}

/// `LeakyReLUSampler.apply(negativeFactor, input)`.
#[inline]
pub(crate) fn leaky_relu(negative_factor: f32, input: f32) -> f32 {
    if input > 0.0 {
        input
    } else {
        input * negative_factor
    }
}

/// `SqueezeSampler.apply(input)`.
#[inline]
pub(crate) fn squeeze(input: f32) -> f32 {
    let clamped = crate::mth::clamp_f(input, -1.0, 1.0);
    clamped / 2.0 - cube_f(clamped) / 24.0
}

/// A `worldgen/noise` holder: compared by id (`Holder.Reference` identity).
#[derive(Clone)]
pub(crate) struct NoiseRef {
    pub id: Arc<str>,
    pub params: Arc<NormalNoise>,
}

impl PartialEq for NoiseRef {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for NoiseRef {}

impl Hash for NoiseRef {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl fmt::Debug for NoiseRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.id)
    }
}

/// A `worldgen/density_function` registry entry (`Holder.Reference`): compared by id.
pub(crate) struct Holder {
    pub id: Arc<str>,
    pub value: DensityFunction,
}

impl PartialEq for Holder {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for Holder {}

impl Hash for Holder {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl fmt::Debug for Holder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.id)
    }
}

/// One density function record.
#[derive(Debug, PartialEq, Eq, Hash)]
pub(crate) enum Kind {
    /// `ConstantFunction`.
    Constant(F32),
    /// `SimpleDensityFunction` (`blend_alpha`, `blend_offset`, `beardifier`).
    Simple(SimpleKind),
    /// `NoiseFunction`.
    Noise {
        noise: NoiseRef,
        xz_scale: F64,
        y_scale: F64,
        shift_x: DensityFunction,
        shift_y: DensityFunction,
        shift_z: DensityFunction,
    },
    /// `EndIslandFunction`.
    EndIslands,
    /// `DistanceToPointFunction`.
    DistanceToPoint {
        point: [i32; 3],
        metric: DistanceMetric,
    },
    /// `GradientFunction`.
    Gradient {
        axis: Axis,
        tiling: TilingMode,
        from_coordinate: i32,
        to_coordinate: i32,
        from_value: F32,
        to_value: F32,
    },
    /// `ShiftNoiseFunction.Shift`, `ShiftA`, `ShiftB`.
    Shift { kind: ShiftKind, noise: NoiseRef },
    /// `UnaryFunction`.
    Unary(UnaryOp, DensityFunction),
    /// `RoundFunction`.
    Round {
        mode: RoundMode,
        input: DensityFunction,
        multiple: DensityFunction,
    },
    /// `BinaryFunction`.
    Binary(BinaryOp, DensityFunction, DensityFunction),
    /// `PowFunction`.
    Pow {
        base: DensityFunction,
        exponent: DensityFunction,
    },
    /// `SplineFunction`.
    Spline(Spline),
    /// `LerpFunction`.
    Lerp {
        alpha: DensityFunction,
        first: DensityFunction,
        second: DensityFunction,
    },
    /// `ClampFunction`.
    Clamp {
        input: DensityFunction,
        min: F32,
        max: F32,
    },
    /// `RangeChoiceFunction`.
    RangeChoice {
        input: DensityFunction,
        min_inclusive: F32,
        max_exclusive: F32,
        when_in_range: DensityFunction,
        when_out_of_range: DensityFunction,
    },
    /// `IntervalSelectFunction`.
    IntervalSelect {
        input: DensityFunction,
        thresholds: Box<[F32]>,
        functions: Box<[DensityFunction]>,
    },
    /// `CacheFunction`.
    Cache(DensityFunction),
    /// `BlendDensityFunction`.
    BlendDensity(DensityFunction),
    /// `InterpolatedFunction`.
    Interpolated {
        input: DensityFunction,
        cell_size_xz: i32,
        cell_size_y: i32,
    },
    /// `SliceFunction`.
    Slice {
        axis: Axis,
        coordinate: i32,
        input: DensityFunction,
    },
    /// `FindTopSurfaceFunction`.
    FindTopSurface {
        density: DensityFunction,
        upper_bound: DensityFunction,
        lower_bound: i32,
        cell_height: i32,
    },
    /// `BlendedNoise` (`old_blended_noise`).
    BlendedNoise {
        xz_scale: F64,
        y_scale: F64,
        xz_factor: F64,
        y_factor: F64,
        smear_scale_multiplier: F64,
    },
    /// `DensityFunctions.HolderHolder`: a reference to a registry entry.
    Reference(Arc<Holder>),
    /// `DensityFunctionCompiler.PreparedCache`: a deduplicated `cache` whose input is already
    /// compiled to instruction `sampler` of the program. Only built by the compiler.
    PreparedCache { id: u32, sampler: u32 },
}

pub(crate) struct Node {
    pub kind: Kind,
    range: Interval,
    axes: u8,
    hash: u64,
}

/// A parsed density function (`DensityFunction`): immutable, cheap to clone, compared and hashed
/// structurally with references compared by id, like Vanilla's records and holders.
#[derive(Clone)]
pub struct DensityFunction(Arc<Node>);

impl PartialEq for DensityFunction {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
            || (self.0.hash == other.0.hash && self.0.kind == other.0.kind)
    }
}

impl Eq for DensityFunction {}

impl Hash for DensityFunction {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.0.hash);
    }
}

impl fmt::Debug for DensityFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.kind.fmt(f)
    }
}

impl DensityFunction {
    /// A node for `kind` with its range, axes and hash.
    pub(crate) fn new(kind: Kind) -> Self {
        let mut hasher = FxHasher::default();
        kind.hash(&mut hasher);
        let hash = hasher.finish();
        let (range, axes) = match &kind {
            Kind::PreparedCache { .. } => unreachable!("prepared caches are built by the compiler"),
            _ => (range_of(&kind), axes_of(&kind)),
        };
        Self(Arc::new(Node {
            kind,
            range,
            axes,
            hash,
        }))
    }

    /// `DensityFunctionCompiler.PreparedCache(id, range, domainAxes, cachingSampler)`.
    pub(crate) fn prepared_cache(id: u32, sampler: u32, range: Interval, axes: u8) -> Self {
        let kind = Kind::PreparedCache { id, sampler };
        let mut hasher = FxHasher::default();
        kind.hash(&mut hasher);
        let hash = hasher.finish();
        Self(Arc::new(Node {
            kind,
            range,
            axes,
            hash,
        }))
    }

    /// `ConstantFunction(value)`.
    pub(crate) fn constant(value: f32) -> Self {
        Self::new(Kind::Constant(F32(value)))
    }

    /// The record of this function.
    pub(crate) fn kind(&self) -> &Kind {
        &self.0.kind
    }

    /// `range()`.
    pub fn range(&self) -> Interval {
        self.0.range
    }

    /// `domainAxes()`: the axes the value depends on (bit 1 = X, 2 = Y, 4 = Z).
    pub fn domain_axes(&self) -> u8 {
        self.0.axes
    }

    /// Whether both handles share one node (Java reference identity, `==`).
    pub(crate) fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// `this instanceof ConstantFunction(float value)`.
    pub(crate) fn as_constant(&self) -> Option<f32> {
        match self.0.kind {
            Kind::Constant(F32(value)) => Some(value),
            _ => None,
        }
    }

    /// `this.equals(DensityFunctions.zero())`.
    pub(crate) fn is_zero(&self) -> bool {
        self.as_constant().is_some_and(|v| v.to_bits() == 0)
    }

    /// `rewriteChildren(rule)`: this node with `rule` applied to its direct children, or this
    /// same node when no child changed (by identity).
    pub(crate) fn rewrite_children(&self, rule: &mut dyn FnMut(&Self) -> Self) -> Self {
        let mut map = |f: &Self, changed: &mut bool| {
            let new = rule(f);
            if !new.same(f) {
                *changed = true;
            }
            new
        };
        let mut changed = false;
        let kind = match &self.0.kind {
            Kind::Constant(_)
            | Kind::Simple(_)
            | Kind::EndIslands
            | Kind::DistanceToPoint { .. }
            | Kind::Gradient { .. }
            | Kind::BlendedNoise { .. }
            | Kind::PreparedCache { .. } => return self.clone(),
            // Vanilla's shift functions always return a new record.
            Kind::Shift { kind, noise } => {
                return Self::new(Kind::Shift {
                    kind: *kind,
                    noise: noise.clone(),
                });
            }
            Kind::Reference(holder) => {
                let new = rule(&holder.value);
                return if new.same(&holder.value) {
                    self.clone()
                } else {
                    new
                };
            }
            Kind::Noise {
                noise,
                xz_scale,
                y_scale,
                shift_x,
                shift_y,
                shift_z,
            } => Kind::Noise {
                noise: noise.clone(),
                xz_scale: *xz_scale,
                y_scale: *y_scale,
                shift_x: map(shift_x, &mut changed),
                shift_y: map(shift_y, &mut changed),
                shift_z: map(shift_z, &mut changed),
            },
            Kind::Unary(op, input) => Kind::Unary(*op, map(input, &mut changed)),
            Kind::Round {
                mode,
                input,
                multiple,
            } => Kind::Round {
                mode: *mode,
                input: map(input, &mut changed),
                multiple: map(multiple, &mut changed),
            },
            Kind::Binary(op, left, right) => {
                Kind::Binary(*op, map(left, &mut changed), map(right, &mut changed))
            }
            Kind::Pow { base, exponent } => Kind::Pow {
                base: map(base, &mut changed),
                exponent: map(exponent, &mut changed),
            },
            Kind::Spline(spline) => {
                Kind::Spline(spline.map_coordinates(&mut |f| map(f, &mut changed)))
            }
            Kind::Lerp {
                alpha,
                first,
                second,
            } => Kind::Lerp {
                alpha: map(alpha, &mut changed),
                first: map(first, &mut changed),
                second: map(second, &mut changed),
            },
            Kind::Clamp { input, min, max } => Kind::Clamp {
                input: map(input, &mut changed),
                min: *min,
                max: *max,
            },
            Kind::RangeChoice {
                input,
                min_inclusive,
                max_exclusive,
                when_in_range,
                when_out_of_range,
            } => Kind::RangeChoice {
                input: map(input, &mut changed),
                min_inclusive: *min_inclusive,
                max_exclusive: *max_exclusive,
                when_in_range: map(when_in_range, &mut changed),
                when_out_of_range: map(when_out_of_range, &mut changed),
            },
            Kind::IntervalSelect {
                input,
                thresholds,
                functions,
            } => Kind::IntervalSelect {
                input: map(input, &mut changed),
                thresholds: thresholds.clone(),
                functions: functions.iter().map(|f| map(f, &mut changed)).collect(),
            },
            Kind::Cache(input) => Kind::Cache(map(input, &mut changed)),
            Kind::BlendDensity(input) => Kind::BlendDensity(map(input, &mut changed)),
            Kind::Interpolated {
                input,
                cell_size_xz,
                cell_size_y,
            } => Kind::Interpolated {
                input: map(input, &mut changed),
                cell_size_xz: *cell_size_xz,
                cell_size_y: *cell_size_y,
            },
            Kind::Slice {
                axis,
                coordinate,
                input,
            } => Kind::Slice {
                axis: *axis,
                coordinate: *coordinate,
                input: map(input, &mut changed),
            },
            Kind::FindTopSurface {
                density,
                upper_bound,
                lower_bound,
                cell_height,
            } => Kind::FindTopSurface {
                density: map(density, &mut changed),
                upper_bound: map(upper_bound, &mut changed),
                lower_bound: *lower_bound,
                cell_height: *cell_height,
            },
        };
        if changed {
            Self::new(kind)
        } else {
            self.clone()
        }
    }
}

/// `range()` of a record whose children are already built.
fn range_of(kind: &Kind) -> Interval {
    match kind {
        Kind::Constant(F32(value)) => Interval::of_exact(*value),
        Kind::Simple(SimpleKind::BlendAlpha) => Interval::of(0.0, 1.0),
        // `BLEND_OFFSET` and `Beardifier.RANGE`.
        Kind::Simple(SimpleKind::BlendOffset | SimpleKind::Beardifier) => Interval::INFINITE,
        Kind::Noise { noise, .. } => noise.params.range(),
        Kind::EndIslands => Interval::of(-0.84375, 0.5625),
        Kind::DistanceToPoint { .. } => Interval::of(0.0, f32::INFINITY),
        Kind::Gradient {
            from_value,
            to_value,
            ..
        } => Interval::encapsulating_values(from_value.0, to_value.0),
        Kind::Shift { noise, .. } => noise.params.range() * Interval::of_exact(4.0),
        Kind::Unary(op, input) => {
            let input = input.range();
            match op {
                UnaryOp::Abs => Interval::abs(input),
                UnaryOp::Square => Interval::square(input),
                UnaryOp::Cube => Interval::map_monotonic(input, cube_f),
                UnaryOp::Sqrt => Interval::pow(input, Interval::of_exact(0.5)),
                UnaryOp::HalfNegative => Interval::map_monotonic(input, |v| leaky_relu(0.5, v)),
                UnaryOp::QuarterNegative => Interval::map_monotonic(input, |v| leaky_relu(0.25, v)),
                UnaryOp::Reciprocal => Interval::reciprocal(input),
                UnaryOp::Negate => Interval::of_exact(0.0) - input,
                UnaryOp::Squeeze => Interval::map_monotonic(input, squeeze),
                UnaryOp::Log => Interval::log(input),
                UnaryOp::Sign => Interval::sign(input),
            }
        }
        Kind::Round {
            mode,
            input,
            multiple,
        } => {
            let multiple = multiple.range();
            Interval::map_monotonic(input.range() / multiple, |v| mode.round_to_integer(v))
                * multiple
        }
        Kind::Binary(op, left, right) => {
            let (left, right) = (left.range(), right.range());
            match op {
                BinaryOp::Add => left + right,
                BinaryOp::Sub => left - right,
                BinaryOp::Mul => left * right,
                BinaryOp::Div => left / right,
                BinaryOp::Min => Interval::min_of(left, right),
                BinaryOp::Max => Interval::max_of(left, right),
            }
        }
        Kind::Pow { base, exponent } => Interval::pow(base.range(), exponent.range()),
        Kind::Spline(spline) => spline.range(),
        Kind::Lerp {
            alpha,
            first,
            second,
        } => Interval::lerp(alpha.range(), first.range(), second.range()),
        Kind::Clamp { input, min, max } => Interval::clamp(input.range(), min.0, max.0),
        Kind::RangeChoice {
            when_in_range,
            when_out_of_range,
            ..
        } => Interval::encapsulating(&[when_in_range.range(), when_out_of_range.range()]),
        Kind::IntervalSelect { functions, .. } => {
            let ranges: Vec<Interval> = functions.iter().map(DensityFunction::range).collect();
            Interval::encapsulating(&ranges)
        }
        Kind::Cache(input)
        | Kind::BlendDensity(input)
        | Kind::Interpolated { input, .. }
        | Kind::Slice { input, .. } => input.range(),
        Kind::FindTopSurface {
            upper_bound,
            lower_bound,
            ..
        } => {
            let lower = *lower_bound as f32;
            Interval::of(lower, max_f(lower, upper_bound.range().max()))
        }
        Kind::BlendedNoise {
            y_scale,
            smear_scale_multiplier,
            ..
        } => blended_noise_range(y_scale.0, smear_scale_multiplier.0),
        Kind::Reference(holder) => holder.value.range(),
        Kind::PreparedCache { .. } => unreachable!(),
    }
}

/// `domainAxes()` of a record whose children are already built.
fn axes_of(kind: &Kind) -> u8 {
    match kind {
        Kind::Constant(_) => 0,
        Kind::Simple(SimpleKind::BlendAlpha | SimpleKind::BlendOffset) => AXIS_X | AXIS_Z,
        Kind::Simple(SimpleKind::Beardifier) => ALL_AXES,
        Kind::Noise {
            xz_scale,
            y_scale,
            shift_x,
            shift_y,
            shift_z,
            ..
        } => {
            let mut axes = ALL_AXES;
            if y_scale.0 == 0.0 {
                axes &= !AXIS_Y;
            }
            if xz_scale.0 == 0.0 {
                axes &= !(AXIS_X | AXIS_Z);
            }
            axes | shift_x.domain_axes() | shift_y.domain_axes() | shift_z.domain_axes()
        }
        Kind::EndIslands => AXIS_X | AXIS_Z,
        Kind::DistanceToPoint { .. } => ALL_AXES,
        Kind::Gradient { axis, .. } => axis.bit(),
        Kind::Shift { kind, .. } => match kind {
            ShiftKind::Shift => ALL_AXES,
            ShiftKind::ShiftA | ShiftKind::ShiftB => AXIS_X | AXIS_Z,
        },
        Kind::Unary(_, input)
        | Kind::Cache(input)
        | Kind::BlendDensity(input)
        | Kind::Interpolated { input, .. }
        | Kind::Clamp { input, .. } => input.domain_axes(),
        Kind::Round {
            input, multiple, ..
        } => input.domain_axes() | multiple.domain_axes(),
        Kind::Binary(_, left, right) => left.domain_axes() | right.domain_axes(),
        Kind::Pow { base, exponent } => base.domain_axes() | exponent.domain_axes(),
        Kind::Spline(spline) => {
            let mut axes = 0;
            spline.for_each_coordinate(&mut |f| axes |= f.domain_axes());
            axes
        }
        Kind::Lerp {
            alpha,
            first,
            second,
        } => alpha.domain_axes() | first.domain_axes() | second.domain_axes(),
        Kind::RangeChoice {
            input,
            when_in_range,
            when_out_of_range,
            ..
        } => input.domain_axes() | when_in_range.domain_axes() | when_out_of_range.domain_axes(),
        Kind::IntervalSelect {
            input, functions, ..
        } => functions
            .iter()
            .fold(input.domain_axes(), |axes, f| axes | f.domain_axes()),
        Kind::Slice { axis, input, .. } => input.domain_axes() & !axis.bit(),
        Kind::FindTopSurface {
            density,
            upper_bound,
            ..
        } => (density.domain_axes() | upper_bound.domain_axes()) & !AXIS_Y,
        Kind::BlendedNoise { .. } => ALL_AXES,
        Kind::Reference(holder) => holder.value.domain_axes(),
        Kind::PreparedCache { .. } => unreachable!(),
    }
}
