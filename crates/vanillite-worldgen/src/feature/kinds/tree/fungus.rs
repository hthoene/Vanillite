//! Huge nether fungi (`HugeFungusFeature`).

use serde_json::Value;
use vanillite_base::BlockPos;
use vanillite_base::random::{DecorationRandom, RandomSource};
use vanillite_data::block::{BlockState, ids};

use super::try_set_int;
use crate::Error;
use crate::behaviour;
use crate::blocks::parse_block_state;
use crate::carver::providers::next_int;
use crate::feature::region::WorldGenRegion;
use crate::feature::{BlockPredicate, FeatureContext, FeatureParser, FeatureType, bool_or, field};

/// `HugeFungusFeature.HUGE_PROBABILITY`.
const HUGE_PROBABILITY: f32 = 0.06;
/// `Block.UPDATE_INVISIBLE | Block.UPDATE_SKIP_SHAPE_UPDATE_ON_WIRE`, as the origin is cleared.
const CLEAR_ORIGIN_FLAGS: i32 = 260;
/// `Block.UPDATE_CLIENTS`.
const UPDATE_CLIENTS: i32 = 2;

/// `minecraft:huge_fungus` (`HugeFungusFeature`).
#[derive(Debug)]
pub struct HugeFungusFeature {
    valid_base_state: BlockState,
    stem_state: BlockState,
    hat_state: BlockState,
    decor_state: BlockState,
    replaceable_blocks: BlockPredicate,
    planted: bool,
}

impl FeatureType for HugeFungusFeature {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        Ok(Self {
            valid_base_state: parse_block_state(field(json, "valid_base_block")?)?,
            stem_state: parse_block_state(field(json, "stem_state")?)?,
            hat_state: parse_block_state(field(json, "hat_state")?)?,
            decor_state: parse_block_state(field(json, "decor_state")?)?,
            replaceable_blocks: parser.block_predicate(field(json, "replaceable_blocks")?)?,
            planted: bool_or(json, "planted", false),
        })
    }

    /// `HugeFungusFeature.place`.
    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let below = ctx.level.block(origin.offset(0, -1, 0));
        if below.block() != self.valid_base_state.block() {
            return false;
        }
        let mut total_height = next_int(ctx.random, 4, 13);
        if ctx.random.next_int_bounded(12) == 0 {
            total_height *= 2;
        }
        if !self.planted {
            // `chunkGenerator.getGenDepth()`.
            let max_height = ctx.env.context.gen_depth();
            if origin.y + total_height + 1 >= max_height {
                return false;
            }
        }
        let is_huge = !self.planted && ctx.random.next_float() < HUGE_PROBABILITY;
        ctx.level
            .set_block(origin, BlockState::AIR, CLEAR_ORIGIN_FLAGS);
        self.place_stem(ctx.level, ctx.random, origin, total_height, is_huge);
        self.place_hat(ctx.level, ctx.random, origin, total_height, is_huge);
        true
    }
}

impl HugeFungusFeature {
    /// `isReplaceable(level, pos, checkNonReplaceablePlants)`.
    fn is_replaceable(
        &self,
        level: &WorldGenRegion<'_>,
        pos: BlockPos,
        check_plants: bool,
    ) -> bool {
        behaviour::can_be_replaced(level.block(pos))
            || check_plants && self.replaceable_blocks.test(level, pos)
    }

    /// `placeStem`.
    fn place_stem(
        &self,
        level: &mut WorldGenRegion<'_>,
        random: &mut DecorationRandom,
        origin: BlockPos,
        total_height: i32,
        is_huge: bool,
    ) {
        let stem_radius = i32::from(is_huge);
        for dx in -stem_radius..=stem_radius {
            for dz in -stem_radius..=stem_radius {
                let corner = is_huge && dx.abs() == stem_radius && dz.abs() == stem_radius;
                for dy in 0..total_height {
                    let pos = origin.offset(dx, dy, dz);
                    if !self.is_replaceable(level, pos, true) {
                        continue;
                    }
                    if self.planted {
                        if !level.block(pos.offset(0, -1, 0)).is_air() {
                            level.destroy_block(pos);
                        }
                        level.set_block_and_update(pos, self.stem_state);
                    } else if corner {
                        if random.next_float() < 0.1 {
                            level.set_block_and_update(pos, self.stem_state);
                        }
                    } else {
                        level.set_block_and_update(pos, self.stem_state);
                    }
                }
            }
        }
    }

