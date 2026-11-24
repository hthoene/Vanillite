//! Shared helpers of the placement rules: directions and axes, typed state property access,
//! fluids, face sturdiness (`SupportType`) and shape faces.

use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockId, BlockKind, BlockState, Shape, flags, ids};

use super::tags;
use super::{BlockReader, PlaceContext};

/// `Direction.Axis`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Axis {
    X,
    Y,
    Z,
}

impl Axis {
    /// The property value (`x`, `y`, `z`).
    pub const fn name(self) -> &'static str {
        match self {
            Self::X => "x",
            Self::Y => "y",
            Self::Z => "z",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "x" => Some(Self::X),
            "y" => Some(Self::Y),
            "z" => Some(Self::Z),
            _ => None,
        }
    }

    pub const fn is_horizontal(self) -> bool {
        !matches!(self, Self::Y)
    }
}

/// The four horizontal directions in 2D data order (`Direction.Plane.HORIZONTAL` iterates
/// north, east, south, west; see [`HORIZONTAL`]).
pub const BY_2D: [Direction; 4] = [
    Direction::South,
    Direction::West,
    Direction::North,
    Direction::East,
];

/// `Direction.Plane.HORIZONTAL` in its iteration order: north, east, south, west.
pub const HORIZONTAL: [Direction; 4] = [
    Direction::North,
    Direction::East,
    Direction::South,
    Direction::West,
];

/// `Direction.UPDATE_SHAPE_ORDER` / `Block.UPDATE_SHAPE_ORDER`: west, east, north, south, down, up.
pub const UPDATE_SHAPE_ORDER: [Direction; 6] = [
    Direction::West,
    Direction::East,
    Direction::North,
    Direction::South,
    Direction::Down,
    Direction::Up,
];

/// Vanilla `Direction` methods that `vanillite_base` does not have.
pub trait DirExt: Copy {
    fn axis(self) -> Axis;
    fn is_horizontal(self) -> bool;
    fn is_vertical(self) -> bool;
    /// `getClockWise()` around Y (north → east → south → west). Vertical directions stay.
    fn clockwise(self) -> Direction;
    /// `getCounterClockWise()` around Y. Vertical directions stay.
    fn counter_clockwise(self) -> Direction;
    /// `get2DDataValue()`: south 0, west 1, north 2, east 3; -1 for vertical directions.
    fn data_2d(self) -> i32;
    /// `getAxisDirection() == POSITIVE`.
    fn is_positive(self) -> bool;
    /// The property value (`down`, `up`, `north`, …).
    fn name(self) -> &'static str;
    /// `getStepX/Y/Z`.
    fn step_x(self) -> i32;
    fn step_y(self) -> i32;
    fn step_z(self) -> i32;
    /// `toYRot()`.
    fn to_y_rot(self) -> f32;
}

impl DirExt for Direction {
    #[inline]
    fn axis(self) -> Axis {
        match self {
            Direction::Down | Direction::Up => Axis::Y,
            Direction::North | Direction::South => Axis::Z,
            Direction::West | Direction::East => Axis::X,
        }
    }

    #[inline]
    fn is_horizontal(self) -> bool {
        !self.is_vertical()
    }

    #[inline]
    fn is_vertical(self) -> bool {
        matches!(self, Direction::Down | Direction::Up)
    }

    #[inline]
    fn clockwise(self) -> Direction {
        match self {
            Direction::North => Direction::East,
            Direction::East => Direction::South,
            Direction::South => Direction::West,
            Direction::West => Direction::North,
            d => d,
        }
    }

    #[inline]
    fn counter_clockwise(self) -> Direction {
        match self {
            Direction::North => Direction::West,
            Direction::West => Direction::South,
            Direction::South => Direction::East,
            Direction::East => Direction::North,
            d => d,
        }
    }

    #[inline]
    fn data_2d(self) -> i32 {
        match self {
            Direction::South => 0,
            Direction::West => 1,
            Direction::North => 2,
            Direction::East => 3,
            _ => -1,
        }
    }

    #[inline]
    fn is_positive(self) -> bool {
        matches!(self, Direction::Up | Direction::South | Direction::East)
    }

