//! `DensityFunctionCompiler`, `DfRewriteRule` and every function's `compileSampler`: rewrites
//! functions (inline references, deduplicate caches, slice uniform axes) and lowers them into one
//! [`Program`].

use std::sync::Arc;

use rustc_hash::FxHashMap;
use vanillite_base::random::RandomSource;

use super::function::{
    ALL_AXES, AXIS_X, AXIS_Y, AXIS_Z, Axis, BinaryOp, DensityFunction, F32, Kind, ShiftKind,
    SimpleKind, TilingMode, UnaryOp,
};
use super::program::{ContextField, GradientTiling, IntervalSelectTable, Op, OpIndex, Program};
use super::spline::{Spline, SplineMultipoint, SplinePoint, SplineProgram, SplineValue};
use super::{CompileContext, FnId};
use crate::noise::{Noise, blended_fbm_set};
use crate::random::AnyRandom;

/// `BlendedNoise.NOISE_SEED`.
const BLENDED_NOISE_SEED: &str = "minecraft:terrain";
/// `BlendedNoise.BASE_SCALE`.
const BLENDED_BASE_SCALE: f64 = 684.412;

/// `DensityFunctionCompiler`: compiles functions into one shared [`Program`].
pub struct DensityCompiler<'a> {
    ctx: &'a dyn CompileContext,
    /// `samplers`: compiled functions by their original (unrewritten) form.
    samplers: FxHashMap<DensityFunction, FnId>,
    /// `preparedCaches`: by the unrewritten `cache` input.
    prepared_caches: FxHashMap<DensityFunction, DensityFunction>,
    /// Instructions of rewritten functions; equal functions share instructions.
    compiled: FxHashMap<DensityFunction, OpIndex>,
    /// Noise instances by identity.
    noise_indices: FxHashMap<*const Noise, u32>,
    program: Program,
}

impl std::fmt::Debug for DensityCompiler<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DensityCompiler")
            .field("functions", &self.samplers.len())
            .field("ops", &self.program.ops.len())
            .finish_non_exhaustive()
    }
}

impl<'a> DensityCompiler<'a> {
    /// A compiler with an empty program.
    pub fn new(ctx: &'a dyn CompileContext) -> Self {
        Self {
            ctx,
            samplers: FxHashMap::default(),
            prepared_caches: FxHashMap::default(),
            compiled: FxHashMap::default(),
            noise_indices: FxHashMap::default(),
            program: Program::default(),
        }
    }

    /// `getSampler(function)`: compiles `function` once; equal functions share one id.
    pub fn compile(&mut self, function: &DensityFunction) -> FnId {
        if let Some(&id) = self.samplers.get(function) {
            return id;
        }
        let optimized = self.optimize(function);
        let id = FnId(self.compile_node(&optimized));
        self.samplers.insert(function.clone(), id);
        id
    }

    /// The program compiled so far.
    pub fn program(&self) -> &Program {
        &self.program
    }

    /// The finished program.
    pub fn finish(mut self) -> Program {
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        self.program.id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.program
    }

    /// `optimizerRule`: inline references and prepare caches, then slice uniform axes.
    fn optimize(&mut self, function: &DensityFunction) -> DensityFunction {
        let function = self.inline_and_prepare_caches(function);
        slice_uniform_axes(ALL_AXES, &function)
    }

    /// The first optimizer rule: `INLINE_REFERENCE`, then caches become prepared caches and
    /// everything else is rewritten recursively.
    fn inline_and_prepare_caches(&mut self, function: &DensityFunction) -> DensityFunction {
        let function = match function.kind() {
            Kind::Reference(holder) => holder.value.clone(),
            _ => function.clone(),
        };
        if let Kind::Cache(input) = function.kind() {
            return self.reuse_or_prepare_cache(input);
        }
        function.rewrite_children(&mut |child| self.inline_and_prepare_caches(child))
    }

