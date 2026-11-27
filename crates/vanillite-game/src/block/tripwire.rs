//! Tripwire and tripwire hooks (`TripWireBlock`, `TripWireHookBlock`).
//!
//! The string's shape update (connections) is shared with world generation. The `string-
//! duplication` exploit switch does not change anything here: the known Vanilla string
//! duplications come from item and block drops around pistons and hooks that do not exist in
//! this port yet, so Vanilla's update logic is kept as is.

use vanillite_base::random::RandomSource;
use vanillite_base::{Aabb, BlockPos, Direction};
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_world::entity::EntityId;
use vanillite_worldgen::behaviour::StateExt;

use super::plate::ignores_block_triggers;
use super::{BlockBehaviour, sound_id};
use crate::level::signal::powered;
use crate::level::{Level, SOUND_BLOCKS, update};

/// `WIRE_DIST_MAX`.
const WIRE_DIST_MAX: i32 = 42;

/// Fills the tripwire and hook behaviours of `block`.
pub(super) fn fill(block: BlockId, b: &mut BlockBehaviour) {
    if block == ids::TRIPWIRE {
        b.on_place = Some(wire_on_place);
        b.affect_neighbors_after_removal = Some(wire_affect_neighbors_after_removal);
        b.entity_inside = Some(wire_entity_inside);
        b.tick = Some(wire_tick);
    } else if block == ids::TRIPWIRE_HOOK {
        b.tick = Some(hook_tick);
        b.affect_neighbors_after_removal = Some(hook_affect_neighbors_after_removal);
        b.signal = Some(hook_signal);
        b.direct_signal = Some(hook_direct_signal);
    }
}

// --- TripWireBlock -----------------------------------------------------------------------

/// `onPlace`.
fn wire_on_place(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    old: BlockState,
    _moved: bool,
) {
    if !old.is(state.block()) {
        update_source(level, pos, state);
    }
}

/// `affectNeighborsAfterRemoval`.
fn wire_affect_neighbors_after_removal(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    moved: bool,
) {
    if !moved {
        update_source(level, pos, state.set_flag("powered", true));
    }
}

/// `playerWillDestroy`: string cut with shears is disarmed first (flags 260: clients, no block
/// entity side effects), so the hooks do not fire, and the `shear` game event is posted. The
/// block breaking code calls this before it destroys the block.
pub fn wire_player_will_destroy(
    level: &mut Level,
    pos: BlockPos,
    state: BlockState,
    holding_shears: bool,
    player: Option<EntityId>,
) {
    if holding_shears && state.is(ids::TRIPWIRE) {
        level.set_block(
            pos,
            state.set_flag("disarmed", true),
            update::CLIENTS | update::SKIP_BLOCK_ENTITY_SIDEEFFECTS,
        );
        level.game_event_at(player, crate::level::game_event::events().shear, pos);
    }
}

/// `updateSource(level, pos, state)`: the hooks south and west of the string recompute.
fn update_source(level: &mut Level, pos: BlockPos, state: BlockState) {
    for direction in [Direction::South, Direction::West] {
        for i in 1..WIRE_DIST_MAX {
            let test = step(pos, direction, i);
            let block = level.block(test);
            if block.is(ids::TRIPWIRE_HOOK) {
                if block.direction("facing") == Some(direction.opposite()) {
                    calculate_state(level, test, block, false, true, i, Some(state));
                }
                break;
            }
            if !block.is(ids::TRIPWIRE) {
                break;
            }
        }
    }
}

/// `entityInside`; a plugin may keep a player from triggering it
/// ([`super::contact::physical_cancelled`]).
fn wire_entity_inside(level: &mut Level, state: BlockState, pos: BlockPos, entity: EntityId) {
    if !powered(state)
        && !level.chunks.has_block_tick(pos, ids::TRIPWIRE.0)
        && !super::contact::physical_cancelled(level, pos, entity)
    {
        let pressed = !ignores_block_triggers(level, entity);
        check_pressed_with(level, pos, pressed);
    }
}

