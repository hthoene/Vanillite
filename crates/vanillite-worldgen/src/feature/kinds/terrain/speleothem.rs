//! Dripstone-like features: `SpeleothemFeature`, `SpeleothemClusterFeature` and
//! `LargeDripstoneFeature`, with the shared `SpeleothemUtils`.

use serde_json::Value;
use vanillite_base::math::{clamped_map, cos, d2i, floor, java_max, sin};
use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Direction};
use vanillite_chunk::HeightmapKind;
use vanillite_data::block::{BlockId, BlockState, ids};

use super::column::Column;
use super::{HORIZONTAL, direction_name, is, is_water, random_direction, state_field};
use crate::Error;
use crate::blocks::BlockSet;
use crate::carver::providers::{
    FloatProvider, IntProvider, normal, random_between, random_between_inclusive,
};
use crate::feature::region::{WorldGenRegion, flags};
use crate::feature::{
    FeatureContext, FeatureParser, FeatureType, field, float_field, int_field, int_or,
};
use crate::mth::{clamp_f, sqrt_f};

/// `SpeleothemThickness` property values.
const TIP_MERGE: &str = "tip_merge";
const TIP: &str = "tip";
const FRUSTUM: &str = "frustum";
const MIDDLE: &str = "middle";
const BASE: &str = "base";

/// The blocks a speleothem feature works with (`baseBlock`, `pointedBlock`,
/// `replaceableBlocks`).
#[derive(Debug)]
struct SpeleothemBlocks {
    base: BlockId,
    pointed: BlockId,
    replaceable: BlockSet,
}

impl SpeleothemBlocks {
    fn parse(json: &Value, parser: &FeatureParser<'_>) -> Result<Self, Error> {
        Ok(Self {
            base: state_field(json, "base_block")?.block(),
            pointed: state_field(json, "pointed_block")?.block(),
            replaceable: parser.block_set(field(json, "replaceable_blocks")?)?,
        })
    }

    /// `SpeleothemUtils.isBase`.
    fn is_base(&self, state: BlockState) -> bool {
        is(state, self.base) || self.replaceable.contains(state)
    }

    /// `SpeleothemUtils.placeBaseBlockIfPossible`.
    fn place_base_block_if_possible(&self, level: &mut WorldGenRegion<'_>, pos: BlockPos) -> bool {
        if self.replaceable.contains(level.block(pos)) {
            level.set_block(pos, self.base.default_state(), flags::CLIENTS);
            true
        } else {
            false
        }
    }

    /// `SpeleothemUtils.createPointedBlock`.
    fn pointed_block(&self, direction: Direction, thickness: &str) -> BlockState {
        let state = self.pointed.default_state();
        let state = state
            .with("vertical_direction", direction_name(direction))
            .unwrap_or(state);
        state.with("thickness", thickness).unwrap_or(state)
    }

    /// `SpeleothemUtils.growSpeleothem`: builds the column base to tip if the block behind the
    /// start is a base block.
    fn grow(
        &self,
        level: &mut WorldGenRegion<'_>,
        start: BlockPos,
        tip_direction: Direction,
        height: i32,
        merged_tip: bool,
    ) {
        if !self.is_base(level.block(start.relative(tip_direction.opposite()))) {
            return;
        }
        let mut pos = start;
        let mut place = |thickness: &str, level: &mut WorldGenRegion<'_>| {
            let mut state = self.pointed_block(tip_direction, thickness);
            if is(state, self.pointed) {
                let water = if is_water(level.fluid(pos)) {
                    "true"
                } else {
                    "false"
                };
                state = state.with("waterlogged", water).unwrap_or(state);
            }
            level.set_block(pos, state, flags::CLIENTS);
            pos = pos.relative(tip_direction);
        };
        // `buildBaseToTipColumn`
        if height >= 3 {
            place(BASE, level);
            for _ in 0..height - 3 {
                place(MIDDLE, level);
            }
        }
        if height >= 2 {
            place(FRUSTUM, level);
        }
        if height >= 1 {
            place(if merged_tip { TIP_MERGE } else { TIP }, level);
        }
    }
}

/// `SpeleothemUtils.isEmptyOrWater(state)`.
fn is_empty_or_water(state: BlockState) -> bool {
    state.is_air() || is(state, ids::WATER)
}

