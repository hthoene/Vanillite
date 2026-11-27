//! Observers, levers, buttons, the redstone block, lamps, copper bulbs, lightning rods, doors,
//! trapdoors and fence gates.

use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockId, BlockState};
use vanillite_worldgen::behaviour::{BlockLevel, PosExt, StateExt};

use super::{BlockSet, connected_direction, direction_from_yaw, door_pitch, game_event};
use crate::block::fluid::WATER;
use crate::block::{Interaction, UseContext};
use crate::level::signal::powered;
use crate::level::{Level, SOUND_BLOCKS, update};

// --- Shared signals ------------------------------------------------------------------------

/// `ownSignal` of blocks that emit 15 everywhere while powered (levers, buttons, lightning
/// rods).
pub(super) fn powered_signal(_: &Level, state: BlockState, _: BlockPos, _: Direction) -> i32 {
    if powered(state) { 15 } else { 0 }
}

/// `PoweredBlock.ownSignal`: the redstone block.
pub(super) fn full_signal(_: &Level, _: BlockState, _: BlockPos, _: Direction) -> i32 {
    15
}

/// `getDirectSignal` of levers and buttons: into the block they are attached to.
pub(super) fn attached_direct_signal(
    _: &Level,
    state: BlockState,
    _: BlockPos,
    direction: Direction,
) -> i32 {
    if powered(state) && connected_direction(state) == direction {
        15
    } else {
        0
    }
}

/// `updateNeighbours` of levers and buttons: around the block, then around the block it is
/// attached to.
fn update_attached(level: &mut Level, state: BlockState, pos: BlockPos) {
    let front = connected_direction(state).opposite();
    level.update_neighbors_at(pos, state.block());
    level.update_neighbors_at(pos.relative(front), state.block());
}

/// `affectNeighborsAfterRemoval` of levers and buttons.
pub(super) fn attached_removal(level: &mut Level, state: BlockState, pos: BlockPos, moved: bool) {
    if !moved && powered(state) {
        update_attached(level, state, pos);
    }
}

// --- Observer -------------------------------------------------------------------------------

fn facing(state: BlockState) -> Direction {
    state.direction("facing").unwrap_or(Direction::North)
}

/// `ObserverBlock.getSignal` / `getDirectSignal`: out of its back.
pub(super) fn observer_signal(
    _: &Level,
    state: BlockState,
    _: BlockPos,
    direction: Direction,
) -> i32 {
    if facing(state) == direction && powered(state) {
        15
    } else {
        0
    }
}

/// `ObserverBlock.tick`: a 2-tick pulse.
pub(super) fn observer_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    if powered(state) {
        level.set_block(pos, state.set_flag("powered", false), update::CLIENTS);
    } else {
        level.set_block(pos, state.set_flag("powered", true), update::CLIENTS);
        level.schedule_block_tick(pos, state.block(), 2);
    }
    observer_update_front(level, pos, state);
}

/// `ObserverBlock.updateNeighborsInFront`.
fn observer_update_front(level: &mut Level, pos: BlockPos, state: BlockState) {
    let direction = facing(state);
    let back = pos.relative(direction.opposite());
    level.neighbor_changed(back, state.block());
    level.update_neighbors_at_except(back, state.block(), Some(direction));
}

/// `ObserverBlock.onPlace`: an observer placed powered without a pending tick turns off.
pub(super) fn observer_on_place(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    old: BlockState,
    _: bool,
) {
    if state.block() != old.block() && powered(state) && !level.has_block_tick(pos, state.block()) {
        let new = state.set_flag("powered", false);
        level.set_block(pos, new, update::CLIENTS | update::KNOWN_SHAPE);
        observer_update_front(level, pos, new);
    }
}

/// `ObserverBlock.affectNeighborsAfterRemoval`.
pub(super) fn observer_removal(level: &mut Level, state: BlockState, pos: BlockPos, _: bool) {
    if powered(state) && level.has_block_tick(pos, state.block()) {
        observer_update_front(level, pos, state.set_flag("powered", false));
    }
}

// --- Lever and buttons ----------------------------------------------------------------------

/// `LeverBlock.useWithoutItem` and `pull`.
pub(super) fn lever_use(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    _: &UseContext,
) -> Interaction {
    lever_pull(level, state, pos);
    Interaction::Success
}

