//! `PortalShape`: the obsidian frame of a nether portal, found from a block inside it.
//!
//! Shared by the portal block's shape update (a broken frame removes the portal) and the live
//! world (lighting a frame, creating exit portals).

use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockState, ids};

use super::{BlockLevel, StateExt, tags};

/// `PortalShape.MAX_WIDTH` and `MAX_HEIGHT`.
pub const MAX_PORTAL_SIZE: i32 = 21;
const MIN_WIDTH: i32 = 2;
const MIN_HEIGHT: i32 = 3;

/// A horizontal axis (`Direction.Axis.X` or `Z`), the `axis` property of nether portals.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HorizontalAxis {
    /// The portal plane runs along X.
    X,
    /// The portal plane runs along Z.
    Z,
}

impl HorizontalAxis {
    /// The other horizontal axis.
    pub fn other(self) -> Self {
        match self {
            Self::X => Self::Z,
            Self::Z => Self::X,
        }
    }

    /// The property value (`x` or `z`).
    pub fn name(self) -> &'static str {
        match self {
            Self::X => "x",
            Self::Z => "z",
        }
    }

    /// The `axis` property of a state, if it has a horizontal one.
    pub fn of(state: BlockState) -> Option<Self> {
        match state.value("axis") {
            Some("x") => Some(Self::X),
            Some("z") => Some(Self::Z),
            _ => None,
        }
    }

    /// The axis of a horizontal direction (`Direction.getAxis`); `None` for up and down.
    pub fn of_direction(direction: Direction) -> Option<Self> {
        match direction {
            Direction::East | Direction::West => Some(Self::X),
            Direction::North | Direction::South => Some(Self::Z),
            Direction::Up | Direction::Down => None,
        }
    }

    /// `Direction.get(POSITIVE, axis)`.
    pub fn positive(self) -> Direction {
        match self {
            Self::X => Direction::East,
            Self::Z => Direction::South,
        }
    }

    /// The coordinate of `pos` along this axis.
    pub fn coord(self, pos: BlockPos) -> i32 {
        match self {
            Self::X => pos.x,
            Self::Z => pos.z,
        }
    }
}

/// `pos.relative(direction, distance)`.
pub fn relative_n(pos: BlockPos, direction: Direction, distance: i32) -> BlockPos {
    let [x, y, z] = direction.normal();
    pos.offset(x * distance, y * distance, z * distance)
}

/// `PortalShape.FRAME`: `#minecraft:nether_portal_frame`.
pub fn is_frame(state: BlockState) -> bool {
    tags().blocks.nether_portal_frame.contains(state)
}

/// `PortalShape.isEmpty`: air, fire or a portal block.
fn is_empty(state: BlockState) -> bool {
    state.is_air() || tags().blocks.fire.contains(state) || state.is(ids::NETHER_PORTAL)
}

/// A nether portal frame (`PortalShape`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortalShape {
    /// The portal's axis.
    pub axis: HorizontalAxis,
    /// Portal blocks already inside the frame.
    pub portal_blocks: i32,
    /// `rightDir`: west for X portals, south for Z portals.
    pub right: Direction,
    /// The lowest inner block on the left.
    pub bottom_left: BlockPos,
    /// Inner height (0 if invalid).
    pub height: i32,
    /// Inner width (0 if invalid).
    pub width: i32,
}

impl PortalShape {
    /// `findAnyShape`: the frame around `pos` in the plane of `axis`, valid or not.
    pub fn find_any(level: &impl BlockLevel, pos: BlockPos, axis: HorizontalAxis) -> Self {
        let right = match axis {
            HorizontalAxis::X => Direction::West,
            HorizontalAxis::Z => Direction::South,
        };
        let empty = |bottom_left| PortalShape {
            axis,
            portal_blocks: 0,
            right,
            bottom_left,
            height: 0,
            width: 0,
        };
        let Some(bottom_left) = calculate_bottom_left(level, right, pos) else {
            return empty(pos);
        };
        let width = distance_until_edge_above_frame(level, bottom_left, right);
        if !(MIN_WIDTH..=MAX_PORTAL_SIZE).contains(&width) {
            return empty(bottom_left);
        }
        let mut portal_blocks = 0;
        let mut height = distance_until_top(level, bottom_left, right, width, &mut portal_blocks);
        if !(MIN_HEIGHT..=MAX_PORTAL_SIZE).contains(&height)
            || !has_top_frame(level, bottom_left, right, width, height)
        {
            height = 0;
        }
        PortalShape {
            axis,
            portal_blocks,
            right,
            bottom_left,
            height,
            width,
        }
    }

