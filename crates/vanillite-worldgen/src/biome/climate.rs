//! `Climate`: quantized climate parameters, target points, parameter lists and the climate
//! sampler of the multi-noise biome source.
//!
//! Climate values are floats quantized to `long`s (`value * 10000`, truncated); every comparison
//! and distance works on the quantized values with Java's wrapping `long` arithmetic.

use std::fmt;
use std::sync::{Mutex, PoisonError};

use rustc_hash::FxHashMap;
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

/// Sampled climates of quart columns at quart y 0, shared by all threads of one generator.
///
/// The carvers of every chunk look up the biomes of the 17 × 17 chunks around it with an
/// uncached resolver, so each column is sampled by up to 289 chunks. The climate is a pure
/// function of the position; the R-tree search result depends on the query order only through
/// its hint (the previous result), so with the climate the cache keeps the last searches as
/// (hint, result) pairs: neighbouring chunks query the columns in the same order and mostly
/// repeat them. Sharded so threads rarely meet; a full shard is cleared.
pub struct ClimateCache {
    shards: Box<[Mutex<ClimateShard>]>,
}

/// Climates and recent searches by quart column.
type ClimateShard = FxHashMap<(i32, i32), ClimateEntry>;

/// A cached column: its climate and its last two searches as `(hint + 1 or 0, result)`
/// (`u32::MAX` hints mark unused slots).
#[derive(Clone, Copy)]
struct ClimateEntry {
    target: TargetPoint,
    searches: [(u32, u32); 2],
}

/// The memo key of a search hint.
fn hint_key(hint: Option<u32>) -> u32 {
    hint.map_or(0, |h| h.wrapping_add(1))
}

impl ClimateCache {
    const SHARDS: usize = 32;
    /// Columns per shard before it is cleared (about 2 MiB for all shards).
    const MAX_PER_SHARD: usize = 1024;

    /// An empty cache.
    pub fn new() -> Self {
        Self {
            shards: (0..Self::SHARDS).map(|_| Mutex::default()).collect(),
        }
    }

    /// The R-tree search result for column `(quart_x, quart_z)` with hint `hint`: `search`
    /// of the column's climate (from `sample` if it is not cached yet), unless the same search
    /// is cached. Both callbacks must be pure.
    pub fn search_at(
        &self,
        quart_x: i32,
        quart_z: i32,
        hint: Option<u32>,
        sample: impl FnOnce() -> TargetPoint,
        search: impl FnOnce(&TargetPoint, Option<u32>) -> u32,
    ) -> u32 {
        let hash = (quart_x as u32 as u64)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add((quart_z as u32 as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F));
        let shard = &self.shards[(hash >> 59) as usize % Self::SHARDS];
        let key = (quart_x, quart_z);
        let hint_key = hint_key(hint);
        let cached = shard
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
            .copied();
        let target = match cached {
            Some(entry) => {
                if let Some(&(_, result)) = entry.searches.iter().find(|s| s.0 == hint_key) {
                    return result;
                }
                entry.target
            }
            None => sample(),
        };
        let result = search(&target, hint);
        let mut map = shard.lock().unwrap_or_else(PoisonError::into_inner);
        if map.len() >= Self::MAX_PER_SHARD && !map.contains_key(&key) {
            map.clear();
        }
        let entry = map.entry(key).or_insert(ClimateEntry {
            target,
            searches: [(u32::MAX, 0); 2],
        });
        entry.searches = [(hint_key, result), entry.searches[0]];
        result
    }
}

impl Default for ClimateCache {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for ClimateCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClimateCache").finish_non_exhaustive()
    }
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
    /// The entries by depth parameter, for searches along a column (see [`ColumnSearch`]);
    /// `None` when the entries have too many different depth parameters for that to pay off.
    depth_classes: Option<DepthClasses>,
}

/// Index of the depth in a parameter array.
const DEPTH: usize = 4;
/// Most different depth parameters a list may have to be searched by column.
const MAX_DEPTH_CLASSES: usize = 8;
/// Target coordinates up to this magnitude cannot overflow a distance.
const SAFE_COORDINATE: i64 = 1 << 29;

/// The entries of a list grouped by their depth parameter.
#[derive(Debug)]
struct DepthClasses {
    classes: Vec<DepthClass>,
}

/// The entries with one depth parameter.
#[derive(Debug)]
struct DepthClass {
    depth: Parameter,
    /// A search tree over the entries of the class.
    tree: RTree,
    /// Per leaf of `tree`: the entry's index in the list.
    entries: Vec<u32>,
    /// Per leaf of `tree`: the entry's depth-first position in the list's tree.
    ranks: Vec<u32>,
}