/// `LeverBlock.pull` (the sound goes to everyone: Vanilla passes no player).
pub fn lever_pull(level: &mut Level, state: BlockState, pos: BlockPos) {
    let state = state.set_flag("powered", !powered(state));
    level.set_block_and_update(pos, state);
    update_attached(level, state, pos);
    let on = powered(state);
    let sound = crate::block::sound_id("block.lever.click");
    level.play_block_sound(
        None,
        pos,
        sound,
        SOUND_BLOCKS,
        0.3,
        if on { 0.6 } else { 0.5 },
    );
    game_event(
        level,
        if on {
            "block_activate"
        } else {
            "block_deactivate"
        },
        pos,
        None,
    );
}

/// `ticksToStayPressed`: wooden buttons 30, stone buttons 20.
fn button_ticks(block: BlockId) -> i32 {
    if BlockSet::of(block).family == "stone" {
        20
    } else {
        30
    }
}

/// `ButtonBlock.useWithoutItem`.
pub(super) fn button_use(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    context: &UseContext,
) -> Interaction {
    if powered(state) {
        return Interaction::Consume;
    }
    button_press(level, state, pos, Some(context.player));
    Interaction::Success
}

/// `ButtonBlock.press`.
pub fn button_press(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    player: Option<vanillite_world::entity::EntityId>,
) {
    level.set_block_and_update(pos, state.set_flag("powered", true));
    update_attached(level, state, pos);
    level.schedule_block_tick(pos, state.block(), button_ticks(state.block()));
    let sound = BlockSet::of(state.block()).button_sound(true);
    level.play_block_sound(player, pos, sound, SOUND_BLOCKS, 1.0, 1.0);
    game_event(level, "block_activate", pos, player);
}

/// `ButtonBlock.tick`.
pub(super) fn button_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    if powered(state) {
        button_check_pressed(level, state, pos);
    }
}

/// `ButtonBlock.entityInside`: an arrow presses a wooden button.
pub(super) fn button_entity_inside(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    _: vanillite_world::entity::EntityId,
) {
    if BlockSet::of(state.block()).can_button_be_activated_by_arrows() && !powered(state) {
        button_check_pressed(level, state, pos);
    }
}

/// `ButtonBlock.checkPressed`: the button stays pressed while an arrow is in it.
fn button_check_pressed(level: &mut Level, state: BlockState, pos: BlockPos) {
    let arrow = if BlockSet::of(state.block()).can_button_be_activated_by_arrows() {
        first_arrow_in(level, state, pos)
    } else {
        None
    };
    let should_be_pressed = arrow.is_some();
    if should_be_pressed != powered(state) {
        level.set_block_and_update(pos, state.set_flag("powered", should_be_pressed));
        update_attached(level, state, pos);
        let sound = BlockSet::of(state.block()).button_sound(should_be_pressed);
        level.play_block_sound(None, pos, sound, SOUND_BLOCKS, 1.0, 1.0);
        let event = if should_be_pressed {
            "block_activate"
        } else {
            "block_deactivate"
        };
        game_event(level, event, pos, arrow);
    }
    if should_be_pressed {
        level.schedule_block_tick(pos, state.block(), button_ticks(state.block()));
    }
}

/// `level.getEntitiesOfClass(AbstractArrow.class, shape.bounds().move(pos))`'s first.
fn first_arrow_in(
    level: &Level,
    state: BlockState,
    pos: BlockPos,
) -> Option<vanillite_world::entity::EntityId> {
    let shape = state.outline_shape();
    let first = shape.first()?;
    let mut b = *first;
    for s in shape {
        for i in 0..3 {
            b[i] = b[i].min(s[i]);
            b[i + 3] = b[i + 3].max(s[i + 3]);
        }
    }
    let (x, y, z) = (f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
    let aabb =
        vanillite_base::Aabb::new(x + b[0], y + b[1], z + b[2], x + b[3], y + b[4], z + b[5]);
    let store = &level.entities.store;
    let mut found = None;
    store.for_each_in(&aabb, false, |id| {
        if found.is_none()
            && store.get(id).is_some_and(|e| {
                crate::mob::projectile::is_arrow_kind(e.kind())
                    || e.kind() == vanillite_data::entity::ids::TRIDENT
            })
        {
            found = Some(id);
        }
    });
    found
}

// --- Lamps, bulbs, rods -----------------------------------------------------------------------

/// `RedstoneLampBlock.neighborChanged`: on at once, off after 4 ticks.
pub(super) fn lamp_neighbor_changed(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    _: BlockId,
    _: bool,
) {
    let lit = state.flag("lit");
    if lit != level.has_neighbor_signal(pos) {
        if lit {
            level.schedule_block_tick(pos, state.block(), 4);
        } else {
            level.set_block(pos, state.set_flag("lit", true), update::CLIENTS);
        }
    }
}

/// `RedstoneLampBlock.tick`.
pub(super) fn lamp_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    if state.flag("lit") && !level.has_neighbor_signal(pos) {
        level.set_block(pos, state.set_flag("lit", false), update::CLIENTS);
    }
}

