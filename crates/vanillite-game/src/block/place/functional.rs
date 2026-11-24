//! Redstone components, workstations, containers with a facing, technical and fluid-like
//! blocks.
//!
//! Neighbour redstone signals (`SignalGetter.hasNeighborSignal`, used by lamps, crafters and the
//! repeater lock) are evaluated from block states only: the outputs that Vanilla reads from block
//! entities (comparator, trapped chest, jukebox, test block) are approximated, see `signal::own_signal`.

use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockId, BlockKind, BlockState, flags, ids};

use super::signal;
use super::tags;
use super::util::{self, DirExt, HORIZONTAL, StateExt, Support};
use super::{BlockReader, PlaceContext};

/// `getStateForPlacement` of the kinds of this group (no survival check).
pub(super) fn state_for_placement(
    block: BlockId,
    ctx: &PlaceContext,
    level: &dyn BlockReader,
) -> Option<BlockState> {
    use BlockKind as K;
    let default = block.default_state();
    let pos = ctx.pos;
    let state = match block.info().kind {
        K::RedstoneWireBlock => wire_placement_state(default, pos, level),
        // `DiodeBlock.getStateForPlacement`, plus the repeater's lock.
        K::RepeaterBlock => {
            let state = default.set_dir("facing", ctx.horizontal_direction().opposite());
            state.set_bool("locked", repeater_locked(state, pos, level))
        }
        K::ComparatorBlock => default.set_dir("facing", ctx.horizontal_direction().opposite()),
        // `ObserverBlock`: the nearest looking direction (opposite of the opposite).
        K::ObserverBlock => default.set_dir("facing", ctx.nearest_looking_direction()),
        K::PistonBaseBlock => default
            .set_dir("facing", ctx.nearest_looking_direction().opposite())
            .set_bool("extended", false),
        K::DispenserBlock | K::DropperBlock | K::CommandBlock | K::BarrelBlock => {
            default.set_dir("facing", ctx.nearest_looking_direction().opposite())
        }
        K::CrafterBlock => {
            let front = ctx.nearest_looking_direction().opposite();
            let top = match front {
                Direction::Down => ctx.horizontal_direction().opposite(),
                Direction::Up => ctx.horizontal_direction(),
                _ => Direction::Up,
            };
            default
                .set("orientation", front_and_top(front, top))
                .set_bool("triggered", signal::has_neighbor_signal(level, pos))
        }
        K::HopperBlock => {
            let direction = ctx.face.opposite();
            let facing = if direction.is_vertical() {
                Direction::Down
            } else {
                direction
            };
            default.set_dir("facing", facing).set_bool("enabled", true)
        }
        // `BaseRailBlock.getStateForPlacement`.
        K::RailBlock | K::PoweredRailBlock | K::DetectorRailBlock => {
            let direction = ctx.horizontal_direction();
            let east_west = matches!(direction, Direction::East | Direction::West);
            default
                .set(
                    "shape",
                    if east_west {
                        "east_west"
                    } else {
                        "north_south"
                    },
                )
                .set_bool("waterlogged", util::is_water_source(level, pos))
        }
        // The book flag comes from block entity data of game masters' items: not modelled.
        K::LecternBlock => default
            .set_dir("facing", ctx.horizontal_direction().opposite())
            .set_bool("has_book", false),
        K::NoteBlock => note_block_instrument(default, pos, level),
        K::RedstoneLampBlock => default.set_bool("lit", signal::has_neighbor_signal(level, pos)),
        K::JigsawBlock => {
            let front = ctx.face;
            let top = if front.is_vertical() {
                ctx.horizontal_direction().opposite()
            } else {
                Direction::Up
            };
            default.set("orientation", front_and_top(front, top))
        }
        K::SculkSensorBlock | K::SculkShriekerBlock => {
            default.set_bool("waterlogged", util::is_water_source(level, pos))
        }
        K::CalibratedSculkSensorBlock => default
            .set_bool("waterlogged", util::is_water_source(level, pos))
            .set_dir("facing", ctx.horizontal_direction()),
        K::FurnaceBlock
        | K::BlastFurnaceBlock
        | K::SmokerBlock
        | K::BeehiveBlock
        | K::ChiseledBookShelfBlock
        | K::LoomBlock
        | K::StonecutterBlock
        | K::CarvedPumpkinBlock
        | K::VaultBlock => default.set_dir("facing", ctx.horizontal_direction().opposite()),
        K::EndPortalFrameBlock => default
            .set_dir("facing", ctx.horizontal_direction().opposite())
            .set_bool("eye", false),
        K::CampfireBlock => {
            let water = util::is_water_source(level, pos);
            default
                .set_bool("waterlogged", water)
                .set_bool(
                    "signal_fire",
                    level.state(util::below(pos)).is(ids::HAY_BLOCK),
                )
                .set_bool("lit", !water)
                .set_dir("facing", ctx.horizontal_direction())
        }
        K::ShulkerBoxBlock => default.set_dir("facing", ctx.face),
        K::FireBlock => fire_state(level, pos),
        // `BaseFireBlock.getStateForPlacement` → `BaseFireBlock.getState`.
        K::SoulFireBlock => base_fire_state(level, pos),
        // No `getStateForPlacement` of their own (`Block.defaultBlockState()`); the test block's
        // mode from the item's block state component is the caller's.
        _ => default,
    };
    Some(state)
}