    /// `reuseOrPrepareCache(cache)`: one cache cell per distinct (unrewritten) input.
    fn reuse_or_prepare_cache(&mut self, input: &DensityFunction) -> DensityFunction {
        if let Some(prepared) = self.prepared_caches.get(input) {
            return prepared.clone();
        }
        let id = self.program.cache_cells;
        self.program.cache_cells += 1;
        let optimized = self.optimize(input);
        let sampler = self.compile_node(&optimized);
        let cached = self.program.push(Op::Cached {
            cell: id,
            input: sampler,
        });
        let prepared =
            DensityFunction::prepared_cache(id, cached, optimized.range(), optimized.domain_axes());
        self.prepared_caches.insert(input.clone(), prepared.clone());
        prepared
    }

    fn noise_index(&mut self, noise: Arc<Noise>) -> u32 {
        let next = self.program.noises.len() as u32;
        let index = *self
            .noise_indices
            .entry(Arc::as_ptr(&noise))
            .or_insert(next);
        if index == next {
            self.program.noises.push(noise);
        }
        index
    }

    fn push(&mut self, op: Op) -> OpIndex {
        self.program.push(op)
    }

    /// `compileSampler(context)` of a rewritten function.
    fn compile_node(&mut self, function: &DensityFunction) -> OpIndex {
        if let Some(&index) = self.compiled.get(function) {
            return index;
        }
        let index = self.lower(function);
        self.program.set_range(index, function.range());
        self.compiled.insert(function.clone(), index);
        index
    }

