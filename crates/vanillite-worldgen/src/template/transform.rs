//! Rotations and mirrors of positions and block states (`Rotation`, `Mirror`,
//! `StructureTemplate.transform`, `BlockState.rotate`/`mirror`).
//!
//! [`rotate_state`] and [`mirror_state`] port every `Block.rotate`/`Block.mirror` override of
//! Vanilla 26.3, keyed by the block's properties (see [`StateKind`]).

use std::sync::OnceLock;

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Direction, Vec3};
use vanillite_data::block::{BLOCK_COUNT, BlockId, BlockState, PROPERTIES};

use crate::Error;

/// `net.minecraft.world.level.block.Rotation`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Rotation {
    #[default]
    None,
    Clockwise90,
    Clockwise180,
    Counterclockwise90,
}

impl Rotation {
    /// `Rotation.values()`, in Vanilla's order.
    pub const ALL: [Self; 4] = [
        Self::None,
        Self::Clockwise90,
        Self::Clockwise180,
        Self::Counterclockwise90,
    ];

    /// Parses the serialized name (`none`, `clockwise_90`, `180`, `counterclockwise_90`).
    pub fn from_name(name: &str) -> Result<Self, Error> {
        Ok(match name {
            "none" => Self::None,
            "clockwise_90" => Self::Clockwise90,
            "180" => Self::Clockwise180,
            "counterclockwise_90" => Self::Counterclockwise90,
            other => return Err(Error::new(format!("unknown rotation {other}"))),
        })
    }

    /// `Rotation.getRandom(random)`: `values()[nextInt(4)]`.
    pub fn random<R: RandomSource + ?Sized>(random: &mut R) -> Self {
        Self::ALL[random.next_int_bounded(4) as usize]
    }

    /// `getRotated(rot)`: this rotation followed by `other`.
    pub fn rotated(self, other: Self) -> Self {
        Self::ALL[(self as usize + other as usize) % 4]
    }

    /// `rotate(direction)`: vertical directions stay.
    pub fn rotate(self, direction: Direction) -> Direction {
        if matches!(direction, Direction::Up | Direction::Down) {
            return direction;
        }
        match self {
            Self::None => direction,
            Self::Clockwise90 => clockwise(direction),
            Self::Clockwise180 => direction.opposite(),
            Self::Counterclockwise90 => counter_clockwise(direction),
        }
    }

    /// `rotate(rotation, steps)` for segmented rotations (signs, banners, skulls: 16 steps).
    pub fn rotate_segment(self, rotation: i32, steps: i32) -> i32 {
        match self {
            Self::None => rotation,
            Self::Clockwise90 => (rotation + steps / 4) % steps,
            Self::Clockwise180 => (rotation + steps / 2) % steps,
            Self::Counterclockwise90 => (rotation + steps * 3 / 4) % steps,
        }
    }
}

/// `net.minecraft.world.level.block.Mirror`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Mirror {
    #[default]
    None,
    /// Mirrors along Z (north ↔ south).
    LeftRight,
    /// Mirrors along X (east ↔ west).
    FrontBack,
}

impl Mirror {
    /// Parses the serialized name (`none`, `left_right`, `front_back`).
    pub fn from_name(name: &str) -> Result<Self, Error> {
        Ok(match name {
            "none" => Self::None,
            "left_right" => Self::LeftRight,
            "front_back" => Self::FrontBack,
            other => return Err(Error::new(format!("unknown mirror {other}"))),
        })
    }

    /// `mirror(rotation, steps)` for segmented rotations.
    pub fn mirror_segment(self, rotation: i32, steps: i32) -> i32 {
        let half = steps / 2;
        let corrected = if rotation > half {
            rotation - steps
        } else {
            rotation
        };
        match self {
            Self::None => rotation,
            Self::LeftRight => (half - corrected + steps) % steps,
            Self::FrontBack => (steps - corrected) % steps,
        }
    }

