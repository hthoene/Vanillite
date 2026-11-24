//! Value and height providers as carvers (and later features) read them from data packs.
//!
//! Ports `net.minecraft.util.valueproviders` (`IntProvider`, `FloatProvider` and their
//! implementations), `net.minecraft.util.random.WeightedList` (selection only),
//! `net.minecraft.world.level.levelgen.heightproviders` (`HeightProvider`), `VerticalAnchor` and
//! `WorldGenerationContext`.
//!
//! Parsing follows the codecs: a bare number is a constant (`IntProviders.CODEC`,
//! `FloatProviders.CODEC`), a bare vertical anchor is a constant height (`HeightProvider.CODEC`),
//! everything else dispatches on `type`.

use serde_json::Value;
use vanillite_base::math::{clamp_f, clamp_i};
use vanillite_base::random::RandomSource;

use crate::Error;

/// `WorldGenerationContext`: the vertical range providers resolve anchors against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorldGenerationContext {
    min_y: i32,
    height: i32,
    sea_level: i32,
}

impl WorldGenerationContext {
    /// A context with the given generation range and sea level.
    pub const fn new(min_gen_y: i32, gen_depth: i32, sea_level: i32) -> Self {
        Self {
            min_y: min_gen_y,
            height: gen_depth,
            sea_level,
        }
    }

    /// `new WorldGenerationContext(generator, heightAccessor)`: the generator's range clamped to
    /// the level's (`max` of the minimum Y, `min` of the heights).
    pub fn for_generator(
        generator_min_y: i32,
        generator_gen_depth: i32,
        sea_level: i32,
        level_min_y: i32,
        level_height: i32,
    ) -> Self {
        Self::new(
            level_min_y.max(generator_min_y),
            level_height.min(generator_gen_depth),
            sea_level,
        )
    }

    /// `getMinGenY()`.
    pub const fn min_gen_y(&self) -> i32 {
        self.min_y
    }

    /// `getGenDepth()`.
    pub const fn gen_depth(&self) -> i32 {
        self.height
    }

    /// `seaLevel()`.
    pub const fn sea_level(&self) -> i32 {
        self.sea_level
    }
}

/// `VerticalAnchor`: a Y coordinate relative to the generation range or the sea level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerticalAnchor {
    /// `{"absolute": y}`.
    Absolute(i32),
    /// `{"above_bottom": offset}`.
    AboveBottom(i32),
    /// `{"below_top": offset}`.
    BelowTop(i32),
    /// `{"relative_to_sea_level": offset}`.
    RelativeToSeaLevel(i32),
}

/// `DimensionType.MIN_Y`.
const MIN_Y: i32 = -2032;
/// `DimensionType.MAX_Y`.
const MAX_Y: i32 = 2031;

impl VerticalAnchor {
    /// Parses `VerticalAnchor.CODEC`: an object with exactly one of the four keys.
    pub fn from_json(json: &Value) -> Result<Self, Error> {
        let map = json
            .as_object()
            .ok_or_else(|| Error::new(format!("vertical anchor: not an object: {json}")))?;
        let mut found = None;
        for (key, make) in [
            ("absolute", Self::Absolute as fn(i32) -> Self),
            ("above_bottom", Self::AboveBottom),
            ("below_top", Self::BelowTop),
            ("relative_to_sea_level", Self::RelativeToSeaLevel),
        ] {
            if let Some(value) = map.get(key) {
                if found.is_some() {
                    return Err(Error::new(format!("vertical anchor: ambiguous {json}")));
                }
                let v = int_value(value, key)?;
                if !(MIN_Y..=MAX_Y).contains(&v) {
                    return Err(Error::new(format!(
                        "vertical anchor: {key} out of range: {v}"
                    )));
                }
                found = Some(make(v));
            }
        }
        found.ok_or_else(|| Error::new(format!("vertical anchor: no anchor in {json}")))
    }

    /// `resolveY(context)`.
    pub const fn resolve_y(&self, context: &WorldGenerationContext) -> i32 {
        match *self {
            Self::Absolute(y) => y,
            Self::AboveBottom(offset) => context.min_gen_y().wrapping_add(offset),
            Self::BelowTop(offset) => context
                .gen_depth()
                .wrapping_sub(1)
                .wrapping_add(context.min_gen_y())
                .wrapping_sub(offset),
            Self::RelativeToSeaLevel(offset) => context.sea_level().wrapping_add(offset),
        }
    }
}