    #[inline]
    fn name(self) -> &'static str {
        match self {
            Direction::Down => "down",
            Direction::Up => "up",
            Direction::North => "north",
            Direction::South => "south",
            Direction::West => "west",
            Direction::East => "east",
        }
    }

    #[inline]
    fn step_x(self) -> i32 {
        self.normal()[0]
    }

    #[inline]
    fn step_y(self) -> i32 {
        self.normal()[1]
    }

    #[inline]
    fn step_z(self) -> i32 {
        self.normal()[2]
    }

    #[inline]
    fn to_y_rot(self) -> f32 {
        ((self.data_2d() & 3) * 90) as f32
    }
}

/// Parses a direction property value.
pub fn dir_from_name(name: &str) -> Option<Direction> {
    match name {
        "down" => Some(Direction::Down),
        "up" => Some(Direction::Up),
        "north" => Some(Direction::North),
        "south" => Some(Direction::South),
        "west" => Some(Direction::West),
        "east" => Some(Direction::East),
        _ => None,
    }
}

/// `Direction.from2DDataValue`.
#[inline]
pub fn from_2d(data: i32) -> Direction {
    BY_2D[(data % 4).unsigned_abs() as usize]
}

/// `Direction.fromYRot(double)`.
#[inline]
pub fn from_y_rot(y_rot: f64) -> Direction {
    from_2d(vanillite_base::math::floor(y_rot / 90.0 + 0.5) & 3)
}

/// `Direction.fromAxisAndDirection`.
#[inline]
pub fn from_axis(axis: Axis, positive: bool) -> Direction {
    match (axis, positive) {
        (Axis::X, true) => Direction::East,
        (Axis::X, false) => Direction::West,
        (Axis::Y, true) => Direction::Up,
        (Axis::Y, false) => Direction::Down,
        (Axis::Z, true) => Direction::South,
        (Axis::Z, false) => Direction::North,
    }
}

/// Java `Math.round(float)`.
#[inline]
pub fn round_f(f: f32) -> i32 {
    // `f + 0.5` is exact in double precision, so this is Java's floor(f + 0.5) without the
    // float rounding of the sum.
    (f64::from(f) + 0.5).floor() as i32
}

/// `RotationSegment.convertToSegment(float)`: 16 segments, 0 = north (south in degrees 0).
#[inline]
pub fn rotation_segment(degrees: f32) -> i32 {
    round_f(degrees * (16.0_f32 / 360.0_f32)) & 15
}

/// `RotationSegment.convertToSegment(Direction)`.
#[inline]
pub fn rotation_segment_dir(direction: Direction) -> i32 {
    if direction.is_vertical() {
        0
    } else {
        direction.data_2d() << 2
    }
}

/// Integer property values as strings (`0` … `25`).
const NUMBERS: [&str; 26] = [
    "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13", "14", "15", "16",
    "17", "18", "19", "20", "21", "22", "23", "24", "25",
];

/// Typed access to block state properties. Setters leave the state unchanged when the block has
/// no such property or value (Vanilla would throw; callers only use valid properties).
pub trait StateExt: Copy {
    fn kind(self) -> BlockKind;
    /// `BlockState.is(Block)`.
    fn is(self, block: BlockId) -> bool;
    fn has_prop(self, prop: &str) -> bool;
    fn set(self, prop: &str, value: &str) -> Self;
    fn set_bool(self, prop: &str, value: bool) -> Self;
    fn set_int(self, prop: &str, value: i32) -> Self;
    fn set_dir(self, prop: &str, value: Direction) -> Self;
    fn set_axis(self, prop: &str, value: Axis) -> Self;
    fn get(self, prop: &str) -> Option<&'static str>;
    /// `false` when missing.
    fn get_bool(self, prop: &str) -> bool;
    /// `0` when missing.
    fn get_int(self, prop: &str) -> i32;
    fn get_dir(self, prop: &str) -> Option<Direction>;
    fn get_axis(self, prop: &str) -> Option<Axis>;
    /// `BlockState.isFaceSturdy(level, pos, direction, support)` (from the generated, level
    /// independent facts).
    fn sturdy(self, direction: Direction, support: Support) -> bool;
    /// `BlockState.isSolid()` (legacy solid).
    fn is_solid(self) -> bool;
    /// `BlockState.canBeReplaced()` (the replaceable flag).
    fn replaceable(self) -> bool;
    /// `BlockState.blocksMotion()`.
    fn blocks_motion(self) -> bool;
    /// `BlockState.isRedstoneConductor` (generated, level independent).
    fn is_redstone_conductor(self) -> bool;
    /// `BlockState.isCollisionShapeFullBlock`.
    fn collision_full_block(self) -> bool;
}