/// `canSurvive` of the kinds of this group.
pub(super) fn can_survive(state: BlockState, pos: BlockPos, level: &dyn BlockReader) -> bool {
    use BlockKind as K;
    let below = util::below(pos);
    match state.kind() {
        K::RedstoneWireBlock => wire_can_survive_on(level.state(below)),
        // `DiodeBlock.canSurviveOn`.
        K::RepeaterBlock | K::ComparatorBlock => {
            util::sturdy(level, below, Direction::Up, Support::Rigid)
        }
        K::RailBlock | K::PoweredRailBlock | K::DetectorRailBlock => {
            util::can_support_rigid_block(level, below)
        }
        K::PressurePlateBlock | K::WeightedPressurePlateBlock => {
            util::can_support_rigid_block(level, below)
                || util::can_support_center(level, below, Direction::Up)
        }
        K::PistonHeadBlock => {
            let Some(facing) = state.get_dir("facing") else {
                return false;
            };
            let base = level.state(pos.relative(facing.opposite()));
            is_fitting_piston_base(state, base)
                || (base.is(ids::MOVING_PISTON) && base.get_dir("facing") == Some(facing))
        }
        K::FireBlock => {
            util::sturdy(level, below, Direction::Up, Support::Full)
                || is_valid_fire_location(level, pos)
        }
        K::SoulFireBlock => tags::SOUL_FIRE_BASE_BLOCKS.has(level.state(below)),
        K::BubbleColumnBlock => {
            let below_state = level.state(below);
            below_state.is(ids::BUBBLE_COLUMN)
                || tags::ENABLES_BUBBLE_COLUMN_PUSH_UP.has(below_state)
                || tags::ENABLES_BUBBLE_COLUMN_DRAG_DOWN.has(below_state)
        }
        _ => true,
    }
}

/// `canBeReplaced(state, context)` of the kinds of this group: none overrides it.
pub(super) fn can_be_replaced(state: BlockState, ctx: &PlaceContext) -> bool {
    util::default_can_be_replaced(state, ctx)
}

/// Blocks placed with `state`: none in this group (pistons extending, diodes and crafters
/// scheduling ticks and command blocks reading their signal are behaviour, not placement).
pub(super) fn placed_extra(
    _state: BlockState,
    _pos: BlockPos,
    _ctx: &PlaceContext,
    _level: &dyn BlockReader,
    _out: &mut Vec<(BlockPos, BlockState)>,
) {
}