/// `WeightedList`: entries picked with probability proportional to their weight.
#[derive(Clone, Debug, PartialEq)]
pub struct WeightedList<T> {
    entries: Vec<(T, i32)>,
    total_weight: i32,
}

impl<T> WeightedList<T> {
    /// Parses `WeightedList.nonEmptyCodec(codec)`: `[{"data": …, "weight": n}, …]`.
    pub fn from_json(
        json: &Value,
        mut parse: impl FnMut(&Value) -> Result<T, Error>,
    ) -> Result<Self, Error> {
        let items = json
            .as_array()
            .ok_or_else(|| Error::new("weighted list: not an array"))?;
        let mut entries = Vec::with_capacity(items.len());
        let mut total: i64 = 0;
        for item in items {
            let data = item
                .get("data")
                .ok_or_else(|| Error::new("weighted list: entry without data"))?;
            let weight = int_value(field(item, "weight")?, "weight")?;
            if weight < 0 {
                return Err(Error::new("weighted list: weight should be >= 0"));
            }
            total += i64::from(weight);
            entries.push((parse(data)?, weight));
        }
        let total_weight = i32::try_from(total)
            .map_err(|_| Error::new("weighted list: sum of weights must be <= 2147483647"))?;
        if total_weight == 0 {
            return Err(Error::new("weighted list: must not be empty"));
        }
        Ok(Self {
            entries,
            total_weight,
        })
    }

    /// `getRandomOrThrow(random)`.
    pub fn get_random<R: RandomSource>(&self, random: &mut R) -> &T {
        let mut index = random.next_int_bounded(self.total_weight);
        for (value, weight) in &self.entries {
            index -= weight;
            if index < 0 {
                return value;
            }
        }
        unreachable!("index below the total weight")
    }

    /// The entries with their weights.
    pub fn entries(&self) -> &[(T, i32)] {
        &self.entries
    }
}

/// `IntProvider`.
#[derive(Clone, Debug, PartialEq)]
pub enum IntProvider {
    /// `ConstantInt`.
    Constant(i32),
    /// `UniformInt`.
    Uniform {
        min_inclusive: i32,
        max_inclusive: i32,
    },
    /// `BiasedToBottomInt`.
    BiasedToBottom {
        min_inclusive: i32,
        max_inclusive: i32,
    },
    /// `VeryBiasedToBottomInt`.
    VeryBiasedToBottom {
        min_inclusive: i32,
        max_inclusive: i32,
    },
    /// `ClampedInt`.
    Clamped {
        source: Box<IntProvider>,
        min_inclusive: i32,
        max_inclusive: i32,
    },
    /// `WeightedListInt`.
    WeightedList(WeightedList<IntProvider>),
    /// `ClampedNormalInt`.
    ClampedNormal {
        mean: f32,
        deviation: f32,
        min_inclusive: i32,
        max_inclusive: i32,
    },
    /// `TrapezoidInt`.
    Trapezoid {
        min_inclusive: i32,
        max_inclusive: i32,
        plateau: i32,
    },
}

