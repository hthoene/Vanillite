//! `DensityFunction.CODEC` and the codecs of every density function type
//! (`DensityFunctions.bootstrap`), reading `worldgen/density_function` JSON.

use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};
use serde_json::{Map, Value};
use vanillite_datapack::LoadedPacks;

use super::function::{
    Axis, BinaryOp, DensityFunction, DistanceMetric, F32, F64, Holder, Kind, NoiseRef, RoundMode,
    ShiftKind, SimpleKind, TilingMode, UnaryOp,
};
use super::spline::{Multipoint, Spline};
use crate::Error;
use crate::noise::NormalNoise;

/// `DensityFunctions.MAX_REASONABLE_NOISE_VALUE` (`NOISE_VALUE_CODEC` bounds).
const MAX_REASONABLE_NOISE_VALUE: f32 = 1_000_000.0;
/// `DimensionType.MIN_Y * 2` and `MAX_Y * 2` (`find_top_surface.lower_bound`).
const LOWER_BOUND_RANGE: (i32, i32) = (-4064, 4062);

const DENSITY_FUNCTION_REGISTRY: &str = "minecraft:worldgen/density_function";
const NOISE_REGISTRY: &str = "minecraft:worldgen/noise";

/// `Identifier.parse`: ids without a namespace are in `minecraft`.
fn normalize_id(id: &str) -> String {
    if id.contains(':') {
        id.to_owned()
    } else {
        format!("minecraft:{id}")
    }
}

fn err(message: impl Into<String>) -> Error {
    Error::new(format!("density function: {}", message.into()))
}

fn field<'v>(map: &'v Map<String, Value>, name: &str) -> Result<&'v Value, Error> {
    map.get(name)
        .ok_or_else(|| err(format!("missing field {name}")))
}

fn f64_field(map: &Map<String, Value>, name: &str) -> Result<f64, Error> {
    field(map, name)?
        .as_f64()
        .ok_or_else(|| err(format!("{name} is not a number")))
}

/// A `Codec.FLOAT` field.
fn f32_field(map: &Map<String, Value>, name: &str) -> Result<f32, Error> {
    Ok(f64_field(map, name)? as f32)
}

/// A `NOISE_VALUE_CODEC` field.
fn noise_value(value: &Value, name: &str) -> Result<f32, Error> {
    let value = value
        .as_f64()
        .ok_or_else(|| err(format!("{name} is not a number")))? as f32;
    if (-MAX_REASONABLE_NOISE_VALUE..=MAX_REASONABLE_NOISE_VALUE).contains(&value) {
        Ok(value)
    } else {
        Err(err(format!("{name} {value} is outside [-1e6, 1e6]")))
    }
}

fn i32_field(map: &Map<String, Value>, name: &str) -> Result<i32, Error> {
    field(map, name)?
        .as_i64()
        .and_then(|v| i32::try_from(v).ok())
        .ok_or_else(|| err(format!("{name} is not an int")))
}

fn positive_i32_field(map: &Map<String, Value>, name: &str) -> Result<i32, Error> {
    let value = i32_field(map, name)?;
    if value > 0 {
        Ok(value)
    } else {
        Err(err(format!("{name} must be positive")))
    }
}

fn str_field<'v>(map: &'v Map<String, Value>, name: &str) -> Result<&'v str, Error> {
    field(map, name)?
        .as_str()
        .ok_or_else(|| err(format!("{name} is not a string")))
}

fn axis(name: &str) -> Result<Axis, Error> {
    match name {
        "x" => Ok(Axis::X),
        "y" => Ok(Axis::Y),
        "z" => Ok(Axis::Z),
        _ => Err(err(format!("unknown axis {name}"))),
    }
}

/// Parses density functions, resolving registry references against loaded data packs.
#[derive(Debug)]
pub struct DensityParser<'a> {
    packs: &'a LoadedPacks,
    /// Registry entries already parsed, as references (one holder per id).
    references: FxHashMap<String, DensityFunction>,
    /// Entries being parsed, to reject reference cycles.
    in_progress: FxHashSet<String>,
    noises: FxHashMap<String, NoiseRef>,
}

impl<'a> DensityParser<'a> {
    /// A parser over the `worldgen/density_function` and `worldgen/noise` registries of `packs`.
    pub fn new(packs: &'a LoadedPacks) -> Self {
        Self {
            packs,
            references: FxHashMap::default(),
            in_progress: FxHashSet::default(),
            noises: FxHashMap::default(),
        }
    }

    /// Parses an inline density function or a reference string (`DensityFunction.CODEC`).
    pub fn parse(&mut self, json: &Value) -> Result<DensityFunction, Error> {
        match json {
            Value::String(id) => self.reference(id),
            Value::Number(_) => Ok(DensityFunction::constant(noise_value(json, "constant")?)),
            Value::Object(map) => self.parse_object(map),
            _ => Err(err(format!("expected a function, got {json}"))),
        }
    }