/// `SpeleothemUtils.isEmptyOrWaterOrLava(state)`.
fn is_empty_or_water_or_lava(state: BlockState) -> bool {
    state.is_air() || is(state, ids::WATER) || is(state, ids::LAVA)
}

/// `SpeleothemUtils.getSpeleothemHeight`.
fn speleothem_height(xz_distance: f64, radius: f64, scale: f64, bluntness: f64) -> f64 {
    let xz_distance = if xz_distance < bluntness {
        bluntness
    } else {
        xz_distance
    };
    let r = xz_distance / radius * 0.384;
    let part1 = 0.75 * r.powf(1.333_333_333_333_333_3);
    let part2 = r.powf(0.666_666_666_666_666_6);
    let part3 = 0.333_333_333_333_333_3 * r.ln();
    let height = java_max(scale * (part1 - part2 - part3), 0.0);
    height / 0.384 * radius
}

/// `SpeleothemUtils.isCircleMostlyEmbeddedInStone`.
fn is_circle_mostly_embedded_in_stone(
    level: &WorldGenRegion<'_>,
    center: BlockPos,
    radius: i32,
) -> bool {
    if is_empty_or_water_or_lava(level.block(center)) {
        return false;
    }
    let increment = 6.0f32 / radius as f32;
    let full_turn = (std::f64::consts::PI * 2.0) as f32;
    let mut angle = 0.0f32;
    while angle < full_turn {
        let dx = (cos(f64::from(angle)) * radius as f32) as i32;
        let dz = (sin(f64::from(angle)) * radius as f32) as i32;
        if is_empty_or_water_or_lava(level.block(center.offset(dx, 0, dz))) {
            return false;
        }
        angle += increment;
    }
    true
}

/// An optional float field with a default, checked against `[0, 1]`.
fn chance_or(json: &Value, name: &str, default: f32) -> Result<f32, Error> {
    let value = match json.get(name) {
        Some(_) => float_field(json, name)?,
        None => default,
    };
    if !(0.0..=1.0).contains(&value) {
        return Err(Error::new(format!("{name} {value} outside of [0, 1]")));
    }
    Ok(value)
}

/// An int field checked against a range.
fn int_in(json: &Value, name: &str, min: i32, max: i32) -> Result<i32, Error> {
    let value = int_field(json, name)?;
    if !(min..=max).contains(&value) {
        return Err(Error::new(format!(
            "{name} {value} outside of [{min}, {max}]"
        )));
    }
    Ok(value)
}

/// `speleothem`: a single pointed block (one or two long) on a patch of base blocks.
#[derive(Debug)]
pub struct SpeleothemFeature {
    blocks: SpeleothemBlocks,
    chance_of_taller_generation: f32,
    chance_of_directional_spread: f32,
    chance_of_spread_radius2: f32,
    chance_of_spread_radius3: f32,
}

impl FeatureType for SpeleothemFeature {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        Ok(Self {
            blocks: SpeleothemBlocks::parse(json, parser)?,
            chance_of_taller_generation: chance_or(json, "chance_of_taller_generation", 0.2)?,
            chance_of_directional_spread: chance_or(json, "chance_of_directional_spread", 0.7)?,
            chance_of_spread_radius2: chance_or(json, "chance_of_spread_radius2", 0.5)?,
            chance_of_spread_radius3: chance_or(json, "chance_of_spread_radius3", 0.5)?,
        })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let level = &mut *ctx.level;
        let random = &mut *ctx.random;
        // `getTipDirection`
        let above = self
            .blocks
            .is_base(level.block(origin.relative(Direction::Up)));
        let below = self
            .blocks
            .is_base(level.block(origin.relative(Direction::Down)));
        let tip = match (above, below) {
            (true, true) => {
                if random.next_boolean() {
                    Direction::Down
                } else {
                    Direction::Up
                }
            }
            (true, false) => Direction::Down,
            (false, true) => Direction::Up,
            (false, false) => return false,
        };
        let root = origin.relative(tip.opposite());
        self.create_patch_of_base_blocks(level, random, root);
        let height = if random.next_float() < self.chance_of_taller_generation
            && is_empty_or_water(level.block(origin.relative(tip)))
        {
            2
        } else {
            1
        };
        self.blocks.grow(level, origin, tip, height, false);
        true
    }
}