impl IntProvider {
    /// Parses `IntProviders.CODEC`.
    pub fn from_json(json: &Value) -> Result<Self, Error> {
        if json.is_number() {
            return Ok(Self::Constant(int_value(json, "int provider")?));
        }
        let min_max = |min: &str, max: &str| -> Result<(i32, i32), Error> {
            let lo = int_field(json, min)?;
            let hi = int_field(json, max)?;
            if hi < lo {
                return Err(Error::new(format!(
                    "int provider: max must be at least min: [{lo}, {hi}]"
                )));
            }
            Ok((lo, hi))
        };
        let provider = match type_name(json)? {
            "constant" => Self::Constant(int_field(json, "value")?),
            "uniform" => {
                let (min_inclusive, max_inclusive) = min_max("min_inclusive", "max_inclusive")?;
                Self::Uniform {
                    min_inclusive,
                    max_inclusive,
                }
            }
            "biased_to_bottom" => {
                let (min_inclusive, max_inclusive) = min_max("min_inclusive", "max_inclusive")?;
                Self::BiasedToBottom {
                    min_inclusive,
                    max_inclusive,
                }
            }
            "very_biased_to_bottom" => {
                let (min_inclusive, max_inclusive) = min_max("min_inclusive", "max_inclusive")?;
                Self::VeryBiasedToBottom {
                    min_inclusive,
                    max_inclusive,
                }
            }
            "clamped" => {
                let (min_inclusive, max_inclusive) = min_max("min_inclusive", "max_inclusive")?;
                Self::Clamped {
                    source: Box::new(Self::from_json(field(json, "source")?)?),
                    min_inclusive,
                    max_inclusive,
                }
            }
            "weighted_list" => Self::WeightedList(WeightedList::from_json(
                field(json, "distribution")?,
                Self::from_json,
            )?),
            "clamped_normal" => {
                let (min_inclusive, max_inclusive) = min_max("min_inclusive", "max_inclusive")?;
                Self::ClampedNormal {
                    mean: float_field(json, "mean")?,
                    deviation: float_field(json, "deviation")?,
                    min_inclusive,
                    max_inclusive,
                }
            }
            "trapezoid" => {
                let (min_inclusive, max_inclusive) = min_max("min", "max")?;
                let plateau = int_field(json, "plateau")?;
                if plateau > max_inclusive.wrapping_sub(min_inclusive) {
                    return Err(Error::new(
                        "int provider: plateau can at most be the full span",
                    ));
                }
                Self::Trapezoid {
                    min_inclusive,
                    max_inclusive,
                    plateau,
                }
            }
            other => return Err(Error::new(format!("unknown int provider type {other}"))),
        };
        Ok(provider)
    }

    /// Parses `IntProviders.codec(min, max)`: the provider's range must lie within `min..=max`.
    pub fn from_json_in_range(json: &Value, min: i32, max: i32) -> Result<Self, Error> {
        let provider = Self::from_json(json)?;
        if provider.min_inclusive() < min || provider.max_inclusive() > max {
            return Err(Error::new(format!(
                "int provider out of range [{min}-{max}]: [{}-{}]",
                provider.min_inclusive(),
                provider.max_inclusive()
            )));
        }
        Ok(provider)
    }

    /// `sample(random)`.
    pub fn sample<R: RandomSource>(&self, random: &mut R) -> i32 {
        match self {
            Self::Constant(value) => *value,
            Self::Uniform {
                min_inclusive,
                max_inclusive,
            } => random_between_inclusive(random, *min_inclusive, *max_inclusive),
            Self::BiasedToBottom {
                min_inclusive,
                max_inclusive,
            } => {
                let span = max_inclusive.wrapping_sub(*min_inclusive).wrapping_add(1);
                let inner = random.next_int_bounded(span) + 1;
                min_inclusive.wrapping_add(random.next_int_bounded(inner))
            }
            Self::VeryBiasedToBottom {
                min_inclusive,
                max_inclusive,
            } => {
                let span = max_inclusive.wrapping_sub(*min_inclusive).wrapping_add(1);
                let a = random.next_int_bounded(span) + 1;
                let b = random.next_int_bounded(a) + 1;
                min_inclusive.wrapping_add(random.next_int_bounded(b))
            }
            Self::Clamped {
                source,
                min_inclusive,
                max_inclusive,
            } => clamp_i(source.sample(random), *min_inclusive, *max_inclusive),
            Self::WeightedList(list) => list.get_random(random).sample(random),
            Self::ClampedNormal {
                mean,
                deviation,
                min_inclusive,
                max_inclusive,
            } => {
                // `(int) Mth.clamp(Mth.normal(…), (float) min, (float) max)`.
                let value = normal(random, *mean, *deviation);
                clamp_f(value, *min_inclusive as f32, *max_inclusive as f32) as i32
            }
            Self::Trapezoid {
                min_inclusive,
                max_inclusive,
                plateau,
            } => {
                let (min, max, plateau) = (*min_inclusive, *max_inclusive, *plateau);
                if plateau == 0 && max == min.wrapping_neg() {
                    random.next_int_bounded(max + 1) - random.next_int_bounded(max + 1)
                } else {
                    let range = max.wrapping_sub(min);
                    if plateau == range {
                        random_between_inclusive(random, min, max)
                    } else {
                        let plateau_start = (range - plateau) / 2;
                        let plateau_end = range - plateau_start;
                        min.wrapping_add(random_between_inclusive(random, 0, plateau_end))
                            .wrapping_add(random_between_inclusive(random, 0, plateau_start))
                    }
                }
            }
        }
    }