    /// The registry entry `id` as a reference (`HolderHolder`).
    pub fn reference(&mut self, id: &str) -> Result<DensityFunction, Error> {
        let id = normalize_id(id);
        if let Some(function) = self.references.get(&id) {
            return Ok(function.clone());
        }
        if !self.in_progress.insert(id.clone()) {
            return Err(err(format!("reference cycle through {id}")));
        }
        let entry = self
            .packs
            .registry(DENSITY_FUNCTION_REGISTRY)
            .and_then(|r| r.get(&id))
            .ok_or_else(|| err(format!("unknown density function {id}")))?;
        let json: Value =
            serde_json::from_str(entry.json.get()).map_err(|e| err(format!("{id}: {e}")))?;
        let value = self.parse(&json);
        self.in_progress.remove(&id);
        let function = DensityFunction::new(Kind::Reference(Arc::new(Holder {
            id: id.as_str().into(),
            value: value?,
        })));
        self.references.insert(id, function.clone());
        Ok(function)
    }

    /// A `worldgen/noise` holder (`NormalNoise.CODEC`); only registry references are supported,
    /// since Vanilla can only instantiate noises with a registry key.
    fn noise(&mut self, json: &Value) -> Result<NoiseRef, Error> {
        let id = normalize_id(
            json.as_str()
                .ok_or_else(|| err("inline noise parameters are not supported"))?,
        );
        if let Some(noise) = self.noises.get(&id) {
            return Ok(noise.clone());
        }
        let entry = self
            .packs
            .registry(NOISE_REGISTRY)
            .and_then(|r| r.get(&id))
            .ok_or_else(|| err(format!("unknown noise {id}")))?;
        let json: Value =
            serde_json::from_str(entry.json.get()).map_err(|e| err(format!("{id}: {e}")))?;
        let noise = NoiseRef {
            id: id.as_str().into(),
            params: Arc::new(NormalNoise::from_json(&json)?),
        };
        self.noises.insert(id, noise.clone());
        Ok(noise)
    }

    fn function_field(
        &mut self,
        map: &Map<String, Value>,
        name: &str,
    ) -> Result<DensityFunction, Error> {
        self.parse(field(map, name)?)
    }

    /// An `optionalFieldOf(name, default)` function field.
    fn optional_function(
        &mut self,
        map: &Map<String, Value>,
        name: &str,
        default: f32,
    ) -> Result<DensityFunction, Error> {
        match map.get(name) {
            Some(json) => self.parse(json),
            None => Ok(DensityFunction::constant(default)),
        }
    }

