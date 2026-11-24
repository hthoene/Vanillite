//! Plant and surface vegetation features.

mod multiface;
mod patch;
mod simple_block;
pub(crate) mod util;

use std::sync::LazyLock;

use serde_json::Value;
use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Direction};
use vanillite_chunk::HeightmapKind;
use vanillite_data::block::{BlockState, ids};

pub use multiface::MultifaceGrowthFeature;
pub use patch::{VegetationPatchFeature, WaterloggedVegetationPatchFeature};
pub use simple_block::SimpleBlockFeature;

use self::multiface::can_attach_at;
use self::util::{parse_direction, with, with_bool};
use crate::Error;
use crate::behaviour::{SupportType, can_survive, is_face_sturdy};
use crate::blocks::BlockSet;
use crate::carver::providers::IntProvider;
use crate::feature::{
    BlockPredicate, BlockStateProvider, FeatureContext, FeatureParser, FeatureType,
    PlacedFeatureId, field, float_field, float_or, int_field,
};

/// A float within `0..=1` (`Codec.floatRange(0, 1)`).
fn unit_float(value: f32, name: &str) -> Result<f32, Error> {
    if (0.0..=1.0).contains(&value) {
        Ok(value)
    } else {
        Err(Error::new(format!("{name} out of range: {value}")))
    }
}

/// The bamboo states `BambooFeature` places.
struct BambooStates {
    trunk: BlockState,
    final_large: BlockState,
    top_large: BlockState,
    top_small: BlockState,
}

static BAMBOO: LazyLock<BambooStates> = LazyLock::new(|| {
    let trunk = with(
        with(
            with(ids::BAMBOO.default_state(), "age", "1"),
            "leaves",
            "none",
        ),
        "stage",
        "0",
    );
    BambooStates {
        trunk,
        final_large: with(with(trunk, "leaves", "large"), "stage", "1"),
        top_large: with(trunk, "leaves", "large"),
        top_small: with(trunk, "leaves", "small"),
    }
});

/// `bamboo`: a bamboo stalk, sometimes on a disc of podzol.
#[derive(Debug)]
pub struct BambooFeature {
    probability: f32,
    /// `#minecraft:beneath_bamboo_podzol_replaceable`.
    podzol_replaceable: BlockSet,
}

impl FeatureType for BambooFeature {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        Ok(Self {
            probability: unit_float(float_field(json, "probability")?, "probability")?,
            podzol_replaceable: parser.block_tag("minecraft:beneath_bamboo_podzol_replaceable"),
        })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let level = &mut *ctx.level;
        let random = &mut *ctx.random;
        if !level.is_empty_block(origin) {
            return false;
        }
        if can_survive(level, ids::BAMBOO.default_state(), origin) {
            let height = random.next_int_bounded(12) + 5;
            if random.next_float() < self.probability {
                let r = random.next_int_bounded(4) + 1;
                for xx in origin.x - r..=origin.x + r {
                    for zz in origin.z - r..=origin.z + r {
                        let xd = xx - origin.x;
                        let zd = zz - origin.z;
                        if xd * xd + zd * zd <= r * r {
                            let y = level.height_at(HeightmapKind::WorldSurface, xx, zz) - 1;
                            let podzol = BlockPos::new(xx, y, zz);
                            if self.podzol_replaceable.contains(level.block(podzol)) {
                                level.set_block(podzol, ids::PODZOL.default_state(), 2);
                            }
                        }
                    }
                }
            }
            let states = &*BAMBOO;
            let mut pos = origin;
            let mut i = 0;
            while i < height && level.is_empty_block(pos) {
                level.set_block(pos, states.trunk, 2);
                pos = pos.offset(0, 1, 0);
                i += 1;
            }
            if pos.y - origin.y >= 3 {
                level.set_block(pos, states.final_large, 2);
                pos = pos.offset(0, -1, 0);
                level.set_block(pos, states.top_large, 2);
                pos = pos.offset(0, -1, 0);
                level.set_block(pos, states.top_small, 2);
            }
        }
        true
    }
}

