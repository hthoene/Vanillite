//! Placement modifiers (`net.minecraft.world.level.levelgen.placement`): turn one position into
//! zero or more positions.
//!
//! Every type registered in `PlacementModifierTypes` is ported, including `cuboid`
//! (`levelgen.feature.CuboidPlacement`). [`CaveSurface`] and [`parse_heightmap`] are the
//! placement package's helpers that feature configurations use too.

use serde_json::Value;
use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Direction};
use vanillite_chunk::HeightmapKind;
use vanillite_data::block::{BlockState, ids};

use super::predicate::{BlockPredicate, parse_vec3i};
use super::region::WorldGenRegion;
use super::{FeatureEnv, FeatureParser, PlacedFeatureId};
use crate::Error;
use crate::anchor::codec_int;
use crate::biome::biome_info_noise;
use crate::carver::providers::{HeightProvider, IntProvider};

/// `PlacementContext`: the region, the dimension's feature data and the top level placed
/// feature (set when the biome filter applies).
///
/// Vanilla's `PlacementContext` is also the `WorldGenerationContext` of the generator and
/// level; that is `env.context`.
#[derive(Debug)]
pub struct PlacementContext<'r, 'a> {
    pub level: &'r WorldGenRegion<'a>,
    pub env: &'r FeatureEnv,
    pub top_feature: Option<PlacedFeatureId>,
}

impl PlacementContext<'_, '_> {
    /// `getHeight(type, x, z)`.
    #[inline]
    pub fn height(&self, kind: HeightmapKind, x: i32, z: i32) -> i32 {
        self.level.height_at(kind, x, z)
    }

    /// `getBlockState(pos)`.
    #[inline]
    pub fn block(&self, pos: BlockPos) -> BlockState {
        self.level.block(pos)
    }

    /// `getMinY()` (of the level).
    #[inline]
    pub fn min_y(&self) -> i32 {
        self.level.min_y()
    }
}

/// A placement modifier (`PlacementModifier`).
#[derive(Clone, Debug)]
pub enum PlacementModifier {
    /// `block_predicate_filter` (`BlockPredicateFilter`).
    BlockPredicateFilter(BlockPredicate),
    /// `rarity_filter` (`RarityFilter`): passes with chance `1 / chance`.
    RarityFilter { chance: i32 },
    /// `random_chance` (`RandomChancePlacement`): passes with chance `chance`.
    RandomChance { chance: f32 },
    /// `surface_relative_threshold_filter` (`SurfaceRelativeThresholdFilter`).
    SurfaceRelativeThresholdFilter {
        heightmap: HeightmapKind,
        min_inclusive: i32,
        max_inclusive: i32,
    },
    /// `surface_water_depth_filter` (`SurfaceWaterDepthFilter`).
    SurfaceWaterDepthFilter { max_water_depth: i32 },
    /// `biome` (`BiomeFilter`): the biome at the position has the top level placed feature.
    Biome,
    /// `count` (`CountPlacement`): the position repeated `count` times.
    Count(IntProvider),
    /// `noise_based_count` (`NoiseBasedCountPlacement`).
    NoiseBasedCount {
        noise_to_count_ratio: i32,
        noise_factor: f64,
        noise_offset: f64,
    },
    /// `noise_threshold_count` (`NoiseThresholdCountPlacement`).
    NoiseThresholdCount {
        noise_level: f64,
        below_noise: i32,
        above_noise: i32,
    },
    /// `count_on_every_layer` (`CountOnEveryLayerPlacement`).
    CountOnEveryLayer(IntProvider),
    /// `cuboid` (`CuboidPlacement`).
    Cuboid(CuboidPlacement),
    /// `environment_scan` (`EnvironmentScanPlacement`).
    EnvironmentScan {
        direction_of_search: Direction,
        target_condition: BlockPredicate,
        allowed_search_condition: BlockPredicate,
        max_steps: i32,
    },
    /// `heightmap` (`HeightmapPlacement`).
    Heightmap(HeightmapKind),
    /// `height_range` (`HeightRangePlacement`).
    HeightRange(HeightProvider),
    /// `in_square` (`InSquarePlacement`).
    InSquare,
    /// `offset` (`OffsetPlacement`).
    Offset {
        x: IntProvider,
        y: IntProvider,
        z: IntProvider,
    },
    /// `randomly_selected` (`RandomlySelectedPlacement`): one of the modifiers, picked
    /// uniformly.
    RandomlySelected(Vec<PlacementModifier>),
    /// `fixed_placement` (`FixedPlacement`): the listed positions inside the origin's chunk.
    FixedPlacement(Vec<BlockPos>),
}