    fn lower(&mut self, function: &DensityFunction) -> OpIndex {
        match function.kind() {
            Kind::Constant(value) => self.push(Op::Constant { value: value.0 }),
            Kind::Simple(kind) => {
                let (field, fallback) = match kind {
                    SimpleKind::BlendAlpha => (ContextField::BlendAlpha, 1.0),
                    SimpleKind::BlendOffset => (ContextField::BlendOffset, 0.0),
                    SimpleKind::Beardifier => (ContextField::Beardifier, 0.0),
                };
                self.push(Op::ContextBound { field, fallback })
            }
            Kind::Noise {
                noise,
                xz_scale,
                y_scale,
                shift_x,
                shift_y,
                shift_z,
            } => {
                let noise = self.noise_index(self.ctx.create_noise(&noise.id));
                let (xz_scale, y_scale) = (xz_scale.0, y_scale.0);
                if shift_x.is_zero() && shift_y.is_zero() && shift_z.is_zero() {
                    return self.push(Op::Noise {
                        noise,
                        xz_scale,
                        y_scale,
                    });
                }
                let shift_x = self.compile_node(shift_x);
                let shift_z = self.compile_node(shift_z);
                if shift_y.is_zero() {
                    self.push(Op::ShiftedNoiseXz {
                        shift_x,
                        shift_z,
                        noise,
                        xz_scale,
                        y_scale,
                    })
                } else {
                    let shift_y = self.compile_node(shift_y);
                    self.push(Op::ShiftedNoiseXyz {
                        shift_x,
                        shift_y,
                        shift_z,
                        noise,
                        xz_scale,
                        y_scale,
                    })
                }
            }
            Kind::EndIslands => {
                let mut random = self.ctx.create_end_island_random();
                random.consume_count(17292);
                let noise = Noise::simplex(&mut AnyRandom::Legacy(random), true);
                let noise = self.noise_index(Arc::new(noise));
                self.push(Op::EndIslands { noise })
            }
            Kind::DistanceToPoint { point, metric } => self.push(Op::DistanceToPoint {
                x: point[0],
                y: point[1],
                z: point[2],
                metric: *metric,
            }),
            Kind::Gradient {
                axis,
                tiling,
                from_coordinate,
                to_coordinate,
                from_value,
                to_value,
            } => {
                let range = to_coordinate.wrapping_sub(*from_coordinate);
                let factor = (to_value.0 - from_value.0) / range as f32;
                let tiling = match tiling {
                    TilingMode::ClampToEdge => GradientTiling::Clamped {
                        min: (*from_coordinate).min(*to_coordinate),
                        max: (*from_coordinate).max(*to_coordinate),
                    },
                    TilingMode::Repeat => GradientTiling::Repeat { range },
                    TilingMode::MirroredRepeat => GradientTiling::MirroredRepeat { range },
                };
                self.push(Op::Gradient {
                    axis: *axis,
                    tiling,
                    from_coordinate: *from_coordinate,
                    from_value: from_value.0,
                    factor,
                })
            }
            Kind::Shift { kind, noise } => {
                let noise = self.noise_index(self.ctx.create_noise(&noise.id));
                let y_scale = match kind {
                    ShiftKind::Shift => 0.25,
                    ShiftKind::ShiftA => 0.0,
                    ShiftKind::ShiftB => return self.push(Op::ShiftB { noise }),
                };
                let input = self.push(Op::Noise {
                    noise,
                    xz_scale: 0.25,
                    y_scale,
                });
                self.push(Op::ConstMul { input, value: 4.0 })
            }
            Kind::Unary(op, input) => {
                let input = self.compile_node(input);
                self.push(match op {
                    UnaryOp::Abs => Op::Abs { input },
                    UnaryOp::Square => Op::Square { input },
                    UnaryOp::Cube => Op::Cube { input },
                    UnaryOp::Sqrt => Op::Sqrt { input },
                    UnaryOp::HalfNegative => Op::LeakyRelu {
                        input,
                        negative_factor: 0.5,
                    },
                    UnaryOp::QuarterNegative => Op::LeakyRelu {
                        input,
                        negative_factor: 0.25,
                    },
                    UnaryOp::Reciprocal => Op::Reciprocal { input },
                    UnaryOp::Negate => Op::Negate { input },
                    UnaryOp::Squeeze => Op::Squeeze { input },
                    UnaryOp::Log => Op::Log { input },
                    UnaryOp::Sign => Op::Sign { input },
                })
            }
            Kind::Round {
                mode,
                input,
                multiple,
            } => {
                let input = self.compile_node(input);
                if multiple.as_constant() == Some(1.0) {
                    return self.push(Op::RoundToInteger { mode: *mode, input });
                }
                let multiple = self.compile_node(multiple);
                self.push(Op::Round {
                    mode: *mode,
                    input,
                    multiple,
                })
            }
            Kind::Binary(op, left_fn, right_fn) => self.lower_binary(*op, left_fn, right_fn),
            Kind::Pow { base, exponent } => {
                let base_op = self.compile_node(base);
                let exponent_op = self.compile_node(exponent);
                if let Some(base) = base.as_constant() {
                    return self.push(Op::PowConstBase {
                        base: f64::from(base),
                        exponent: exponent_op,
                    });
                }
                match exponent.as_constant() {
                    Some(exponent) => self.lower_const_exponent(base_op, exponent),
                    None => self.push(Op::Pow {
                        base: base_op,
                        exponent: exponent_op,
                    }),
                }
            }
            Kind::Spline(spline) => self.lower_spline(spline),
            Kind::Lerp {
                alpha,
                first,
                second,
            } => {
                let alpha_op = self.compile_node(alpha);
                let first_op = self.compile_node(first);
                let second_op = self.compile_node(second);
                if let Some(first) = first.as_constant() {
                    self.push(Op::LerpConstFirst {
                        alpha: alpha_op,
                        first,
                        second: second_op,
                    })
                } else if let Some(second) = second.as_constant() {
                    self.push(Op::LerpConstSecond {
                        alpha: alpha_op,
                        first: first_op,
                        second,
                    })
                } else {
                    self.push(Op::Lerp {
                        alpha: alpha_op,
                        first: first_op,
                        second: second_op,
                    })
                }
            }
            Kind::Clamp { input, min, max } => {
                let input = self.compile_node(input);
                self.push(Op::Clamp {
                    input,
                    min: min.0,
                    max: max.0,
                })
            }
            Kind::RangeChoice {
                input,
                min_inclusive,
                max_exclusive,
                when_in_range,
                when_out_of_range,
            } => {
                let input = self.compile_node(input);
                let (min_inclusive, max_exclusive) = (min_inclusive.0, max_exclusive.0);
                if let (Some(when_in_range), Some(when_out_of_range)) =
                    (when_in_range.as_constant(), when_out_of_range.as_constant())
                {
                    return self.push(Op::RangeChoiceConst {
                        input,
                        min_inclusive,
                        max_exclusive,
                        when_in_range,
                        when_out_of_range,
                    });
                }
                let when_in_range = self.compile_node(when_in_range);
                let when_out_of_range = self.compile_node(when_out_of_range);
                self.push(Op::RangeChoice {
                    input,
                    min_inclusive,
                    max_exclusive,
                    when_in_range,
                    when_out_of_range,
                })
            }
            Kind::IntervalSelect {
                input,
                thresholds,
                functions,
            } => {
                let input = self.compile_node(input);
                if thresholds.len() == 1 {
                    let if_below = self.compile_node(&functions[0]);
                    let if_above = self.compile_node(&functions[functions.len() - 1]);
                    return self.push(Op::IntervalSelectSingle {
                        input,
                        threshold: thresholds[0].0,
                        if_below,
                        if_above,
                    });
                }
                let functions = functions.iter().map(|f| self.compile_node(f)).collect();
                let table = self.program.interval_selects.len() as u32;
                self.program.interval_selects.push(IntervalSelectTable {
                    thresholds: thresholds.iter().map(|t| t.0).collect(),
                    functions,
                });
                self.push(Op::IntervalSelect { input, table })
            }
            Kind::Cache(_) => {
                unreachable!("cannot compile a cache before it has been deduplicated")
            }
            Kind::PreparedCache { sampler, .. } => *sampler,
            Kind::BlendDensity(input) => {
                let input = self.compile_node(input);
                self.push(Op::BlendDensity { input })
            }
            Kind::Interpolated {
                input,
                cell_size_xz,
                cell_size_y,
            } => {
                let input = self.compile_node(input);
                self.push(Op::Interpolated {
                    input,
                    cell_size_xz: *cell_size_xz,
                    cell_size_y: *cell_size_y,
                    cell_size_xz_inv: 1.0 / *cell_size_xz as f32,
                    cell_size_y_inv: 1.0 / *cell_size_y as f32,
                })
            }
            Kind::Slice {
                axis,
                coordinate,
                input,
            } => {
                if let Kind::Slice {
                    axis: inner_axis,
                    coordinate: inner_coordinate,
                    input: inner_input,
                } = input.kind()
                {
                    let xz = match (axis, inner_axis) {
                        (Axis::X, Axis::Z) => Some((*coordinate, *inner_coordinate)),
                        (Axis::Z, Axis::X) => Some((*inner_coordinate, *coordinate)),
                        _ => None,
                    };
                    if let Some((x, z)) = xz {
                        let input = self.compile_node(inner_input);
                        return self.push(Op::SliceXz { input, x, z });
                    }
                }
                let input = self.compile_node(input);
                let coordinate = *coordinate;
                self.push(match axis {
                    Axis::X => Op::SliceX {
                        input,
                        x: coordinate,
                    },
                    Axis::Y => Op::SliceY {
                        input,
                        y: coordinate,
                    },
                    Axis::Z => Op::SliceZ {
                        input,
                        z: coordinate,
                    },
                })
            }
            Kind::FindTopSurface {
                density,
                upper_bound,
                lower_bound,
                cell_height,
            } => {
                let density = self.compile_node(density);
                let upper_bound = self.compile_node(upper_bound);
                let input = self.push(Op::FindTopSurface {
                    density,
                    upper_bound,
                    lower_bound: *lower_bound,
                    cell_height: *cell_height,
                });
                self.push(Op::SliceY { input, y: 0 })
            }
            Kind::BlendedNoise {
                xz_scale,
                y_scale,
                xz_factor,
                y_factor,
                smear_scale_multiplier,
            } => {
                // `BlendedNoise.compileSampler(random)`.
                let mut random = self.ctx.create_random(BLENDED_NOISE_SEED);
                let fbm =
                    blended_fbm_set(y_scale.0, y_factor.0, smear_scale_multiplier.0, &mut random);
                let xz_multiplier = BLENDED_BASE_SCALE * xz_scale.0;
                let y_multiplier = BLENDED_BASE_SCALE * y_scale.0;
                let min_limit = self.noise_index(Arc::new(fbm.min_limit));
                let max_limit = self.noise_index(Arc::new(fbm.max_limit));
                let main = self.noise_index(Arc::new(fbm.main));
                let min_limit = self.push(Op::Noise {
                    noise: min_limit,
                    xz_scale: xz_multiplier,
                    y_scale: y_multiplier,
                });
                let max_limit = self.push(Op::Noise {
                    noise: max_limit,
                    xz_scale: xz_multiplier,
                    y_scale: y_multiplier,
                });
                let main = self.push(Op::Noise {
                    noise: main,
                    xz_scale: xz_multiplier / xz_factor.0,
                    y_scale: y_multiplier / y_factor.0,
                });
                let shifted = self.push(Op::ConstAdd {
                    input: main,
                    value: 0.5,
                });
                let choice = self.push(Op::Clamp {
                    input: shifted,
                    min: 0.0,
                    max: 1.0,
                });
                self.push(Op::Lerp {
                    alpha: choice,
                    first: min_limit,
                    second: max_limit,
                })
            }
            // A reference that survived rewriting (its value did not change): Vanilla's
            // `HolderHolder.compileSampler` compiles the value.
            Kind::Reference(holder) => self.compile_node(&holder.value),
        }
    }