/// Blocks changed when a player breaks `state` (`playerWillDestroy`): a piston head broken in
/// creative removes its base without drops (`level.destroyBlock(basePos, false)`, flags 3).
pub(super) fn destroy_extra(
    state: BlockState,
    pos: BlockPos,
    level: &dyn BlockReader,
    creative: bool,
    out: &mut Vec<(BlockPos, BlockState)>,
) {
    if state.kind() == BlockKind::PistonHeadBlock && creative {
        let Some(facing) = state.get_dir("facing") else {
            return;
        };
        let base_pos = pos.relative(facing.opposite());
        let base = level.state(base_pos);
        if is_fitting_piston_base(state, base) {
            // `destroyBlock` leaves the base's fluid; piston bases hold none.
            out.push((base_pos, base.fluid_state().block_state()));
        }
    }
}

/// `PistonHeadBlock.isFittingBase`.
fn is_fitting_piston_base(head: BlockState, base: BlockState) -> bool {
    let base_block = if head.get("type") == Some("sticky") {
        ids::STICKY_PISTON
    } else {
        ids::PISTON
    };
    base.is(base_block)
        && base.get_bool("extended")
        && base.get_dir("facing").is_some()
        && base.get_dir("facing") == head.get_dir("facing")
}

/// `FrontAndTop.fromFrontAndTop(front, top).getSerializedName()`.
fn front_and_top(front: Direction, top: Direction) -> &'static str {
    use Direction as D;
    match (front, top) {
        (D::Down, D::East) => "down_east",
        (D::Down, D::North) => "down_north",
        (D::Down, D::South) => "down_south",
        (D::Down, D::West) => "down_west",
        (D::Up, D::East) => "up_east",
        (D::Up, D::North) => "up_north",
        (D::Up, D::South) => "up_south",
        (D::Up, D::West) => "up_west",
        (D::West, _) => "west_up",
        (D::East, _) => "east_up",
        (D::North, _) => "north_up",
        (D::South, _) => "south_up",
        // Vertical front with a vertical top does not exist; Vanilla's lookup yields null.
        (D::Down, _) => "down_north",
        (D::Up, _) => "up_north",
    }
}

/// `NoteBlock.setInstrument`: the instrument of the block above if it is a mob head, else the
/// one below (a head below plays the harp).
fn note_block_instrument(state: BlockState, pos: BlockPos, level: &dyn BlockReader) -> BlockState {
    let above = instrument(level.state(util::above(pos)));
    if works_above_note_block(above) {
        return state.set("instrument", above);
    }
    let below = instrument(level.state(util::below(pos)));
    state.set(
        "instrument",
        if works_above_note_block(below) {
            "harp"
        } else {
            below
        },
    )
}

/// `BlockState.instrument()` as its serialized name.
fn instrument(state: BlockState) -> &'static str {
    vanillite_data::block::INSTRUMENTS
        .get(usize::from(state.misc().instrument))
        .copied()
        .unwrap_or("harp")
}

/// `NoteBlockInstrument.worksAboveNoteBlock`: mob heads and custom heads.
fn works_above_note_block(instrument: &str) -> bool {
    matches!(
        instrument,
        "zombie" | "skeleton" | "creeper" | "dragon" | "wither_skeleton" | "piglin" | "custom_head"
    )
}

// ---------------------------------------------------------------------------------------------
// Fire

