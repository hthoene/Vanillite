//! Fallen trees (`FallenTreeFeature`): a stump and a log lying next to it.

use serde_json::Value;
use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Direction};

use super::decorator::{self, DecoratorContext, TreeDecorator};
use super::{
    Axis, JavaHashSet, TreeBlocks, mark_above_for_post_processing, random_horizontal, relative,
    with_axis,
};
use crate::Error;
use crate::behaviour::{self, SupportType};
use crate::carver::providers::IntProvider;
use crate::feature::region::WorldGenRegion;
use crate::feature::{BlockStateProvider, FeatureContext, FeatureParser, FeatureType, field};

/// `minecraft:fallen_tree` (`FallenTreeFeature`).
#[derive(Debug)]
pub struct FallenTreeFeature {
    trunk_provider: BlockStateProvider,
    log_length: IntProvider,
    stump_decorators: Vec<TreeDecorator>,
    log_decorators: Vec<TreeDecorator>,
    blocks: TreeBlocks,
}

impl FeatureType for FallenTreeFeature {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        Ok(Self {
            trunk_provider: parser.state_provider(field(json, "trunk_provider")?)?,
            log_length: IntProvider::from_json_in_range(field(json, "log_length")?, 0, 16)?,
            stump_decorators: decorator::parse_list(field(json, "stump_decorators")?, parser)?,
            log_decorators: decorator::parse_list(field(json, "log_decorators")?, parser)?,
            blocks: TreeBlocks::new(parser),
        })
    }

    /// `FallenTreeFeature.place` (`placeFallenTree`).
    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        // `placeStump`.
        let stump = self.place_log_block(ctx, origin, None);
        let mut stump_set = JavaHashSet::new();
        stump_set.insert(stump);
        self.decorate_logs(ctx, &stump_set, &self.stump_decorators);

        let direction = random_horizontal(ctx.random);
        let log_length = self.log_length.sample(ctx.random) - 2;
        let distance = 2 + ctx.random.next_int_bounded(2);
        let mut start = relative(origin, direction, distance);
        self.set_ground_height(ctx.level, &mut start);
        if self.can_place_entire_fallen_log(ctx.level, log_length, start, direction) {
            self.place_fallen_log(ctx, log_length, start, direction);
        }
        true
    }
}

impl FallenTreeFeature {
    /// `setGroundHeightForFallenLogStartPos`.
    fn set_ground_height(&self, level: &WorldGenRegion<'_>, start: &mut BlockPos) {
        *start = start.offset(0, 1, 0);
        for _ in 0..6 {
            if self.may_place_on(level, *start) {
                return;
            }
            *start = start.offset(0, -1, 0);
        }
    }

    /// `canPlaceEntireFallenLog`.
    fn can_place_entire_fallen_log(
        &self,
        level: &WorldGenRegion<'_>,
        log_length: i32,
        start: BlockPos,
        direction: Direction,
    ) -> bool {
        let mut gap_in_ground = 0;
        let mut pos = start;
        for _ in 0..log_length {
            if !self.blocks.valid_tree_pos(level, pos) {
                return false;
            }
            if !is_over_solid_ground(level, pos) {
                gap_in_ground += 1;
                if gap_in_ground > 2 {
                    return false;
                }
            } else {
                gap_in_ground = 0;
            }
            pos = pos.relative(direction);
        }
        true
    }

    /// `placeFallenLog`.
    fn place_fallen_log(
        &self,
        ctx: &mut FeatureContext<'_, '_>,
        log_length: i32,
        start: BlockPos,
        direction: Direction,
    ) {
        let mut fallen_log = JavaHashSet::new();
        let mut pos = start;
        for _ in 0..log_length {
            fallen_log.insert(self.place_log_block(ctx, pos, Some(Axis::of(direction))));
            pos = pos.relative(direction);
        }
        self.decorate_logs(ctx, &fallen_log, &self.log_decorators);
    }

    /// `mayPlaceOn`.
    fn may_place_on(&self, level: &WorldGenRegion<'_>, pos: BlockPos) -> bool {
        self.blocks.valid_tree_pos(level, pos) && is_over_solid_ground(level, pos)
    }

    /// `placeLogBlock`: the trunk state (turned sideways for the fallen log) with
    /// `setBlockAndUpdate`, then `markAboveForPostProcessing`.
    fn place_log_block(
        &self,
        ctx: &mut FeatureContext<'_, '_>,
        pos: BlockPos,
        sideways: Option<Axis>,
    ) -> BlockPos {
        let state = self.trunk_provider.get_state(ctx.level, ctx.random, pos);
        let state = match sideways {
            Some(axis) => with_axis(state, axis),
            None => state,
        };
        ctx.level.set_block_and_update(pos, state);
        mark_above_for_post_processing(ctx.level, pos);
        pos
    }

    /// `decorateLogs`: runs decorators over `logs` with a setter that records nothing.
    fn decorate_logs(
        &self,
        ctx: &mut FeatureContext<'_, '_>,
        logs: &JavaHashSet,
        decorators: &[TreeDecorator],
    ) {
        if decorators.is_empty() {
            return;
        }
        let mut context = DecoratorContext::new(
            ctx,
            &self.blocks,
            logs.to_sorted_by_y(),
            Vec::new(),
            Vec::new(),
            None,
        );
        for decorator in decorators {
            decorator.place(&mut context);
        }
    }
}

/// `isOverSolidGround`: the block below has a sturdy top face.
fn is_over_solid_ground(level: &WorldGenRegion<'_>, pos: BlockPos) -> bool {
    behaviour::is_face_sturdy(
        level.block(pos.offset(0, -1, 0)),
        Direction::Up,
        SupportType::Full,
    )
}