impl StateExt for BlockState {
    #[inline]
    fn kind(self) -> BlockKind {
        self.block().info().kind
    }

    #[inline]
    fn is(self, block: BlockId) -> bool {
        self.block() == block
    }

    #[inline]
    fn has_prop(self, prop: &str) -> bool {
        self.value(prop).is_some()
    }

    #[inline]
    fn set(self, prop: &str, value: &str) -> Self {
        self.with(prop, value).unwrap_or(self)
    }

    #[inline]
    fn set_bool(self, prop: &str, value: bool) -> Self {
        self.set(prop, if value { "true" } else { "false" })
    }

    #[inline]
    fn set_int(self, prop: &str, value: i32) -> Self {
        match usize::try_from(value).ok().and_then(|i| NUMBERS.get(i)) {
            Some(v) => self.set(prop, v),
            None => self,
        }
    }

    #[inline]
    fn set_dir(self, prop: &str, value: Direction) -> Self {
        self.set(prop, value.name())
    }

    #[inline]
    fn set_axis(self, prop: &str, value: Axis) -> Self {
        self.set(prop, value.name())
    }

    #[inline]
    fn get(self, prop: &str) -> Option<&'static str> {
        self.value(prop)
    }

    #[inline]
    fn get_bool(self, prop: &str) -> bool {
        self.value(prop) == Some("true")
    }

    #[inline]
    fn get_int(self, prop: &str) -> i32 {
        self.value(prop).and_then(|v| v.parse().ok()).unwrap_or(0)
    }

    #[inline]
    fn get_dir(self, prop: &str) -> Option<Direction> {
        self.value(prop).and_then(dir_from_name)
    }

    #[inline]
    fn get_axis(self, prop: &str) -> Option<Axis> {
        self.value(prop).and_then(Axis::from_name)
    }

    #[inline]
    fn sturdy(self, direction: Direction, support: Support) -> bool {
        let misc = self.misc();
        let mask = match support {
            Support::Full => misc.sturdy_full,
            Support::Center => misc.sturdy_center,
            Support::Rigid => misc.sturdy_rigid,
        };
        mask >> (direction as u8) & 1 != 0
    }

    #[inline]
    fn is_solid(self) -> bool {
        self.has(flags::SOLID)
    }

    #[inline]
    fn replaceable(self) -> bool {
        self.has(flags::REPLACEABLE)
    }

    #[inline]
    fn blocks_motion(self) -> bool {
        let block = self.block();
        block != ids::COBWEB && block != ids::BAMBOO_SAPLING && self.is_solid()
    }

    #[inline]
    fn is_redstone_conductor(self) -> bool {
        self.has(flags::REDSTONE_CONDUCTOR)
    }

    #[inline]
    fn collision_full_block(self) -> bool {
        self.has(flags::COLLISION_FULL_BLOCK)
    }
}

/// `SupportType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Support {
    Full,
    Center,
    Rigid,
}

/// `level.getBlockState(pos).isFaceSturdy(level, pos, direction, support)`.
#[inline]
pub fn sturdy(
    level: &dyn BlockReader,
    pos: BlockPos,
    direction: Direction,
    support: Support,
) -> bool {
    level.state(pos).sturdy(direction, support)
}

/// `Block.canSupportRigidBlock(level, below)`.
#[inline]
pub fn can_support_rigid_block(level: &dyn BlockReader, below: BlockPos) -> bool {
    sturdy(level, below, Direction::Up, Support::Rigid)
}