    /// `minInclusive()`.
    pub fn min_inclusive(&self) -> i32 {
        match self {
            Self::Constant(value) => *value,
            Self::Uniform { min_inclusive, .. }
            | Self::BiasedToBottom { min_inclusive, .. }
            | Self::VeryBiasedToBottom { min_inclusive, .. }
            | Self::ClampedNormal { min_inclusive, .. }
            | Self::Trapezoid { min_inclusive, .. } => *min_inclusive,
            Self::Clamped {
                source,
                min_inclusive,
                ..
            } => (*min_inclusive).max(source.min_inclusive()),
            Self::WeightedList(list) => list
                .entries()
                .iter()
                .map(|(p, _)| p.min_inclusive())
                .fold(i32::MAX, i32::min),
        }
    }

    /// `maxInclusive()`.
    pub fn max_inclusive(&self) -> i32 {
        match self {
            Self::Constant(value) => *value,
            Self::Uniform { max_inclusive, .. }
            | Self::BiasedToBottom { max_inclusive, .. }
            | Self::VeryBiasedToBottom { max_inclusive, .. }
            | Self::ClampedNormal { max_inclusive, .. }
            | Self::Trapezoid { max_inclusive, .. } => *max_inclusive,
            Self::Clamped {
                source,
                max_inclusive,
                ..
            } => (*max_inclusive).min(source.max_inclusive()),
            Self::WeightedList(list) => list
                .entries()
                .iter()
                .map(|(p, _)| p.max_inclusive())
                .fold(i32::MIN, i32::max),
        }
    }
}

/// `FloatProvider`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FloatProvider {
    /// `ConstantFloat`.
    Constant(f32),
    /// `UniformFloat`.
    Uniform { min: f32, max: f32 },
    /// `ClampedNormalFloat`.
    ClampedNormal {
        mean: f32,
        deviation: f32,
        min: f32,
        max: f32,
    },
    /// `TrapezoidFloat`.
    Trapezoid { min: f32, max: f32, plateau: f32 },
}

impl FloatProvider {
    /// Parses `FloatProviders.CODEC`.
    pub fn from_json(json: &Value) -> Result<Self, Error> {
        if json.is_number() {
            return Ok(Self::Constant(float_value(json, "float provider")?));
        }
        let provider = match type_name(json)? {
            "constant" => Self::Constant(float_field(json, "value")?),
            "uniform" => {
                let min = float_field(json, "min_inclusive")?;
                let max = float_field(json, "max_exclusive")?;
                if max <= min {
                    return Err(Error::new(format!(
                        "float provider: max must be larger than min: [{min}, {max}]"
                    )));
                }
                Self::Uniform { min, max }
            }
            "clamped_normal" => {
                let min = float_field(json, "min")?;
                let max = float_field(json, "max")?;
                if max < min {
                    return Err(Error::new("float provider: max must be larger than min"));
                }
                Self::ClampedNormal {
                    mean: float_field(json, "mean")?,
                    deviation: float_field(json, "deviation")?,
                    min,
                    max,
                }
            }
            "trapezoid" => {
                let min = float_field(json, "min")?;
                let max = float_field(json, "max")?;
                let plateau = float_field(json, "plateau")?;
                if max < min {
                    return Err(Error::new("float provider: max must be larger than min"));
                }
                if plateau > max - min {
                    return Err(Error::new(
                        "float provider: plateau can at most be the full span",
                    ));
                }
                Self::Trapezoid { min, max, plateau }
            }
            other => return Err(Error::new(format!("unknown float provider type {other}"))),
        };
        Ok(provider)
    }

    /// Parses `FloatProviders.codec(min, max)`: the provider's range must lie within `min..=max`.
    pub fn from_json_in_range(json: &Value, min: f32, max: f32) -> Result<Self, Error> {
        let provider = Self::from_json(json)?;
        if provider.min() < min || provider.max() > max {
            return Err(Error::new(format!(
                "float provider out of range [{min}-{max}]: [{}-{}]",
                provider.min(),
                provider.max()
            )));
        }
        Ok(provider)
    }

    /// `sample(random)`.
    pub fn sample<R: RandomSource>(&self, random: &mut R) -> f32 {
        match *self {
            Self::Constant(value) => value,
            Self::Uniform { min, max } => random_between(random, min, max),
            Self::ClampedNormal {
                mean,
                deviation,
                min,
                max,
            } => clamp_f(normal(random, mean, deviation), min, max),
            Self::Trapezoid { min, max, plateau } => {
                let range = max - min;
                let plateau_start = (range - plateau) / 2.0;
                let plateau_end = range - plateau_start;
                min + random.next_float() * plateau_end + random.next_float() * plateau_start
            }
        }
    }