/// `CopperBulbBlock.onPlace`.
pub(super) fn bulb_on_place(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    old: BlockState,
    _: bool,
) {
    if old.block() != state.block() {
        bulb_check_and_flip(level, state, pos);
    }
}

/// `CopperBulbBlock.neighborChanged`.
pub(super) fn bulb_neighbor_changed(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    _: BlockId,
    _: bool,
) {
    bulb_check_and_flip(level, state, pos);
}

/// `CopperBulbBlock.checkAndFlip`: a rising signal toggles the light.
fn bulb_check_and_flip(level: &mut Level, state: BlockState, pos: BlockPos) {
    let signal = level.has_neighbor_signal(pos);
    if signal == powered(state) {
        return;
    }
    let mut new = state;
    if !powered(state) {
        new = state.set_flag("lit", !state.flag("lit"));
        let name = if new.flag("lit") {
            "block.copper_bulb.turn_on"
        } else {
            "block.copper_bulb.turn_off"
        };
        let sound = crate::block::sound_id(name);
        level.play_block_sound(None, pos, sound, SOUND_BLOCKS, 1.0, 1.0);
    }
    level.set_block_and_update(pos, new.set_flag("powered", signal));
}

/// `CopperBulbBlock.getAnalogOutputSignal`.
pub(super) fn bulb_analog_output(level: &Level, _: BlockState, pos: BlockPos, _: Direction) -> i32 {
    if level.block(pos).flag("lit") { 15 } else { 0 }
}

/// `LightningRodBlock.getDirectSignal`: into the block it stands on or hangs from.
pub(super) fn lightning_rod_direct_signal(
    _: &Level,
    state: BlockState,
    _: BlockPos,
    direction: Direction,
) -> i32 {
    if powered(state) && facing(state) == direction {
        15
    } else {
        0
    }
}

/// `LightningRodBlock.onLightningStrike`: a pulse of 8 ticks into the block it stands on or
/// hangs from.
pub(super) fn lightning_rod_strike(level: &mut Level, state: BlockState, pos: BlockPos) {
    level.set_block_and_update(pos, state.set_flag("powered", true));
    let direction = facing(state);
    level.update_neighbors_at(pos.relative(direction.opposite()), state.block());
    level.schedule_block_tick(pos, state.block(), 8);
    let axis = match direction {
        Direction::West | Direction::East => 0,
        Direction::Down | Direction::Up => 1,
        Direction::North | Direction::South => 2,
    };
    level.level_event(None, 3002, pos, axis);
}

/// `LightningRodBlock.onPlace`: a powered rod that became another block (a copper stage)
/// ends its pulse itself.
pub(super) fn lightning_rod_on_place(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    old: BlockState,
    _: bool,
) {
    if old.block() != state.block()
        && powered(state)
        && !level.chunks.has_block_tick(pos, state.block().0)
    {
        level.schedule_block_tick(pos, state.block(), 8);
    }
}

/// `LightningRodBlock.affectNeighborsAfterRemoval`.
pub(super) fn lightning_rod_removal(level: &mut Level, state: BlockState, pos: BlockPos, _: bool) {
    if powered(state) {
        let direction = facing(state);
        level.update_neighbors_at(pos.relative(direction.opposite()), state.block());
    }
}

/// `LightningRodBlock.tick`: the pulse of a strike ends.
pub(super) fn lightning_rod_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    let new = state.set_flag("powered", false);
    level.set_block(pos, new, update::ALL);
    let direction = facing(state);
    level.update_neighbors_at(pos.relative(direction.opposite()), state.block());
}

// --- Doors, trapdoors, fence gates --------------------------------------------------------------