/// The closest entries of a [`ParameterList`] for the targets of one column: targets that
/// differ only in depth. Within one depth class (entries with the same depth parameter) the
/// closest entry is the same for every depth, so one search per class answers the whole
/// column: a target's closest entry is the best class entry with the smallest total distance.
/// Ties resolve as in the list's tree (see `rtree.rs`): the previous result if it is as close,
/// else the first closest entry in depth-first order.
#[derive(Clone, Copy, Debug)]
pub struct ColumnSearch {
    /// The target's coordinates other than depth.
    key: [i64; 5],
    /// Per depth class: the distance without the depth term, the entry and its rank.
    best: [(i64, u32, u32); MAX_DEPTH_CLASSES],
}

/// The coordinates of a target other than its depth.
#[inline]
fn column_key(target: &[i64; 7]) -> [i64; 5] {
    [target[0], target[1], target[2], target[3], target[5]]
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
        let depth_classes = DepthClasses::build(&spaces, &tree);
        Ok(Self {
            values,
            tree,
            depth_classes,
        })
    }

    /// The column search for the column of `target` (any of its targets), if the list supports
    /// it and the target is in the range where distances cannot overflow.
    pub fn column_search(&self, target: &TargetPoint) -> Option<ColumnSearch> {
        let classes = self.depth_classes.as_ref()?;
        let mut target = target.to_parameter_array();
        if target.iter().any(|c| c.abs() > SAFE_COORDINATE) {
            return None;
        }
        let key = column_key(&target);
        let mut best = [(i64::MAX, 0, u32::MAX); MAX_DEPTH_CLASSES];
        for (class, best) in classes.classes.iter().zip(&mut best) {
            // Inside the class's depth interval, so the depth adds nothing.
            target[DEPTH] = class.depth.min;
            let (leaf, distance) = class.tree.search_lowest_rank(&target, &class.ranks);
            *best = (distance, class.entries[leaf], class.ranks[leaf]);
        }
        Some(ColumnSearch { key, best })
    }

    /// `tree.search(target, cache)` for a target of the column `column` was made for: the index
    /// of the closest value; updates `cache` like [`Self::find_value`]. `None` (nothing done)
    /// if the target is not of that column.
    #[inline]
    pub fn search_in_column(
        &self,
        column: &ColumnSearch,
        target: &TargetPoint,
        cache: &mut SearchCache,
    ) -> Option<u32> {
        let classes = self.depth_classes.as_ref()?;
        let target = target.to_parameter_array();
        if column_key(&target) != column.key || target[DEPTH].abs() > SAFE_COORDINATE {
            return None;
        }
        let mut closest = (i64::MAX, 0, u32::MAX);
        for (class, &(base, entry, rank)) in classes.classes.iter().zip(&column.best) {
            let depth = class.depth.distance(target[DEPTH]);
            let total = base + depth * depth;
            if total < closest.0 || (total == closest.0 && rank < closest.2) {
                closest = (total, entry, rank);
            }
        }
        let mut index = closest.1;
        if let Some(previous) = cache.0
            && (previous as usize) < self.values.len()
            && distance(self.tree.leaf_space(previous as usize), &target) == closest.0
        {
            index = previous;
        }
        cache.0 = Some(index);
        Some(index)
    }

    /// `values()`.
    pub fn values(&self) -> &[(ParameterPoint, T)] {
        &self.values
    }

    /// `tree.search(target, hint)`: the index of the closest value, the search starting from
    /// the leaf `hint` (a previous result).
    pub fn search_index(&self, target: &TargetPoint, hint: Option<u32>) -> u32 {
        self.tree
            .search(&target.to_parameter_array(), hint.map(|i| i as usize)) as u32
    }

    /// The value at `index` (a [`Self::search_index`] result).
    pub fn value_at(&self, index: u32) -> &T {
        &self.values[index as usize].1
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

impl DepthClasses {
    /// Groups the entries (`spaces`, searched by `tree`) by depth parameter.
    fn build(spaces: &[[Parameter; 7]], tree: &RTree) -> Option<Self> {
        let mut depths: Vec<Parameter> = Vec::new();
        for space in spaces {
            if !depths.contains(&space[DEPTH]) {
                if depths.len() == MAX_DEPTH_CLASSES {
                    return None;
                }
                depths.push(space[DEPTH]);
            }
        }
        // Entry coordinates in the range where distances cannot overflow.
        if spaces
            .iter()
            .flatten()
            .any(|p| p.min.abs() > SAFE_COORDINATE || p.max.abs() > SAFE_COORDINATE)
        {
            return None;
        }
        let all_ranks = tree.depth_first_ranks();
        let classes = depths
            .into_iter()
            .map(|depth| {
                let entries: Vec<u32> = (0..spaces.len() as u32)
                    .filter(|&i| spaces[i as usize][DEPTH] == depth)
                    .collect();
                let class_spaces: Vec<[Parameter; 7]> =
                    entries.iter().map(|&i| spaces[i as usize]).collect();
                DepthClass {
                    depth,
                    tree: RTree::build(&class_spaces, 6),
                    ranks: entries.iter().map(|&i| all_ranks[i as usize]).collect(),
                    entries,
                }
            })
            .collect();
        Some(Self { classes })
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