/// `tick`.
fn wire_tick(level: &mut Level, _state: BlockState, pos: BlockPos) {
    if powered(level.block(pos)) {
        // `checkPressed(level, pos)`: the entities in the string's shape bounds.
        let state = level.block(pos);
        let aabb = wire_bounds(pos, state.flag("attached"));
        let store = &level.entities.store;
        let mut pressed = false;
        store.for_each_in(&aabb, false, |id| {
            if !ignores_block_triggers(level, id) {
                pressed = true;
            }
        });
        check_pressed_with(level, pos, pressed);
    }
}

/// The bounds of the string's shape (`SHAPE_ATTACHED` 1..2.5 px high, `SHAPE_NOT_ATTACHED`
/// 0..8 px high, both full width).
fn wire_bounds(pos: BlockPos, attached: bool) -> Aabb {
    let (x, y, z) = (f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
    let (min_y, max_y) = if attached {
        (1.0 / 16.0, 2.5 / 16.0)
    } else {
        (0.0, 8.0 / 16.0)
    };
    Aabb::new(x, y + min_y, z, x + 1.0, y + max_y, z + 1.0)
}

/// `checkPressed(level, pos, entities)` with whether any entity presses it.
fn check_pressed_with(level: &mut Level, pos: BlockPos, should_be_pressed: bool) {
    let mut state = level.block(pos);
    let was_pressed = powered(state);
    if should_be_pressed != was_pressed {
        state = state.set_flag("powered", should_be_pressed);
        level.set_block_and_update(pos, state);
        update_source(level, pos, state);
    }
    if should_be_pressed {
        level.schedule_block_tick(pos, ids::TRIPWIRE, 10);
    } else if was_pressed {
        level.schedule_block_tick(pos, ids::TRIPWIRE, 1);
    }
}

// --- TripWireHookBlock -------------------------------------------------------------------

/// `setPlacedBy`: the block placing code calls this after placing a hook.
pub fn hook_set_placed_by(level: &mut Level, pos: BlockPos, state: BlockState) {
    calculate_state(level, pos, state, false, false, -1, None);
}

/// `tick`.
fn hook_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    calculate_state(level, pos, state, false, true, -1, None);
}

/// `calculateState(level, pos, state, isBeingDestroyed, canUpdate, wireSource,
/// wireSourceState)`.
fn calculate_state(
    level: &mut Level,
    pos: BlockPos,
    state: BlockState,
    being_destroyed: bool,
    can_update: bool,
    wire_source: i32,
    wire_source_state: Option<BlockState>,
) {
    let Some(direction) = state.direction("facing") else {
        return;
    };
    let was_attached = state.flag("attached");
    let was_powered = state.flag("powered");
    let block = state.block();
    let mut attached = !being_destroyed;
    let mut powered_now = false;
    let mut receiver = 0;
    let mut wires: [Option<BlockState>; WIRE_DIST_MAX as usize] = [None; WIRE_DIST_MAX as usize];
    for i in 1..WIRE_DIST_MAX {
        let test = step(pos, direction, i);
        let mut wire = level.block(test);
        if wire.is(ids::TRIPWIRE_HOOK) {
            if wire.direction("facing") == Some(direction.opposite()) {
                receiver = i;
            }
            break;
        }
        if !wire.is(ids::TRIPWIRE) && i != wire_source {
            wires[i as usize] = None;
            attached = false;
        } else {
            if i == wire_source {
                wire = wire_source_state.unwrap_or(wire);
            }
            let armed = !wire.flag("disarmed");
            let wire_powered = wire.flag("powered");
            powered_now |= armed && wire_powered;
            wires[i as usize] = Some(wire);
            if i == wire_source {
                level.schedule_block_tick(pos, block, 10);
                attached &= armed;
            }
        }
    }
    attached &= receiver > 1;
    powered_now &= attached;
    let new_state = block
        .default_state()
        .set_flag("attached", attached)
        .set_flag("powered", powered_now);
    if receiver > 0 {
        let test = step(pos, direction, receiver);
        let opposite = direction.opposite();
        level.set_block_and_update(test, new_state.set_direction("facing", opposite));
        notify_neighbors(level, block, test, opposite);
        if !level.block(pos).is(ids::TRIPWIRE_HOOK) {
            on_removed(level, new_state, pos);
            return;
        }
        emit_state(
            level,
            test,
            attached,
            powered_now,
            was_attached,
            was_powered,
        );
    }
    emit_state(level, pos, attached, powered_now, was_attached, was_powered);
    if !being_destroyed {
        level.set_block_and_update(pos, new_state.set_direction("facing", direction));
        if can_update {
            notify_neighbors(level, block, pos, direction);
        }
    }
    if was_attached != attached {
        for i in 1..receiver {
            let test = step(pos, direction, i);
            let Some(wire) = wires[i as usize] else {
                continue;
            };
            let current = level.block(test);
            if current.is(ids::TRIPWIRE) || current.is(ids::TRIPWIRE_HOOK) {
                level.set_block_and_update(test, wire.set_flag("attached", attached));
            }
        }
    }
}