    /// `min()`.
    pub fn min(&self) -> f32 {
        match *self {
            Self::Constant(value) => value,
            Self::Uniform { min, .. }
            | Self::ClampedNormal { min, .. }
            | Self::Trapezoid { min, .. } => min,
        }
    }

    /// `max()`.
    pub fn max(&self) -> f32 {
        match *self {
            Self::Constant(value) => value,
            Self::Uniform { max, .. }
            | Self::ClampedNormal { max, .. }
            | Self::Trapezoid { max, .. } => max,
        }
    }
}

/// `HeightProvider`.
#[derive(Clone, Debug, PartialEq)]
pub enum HeightProvider {
    /// `ConstantHeight`.
    Constant(VerticalAnchor),
    /// `UniformHeight`.
    Uniform {
        min_inclusive: VerticalAnchor,
        max_inclusive: VerticalAnchor,
    },
    /// `BiasedToBottomHeight`.
    BiasedToBottom {
        min_inclusive: VerticalAnchor,
        max_inclusive: VerticalAnchor,
        inner: i32,
    },
    /// `VeryBiasedToBottomHeight`.
    VeryBiasedToBottom {
        min_inclusive: VerticalAnchor,
        max_inclusive: VerticalAnchor,
        inner: i32,
    },
    /// `TrapezoidHeight`.
    Trapezoid {
        min_inclusive: VerticalAnchor,
        max_inclusive: VerticalAnchor,
        plateau: i32,
    },
    /// `WeightedListHeight`.
    WeightedList(WeightedList<HeightProvider>),
}

impl HeightProvider {
    /// Parses `HeightProvider.CODEC`: a vertical anchor or a typed provider.
    pub fn from_json(json: &Value) -> Result<Self, Error> {
        if json.get("type").is_none() {
            return Ok(Self::Constant(VerticalAnchor::from_json(json)?));
        }
        let anchor = |name: &str| VerticalAnchor::from_json(field(json, name)?);
        let inner = || -> Result<i32, Error> {
            match json.get("inner") {
                None => Ok(1),
                Some(v) => {
                    let inner = int_value(v, "inner")?;
                    if inner < 1 {
                        return Err(Error::new("height provider: inner must be positive"));
                    }
                    Ok(inner)
                }
            }
        };
        let provider = match type_name(json)? {
            "constant" => Self::Constant(anchor("value")?),
            "uniform" => Self::Uniform {
                min_inclusive: anchor("min_inclusive")?,
                max_inclusive: anchor("max_inclusive")?,
            },
            "biased_to_bottom" => Self::BiasedToBottom {
                min_inclusive: anchor("min_inclusive")?,
                max_inclusive: anchor("max_inclusive")?,
                inner: inner()?,
            },
            "very_biased_to_bottom" => Self::VeryBiasedToBottom {
                min_inclusive: anchor("min_inclusive")?,
                max_inclusive: anchor("max_inclusive")?,
                inner: inner()?,
            },
            "trapezoid" => Self::Trapezoid {
                min_inclusive: anchor("min_inclusive")?,
                max_inclusive: anchor("max_inclusive")?,
                plateau: match json.get("plateau") {
                    None => 0,
                    Some(v) => int_value(v, "plateau")?,
                },
            },
            "weighted_list" => Self::WeightedList(WeightedList::from_json(
                field(json, "distribution")?,
                Self::from_json,
            )?),
            other => return Err(Error::new(format!("unknown height provider type {other}"))),
        };
        Ok(provider)
    }

