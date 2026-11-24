//! `multiface_growth` (glow lichen, sculk veins) with the `MultifaceBlock` and
//! `MultifaceSpreader` placement logic it needs, plus `MultifaceBlock.canAttachTo`, which vines
//! and pale moss carpets share.

use serde_json::Value;
use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockId, BlockState, Shape, flags as block_flags, ids};

use super::util::{
    axis, bool_value, dist_manhattan, is_water_fluid, is_water_source, shuffle, with_bool,
};
use crate::Error;
use crate::behaviour::{SupportType, can_be_replaced, is_face_sturdy};
use crate::blocks::{BlockSet, qualify};
use crate::feature::region::WorldGenRegion;
use crate::feature::{
    FeatureContext, FeatureParser, FeatureType, bool_or, field, float_or, int_or,
};

/// `PipeBlock.PROPERTY_BY_DIRECTION` names, indexed by `Direction` ordinal.
const FACE_PROPERTY: [&str; 6] = ["down", "up", "north", "south", "west", "east"];

/// `MultifaceBlock.hasFace(state, direction)`.
fn has_face(state: BlockState, direction: Direction) -> bool {
    bool_value(state, FACE_PROPERTY[direction as usize])
}

/// `Block.isFaceFull(shape, face)`: the shape's slice at the `face` side of the block covers
/// the whole face.
pub(crate) fn is_face_full(shape: Shape, face: Direction) -> bool {
    const EPSILON: f64 = 1.0e-7;
    if shape.is_empty() {
        return false;
    }
    let a = usize::from(axis(face));
    let positive = matches!(face, Direction::Up | Direction::South | Direction::East);
    // The two axes spanning the face, in box coordinate order.
    let (u, v) = match a {
        0 => (1, 2),
        1 => (0, 2),
        _ => (0, 1),
    };
    // `Shapes.getFaceShape`: only the slab next to the face counts, i.e. the boxes reaching it.
    let touches = |b: &[f64; 6]| {
        if positive {
            (b[a + 3] - 1.0).abs() <= EPSILON
        } else {
            b[a].abs() <= EPSILON
        }
    };
    let mut us = vec![0.0, 1.0];
    let mut vs = vec![0.0, 1.0];
    for b in shape.iter().filter(|b| touches(b)) {
        us.extend([b[u].clamp(0.0, 1.0), b[u + 3].clamp(0.0, 1.0)]);
        vs.extend([b[v].clamp(0.0, 1.0), b[v + 3].clamp(0.0, 1.0)]);
    }
    us.sort_by(f64::total_cmp);
    us.dedup();
    vs.sort_by(f64::total_cmp);
    vs.dedup();
    // Every cell of the grid the box edges span must lie in some box touching the face.
    us.windows(2).all(|uw| {
        let mu = (uw[0] + uw[1]) * 0.5;
        vs.windows(2).all(|vw| {
            let mv = (vw[0] + vw[1]) * 0.5;
            shape
                .iter()
                .any(|b| touches(b) && b[u] <= mu && mu <= b[u + 3] && b[v] <= mv && mv <= b[v + 3])
        })
    })
}

/// `MultifaceBlock.canAttachTo(level, directionTowardsNeighbour, neighbourPos, neighbourState)`:
/// the neighbour's support or collision shape has a full face towards the block.
pub(crate) fn can_attach_to(direction_towards_neighbour: Direction, neighbour: BlockState) -> bool {
    let face = direction_towards_neighbour.opposite();
    is_face_sturdy(neighbour, face, SupportType::Full)
        || neighbour.has(block_flags::COLLISION_FULL_BLOCK)
        || is_face_full(neighbour.collision_shape(), face)
}

/// `MultifaceBlock.canAttachTo(level, pos, directionTowardsNeighbour)`.
pub(crate) fn can_attach_at(
    level: &WorldGenRegion<'_>,
    pos: BlockPos,
    direction_towards_neighbour: Direction,
) -> bool {
    let neighbour = level.block(pos.relative(direction_towards_neighbour));
    can_attach_to(direction_towards_neighbour, neighbour)
}

/// Up to six directions without allocation (Vanilla's small `ObjectArrayList<Direction>`s).
#[derive(Clone, Copy, Debug)]
struct Directions {
    items: [Direction; 6],
    len: usize,
}

