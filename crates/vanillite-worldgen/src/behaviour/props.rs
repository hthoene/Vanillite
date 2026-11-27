//! Direction helpers and typed access to block state properties.
//!
//! Vanilla reads properties through typed `Property` objects; here a property is addressed by its
//! name, and the helpers convert between the serialized values and Rust types.

use std::sync::OnceLock;
use vanillite_base::{BlockPos, Direction};

use vanillite_data::block::{BLOCK_COUNT, BlockId, BlockState, PROPERTIES, StateProperty};

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

/// How a block reads and sets one integer property without looking it up by name.
#[derive(Clone, Copy, Debug)]
enum IntEntry {
    /// The block does not have the property.
    Absent,
    /// Values `first..first + count` in index order.
    Range(StateProperty, i32, u16),
    /// Other values: read and set by name ([`StateExt::int`], [`StateExt::set_int`]).
    ByName,
}

/// An integer property of every block that has one named `name` (`distance`, …), read and set
/// as [`StateExt::int`] and [`StateExt::set_int`] do, from a table per block.
#[derive(Debug)]
pub struct IntProperty {
    name: &'static str,
    blocks: Box<[IntEntry]>,
}

impl IntProperty {
    fn new(name: &'static str) -> Self {
        let blocks = (0..BLOCK_COUNT)
            .map(|block| {
                let block = BlockId(block as u16);
                let Some(info) = block.info().properties.iter().find_map(|&p| {
                    let info = &PROPERTIES[usize::from(p)];
                    (info.name == name).then_some(info)
                }) else {
                    return IntEntry::Absent;
                };
                let first = info.values.first().and_then(|v| v.parse::<i32>().ok());
                match (first, StateProperty::of(block, name)) {
                    (Some(first), Some(property))
                        if info.values.iter().enumerate().all(|(i, v)| {
                            i32::try_from(i).is_ok_and(|i| v.parse() == Ok(first + i))
                        }) && u16::try_from(info.values.len()).is_ok() =>
                    {
                        IntEntry::Range(property, first, info.values.len() as u16)
                    }
                    _ => IntEntry::ByName,
                }
            })
            .collect();
        Self { name, blocks }
    }

    /// `distance` (leaves, scaffolding).
    pub fn distance() -> &'static Self {
        static DISTANCE: OnceLock<IntProperty> = OnceLock::new();
        DISTANCE.get_or_init(|| Self::new("distance"))
    }

    #[inline]
    fn entry(&self, state: BlockState) -> IntEntry {
        self.blocks
            .get(usize::from(state.block().0))
            .copied()
            .unwrap_or(IntEntry::ByName)
    }

    /// [`StateExt::int`].
    #[inline]
    pub fn get(&self, state: BlockState) -> Option<i32> {
        match self.entry(state) {
            IntEntry::Absent => None,
            IntEntry::Range(property, first, _) => Some(first + i32::from(property.index(state))),
            IntEntry::ByName => state.int(self.name),
        }
    }

    /// [`StateExt::set_int`].
    #[inline]
    pub fn set(&self, state: BlockState, value: i32) -> BlockState {
        match self.entry(state) {
            IntEntry::Absent => state,
            IntEntry::Range(property, first, count) => match value.checked_sub(first) {
                Some(index) if (0..i32::from(count)).contains(&index) => {
                    property.with_index(state, index as u16)
                }
                _ => state,
            },
            IntEntry::ByName => state.set_int(self.name, value),
        }
    }
}

/// Per block: its `waterlogged` property and the index of `true`, if it has the property.
type WaterloggedTable = Box<[Option<(StateProperty, u16)>]>;

/// `waterlogged` of every block that has it, read without looking it up by name.
fn waterlogged_table() -> &'static [Option<(StateProperty, u16)>] {
    static TABLE: OnceLock<WaterloggedTable> = OnceLock::new();
    TABLE.get_or_init(|| {
        (0..BLOCK_COUNT)
            .map(|block| {
                let property = StateProperty::of(BlockId(block as u16), "waterlogged")?;
                Some((property, property.value_index("true")?))
            })
            .collect()
    })
}

impl StateExt for BlockState {
    #[inline]
    fn is(self, block: BlockId) -> bool {
        self.block() == block
    }

    #[inline]
    fn waterlogged(self) -> bool {
        match waterlogged_table().get(usize::from(self.block().0)) {
            Some(Some((property, true_index))) => property.index(self) == *true_index,
            Some(None) => false,
            None => self.flag("waterlogged"),
        }
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

/// The properties of `minecraft:redstone_wire`, resolved once: the wire's shape and signal code
/// reads and writes them on every update.
#[derive(Debug)]
pub struct WireProperties {
    /// `power` (0..=15).
    pub power: vanillite_data::block::StateProperty,
    /// The side properties in [`HORIZONTAL`] order.
    sides: [vanillite_data::block::StateProperty; 4],
    /// Value indices of the side values.
    pub up: u16,
    pub side: u16,
    pub none: u16,
}

impl WireProperties {
    /// The side property facing `direction` (horizontal).
    #[inline]
    pub fn side_of(&self, direction: Direction) -> vanillite_data::block::StateProperty {
        self.sides[match direction {
            Direction::North => 0,
            Direction::East => 1,
            Direction::South => 2,
            _ => 3,
        }]
    }

    /// The value index of a side value name (`up`, `side`, `none`).
    #[inline]
    pub fn side_index(&self, value: &str) -> u16 {
        match value {
            "up" => self.up,
            "side" => self.side,
            _ => self.none,
        }
    }

    /// Whether the wire state is connected towards `direction` (its side is not `none`).
    #[inline]
    pub fn connected(&self, state: BlockState, direction: Direction) -> bool {
        self.side_of(direction).index(state) != self.none
    }
}

/// The redstone wire's properties.
#[inline]
pub fn wire_properties() -> &'static WireProperties {
    static PROPERTIES: std::sync::OnceLock<WireProperties> = std::sync::OnceLock::new();
    PROPERTIES.get_or_init(|| {
        let wire = vanillite_data::block::ids::REDSTONE_WIRE;
        let property = |name| {
            vanillite_data::block::StateProperty::of(wire, name)
                .expect("redstone wire has its properties")
        };
        let north = property("north");
        let value = |v| north.value_index(v).expect("wire side values");
        WireProperties {
            power: property("power"),
            sides: HORIZONTAL.map(|d| property(direction_name(d))),
            up: value("up"),
            side: value("side"),
            none: value("none"),
        }
    })
}