    /// `BinaryFunction.compileSampler`.
    fn lower_binary(
        &mut self,
        op: BinaryOp,
        left_fn: &DensityFunction,
        right_fn: &DensityFunction,
    ) -> OpIndex {
        let left = self.compile_node(left_fn);
        let right = self.compile_node(right_fn);
        let left_const = left_fn.as_constant();
        let right_const = right_fn.as_constant();
        let op = match op {
            BinaryOp::Add => match (left_const, right_const) {
                (Some(value), _) => Op::ConstAdd {
                    input: right,
                    value,
                },
                (_, Some(value)) => Op::ConstAdd { input: left, value },
                _ => Op::Add { left, right },
            },
            BinaryOp::Sub => match (left_const, right_const) {
                (Some(value), _) => Op::ConstSub {
                    value,
                    input: right,
                },
                (_, Some(value)) => Op::ConstAdd {
                    input: left,
                    value: -value,
                },
                _ => Op::Sub { left, right },
            },
            BinaryOp::Mul => match (left_const, right_const) {
                (Some(value), _) => Op::ConstMul {
                    input: right,
                    value,
                },
                (_, Some(value)) => Op::ConstMul { input: left, value },
                _ => Op::Mul { left, right },
            },
            BinaryOp::Div => match (left_const, right_const) {
                (Some(value), _) => Op::ConstDiv {
                    value,
                    input: right,
                },
                (_, Some(value)) => Op::ConstMul {
                    input: left,
                    value: 1.0 / value,
                },
                _ => Op::Div { left, right },
            },
            BinaryOp::Min => {
                let (left_range, right_range) = (left_fn.range(), right_fn.range());
                if left_range.max() < right_range.min() {
                    return left;
                }
                if right_range.max() < left_range.min() {
                    return right;
                }
                match (left_const, right_const) {
                    (Some(value), _) => Op::ConstMin {
                        input: right,
                        value,
                    },
                    (_, Some(value)) => Op::ConstMin { input: left, value },
                    _ => Op::Min {
                        left,
                        right,
                        right_min: right_range.min(),
                    },
                }
            }
            BinaryOp::Max => {
                let (left_range, right_range) = (left_fn.range(), right_fn.range());
                if left_range.min() > right_range.max() {
                    return left;
                }
                if right_range.min() > left_range.max() {
                    return right;
                }
                match (left_const, right_const) {
                    (Some(value), _) => Op::ConstMax {
                        input: right,
                        value,
                    },
                    (_, Some(value)) => Op::ConstMax { input: left, value },
                    _ => Op::Max {
                        left,
                        right,
                        right_max: right_range.max(),
                    },
                }
            }
        };
        self.push(op)
    }