impl Directions {
    const EMPTY: Self = Self {
        items: [Direction::Down; 6],
        len: 0,
    };

    fn push(&mut self, direction: Direction) {
        self.items[self.len] = direction;
        self.len += 1;
    }

    fn as_slice(&self) -> &[Direction] {
        &self.items[..self.len]
    }

    fn shuffled<R: RandomSource>(mut self, random: &mut R) -> Self {
        shuffle(&mut self.items[..self.len], random);
        self
    }
}

/// `MultifaceSpreader.SpreadType`, in `DEFAULT_SPREAD_ORDER`.
#[derive(Clone, Copy, Debug)]
enum SpreadType {
    SamePosition,
    SamePlane,
    WrapAround,
}

impl SpreadType {
    const DEFAULT_ORDER: [Self; 3] = [Self::SamePosition, Self::SamePlane, Self::WrapAround];

    /// `getSpreadPos(pos, spreadDirection, fromFace)`: the position and face to spread to.
    fn spread_pos(
        self,
        pos: BlockPos,
        spread: Direction,
        from_face: Direction,
    ) -> (BlockPos, Direction) {
        match self {
            Self::SamePosition => (pos, spread),
            Self::SamePlane => (pos.relative(spread), from_face),
            Self::WrapAround => (pos.relative(spread).relative(from_face), spread.opposite()),
        }
    }
}

/// The `MultifaceSpreadeableBlock`s: their spreader configurations differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpreaderKind {
    /// `GlowLichenBlock`: `MultifaceSpreader.DefaultSpreaderConfig`.
    GlowLichen,
    /// `SculkVeinBlock.SculkVeinSpreaderConfig` with the default spread order.
    SculkVein,
}

/// A `MultifaceSpreadeableBlock` with its `MultifaceSpreader`.
#[derive(Debug)]
struct SpreadableBlock {
    block: BlockId,
    kind: SpreaderKind,
    /// `#minecraft:fire`, which sculk veins never replace.
    fire: BlockSet,
}

impl SpreadableBlock {
    /// `MultifaceBlock.isValidStateForPlacement(level, oldState, placementPos, direction)`;
    /// both spreadable blocks support every face.
    fn is_valid_state_for_placement(
        &self,
        level: &WorldGenRegion<'_>,
        old: BlockState,
        pos: BlockPos,
        direction: Direction,
    ) -> bool {
        if old.block() == self.block && has_face(old, direction) {
            return false;
        }
        can_attach_at(level, pos, direction)
    }

    /// `MultifaceBlock.getStateForPlacement(oldState, level, placementPos, direction)`.
    fn state_for_placement(
        &self,
        level: &WorldGenRegion<'_>,
        old: BlockState,
        pos: BlockPos,
        direction: Direction,
    ) -> Option<BlockState> {
        if !self.is_valid_state_for_placement(level, old, pos, direction) {
            return None;
        }
        let state = if old.block() == self.block {
            old
        } else if is_water_source(old.fluid_state()) {
            with_bool(self.block.default_state(), "waterlogged", true)
        } else {
            self.block.default_state()
        };
        Some(with_bool(state, FACE_PROPERTY[direction as usize], true))
    }

    /// `SpreadConfig.isOtherBlockValidAsSource(state)`.
    fn is_other_block_valid_as_source(&self, state: BlockState) -> bool {
        match self.kind {
            SpreaderKind::GlowLichen => false,
            SpreaderKind::SculkVein => state.block() != ids::SCULK_VEIN,
        }
    }

    /// `stateCanBeReplaced(level, sourcePos, placementPos, placementDirection, existingState)`.
    fn state_can_be_replaced(
        &self,
        level: &WorldGenRegion<'_>,
        source: BlockPos,
        pos: BlockPos,
        direction: Direction,
        existing: BlockState,
    ) -> bool {
        let default = || {
            existing.is_air()
                || existing.block() == self.block
                || (existing.block() == ids::WATER && existing.fluid_state().info().source)
        };
        match self.kind {
            SpreaderKind::GlowLichen => default(),
            SpreaderKind::SculkVein => {
                let against = level.block(pos.relative(direction)).block();
                if against == ids::SCULK
                    || against == ids::SCULK_CATALYST
                    || against == ids::MOVING_PISTON
                {
                    return false;
                }
                if dist_manhattan(source, pos) == 2 {
                    let neighbour = source.relative(direction.opposite());
                    if is_face_sturdy(level.block(neighbour), direction, SupportType::Full) {
                        return false;
                    }
                }
                let fluid = existing.fluid_state();
                if !fluid.is_empty() && !is_water_fluid(fluid) {
                    return false;
                }
                !self.fire.contains(existing) && (can_be_replaced(existing) || default())
            }
        }
    }