impl SpeleothemFeature {
    /// `createPatchOfBaseBlocks`.
    fn create_patch_of_base_blocks<R: RandomSource>(
        &self,
        level: &mut WorldGenRegion<'_>,
        random: &mut R,
        pos: BlockPos,
    ) {
        self.blocks.place_base_block_if_possible(level, pos);
        for direction in HORIZONTAL {
            if random.next_float() > self.chance_of_directional_spread {
                continue;
            }
            let pos1 = pos.relative(direction);
            self.blocks.place_base_block_if_possible(level, pos1);
            if random.next_float() > self.chance_of_spread_radius2 {
                continue;
            }
            let pos2 = pos1.relative(random_direction(random));
            self.blocks.place_base_block_if_possible(level, pos2);
            if random.next_float() > self.chance_of_spread_radius3 {
                continue;
            }
            let pos3 = pos2.relative(random_direction(random));
            self.blocks.place_base_block_if_possible(level, pos3);
        }
    }
}

/// `speleothem_cluster`: columns of stalactites and stalagmites (and pools) around the origin.
#[derive(Debug)]
pub struct SpeleothemClusterFeature {
    blocks: SpeleothemBlocks,
    floor_to_ceiling_search_range: i32,
    height: IntProvider,
    radius: IntProvider,
    max_stalagmite_stalactite_height_diff: i32,
    height_deviation: i32,
    speleothem_block_layer_thickness: IntProvider,
    density: FloatProvider,
    wetness: FloatProvider,
    chance_of_speleothem_at_max_distance_from_center: f32,
    max_distance_from_edge_affecting_chance_of_speleothem: i32,
    max_distance_from_center_affecting_height_bias: i32,
    base_stone_overworld: BlockSet,
}

impl FeatureType for SpeleothemClusterFeature {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        let chance = float_field(json, "chance_of_speleothem_at_max_distance_from_center")?;
        if !(0.0..=1.0).contains(&chance) {
            return Err(Error::new(
                "chance_of_speleothem_at_max_distance_from_center out of range",
            ));
        }
        Ok(Self {
            blocks: SpeleothemBlocks::parse(json, parser)?,
            floor_to_ceiling_search_range: int_in(json, "floor_to_ceiling_search_range", 1, 512)?,
            height: IntProvider::from_json_in_range(field(json, "height")?, 1, 128)?,
            radius: IntProvider::from_json_in_range(field(json, "radius")?, 1, 128)?,
            max_stalagmite_stalactite_height_diff: int_in(
                json,
                "max_stalagmite_stalactite_height_diff",
                0,
                64,
            )?,
            height_deviation: int_in(json, "height_deviation", 1, 64)?,
            speleothem_block_layer_thickness: IntProvider::from_json_in_range(
                field(json, "speleothem_block_layer_thickness")?,
                0,
                128,
            )?,
            density: FloatProvider::from_json_in_range(field(json, "density")?, 0.0, 2.0)?,
            wetness: FloatProvider::from_json_in_range(field(json, "wetness")?, 0.0, 2.0)?,
            chance_of_speleothem_at_max_distance_from_center: chance,
            max_distance_from_edge_affecting_chance_of_speleothem: int_in(
                json,
                "max_distance_from_edge_affecting_chance_of_speleothem",
                1,
                64,
            )?,
            max_distance_from_center_affecting_height_bias: int_in(
                json,
                "max_distance_from_center_affecting_height_bias",
                1,
                64,
            )?,
            base_stone_overworld: parser.block_tag("minecraft:base_stone_overworld"),
        })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let level = &mut *ctx.level;
        let random = &mut *ctx.random;
        if !is_empty_or_water(level.block(origin)) {
            return false;
        }
        let height = self.height.sample(random);
        let wetness = self.wetness.sample(random);
        let density = self.density.sample(random);
        let x_radius = self.radius.sample(random);
        let z_radius = self.radius.sample(random);
        for dx in -x_radius..=x_radius {
            for dz in -z_radius..=z_radius {
                let chance = self.chance_of_stalagmite_or_stalactite(x_radius, z_radius, dx, dz);
                let pos = origin.offset(dx, 0, dz);
                let column = ColumnSpot {
                    pos,
                    dx,
                    dz,
                    chance_of_water: wetness,
                    chance,
                    cluster_height: height,
                    density,
                };
                self.place_column(level, random, &column);
            }
        }
        true
    }
}

