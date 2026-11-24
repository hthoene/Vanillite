//! Repeaters and comparators (`DiodeBlock`, `RepeaterBlock`, `ComparatorBlock`), and the
//! analog outputs a comparator reads from a block's state alone.

use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_world::ticks::TickPriority;
use vanillite_worldgen::behaviour::StateExt;

use crate::block::{AnalogFn, Interaction, UseContext, drops};
use crate::level::block_entity::BlockEntity;
use crate::level::signal::{DIRECTIONS, is_diode, powered, wire_power};
use crate::level::{Level, SOUND_BLOCKS, update};

fn facing(state: BlockState) -> Direction {
    state.direction("facing").unwrap_or(Direction::North)
}

fn is_comparator(state: BlockState) -> bool {
    state.is(ids::COMPARATOR)
}

/// `getDelay`: repeaters wait twice their delay setting, comparators 2 ticks.
fn delay(state: BlockState) -> i32 {
    if is_comparator(state) {
        2
    } else {
        state.int("delay").unwrap_or(1) * 2
    }
}

/// `isLocked`: a repeater with a powered diode at its side.
fn is_locked(level: &Level, pos: BlockPos, state: BlockState) -> bool {
    !is_comparator(state) && alternate_signal(level, pos, state) > 0
}

/// `getOutputSignal`.
fn output_signal(level: &Level, pos: BlockPos, state: BlockState) -> i32 {
    if is_comparator(state) {
        level.comparator_output(pos)
    } else {
        15
    }
}

/// `ownSignal`.
fn own_signal(level: &Level, state: BlockState, pos: BlockPos) -> i32 {
    if powered(state) {
        output_signal(level, pos, state)
    } else {
        0
    }
}

/// `getSignal`: only towards the front.
pub(super) fn signal(level: &Level, state: BlockState, pos: BlockPos, direction: Direction) -> i32 {
    if facing(state) == direction {
        own_signal(level, state, pos)
    } else {
        0
    }
}

/// `getDirectSignal`: the same as the weak signal.
pub(super) fn direct_signal(
    level: &Level,
    state: BlockState,
    pos: BlockPos,
    direction: Direction,
) -> i32 {
    signal(level, state, pos, direction)
}

/// `DiodeBlock.getInputSignal`, and `ComparatorBlock.getInputSignal` with the analog output
/// of the block behind (directly or through a conductor). Item frames are not read: they do not
/// exist yet.
fn input_signal(level: &Level, pos: BlockPos, state: BlockState) -> i32 {
    let direction = facing(state);
    let target = pos.relative(direction);
    let mut input = level.signal(target, direction);
    if input < 15 {
        let target_state = level.block(target);
        if target_state.is(ids::REDSTONE_WIRE) {
            input = input.max(wire_power(target_state));
        }
    }
    if !is_comparator(state) {
        return input;
    }
    let target_state = level.block(target);
    if Level::has_analog_output(target_state) {
        input = level.analog_output(target_state, target, direction.opposite());
    } else if input < 15 && Level::is_redstone_conductor(target_state) {
        let behind = target.relative(direction);
        let behind_state = level.block(behind);
        if Level::has_analog_output(behind_state) {
            input = level.analog_output(behind_state, behind, direction.opposite());
        }
    }
    input
}

/// `getAlternateSignal`: the stronger side input (repeaters only see diodes).
fn alternate_signal(level: &Level, pos: BlockPos, state: BlockState) -> i32 {
    let direction = facing(state);
    let clockwise = clockwise(direction);
    let counter = clockwise.opposite();
    let only_diodes = !is_comparator(state);
    level
        .control_input_signal(pos.relative(clockwise), clockwise, only_diodes)
        .max(level.control_input_signal(pos.relative(counter), counter, only_diodes))
}

fn clockwise(direction: Direction) -> Direction {
    match direction {
        Direction::North => Direction::East,
        Direction::East => Direction::South,
        Direction::South => Direction::West,
        Direction::West => Direction::North,
        vertical => vertical,
    }
}

