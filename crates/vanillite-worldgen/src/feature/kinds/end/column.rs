//! `stepped_column_cluster`: clusters of columns (basalt columns in the Nether) that get lower
//! away from the centre.

use serde_json::Value;
use vanillite_base::BlockPos;
use vanillite_base::random::RandomSource;

use crate::Error;
use crate::blocks::BlockSet;
use crate::carver::providers::IntProvider;
use crate::feature::kinds::vegetation::util::dist_manhattan;
use crate::feature::region::WorldGenRegion;
use crate::feature::{
    BlockPredicate, BlockStateProvider, FeatureContext, FeatureParser, FeatureType, field,
};

/// `stepped_column_cluster`.
#[derive(Debug)]
pub struct SteppedColumnClusterFeature {
    block: BlockStateProvider,
    continue_through: BlockPredicate,
    can_replace: BlockPredicate,
    cannot_place_on: BlockSet,
    cluster_reach: IntProvider,
    column_count: IntProvider,
    column_reach: IntProvider,
    height: IntProvider,
}

impl FeatureType for SteppedColumnClusterFeature {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        let int = |name: &str, min: i32, max: i32| {
            IntProvider::from_json_in_range(field(json, name)?, min, max)
        };
        Ok(Self {
            block: parser.state_provider(field(json, "block")?)?,
            continue_through: parser.block_predicate(field(json, "continue_through")?)?,
            can_replace: parser.block_predicate(field(json, "can_replace")?)?,
            cannot_place_on: parser.block_set(field(json, "cannot_place_on")?)?,
            cluster_reach: int("cluster_reach", 0, 13)?,
            column_count: int("column_count", 1, 150)?,
            column_reach: int("column_reach", 0, 3)?,
            height: int("height", 1, 10)?,
        })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        if !self.can_place_at(ctx.level, origin) {
            return false;
        }
        let column_height = self.height.sample(ctx.random);
        let cluster_reach = column_height.min(self.cluster_reach.sample(ctx.random));
        let count = self.column_count.sample(ctx.random);
        let width = 2 * cluster_reach + 1;
        let mut placed = false;
        // `BlockPos.randomBetweenClosed(random, count, ...)` over one layer: each position is
        // drawn (x, y, z) just before its iteration.
        for _ in 0..count {
            let x = origin.x - cluster_reach + ctx.random.next_int_bounded(width);
            let y = origin.y + ctx.random.next_int_bounded(1);
            let z = origin.z - cluster_reach + ctx.random.next_int_bounded(width);
            let pos = BlockPos::new(x, y, z);
            let blocks_to_place = column_height - dist_manhattan(pos, origin);
            if blocks_to_place >= 0 {
                let reach = self.column_reach.sample(ctx.random);
                placed |= self.place_column(ctx, pos, blocks_to_place, reach);
            }
        }
        placed
    }
}

impl SteppedColumnClusterFeature {
    /// `placeColumn`.
    fn place_column(
        &self,
        ctx: &mut FeatureContext<'_, '_>,
        origin: BlockPos,
        column_height: i32,
        reach: i32,
    ) -> bool {
        let mut placed_any = false;
        for z in origin.z - reach..=origin.z + reach {
            for x in origin.x - reach..=origin.x + reach {
                let pos = BlockPos::new(x, origin.y, z);
                let step_limit = dist_manhattan(pos, origin);
                let column = if self.can_replace.test(ctx.level, pos) {
                    self.find_surface(ctx.level, pos, step_limit)
                } else {
                    self.find_air(ctx.level, pos, step_limit)
                };
                let Some(mut cursor) = column else {
                    continue;
                };
                let mut blocks = column_height - step_limit / 2;
                while blocks >= 0 {
                    if self.can_replace.test(ctx.level, cursor) {
                        let state = self.block.get_state(ctx.level, ctx.random, cursor);
                        ctx.level.set_block_and_update(cursor, state);
                        cursor = cursor.offset(0, 1, 0);
                        placed_any = true;
                    } else {
                        if !self.continue_through.test(ctx.level, cursor) {
                            break;
                        }
                        cursor = cursor.offset(0, 1, 0);
                    }
                    blocks -= 1;
                }
            }
        }
        placed_any
    }

    /// `findSurface`: down to the first position the column can stand on.
    fn find_surface(
        &self,
        level: &WorldGenRegion<'_>,
        mut cursor: BlockPos,
        mut limit: i32,
    ) -> Option<BlockPos> {
        while cursor.y > level.min_y() + 1 && limit > 0 {
            limit -= 1;
            if self.can_place_at(level, cursor) {
                return Some(cursor);
            }
            cursor = cursor.offset(0, -1, 0);
        }
        None
    }

    /// `canPlaceAt`: replaceable, on a non-air block it may stand on.
    fn can_place_at(&self, level: &WorldGenRegion<'_>, pos: BlockPos) -> bool {
        if !self.can_replace.test(level, pos) {
            return false;
        }
        let below = level.block(pos.offset(0, -1, 0));
        !below.is_air() && !self.cannot_place_on.contains(below)
    }

    /// `findAir`: up to the first air, unless a block it may not stand on comes first.
    fn find_air(
        &self,
        level: &WorldGenRegion<'_>,
        mut cursor: BlockPos,
        mut limit: i32,
    ) -> Option<BlockPos> {
        while cursor.y <= level.max_y() && limit > 0 {
            limit -= 1;
            let state = level.block(cursor);
            if self.cannot_place_on.contains(state) {
                return None;
            }
            if state.is_air() {
                return Some(cursor);
            }
            cursor = cursor.offset(0, 1, 0);
        }
        None
    }
}
