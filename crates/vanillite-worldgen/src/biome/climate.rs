//! `Climate`: quantized climate parameters, target points, parameter lists and the climate
//! sampler of the multi-noise biome source.
//!
//! Climate values are floats quantized to `long`s (`value * 10000`, truncated); every comparison
//! and distance works on the quantized values with Java's wrapping `long` arithmetic.

use serde_json::Value;

use super::rtree::RTree;
use crate::Error;
use crate::density::{FnId, Program, SamplerContext};

/// `Climate.QUANTIZATION_FACTOR`.
const QUANTIZATION_FACTOR: f32 = 10000.0;

/// `Climate.quantizeCoord(coord)`: `(long)(coord * 10000.0F)` (saturating, NaN gives 0).
#[inline]
pub fn quantize_coord(coord: f32) -> i64 {
    (coord * QUANTIZATION_FACTOR) as i64
}

/// `Climate.unquantizeCoord(coord)`.
#[inline]
pub fn unquantize_coord(coord: i64) -> f32 {
    coord as f32 / QUANTIZATION_FACTOR
}

/// `Climate.target(temperature, humidity, continentalness, erosion, depth, weirdness)`.
#[inline]
pub fn target(
    temperature: f32,
    humidity: f32,
    continentalness: f32,
    erosion: f32,
    depth: f32,
    weirdness: f32,
) -> TargetPoint {
    TargetPoint {
        temperature: quantize_coord(temperature),
        humidity: quantize_coord(humidity),
        continentalness: quantize_coord(continentalness),
        erosion: quantize_coord(erosion),
        depth: quantize_coord(depth),
        weirdness: quantize_coord(weirdness),
    }
}

/// `Climate.Parameter`: a closed interval of quantized values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Parameter {
    pub min: i64,
    pub max: i64,
}

impl Parameter {
    /// `Parameter.point(value)`.
    pub fn point(value: f32) -> Self {
        let q = quantize_coord(value);
        Self { min: q, max: q }
    }

    /// `Parameter.span(min, max)`; fails if `min > max`.
    pub fn span(min: f32, max: f32) -> Result<Self, Error> {
        if min > max {
            return Err(Error::new(format!("min > max: {min} {max}")));
        }
        Ok(Self {
            min: quantize_coord(min),
            max: quantize_coord(max),
        })
    }

    /// `distance(target)`: how far `target` lies outside the interval (0 inside).
    #[inline]
    pub fn distance(&self, target: i64) -> i64 {
        let above = target.wrapping_sub(self.max);
        let below = self.min.wrapping_sub(target);
        if above > 0 { above } else { below.max(0) }
    }

    /// `span(other)`: the smallest interval containing both.
    #[inline]
    pub fn union(&self, other: &Self) -> Self {
        Self {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
        }
    }

    /// Center as the R-tree sorts by it: `(min + max) / 2` (truncating).
    #[inline]
    pub(crate) fn center(&self) -> i64 {
        self.min.wrapping_add(self.max) / 2
    }
}

/// `Climate.ParameterPoint`: the climate region of one biome entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ParameterPoint {
    pub temperature: Parameter,
    pub humidity: Parameter,
    pub continentalness: Parameter,
    pub erosion: Parameter,
    pub depth: Parameter,
    pub weirdness: Parameter,
    /// Quantized offset (always added as a distance).
    pub offset: i64,
}

impl ParameterPoint {
    /// `Climate.parameters(temperature, ..., offset)` with point parameters.
    #[allow(clippy::too_many_arguments)]
    pub fn of_points(
        temperature: f32,
        humidity: f32,
        continentalness: f32,
        erosion: f32,
        depth: f32,
        weirdness: f32,
        offset: f32,
    ) -> Self {
        Self {
            temperature: Parameter::point(temperature),
            humidity: Parameter::point(humidity),
            continentalness: Parameter::point(continentalness),
            erosion: Parameter::point(erosion),
            depth: Parameter::point(depth),
            weirdness: Parameter::point(weirdness),
            offset: quantize_coord(offset),
        }
    }

    /// `fitness(target)`: the squared distance to `target` (the brute-force search metric).
    pub fn fitness(&self, target: &TargetPoint) -> i64 {
        let space = self.parameter_space();
        let target = target.to_parameter_array();
        distance(&space, &target)
    }