/// The per-column arguments of `SpeleothemClusterFeature.placeColumn`.
struct ColumnSpot {
    pos: BlockPos,
    dx: i32,
    dz: i32,
    chance_of_water: f32,
    chance: f64,
    cluster_height: i32,
    density: f32,
}

impl SpeleothemClusterFeature {
    /// `placeColumn`.
    fn place_column<R: RandomSource>(
        &self,
        level: &mut WorldGenRegion<'_>,
        random: &mut R,
        spot: &ColumnSpot,
    ) {
        let pos = spot.pos;
        let at_y = |y: i32| BlockPos::new(pos.x, y, pos.z);
        let Some(base_column) = Column::scan(
            level,
            pos,
            self.floor_to_ceiling_search_range,
            is_empty_or_water,
            |s| !is_empty_or_water(s),
        ) else {
            return;
        };
        let ceiling = base_column.ceiling;
        let base_floor = base_column.floor;
        if ceiling.is_none() && base_floor.is_none() {
            return;
        }
        let want_pool = random.next_float() < spot.chance_of_water;
        let column = match base_floor {
            Some(floor_y) if want_pool && self.can_place_pool(level, at_y(floor_y)) => {
                level.set_block(at_y(floor_y), ids::WATER.default_state(), flags::CLIENTS);
                Column {
                    floor: Some(floor_y - 1),
                    ceiling,
                }
            }
            _ => base_column,
        };
        let floor = column.floor;

        let want_stalactite = random.next_double() < spot.chance;
        let stalactite_height = match ceiling {
            Some(ceiling_y) if want_stalactite && !is(level.block(at_y(ceiling_y)), ids::LAVA) => {
                let thickness = self.speleothem_block_layer_thickness.sample(random);
                self.replace_blocks_with_base_blocks(
                    level,
                    at_y(ceiling_y),
                    thickness,
                    Direction::Up,
                );
                let max_height = match floor {
                    Some(floor_y) => spot.cluster_height.min(ceiling_y - floor_y),
                    None => spot.cluster_height,
                };
                self.speleothem_height(random, spot.dx, spot.dz, spot.density, max_height)
            }
            _ => 0,
        };

        let want_stalagmite = random.next_double() < spot.chance;
        let stalagmite_height = match floor {
            Some(floor_y) if want_stalagmite && !is(level.block(at_y(floor_y)), ids::LAVA) => {
                let thickness = self.speleothem_block_layer_thickness.sample(random);
                self.replace_blocks_with_base_blocks(
                    level,
                    at_y(floor_y),
                    thickness,
                    Direction::Down,
                );
                if ceiling.is_some() {
                    let diff = self.max_stalagmite_stalactite_height_diff;
                    0.max(stalactite_height + random_between_inclusive(random, -diff, diff))
                } else {
                    self.speleothem_height(
                        random,
                        spot.dx,
                        spot.dz,
                        spot.density,
                        spot.cluster_height,
                    )
                }
            }
            _ => 0,
        };

        let (actual_stalactite, actual_stalagmite) = match (ceiling, floor) {
            (Some(ceiling_y), Some(floor_y))
                if ceiling_y - stalactite_height <= floor_y + stalagmite_height =>
            {
                let lowest_stalactite_bottom = (ceiling_y - stalactite_height).max(floor_y + 1);
                let highest_stalagmite_top = (floor_y + stalagmite_height).min(ceiling_y - 1);
                let bottom = random_between_inclusive(
                    random,
                    lowest_stalactite_bottom,
                    highest_stalagmite_top + 1,
                );
                (ceiling_y - bottom, bottom - 1 - floor_y)
            }
            _ => (stalactite_height, stalagmite_height),
        };

        let merge_tips = random.next_boolean()
            && actual_stalactite > 0
            && actual_stalagmite > 0
            && column.height() == Some(actual_stalactite + actual_stalagmite);
        if let Some(ceiling_y) = ceiling {
            self.blocks.grow(
                level,
                at_y(ceiling_y - 1),
                Direction::Down,
                actual_stalactite,
                merge_tips,
            );
        }
        if let Some(floor_y) = floor {
            self.blocks.grow(
                level,
                at_y(floor_y + 1),
                Direction::Up,
                actual_stalagmite,
                merge_tips,
            );
        }
    }