    /// `sample(random, context)`.
    ///
    /// Empty ranges return the minimum as Vanilla does (which also logs a warning).
    pub fn sample<R: RandomSource>(&self, random: &mut R, context: &WorldGenerationContext) -> i32 {
        match self {
            Self::Constant(anchor) => anchor.resolve_y(context),
            Self::Uniform {
                min_inclusive,
                max_inclusive,
            } => {
                let min = min_inclusive.resolve_y(context);
                let max = max_inclusive.resolve_y(context);
                if min > max {
                    min
                } else {
                    random_between_inclusive(random, min, max)
                }
            }
            Self::BiasedToBottom {
                min_inclusive,
                max_inclusive,
                inner,
            } => {
                let min = min_inclusive.resolve_y(context);
                let max = max_inclusive.resolve_y(context);
                let span = max.wrapping_sub(min).wrapping_sub(*inner).wrapping_add(1);
                if span <= 0 {
                    min
                } else {
                    let limit = random.next_int_bounded(span);
                    random
                        .next_int_bounded(limit.wrapping_add(*inner))
                        .wrapping_add(min)
                }
            }
            Self::VeryBiasedToBottom {
                min_inclusive,
                max_inclusive,
                inner,
            } => {
                let min = min_inclusive.resolve_y(context);
                let max = max_inclusive.resolve_y(context);
                let span = max.wrapping_sub(min).wrapping_sub(*inner).wrapping_add(1);
                if span <= 0 {
                    min
                } else {
                    let upper = next_int(random, min.wrapping_add(*inner), max);
                    let biased_upper = next_int(random, min, upper.wrapping_sub(1));
                    next_int(
                        random,
                        min,
                        biased_upper.wrapping_sub(1).wrapping_add(*inner),
                    )
                }
            }
            Self::Trapezoid {
                min_inclusive,
                max_inclusive,
                plateau,
            } => {
                let min = min_inclusive.resolve_y(context);
                let max = max_inclusive.resolve_y(context);
                if min > max {
                    min
                } else {
                    let range = max.wrapping_sub(min);
                    if *plateau >= range {
                        random_between_inclusive(random, min, max)
                    } else {
                        let plateau_start = (range - plateau) / 2;
                        let plateau_end = range - plateau_start;
                        min.wrapping_add(random_between_inclusive(random, 0, plateau_end))
                            .wrapping_add(random_between_inclusive(random, 0, plateau_start))
                    }
                }
            }
            Self::WeightedList(list) => list.get_random(random).sample(random, context),
        }
    }
}

/// `Mth.randomBetweenInclusive(random, min, maxInclusive)`.
pub fn random_between_inclusive<R: RandomSource>(
    random: &mut R,
    min: i32,
    max_inclusive: i32,
) -> i32 {
    random
        .next_int_bounded(max_inclusive.wrapping_sub(min).wrapping_add(1))
        .wrapping_add(min)
}

/// `Mth.randomBetween(random, min, maxExclusive)`.
pub fn random_between<R: RandomSource>(random: &mut R, min: f32, max_exclusive: f32) -> f32 {
    random.next_float() * (max_exclusive - min) + min
}

/// `Mth.nextInt(random, minInclusive, maxInclusive)`.
pub fn next_int<R: RandomSource>(random: &mut R, min_inclusive: i32, max_inclusive: i32) -> i32 {
    if min_inclusive >= max_inclusive {
        min_inclusive
    } else {
        random
            .next_int_bounded(max_inclusive.wrapping_sub(min_inclusive).wrapping_add(1))
            .wrapping_add(min_inclusive)
    }
}

/// `Mth.normal(random, mean, deviation)`: `mean + (float) random.nextGaussian() * deviation`.
pub fn normal<R: RandomSource>(random: &mut R, mean: f32, deviation: f32) -> f32 {
    mean + random.next_gaussian() as f32 * deviation
}

fn field<'v>(json: &'v Value, name: &str) -> Result<&'v Value, Error> {
    json.get(name)
        .ok_or_else(|| Error::new(format!("provider: missing field {name} in {json}")))
}

fn type_name(json: &Value) -> Result<&str, Error> {
    let name = field(json, "type")?
        .as_str()
        .ok_or_else(|| Error::new("provider: type is not a string"))?;
    Ok(name.strip_prefix("minecraft:").unwrap_or(name))
}

fn int_value(json: &Value, what: &str) -> Result<i32, Error> {
    json.as_i64()
        .and_then(|v| i32::try_from(v).ok())
        .ok_or_else(|| Error::new(format!("{what}: not an int: {json}")))
}

fn int_field(json: &Value, name: &str) -> Result<i32, Error> {
    int_value(field(json, name)?, name)
}

/// `Codec.FLOAT`: the JSON number narrowed to `float`.
fn float_value(json: &Value, what: &str) -> Result<f32, Error> {
    json.as_f64()
        .map(|v| v as f32)
        .ok_or_else(|| Error::new(format!("{what}: not a number: {json}")))
}

fn float_field(json: &Value, name: &str) -> Result<f32, Error> {
    float_value(field(json, name)?, name)
}