    /// `getRotation(direction)`: the rotation that mirrors a facing of `direction`.
    pub fn rotation_for(self, direction: Direction) -> Rotation {
        let z = matches!(direction, Direction::North | Direction::South);
        let x = matches!(direction, Direction::West | Direction::East);
        if (self == Self::LeftRight && z) || (self == Self::FrontBack && x) {
            Rotation::Clockwise180
        } else {
            Rotation::None
        }
    }

    /// `mirror(direction)`.
    pub fn mirror(self, direction: Direction) -> Direction {
        match (self, direction) {
            (Self::FrontBack, Direction::West | Direction::East)
            | (Self::LeftRight, Direction::North | Direction::South) => direction.opposite(),
            _ => direction,
        }
    }
}

/// `Direction.getClockWise()` of a horizontal direction (vertical ones stay).
pub fn clockwise(direction: Direction) -> Direction {
    match direction {
        Direction::North => Direction::East,
        Direction::East => Direction::South,
        Direction::South => Direction::West,
        Direction::West => Direction::North,
        other => other,
    }
}

/// `Direction.getCounterClockWise()` of a horizontal direction (vertical ones stay).
pub fn counter_clockwise(direction: Direction) -> Direction {
    match direction {
        Direction::North => Direction::West,
        Direction::West => Direction::South,
        Direction::South => Direction::East,
        Direction::East => Direction::North,
        other => other,
    }
}

/// A direction by its serialized name.
pub fn direction_by_name(name: &str) -> Option<Direction> {
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

/// The serialized name of a direction.
pub fn direction_name(direction: Direction) -> &'static str {
    match direction {
        Direction::Down => "down",
        Direction::Up => "up",
        Direction::North => "north",
        Direction::South => "south",
        Direction::West => "west",
        Direction::East => "east",
    }
}

/// `StructureTemplate.transform(pos, mirror, rotation, pivot)`.
pub fn transform(pos: BlockPos, mirror: Mirror, rotation: Rotation, pivot: BlockPos) -> BlockPos {
    let (mut x, y, mut z) = (pos.x, pos.y, pos.z);
    match mirror {
        Mirror::LeftRight => z = z.wrapping_neg(),
        Mirror::FrontBack => x = x.wrapping_neg(),
        Mirror::None => {}
    }
    let (px, pz) = (pivot.x, pivot.z);
    match rotation {
        Rotation::Counterclockwise90 => BlockPos::new(
            px.wrapping_sub(pz).wrapping_add(z),
            y,
            px.wrapping_add(pz).wrapping_sub(x),
        ),
        Rotation::Clockwise90 => BlockPos::new(
            px.wrapping_add(pz).wrapping_sub(z),
            y,
            pz.wrapping_sub(px).wrapping_add(x),
        ),
        Rotation::Clockwise180 => BlockPos::new(
            px.wrapping_add(px).wrapping_sub(x),
            y,
            pz.wrapping_add(pz).wrapping_sub(z),
        ),
        Rotation::None => BlockPos::new(x, y, z),
    }
}

/// `StructureTemplate.transform(Vec3, mirror, rotation, pivot)` (entity positions).
pub fn transform_vec3(pos: Vec3, mirror: Mirror, rotation: Rotation, pivot: BlockPos) -> Vec3 {
    let (mut x, y, mut z) = (pos.x, pos.y, pos.z);
    match mirror {
        Mirror::LeftRight => z = 1.0 - z,
        Mirror::FrontBack => x = 1.0 - x,
        Mirror::None => {}
    }
    let (px, pz) = (pivot.x, pivot.z);
    match rotation {
        Rotation::Counterclockwise90 => Vec3::new(
            f64::from(px.wrapping_sub(pz)) + z,
            y,
            f64::from(px.wrapping_add(pz).wrapping_add(1)) - x,
        ),
        Rotation::Clockwise90 => Vec3::new(
            f64::from(px.wrapping_add(pz).wrapping_add(1)) - z,
            y,
            f64::from(pz.wrapping_sub(px)) + x,
        ),
        Rotation::Clockwise180 => Vec3::new(
            f64::from(px.wrapping_add(px).wrapping_add(1)) - x,
            y,
            f64::from(pz.wrapping_add(pz).wrapping_add(1)) - z,
        ),
        Rotation::None => Vec3::new(x, y, z),
    }
}