    /// `getSpeleothemHeight`.
    fn speleothem_height<R: RandomSource>(
        &self,
        random: &mut R,
        dx: i32,
        dz: i32,
        density: f32,
        max_height: i32,
    ) -> i32 {
        if random.next_float() > density {
            return 0;
        }
        let distance = dx.abs() + dz.abs();
        let mean = clamped_map(
            f64::from(distance),
            0.0,
            f64::from(self.max_distance_from_center_affecting_height_bias),
            f64::from(max_height) / 2.0,
            0.0,
        ) as f32;
        // `randomBetweenBiased` = `ClampedNormalFloat.sample(random, mean, deviation, 0, max)`
        let value = normal(random, mean, self.height_deviation as f32);
        clamp_f(value, 0.0, max_height as f32) as i32
    }

    /// `canPlacePool`.
    fn can_place_pool(&self, level: &WorldGenRegion<'_>, pos: BlockPos) -> bool {
        let state = level.block(pos);
        if is(state, ids::WATER) || is(state, self.blocks.base) || is(state, self.blocks.pointed) {
            return false;
        }
        if is_water(level.fluid(pos.relative(Direction::Up))) {
            return false;
        }
        HORIZONTAL
            .iter()
            .all(|&d| self.can_be_adjacent_to_water(level, pos.relative(d)))
            && self.can_be_adjacent_to_water(level, pos.relative(Direction::Down))
    }

    /// `canBeAdjacentToWater`.
    fn can_be_adjacent_to_water(&self, level: &WorldGenRegion<'_>, pos: BlockPos) -> bool {
        let state = level.block(pos);
        self.base_stone_overworld.contains(state) || is_water(state.fluid_state())
    }

    /// `replaceBlocksWithBaseBlocks`.
    fn replace_blocks_with_base_blocks(
        &self,
        level: &mut WorldGenRegion<'_>,
        first: BlockPos,
        max_count: i32,
        direction: Direction,
    ) {
        let mut pos = first;
        for _ in 0..max_count {
            if !self.blocks.place_base_block_if_possible(level, pos) {
                return;
            }
            pos = pos.relative(direction);
        }
    }

    /// `getChanceOfStalagmiteOrStalactite`: `Mth.clampedMap` in `float`.
    fn chance_of_stalagmite_or_stalactite(
        &self,
        x_radius: i32,
        z_radius: i32,
        dx: i32,
        dz: i32,
    ) -> f64 {
        let distance_from_edge = (x_radius - dx.abs()).min(z_radius - dz.abs());
        let from_max = self.max_distance_from_edge_affecting_chance_of_speleothem as f32;
        let to_min = self.chance_of_speleothem_at_max_distance_from_center;
        let factor = (distance_from_edge as f32 - 0.0) / (from_max - 0.0);
        let chance = if factor < 0.0 {
            to_min
        } else if factor > 1.0 {
            1.0
        } else {
            to_min + factor * (1.0 - to_min)
        };
        f64::from(chance)
    }
}

/// `large_dripstone`: a big stalactite and stalagmite pair, optionally bent by wind.
#[derive(Debug)]
pub struct LargeDripstoneFeature {
    replaceable_blocks: BlockSet,
    floor_to_ceiling_search_range: i32,
    column_radius: IntProvider,
    height_scale: FloatProvider,
    max_column_radius_to_cave_height_ratio: f32,
    stalactite_bluntness: FloatProvider,
    stalagmite_bluntness: FloatProvider,
    wind_speed: FloatProvider,
    min_radius_for_wind: i32,
    min_bluntness_for_wind: f32,
    base_stone_overworld: BlockSet,
}