/// `CuboidPlacement`: every position of a random box above and beside the origin, optionally
/// without its edges or interior.
#[derive(Clone, Debug)]
pub struct CuboidPlacement {
    pub xz_size: IntProvider,
    pub y_size: IntProvider,
    pub include_edges: bool,
    pub include_interior: bool,
}

impl CuboidPlacement {
    /// Parses `CuboidPlacement.CODEC`.
    pub fn parse(json: &Value) -> Result<Self, Error> {
        Ok(Self {
            xz_size: IntProvider::from_json_in_range(field(json, "xz_size")?, 1, 16)?,
            y_size: IntProvider::from_json_in_range(field(json, "y_size")?, 1, 16)?,
            include_edges: bool_or(json, "include_edges", true)?,
            include_interior: bool_or(json, "include_interior", true)?,
        })
    }

    /// `modify(context, random, origin, output)`.
    pub fn modify<R: RandomSource>(
        &self,
        random: &mut R,
        origin: BlockPos,
        out: &mut Vec<BlockPos>,
    ) {
        let height = self.y_size.sample(random);
        let width = self.xz_size.sample(random);
        let length = self.xz_size.sample(random);
        let (edges, interior) = (self.include_edges, self.include_interior);
        for x in 0..=width {
            let x_edge = x == 0 || x == width;
            for y in 0..=height {
                let y_edge = y == 0 || y == height;
                for z in 0..=length {
                    let z_edge = z == 0 || z == length;
                    if (edges || !x_edge || !y_edge)
                        && (edges || !z_edge || !y_edge)
                        && (edges || !x_edge || !z_edge)
                        && (interior || x_edge || y_edge || z_edge)
                    {
                        out.push(BlockPos::new(
                            x.wrapping_add(origin.x),
                            y.wrapping_add(origin.y),
                            z.wrapping_add(origin.z),
                        ));
                    }
                }
            }
        }
    }
}

/// `CaveSurface`: the floor or ceiling of a cave, as feature configurations name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaveSurface {
    /// `ceiling`.
    Ceiling,
    /// `floor`.
    Floor,
}

impl CaveSurface {
    /// Parses `CaveSurface.CODEC` (`"ceiling"` or `"floor"`).
    pub fn parse(json: &Value) -> Result<Self, Error> {
        match json.as_str() {
            Some("ceiling") => Ok(Self::Ceiling),
            Some("floor") => Ok(Self::Floor),
            _ => Err(Error::new(format!("unknown cave surface {json}"))),
        }
    }

    /// `getDirection()`: towards the surface (up for the ceiling).
    pub const fn direction(self) -> Direction {
        match self {
            Self::Ceiling => Direction::Up,
            Self::Floor => Direction::Down,
        }
    }

    /// `getY()`: `1` for the ceiling, `-1` for the floor.
    pub const fn y(self) -> i32 {
        match self {
            Self::Ceiling => 1,
            Self::Floor => -1,
        }
    }
}

/// `Heightmap.Types.CODEC`: a heightmap type by its serialized name (`MOTION_BLOCKING`, …).
pub fn parse_heightmap(json: &Value) -> Result<HeightmapKind, Error> {
    Ok(match json.as_str() {
        Some("WORLD_SURFACE_WG") => HeightmapKind::WorldSurfaceWg,
        Some("WORLD_SURFACE") => HeightmapKind::WorldSurface,
        Some("OCEAN_FLOOR_WG") => HeightmapKind::OceanFloorWg,
        Some("OCEAN_FLOOR") => HeightmapKind::OceanFloor,
        Some("MOTION_BLOCKING") => HeightmapKind::MotionBlocking,
        Some("MOTION_BLOCKING_NO_LEAVES") => HeightmapKind::MotionBlockingNoLeaves,
        _ => return Err(Error::new(format!("unknown heightmap type {json}"))),
    })
}