    /// `SpreadConfig.canSpreadInto(level, sourcePos, spreadPos)`.
    fn can_spread_into(
        &self,
        level: &WorldGenRegion<'_>,
        source: BlockPos,
        (pos, face): (BlockPos, Direction),
    ) -> bool {
        let existing = level.block(pos);
        self.state_can_be_replaced(level, source, pos, face, existing)
            && self.is_valid_state_for_placement(level, existing, pos, face)
    }

    /// `MultifaceSpreader.getSpreadFromFaceTowardDirection`.
    fn spread_from_face_toward(
        &self,
        level: &WorldGenRegion<'_>,
        state: BlockState,
        pos: BlockPos,
        starting_face: Direction,
        spread: Direction,
    ) -> Option<(BlockPos, Direction)> {
        if axis(spread) == axis(starting_face) {
            return None;
        }
        if !(self.is_other_block_valid_as_source(state)
            || (has_face(state, starting_face) && !has_face(state, spread)))
        {
            return None;
        }
        SpreadType::DEFAULT_ORDER
            .iter()
            .map(|t| t.spread_pos(pos, spread, starting_face))
            .find(|&target| self.can_spread_into(level, pos, target))
    }

    /// `MultifaceSpreader.spreadToFace` with `SpreadConfig.placeBlock`.
    fn spread_to_face(
        &self,
        level: &mut WorldGenRegion<'_>,
        (pos, face): (BlockPos, Direction),
        post_process: bool,
    ) -> bool {
        let old = level.block(pos);
        let Some(state) = self.state_for_placement(level, old, pos, face) else {
            return false;
        };
        if post_process {
            level.mark_post_processing(pos);
        }
        level.set_block(pos, state, 2)
    }

    /// `MultifaceSpreader.spreadFromFaceTowardRandomDirection`: whether it spread.
    fn spread_from_face_toward_random_direction<R: RandomSource>(
        &self,
        level: &mut WorldGenRegion<'_>,
        state: BlockState,
        pos: BlockPos,
        starting_face: Direction,
        random: &mut R,
        post_process: bool,
    ) -> bool {
        let mut directions = Direction::ALL;
        shuffle(&mut directions, random);
        directions.into_iter().any(|spread| {
            self.spread_from_face_toward(level, state, pos, starting_face, spread)
                .is_some_and(|target| self.spread_to_face(level, target, post_process))
        })
    }
}

/// `multiface_growth`: places a multiface block on a matching neighbour at the origin or up to
/// `search_range` blocks away, then maybe spreads it once.
#[derive(Debug)]
pub struct MultifaceGrowthFeature {
    block: SpreadableBlock,
    search_range: i32,
    chance_of_spreading: f32,
    can_be_placed_on: BlockSet,
    /// `validDirections()`: up (ceiling), down (floor), then the horizontal ones (walls).
    valid_directions: Directions,
}

