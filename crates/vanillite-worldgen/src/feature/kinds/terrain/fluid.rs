//! Fluid related features: `LakeFeature`, `SpringFeature` and `UnderwaterMagmaFeature`.

use serde_json::Value;
use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockState, flags as state_flags, ids};
use vanillite_data::fluid::FluidState;

use super::column::Column;
use super::{
    Climates, HORIZONTAL, is, is_face_full, is_water, mark_above_for_post_processing,
    parse_fluid_state, schedule_fluid_tick,
};
use crate::Error;
use crate::behaviour::is_solid;
use crate::blocks::BlockSet;
use crate::feature::region::{WorldGenRegion, flags};
use crate::feature::{
    BlockPredicate, BlockStateProvider, FeatureContext, FeatureParser, FeatureType, bool_or, field,
    float_field, int_field, int_or,
};

/// `lake` (deprecated in Vanilla, still used by lava lakes).
#[derive(Debug)]
pub struct LakeFeature {
    fluid: BlockStateProvider,
    barrier: BlockStateProvider,
    can_place_feature: BlockPredicate,
    can_replace_with_air_or_fluid: BlockPredicate,
    can_replace_with_barrier: BlockPredicate,
    climates: Climates,
}

/// The lake shape: 16 × 16 × 8 cells, index `(x * 16 + z) * 8 + y`.
struct LakeGrid([bool; 2048]);

impl LakeGrid {
    #[inline]
    fn get(&self, x: i32, z: i32, y: i32) -> bool {
        self.0[((x * 16 + z) * 8 + y) as usize]
    }

    /// Whether a cell outside the lake touches the lake (the `check` of `place`).
    fn is_edge(&self, x: i32, z: i32, y: i32) -> bool {
        !self.get(x, z, y)
            && (x < 15 && self.get(x + 1, z, y)
                || x > 0 && self.get(x - 1, z, y)
                || z < 15 && self.get(x, z + 1, y)
                || z > 0 && self.get(x, z - 1, y)
                || y < 7 && self.get(x, z, y + 1)
                || y > 0 && self.get(x, z, y - 1))
    }
}

impl FeatureType for LakeFeature {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        Ok(Self {
            fluid: parser.state_provider(field(json, "fluid")?)?,
            barrier: parser.state_provider(field(json, "barrier")?)?,
            can_place_feature: parser.block_predicate(field(json, "can_place_feature")?)?,
            can_replace_with_air_or_fluid: parser
                .block_predicate(field(json, "can_replace_with_air_or_fluid")?)?,
            can_replace_with_barrier: parser
                .block_predicate(field(json, "can_replace_with_barrier")?)?,
            climates: Climates::new(parser),
        })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let level = &mut *ctx.level;
        let random = &mut *ctx.random;
        if origin.y <= level.min_y() + 4 {
            return false;
        }
        let origin = origin.offset(-8, -4, -8);
        let mut grid = LakeGrid([false; 2048]);
        let spots = random.next_int_bounded(4) + 4;
        for _ in 0..spots {
            let xr = random.next_double() * 6.0 + 3.0;
            let yr = random.next_double() * 4.0 + 2.0;
            let zr = random.next_double() * 6.0 + 3.0;
            let xp = random.next_double() * (16.0 - xr - 2.0) + 1.0 + xr / 2.0;
            let yp = random.next_double() * (8.0 - yr - 4.0) + 2.0 + yr / 2.0;
            let zp = random.next_double() * (16.0 - zr - 2.0) + 1.0 + zr / 2.0;
            for xx in 1..15i32 {
                for zz in 1..15i32 {
                    for yy in 1..7i32 {
                        let xd = (f64::from(xx) - xp) / (xr / 2.0);
                        let yd = (f64::from(yy) - yp) / (yr / 2.0);
                        let zd = (f64::from(zz) - zp) / (zr / 2.0);
                        if xd * xd + yd * yd + zd * zd < 1.0 {
                            grid.0[((xx * 16 + zz) * 8 + yy) as usize] = true;
                        }
                    }
                }
            }
        }

        let fluid = self.fluid.get_state(level, random, origin);
        for xx in 0..16 {
            for zz in 0..16 {
                for yy in 0..8 {
                    if !grid.is_edge(xx, zz, yy) {
                        continue;
                    }
                    let pos = origin.offset(xx, yy, zz);
                    let state = level.block(pos);
                    if yy >= 4 && state.has(state_flags::LIQUID) {
                        return false;
                    }
                    if yy < 4 && !is_solid(state) && state != fluid {
                        return false;
                    }
                    if !self.can_place_feature.test(level, pos) {
                        return false;
                    }
                }
            }
        }

        let cave_air = ids::CAVE_AIR.default_state();
        for xx in 0..16 {
            for zz in 0..16 {
                for yy in 0..8 {
                    if !grid.get(xx, zz, yy) {
                        continue;
                    }
                    let pos = origin.offset(xx, yy, zz);
                    if self.can_replace_with_air_or_fluid.test(level, pos) {
                        let place_air = yy >= 4;
                        level.set_block(
                            pos,
                            if place_air { cave_air } else { fluid },
                            flags::CLIENTS,
                        );
                        if place_air {
                            level.schedule_block_tick(pos, ids::CAVE_AIR);
                            mark_above_for_post_processing(level, pos);
                        }
                    }
                }
            }
        }