    fn parse_object(&mut self, map: &Map<String, Value>) -> Result<DensityFunction, Error> {
        let ty = str_field(map, "type")?;
        let ty = ty.strip_prefix("minecraft:").unwrap_or(ty);
        let unary_op = match ty {
            "abs" => Some(UnaryOp::Abs),
            "square" => Some(UnaryOp::Square),
            "cube" => Some(UnaryOp::Cube),
            "sqrt" => Some(UnaryOp::Sqrt),
            "half_negative" => Some(UnaryOp::HalfNegative),
            "quarter_negative" => Some(UnaryOp::QuarterNegative),
            "reciprocal" => Some(UnaryOp::Reciprocal),
            "negate" => Some(UnaryOp::Negate),
            "squeeze" => Some(UnaryOp::Squeeze),
            "log" => Some(UnaryOp::Log),
            "sign" => Some(UnaryOp::Sign),
            _ => None,
        };
        if let Some(op) = unary_op {
            let input = self.function_field(map, "input")?;
            return Ok(DensityFunction::new(Kind::Unary(op, input)));
        }
        let binary_op = match ty {
            "add" => Some(BinaryOp::Add),
            "sub" => Some(BinaryOp::Sub),
            "mul" => Some(BinaryOp::Mul),
            "div" => Some(BinaryOp::Div),
            "min" => Some(BinaryOp::Min),
            "max" => Some(BinaryOp::Max),
            _ => None,
        };
        if let Some(op) = binary_op {
            let left = self.function_field(map, "left")?;
            let right = self.function_field(map, "right")?;
            return Ok(DensityFunction::new(Kind::Binary(op, left, right)));
        }
        let round_mode = match ty {
            "floor" => Some(RoundMode::Floor),
            "round" => Some(RoundMode::Round),
            "ceil" => Some(RoundMode::Ceil),
            "truncate" => Some(RoundMode::Truncate),
            _ => None,
        };
        if let Some(mode) = round_mode {
            let input = self.function_field(map, "input")?;
            let multiple = self.optional_function(map, "multiple", 1.0)?;
            return Ok(DensityFunction::new(Kind::Round {
                mode,
                input,
                multiple,
            }));
        }
        let kind = match ty {
            "constant" => Kind::Constant(F32(noise_value(field(map, "value")?, "value")?)),
            "blend_alpha" => Kind::Simple(SimpleKind::BlendAlpha),
            "blend_offset" => Kind::Simple(SimpleKind::BlendOffset),
            "beardifier" => Kind::Simple(SimpleKind::Beardifier),
            "noise" => Kind::Noise {
                noise: self.noise(field(map, "noise")?)?,
                xz_scale: F64(f64_field(map, "xz_scale")?),
                y_scale: F64(f64_field(map, "y_scale")?),
                shift_x: self.optional_function(map, "shift_x", 0.0)?,
                shift_y: self.optional_function(map, "shift_y", 0.0)?,
                shift_z: self.optional_function(map, "shift_z", 0.0)?,
            },
            "end_outer_islands" => Kind::EndIslands,
            "distance_to_point" => {
                let point = field(map, "point")?
                    .as_array()
                    .filter(|a| a.len() == 3)
                    .ok_or_else(|| err("point must be a list of 3 ints"))?;
                let mut coords = [0i32; 3];
                for (c, v) in coords.iter_mut().zip(point) {
                    *c = v
                        .as_i64()
                        .and_then(|v| i32::try_from(v).ok())
                        .ok_or_else(|| err("point must be a list of 3 ints"))?;
                }
                let metric = match str_field(map, "metric")? {
                    "euclidean" => DistanceMetric::Euclidean,
                    "euclidean_squared" => DistanceMetric::EuclideanSquared,
                    "manhattan" => DistanceMetric::Manhattan,
                    "chebyshev" => DistanceMetric::Chebyshev,
                    other => return Err(err(format!("unknown distance metric {other}"))),
                };
                Kind::DistanceToPoint {
                    point: coords,
                    metric,
                }
            }
            "gradient" => {
                let tiling = match map.get("tiling").map(|t| t.as_str()) {
                    None => TilingMode::ClampToEdge,
                    Some(Some("clamp_to_edge")) => TilingMode::ClampToEdge,
                    Some(Some("repeat")) => TilingMode::Repeat,
                    Some(Some("mirrored_repeat")) => TilingMode::MirroredRepeat,
                    Some(other) => return Err(err(format!("unknown tiling {other:?}"))),
                };
                let from_coordinate = i32_field(map, "from_coordinate")?;
                let to_coordinate = i32_field(map, "to_coordinate")?;
                if from_coordinate == to_coordinate {
                    return Err(err("from_coordinate cannot be equal to to_coordinate"));
                }
                Kind::Gradient {
                    axis: axis(str_field(map, "axis")?)?,
                    tiling,
                    from_coordinate,
                    to_coordinate,
                    from_value: F32(noise_value(field(map, "from_value")?, "from_value")?),
                    to_value: F32(noise_value(field(map, "to_value")?, "to_value")?),
                }
            }
            "shift" | "shift_a" | "shift_b" => Kind::Shift {
                kind: match ty {
                    "shift" => ShiftKind::Shift,
                    "shift_a" => ShiftKind::ShiftA,
                    _ => ShiftKind::ShiftB,
                },
                noise: self.noise(field(map, "noise")?)?,
            },
            "pow" => Kind::Pow {
                base: self.function_field(map, "base")?,
                exponent: self.function_field(map, "exponent")?,
            },
            "spline" => Kind::Spline(self.spline(field(map, "spline")?)?),
            "lerp" => Kind::Lerp {
                alpha: self.function_field(map, "alpha")?,
                first: self.function_field(map, "first")?,
                second: self.function_field(map, "second")?,
            },
            "clamp" => {
                let min = noise_value(field(map, "min")?, "min")?;
                let max = noise_value(field(map, "max")?, "max")?;
                if max < min {
                    return Err(err(format!(
                        "min ({min}) must be less than or equal to max ({max})"
                    )));
                }
                Kind::Clamp {
                    input: self.function_field(map, "input")?,
                    min: F32(min),
                    max: F32(max),
                }
            }
            "range_choice" => Kind::RangeChoice {
                input: self.function_field(map, "input")?,
                min_inclusive: F32(noise_value(field(map, "min_inclusive")?, "min_inclusive")?),
                max_exclusive: F32(noise_value(field(map, "max_exclusive")?, "max_exclusive")?),
                when_in_range: self.function_field(map, "when_in_range")?,
                when_out_of_range: self.function_field(map, "when_out_of_range")?,
            },
            "interval_select" => self.interval_select(map)?,
            "cache" => Kind::Cache(self.function_field(map, "input")?),
            "blend_density" => Kind::BlendDensity(self.function_field(map, "input")?),
            "interpolated" => Kind::Interpolated {
                input: self.function_field(map, "input")?,
                cell_size_xz: positive_i32_field(map, "cell_size_xz")?,
                cell_size_y: positive_i32_field(map, "cell_size_y")?,
            },
            "slice" => Kind::Slice {
                axis: axis(str_field(map, "axis")?)?,
                coordinate: i32_field(map, "coordinate")?,
                input: self.function_field(map, "input")?,
            },
            "find_top_surface" => {
                let lower_bound = i32_field(map, "lower_bound")?;
                if !(LOWER_BOUND_RANGE.0..=LOWER_BOUND_RANGE.1).contains(&lower_bound) {
                    return Err(err(format!("lower_bound {lower_bound} out of range")));
                }
                Kind::FindTopSurface {
                    density: self.function_field(map, "density")?,
                    upper_bound: self.function_field(map, "upper_bound")?,
                    lower_bound,
                    cell_height: positive_i32_field(map, "cell_height")?,
                }
            }
            "old_blended_noise" => {
                let scale = |name: &str| -> Result<F64, Error> {
                    let v = f64_field(map, name)?;
                    if (0.001..=1000.0).contains(&v) {
                        Ok(F64(v))
                    } else {
                        Err(err(format!("{name} {v} is outside [0.001, 1000]")))
                    }
                };
                let smear = f64_field(map, "smear_scale_multiplier")?;
                if !(1.0..=8.0).contains(&smear) {
                    return Err(err(format!(
                        "smear_scale_multiplier {smear} is outside [1, 8]"
                    )));
                }
                Kind::BlendedNoise {
                    xz_scale: scale("xz_scale")?,
                    y_scale: scale("y_scale")?,
                    xz_factor: scale("xz_factor")?,
                    y_factor: scale("y_factor")?,
                    smear_scale_multiplier: F64(smear),
                }
            }
            other => return Err(err(format!("unknown type {other}"))),
        };
        Ok(DensityFunction::new(kind))
    }