/// One layer of a [`BlockColumnFeature`].
#[derive(Debug)]
struct ColumnLayer {
    height: IntProvider,
    provider: BlockStateProvider,
}

/// `block_column`: layers of blocks stacked in a direction, truncated where placement is not
/// allowed.
#[derive(Debug)]
pub struct BlockColumnFeature {
    layers: Vec<ColumnLayer>,
    direction: Direction,
    allowed_placement: BlockPredicate,
    prioritize_tip: bool,
}

impl FeatureType for BlockColumnFeature {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        let layers = field(json, "layers")?
            .as_array()
            .ok_or_else(|| Error::new("layers is not a list"))?
            .iter()
            .map(|layer| {
                Ok(ColumnLayer {
                    height: IntProvider::from_json_in_range(field(layer, "height")?, 0, i32::MAX)?,
                    provider: parser.state_provider(field(layer, "provider")?)?,
                })
            })
            .collect::<Result<_, Error>>()?;
        Ok(Self {
            layers,
            direction: parse_direction(field(json, "direction")?)?,
            allowed_placement: parser.block_predicate(field(json, "allowed_placement")?)?,
            prioritize_tip: field(json, "prioritize_tip")?
                .as_bool()
                .ok_or_else(|| Error::new("prioritize_tip is not a boolean"))?,
        })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let mut heights: Vec<i32> = Vec::with_capacity(self.layers.len());
        let mut total: i32 = 0;
        for layer in &self.layers {
            let height = layer.height.sample(ctx.random);
            heights.push(height);
            total = total.wrapping_add(height);
        }
        if total == 0 {
            return false;
        }
        let mut next = origin.relative(self.direction);
        for y in 0..total {
            if !self.allowed_placement.test(ctx.level, next) {
                truncate(&mut heights, total, y, self.prioritize_tip);
                break;
            }
            next = next.relative(self.direction);
        }
        let mut pos = origin;
        for (layer, &count) in self.layers.iter().zip(&heights) {
            for _ in 0..count {
                let state = layer.provider.get_state(ctx.level, ctx.random, pos);
                ctx.level.set_block(pos, state, 2);
                pos = pos.relative(self.direction);
            }
        }
        true
    }
}

/// `BlockColumnFeature.truncate`: removes `total - new_height` blocks from the base layers
/// first, or from the tip first with `prioritize_tip`.
fn truncate(heights: &mut [i32], total: i32, new_height: i32, prioritize_tip: bool) {
    let mut to_remove = total - new_height;
    let mut take = |height: &mut i32| {
        let removed = (*height).min(to_remove);
        to_remove -= removed;
        *height -= removed;
        to_remove > 0
    };
    if prioritize_tip {
        for height in heights.iter_mut() {
            if !take(height) {
                break;
            }
        }
    } else {
        for height in heights.iter_mut().rev() {
            if !take(height) {
                break;
            }
        }
    }
}

/// `block_pile`: a rough ellipse of blocks two high on sturdy ground.
#[derive(Debug)]
pub struct BlockPileFeature {
    state_provider: BlockStateProvider,
}

impl FeatureType for BlockPileFeature {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        Ok(Self {
            state_provider: parser.state_provider(field(json, "state_provider")?)?,
        })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        if origin.y < ctx.level.min_y() + 5 {
            return false;
        }
        let xr = 2 + ctx.random.next_int_bounded(2);
        let zr = 2 + ctx.random.next_int_bounded(2);
        // `BlockPos.betweenClosed`: x fastest, then y, then z.
        for z in origin.z - zr..=origin.z + zr {
            for y in origin.y..=origin.y + 1 {
                for x in origin.x - xr..=origin.x + xr {
                    let pos = BlockPos::new(x, y, z);
                    let xd = origin.x - x;
                    let zd = origin.z - z;
                    let a = ctx.random.next_float() * 10.0;
                    let b = ctx.random.next_float() * 6.0;
                    if ((xd * xd + zd * zd) as f32) <= a - b
                        || f64::from(ctx.random.next_float()) < 0.031
                    {
                        self.try_place_block(ctx, pos);
                    }
                }
            }
        }
        true
    }
}