/// Blocks that `FireBlock.bootStrap` makes flammable (all with ignite odds above zero), sorted
/// block ids.
static FLAMMABLE: &[u16] = &[
    13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61,
    63, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 75, 76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86,
    87, 88, 89, 90, 91, 92, 93, 94, 95, 96, 97, 98, 99, 100, 101, 102, 103, 104, 105, 106, 107,
    135, 140, 141, 142, 143, 144, 145, 146, 151, 152, 153, 154, 155, 156, 157, 158, 159, 160, 161,
    162, 163, 164, 165, 166, 167, 168, 169, 170, 171, 172, 173, 174, 175, 176, 177, 178, 179, 180,
    181, 182, 183, 184, 185, 186, 187, 188, 189, 190, 191, 192, 193, 194, 195, 196, 197, 198, 200,
    201, 202, 203, 204, 205, 206, 207, 208, 209, 210, 211, 212, 213, 214, 220, 221, 223, 224, 225,
    226, 228, 229, 230, 231, 232, 233, 234, 244, 329, 333, 416, 417, 419, 455, 456, 457, 569, 570,
    571, 572, 573, 574, 575, 576, 591, 592, 593, 594, 595, 596, 597, 598, 599, 600, 601, 602, 603,
    604, 605, 606, 607, 609, 611, 612, 613, 614, 615, 616, 653, 654, 655, 656, 657, 658, 659, 660,
    661, 662, 663, 664, 682, 683, 684, 685, 686, 687, 688, 689, 690, 691, 692, 693, 694, 695, 696,
    697, 698, 699, 700, 701, 722, 834, 882, 927, 935, 951, 999, 1000, 1001, 1002, 1225, 1226, 1227,
    1228, 1229, 1231, 1232, 1233, 1235, 1236, 1237, 1238, 1278, 1279, 1280, 1281, 1282, 1285,
];

/// `FireBlock.canBurn`: flammable and not waterlogged.
fn can_burn(state: BlockState) -> bool {
    !state.get_bool("waterlogged") && FLAMMABLE.binary_search(&state.block().0).is_ok()
}

/// `FireBlock.isValidFireLocation`.
fn is_valid_fire_location(level: &dyn BlockReader, pos: BlockPos) -> bool {
    Direction::ALL
        .iter()
        .any(|d| can_burn(level.state(pos.relative(*d))))
}

/// `FireBlock.getStateForPlacement(level, pos)`.
fn fire_state(level: &dyn BlockReader, pos: BlockPos) -> BlockState {
    let below = util::below(pos);
    let below_state = level.state(below);
    let default = ids::FIRE.default_state();
    if can_burn(below_state) || below_state.sturdy(Direction::Up, Support::Full) {
        return default;
    }
    let mut state = default;
    for direction in Direction::ALL {
        if direction != Direction::Down {
            state = state.set_bool(
                direction.name(),
                can_burn(level.state(pos.relative(direction))),
            );
        }
    }
    state
}

/// `BaseFireBlock.getState(level, pos)`: soul fire on soul fire bases, else fire.
fn base_fire_state(level: &dyn BlockReader, pos: BlockPos) -> BlockState {
    if tags::SOUL_FIRE_BASE_BLOCKS.has(level.state(util::below(pos))) {
        ids::SOUL_FIRE.default_state()
    } else {
        fire_state(level, pos)
    }
}

// ---------------------------------------------------------------------------------------------
// Redstone wire

/// A `RedstoneSide`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RedstoneSide {
    None,
    Side,
    Up,
}

impl RedstoneSide {
    fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Side => "side",
            Self::Up => "up",
        }
    }

    fn parse(value: Option<&str>) -> Self {
        match value {
            Some("side") => Self::Side,
            Some("up") => Self::Up,
            _ => Self::None,
        }
    }

    pub(super) fn connected(self) -> bool {
        self != Self::None
    }
}

/// The four sides of a wire in [`HORIZONTAL`] order (north, east, south, west).
pub(super) type Sides = [RedstoneSide; 4];

pub(super) fn wire_sides(state: BlockState) -> Sides {
    HORIZONTAL.map(|d| RedstoneSide::parse(state.get(d.name())))
}

fn with_sides(state: BlockState, sides: Sides) -> BlockState {
    let mut state = state;
    for (direction, side) in HORIZONTAL.iter().zip(sides) {
        state = state.set(direction.name(), side.name());
    }
    state
}

pub(super) fn horizontal_index(direction: Direction) -> usize {
    match direction {
        Direction::North => 0,
        Direction::East => 1,
        Direction::South => 2,
        _ => 3,
    }
}

/// `RedstoneWireBlock.canSurviveOn`.
fn wire_can_survive_on(state: BlockState) -> bool {
    state.sturdy(Direction::Up, Support::Full) || state.is(ids::HOPPER)
}