impl PlacementModifier {
    /// Parses `PlacementModifier.CODEC` (dispatch on `type`).
    pub fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        let kind = json
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::new(format!("placement modifier without type: {json}")))?;
        let modifier = match kind.strip_prefix("minecraft:").unwrap_or(kind) {
            "block_predicate_filter" => {
                Self::BlockPredicateFilter(parser.block_predicate(field(json, "predicate")?)?)
            }
            "rarity_filter" => {
                let chance = int(json, "chance")?;
                if chance <= 0 {
                    return Err(Error::new(format!(
                        "rarity_filter: value must be positive: {chance}"
                    )));
                }
                Self::RarityFilter { chance }
            }
            "random_chance" => {
                let chance = float(json, "chance")?;
                if !(0.0..=1.0).contains(&chance) {
                    return Err(Error::new(format!(
                        "random_chance: value {chance} outside of range [0:1]"
                    )));
                }
                Self::RandomChance { chance }
            }
            "surface_relative_threshold_filter" => Self::SurfaceRelativeThresholdFilter {
                heightmap: parse_heightmap(field(json, "heightmap")?)?,
                min_inclusive: int_or(json, "min_inclusive", i32::MIN)?,
                max_inclusive: int_or(json, "max_inclusive", i32::MAX)?,
            },
            "surface_water_depth_filter" => Self::SurfaceWaterDepthFilter {
                max_water_depth: int(json, "max_water_depth")?,
            },
            "biome" => Self::Biome,
            "count" => Self::Count(IntProvider::from_json_in_range(
                field(json, "count")?,
                0,
                4096,
            )?),
            "noise_based_count" => Self::NoiseBasedCount {
                noise_to_count_ratio: int(json, "noise_to_count_ratio")?,
                noise_factor: double(json, "noise_factor")?,
                noise_offset: match json.get("noise_offset") {
                    None => 0.0,
                    Some(_) => double(json, "noise_offset")?,
                },
            },
            "noise_threshold_count" => Self::NoiseThresholdCount {
                noise_level: double(json, "noise_level")?,
                below_noise: int(json, "below_noise")?,
                above_noise: int(json, "above_noise")?,
            },
            "count_on_every_layer" => Self::CountOnEveryLayer(IntProvider::from_json_in_range(
                field(json, "count")?,
                0,
                256,
            )?),
            "cuboid" => Self::Cuboid(CuboidPlacement::parse(json)?),
            "environment_scan" => {
                let direction_of_search = match field(json, "direction_of_search")?.as_str() {
                    Some("up") => Direction::Up,
                    Some("down") => Direction::Down,
                    _ => {
                        return Err(Error::new(
                            "environment_scan: direction_of_search must be up or down",
                        ));
                    }
                };
                let target_condition = parser.block_predicate(field(json, "target_condition")?)?;
                let allowed_search_condition = match json.get("allowed_search_condition") {
                    None => BlockPredicate::always_true(),
                    Some(predicate) => parser.block_predicate(predicate)?,
                };
                let max_steps = int(json, "max_steps")?;
                if !(1..=32).contains(&max_steps) {
                    return Err(Error::new(format!(
                        "environment_scan: max_steps {max_steps} outside of range [1:32]"
                    )));
                }
                Self::EnvironmentScan {
                    direction_of_search,
                    target_condition,
                    allowed_search_condition,
                    max_steps,
                }
            }
            "heightmap" => Self::Heightmap(parse_heightmap(field(json, "heightmap")?)?),
            "height_range" => Self::HeightRange(HeightProvider::from_json(field(json, "height")?)?),
            "in_square" => Self::InSquare,
            "offset" => {
                let axis = |name| IntProvider::from_json_in_range(field(json, name)?, -16, 16);
                Self::Offset {
                    x: axis("x")?,
                    y: axis("y")?,
                    z: axis("z")?,
                }
            }
            "randomly_selected" => {
                let placements = field(json, "placements")?
                    .as_array()
                    .ok_or_else(|| Error::new("randomly_selected: placements is not a list"))?
                    .iter()
                    .map(|p| parser.placement(p))
                    .collect::<Result<Vec<_>, _>>()?;
                if placements.is_empty() {
                    return Err(Error::new("randomly_selected: list must have contents"));
                }
                Self::RandomlySelected(placements)
            }
            "fixed_placement" => Self::FixedPlacement(
                field(json, "positions")?
                    .as_array()
                    .ok_or_else(|| Error::new("fixed_placement: positions is not a list"))?
                    .iter()
                    .map(|p| parse_vec3i(p, None))
                    .collect::<Result<_, _>>()?,
            ),
            other => {
                return Err(Error::new(format!(
                    "unknown placement modifier type {other}"
                )));
            }
        };
        Ok(modifier)
    }

    /// `modify(context, random, origin, output)`: appends the resulting positions to `out`.
    pub fn modify<R: RandomSource>(
        &self,
        ctx: &PlacementContext<'_, '_>,
        random: &mut R,
        origin: BlockPos,
        out: &mut Vec<BlockPos>,
    ) {
        match self {
            // `PlacementFilter`s.
            Self::BlockPredicateFilter(predicate) => {
                if predicate.test(ctx.level, origin) {
                    out.push(origin);
                }
            }
            Self::RarityFilter { chance } => {
                if random.next_float() < 1.0 / *chance as f32 {
                    out.push(origin);
                }
            }
            Self::RandomChance { chance } => {
                if random.next_float() < *chance {
                    out.push(origin);
                }
            }
            Self::SurfaceRelativeThresholdFilter {
                heightmap,
                min_inclusive,
                max_inclusive,
            } => {
                let surface_y = i64::from(ctx.height(*heightmap, origin.x, origin.z));
                let min_y = surface_y + i64::from(*min_inclusive);
                let max_y = surface_y + i64::from(*max_inclusive);
                if (min_y..=max_y).contains(&i64::from(origin.y)) {
                    out.push(origin);
                }
            }
            Self::SurfaceWaterDepthFilter { max_water_depth } => {
                let ocean_floor = ctx.height(HeightmapKind::OceanFloor, origin.x, origin.z);
                let surface = ctx.height(HeightmapKind::WorldSurface, origin.x, origin.z);
                if surface.wrapping_sub(ocean_floor) <= *max_water_depth {
                    out.push(origin);
                }
            }
            Self::Biome => {
                if biome_filter(ctx, origin) {
                    out.push(origin);
                }
            }
            // `RepeatingPlacement`s.
            Self::Count(count) => repeat(out, origin, count.sample(random)),
            Self::NoiseBasedCount {
                noise_to_count_ratio,
                noise_factor,
                noise_offset,
            } => {
                let noise = f64::from(biome_info_noise(
                    f64::from(origin.x) / noise_factor,
                    f64::from(origin.z) / noise_factor,
                ));
                let count = ((noise + noise_offset) * f64::from(*noise_to_count_ratio)).ceil();
                repeat(out, origin, count as i32);
            }
            Self::NoiseThresholdCount {
                noise_level,
                below_noise,
                above_noise,
            } => {
                let noise = f64::from(biome_info_noise(
                    f64::from(origin.x) / 200.0,
                    f64::from(origin.z) / 200.0,
                ));
                let count = if noise < *noise_level {
                    *below_noise
                } else {
                    *above_noise
                };
                repeat(out, origin, count);
            }
            // Other modifiers.
            Self::CountOnEveryLayer(count) => count_on_every_layer(ctx, count, random, origin, out),
            Self::Cuboid(cuboid) => cuboid.modify(random, origin, out),
            Self::EnvironmentScan {
                direction_of_search,
                target_condition,
                allowed_search_condition,
                max_steps,
            } => {
                let level = ctx.level;
                let mut pos = origin;
                if !allowed_search_condition.test(level, pos) {
                    return;
                }
                for _ in 0..*max_steps {
                    if target_condition.test(level, pos) {
                        out.push(pos);
                        return;
                    }
                    pos = pos.relative(*direction_of_search);
                    if level.is_outside_build_height(pos.y) {
                        return;
                    }
                    if !allowed_search_condition.test(level, pos) {
                        break;
                    }
                }
                if target_condition.test(level, pos) {
                    out.push(pos);
                }
            }
            Self::Heightmap(heightmap) => {
                let height = ctx.height(*heightmap, origin.x, origin.z);
                if height > ctx.min_y() {
                    out.push(BlockPos::new(origin.x, height, origin.z));
                }
            }
            Self::HeightRange(height) => {
                let y = height.sample(random, &ctx.env.context);
                out.push(BlockPos::new(origin.x, y, origin.z));
            }
            Self::InSquare => {
                let x = random.next_int_bounded(16).wrapping_add(origin.x);
                let z = random.next_int_bounded(16).wrapping_add(origin.z);
                out.push(BlockPos::new(x, origin.y, z));
            }
            Self::Offset { x, y, z } => {
                let dx = x.sample(random);
                let dy = y.sample(random);
                let dz = z.sample(random);
                out.push(origin.offset(dx, dy, dz));
            }
            Self::RandomlySelected(placements) => {
                // `Util.getRandom(list, random)`.
                let index = random.next_int_bounded(placements.len() as i32) as usize;
                placements[index].modify(ctx, random, origin, out);
            }
            Self::FixedPlacement(positions) => {
                let (chunk_x, chunk_z) = (origin.x >> 4, origin.z >> 4);
                out.extend(
                    positions
                        .iter()
                        .filter(|p| p.x >> 4 == chunk_x && p.z >> 4 == chunk_z),
                );
            }
        }
    }
}

