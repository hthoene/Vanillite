//! `simple_block` with the special placements it delegates to: `DoublePlantBlock.placeAt` and
//! `MossyCarpetBlock.placeAt`.

use serde_json::Value;
use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockId, BlockState, ids};

use super::multiface::can_attach_at;
use super::util::{HORIZONTAL, bool_value, is_water, with, with_bool};
use crate::Error;
use crate::behaviour::{can_be_replaced, can_survive};
use crate::feature::region::WorldGenRegion;
use crate::feature::{
    BlockStateProvider, FeatureContext, FeatureParser, FeatureType, bool_or, field,
};

/// `simple_block`: one block from a provider if it can survive; double plants get both halves,
/// pale moss carpets their side growth.
#[derive(Debug)]
pub struct SimpleBlockFeature {
    to_place: BlockStateProvider,
    schedule_tick: bool,
}

impl FeatureType for SimpleBlockFeature {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        Ok(Self {
            to_place: parser.state_provider(field(json, "to_place")?)?,
            schedule_tick: bool_or(json, "schedule_tick", false),
        })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let Some(state) = self
            .to_place
            .get_optional_state(ctx.level, ctx.random, origin)
        else {
            return false;
        };
        if !can_survive(ctx.level, state, origin) {
            return false;
        }
        let block = state.block();
        if is_double_plant(block) {
            let above = ctx.level.block(origin.offset(0, 1, 0));
            if !above.is_air()
                && (state.fluid_state() != above.fluid_state() || !can_be_replaced(above))
            {
                return false;
            }
            place_double_plant(ctx.level, state, origin, 2);
        } else if block == ids::PALE_MOSS_CARPET {
            place_mossy_carpet(ctx.level, origin, 2);
        } else {
            ctx.level.set_block(origin, state, 2);
        }
        if self.schedule_tick {
            let placed = ctx.level.block(origin).block();
            ctx.level.schedule_block_tick(origin, placed);
        }
        true
    }
}

/// `block instanceof DoublePlantBlock`: tall flowers and grasses, tall seagrass, small
/// dripleaf and the pitcher plant and crop.
fn is_double_plant(block: BlockId) -> bool {
    matches!(
        block,
        ids::TALL_SEAGRASS
            | ids::SUNFLOWER
            | ids::LILAC
            | ids::ROSE_BUSH
            | ids::PEONY
            | ids::TALL_GRASS
            | ids::LARGE_FERN
            | ids::PITCHER_CROP
            | ids::PITCHER_PLANT
            | ids::SMALL_DRIPLEAF
    )
}

/// `DoublePlantBlock.copyWaterloggedFrom(level, pos, state)`.
fn copy_waterlogged_from(
    level: &WorldGenRegion<'_>,
    pos: BlockPos,
    state: BlockState,
) -> BlockState {
    if state.value("waterlogged").is_some() {
        with_bool(state, "waterlogged", is_water(level.fluid(pos)))
    } else {
        state
    }
}

/// `DoublePlantBlock.placeAt(level, state, lowerPos, updateType)`.
pub(crate) fn place_double_plant(
    level: &mut WorldGenRegion<'_>,
    state: BlockState,
    lower: BlockPos,
    update_flags: i32,
) {
    let upper = lower.offset(0, 1, 0);
    let lower_state = copy_waterlogged_from(level, lower, with(state, "half", "lower"));
    level.set_block(lower, lower_state, update_flags);
    let upper_state = copy_waterlogged_from(level, upper, with(state, "half", "upper"));
    level.set_block(upper, upper_state, update_flags);
}

/// `MossyCarpetBlock.PROPERTY_BY_DIRECTION` for a horizontal direction.
fn wall_property(direction: Direction) -> &'static str {
    match direction {
        Direction::North => "north",
        Direction::East => "east",
        Direction::South => "south",
        _ => "west",
    }
}

/// `MossyCarpetBlock.BASE` (`bottom`).
fn is_base(state: BlockState) -> bool {
    bool_value(state, "bottom")
}

/// `MossyCarpetBlock.hasFaces`.
fn has_carpet_faces(state: BlockState) -> bool {
    is_base(state)
        || HORIZONTAL
            .iter()
            .any(|&d| state.value(wall_property(d)) != Some("none"))
}

/// `MossyCarpetBlock.getUpdatedState(state, level, pos, createSides)`.
fn updated_carpet_state(
    level: &WorldGenRegion<'_>,
    mut state: BlockState,
    pos: BlockPos,
    create_sides: bool,
) -> BlockState {
    let mut above: Option<BlockState> = None;
    let mut below: Option<BlockState> = None;
    let create_sides = create_sides || is_base(state);
    for direction in HORIZONTAL {
        let property = wall_property(direction);
        // `canSupportAtFace`: never for up, which is not horizontal anyway.
        let mut side = if can_attach_at(level, pos, direction) {
            if create_sides {
                "low"
            } else {
                state.value(property).unwrap_or("none")
            }
        } else {
            "none"
        };
        if side == "low" {
            let above = *above.get_or_insert_with(|| level.block(pos.offset(0, 1, 0)));
            if above.block() == ids::PALE_MOSS_CARPET
                && above.value(property) != Some("none")
                && !is_base(above)
            {
                side = "tall";
            }
            if !is_base(state) {
                let below = *below.get_or_insert_with(|| level.block(pos.offset(0, -1, 0)));
                if below.block() == ids::PALE_MOSS_CARPET && below.value(property) == Some("none") {
                    side = "none";
                }
            }
        }
        state = with(state, property, side);
    }
    state
}

/// `MossyCarpetBlock.createTopperWithSideChance(level, pos, random::nextBoolean)` with the
/// region's own random (`level.getRandom()`).
fn carpet_topper(level: &mut WorldGenRegion<'_>, pos: BlockPos) -> BlockState {
    let above = pos.offset(0, 1, 0);
    let previous = level.block(above);
    let carpet_above = previous.block() == ids::PALE_MOSS_CARPET;
    if (carpet_above && is_base(previous)) || (!carpet_above && !can_be_replaced(previous)) {
        return BlockState::AIR;
    }
    let no_base = with_bool(ids::PALE_MOSS_CARPET.default_state(), "bottom", false);
    let mut state = updated_carpet_state(level, no_base, above, true);
    let random = level.random();
    for direction in HORIZONTAL {
        let property = wall_property(direction);
        if state.value(property) != Some("none") && !random.next_boolean() {
            state = with(state, property, "none");
        }
    }
    if has_carpet_faces(state) && state != previous {
        state
    } else {
        BlockState::AIR
    }
}

/// `MossyCarpetBlock.placeAt(level, pos, level.getRandom(), updateType)`.
pub(crate) fn place_mossy_carpet(level: &mut WorldGenRegion<'_>, pos: BlockPos, update_flags: i32) {
    let carpet = ids::PALE_MOSS_CARPET.default_state();
    let adjusted = updated_carpet_state(level, carpet, pos, true);
    level.set_block(pos, adjusted, update_flags);
    let topper = carpet_topper(level, pos);
    if !topper.is_air() {
        level.set_block(pos.offset(0, 1, 0), topper, update_flags);
        let bottom = updated_carpet_state(level, adjusted, pos, true);
        level.set_block(pos, bottom, update_flags);
    }
}