/// `RedstoneWireBlock.getStateForPlacement`: the connection state of the cross state.
fn wire_placement_state(default: BlockState, pos: BlockPos, level: &dyn BlockReader) -> BlockState {
    let sides = wire_connection_sides([RedstoneSide::Side; 4], level, pos);
    with_sides(default, sides)
}

/// `RedstoneWireBlock.getConnectionState` on the sides of a wire.
pub(super) fn wire_connection_sides(
    current: Sides,
    level: &dyn BlockReader,
    pos: BlockPos,
) -> Sides {
    let was_dot = current.iter().all(|s| !s.connected());
    // `getMissingConnections` on the default state (all sides none).
    let can_connect_up = !level.state(util::above(pos)).is_redstone_conductor();
    let mut sides = [RedstoneSide::None; 4];
    for (i, direction) in HORIZONTAL.iter().enumerate() {
        sides[i] = wire_connecting_side(level, pos, *direction, can_connect_up);
    }
    let is_dot = sides.iter().all(|s| !s.connected());
    if was_dot && is_dot {
        return sides;
    }
    let [north, east, south, west] = sides.map(RedstoneSide::connected);
    let north_south_empty = !north && !south;
    let east_west_empty = !east && !west;
    if !west && north_south_empty {
        sides[3] = RedstoneSide::Side;
    }
    if !east && north_south_empty {
        sides[1] = RedstoneSide::Side;
    }
    if !north && east_west_empty {
        sides[0] = RedstoneSide::Side;
    }
    if !south && east_west_empty {
        sides[2] = RedstoneSide::Side;
    }
    sides
}

/// `RedstoneWireBlock.getConnectingSide(level, pos, direction, canConnectUp)`.
fn wire_connecting_side(
    level: &dyn BlockReader,
    pos: BlockPos,
    direction: Direction,
    can_connect_up: bool,
) -> RedstoneSide {
    let relative_pos = pos.relative(direction);
    let relative = level.state(relative_pos);
    if can_connect_up {
        let placeable_above = matches!(
            relative.kind(),
            BlockKind::TrapDoorBlock | BlockKind::WeatheringCopperTrapDoorBlock
        ) || wire_can_survive_on(relative);
        if placeable_above && should_wire_connect_to(level.state(util::above(relative_pos)), None) {
            return if relative.sturdy(direction.opposite(), Support::Full) {
                RedstoneSide::Up
            } else {
                RedstoneSide::Side
            };
        }
    }
    if !should_wire_connect_to(relative, Some(direction))
        && (relative.is_redstone_conductor()
            || !should_wire_connect_to(level.state(util::below(relative_pos)), None))
    {
        RedstoneSide::None
    } else {
        RedstoneSide::Side
    }
}

/// `BlockState.shouldRedstoneWireConnectTo(level, pos, direction)`.
fn should_wire_connect_to(state: BlockState, direction: Option<Direction>) -> bool {
    match state.kind() {
        BlockKind::RedstoneWireBlock => true,
        BlockKind::RepeaterBlock => {
            let facing = state.get_dir("facing");
            direction.is_some()
                && (facing == direction || facing.map(Direction::opposite) == direction)
        }
        BlockKind::ObserverBlock => direction.is_some() && state.get_dir("facing") == direction,
        _ => state.has(flags::SIGNAL_SOURCE) && direction.is_some(),
    }
}

/// `RepeaterBlock.isLocked`: a powered diode pointing into either side.
fn repeater_locked(state: BlockState, pos: BlockPos, level: &dyn BlockReader) -> bool {
    let Some(facing) = state.get_dir("facing") else {
        return false;
    };
    [facing.clockwise(), facing.counter_clockwise()]
        .into_iter()
        .any(|side| {
            // `getControlInputSignal(pos, side, onlyDiodes = true)`.
            let side_pos = pos.relative(side);
            let side_state = level.state(side_pos);
            matches!(
                side_state.kind(),
                BlockKind::RepeaterBlock | BlockKind::ComparatorBlock
            ) && signal::direct_signal(side_state, level, side_pos, side) > 0
        })
}