    /// `placeHat`.
    fn place_hat(
        &self,
        level: &mut WorldGenRegion<'_>,
        random: &mut DecorationRandom,
        origin: BlockPos,
        total_height: i32,
        is_huge: bool,
    ) {
        let place_vines = self.hat_state.block() == ids::NETHER_WART_BLOCK;
        let hat_height = (random.next_int_bounded(1 + total_height / 3) + 5).min(total_height);
        let hat_start_y = total_height - hat_height;
        for dy in hat_start_y..=total_height {
            let mut radius = if dy < total_height - random.next_int_bounded(3) {
                2
            } else {
                1
            };
            if hat_height > 8 && dy < hat_start_y + 4 {
                radius = 3;
            }
            if is_huge {
                radius += 1;
            }
            for dx in -radius..=radius {
                for dz in -radius..=radius {
                    let edge_x = dx == -radius || dx == radius;
                    let edge_z = dz == -radius || dz == radius;
                    let inside = !edge_x && !edge_z && dy != total_height;
                    let corner = edge_x && edge_z;
                    let hat_bottom = dy < hat_start_y + 3;
                    let pos = origin.offset(dx, dy, dz);
                    if !self.is_replaceable(level, pos, false) {
                        continue;
                    }
                    if self.planted && !level.block(pos.offset(0, -1, 0)).is_air() {
                        level.destroy_block(pos);
                    }
                    if hat_bottom {
                        if !inside {
                            self.place_hat_drop_block(level, random, pos, place_vines);
                        }
                    } else if inside {
                        let vines = if place_vines { 0.1 } else { 0.0 };
                        self.place_hat_block(level, random, pos, 0.1, 0.2, vines);
                    } else if corner {
                        let vines = if place_vines { 0.083 } else { 0.0 };
                        self.place_hat_block(level, random, pos, 0.01, 0.7, vines);
                    } else {
                        let vines = if place_vines { 0.07 } else { 0.0 };
                        self.place_hat_block(level, random, pos, 5.0E-4, 0.98, vines);
                    }
                }
            }
        }
    }

    /// `placeHatBlock`.
    fn place_hat_block(
        &self,
        level: &mut WorldGenRegion<'_>,
        random: &mut DecorationRandom,
        pos: BlockPos,
        decor_probability: f32,
        hat_probability: f32,
        vines_probability: f32,
    ) {
        if random.next_float() < decor_probability {
            level.set_block_and_update(pos, self.decor_state);
        } else if random.next_float() < hat_probability {
            level.set_block_and_update(pos, self.hat_state);
            if random.next_float() < vines_probability {
                try_place_weeping_vines(level, random, pos);
            }
        }
    }

    /// `placeHatDropBlock`.
    fn place_hat_drop_block(
        &self,
        level: &mut WorldGenRegion<'_>,
        random: &mut DecorationRandom,
        pos: BlockPos,
        place_vines: bool,
    ) {
        if level.block(pos.offset(0, -1, 0)).block() == self.hat_state.block() {
            level.set_block_and_update(pos, self.hat_state);
        } else if f64::from(random.next_float()) < 0.15 {
            level.set_block_and_update(pos, self.hat_state);
            if place_vines && random.next_int_bounded(11) == 0 {
                try_place_weeping_vines(level, random, pos);
            }
        }
    }
}

/// `HugeFungusFeature.tryPlaceWeepingVines`.
fn try_place_weeping_vines(
    level: &mut WorldGenRegion<'_>,
    random: &mut DecorationRandom,
    hat: BlockPos,
) {
    let pos = hat.offset(0, -1, 0);
    if !level.is_empty_block(pos) {
        return;
    }
    let mut goal_height = next_int(random, 1, 5);
    if random.next_int_bounded(7) == 0 {
        goal_height *= 2;
    }
    // `placeWeepingVinesColumn(level, random, pos, goalHeight, 23, 25)`.
    let mut pos = pos;
    for height in 0..=goal_height {
        if level.is_empty_block(pos) {
            if height == goal_height || !level.is_empty_block(pos.offset(0, -1, 0)) {
                let age = next_int(random, 23, 25);
                let head = try_set_int(ids::WEEPING_VINES.default_state(), "age", age);
                level.set_block(pos, head, UPDATE_CLIENTS);
                break;
            }
            level.set_block(
                pos,
                ids::WEEPING_VINES_PLANT.default_state(),
                UPDATE_CLIENTS,
            );
        }
        pos = pos.offset(0, -1, 0);
    }
}