fn is_lower(state: BlockState) -> bool {
    state.value("half") == Some("lower")
}

/// `DoorBlock.neighborChanged`.
pub(super) fn door_neighbor_changed(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    source: BlockId,
    _: bool,
) {
    let other = if is_lower(state) {
        pos.above()
    } else {
        pos.below()
    };
    let signal = level.has_neighbor_signal(pos) || level.has_neighbor_signal(other);
    if source != state.block() && signal != powered(state) {
        if signal != state.flag("open") {
            door_sound(level, state, pos, signal, None);
            game_event(
                level,
                if signal { "block_open" } else { "block_close" },
                pos,
                None,
            );
        }
        let new = state.set_flag("powered", signal).set_flag("open", signal);
        level.set_block(pos, new, update::CLIENTS);
    }
}

fn door_sound(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    open: bool,
    player: Option<vanillite_world::entity::EntityId>,
) {
    let sound = BlockSet::of(state.block()).door_sound(open);
    let pitch = door_pitch(level);
    level.play_block_sound(player, pos, sound, SOUND_BLOCKS, 1.0, pitch);
}

/// `DoorBlock.setOpen(source, level, state, pos, open)`: mobs opening and closing doors.
pub fn set_door_open(
    level: &mut Level,
    pos: BlockPos,
    open: bool,
    source: Option<vanillite_world::entity::EntityId>,
) {
    let state = level.block(pos);
    if !state.block().name().ends_with("_door") || state.flag("open") == open {
        return;
    }
    let new = state.set_flag("open", open);
    level.set_block(pos, new, update::CLIENTS | update::IMMEDIATE);
    door_sound(level, new, pos, open, source);
    game_event(
        level,
        if open { "block_open" } else { "block_close" },
        pos,
        source,
    );
}

/// `DoorBlock.useWithoutItem`.
pub(super) fn door_use(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    context: &UseContext,
) -> Interaction {
    if !BlockSet::of(state.block()).can_open_by_hand {
        return Interaction::Pass;
    }
    let new = state.set_flag("open", !state.flag("open"));
    level.set_block(pos, new, update::CLIENTS | update::IMMEDIATE);
    let open = new.flag("open");
    door_sound(level, new, pos, open, Some(context.player));
    game_event(
        level,
        if open { "block_open" } else { "block_close" },
        pos,
        Some(context.player),
    );
    Interaction::Success
}

/// `TrapDoorBlock.neighborChanged`.
pub(super) fn trapdoor_neighbor_changed(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    _: BlockId,
    _: bool,
) {
    let signal = level.has_neighbor_signal(pos);
    if signal == powered(state) {
        return;
    }
    let mut new = state;
    if state.flag("open") != signal {
        new = new.set_flag("open", signal);
        trapdoor_sound(level, state, pos, signal, None);
    }
    level.set_block(pos, new.set_flag("powered", signal), update::CLIENTS);
    if new.waterlogged() {
        BlockLevel::schedule_fluid_tick(level, pos, WATER);
    }
}

/// `TrapDoorBlock.playSound` with its game event.
fn trapdoor_sound(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    open: bool,
    player: Option<vanillite_world::entity::EntityId>,
) {
    let sound = BlockSet::of(state.block()).trapdoor_sound(open);
    let pitch = door_pitch(level);
    level.play_block_sound(player, pos, sound, SOUND_BLOCKS, 1.0, pitch);
    game_event(
        level,
        if open { "block_open" } else { "block_close" },
        pos,
        player,
    );
}

/// `TrapDoorBlock.useWithoutItem` and `toggle`.
pub(super) fn trapdoor_use(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    context: &UseContext,
) -> Interaction {
    if !BlockSet::of(state.block()).can_open_by_hand {
        return Interaction::Pass;
    }
    let new = state.set_flag("open", !state.flag("open"));
    level.set_block(pos, new, update::CLIENTS);
    if new.waterlogged() {
        BlockLevel::schedule_fluid_tick(level, pos, WATER);
    }
    trapdoor_sound(level, state, pos, new.flag("open"), Some(context.player));
    Interaction::Success
}