impl FeatureType for MultifaceGrowthFeature {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        let name = field(json, "block")?
            .as_str()
            .map(qualify)
            .ok_or_else(|| Error::new("block is not an id"))?;
        let block =
            BlockId::by_name(&name).ok_or_else(|| Error::new(format!("unknown block {name}")))?;
        let kind = match block {
            ids::GLOW_LICHEN => SpreaderKind::GlowLichen,
            ids::SCULK_VEIN => SpreaderKind::SculkVein,
            _ => {
                return Err(Error::new(
                    "Growth block should be a multiface spreadeable block",
                ));
            }
        };
        let search_range = int_or(json, "search_range", 10);
        if !(1..=64).contains(&search_range) {
            return Err(Error::new(format!(
                "search_range out of range: {search_range}"
            )));
        }
        let chance_of_spreading = float_or(json, "chance_of_spreading", 0.5);
        if !(0.0..=1.0).contains(&chance_of_spreading) {
            return Err(Error::new(format!(
                "chance_of_spreading out of range: {chance_of_spreading}"
            )));
        }
        let mut valid_directions = Directions::EMPTY;
        if bool_or(json, "can_place_on_ceiling", false) {
            valid_directions.push(Direction::Up);
        }
        if bool_or(json, "can_place_on_floor", false) {
            valid_directions.push(Direction::Down);
        }
        if bool_or(json, "can_place_on_wall", false) {
            for direction in super::util::HORIZONTAL {
                valid_directions.push(direction);
            }
        }
        Ok(Self {
            block: SpreadableBlock {
                block,
                kind,
                fire: parser.block_tag("minecraft:fire"),
            },
            search_range,
            chance_of_spreading,
            can_be_placed_on: parser.block_set(field(json, "can_be_placed_on")?)?,
            valid_directions,
        })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let origin_state = ctx.level.block(origin);
        if !is_air_or_water(origin_state) {
            return false;
        }
        let search_directions = self.valid_directions.shuffled(ctx.random);
        if self.place_growth_if_possible(ctx, origin, origin_state, search_directions.as_slice()) {
            return true;
        }
        for &search in search_directions.as_slice() {
            let mut placement_directions = Directions::EMPTY;
            for &direction in self.valid_directions.as_slice() {
                if direction != search.opposite() {
                    placement_directions.push(direction);
                }
            }
            let placement_directions = placement_directions.shuffled(ctx.random);
            for _ in 0..self.search_range {
                // Vanilla sets the cursor to `origin + searchDirection` on every step, so the
                // search never goes further than the first neighbour.
                let pos = origin.relative(search);
                let state = ctx.level.block(pos);
                if !is_air_or_water(state) && state.block() != self.block.block {
                    break;
                }
                if self.place_growth_if_possible(ctx, pos, state, placement_directions.as_slice()) {
                    return true;
                }
            }
        }
        false
    }
}

impl MultifaceGrowthFeature {
    /// `placeGrowthIfPossible`.
    fn place_growth_if_possible(
        &self,
        ctx: &mut FeatureContext<'_, '_>,
        pos: BlockPos,
        old: BlockState,
        directions: &[Direction],
    ) -> bool {
        for &direction in directions {
            let neighbour = ctx.level.block(pos.relative(direction));
            if !self.can_be_placed_on.contains(neighbour) {
                continue;
            }
            let Some(state) = self
                .block
                .state_for_placement(ctx.level, old, pos, direction)
            else {
                return false;
            };
            ctx.level.set_block_and_update(pos, state);
            ctx.level.mark_post_processing(pos);
            if ctx.random.next_float() < self.chance_of_spreading {
                self.block.spread_from_face_toward_random_direction(
                    ctx.level, state, pos, direction, ctx.random, true,
                );
            }
            return true;
        }
        false
    }
}

/// `MultifaceGrowthFeature.isAirOrWater`.
fn is_air_or_water(state: BlockState) -> bool {
    state.is_air() || state.block() == ids::WATER
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: Shape = &[[0.0, 0.0, 0.0, 1.0, 1.0, 1.0]];
    const SLAB: Shape = &[[0.0, 0.0, 0.0, 1.0, 0.5, 1.0]];
    const HALVES: Shape = &[
        [0.0, 0.0, 0.0, 0.5, 1.0, 1.0],
        [0.5, 0.0, 0.0, 1.0, 1.0, 1.0],
    ];
    const POST: Shape = &[[0.25, 0.0, 0.25, 0.75, 1.0, 0.75]];

    #[test]
    fn face_fullness_of_shapes() {
        for face in Direction::ALL {
            assert!(is_face_full(FULL, face));
            assert!(is_face_full(HALVES, face));
            assert!(!is_face_full(&[], face));
        }
        assert!(is_face_full(SLAB, Direction::Down));
        assert!(!is_face_full(SLAB, Direction::Up));
        assert!(!is_face_full(SLAB, Direction::North));
        assert!(!is_face_full(POST, Direction::Up));
    }
}