/// `Block.canSupportCenter(level, belowPos, direction)`.
pub fn can_support_center(level: &dyn BlockReader, pos: BlockPos, direction: Direction) -> bool {
    let state = level.state(pos);
    if direction == Direction::Down && tags::UNSTABLE_BOTTOM_CENTER.has(state) {
        return false;
    }
    state.sturdy(direction, Support::Center)
}

/// `Block.isExceptionForConnection`.
pub fn is_exception_for_connection(state: BlockState) -> bool {
    let block = state.block();
    matches!(
        state.kind(),
        BlockKind::LeavesBlock
            | BlockKind::MangroveLeavesBlock
            | BlockKind::TintedParticleLeavesBlock
            | BlockKind::UntintedParticleLeavesBlock
    ) || block == ids::BARRIER
        || block == ids::CARVED_PUMPKIN
        || block == ids::JACK_O_LANTERN
        || block == ids::MELON
        || block == ids::PUMPKIN
        || tags::SHULKER_BOXES.contains(block)
}

/// Fluid registry ids (`minecraft:fluid`).
pub mod fluid {
    pub const FLOWING_WATER: u16 = 1;
    pub const WATER: u16 = 2;
    pub const FLOWING_LAVA: u16 = 3;
    pub const LAVA: u16 = 4;
}

/// `getFluidState(pos).getType() == Fluids.WATER` (a water source, also in waterlogged blocks).
#[inline]
pub fn is_water_source(level: &dyn BlockReader, pos: BlockPos) -> bool {
    level.state(pos).fluid_state().info().fluid == fluid::WATER
}

/// `getFluidState(pos).is(FluidTags.WATER)` (source or flowing).
#[inline]
pub fn is_water(level: &dyn BlockReader, pos: BlockPos) -> bool {
    fluid_is_water(level.state(pos))
}

/// `state.getFluidState().is(FluidTags.WATER)`.
#[inline]
pub fn fluid_is_water(state: BlockState) -> bool {
    matches!(
        state.fluid_state().info().fluid,
        fluid::WATER | fluid::FLOWING_WATER
    )
}

/// `state.getFluidState().is(FluidTags.LAVA)`.
#[inline]
pub fn fluid_is_lava(state: BlockState) -> bool {
    matches!(
        state.fluid_state().info().fluid,
        fluid::LAVA | fluid::FLOWING_LAVA
    )
}

/// `getFluidState().getAmount()`.
#[inline]
pub fn fluid_amount(state: BlockState) -> i32 {
    i32::from(state.fluid_state().info().amount)
}

/// Sets `waterlogged` from a water source at the placement position, the common
/// `getStateForPlacement` idiom. States without the property are returned unchanged.
#[inline]
pub fn waterlogged(state: BlockState, ctx: &PlaceContext, level: &dyn BlockReader) -> BlockState {
    if state.has_prop("waterlogged") {
        state.set_bool("waterlogged", is_water_source(level, ctx.pos))
    } else {
        state
    }
}

/// `Block.defaultBlockState()` with [`waterlogged`] applied.
#[inline]
pub fn default_placement(
    block: BlockId,
    ctx: &PlaceContext,
    level: &dyn BlockReader,
) -> BlockState {
    waterlogged(block.default_state(), ctx, level)
}

/// `BlockBehaviour.canBeReplaced(state, context)`: the replaceable flag, unless the held item is
/// this block's own item.
#[inline]
pub fn default_can_be_replaced(state: BlockState, ctx: &PlaceContext) -> bool {
    state.replaceable() && state.block().info().item != ctx.item.0
}

/// Whether the held item is `block`'s item (`context.getItemInHand().is(block.asItem())`).
#[inline]
pub fn holds_item_of(ctx: &PlaceContext, block: BlockId) -> bool {
    block.info().item == ctx.item.0
}

/// Whether `pos` is inside the build height.
#[inline]
pub fn in_build_height(level: &dyn BlockReader, pos: BlockPos) -> bool {
    pos.y >= level.min_y() && pos.y < level.max_y()
}

/// Neighbour position.
#[inline]
pub fn rel(pos: BlockPos, direction: Direction) -> BlockPos {
    pos.relative(direction)
}