impl FeatureType for LargeDripstoneFeature {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        let search_range = int_or(json, "floor_to_ceiling_search_range", 30);
        if !(1..=512).contains(&search_range) {
            return Err(Error::new(
                "floor_to_ceiling_search_range outside of [1, 512]",
            ));
        }
        let ratio = float_field(json, "max_column_radius_to_cave_height_ratio")?;
        let min_bluntness_for_wind = float_field(json, "min_bluntness_for_wind")?;
        if !(0.1..=1.0).contains(&ratio) || !(0.0..=5.0).contains(&min_bluntness_for_wind) {
            return Err(Error::new("large_dripstone: value out of range"));
        }
        Ok(Self {
            replaceable_blocks: parser.block_set(field(json, "replaceable_blocks")?)?,
            floor_to_ceiling_search_range: search_range,
            column_radius: IntProvider::from_json_in_range(field(json, "column_radius")?, 1, 16)?,
            height_scale: FloatProvider::from_json_in_range(
                field(json, "height_scale")?,
                0.0,
                20.0,
            )?,
            max_column_radius_to_cave_height_ratio: ratio,
            stalactite_bluntness: FloatProvider::from_json_in_range(
                field(json, "stalactite_bluntness")?,
                0.1,
                10.0,
            )?,
            stalagmite_bluntness: FloatProvider::from_json_in_range(
                field(json, "stalagmite_bluntness")?,
                0.1,
                10.0,
            )?,
            wind_speed: FloatProvider::from_json_in_range(field(json, "wind_speed")?, 0.0, 2.0)?,
            min_radius_for_wind: int_in(json, "min_radius_for_wind", 0, 100)?,
            min_bluntness_for_wind,
            base_stone_overworld: parser.block_tag("minecraft:base_stone_overworld"),
        })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let level = &mut *ctx.level;
        let random = &mut *ctx.random;
        if !is_empty_or_water(level.block(origin)) {
            return false;
        }
        let replaceable = &self.replaceable_blocks;
        let column = Column::scan(
            level,
            origin,
            self.floor_to_ceiling_search_range,
            is_empty_or_water,
            |s| is(s, ids::DRIPSTONE_BLOCK) || replaceable.contains(s) || is(s, ids::LAVA),
        );
        let Some(Column {
            floor: Some(floor_y),
            ceiling: Some(ceiling_y),
        }) = column
        else {
            return false;
        };
        let column_height = ceiling_y - floor_y - 1;
        if column_height < 4 {
            return false;
        }
        let by_height = (column_height as f32 * self.max_column_radius_to_cave_height_ratio) as i32;
        let (min_radius, max_radius) = (
            self.column_radius.min_inclusive(),
            self.column_radius.max_inclusive(),
        );
        // `Mth.clamp(int, int, int)`
        let max_column_radius = by_height.max(min_radius).min(max_radius);
        let radius = random_between_inclusive(random, min_radius, max_column_radius);
        let mut stalactite = LargeDripstone::new(
            BlockPos::new(origin.x, ceiling_y - 1, origin.z),
            false,
            random,
            radius,
            &self.stalactite_bluntness,
            &self.height_scale,
        );
        let mut stalagmite = LargeDripstone::new(
            BlockPos::new(origin.x, floor_y + 1, origin.z),
            true,
            random,
            radius,
            &self.stalagmite_bluntness,
            &self.height_scale,
        );
        let wind = if stalactite
            .is_suitable_for_wind(self.min_radius_for_wind, self.min_bluntness_for_wind)
            && stalagmite
                .is_suitable_for_wind(self.min_radius_for_wind, self.min_bluntness_for_wind)
        {
            WindOffsetter::new(origin.y, random, &self.wind_speed, 16 - radius)
        } else {
            WindOffsetter::NONE
        };
        let stalactite_embedded = stalactite.move_back_until_base_is_inside_stone(level, &wind);
        let stalagmite_embedded = stalagmite.move_back_until_base_is_inside_stone(level, &wind);
        if stalactite_embedded {
            stalactite.place_blocks(level, random, &wind, &self.base_stone_overworld);
        }
        if stalagmite_embedded {
            stalagmite.place_blocks(level, random, &wind, &self.base_stone_overworld);
        }
        true
    }
}

/// `LargeDripstoneFeature.LargeDripstone`.
struct LargeDripstone {
    root: BlockPos,
    pointing_up: bool,
    radius: i32,
    bluntness: f64,
    scale: f64,
}

impl LargeDripstone {
    /// `makeDripstone`.
    fn new<R: RandomSource>(
        root: BlockPos,
        pointing_up: bool,
        random: &mut R,
        radius: i32,
        bluntness: &FloatProvider,
        height_scale: &FloatProvider,
    ) -> Self {
        let bluntness = f64::from(bluntness.sample(random));
        let scale = f64::from(height_scale.sample(random));
        Self {
            root,
            pointing_up,
            radius,
            bluntness,
            scale,
        }
    }

    /// `getHeightAtRadius`.
    fn height_at_radius(&self, check_radius: f32) -> i32 {
        d2i(speleothem_height(
            f64::from(check_radius),
            f64::from(self.radius),
            self.scale,
            self.bluntness,
        ))
    }