/// `RepeatingPlacement.modify`: `count` copies of the origin (none for `count <= 0`).
fn repeat(out: &mut Vec<BlockPos>, origin: BlockPos, count: i32) {
    for _ in 0..count {
        out.push(origin);
    }
}

/// `BiomeFilter.shouldPlace`.
///
/// Panics like Vanilla (`IllegalStateException`) when the placed feature is not placed as a top
/// level feature of a biome.
fn biome_filter(ctx: &PlacementContext<'_, '_>, origin: BlockPos) -> bool {
    let feature = ctx.top_feature.expect(
        "Tried to biome check an unregistered feature, or a feature that should not restrict \
         the biome",
    );
    let biome = ctx.level.biome(origin);
    ctx.env
        .biome_feature_sets
        .get(usize::from(biome))
        .is_some_and(|set| set.contains(&feature))
}

/// `CountOnEveryLayerPlacement.modify`. The count is sampled again before every iteration, as
/// Vanilla's loop condition does.
fn count_on_every_layer<R: RandomSource>(
    ctx: &PlacementContext<'_, '_>,
    count: &IntProvider,
    random: &mut R,
    origin: BlockPos,
    out: &mut Vec<BlockPos>,
) {
    let mut layer = 0;
    loop {
        let mut found_any = false;
        let mut i = 0;
        while i < count.sample(random) {
            let x = random.next_int_bounded(16).wrapping_add(origin.x);
            let z = random.next_int_bounded(16).wrapping_add(origin.z);
            let start_y = ctx.height(HeightmapKind::MotionBlocking, x, z);
            if let Some(y) = find_on_ground_y(ctx, x, start_y, z, layer) {
                out.push(BlockPos::new(x, y, z));
                found_any = true;
            }
            i += 1;
        }
        layer += 1;
        if !found_any {
            break;
        }
    }
}