/// How a block rotates and mirrors: which `Block.rotate`/`Block.mirror` override applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StateKind {
    /// No override (`BlockBehaviour.rotate`/`mirror` return the state), e.g. fire and chorus
    /// plants whose `north`/`east`/… properties are not rotated.
    Fixed,
    /// `facing` rotated; mirrored through `rotate(mirror.getRotation(facing))`
    /// (`HorizontalDirectionalBlock`, `DirectionalBlock` subclasses, chests, hoppers, …).
    Facing,
    /// `AnvilBlock`: `facing` rotated, no mirror.
    FacingRotateOnly,
    /// `DoorBlock`: like [`Self::Facing`], mirroring also cycles `hinge`.
    Door,
    /// `StairBlock`: `facing` rotated; mirroring flips the shape.
    Stairs,
    /// `RotatedPillarBlock.rotatePillar` (and `NetherPortalBlock`): `axis` x ↔ z, no mirror.
    Pillar,
    /// `rotation` in 16 segments (banners, standing and ceiling hanging signs, skulls).
    Segments,
    /// `orientation` (`FrontAndTop` of jigsaws and crafters).
    Orientation,
    /// `BaseRailBlock` shapes.
    Rail,
    /// `north`/`east`/`south`/`west` permuted (`CrossCollisionBlock`, `WallBlock`, `VineBlock`,
    /// `RedstoneWireBlock`, `TripWireBlock`, `MossyCarpetBlock`, `HugeMushroomBlock`,
    /// `MultifaceBlock`).
    FourWay,
}

fn kinds() -> &'static [StateKind] {
    static KINDS: OnceLock<Vec<StateKind>> = OnceLock::new();
    KINDS.get_or_init(|| {
        (0..BLOCK_COUNT)
            .map(|id| kind_of(BlockId(id as u16)))
            .collect()
    })
}

fn kind_of(block: BlockId) -> StateKind {
    let info = block.info();
    let has = |name: &str| {
        info.properties
            .iter()
            .any(|&p| PROPERTIES[usize::from(p)].name == name)
    };
    match info.name {
        // `FireBlock` (`BaseFireBlock`) and `ChorusPlantBlock` (`PipeBlock`) have direction
        // properties but no rotate/mirror override.
        "minecraft:fire" | "minecraft:chorus_plant" => return StateKind::Fixed,
        "minecraft:anvil" | "minecraft:chipped_anvil" | "minecraft:damaged_anvil" => {
            return StateKind::FacingRotateOnly;
        }
        _ => {}
    }
    if has("orientation") {
        StateKind::Orientation
    } else if has("rotation") {
        StateKind::Segments
    } else if has("facing") {
        if has("hinge") {
            StateKind::Door
        } else if has("shape") {
            StateKind::Stairs
        } else {
            StateKind::Facing
        }
    } else if has("shape") {
        StateKind::Rail
    } else if has("axis") {
        StateKind::Pillar
    } else if has("north") && has("east") && has("south") && has("west") {
        StateKind::FourWay
    } else {
        StateKind::Fixed
    }
}

fn kind(state: BlockState) -> StateKind {
    kinds()[usize::from(state.block().0)]
}

fn set(state: BlockState, property: &str, value: &str) -> BlockState {
    state.with(property, value).unwrap_or(state)
}

fn get_direction(state: BlockState, property: &str) -> Option<Direction> {
    state.value(property).and_then(direction_by_name)
}