fn is_subtract(state: BlockState) -> bool {
    state.value("mode") == Some("subtract")
}

/// `shouldTurnOn`.
fn should_turn_on(level: &Level, pos: BlockPos, state: BlockState) -> bool {
    let input = input_signal(level, pos, state);
    if !is_comparator(state) {
        return input > 0;
    }
    if input == 0 {
        return false;
    }
    let side = alternate_signal(level, pos, state);
    input > side || (input == side && !is_subtract(state))
}

/// `ComparatorBlock.calculateOutputSignal`.
fn calculate_output_signal(level: &Level, pos: BlockPos, state: BlockState) -> i32 {
    let input = input_signal(level, pos, state);
    if input == 0 {
        return 0;
    }
    let side = alternate_signal(level, pos, state);
    if side > input {
        0
    } else if is_subtract(state) {
        input - side
    } else {
        input
    }
}

/// `shouldPrioritize`: the diode behind this one does not face it.
fn should_prioritize(level: &Level, pos: BlockPos, state: BlockState) -> bool {
    let direction = facing(state).opposite();
    let behind = level.block(pos.relative(direction));
    is_diode(behind) && facing(behind) != direction
}

/// `tick`.
pub(super) fn tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    if is_comparator(state) {
        refresh_output_state(level, pos, state);
        return;
    }
    if is_locked(level, pos, state) {
        return;
    }
    let on = powered(state);
    let should = should_turn_on(level, pos, state);
    if on && !should {
        level.set_block(pos, state.set_flag("powered", false), update::CLIENTS);
    } else if !on {
        level.set_block(pos, state.set_flag("powered", true), update::CLIENTS);
        if !should {
            level.schedule_block_tick_with(
                pos,
                state.block(),
                delay(state),
                TickPriority::VERY_HIGH,
            );
        }
    }
}

/// `neighborChanged`.
pub(super) fn neighbor_changed(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    _: BlockId,
    _: bool,
) {
    if level.block(pos).block() != state.block() {
        return;
    }
    if level.can_survive(state, pos) {
        check_tick_on_neighbor(level, pos, state);
    } else {
        drops::drop_resources(level, pos, state, None);
        level.remove_block(pos, false);
        for direction in DIRECTIONS {
            level.update_neighbors_at(pos.relative(direction), state.block());
        }
    }
}

/// `checkTickOnNeighbor`.
fn check_tick_on_neighbor(level: &mut Level, pos: BlockPos, state: BlockState) {
    let block = state.block();
    if is_comparator(state) {
        if level.chunks.block_tick_runs_this_tick(pos, block.0) {
            return;
        }
        let output = calculate_output_signal(level, pos, state);
        let old = level.comparator_output(pos);
        if output != old || powered(state) != should_turn_on(level, pos, state) {
            let priority = if should_prioritize(level, pos, state) {
                TickPriority::HIGH
            } else {
                TickPriority::NORMAL
            };
            level.schedule_block_tick_with(pos, block, 2, priority);
        }
        return;
    }
    if is_locked(level, pos, state) {
        return;
    }
    let on = powered(state);
    let should = should_turn_on(level, pos, state);
    if on != should && !level.chunks.block_tick_runs_this_tick(pos, block.0) {
        let priority = if should_prioritize(level, pos, state) {
            TickPriority::EXTREMELY_HIGH
        } else if on {
            TickPriority::VERY_HIGH
        } else {
            TickPriority::HIGH
        };
        level.schedule_block_tick_with(pos, block, delay(state), priority);
    }
}

/// `ComparatorBlock.refreshOutputState`.
fn refresh_output_state(level: &mut Level, pos: BlockPos, state: BlockState) {
    let output = calculate_output_signal(level, pos, state);
    let mut old = 0;
    if let Some(BlockEntity::Comparator { output: stored }) = level.block_entity_mut(pos) {
        old = *stored;
        *stored = output;
    }
    if old != output || !is_subtract(state) {
        let source_on = should_turn_on(level, pos, state);
        let is_on = powered(state);
        if is_on && !source_on {
            level.set_block(pos, state.set_flag("powered", false), update::CLIENTS);
        } else if !is_on && source_on {
            level.set_block(pos, state.set_flag("powered", true), update::CLIENTS);
        }
        update_neighbors_in_front(level, pos, state);
    }
}