#[inline]
pub fn above(pos: BlockPos) -> BlockPos {
    pos.relative(Direction::Up)
}

#[inline]
pub fn below(pos: BlockPos) -> BlockPos {
    pos.relative(Direction::Down)
}

const EPS: f64 = 1.0e-7;

/// Whether `shape` has no boxes.
#[inline]
pub fn shape_is_empty(shape: Shape) -> bool {
    shape.is_empty()
}

/// The boxes of `shape` touching the face towards `direction`, projected to the face as
/// `[min_u, min_v, max_u, max_v]` (`VoxelShape.getFaceShape`).
fn face_boxes(shape: Shape, direction: Direction) -> impl Iterator<Item = [f64; 4]> {
    shape.iter().filter_map(move |b| {
        let (axis_min, axis_max, u0, v0, u1, v1) = match direction.axis() {
            Axis::X => (b[0], b[3], b[1], b[2], b[4], b[5]),
            Axis::Y => (b[1], b[4], b[0], b[2], b[3], b[5]),
            Axis::Z => (b[2], b[5], b[0], b[1], b[3], b[4]),
        };
        let touches = if direction.is_positive() {
            axis_max >= 1.0 - EPS
        } else {
            axis_min <= EPS
        };
        touches.then_some([u0, v0, u1, v1])
    })
}

/// Whether 2D rectangles cover `[x0, x1] × [y0, y1]`.
fn rects_cover(rects: &[[f64; 4]], x0: f64, y0: f64, x1: f64, y1: f64) -> bool {
    let mut xs: Vec<f64> = vec![x0, x1];
    let mut ys: Vec<f64> = vec![y0, y1];
    for r in rects {
        if r[0] > x0 && r[0] < x1 {
            xs.push(r[0]);
        }
        if r[2] > x0 && r[2] < x1 {
            xs.push(r[2]);
        }
        if r[1] > y0 && r[1] < y1 {
            ys.push(r[1]);
        }
        if r[3] > y0 && r[3] < y1 {
            ys.push(r[3]);
        }
    }
    xs.sort_by(f64::total_cmp);
    ys.sort_by(f64::total_cmp);
    for xw in xs.windows(2) {
        if xw[1] - xw[0] < EPS {
            continue;
        }
        let cx = (xw[0] + xw[1]) * 0.5;
        for yw in ys.windows(2) {
            if yw[1] - yw[0] < EPS {
                continue;
            }
            let cy = (yw[0] + yw[1]) * 0.5;
            if !rects
                .iter()
                .any(|r| r[0] <= cx && cx <= r[2] && r[1] <= cy && cy <= r[3])
            {
                return false;
            }
        }
    }
    true
}

/// `Block.isFaceFull(shape, direction)`: the face of `shape` towards `direction` covers the whole
/// block face.
pub fn face_full(shape: Shape, direction: Direction) -> bool {
    let rects: Vec<[f64; 4]> = face_boxes(shape, direction).collect();
    !rects.is_empty() && rects_cover(&rects, 0.0, 0.0, 1.0, 1.0)
}

/// Whether the face of `shape` towards `direction` covers the rectangle
/// `[u0, v0, u1, v1]` of that face (`!Shapes.joinIsNotEmpty(rect, face, ONLY_FIRST)`; u/v are the
/// two other axes in x, y, z order).
pub fn face_covers(shape: Shape, direction: Direction, rect: [f64; 4]) -> bool {
    let rects: Vec<[f64; 4]> = face_boxes(shape, direction).collect();
    rects_cover(&rects, rect[0], rect[1], rect[2], rect[3])
}

/// `Block.isShapeFullBlock(shape)`.
pub fn is_shape_full_block(shape: Shape) -> bool {
    // A shape is a full block when all six faces are full and the boxes fill the volume; the
    // generated shapes are merged boxes, so a single unit box is the only full block shape.
    shape.iter().any(|b| {
        b[0] <= EPS
            && b[1] <= EPS
            && b[2] <= EPS
            && b[3] >= 1.0 - EPS
            && b[4] >= 1.0 - EPS
            && b[5] >= 1.0 - EPS
    })
}
