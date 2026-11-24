//! Blob shaped features: `BlockBlobFeature`, `ReplaceBlobsFeature` and `DiskFeature`.

use serde_json::Value;
use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockId, BlockState};

use super::{mark_above_for_post_processing, state_field};
use crate::Error;
use crate::carver::providers::IntProvider;
use crate::feature::region::{WorldGenRegion, flags};
use crate::feature::{
    BlockPredicate, BlockStateProvider, FeatureContext, FeatureParser, FeatureType, field,
    int_field,
};

/// `block_blob`: three overlapping small blobs sinking from the origin onto a valid block.
#[derive(Debug)]
pub struct BlockBlobFeature {
    state: BlockState,
    can_place_on: BlockPredicate,
}

impl FeatureType for BlockBlobFeature {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        Ok(Self {
            state: state_field(json, "state")?,
            can_place_on: parser.block_predicate(field(json, "can_place_on")?)?,
        })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let min_y = ctx.level.min_y();
        let mut origin = origin;
        while origin.y > min_y + 3
            && !self
                .can_place_on
                .test(ctx.level, origin.relative(Direction::Down))
        {
            origin = origin.relative(Direction::Down);
        }
        if origin.y <= min_y + 3 {
            return false;
        }
        for _ in 0..3 {
            let xr = ctx.random.next_int_bounded(2);
            let yr = ctx.random.next_int_bounded(2);
            let zr = ctx.random.next_int_bounded(2);
            let tr = (xr + yr + zr) as f32 * 0.333 + 0.5;
            let limit = f64::from(tr * tr);
            for z in -zr..=zr {
                for y in -yr..=yr {
                    for x in -xr..=xr {
                        let pos = origin.offset(x, y, z);
                        if pos.dist_sqr(origin) <= limit {
                            ctx.level.set_block_and_update(pos, self.state);
                        }
                    }
                }
            }
            let dx = -1 + ctx.random.next_int_bounded(2);
            let dy = -ctx.random.next_int_bounded(2);
            let dz = -1 + ctx.random.next_int_bounded(2);
            origin = origin.offset(dx, dy, dz);
        }
        true
    }
}

/// `netherrack_replace_blobs`: replaces a target block in a Manhattan-distance blob around the
/// first target found below the origin.
#[derive(Debug)]
pub struct ReplaceBlobsFeature {
    target: BlockId,
    state: BlockState,
    radius: IntProvider,
}

impl FeatureType for ReplaceBlobsFeature {
    fn parse(json: &Value, _parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        Ok(Self {
            target: state_field(json, "target")?.block(),
            state: state_field(json, "state")?,
            radius: IntProvider::from_json_in_range(field(json, "radius")?, 0, 12)?,
        })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let level = &mut *ctx.level;
        let start_y = origin.y.max(level.min_y() + 1).min(level.max_y());
        let Some(center) = find_target(
            level,
            BlockPos::new(origin.x, start_y, origin.z),
            self.target,
        ) else {
            return false;
        };
        let radius_x = self.radius.sample(ctx.random);
        let radius_y = self.radius.sample(ctx.random);
        let radius_z = self.radius.sample(ctx.random);
        let maximum = radius_x.max(radius_y.max(radius_z));
        let mut replaced_any = false;
        for pos in ManhattanBox::new(center, radius_x, radius_y, radius_z) {
            let distance =
                (pos.x - center.x).abs() + (pos.y - center.y).abs() + (pos.z - center.z).abs();
            if distance > maximum {
                break;
            }
            if level.block(pos).block() == self.target {
                level.set_block_and_update(pos, self.state);
                replaced_any = true;
            }
        }
        replaced_any
    }
}

/// `ReplaceBlobsFeature.findTarget`: the first target block at or below the cursor, above
/// `minY + 1`.
fn find_target(level: &WorldGenRegion<'_>, cursor: BlockPos, target: BlockId) -> Option<BlockPos> {
    let mut cursor = cursor;
    while cursor.y > level.min_y() + 1 {
        if level.block(cursor).block() == target {
            return Some(cursor);
        }
        cursor = cursor.relative(Direction::Down);
    }
    None
}

/// `BlockPos.withinBoxByManhattanDistance(origin, reachX, reachY, reachZ)`: the positions of
/// the box in order of Manhattan distance, in Vanilla's iteration order.
struct ManhattanBox {
    origin: BlockPos,
    reach: [i32; 3],
    max_depth: i32,
    depth: i32,
    max_x: i32,
    max_y: i32,
    x: i32,
    y: i32,
    /// The z of the last position when its mirror (`-z`) comes next.
    mirror: Option<i32>,
}