    /// `parameterSpace()`: the six parameters and the offset as a point parameter.
    pub fn parameter_space(&self) -> [Parameter; 7] {
        [
            self.temperature,
            self.humidity,
            self.continentalness,
            self.erosion,
            self.depth,
            self.weirdness,
            Parameter {
                min: self.offset,
                max: self.offset,
            },
        ]
    }

    /// Parses a `ParameterPoint.CODEC` object as Vanilla's codec does (floats are quantized,
    /// parameters lie in `[-2, 2]`, the offset in `[0, 1]`).
    pub fn from_json(json: &Value) -> Result<Self, Error> {
        let param = |name: &str| -> Result<Parameter, Error> {
            let v = json
                .get(name)
                .ok_or_else(|| Error::new(format!("climate parameter {name} missing")))?;
            let (min, max) = parameter_bounds(v)
                .ok_or_else(|| Error::new(format!("climate parameter {name}: invalid {v}")))?;
            if !(-2.0..=2.0).contains(&min) || !(-2.0..=2.0).contains(&max) {
                return Err(Error::new(format!(
                    "climate parameter {name} outside [-2, 2]: {v}"
                )));
            }
            if min > max {
                return Err(Error::new(format!(
                    "Cannon construct interval, min > max ({min} > {max})"
                )));
            }
            Ok(Parameter {
                min: quantize_coord(min),
                max: quantize_coord(max),
            })
        };
        let offset = json
            .get("offset")
            .and_then(json_f32)
            .filter(|o| (0.0..=1.0).contains(o))
            .ok_or_else(|| Error::new("climate offset missing or outside [0, 1]"))?;
        Ok(Self {
            temperature: param("temperature")?,
            humidity: param("humidity")?,
            continentalness: param("continentalness")?,
            erosion: param("erosion")?,
            depth: param("depth")?,
            weirdness: param("weirdness")?,
            offset: quantize_coord(offset),
        })
    }
}

/// A JSON number as Java's `Codec.FLOAT` reads it.
pub(crate) fn json_f32(v: &Value) -> Option<f32> {
    v.as_f64().map(|f| f as f32)
}

/// `ExtraCodecs.intervalCodec` bounds: a number, `[min, max]` or `{"min", "max"}`.
pub(crate) fn parameter_bounds(v: &Value) -> Option<(f32, f32)> {
    match v {
        Value::Number(_) => json_f32(v).map(|f| (f, f)),
        Value::Array(a) if a.len() == 2 => Some((json_f32(&a[0])?, json_f32(&a[1])?)),
        Value::Object(o) => Some((json_f32(o.get("min")?)?, json_f32(o.get("max")?)?)),
        _ => None,
    }
}

/// `Node.distance(target)` / `ParameterPoint.fitness`: sum of squared per-axis distances.
#[inline]
pub(crate) fn distance(space: &[Parameter; 7], target: &[i64; 7]) -> i64 {
    let mut distance = 0i64;
    for i in 0..7 {
        let d = space[i].distance(target[i]);
        distance = distance.wrapping_add(d.wrapping_mul(d));
    }
    distance
}

/// `Climate.TargetPoint`: a sampled, quantized climate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct TargetPoint {
    pub temperature: i64,
    pub humidity: i64,
    pub continentalness: i64,
    pub erosion: i64,
    pub depth: i64,
    pub weirdness: i64,
}

impl TargetPoint {
    /// `toParameterArray()`: the six values and a zero offset.
    #[inline]
    pub fn to_parameter_array(&self) -> [i64; 7] {
        [
            self.temperature,
            self.humidity,
            self.continentalness,
            self.erosion,
            self.depth,
            self.weirdness,
            0,
        ]
    }
}

/// The last search result of a [`ParameterList`] (Vanilla's `RTree.lastResult`).
///
/// The R-tree search starts from the previous result and only replaces it by a strictly closer
/// entry, so when several entries are equally close the previous result wins. Vanilla keeps it in
/// a thread-local per tree, which makes ties depend on what the worker thread searched before;
/// here every caller owns its cache (one per resolver), so results are deterministic and equal to
/// Vanilla's for the same sequence of searches. See [`ParameterList::find_value`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SearchCache(pub(crate) Option<u32>);

/// `Climate.ParameterList`: biome entries with their climate regions and the R-tree over them.
#[derive(Debug)]
pub struct ParameterList<T> {
    values: Vec<(ParameterPoint, T)>,
    tree: RTree,
}