impl BlockPileFeature {
    /// `tryPlaceBlock` with `mayPlaceOn`.
    fn try_place_block(&self, ctx: &mut FeatureContext<'_, '_>, pos: BlockPos) {
        if !ctx.level.is_empty_block(pos) {
            return;
        }
        let below = ctx.level.block(pos.offset(0, -1, 0));
        let may_place = if below.block() == ids::DIRT_PATH {
            ctx.random.next_boolean()
        } else {
            is_face_sturdy(below, Direction::Up, SupportType::Full)
        };
        if may_place {
            let state = self.state_provider.get_state(ctx.level, ctx.random, pos);
            ctx.level.set_block(pos, state, 260);
        }
    }
}

/// `projected_random_patchy_square`: a square, sparser towards the corners, each block
/// dropped through `project_through` blocks.
#[derive(Debug)]
pub struct ProjectedRandomPatchySquare {
    block: BlockStateProvider,
    project_through: BlockPredicate,
    size: IntProvider,
    max_projection_height: i32,
}

impl FeatureType for ProjectedRandomPatchySquare {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        let max_projection_height = int_field(json, "max_projection_height")?;
        if max_projection_height < 0 {
            return Err(Error::new("max_projection_height must be non-negative"));
        }
        Ok(Self {
            block: parser.state_provider(field(json, "block")?)?,
            project_through: parser.block_predicate(field(json, "project_through")?)?,
            size: IntProvider::from_json_in_range(field(json, "size")?, 1, 16)?,
            max_projection_height,
        })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let size = self.size.sample(ctx.random);
        let bound = size * size + 1;
        for dx in -size..=size {
            for dz in -size..=size {
                let probability = dx.abs() * dz.abs();
                if ctx.random.next_int_bounded(bound) >= bound - probability {
                    continue;
                }
                let mut base = origin.offset(dx, 0, dz);
                let mut drop = self.max_projection_height;
                while self.project_through.test(ctx.level, base.offset(0, -1, 0)) {
                    base = base.offset(0, -1, 0);
                    drop -= 1;
                    if drop <= 0 {
                        break;
                    }
                }
                if let Some(state) = self.block.get_optional_state(ctx.level, ctx.random, base) {
                    ctx.level.set_block(base, state, 2);
                }
            }
        }
        true
    }
}

/// `random_neighbor_spread`: a block at the origin, then more of it at random offsets that
/// touch exactly one accepted neighbour.
#[derive(Debug)]
pub struct RandomNeighborSpreadFeature {
    block: BlockStateProvider,
    accepted_neighbors: BlockSet,
    can_replace: BlockPredicate,
    attempts: IntProvider,
    xz_offset: IntProvider,
    y_offset: IntProvider,
}

impl FeatureType for RandomNeighborSpreadFeature {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        Ok(Self {
            block: parser.state_provider(field(json, "block")?)?,
            accepted_neighbors: parser.block_set(field(json, "accepted_neighbors")?)?,
            can_replace: parser.block_predicate(field(json, "can_replace")?)?,
            attempts: IntProvider::from_json_in_range(field(json, "attempts")?, 1, 3000)?,
            xz_offset: IntProvider::from_json_in_range(field(json, "xz_offset")?, -16, 16)?,
            y_offset: IntProvider::from_json_in_range(field(json, "y_offset")?, -16, 16)?,
        })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let state = self.block.get_state(ctx.level, ctx.random, origin);
        ctx.level.set_block(origin, state, 2);
        let attempts = self.attempts.sample(ctx.random);
        for _ in 0..attempts {
            let dx = self.xz_offset.sample(ctx.random);
            let dy = self.y_offset.sample(ctx.random);
            let dz = self.xz_offset.sample(ctx.random);
            let pos = origin.offset(dx, dy, dz);
            if !self.can_replace.test(ctx.level, pos) {
                continue;
            }
            let mut neighbours = 0;
            for direction in Direction::ALL {
                if self
                    .accepted_neighbors
                    .contains(ctx.level.block(pos.relative(direction)))
                {
                    neighbours += 1;
                }
                if neighbours > 1 {
                    break;
                }
            }
            if neighbours == 1 {
                let state = self.block.get_state(ctx.level, ctx.random, pos);
                ctx.level.set_block(pos, state, 2);
            }
        }
        true
    }
}