#[cfg(test)]
mod tests {
    use vanillite_base::{BlockPos, Direction};

    use super::super::state_for_placement;
    use super::super::tests::*;
    use super::super::util::StateExt;

    fn ground() -> (TestLevel, BlockPos) {
        let mut level = TestLevel::new();
        let pos = BlockPos::new(0, 64, 0);
        level.set_block(pos, "stone");
        (level, pos)
    }

    #[test]
    fn furnace_faces_the_player() {
        let (level, pos) = ground();
        for (yaw, facing) in [
            (0.0, "north"),
            (90.0, "east"),
            (180.0, "south"),
            (-90.0, "west"),
            (44.9, "north"),
            (45.1, "east"),
            (-135.1, "south"),
        ] {
            let c = ctx(&level, "furnace", pos, Direction::Up, yaw, 30.0);
            assert_state(
                state_for_placement(&c, &level),
                "furnace",
                &[("facing", facing), ("lit", "false")],
            );
        }
    }

    #[test]
    fn piston_faces_the_player() {
        let (level, pos) = ground();
        // Looking down: the nearest direction is down, the piston faces up.
        let c = ctx(&level, "piston", pos, Direction::Up, 10.0, 80.0);
        assert_state(
            state_for_placement(&c, &level),
            "piston",
            &[("facing", "up"), ("extended", "false")],
        );
        // Looking horizontally south: the piston faces north (towards the player).
        let c = ctx(&level, "sticky_piston", pos, Direction::Up, 0.0, 10.0);
        assert_state(
            state_for_placement(&c, &level),
            "sticky_piston",
            &[("facing", "north")],
        );
    }

    #[test]
    fn observer_faces_away() {
        let (level, pos) = ground();
        let c = ctx(&level, "observer", pos, Direction::Up, 0.0, 10.0);
        assert_state(
            state_for_placement(&c, &level),
            "observer",
            &[("facing", "south")],
        );
    }

    #[test]
    fn redstone_wire_connects_to_wire() {
        let (mut level, pos) = ground();
        // A lone wire is a cross.
        let c = ctx(&level, "redstone", pos, Direction::Up, 0.0, 60.0);
        assert_state(
            state_for_placement(&c, &level),
            "redstone_wire",
            &[
                ("north", "side"),
                ("east", "side"),
                ("south", "side"),
                ("west", "side"),
            ],
        );
        // With a wire to the east: a line east-west.
        level.set_block(BlockPos::new(1, 64, 0), "stone");
        level.set_block(BlockPos::new(1, 65, 0), "redstone_wire");
        let c = ctx(&level, "redstone", pos, Direction::Up, 0.0, 60.0);
        assert_state(
            state_for_placement(&c, &level),
            "redstone_wire",
            &[
                ("north", "none"),
                ("east", "side"),
                ("south", "none"),
                ("west", "side"),
            ],
        );
        // A wire one block up on a stone block to the north: climbs up.
        level.set_block(BlockPos::new(0, 65, -1), "stone");
        level.set_block(BlockPos::new(0, 66, -1), "redstone_wire");
        let c = ctx(&level, "redstone", pos, Direction::Up, 0.0, 60.0);
        assert_state(
            state_for_placement(&c, &level),
            "redstone_wire",
            &[("north", "up"), ("east", "side"), ("south", "none")],
        );
        // Not on air.
        let c = ctx(
            &level,
            "redstone",
            BlockPos::new(5, 70, 5),
            Direction::Up,
            0.0,
            60.0,
        );
        assert_eq!(state_for_placement(&c, &level), None);
    }