impl<T> ParameterList<T> {
    /// `new ParameterList(values)` with Vanilla's 19 children per node. Fails if `values` is empty.
    pub fn new(values: Vec<(ParameterPoint, T)>) -> Result<Self, Error> {
        Self::with_children_per_node(values, 19)
    }

    /// The list with an R-tree of `children_per_node` children (`rebuildWithChildrenPerNode`).
    pub fn with_children_per_node(
        values: Vec<(ParameterPoint, T)>,
        children_per_node: usize,
    ) -> Result<Self, Error> {
        if values.is_empty() {
            return Err(Error::new(
                "Need at least one value to build the search tree.",
            ));
        }
        let spaces: Vec<[Parameter; 7]> = values.iter().map(|v| v.0.parameter_space()).collect();
        let tree = RTree::build(&spaces, children_per_node);
        Ok(Self { values, tree })
    }

    /// `values()`.
    pub fn values(&self) -> &[(ParameterPoint, T)] {
        &self.values
    }

    /// `findValue(target)`: the value of the closest entry (R-tree search).
    ///
    /// `cache` holds the previous result of the caller (see [`SearchCache`]); on exact ties it
    /// decides which of the equally close entries is returned, exactly as Vanilla's thread-local
    /// does. With an empty cache the result is the first closest leaf in tree order. Use one cache
    /// per list.
    #[inline]
    pub fn find_value(&self, target: &TargetPoint, cache: &mut SearchCache) -> &T {
        let index = self
            .tree
            .search(&target.to_parameter_array(), cache.0.map(|i| i as usize));
        cache.0 = Some(index as u32);
        &self.values[index].1
    }

    /// `findValueBruteForce(target)`: the first entry in list order with the lowest fitness.
    pub fn find_value_brute_force(&self, target: &TargetPoint) -> &T {
        let mut best = &self.values[0];
        let mut best_fitness = best.0.fitness(target);
        for value in &self.values[1..] {
            let fitness = value.0.fitness(target);
            if fitness < best_fitness {
                best_fitness = fitness;
                best = value;
            }
        }
        &best.1
    }
}

/// `Climate.Sampler`: the six climate density functions of the noise router.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClimateSampler {
    /// `temperature`.
    pub temperature: FnId,
    /// `humidity` (the router's `vegetation`).
    pub humidity: FnId,
    /// `continentalness` (the router's `continents`).
    pub continentalness: FnId,
    /// `erosion`.
    pub erosion: FnId,
    /// `depth`.
    pub depth: FnId,
    /// `weirdness` (the router's `ridges`).
    pub weirdness: FnId,
}

impl ClimateSampler {
    /// `sample(quartX, quartY, quartZ)`: the target at the quart's corner block.
    pub fn sample(
        &self,
        program: &Program,
        ctx: &mut SamplerContext,
        quart_x: i32,
        quart_y: i32,
        quart_z: i32,
    ) -> TargetPoint {
        let (x, y, z) = (quart_x << 2, quart_y << 2, quart_z << 2);
        target(
            program.sample_value(ctx, self.temperature, x, y, z),
            program.sample_value(ctx, self.humidity, x, y, z),
            program.sample_value(ctx, self.continentalness, x, y, z),
            program.sample_value(ctx, self.erosion, x, y, z),
            program.sample_value(ctx, self.depth, x, y, z),
            program.sample_value(ctx, self.weirdness, x, y, z),
        )
    }

    /// The six functions in `Climate.target` argument order.
    pub fn functions(&self) -> [FnId; 6] {
        [
            self.temperature,
            self.humidity,
            self.continentalness,
            self.erosion,
            self.depth,
            self.weirdness,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantize_saturates_like_java() {
        assert_eq!(quantize_coord(f32::NAN), 0);
        assert_eq!(quantize_coord(f32::INFINITY), i64::MAX);
        assert_eq!(quantize_coord(-1.0e30), i64::MIN);
        assert_eq!(quantize_coord(-0.455), -4550);
    }

    #[test]
    fn parameter_distance() {
        let p = Parameter { min: -10, max: 20 };
        assert_eq!(p.distance(0), 0);
        assert_eq!(p.distance(25), 5);
        assert_eq!(p.distance(-12), 2);
    }
}