/// `CountOnEveryLayerPlacement.findOnGroundYPosition`: the Y above the `layer`-th (from the
/// top) non-empty block below an empty one, scanning down from `start_y`.
fn find_on_ground_y(
    ctx: &PlacementContext<'_, '_>,
    x: i32,
    start_y: i32,
    z: i32,
    layer: i32,
) -> Option<i32> {
    let mut current_layer = 0;
    let mut current = ctx.block(BlockPos::new(x, start_y, z));
    let mut y = start_y;
    while y > ctx.min_y() {
        let below_y = y - 1;
        let below = ctx.block(BlockPos::new(x, below_y, z));
        if !is_empty(below) && is_empty(current) && below.block() != ids::BEDROCK {
            if current_layer == layer {
                return Some(below_y + 1);
            }
            current_layer += 1;
        }
        current = below;
        y -= 1;
    }
    None
}

/// `CountOnEveryLayerPlacement.isEmpty`: air, water or lava (any state).
fn is_empty(state: BlockState) -> bool {
    let block = state.block();
    state.is_air() || block == ids::WATER || block == ids::LAVA
}

fn field<'v>(json: &'v Value, name: &str) -> Result<&'v Value, Error> {
    json.get(name)
        .ok_or_else(|| Error::new(format!("placement: missing field {name} in {json}")))
}

