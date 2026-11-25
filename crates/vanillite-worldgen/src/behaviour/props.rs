//! Direction helpers and typed access to block state properties.
//!
//! Vanilla reads properties through typed `Property` objects; here a property is addressed by its
//! name, and the helpers convert between the serialized values and Rust types.

use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockId, BlockState};

/// `Direction.Plane.HORIZONTAL` in Vanilla's order.
pub const HORIZONTAL: [Direction; 4] = [
    Direction::North,
    Direction::East,
    Direction::South,
    Direction::West,
];

/// `BlockBehaviour.UPDATE_SHAPE_ORDER`: the order of `Block.updateFromNeighbourShapes`.
pub const UPDATE_SHAPE_ORDER: [Direction; 6] = [
    Direction::West,
    Direction::East,
    Direction::North,
    Direction::South,
    Direction::Down,
    Direction::Up,
];

/// `Direction.Axis`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Axis {
    X,
    Y,
    Z,
}

/// `direction.getAxis()`.
#[inline]
pub(crate) const fn axis(direction: Direction) -> Axis {
    match direction {
        Direction::Down | Direction::Up => Axis::Y,
        Direction::North | Direction::South => Axis::Z,
        Direction::West | Direction::East => Axis::X,
    }
}

/// `direction.getAxis().isHorizontal()`.
#[inline]
pub const fn is_horizontal(direction: Direction) -> bool {
    !matches!(direction, Direction::Down | Direction::Up)
}

/// `direction.getClockWise()` around the Y axis (vertical directions are returned unchanged;
/// Vanilla throws for them, no caller passes one).
#[inline]
pub const fn clockwise(direction: Direction) -> Direction {
    match direction {
        Direction::North => Direction::East,
        Direction::East => Direction::South,
        Direction::South => Direction::West,
        Direction::West => Direction::North,
        vertical => vertical,
    }
}

/// `direction.getCounterClockWise()` around the Y axis.
#[inline]
pub const fn counter_clockwise(direction: Direction) -> Direction {
    match direction {
        Direction::North => Direction::West,
        Direction::West => Direction::South,
        Direction::South => Direction::East,
        Direction::East => Direction::North,
        vertical => vertical,
    }
}

/// `direction.getSerializedName()`.
#[inline]
pub(crate) const fn direction_name(direction: Direction) -> &'static str {
    match direction {
        Direction::Down => "down",
        Direction::Up => "up",
        Direction::North => "north",
        Direction::South => "south",
        Direction::West => "west",
        Direction::East => "east",
    }
}

/// The direction with serialized name `name`.
#[inline]
pub(crate) fn direction_by_name(name: &str) -> Option<Direction> {
    Some(match name {
        "down" => Direction::Down,
        "up" => Direction::Up,
        "north" => Direction::North,
        "south" => Direction::South,
        "west" => Direction::West,
        "east" => Direction::East,
        _ => return None,
    })
}

/// `BlockPos.above()` and `below()`.
pub trait PosExt {
    fn above(self) -> Self;
    fn below(self) -> Self;
}

impl PosExt for BlockPos {
    #[inline]
    fn above(self) -> Self {
        self.offset(0, 1, 0)
    }

    #[inline]
    fn below(self) -> Self {
        self.offset(0, -1, 0)
    }
}

/// Typed property access on block states.
pub trait StateExt: Sized {
    /// `state.is(block)`.
    fn is(self, block: BlockId) -> bool;
    /// A boolean property (`false` if the state does not have it, as `getValueOrElse`).
    fn flag(self, property: &str) -> bool;
    /// A direction property (`facing`, `tip_direction`, …).
    fn direction(self, property: &str) -> Option<Direction>;
    /// An integer property (`age`, `distance`, …).
    fn int(self, property: &str) -> Option<i32>;
    /// `trySetValue(property, value)`: the state with a property set by its serialized value,
    /// or the state unchanged if it has no such property or value.
    fn set(self, property: &str, value: &str) -> Self;
    /// [`Self::set`] for a boolean property.
    fn set_flag(self, property: &str, value: bool) -> Self;
    /// [`Self::set`] for a direction property.
    fn set_direction(self, property: &str, value: Direction) -> Self;
    /// [`Self::set`] for an integer property.
    fn set_int(self, property: &str, value: i32) -> Self;
    /// `state.getValue(WATERLOGGED)`, `false` for blocks without the property.
    fn waterlogged(self) -> bool {
        self.flag("waterlogged")
    }
}

impl StateExt for BlockState {
    #[inline]
    fn is(self, block: BlockId) -> bool {
        self.block() == block
    }

    #[inline]
    fn flag(self, property: &str) -> bool {
        self.value(property) == Some("true")
    }

    #[inline]
    fn direction(self, property: &str) -> Option<Direction> {
        self.value(property).and_then(direction_by_name)
    }

    #[inline]
    fn int(self, property: &str) -> Option<i32> {
        self.value(property).and_then(|v| v.parse().ok())
    }

    #[inline]
    fn set(self, property: &str, value: &str) -> Self {
        self.with(property, value).unwrap_or(self)
    }

    #[inline]
    fn set_flag(self, property: &str, value: bool) -> Self {
        self.set(property, if value { "true" } else { "false" })
    }

    #[inline]
    fn set_direction(self, property: &str, value: Direction) -> Self {
        self.set(property, direction_name(value))
    }

    fn set_int(self, property: &str, value: i32) -> Self {
        // The value's position without formatting it (integer values are in ascending order).
        let Some(prop) = self.block().info().properties.iter().find_map(|&p| {
            let prop = &vanillite_data::block::PROPERTIES[usize::from(p)];
            (prop.name == property).then_some(prop)
        }) else {
            return self;
        };
        match prop.values.iter().position(|v| v.parse() == Ok(value)) {
            Some(index) => self.with_index(property, index as u16).unwrap_or(self),
            None => self,
        }
    }
}

/// `block.withPropertiesOf(state)`: the default state of `block` with every property of
/// `state` that `block` also has.
pub fn with_properties_of(block: BlockId, state: BlockState) -> BlockState {
    state
        .properties()
        .fold(block.default_state(), |result, (name, value)| {
            result.set(name, value)
        })
}

/// `state.cycle(property)`: the next value of a property in Vanilla's value order, wrapping.
pub(crate) fn cycle(state: BlockState, property: &str) -> BlockState {
    let info = state.block().info();
    for &p in info.properties {
        let prop = &vanillite_data::block::PROPERTIES[usize::from(p)];
        if prop.name == property {
            let Some(current) = state.value(property) else {
                return state;
            };
            let index = prop.values.iter().position(|v| *v == current).unwrap_or(0);
            return state.set(property, prop.values[(index + 1) % prop.values.len()]);
        }
    }
    state
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_horizontally() {
        for d in HORIZONTAL {
            assert_eq!(counter_clockwise(clockwise(d)), d);
            assert_eq!(clockwise(clockwise(d)), d.opposite());
        }
    }
}