/// `FenceGateBlock.neighborChanged`.
pub(super) fn fence_gate_neighbor_changed(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    _: BlockId,
    _: bool,
) {
    let signal = level.has_neighbor_signal(pos);
    if powered(state) == signal {
        return;
    }
    level.set_block(
        pos,
        state.set_flag("powered", signal).set_flag("open", signal),
        update::CLIENTS,
    );
    if state.flag("open") != signal {
        let sound = BlockSet::of(state.block()).fence_gate_sound(signal);
        let pitch = door_pitch(level);
        level.play_block_sound(None, pos, sound, SOUND_BLOCKS, 1.0, pitch);
        game_event(
            level,
            if signal { "block_open" } else { "block_close" },
            pos,
            None,
        );
    }
}

/// `TrapDoorBlock.onExplosionHit` of an explosion that triggers blocks (wind charges):
/// `toggle` unless powered or iron.
pub fn trapdoor_explosion_hit(level: &mut Level, state: BlockState, pos: BlockPos) {
    if !BlockSet::of(state.block()).can_open_by_wind_charge() || powered(state) {
        return;
    }
    let new = state.set_flag("open", !state.flag("open"));
    level.set_block(pos, new, update::CLIENTS);
    if new.waterlogged() {
        BlockLevel::schedule_fluid_tick(level, pos, WATER);
    }
    trapdoor_sound(level, state, pos, new.flag("open"), None);
}

/// `DoorBlock.onExplosionHit` of an explosion that triggers blocks: the lower half toggles
/// the door unless powered or iron.
pub fn door_explosion_hit(level: &mut Level, state: BlockState, pos: BlockPos) {
    if is_lower(state) && BlockSet::of(state.block()).can_open_by_wind_charge() && !powered(state) {
        set_door_open(level, pos, !state.flag("open"), None);
    }
}

/// `FenceGateBlock.onExplosionHit` of an explosion that triggers blocks.
pub fn fence_gate_explosion_hit(level: &mut Level, state: BlockState, pos: BlockPos) {
    if powered(state) {
        return;
    }
    let open = state.flag("open");
    level.set_block_and_update(pos, state.set_flag("open", !open));
    let sound = BlockSet::of(state.block()).fence_gate_sound(!open);
    let pitch = door_pitch(level);
    level.play_block_sound(None, pos, sound, SOUND_BLOCKS, 1.0, pitch);
    let events = crate::level::game_event::events();
    let event = if open {
        events.block_close
    } else {
        events.block_open
    };
    level.game_event_block(
        event,
        pos,
        crate::level::game_event::GameEventContext {
            source: None,
            affected: Some(state),
        },
    );
}

/// `ButtonBlock.onExplosionHit` of an explosion that triggers blocks.
pub fn button_explosion_hit(level: &mut Level, state: BlockState, pos: BlockPos) {
    if !powered(state) {
        button_press(level, state, pos, None);
    }
}

/// `FenceGateBlock.useWithoutItem`: opens away from the player.
pub(super) fn fence_gate_use(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    context: &UseContext,
) -> Interaction {
    let new = if state.flag("open") {
        state.set_flag("open", false)
    } else {
        let direction = direction_from_yaw(context.yaw);
        let mut new = state;
        if facing(state) == direction.opposite() {
            new = new.set_direction("facing", direction);
        }
        new.set_flag("open", true)
    };
    level.set_block(pos, new, update::CLIENTS | update::IMMEDIATE);
    let open = new.flag("open");
    let sound = BlockSet::of(state.block()).fence_gate_sound(open);
    let pitch = door_pitch(level);
    level.play_block_sound(Some(context.player), pos, sound, SOUND_BLOCKS, 1.0, pitch);
    game_event(
        level,
        if open { "block_open" } else { "block_close" },
        pos,
        Some(context.player),
    );
    Interaction::Success
}

// --- Composter (its fill level is an analog output) -------------------------------------------

/// `ComposterBlock.onPlace`: a composter at level 7 becomes ready after 20 ticks.
pub(super) fn composter_on_place(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    _: BlockState,
    _: bool,
) {
    if state.int("level") == Some(7) {
        level.schedule_block_tick(pos, state.block(), 20);
    }
}

/// `ComposterBlock.tick`.
pub(super) fn composter_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    if state.int("level") == Some(7) {
        level.set_block_and_update(pos, state.set_int("level", 8));
        let sound = crate::block::sound_id("block.composter.ready");
        level.play_block_sound(None, pos, sound, SOUND_BLOCKS, 1.0, 1.0);
    }
}