impl ManhattanBox {
    fn new(origin: BlockPos, reach_x: i32, reach_y: i32, reach_z: i32) -> Self {
        Self {
            origin,
            reach: [reach_x, reach_y, reach_z],
            max_depth: reach_x + reach_y + reach_z,
            depth: 0,
            max_x: 0,
            max_y: 0,
            x: 0,
            y: 0,
            mirror: None,
        }
    }
}

impl Iterator for ManhattanBox {
    type Item = BlockPos;

    fn next(&mut self) -> Option<BlockPos> {
        if let Some(z) = self.mirror.take() {
            // `this.y` already moved on; the mirrored position uses the previous y.
            return Some(BlockPos::new(
                self.origin.x + self.x,
                self.origin.y + self.y - 1,
                self.origin.z - z,
            ));
        }
        loop {
            if self.y > self.max_y {
                self.x += 1;
                if self.x > self.max_x {
                    self.depth += 1;
                    if self.depth > self.max_depth {
                        return None;
                    }
                    self.max_x = self.reach[0].min(self.depth);
                    self.x = -self.max_x;
                }
                self.max_y = self.reach[1].min(self.depth - self.x.abs());
                self.y = -self.max_y;
            }
            let (xx, yy) = (self.x, self.y);
            let zz = self.depth - xx.abs() - yy.abs();
            self.y += 1;
            if zz <= self.reach[2] {
                if zz != 0 {
                    self.mirror = Some(zz);
                }
                return Some(BlockPos::new(
                    self.origin.x + xx,
                    self.origin.y + yy,
                    self.origin.z + zz,
                ));
            }
        }
    }
}

/// `disk`: replaces target blocks in a vertical cylinder.
#[derive(Debug)]
pub struct DiskFeature {
    state_provider: BlockStateProvider,
    target: BlockPredicate,
    radius: IntProvider,
    half_height: i32,
}

impl FeatureType for DiskFeature {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        let half_height = int_field(json, "half_height")?;
        if !(0..=4).contains(&half_height) {
            return Err(Error::new(format!(
                "disk half_height {half_height} outside of [0, 4]"
            )));
        }
        Ok(Self {
            state_provider: parser.state_provider(field(json, "state_provider")?)?,
            target: parser.block_predicate(field(json, "target")?)?,
            radius: IntProvider::from_json_in_range(field(json, "radius")?, 0, 8)?,
            half_height,
        })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let mut placed_any = false;
        let top = origin.y + self.half_height;
        let bottom = origin.y - self.half_height - 1;
        let r = self.radius.sample(ctx.random);
        for z in origin.z - r..=origin.z + r {
            for x in origin.x - r..=origin.x + r {
                let (xd, zd) = (x - origin.x, z - origin.z);
                if xd * xd + zd * zd <= r * r {
                    placed_any |= self.place_column(ctx, top, bottom, x, z);
                }
            }
        }
        placed_any
    }
}

impl DiskFeature {
    /// `placeColumn`.
    fn place_column(
        &self,
        ctx: &mut FeatureContext<'_, '_>,
        top: i32,
        bottom: i32,
        x: i32,
        z: i32,
    ) -> bool {
        let mut placed_any = false;
        let mut placed_above = false;
        for y in (bottom + 1..=top).rev() {
            let pos = BlockPos::new(x, y, z);
            if self.target.test(ctx.level, pos) {
                if let Some(state) = self
                    .state_provider
                    .get_optional_state(ctx.level, ctx.random, pos)
                {
                    ctx.level.set_block(pos, state, flags::CLIENTS);
                    if !placed_above {
                        mark_above_for_post_processing(ctx.level, pos);
                    }
                    placed_any = true;
                    placed_above = true;
                }
            } else {
                placed_above = false;
            }
        }
        placed_any
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manhattan_box_order() {
        let positions: Vec<_> = ManhattanBox::new(BlockPos::ZERO, 1, 1, 1).collect();
        let expected = [
            (0, 0, 0),
            (-1, 0, 0),
            (0, -1, 0),
            (0, 0, 1),
            (0, 0, -1),
            (0, 1, 0),
            (1, 0, 0),
        ];
        assert_eq!(
            &positions[..7],
            expected.map(|(x, y, z)| BlockPos::new(x, y, z)).as_slice()
        );
        assert_eq!(positions.len(), 27);
        let mut sorted = positions.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 27);
    }
}