    /// `PowFunction.compileConstExponent(base, exponent)`.
    fn lower_const_exponent(&mut self, base: OpIndex, exponent: f32) -> OpIndex {
        let abs = exponent.abs();
        let special = if abs == 0.5 {
            self.push(Op::Sqrt { input: base })
        } else if abs == 1.0 {
            base
        } else if abs == 2.0 {
            self.push(Op::Square { input: base })
        } else if abs == 3.0 {
            self.push(Op::Cube { input: base })
        } else {
            return self.push(Op::PowConstExponent {
                base,
                exponent: f64::from(exponent),
            });
        };
        if exponent >= 0.0 {
            special
        } else {
            self.push(Op::Reciprocal { input: special })
        }
    }

    /// `SplineFunction.Sampler`: coordinates deduplicated by function, then flat tables.
    fn lower_spline(&mut self, spline: &Spline) -> OpIndex {
        let mut slots: FxHashMap<DensityFunction, u32> = FxHashMap::default();
        let mut table = SplineProgram {
            coordinates: Vec::new(),
            root: SplineValue::Constant(0.0),
            multipoints: Vec::new(),
            points: Vec::new(),
        };
        table.root = self.flatten_spline(spline, &mut slots, &mut table);
        let index = self.program.splines.len() as u32;
        self.program.splines.push(table);
        self.push(Op::Spline { spline: index })
    }

