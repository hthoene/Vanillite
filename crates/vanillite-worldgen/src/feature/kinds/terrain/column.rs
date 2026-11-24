//! `Column` (`world.level.levelgen.Column`): the free space between a floor and a ceiling.

use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::BlockState;

use crate::feature::region::WorldGenRegion;

/// A vertical column: optional floor and ceiling (the edge blocks' Y).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Column {
    pub floor: Option<i32>,
    pub ceiling: Option<i32>,
}

impl Column {
    /// `getHeight()`: the free space between floor and ceiling of a `Range`.
    pub fn height(&self) -> Option<i32> {
        match (self.floor, self.ceiling) {
            (Some(floor), Some(ceiling)) => Some(ceiling - floor - 1),
            _ => None,
        }
    }

    /// `Column.scan(level, pos, searchRange, insideColumn, validEdge)`: `None` if `pos` is not
    /// inside a column.
    pub fn scan(
        level: &WorldGenRegion<'_>,
        pos: BlockPos,
        search_range: i32,
        inside: impl Fn(BlockState) -> bool,
        valid_edge: impl Fn(BlockState) -> bool,
    ) -> Option<Self> {
        if !inside(level.block(pos)) {
            return None;
        }
        let ceiling = scan_direction(
            level,
            search_range,
            &inside,
            &valid_edge,
            pos,
            Direction::Up,
        );
        let floor = scan_direction(
            level,
            search_range,
            &inside,
            &valid_edge,
            pos,
            Direction::Down,
        );
        Some(Self { floor, ceiling })
    }
}

/// `Column.scanDirection`.
fn scan_direction(
    level: &WorldGenRegion<'_>,
    search_range: i32,
    inside: &impl Fn(BlockState) -> bool,
    valid_edge: &impl Fn(BlockState) -> bool,
    start: BlockPos,
    direction: Direction,
) -> Option<i32> {
    let mut pos = start;
    let mut i = 1;
    while i < search_range && inside(level.block(pos)) {
        pos = pos.relative(direction);
        i += 1;
    }
    valid_edge(level.block(pos)).then_some(pos.y)
}