/// `Codec.INT.fieldOf(name)`.
fn int(json: &Value, name: &str) -> Result<i32, Error> {
    let value = field(json, name)?;
    codec_int(value).ok_or_else(|| Error::new(format!("placement: {name} is not an int: {value}")))
}

/// `Codec.INT.optionalFieldOf(name, default)`.
fn int_or(json: &Value, name: &str, default: i32) -> Result<i32, Error> {
    match json.get(name) {
        None => Ok(default),
        Some(_) => int(json, name),
    }
}

/// `Codec.DOUBLE.fieldOf(name)`.
fn double(json: &Value, name: &str) -> Result<f64, Error> {
    let value = field(json, name)?;
    value
        .as_f64()
        .ok_or_else(|| Error::new(format!("placement: {name} is not a number: {value}")))
}

/// `Codec.FLOAT.fieldOf(name)`.
fn float(json: &Value, name: &str) -> Result<f32, Error> {
    double(json, name).map(|v| v as f32)
}

/// `Codec.BOOL.optionalFieldOf(name, default)`.
fn bool_or(json: &Value, name: &str, default: bool) -> Result<bool, Error> {
    match json.get(name) {
        None => Ok(default),
        Some(value) => value
            .as_bool()
            .ok_or_else(|| Error::new(format!("placement: {name} is not a boolean: {value}"))),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use vanillite_base::random::LegacyRandom;

    use super::*;

    fn cuboid_count(edges: bool, interior: bool) -> usize {
        let cuboid = CuboidPlacement::parse(&json!({
            "xz_size": 2,
            "y_size": 2,
            "include_edges": edges,
            "include_interior": interior,
        }))
        .unwrap();
        let mut out = Vec::new();
        cuboid.modify(&mut LegacyRandom::new(0), BlockPos::new(1, 2, 3), &mut out);
        assert!(
            out.iter()
                .all(|p| (1..=3).contains(&p.x) && (2..=4).contains(&p.y))
        );
        out.len()
    }

    #[test]
    fn cuboid_edges_and_interior() {
        // A 3×3×3 box: 8 corners, 12 edge middles, 6 face centres, 1 centre.
        assert_eq!(cuboid_count(true, true), 27);
        assert_eq!(cuboid_count(true, false), 26);
        assert_eq!(cuboid_count(false, true), 7);
        assert_eq!(cuboid_count(false, false), 6);
    }

    #[test]
    fn heightmap_and_cave_surface_names() {
        assert_eq!(
            parse_heightmap(&json!("OCEAN_FLOOR_WG")).unwrap(),
            HeightmapKind::OceanFloorWg
        );
        assert!(parse_heightmap(&json!("ocean_floor")).is_err());
        let floor = CaveSurface::parse(&json!("floor")).unwrap();
        assert_eq!((floor.direction(), floor.y()), (Direction::Down, -1));
    }
}