/// `onPlace`.
pub(super) fn on_place(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    _: BlockState,
    _: bool,
) {
    update_neighbors_in_front(level, pos, state);
}

/// `affectNeighborsAfterRemoval`.
pub(super) fn affect_neighbors_after_removal(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    moved: bool,
) {
    if !moved {
        update_neighbors_in_front(level, pos, state);
    }
}

/// `updateNeighborsInFront`: the block in front, then its neighbours except this diode.
fn update_neighbors_in_front(level: &mut Level, pos: BlockPos, state: BlockState) {
    let direction = facing(state);
    let front = pos.relative(direction.opposite());
    level.neighbor_changed(front, state.block());
    level.update_neighbors_at_except(front, state.block(), Some(direction));
}

/// `DiodeBlock.setPlacedBy`: a diode placed with its input on ticks once. Call it after a
/// player placed a repeater or comparator.
pub fn set_placed_by(level: &mut Level, state: BlockState, pos: BlockPos) {
    if is_diode(state) && should_turn_on(level, pos, state) {
        level.schedule_block_tick(pos, state.block(), 1);
    }
}

/// `useWithoutItem`: repeaters cycle their delay, comparators their mode.
pub(super) fn use_without_item(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    context: &UseContext,
) -> Interaction {
    if !context.may_build {
        return Interaction::Pass;
    }
    if !is_comparator(state) {
        let next = state.int("delay").unwrap_or(1) % 4 + 1;
        level.set_block_and_update(pos, state.set_int("delay", next));
        return Interaction::Success;
    }
    let new = state.set(
        "mode",
        if is_subtract(state) {
            "compare"
        } else {
            "subtract"
        },
    );
    let pitch = if is_subtract(new) { 0.55 } else { 0.5 };
    let sound = crate::block::sound_id("block.comparator.click");
    level.play_block_sound(Some(context.player), pos, sound, SOUND_BLOCKS, 0.3, pitch);
    level.set_block(pos, new, update::CLIENTS);
    if level.block(pos).is(ids::COMPARATOR) {
        refresh_output_state(level, pos, new);
    }
    Interaction::Success
}

/// The `getAnalogOutputSignal` of blocks whose output comes from their state alone.
pub(super) fn state_analog_output(block: BlockId) -> Option<AnalogFn> {
    let name = block.name();
    let f: AnalogFn = if block == ids::CAKE {
        |_, s, _, _| (7 - s.int("bites").unwrap_or(0)) * 2
    } else if name.ends_with("candle_cake") {
        |_, _, _, _| 14
    } else if block == ids::COMPOSTER
        || block == ids::WATER_CAULDRON
        || block == ids::POWDER_SNOW_CAULDRON
    {
        |_, s, _, _| s.int("level").unwrap_or(0)
    } else if block == ids::LAVA_CAULDRON {
        |_, _, _, _| 3
    } else if block == ids::END_PORTAL_FRAME {
        |_, s, _, _| if s.flag("eye") { 15 } else { 0 }
    } else if block == ids::RESPAWN_ANCHOR {
        |_, s, _, _| (s.int("charges").unwrap_or(0) as f32 / 4.0 * 15.0).floor() as i32
    } else if block == ids::BEEHIVE || block == ids::BEE_NEST {
        |_, s, _, _| s.int("honey_level").unwrap_or(0)
    } else if name.ends_with("copper_golem_statue") {
        |_, s, _, _| {
            let pose = match s.value("copper_golem_pose") {
                Some("standing") => 0,
                Some("sitting") => 1,
                Some("running") => 2,
                Some("star") => 3,
                _ => 0,
            };
            pose + 1
        }
    } else {
        return None;
    };
    Some(f)
}