    /// `moveBackUntilBaseIsInsideStoneAndShrinkRadiusIfNecessary`.
    fn move_back_until_base_is_inside_stone(
        &mut self,
        level: &WorldGenRegion<'_>,
        wind: &WindOffsetter,
    ) -> bool {
        while self.radius > 1 {
            let mut new_root = self.root;
            let max_tries = self.height_at_radius(0.0).min(10);
            for _ in 0..max_tries {
                if is(level.block(new_root), ids::LAVA) {
                    return false;
                }
                if is_circle_mostly_embedded_in_stone(level, wind.offset(new_root), self.radius) {
                    self.root = new_root;
                    return true;
                }
                new_root = new_root.relative(if self.pointing_up {
                    Direction::Down
                } else {
                    Direction::Up
                });
            }
            self.radius /= 2;
        }
        false
    }

    /// `placeBlocks`.
    fn place_blocks<R: RandomSource>(
        &self,
        level: &mut WorldGenRegion<'_>,
        random: &mut R,
        wind: &WindOffsetter,
        base_stone_overworld: &BlockSet,
    ) {
        let dripstone = ids::DRIPSTONE_BLOCK.default_state();
        let step = if self.pointing_up {
            Direction::Up
        } else {
            Direction::Down
        };
        for dx in -self.radius..=self.radius {
            for dz in -self.radius..=self.radius {
                let current_radius = sqrt_f((dx * dx + dz * dz) as f32);
                if current_radius > self.radius as f32 {
                    continue;
                }
                let mut height = self.height_at_radius(current_radius);
                if height <= 0 {
                    continue;
                }
                if f64::from(random.next_float()) < 0.2 {
                    height = (height as f32 * random_between(random, 0.8, 1.0)) as i32;
                }
                let mut pos = self.root.offset(dx, 0, dz);
                let mut has_been_out_of_stone = false;
                let max_y = if self.pointing_up {
                    level.height_at(HeightmapKind::WorldSurfaceWg, pos.x, pos.z)
                } else {
                    i32::MAX
                };
                let mut i = 0;
                while i < height && pos.y < max_y {
                    let wind_pos = wind.offset(pos);
                    let state = level.block(wind_pos);
                    if is_empty_or_water_or_lava(state) {
                        has_been_out_of_stone = true;
                        level.set_block(wind_pos, dripstone, flags::CLIENTS);
                    } else if has_been_out_of_stone && base_stone_overworld.contains(state) {
                        break;
                    }
                    pos = pos.relative(step);
                    i += 1;
                }
            }
        }
    }

    /// `isSuitableForWind`.
    fn is_suitable_for_wind(&self, min_radius: i32, min_bluntness: f32) -> bool {
        self.radius >= min_radius && self.bluntness >= f64::from(min_bluntness)
    }
}

/// `LargeDripstoneFeature.WindOffsetter`.
struct WindOffsetter {
    origin_y: i32,
    /// `windSpeed` (x, z); `None` without wind.
    wind_speed: Option<(f64, f64)>,
    max_offset: i32,
}

impl WindOffsetter {
    /// `WindOffsetter.noWind()`.
    const NONE: Self = Self {
        origin_y: 0,
        wind_speed: None,
        max_offset: 0,
    };

    fn new<R: RandomSource>(
        origin_y: i32,
        random: &mut R,
        wind_speed: &FloatProvider,
        max_offset: i32,
    ) -> Self {
        let speed = wind_speed.sample(random);
        let direction = random_between(random, 0.0, std::f32::consts::PI);
        Self {
            origin_y,
            wind_speed: Some((
                f64::from(cos(f64::from(direction)) * speed),
                f64::from(sin(f64::from(direction)) * speed),
            )),
            max_offset,
        }
    }

    /// `offset(pos)`.
    fn offset(&self, pos: BlockPos) -> BlockPos {
        let Some((wind_x, wind_z)) = self.wind_speed else {
            return pos;
        };
        let dy = f64::from(self.origin_y - pos.y);
        // `Mth.clamp(int, int, int)`
        let clamp = |v: i32| v.max(-self.max_offset).min(self.max_offset);
        let dx = clamp(floor(wind_x * dy));
        let dz = clamp(floor(wind_z * dy));
        pos.offset(dx, 0, dz)
    }
}