    /// `IntervalSelectFunction.CODEC` with its validation.
    fn interval_select(&mut self, map: &Map<String, Value>) -> Result<Kind, Error> {
        let input = self.function_field(map, "input")?;
        let thresholds = field(map, "thresholds")?
            .as_array()
            .ok_or_else(|| err("thresholds must be a list"))?
            .iter()
            .map(|t| noise_value(t, "threshold").map(F32))
            .collect::<Result<Box<[F32]>, Error>>()?;
        let functions = field(map, "functions")?
            .as_array()
            .ok_or_else(|| err("functions must be a list"))?
            .iter()
            .map(|f| self.parse(f))
            .collect::<Result<Box<[DensityFunction]>, Error>>()?;
        if functions.len() < 2 {
            return Err(err("interval_select needs at least 2 functions"));
        }
        if thresholds.len() != functions.len() - 1 {
            return Err(err(format!(
                "expected {} thresholds for {} functions, but got {}",
                functions.len() - 1,
                functions.len(),
                thresholds.len()
            )));
        }
        if thresholds
            .windows(2)
            .any(|w| w[0].0.total_cmp(&w[1].0).is_gt())
        {
            return Err(err(
                "threshold values must be ordered from smallest to largest",
            ));
        }
        Ok(Kind::IntervalSelect {
            input,
            thresholds,
            functions,
        })
    }

    /// `CubicSpline.codec(SplineFunction.Coordinate.CODEC)`.
    fn spline(&mut self, json: &Value) -> Result<Spline, Error> {
        if let Some(value) = json.as_f64() {
            return Ok(Spline::Constant(F32(value as f32)));
        }
        let map = json
            .as_object()
            .ok_or_else(|| err(format!("expected a spline, got {json}")))?;
        let coordinate = self.function_field(map, "coordinate")?;
        let points = field(map, "points")?
            .as_array()
            .filter(|p| !p.is_empty())
            .ok_or_else(|| err("spline points must be a non-empty list"))?;
        let mut locations = Vec::with_capacity(points.len());
        let mut values = Vec::with_capacity(points.len());
        let mut derivatives = Vec::with_capacity(points.len());
        for point in points {
            let point = point
                .as_object()
                .ok_or_else(|| err("spline point must be an object"))?;
            locations.push(F32(f32_field(point, "location")?));
            values.push(self.spline(field(point, "value")?)?);
            derivatives.push(F32(f32_field(point, "derivative")?));
        }
        Ok(Spline::Multipoint(Arc::new(Multipoint {
            coordinate,
            locations: locations.into(),
            values: values.into(),
            derivatives: derivatives.into(),
        })))
    }
}