/// `emitState`: the click, attach and detach sounds.
fn emit_state(
    level: &mut Level,
    pos: BlockPos,
    attached: bool,
    powered: bool,
    was_attached: bool,
    was_powered: bool,
) {
    if powered && !was_powered {
        level.play_block_sound(
            None,
            pos,
            sound_id("block.tripwire.click_on"),
            SOUND_BLOCKS,
            0.4,
            0.6,
        );
        level.game_event_at(None, crate::level::game_event::events().block_activate, pos);
    } else if !powered && was_powered {
        level.play_block_sound(
            None,
            pos,
            sound_id("block.tripwire.click_off"),
            SOUND_BLOCKS,
            0.4,
            0.5,
        );
        level.game_event_at(
            None,
            crate::level::game_event::events().block_deactivate,
            pos,
        );
    } else if attached && !was_attached {
        level.play_block_sound(
            None,
            pos,
            sound_id("block.tripwire.attach"),
            SOUND_BLOCKS,
            0.4,
            0.7,
        );
        level.game_event_at(None, crate::level::game_event::events().block_attach, pos);
    } else if !attached && was_attached {
        let pitch = 1.2 / (level.random().next_float() * 0.2 + 0.9);
        level.play_block_sound(
            None,
            pos,
            sound_id("block.tripwire.detach"),
            SOUND_BLOCKS,
            0.4,
            pitch,
        );
        level.game_event_at(None, crate::level::game_event::events().block_detach, pos);
    }
}

/// `notifyNeighbors(block, level, pos, direction)`: the hook and the block it hangs on.
fn notify_neighbors(level: &mut Level, block: BlockId, pos: BlockPos, direction: Direction) {
    let front = direction.opposite();
    level.update_neighbors_at(pos, block);
    level.update_neighbors_at(pos.relative(front), block);
}

/// `affectNeighborsAfterRemoval`.
fn hook_affect_neighbors_after_removal(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    moved: bool,
) {
    if !moved {
        on_removed(level, state, pos);
    }
}

/// `onRemoved(state, level, pos)`.
fn on_removed(level: &mut Level, state: BlockState, pos: BlockPos) {
    let attached = state.flag("attached");
    let is_powered = state.flag("powered");
    if attached || is_powered {
        calculate_state(level, pos, state, true, false, -1, None);
    }
    if is_powered && let Some(facing) = state.direction("facing") {
        notify_neighbors(level, state.block(), pos, facing);
    }
}

/// `ownSignal`.
fn hook_signal(_level: &Level, state: BlockState, _pos: BlockPos, _direction: Direction) -> i32 {
    if powered(state) { 15 } else { 0 }
}

/// `getDirectSignal`: strongly into the block it hangs on.
fn hook_direct_signal(
    _level: &Level,
    state: BlockState,
    _pos: BlockPos,
    direction: Direction,
) -> i32 {
    if powered(state) && state.direction("facing") == Some(direction) {
        15
    } else {
        0
    }
}

/// `pos.relative(direction, distance)`.
fn step(pos: BlockPos, direction: Direction, distance: i32) -> BlockPos {
    let [dx, dy, dz] = direction.normal();
    pos.offset(dx * distance, dy * distance, dz * distance)
}