        let barrier = self.barrier.get_state(level, random, origin);
        if !barrier.is_air() {
            for xx in 0..16 {
                for zz in 0..16 {
                    for yy in 0..8 {
                        if !(grid.is_edge(xx, zz, yy)
                            && (yy < 4 || random.next_int_bounded(2) != 0))
                        {
                            continue;
                        }
                        let pos = origin.offset(xx, yy, zz);
                        if is_solid(level.block(pos))
                            && self.can_replace_with_barrier.test(level, pos)
                        {
                            level.set_block(pos, barrier, flags::CLIENTS);
                            mark_above_for_post_processing(level, pos);
                        }
                    }
                }
            }
        }

        if is_water(fluid.fluid_state()) {
            let ice = ids::ICE.default_state();
            for xx in 0..16 {
                for zz in 0..16 {
                    let pos = origin.offset(xx, 4, zz);
                    if self.climates.should_freeze(level, pos, pos)
                        && self.can_replace_with_air_or_fluid.test(level, pos)
                    {
                        level.set_block(pos, ice, flags::CLIENTS);
                    }
                }
            }
        }
        true
    }
}

/// `spring_feature`: a single fluid source in a wall with the configured numbers of rock and
/// open neighbours.
#[derive(Debug)]
pub struct SpringFeature {
    state: FluidState,
    requires_block_below: bool,
    rock_count: i32,
    hole_count: i32,
    valid_blocks: BlockSet,
}

impl FeatureType for SpringFeature {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        Ok(Self {
            state: parse_fluid_state(field(json, "state")?)?,
            requires_block_below: bool_or(json, "requires_block_below", true),
            rock_count: int_or(json, "rock_count", 4),
            hole_count: int_or(json, "hole_count", 1),
            valid_blocks: parser.block_set(field(json, "valid_blocks")?)?,
        })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let level = &mut *ctx.level;
        let valid = |pos: BlockPos| self.valid_blocks.contains(level.block(pos));
        if !valid(origin.relative(Direction::Up)) {
            return false;
        }
        if self.requires_block_below && !valid(origin.relative(Direction::Down)) {
            return false;
        }
        let current = level.block(origin);
        if !current.is_air() && !self.valid_blocks.contains(current) {
            return false;
        }
        const SIDES: [Direction; 5] = [
            Direction::West,
            Direction::East,
            Direction::North,
            Direction::South,
            Direction::Down,
        ];
        let rocks = SIDES.iter().filter(|&&d| valid(origin.relative(d))).count();
        let holes = SIDES
            .iter()
            .filter(|&&d| level.is_empty_block(origin.relative(d)))
            .count();
        if rocks as i32 == self.rock_count && holes as i32 == self.hole_count {
            level.set_block(origin, self.state.block_state(), flags::CLIENTS);
            schedule_fluid_tick(level, origin, self.state);
            return true;
        }
        false
    }
}

/// `underwater_magma`: magma blocks scattered on the floor below water, where they are not
/// visible from outside.
#[derive(Debug)]
pub struct UnderwaterMagmaFeature {
    floor_search_range: i32,
    placement_radius_around_floor: i32,
    placement_probability_per_valid_position: f32,
}

impl FeatureType for UnderwaterMagmaFeature {
    fn parse(json: &Value, _parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        let floor_search_range = int_field(json, "floor_search_range")?;
        let radius = int_field(json, "placement_radius_around_floor")?;
        let probability = float_field(json, "placement_probability_per_valid_position")?;
        if !(0..=512).contains(&floor_search_range)
            || !(0..=64).contains(&radius)
            || !(0.0..=1.0).contains(&probability)
        {
            return Err(Error::new("underwater_magma: value out of range"));
        }
        Ok(Self {
            floor_search_range,
            placement_radius_around_floor: radius,
            placement_probability_per_valid_position: probability,
        })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let level = &mut *ctx.level;
        let water = |s: BlockState| is(s, ids::WATER);
        let Some(floor_y) =
            Column::scan(level, origin, self.floor_search_range, water, |s| !water(s))
                .and_then(|c| c.floor)
        else {
            return false;
        };
        let r = self.placement_radius_around_floor;
        let magma = ids::MAGMA_BLOCK.default_state();
        let mut placed = 0;
        for z in origin.z - r..=origin.z + r {
            for y in floor_y - r..=floor_y + r {
                for x in origin.x - r..=origin.x + r {
                    let pos = BlockPos::new(x, y, z);
                    if ctx.random.next_float() < self.placement_probability_per_valid_position
                        && is_valid_magma_placement(level, pos)
                    {
                        level.set_block(pos, magma, flags::CLIENTS);
                        placed += 1;
                    }
                }
            }
        }
        placed > 0
    }
}

/// `UnderwaterMagmaFeature.isValidPlacement`.
fn is_valid_magma_placement(level: &WorldGenRegion<'_>, pos: BlockPos) -> bool {
    let state = level.block(pos);
    if is(state, ids::WATER)
        || state.is_air()
        || is_visible_from_outside(level, pos.relative(Direction::Down), Direction::Up)
    {
        return false;
    }
    !HORIZONTAL
        .iter()
        .any(|&d| is_visible_from_outside(level, pos.relative(d), d.opposite()))
}

/// `isVisibleFromOutside`: the state's face occlusion shape on `covered` is not the full face.
fn is_visible_from_outside(level: &WorldGenRegion<'_>, pos: BlockPos, covered: Direction) -> bool {
    !occlusion_face_full(level.block(pos), covered)
}

/// `Block.isShapeFullBlock(state.getFaceOcclusionShape(direction))`.
fn occlusion_face_full(state: BlockState, direction: Direction) -> bool {
    if state.has(state_flags::SOLID_RENDER) {
        return true;
    }
    if !state.has(state_flags::CAN_OCCLUDE) {
        return false;
    }
    is_face_full(state.occlusion_shape(), direction)
}
