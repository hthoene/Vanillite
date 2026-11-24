//! Block behaviour that world generation needs: survival checks, post-processing marks, shape
//! updates and face sturdiness (`BlockBehaviour`, `BlockState` and the
//! `Block` subclasses of Vanilla 26.3).
//!
//! Every block maps to the Vanilla class that gives it its behaviour (a table derived from
//! `Blocks.java`); the functions here dispatch on that class and read the level through
//! [`WorldGenRegion`]. Block and fluid tags come from the data packs ([`init`]).
//!
//! Not ported (the state is kept unchanged): `NetherPortalBlock.updateShape` (portal frame
//! search), `BeehiveBlock.updateShape` (releases bees next to fire) and the block entity side
//! effect of `PotentSulfurBlock.updateShape` (geyser countdown reset). Shapes are the
//! precomputed ones of an empty level without the random offset of offset blocks (bamboo,
//! pointed dripstone, …), as everywhere in generation.

mod class;
mod geometry;
mod props;
mod survive;
mod tables;
mod update;

#[cfg(test)]
mod tests;

use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockState, flags, ids};

use crate::feature::region::WorldGenRegion;
use props::PosExt;

pub use tables::init;

/// `SupportType` of `isFaceSturdy`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SupportType {
    Full,
    Center,
    Rigid,
}

/// `state.canSurvive(level, pos)`.
pub fn can_survive(level: &WorldGenRegion<'_>, state: BlockState, pos: BlockPos) -> bool {
    survive::can_survive(level, state, pos)
}

/// `state.getPostProcessPos(level, pos)`: the position to mark for post-processing after
/// `setBlock`, for the blocks with a `postProcess` predicate in `Blocks` (mushrooms mark
/// themselves, soul sand and magma blocks the block above).
pub fn post_process_pos(
    _level: &WorldGenRegion<'_>,
    state: BlockState,
    pos: BlockPos,
) -> Option<BlockPos> {
    let block = state.block();
    if block == ids::BROWN_MUSHROOM || block == ids::RED_MUSHROOM {
        Some(pos)
    } else if block == ids::SOUL_SAND || block == ids::MAGMA_BLOCK {
        Some(pos.above())
    } else {
        None
    }
}

/// `state.updateShape(level, ticks, pos, direction, neighbourPos, neighbourState, random)`: the
/// state after its neighbour in `direction` changed. Ticks it schedules go to the region, the
/// random it uses is the region's (`level.getRandom()`, as all generation callers pass).
pub fn update_shape(
    level: &mut WorldGenRegion<'_>,
    state: BlockState,
    pos: BlockPos,
    direction: Direction,
    neighbour_pos: BlockPos,
    neighbour_state: BlockState,
) -> BlockState {
    update::update_shape(level, state, pos, direction, neighbour_pos, neighbour_state)
}

/// `Block.updateFromNeighbourShapes(state, level, pos)`: [`update_shape`] for all six
/// directions in `UPDATE_SHAPE_ORDER` (west, east, north, south, down, up).
pub fn update_from_neighbour_shapes(
    level: &mut WorldGenRegion<'_>,
    state: BlockState,
    pos: BlockPos,
) -> BlockState {
    let mut state = state;
    for direction in props::UPDATE_SHAPE_ORDER {
        let neighbour_pos = pos.relative(direction);
        let neighbour = level.block(neighbour_pos);
        state = update_shape(level, state, pos, direction, neighbour_pos, neighbour);
    }
    state
}

/// `state.isFaceSturdy(level, pos, direction, support)` from the state's support shape.
pub fn is_face_sturdy(state: BlockState, direction: Direction, support: SupportType) -> bool {
    let misc = state.misc();
    let faces = match support {
        SupportType::Full => misc.sturdy_full,
        SupportType::Center => misc.sturdy_center,
        SupportType::Rigid => misc.sturdy_rigid,
    };
    faces & (1 << direction as u8) != 0
}

/// `Block.canSupportRigidBlock(level, pos)`: the top of the block at `pos` is rigid.
pub fn can_support_rigid_block(level: &WorldGenRegion<'_>, pos: BlockPos) -> bool {
    is_face_sturdy(level.block(pos), Direction::Up, SupportType::Rigid)
}

/// `Block.canSupportCenter(level, pos, direction)`.
pub fn can_support_center(level: &WorldGenRegion<'_>, pos: BlockPos, direction: Direction) -> bool {
    let state = level.block(pos);
    if direction == Direction::Down && tables::tags().blocks.unstable_bottom_center.contains(state)
    {
        return false;
    }
    is_face_sturdy(state, direction, SupportType::Center)
}

/// `MultifaceBlock.canAttachTo(level, direction, neighbourPos, neighbourState)`: whether a
/// face towards `direction` can attach to `neighbour` (its opposite face is full in the support
/// or the collision shape).
pub fn can_attach_multiface(neighbour: BlockState, direction: Direction) -> bool {
    let face = direction.opposite();
    is_face_sturdy(neighbour, face, SupportType::Full)
        || geometry::is_face_full(neighbour.collision_shape(), face)
}

/// `Block.isFaceFull(state.getCollisionShape(level, pos), direction)`.
pub fn is_collision_face_full(state: BlockState, direction: Direction) -> bool {
    geometry::is_face_full(state.collision_shape(), direction)
}

/// `level.getRawBrightness(pos, 0)` during generation.
///
/// Chunks in the features stage are not lit yet: the block light engine has no data (0) and
/// the sky light engine reports full light (15) for columns it has not seen. Dimensions without
/// sky light have no sky engine (0). The region does not carry the dimension type, so sky light
/// is inferred from the build height: Vanilla's dimensions without sky light (the Nether and
/// the End) are the ones starting at Y 0.
pub fn raw_brightness(level: &WorldGenRegion<'_>, _pos: BlockPos) -> i32 {
    if level.has_sky_light() { 15 } else { 0 }
}

/// `state.isSolid()` (the legacy "solid" flag).
pub fn is_solid(state: BlockState) -> bool {
    state.has(flags::SOLID)
}

/// `state.canBeReplaced()`.
pub fn can_be_replaced(state: BlockState) -> bool {
    state.has(flags::REPLACEABLE)
}

/// `state.isSolidRender()`.
pub fn is_solid_render(state: BlockState) -> bool {
    state.has(flags::SOLID_RENDER)
}