fn set_direction(state: BlockState, property: &str, direction: Direction) -> BlockState {
    set(state, property, direction_name(direction))
}

fn rotate_facing(state: BlockState, rotation: Rotation) -> BlockState {
    match get_direction(state, "facing") {
        Some(facing) => set_direction(state, "facing", rotation.rotate(facing)),
        None => state,
    }
}

/// Permutes the four horizontal direction properties: the value of `from(d)` moves to `d`.
fn permute_four(state: BlockState, from: impl Fn(Direction) -> Direction) -> BlockState {
    const SIDES: [Direction; 4] = [
        Direction::North,
        Direction::East,
        Direction::South,
        Direction::West,
    ];
    let mut values = [""; 4];
    for (value, side) in values.iter_mut().zip(SIDES) {
        *value = state.value(direction_name(from(side))).unwrap_or_default();
    }
    let mut result = state;
    for (value, side) in values.into_iter().zip(SIDES) {
        result = set(result, direction_name(side), value);
    }
    result
}

const FRONT_AND_TOP: [&str; 12] = [
    "down_east",
    "down_north",
    "down_south",
    "down_west",
    "up_east",
    "up_north",
    "up_south",
    "up_west",
    "west_up",
    "east_up",
    "north_up",
    "south_up",
];

/// `OctahedralGroup.rotate(FrontAndTop)` for the Y rotations and X/Z inversions of
/// `Rotation.rotation()` and `Mirror.rotation()`: both directions are mapped.
fn map_orientation(state: BlockState, map: impl Fn(Direction) -> Direction) -> BlockState {
    let Some(value) = state.value("orientation") else {
        return state;
    };
    let Some((front, top)) = value
        .split_once('_')
        .and_then(|(f, t)| Some((direction_by_name(f)?, direction_by_name(t)?)))
    else {
        return state;
    };
    let (front, top) = (map(front), map(top));
    let wanted = (direction_name(front), direction_name(top));
    FRONT_AND_TOP
        .iter()
        .find(|name| name.split_once('_') == Some(wanted))
        .map_or(state, |name| set(state, "orientation", name))
}

fn rotate_segments(state: BlockState, f: impl Fn(i32) -> i32) -> BlockState {
    match state.value("rotation").and_then(|v| v.parse::<i32>().ok()) {
        Some(value) => set(state, "rotation", &f(value).to_string()),
        None => state,
    }
}

/// `BaseRailBlock.rotate(RailShape, Rotation)`.
fn rotate_rail(shape: &str, rotation: Rotation) -> &str {
    match rotation {
        Rotation::None => shape,
        Rotation::Clockwise180 => match shape {
            "ascending_east" => "ascending_west",
            "ascending_west" => "ascending_east",
            "ascending_north" => "ascending_south",
            "ascending_south" => "ascending_north",
            "south_east" => "north_west",
            "south_west" => "north_east",
            "north_west" => "south_east",
            "north_east" => "south_west",
            other => other,
        },
        Rotation::Counterclockwise90 => match shape {
            "ascending_east" => "ascending_north",
            "ascending_west" => "ascending_south",
            "ascending_north" => "ascending_west",
            "ascending_south" => "ascending_east",
            "north_south" => "east_west",
            "east_west" => "north_south",
            "south_east" => "north_east",
            "south_west" => "south_east",
            "north_west" => "south_west",
            "north_east" => "north_west",
            other => other,
        },
        Rotation::Clockwise90 => match shape {
            "ascending_east" => "ascending_south",
            "ascending_west" => "ascending_north",
            "ascending_north" => "ascending_east",
            "ascending_south" => "ascending_west",
            "north_south" => "east_west",
            "east_west" => "north_south",
            "south_east" => "south_west",
            "south_west" => "north_west",
            "north_west" => "north_east",
            "north_east" => "south_east",
            other => other,
        },
    }
}

