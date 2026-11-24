//! `RootSystemFeature`: a tree on the surface above a column of rooted blocks and hanging roots.

use serde_json::Value;
use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Direction};
use vanillite_chunk::HeightmapKind;
use vanillite_data::block::BlockState;

use super::{is_lava, is_water};
use crate::Error;
use crate::behaviour::{SupportType, can_survive, is_face_sturdy, is_solid};
use crate::blocks::BlockSet;
use crate::feature::region::{WorldGenRegion, flags};
use crate::feature::{
    BlockPredicate, BlockStateProvider, FeatureContext, FeatureParser, FeatureType,
    PlacedFeatureId, field, int_field,
};

/// `Direction.from2DDataValue(0..4)`: south, west, north, east.
const BY_2D_DATA: [Direction; 4] = [
    Direction::South,
    Direction::West,
    Direction::North,
    Direction::East,
];

/// `root_system`.
#[derive(Debug)]
pub struct RootSystemFeature {
    tree_feature: PlacedFeatureId,
    required_vertical_space_for_tree: i32,
    level_test_distance: i32,
    max_level_deviation: i32,
    root_radius: i32,
    root_replaceable: BlockSet,
    root_state_provider: BlockStateProvider,
    root_placement_attempts: i32,
    root_column_max_height: i32,
    hanging_root_radius: i32,
    hanging_roots_vertical_span: i32,
    hanging_root_state_provider: BlockStateProvider,
    hanging_root_placement_attempts: i32,
    allowed_vertical_water_for_tree: i32,
    allowed_tree_position: BlockPredicate,
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

impl FeatureType for RootSystemFeature {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        Ok(Self {
            tree_feature: parser.placed(field(json, "feature")?)?,
            required_vertical_space_for_tree: int_in(
                json,
                "required_vertical_space_for_tree",
                1,
                64,
            )?,
            level_test_distance: int_in(json, "level_test_distance", 0, 16)?,
            max_level_deviation: int_in(json, "max_level_deviation", 0, 64)?,
            root_radius: int_in(json, "root_radius", 1, 64)?,
            root_replaceable: parser.block_set(field(json, "root_replaceable")?)?,
            root_state_provider: parser.state_provider(field(json, "root_state_provider")?)?,
            root_placement_attempts: int_in(json, "root_placement_attempts", 1, 256)?,
            root_column_max_height: int_in(json, "root_column_max_height", 1, 4096)?,
            hanging_root_radius: int_in(json, "hanging_root_radius", 1, 64)?,
            hanging_roots_vertical_span: int_in(json, "hanging_roots_vertical_span", 1, 16)?,
            hanging_root_state_provider: parser
                .state_provider(field(json, "hanging_root_state_provider")?)?,
            hanging_root_placement_attempts: int_in(
                json,
                "hanging_root_placement_attempts",
                1,
                256,
            )?,
            allowed_vertical_water_for_tree: int_in(
                json,
                "allowed_vertical_water_for_tree",
                1,
                64,
            )?,
            allowed_tree_position: parser.block_predicate(field(json, "allowed_tree_position")?)?,
        })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        if !ctx.level.block(origin).is_air() {
            return false;
        }
        if self.place_dirt_and_tree(ctx, origin) {
            self.place_roots(ctx.level, ctx.random, origin);
        }
        true
    }
}

impl RootSystemFeature {
    /// `spaceForTree`.
    fn space_for_tree(&self, level: &WorldGenRegion<'_>, pos: BlockPos) -> bool {
        for i in 1..=self.required_vertical_space_for_tree {
            let state = level.block(pos.offset(0, i, 0));
            if !is_allowed_tree_space(state, i, self.allowed_vertical_water_for_tree) {
                return false;
            }
        }
        if self.level_test_distance > 0 {
            for direction in BY_2D_DATA {
                let [dx, _, dz] = direction.normal();
                let corner = pos.offset(
                    dx * self.level_test_distance,
                    0,
                    dz * self.level_test_distance,
                );
                let below = level.block(corner.offset(0, -self.max_level_deviation, 0));
                let above = level.block(corner.offset(0, self.max_level_deviation, 0));
                if below.is_air() || !above.is_air() {
                    return false;
                }
            }
        }
        true
    }

    /// `placeDirtAndTree`.
    fn place_dirt_and_tree(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let mut working = origin;
        for y in 0..self.root_column_max_height {
            working = working.relative(Direction::Up);
            if ctx
                .level
                .height_at(HeightmapKind::WorldSurface, working.x, working.z)
                < working.y
            {
                return false;
            }
            if self.allowed_tree_position.test(ctx.level, working)
                && self.space_for_tree(ctx.level, working)
            {
                let below = working.relative(Direction::Down);
                if is_lava(ctx.level.fluid(below)) || !is_solid(ctx.level.block(below)) {
                    return false;
                }
                if ctx.place_placed(self.tree_feature, working) {
                    self.place_dirt(ctx.level, ctx.random, origin, origin.y + y);
                    return true;
                }
            }
        }
        false
    }

    /// `placeDirt`.
    fn place_dirt<R: RandomSource>(
        &self,
        level: &mut WorldGenRegion<'_>,
        random: &mut R,
        origin: BlockPos,
        target_height: i32,
    ) {
        for y in origin.y..target_height {
            self.place_rooted_dirt(level, random, BlockPos::new(origin.x, y, origin.z));
        }
    }

    /// `placeRootedDirt`.
    fn place_rooted_dirt<R: RandomSource>(
        &self,
        level: &mut WorldGenRegion<'_>,
        random: &mut R,
        column: BlockPos,
    ) {
        let r = self.root_radius;
        for _ in 0..self.root_placement_attempts {
            let dx = random.next_int_bounded(r) - random.next_int_bounded(r);
            let dz = random.next_int_bounded(r) - random.next_int_bounded(r);
            let pos = column.offset(dx, 0, dz);
            if self.root_replaceable.contains(level.block(pos)) {
                let state = self.root_state_provider.get_state(level, random, pos);
                level.set_block(pos, state, flags::CLIENTS);
            }
        }
    }

    /// `placeRoots`.
    fn place_roots<R: RandomSource>(
        &self,
        level: &mut WorldGenRegion<'_>,
        random: &mut R,
        origin: BlockPos,
    ) {
        let (r, span) = (self.hanging_root_radius, self.hanging_roots_vertical_span);
        for _ in 0..self.hanging_root_placement_attempts {
            let dx = random.next_int_bounded(r) - random.next_int_bounded(r);
            let dy = random.next_int_bounded(span) - random.next_int_bounded(span);
            let dz = random.next_int_bounded(r) - random.next_int_bounded(r);
            let pos = origin.offset(dx, dy, dz);
            if !level.is_empty_block(pos) {
                continue;
            }
            let state = self
                .hanging_root_state_provider
                .get_state(level, random, pos);
            if can_survive(level, state, pos)
                && is_face_sturdy(
                    level.block(pos.relative(Direction::Up)),
                    Direction::Down,
                    SupportType::Full,
                )
            {
                level.set_block(pos, state, flags::CLIENTS);
            }
        }
    }
}

/// `isAllowedTreeSpace`.
fn is_allowed_tree_space(state: BlockState, blocks_above_origin: i32, allowed_water: i32) -> bool {
    if state.is_air() {
        return true;
    }
    let blocks_above_ground = blocks_above_origin + 1;
    blocks_above_ground <= allowed_water && is_water(state.fluid_state())
}