    #[test]
    fn repeater_faces_away_and_locks() {
        let (mut level, pos) = ground();
        let c = ctx(&level, "repeater", pos, Direction::Up, 90.0, 60.0);
        assert_state(
            state_for_placement(&c, &level),
            "repeater",
            &[("facing", "east"), ("locked", "false")],
        );
        // A powered repeater pointing into the side (its input to the north) locks it.
        let side = block("repeater")
            .default_state()
            .set("facing", "north")
            .set_bool("powered", true);
        level.set_block(BlockPos::new(0, 64, -1), "stone");
        level.set(BlockPos::new(0, 65, -1), side);
        let c = ctx(&level, "repeater", pos, Direction::Up, 90.0, 60.0);
        assert_state(
            state_for_placement(&c, &level),
            "repeater",
            &[("facing", "east"), ("locked", "true")],
        );
    }

    #[test]
    fn lamp_lit_by_a_redstone_block() {
        let (mut level, pos) = ground();
        level.set_block(BlockPos::new(1, 65, 0), "redstone_block");
        let c = ctx(&level, "redstone_lamp", pos, Direction::Up, 0.0, 60.0);
        assert_state(
            state_for_placement(&c, &level),
            "redstone_lamp",
            &[("lit", "true")],
        );
        let c = ctx(
            &level,
            "redstone_lamp",
            BlockPos::new(9, 64, 9),
            Direction::Up,
            0.0,
            60.0,
        );
        assert_state(
            state_for_placement(&c, &level),
            "redstone_lamp",
            &[("lit", "false")],
        );
    }

    #[test]
    fn hopper_rail_campfire_shulker() {
        let (mut level, pos) = ground();
        let c = ctx(&level, "hopper", pos, Direction::Up, 0.0, 60.0);
        assert_state(
            state_for_placement(&c, &level),
            "hopper",
            &[("facing", "down")],
        );
        let c = ctx(&level, "hopper", pos, Direction::East, 0.0, 60.0);
        assert_state(
            state_for_placement(&c, &level),
            "hopper",
            &[("facing", "west")],
        );
        let c = ctx(&level, "rail", pos, Direction::Up, 90.0, 60.0);
        assert_state(
            state_for_placement(&c, &level),
            "rail",
            &[("shape", "east_west")],
        );
        level.set_block(BlockPos::new(3, 64, 0), "hay_block");
        let c = ctx(
            &level,
            "campfire",
            BlockPos::new(3, 64, 0),
            Direction::Up,
            0.0,
            60.0,
        );
        assert_state(
            state_for_placement(&c, &level),
            "campfire",
            &[
                ("signal_fire", "true"),
                ("lit", "true"),
                ("facing", "south"),
            ],
        );
        let c = ctx(&level, "shulker_box", pos, Direction::South, 0.0, 60.0);
        assert_state(
            state_for_placement(&c, &level),
            "shulker_box",
            &[("facing", "south")],
        );
    }

    #[test]
    fn note_block_instrument_from_neighbours() {
        let (mut level, pos) = ground();
        let c = ctx(&level, "note_block", pos, Direction::Up, 0.0, 60.0);
        assert_state(
            state_for_placement(&c, &level),
            "note_block",
            &[("instrument", "basedrum")],
        );
        level.set_block(BlockPos::new(0, 66, 0), "zombie_head");
        let c = ctx(&level, "note_block", pos, Direction::Up, 0.0, 60.0);
        assert_state(
            state_for_placement(&c, &level),
            "note_block",
            &[("instrument", "zombie")],
        );
    }

    #[test]
    fn piston_head_creative_removes_base() {
        let (mut level, _) = ground();
        let base = block("piston")
            .default_state()
            .set("facing", "up")
            .set_bool("extended", true);
        let head = block("piston_head").default_state().set("facing", "up");
        level.set(BlockPos::new(0, 65, 0), base);
        let mut out = Vec::new();
        super::super::destroy_extra(head, BlockPos::new(0, 66, 0), &level, true, &mut out);
        assert_eq!(out, vec![(BlockPos::new(0, 65, 0), super::super::AIR)]);
        out.clear();
        super::super::destroy_extra(head, BlockPos::new(0, 66, 0), &level, false, &mut out);
        assert!(out.is_empty());
    }
}
