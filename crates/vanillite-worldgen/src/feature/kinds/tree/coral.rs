//! Coral trees and claws (`CoralTreeFeature`, `CoralClawFeature`).
//!
//! In 26.3 both shape features place a placed feature (the coral block with its decorations) at
//! every block of the shape; there is no shared coral base class any more.

use serde_json::Value;
use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Direction};

use super::{HORIZONTAL, clockwise, counter_clockwise, random_horizontal, shuffle};
use crate::Error;
use crate::feature::{FeatureContext, FeatureParser, FeatureType, PlacedFeatureId, field};

/// `minecraft:coral_tree` (`CoralTreeFeature`).
#[derive(Debug)]
pub struct CoralTreeFeature {
    feature: PlacedFeatureId,
}

impl FeatureType for CoralTreeFeature {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        Ok(Self {
            feature: parser.placed(field(json, "feature")?)?,
        })
    }

    /// `CoralTreeFeature.place`.
    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let mut pos = origin;
        let trunk_height = ctx.random.next_int_bounded(3) + 1;
        for _ in 0..trunk_height {
            if !ctx.place_placed(self.feature, pos) {
                return true;
            }
            pos = pos.offset(0, 1, 0);
        }
        let trunk_top = pos;
        let branches = ctx.random.next_int_bounded(3) + 2;
        let mut directions = HORIZONTAL;
        shuffle(&mut directions, ctx.random);
        for &branch_direction in &directions[..branches as usize] {
            let mut pos = trunk_top.relative(branch_direction);
            let branch_height = ctx.random.next_int_bounded(5) + 2;
            let mut segment_length = 0;
            let mut j = 0;
            while j < branch_height && ctx.place_placed(self.feature, pos) {
                segment_length += 1;
                pos = pos.offset(0, 1, 0);
                if j == 0 || segment_length >= 2 && ctx.random.next_float() < 0.25 {
                    pos = pos.relative(branch_direction);
                    segment_length = 0;
                }
                j += 1;
            }
        }
        true
    }
}

/// `minecraft:coral_claw` (`CoralClawFeature`).
#[derive(Debug)]
pub struct CoralClawFeature {
    feature: PlacedFeatureId,
}

impl FeatureType for CoralClawFeature {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        Ok(Self {
            feature: parser.placed(field(json, "feature")?)?,
        })
    }

    /// `CoralClawFeature.place`.
    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        if !ctx.place_placed(self.feature, origin) {
            return false;
        }
        let claw_direction = random_horizontal(ctx.random);
        let branches = ctx.random.next_int_bounded(2) + 2;
        let mut possible = [
            claw_direction,
            clockwise(claw_direction),
            counter_clockwise(claw_direction),
        ];
        shuffle(&mut possible, ctx.random);
        for &branch_direction in &possible[..branches as usize] {
            let mut pos = origin;
            let sideways_length = ctx.random.next_int_bounded(2) + 1;
            pos = pos.relative(branch_direction);
            let (segment_direction, inward_length) = if branch_direction == claw_direction {
                (claw_direction, ctx.random.next_int_bounded(3) + 2)
            } else {
                pos = pos.offset(0, 1, 0);
                let choices = [branch_direction, Direction::Up];
                let segment = choices[ctx.random.next_int_bounded(2) as usize];
                (segment, ctx.random.next_int_bounded(3) + 3)
            };
            let mut i = 0;
            while i < sideways_length && ctx.place_placed(self.feature, pos) {
                pos = pos.relative(segment_direction);
                i += 1;
            }
            pos = pos.relative(segment_direction.opposite());
            pos = pos.offset(0, 1, 0);
            for _ in 0..inward_length {
                pos = pos.relative(claw_direction);
                if !ctx.place_placed(self.feature, pos) {
                    break;
                }
                if ctx.random.next_float() < 0.25 {
                    pos = pos.offset(0, 1, 0);
                }
            }
        }
        true
    }
}