    fn flatten_spline(
        &mut self,
        spline: &Spline,
        slots: &mut FxHashMap<DensityFunction, u32>,
        table: &mut SplineProgram,
    ) -> SplineValue {
        let m = match spline {
            Spline::Constant(F32(value)) => return SplineValue::Constant(*value),
            Spline::Multipoint(m) => m,
        };
        let coordinate = match slots.get(&m.coordinate) {
            Some(&slot) => slot,
            None => {
                let slot = table.coordinates.len() as u32;
                let op = self.compile_node(&m.coordinate);
                table.coordinates.push(op);
                slots.insert(m.coordinate.clone(), slot);
                slot
            }
        };
        let values: Vec<SplineValue> = m
            .values
            .iter()
            .map(|v| self.flatten_spline(v, slots, table))
            .collect();
        let first_point = table.points.len() as u32;
        for (i, value) in values.into_iter().enumerate() {
            table.points.push(SplinePoint {
                location: m.locations[i].0,
                derivative: m.derivatives[i].0,
                value,
            });
        }
        let index = table.multipoints.len() as u32;
        table.multipoints.push(SplineMultipoint {
            coordinate,
            first_point,
            point_count: m.locations.len() as u32,
        });
        SplineValue::Multipoint(index)
    }
}

/// `DfRewriteRule.SliceUniformAxes(parentDomainAxes)`: slices away axes a function does not
/// depend on but its parent does, so the function is sampled once per remaining column.
fn slice_uniform_axes(parent_axes: u8, function: &DensityFunction) -> DensityFunction {
    if matches!(function.kind(), Kind::Constant(_) | Kind::Gradient { .. }) {
        return function.clone();
    }
    let axes = function.domain_axes();
    if parent_axes == axes {
        return function.rewrite_children(&mut |child| slice_uniform_axes(parent_axes, child));
    }
    let rewritten = function.rewrite_children(&mut |child| slice_uniform_axes(axes, child));
    remove_axes(rewritten, parent_axes & !axes)
}

/// `SliceUniformAxes.removeAxes(function, axes)`.
fn remove_axes(mut function: DensityFunction, axes: u8) -> DensityFunction {
    let filtered = axes & !existing_removed_axes(&function);
    for (bit, axis) in [(AXIS_X, Axis::X), (AXIS_Z, Axis::Z), (AXIS_Y, Axis::Y)] {
        if filtered & bit != 0 {
            function = DensityFunction::new(Kind::Slice {
                axis,
                coordinate: 0,
                input: function,
            });
        }
    }
    function
}

/// `SliceUniformAxes.getExistingRemovedAxes(function)`.
fn existing_removed_axes(mut function: &DensityFunction) -> u8 {
    let mut axes = 0;
    while let Kind::Slice { axis, input, .. } = function.kind() {
        axes |= axis.bit();
        function = input;
    }
    axes
}