/// `single_block_pillar`: a pillar up or down while blocks can be replaced and a chance holds,
/// then an optional cap feature at its end.
#[derive(Debug)]
pub struct SingleBlockPillarFeature {
    block: BlockStateProvider,
    can_replace: BlockPredicate,
    direction: Direction,
    chance_to_continue: f32,
    cap_feature: Option<PlacedFeatureId>,
}

impl FeatureType for SingleBlockPillarFeature {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        let direction = parse_direction(field(json, "direction")?)?;
        if !matches!(direction, Direction::Up | Direction::Down) {
            return Err(Error::new("direction must be vertical"));
        }
        let can_replace = match json.get("can_replace") {
            Some(predicate) => parser.block_predicate(predicate)?,
            None => BlockPredicate::True,
        };
        let cap_feature = match json.get("cap_feature") {
            Some(feature) => Some(parser.placed(feature)?),
            None => None,
        };
        Ok(Self {
            block: parser.state_provider(field(json, "block")?)?,
            can_replace,
            direction,
            chance_to_continue: unit_float(
                float_or(json, "chance_to_continue", 1.0),
                "chance_to_continue",
            )?,
            cap_feature,
        })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let mut pos = origin;
        while self.can_replace.test(ctx.level, pos)
            && ctx.random.next_float() < self.chance_to_continue
            && !ctx.level.is_outside_build_height(pos.y)
        {
            let state = self.block.get_state(ctx.level, ctx.random, pos);
            ctx.level.set_block(pos, state, 2);
            pos = pos.relative(self.direction);
        }
        pos = pos.relative(self.direction.opposite());
        if let Some(cap) = self.cap_feature {
            ctx.place_placed(cap, pos);
        }
        true
    }
}

/// The vine states with one face set, indexed by `Direction` ordinal (`down` has no property
/// and is never used).
static VINE_FACES: LazyLock<[BlockState; 6]> = LazyLock::new(|| {
    let vine = ids::VINE.default_state();
    [
        vine,
        with_bool(vine, "up", true),
        with_bool(vine, "north", true),
        with_bool(vine, "south", true),
        with_bool(vine, "west", true),
        with_bool(vine, "east", true),
    ]
});

/// `vines`: a vine on the first face (not down) with an acceptable neighbour.
#[derive(Debug)]
pub struct VinesFeature;

impl FeatureType for VinesFeature {
    fn parse(_json: &Value, _parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        Ok(Self)
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        if !ctx.level.is_empty_block(origin) {
            return false;
        }
        for direction in Direction::ALL {
            // `VineBlock.isAcceptableNeighbour` is `MultifaceBlock.canAttachTo`.
            if direction != Direction::Down && can_attach_at(ctx.level, origin, direction) {
                ctx.level
                    .set_block(origin, VINE_FACES[direction as usize], 2);
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_removes_from_the_base_or_the_tip() {
        let mut heights = [2, 3, 1];
        truncate(&mut heights, 6, 2, false);
        assert_eq!(heights, [2, 0, 0]);
        let mut heights = [2, 3, 1];
        truncate(&mut heights, 6, 2, true);
        assert_eq!(heights, [0, 1, 1]);
    }
}