    /// `findPortalShape(level, pos, valid, preferredAxis)`: the preferred axis first, then the
    /// other one.
    pub fn find(
        level: &impl BlockLevel,
        pos: BlockPos,
        valid: impl Fn(&PortalShape) -> bool,
        preferred: HorizontalAxis,
    ) -> Option<Self> {
        let first = Self::find_any(level, pos, preferred);
        if valid(&first) {
            return Some(first);
        }
        let second = Self::find_any(level, pos, preferred.other());
        valid(&second).then_some(second)
    }

    /// `findEmptyPortalShape`: a valid frame without portal blocks.
    pub fn find_empty(
        level: &impl BlockLevel,
        pos: BlockPos,
        preferred: HorizontalAxis,
    ) -> Option<Self> {
        Self::find(
            level,
            pos,
            |s| s.is_valid() && s.portal_blocks == 0,
            preferred,
        )
    }

    /// `isValid`.
    pub fn is_valid(&self) -> bool {
        (MIN_WIDTH..=MAX_PORTAL_SIZE).contains(&self.width)
            && (MIN_HEIGHT..=MAX_PORTAL_SIZE).contains(&self.height)
    }

    /// `isComplete`: valid and filled with portal blocks.
    pub fn is_complete(&self) -> bool {
        self.is_valid() && self.portal_blocks == self.width * self.height
    }

    /// The portal state of this frame.
    pub fn portal_state(&self) -> BlockState {
        ids::NETHER_PORTAL
            .default_state()
            .set("axis", self.axis.name())
    }

    /// The inner positions in `createPortalBlocks` order (`BlockPos.betweenClosed`: x fastest,
    /// then y, then z).
    pub fn inner(&self) -> impl Iterator<Item = BlockPos> + use<> {
        let a = self.bottom_left;
        let b = relative_n(a.offset(0, self.height - 1, 0), self.right, self.width - 1);
        let (x0, x1) = (a.x.min(b.x), a.x.max(b.x));
        let (y0, y1) = (a.y.min(b.y), a.y.max(b.y));
        let (z0, z1) = (a.z.min(b.z), a.z.max(b.z));
        (z0..=z1).flat_map(move |z| {
            (y0..=y1).flat_map(move |y| (x0..=x1).map(move |x| BlockPos::new(x, y, z)))
        })
    }
}

fn calculate_bottom_left(
    level: &impl BlockLevel,
    right: Direction,
    mut pos: BlockPos,
) -> Option<BlockPos> {
    let min_y = level.min_y().max(pos.y - MAX_PORTAL_SIZE);
    while pos.y > min_y && is_empty(level.block(pos.offset(0, -1, 0))) {
        pos = pos.offset(0, -1, 0);
    }
    let left = right.opposite();
    let edge = distance_until_edge_above_frame(level, pos, left) - 1;
    (edge >= 0).then(|| relative_n(pos, left, edge))
}

fn distance_until_edge_above_frame(
    level: &impl BlockLevel,
    pos: BlockPos,
    direction: Direction,
) -> i32 {
    for width in 0..=MAX_PORTAL_SIZE {
        let p = relative_n(pos, direction, width);
        let state = level.block(p);
        if !is_empty(state) {
            if is_frame(state) {
                return width;
            }
            break;
        }
        if !is_frame(level.block(p.offset(0, -1, 0))) {
            break;
        }
    }
    0
}

fn has_top_frame(
    level: &impl BlockLevel,
    bottom_left: BlockPos,
    right: Direction,
    width: i32,
    height: i32,
) -> bool {
    (0..width)
        .all(|i| is_frame(level.block(relative_n(bottom_left.offset(0, height, 0), right, i))))
}

fn distance_until_top(
    level: &impl BlockLevel,
    bottom_left: BlockPos,
    right: Direction,
    width: i32,
    portal_blocks: &mut i32,
) -> i32 {
    for height in 0..MAX_PORTAL_SIZE {
        let row = bottom_left.offset(0, height, 0);
        if !is_frame(level.block(relative_n(row, right, -1))) {
            return height;
        }
        if !is_frame(level.block(relative_n(row, right, width))) {
            return height;
        }
        for i in 0..width {
            let state = level.block(relative_n(row, right, i));
            if !is_empty(state) {
                return height;
            }
            if state.is(ids::NETHER_PORTAL) {
                *portal_blocks += 1;
            }
        }
    }
    MAX_PORTAL_SIZE
}