/// `BaseRailBlock.mirror(RailShape, Mirror)`.
fn mirror_rail(shape: &str, mirror: Mirror) -> &str {
    match mirror {
        Mirror::None => shape,
        Mirror::LeftRight => match shape {
            "ascending_north" => "ascending_south",
            "ascending_south" => "ascending_north",
            "south_east" => "north_east",
            "south_west" => "north_west",
            "north_west" => "south_west",
            "north_east" => "south_east",
            other => other,
        },
        Mirror::FrontBack => match shape {
            "ascending_east" => "ascending_west",
            "ascending_west" => "ascending_east",
            "south_east" => "south_west",
            "south_west" => "south_east",
            "north_west" => "north_east",
            "north_east" => "north_west",
            other => other,
        },
    }
}

/// `state.rotate(rotation)`.
pub fn rotate_state(state: BlockState, rotation: Rotation) -> BlockState {
    if rotation == Rotation::None {
        return state;
    }
    match kind(state) {
        StateKind::Fixed => state,
        StateKind::Facing | StateKind::FacingRotateOnly | StateKind::Door | StateKind::Stairs => {
            rotate_facing(state, rotation)
        }
        StateKind::Pillar => match (rotation, state.value("axis")) {
            (Rotation::Clockwise90 | Rotation::Counterclockwise90, Some("x")) => {
                set(state, "axis", "z")
            }
            (Rotation::Clockwise90 | Rotation::Counterclockwise90, Some("z")) => {
                set(state, "axis", "x")
            }
            _ => state,
        },
        StateKind::Segments => rotate_segments(state, |r| rotation.rotate_segment(r, 16)),
        StateKind::Orientation => map_orientation(state, |d| rotation.rotate(d)),
        StateKind::Rail => match state.value("shape") {
            Some(shape) => set(state, "shape", rotate_rail(shape, rotation)),
            None => state,
        },
        // The value of the side that rotates onto `d` comes from the inverse rotation of `d`.
        StateKind::FourWay => {
            let inverse = match rotation {
                Rotation::Clockwise90 => Rotation::Counterclockwise90,
                Rotation::Counterclockwise90 => Rotation::Clockwise90,
                other => other,
            };
            permute_four(state, |d| inverse.rotate(d))
        }
    }
}

/// `state.mirror(mirror)`.
pub fn mirror_state(state: BlockState, mirror: Mirror) -> BlockState {
    if mirror == Mirror::None {
        return state;
    }
    match kind(state) {
        StateKind::Fixed | StateKind::FacingRotateOnly | StateKind::Pillar => state,
        StateKind::Facing => match get_direction(state, "facing") {
            Some(facing) => rotate_state(state, mirror.rotation_for(facing)),
            None => state,
        },
        StateKind::Door => {
            let rotated = match get_direction(state, "facing") {
                Some(facing) => rotate_state(state, mirror.rotation_for(facing)),
                None => state,
            };
            match rotated.value("hinge") {
                Some("left") => set(rotated, "hinge", "right"),
                Some("right") => set(rotated, "hinge", "left"),
                _ => rotated,
            }
        }
        StateKind::Stairs => mirror_stairs(state, mirror),
        StateKind::Segments => rotate_segments(state, |r| mirror.mirror_segment(r, 16)),
        StateKind::Orientation => map_orientation(state, |d| mirror.mirror(d)),
        StateKind::Rail => match state.value("shape") {
            Some(shape) => set(state, "shape", mirror_rail(shape, mirror)),
            None => state,
        },
        StateKind::FourWay => permute_four(state, |d| mirror.mirror(d)),
    }
}

