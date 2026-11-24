//! `delta_feature`: basalt delta pools of a fluid with a rim.

use serde_json::Value;
use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockId, BlockState, ids};

use crate::Error;
use crate::blocks::parse_block_state;
use crate::carver::providers::IntProvider;
use crate::feature::kinds::vegetation::util::dist_manhattan;
use crate::feature::region::WorldGenRegion;
use crate::feature::{FeatureContext, FeatureParser, FeatureType, field};

/// `DeltaFeature.CANNOT_REPLACE`.
const CANNOT_REPLACE: [BlockId; 7] = [
    ids::BEDROCK,
    ids::NETHER_BRICKS,
    ids::NETHER_BRICK_FENCE,
    ids::NETHER_BRICK_STAIRS,
    ids::NETHER_WART,
    ids::CHEST,
    ids::SPAWNER,
];

/// `delta_feature`.
#[derive(Debug)]
pub struct DeltaFeature {
    contents: BlockState,
    rim: BlockState,
    size: IntProvider,
    rim_size: IntProvider,
}

impl FeatureType for DeltaFeature {
    fn parse(json: &Value, _parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        Ok(Self {
            contents: parse_block_state(field(json, "contents")?)?,
            rim: parse_block_state(field(json, "rim")?)?,
            size: IntProvider::from_json_in_range(field(json, "size")?, 0, 16)?,
            rim_size: IntProvider::from_json_in_range(field(json, "rim_size")?, 0, 16)?,
        })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let mut any_placed = false;
        let spawn_rim = ctx.random.next_double() < 0.9;
        let rim_x = if spawn_rim {
            self.rim_size.sample(ctx.random)
        } else {
            0
        };
        let rim_z = if spawn_rim {
            self.rim_size.sample(ctx.random)
        } else {
            0
        };
        let has_rim = spawn_rim && rim_x != 0 && rim_z != 0;
        let radius_x = self.size.sample(ctx.random);
        let radius_z = self.size.sample(ctx.random);
        let radius_limit = radius_x.max(radius_z);
        for pos in ManhattanOrdered::new(origin, radius_x, 0, radius_z, radius_x + radius_z) {
            if dist_manhattan(pos, origin) > radius_limit {
                break;
            }
            if self.is_clear(ctx.level, pos) {
                if has_rim {
                    any_placed = true;
                    ctx.level.set_block_and_update(pos, self.rim);
                }
                let offset = pos.offset(rim_x, 0, rim_z);
                if self.is_clear(ctx.level, offset) {
                    any_placed = true;
                    ctx.level.set_block_and_update(offset, self.contents);
                }
            }
        }
        any_placed
    }
}

impl DeltaFeature {
    /// `DeltaFeature.isClear`: replaceable, air above and solid on the other five sides.
    fn is_clear(&self, level: &WorldGenRegion<'_>, pos: BlockPos) -> bool {
        let state = level.block(pos);
        if state.block() == self.contents.block() || CANNOT_REPLACE.contains(&state.block()) {
            return false;
        }
        Direction::ALL.into_iter().all(|d| {
            let is_air = level.block(pos.relative(d)).is_air();
            is_air == (d == Direction::Up)
        })
    }
}

/// `BlockPos.manhattanOrdered(origin, reachX, reachY, reachZ, maxDepth)`: the positions of a box
/// by increasing Manhattan distance, in Vanilla's order (`withinBoxByManhattanDistance` passes
/// `reachX + reachY + reachZ` as the depth).
#[derive(Debug)]
pub(crate) struct ManhattanOrdered {
    origin: BlockPos,
    reach: [i32; 3],
    max_depth: i32,
    current_depth: i32,
    max_x: i32,
    max_y: i32,
    x: i32,
    y: i32,
    /// The last position returned, mirrored next in z when `z_mirror` is set.
    cursor: BlockPos,
    z_mirror: bool,
    done: bool,
}

impl ManhattanOrdered {
    pub(crate) fn new(
        origin: BlockPos,
        reach_x: i32,
        reach_y: i32,
        reach_z: i32,
        max_depth: i32,
    ) -> Self {
        Self {
            origin,
            reach: [reach_x, reach_y, reach_z],
            max_depth,
            current_depth: 0,
            max_x: 0,
            max_y: 0,
            x: 0,
            y: 0,
            cursor: origin,
            z_mirror: false,
            done: false,
        }
    }
}

impl Iterator for ManhattanOrdered {
    type Item = BlockPos;

    fn next(&mut self) -> Option<BlockPos> {
        if self.done {
            return None;
        }
        if self.z_mirror {
            self.z_mirror = false;
            self.cursor.z = self.origin.z - (self.cursor.z - self.origin.z);
            return Some(self.cursor);
        }
        let [reach_x, reach_y, reach_z] = self.reach;
        loop {
            if self.y > self.max_y {
                self.x += 1;
                if self.x > self.max_x {
                    self.current_depth += 1;
                    if self.current_depth > self.max_depth {
                        // Guava's `AbstractIterator` never computes again after the end.
                        self.done = true;
                        return None;
                    }
                    self.max_x = reach_x.min(self.current_depth);
                    self.x = -self.max_x;
                }
                self.max_y = reach_y.min(self.current_depth - self.x.abs());
                self.y = -self.max_y;
            }
            let (xx, yy) = (self.x, self.y);
            self.y += 1;
            let zz = self.current_depth - xx.abs() - yy.abs();
            if zz <= reach_z {
                self.z_mirror = zz != 0;
                self.cursor =
                    BlockPos::new(self.origin.x + xx, self.origin.y + yy, self.origin.z + zz);
                return Some(self.cursor);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manhattan_order_matches_vanilla() {
        let positions: Vec<(i32, i32, i32)> = ManhattanOrdered::new(BlockPos::ZERO, 1, 0, 1, 2)
            .map(|p| (p.x, p.y, p.z))
            .collect();
        assert_eq!(
            positions,
            [
                (0, 0, 0),
                (-1, 0, 0),
                (0, 0, 1),
                (0, 0, -1),
                (1, 0, 0),
                (-1, 0, 1),
                (-1, 0, -1),
                (1, 0, 1),
                (1, 0, -1),
            ]
        );
    }
}
