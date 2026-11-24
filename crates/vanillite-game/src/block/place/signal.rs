//! Redstone signals read from block states (`SignalGetter`): `hasNeighborSignal`,
//! `getSignal`, `getDirectSignal`. Outputs that Vanilla keeps in block entities are approximated:
//! a powered comparator gives 15; trapped chests, jukeboxes and test blocks give 0.

use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockKind, BlockState};

use super::BlockReader;
use super::functional::{horizontal_index, wire_connection_sides, wire_sides};
use super::util::StateExt;

/// `FaceAttachedHorizontalDirectionalBlock.getConnectedDirection`.
fn connected_direction(state: BlockState) -> Option<Direction> {
    match state.get("face") {
        Some("ceiling") => Some(Direction::Down),
        Some("floor") => Some(Direction::Up),
        _ => state.get_dir("facing"),
    }
}

fn powered_15(state: BlockState) -> i32 {
    if state.get_bool("powered") { 15 } else { 0 }
}

/// `BlockState.getOwnSignal`. Block-entity outputs are approximated: a powered comparator gives
/// 15, trapped chests, jukeboxes and test blocks give 0.
fn own_signal(state: BlockState) -> i32 {
    use BlockKind as K;
    match state.kind() {
        K::PoweredBlock => 15,
        K::RedstoneWireBlock
        | K::WeightedPressurePlateBlock
        | K::DaylightDetectorBlock
        | K::SculkSensorBlock
        | K::CalibratedSculkSensorBlock
        | K::TargetBlock => state.get_int("power"),
        K::ButtonBlock
        | K::LeverBlock
        | K::LightningRodBlock
        | K::WeatheringLightningRodBlock
        | K::ObserverBlock
        | K::DetectorRailBlock
        | K::TripWireHookBlock
        | K::LecternBlock
        | K::PressurePlateBlock
        | K::RepeaterBlock
        | K::ComparatorBlock => powered_15(state),
        K::RedstoneTorchBlock | K::RedstoneWallTorchBlock if state.get_bool("lit") => 15,
        _ => 0,
    }
}

/// `BlockState.getSignal(level, pos, direction)`.
fn block_signal(
    state: BlockState,
    level: &dyn BlockReader,
    pos: BlockPos,
    direction: Direction,
) -> i32 {
    use BlockKind as K;
    let facing = state.get_dir("facing");
    match state.kind() {
        K::RedstoneWireBlock => {
            if direction == Direction::Down {
                return 0;
            }
            let power = state.get_int("power");
            if power == 0 {
                return 0;
            }
            if direction != Direction::Up {
                let sides = wire_connection_sides(wire_sides(state), level, pos);
                if !sides[horizontal_index(direction.opposite())].connected() {
                    return 0;
                }
            }
            power
        }
        K::RepeaterBlock | K::ComparatorBlock | K::ObserverBlock if facing != Some(direction) => 0,
        K::RedstoneTorchBlock if direction == Direction::Up => 0,
        K::RedstoneWallTorchBlock | K::CalibratedSculkSensorBlock if facing == Some(direction) => 0,
        _ => own_signal(state),
    }
}

/// `BlockState.getDirectSignal(level, pos, direction)`.
pub(super) fn direct_signal(
    state: BlockState,
    level: &dyn BlockReader,
    pos: BlockPos,
    direction: Direction,
) -> i32 {
    use BlockKind as K;
    let powered = state.get_bool("powered");
    match state.kind() {
        K::RedstoneWireBlock | K::RepeaterBlock | K::ComparatorBlock | K::ObserverBlock => {
            block_signal(state, level, pos, direction)
        }
        K::PressurePlateBlock
        | K::WeightedPressurePlateBlock
        | K::LecternBlock
        | K::DetectorRailBlock
            if direction == Direction::Up =>
        {
            own_signal(state)
        }
        K::ButtonBlock | K::LeverBlock
            if powered && connected_direction(state) == Some(direction) =>
        {
            15
        }
        K::LightningRodBlock | K::WeatheringLightningRodBlock | K::TripWireHookBlock
            if powered && state.get_dir("facing") == Some(direction) =>
        {
            15
        }
        K::RedstoneTorchBlock | K::RedstoneWallTorchBlock if direction == Direction::Down => {
            block_signal(state, level, pos, direction)
        }
        K::SculkSensorBlock | K::CalibratedSculkSensorBlock if direction == Direction::Up => {
            block_signal(state, level, pos, direction)
        }
        _ => 0,
    }
}

/// `SignalGetter.getDirectSignalTo(pos)`.
fn direct_signal_to(level: &dyn BlockReader, pos: BlockPos) -> i32 {
    let mut result = 0;
    for direction in Direction::ALL {
        let source = pos.relative(direction);
        result = result.max(direct_signal(level.state(source), level, source, direction));
        if result >= 15 {
            break;
        }
    }
    result
}

/// `SignalGetter.getSignal(pos, direction)`.
fn level_signal(level: &dyn BlockReader, pos: BlockPos, direction: Direction) -> i32 {
    let state = level.state(pos);
    let signal = block_signal(state, level, pos, direction);
    if state.is_redstone_conductor() {
        signal.max(direct_signal_to(level, pos))
    } else {
        signal
    }
}

/// `SignalGetter.hasNeighborSignal(pos)`.
pub(super) fn has_neighbor_signal(level: &dyn BlockReader, pos: BlockPos) -> bool {
    Direction::ALL
        .iter()
        .any(|d| level_signal(level, pos.relative(*d), *d) > 0)
}