/// `StairBlock.mirror`, including its quirk that `FRONT_BACK` keeps inner shapes.
fn mirror_stairs(state: BlockState, mirror: Mirror) -> BlockState {
    let Some(facing) = get_direction(state, "facing") else {
        return state;
    };
    let shape = state.value("shape").unwrap_or("straight");
    let z_axis = matches!(facing, Direction::North | Direction::South);
    let turned = || rotate_state(state, Rotation::Clockwise180);
    match mirror {
        Mirror::LeftRight if z_axis => match shape {
            "outer_left" => set(turned(), "shape", "outer_right"),
            "inner_right" => set(turned(), "shape", "inner_left"),
            "inner_left" => set(turned(), "shape", "inner_right"),
            "outer_right" => set(turned(), "shape", "outer_left"),
            _ => turned(),
        },
        Mirror::FrontBack if !z_axis => match shape {
            "outer_left" => set(turned(), "shape", "outer_right"),
            "outer_right" => set(turned(), "shape", "outer_left"),
            _ => turned(),
        },
        _ => state,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::parse_block_state;

    fn state(json: serde_json::Value) -> BlockState {
        parse_block_state(&json).expect("state")
    }

    #[test]
    fn transform_matches_vanilla() {
        let p = BlockPos::new(3, 7, 1);
        let pivot = BlockPos::ZERO;
        assert_eq!(
            transform(p, Mirror::None, Rotation::Clockwise90, pivot),
            BlockPos::new(-1, 7, 3)
        );
        assert_eq!(
            transform(p, Mirror::None, Rotation::Counterclockwise90, pivot),
            BlockPos::new(1, 7, -3)
        );
        assert_eq!(
            transform(p, Mirror::LeftRight, Rotation::Clockwise180, pivot),
            BlockPos::new(-3, 7, 1)
        );
    }

    #[test]
    fn states_rotate_and_mirror() {
        let stairs = state(serde_json::json!({
            "Name": "minecraft:oak_stairs",
            "Properties": {"facing": "north", "shape": "outer_left"}
        }));
        let rotated = rotate_state(stairs, Rotation::Clockwise90);
        assert_eq!(rotated.value("facing"), Some("east"));
        let mirrored = mirror_state(stairs, Mirror::LeftRight);
        assert_eq!(mirrored.value("facing"), Some("south"));
        assert_eq!(mirrored.value("shape"), Some("outer_right"));

        let fence = state(serde_json::json!({
            "Name": "minecraft:oak_fence",
            "Properties": {"north": "true"}
        }));
        let fence = rotate_state(fence, Rotation::Clockwise90);
        assert_eq!(fence.value("east"), Some("true"));
        assert_eq!(fence.value("north"), Some("false"));

        let log = state(serde_json::json!({
            "Name": "minecraft:oak_log",
            "Properties": {"axis": "x"}
        }));
        assert_eq!(
            rotate_state(log, Rotation::Counterclockwise90).value("axis"),
            Some("z")
        );
        assert_eq!(mirror_state(log, Mirror::FrontBack), log);

        let sign = state(serde_json::json!({
            "Name": "minecraft:oak_sign",
            "Properties": {"rotation": "3"}
        }));
        assert_eq!(
            rotate_state(sign, Rotation::Clockwise90).value("rotation"),
            Some("7")
        );
        assert_eq!(
            mirror_state(sign, Mirror::FrontBack).value("rotation"),
            Some("13")
        );

        let jigsaw = state(serde_json::json!({
            "Name": "minecraft:jigsaw",
            "Properties": {"orientation": "north_up"}
        }));
        assert_eq!(
            rotate_state(jigsaw, Rotation::Clockwise90).value("orientation"),
            Some("east_up")
        );

        let rail = state(serde_json::json!({
            "Name": "minecraft:rail",
            "Properties": {"shape": "north_east"}
        }));
        assert_eq!(
            rotate_state(rail, Rotation::Clockwise90).value("shape"),
            Some("south_east")
        );

        let fire = state(serde_json::json!({
            "Name": "minecraft:fire",
            "Properties": {"north": "true"}
        }));
        assert_eq!(rotate_state(fire, Rotation::Clockwise90), fire);
    }
}
