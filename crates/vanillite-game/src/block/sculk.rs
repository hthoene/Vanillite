//! Sculk sensors and calibrated sculk sensors (`SculkSensorBlock`, `CalibratedSculkSensorBlock`,
//! their block entities) and the vibration system they listen with (`VibrationSystem`,
//! `VibrationSelector`, `VibrationInfo`).
//!
//! A sensor's block entity is a game event listener ([`crate::level::game_event`]). A game event
//! in range becomes a candidate (the nearest of a tick wins, ties go to the higher frequency);
//! one tick later the vibration starts travelling, one block per tick, and on arrival the sensor
//! activates with a power that falls with the distance. The vibration particle is not sent.

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, ChunkPos, Direction, Vec3};
use vanillite_config::GameMode;
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_data::entity::ids as entity_ids;
use vanillite_world::entity::{EntityFlags, EntityId};
use vanillite_worldgen::behaviour::{PosExt, StateExt, tags};

use super::{BlockBehaviour, sound_id};
use crate::level::game_event::{
    self, GameEventContext, center, containing, dist_sqr, events, frequency,
};
use crate::level::signal::DIRECTIONS;
use crate::level::{BlockEntity, Level, SOUND_BLOCKS, update};

/// `SculkSensorBlock.getActiveTicks()`.
const ACTIVE_TICKS: i32 = 30;
/// `CalibratedSculkSensorBlock.getActiveTicks()`.
const CALIBRATED_ACTIVE_TICKS: i32 = 10;
/// `SculkSensorBlock.COOLDOWN_TICKS`.
const COOLDOWN_TICKS: i32 = 10;

/// `VibrationInfo`: a vibration on its way.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vibration {
    /// The game event.
    pub event: u16,
    /// Distance from its origin to the listener.
    pub distance: f32,
    /// Where it started.
    pub pos: Vec3,
    /// The entity that caused it.
    pub source: Option<EntityId>,
}

/// The data of a `SculkSensorBlockEntity` (`CalibratedSculkSensorBlockEntity` if `calibrated`)
/// with its `VibrationSystem.Data`.
#[derive(Clone, Debug, PartialEq)]
pub struct SculkSensor {
    /// A calibrated sensor (listener radius 16, filtered by the signal behind it).
    pub calibrated: bool,
    /// `lastVibrationFrequency`.
    pub last_vibration_frequency: i32,
    /// `currentVibration`.
    pub current: Option<Vibration>,
    /// `travelTimeInTicks`.
    pub travel_time: i32,
    /// `VibrationSelector.currentVibrationData`: the candidate and the game time it came.
    pub candidate: Option<(Vibration, i64)>,
}

impl SculkSensor {
    /// A fresh block entity.
    pub fn new(calibrated: bool) -> SculkSensor {
        SculkSensor {
            calibrated,
            last_vibration_frequency: 0,
            current: None,
            travel_time: 0,
            candidate: None,
        }
    }

    /// `VibrationUser.getListenerRadius()`.
    pub fn listener_radius(&self) -> i32 {
        if self.calibrated { 16 } else { 8 }
    }

    /// `VibrationSelector.addCandidate(vibration, tickTime)`.
    fn add_candidate(&mut self, vibration: Vibration, time: i64) {
        let replace = match &self.candidate {
            None => true,
            Some((previous, tick)) => {
                if time != *tick {
                    false
                } else if vibration.distance < previous.distance {
                    true
                } else if vibration.distance > previous.distance {
                    false
                } else {
                    frequency(vibration.event) > frequency(previous.event)
                }
            }
        };
        if replace {
            self.candidate = Some((vibration, time));
        }
    }
}

/// Fills the sculk sensor behaviours.
pub(super) fn fill(block: BlockId, b: &mut BlockBehaviour) {
    if block == ids::SCULK_SENSOR || block == ids::CALIBRATED_SCULK_SENSOR {
        b.tick = Some(tick);
        b.on_place = Some(on_place);
        b.affect_neighbors_after_removal = Some(affect_neighbors_after_removal);
        b.signal = Some(signal);
        b.direct_signal = Some(direct_signal);
        b.analog_output = Some(analog_output);
    }
}

fn is_calibrated(state: BlockState) -> bool {
    state.is(ids::CALIBRATED_SCULK_SENSOR)
}

/// `SculkSensorBlock.canActivate(state)`: the phase is inactive.
fn can_activate(state: BlockState) -> bool {
    state.value("sculk_sensor_phase") == Some("inactive")
}

fn power(state: BlockState) -> i32 {
    i32::from(state.value_index("power").unwrap_or(0))
}

fn with_power(state: BlockState, power: i32) -> BlockState {
    state
        .with_index("power", power.clamp(0, 15) as u16)
        .unwrap_or(state)
}

/// `getSignal`: the power; a calibrated sensor gives none in its facing direction.
fn signal(_: &Level, state: BlockState, _: BlockPos, direction: Direction) -> i32 {
    if is_calibrated(state) && state.direction("facing") == Some(direction) {
        0
    } else {
        power(state)
    }
}

/// `getDirectSignal`: upwards only.
fn direct_signal(level: &Level, state: BlockState, pos: BlockPos, direction: Direction) -> i32 {
    if direction == Direction::Up {
        signal(level, state, pos, direction)
    } else {
        0
    }
}

/// `getAnalogOutputSignal`: the frequency of the last vibration while active.
fn analog_output(level: &Level, state: BlockState, pos: BlockPos, _: Direction) -> i32 {
    match level.block_entity(pos) {
        Some(BlockEntity::SculkSensor(sensor))
            if state.value("sculk_sensor_phase") == Some("active") =>
        {
            sensor.last_vibration_frequency
        }
        _ => 0,
    }
}

/// `SculkSensorBlock.onPlace`: a sensor placed with power and no pending tick loses it.
fn on_place(level: &mut Level, state: BlockState, pos: BlockPos, old: BlockState, _: bool) {
    if !old.is(state.block())
        && power(state) > 0
        && !level.chunks.has_block_tick(pos, state.block().0)
    {
        level.set_block(
            pos,
            with_power(state, 0),
            update::KNOWN_SHAPE | update::CLIENTS,
        );
    }
}

/// `affectNeighborsAfterRemoval`.
fn affect_neighbors_after_removal(level: &mut Level, state: BlockState, pos: BlockPos, _: bool) {
    if state.value("sculk_sensor_phase") == Some("active") {
        update_neighbours(level, pos, state.block());
    }
}

/// `SculkSensorBlock.updateNeighbours`.
fn update_neighbours(level: &mut Level, pos: BlockPos, block: BlockId) {
    level.update_neighbors_at(pos, block);
    level.update_neighbors_at(pos.below(), block);
}

/// `SculkSensorBlock.tick`: active → cooldown, cooldown → inactive.
fn tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    match state.value("sculk_sensor_phase") {
        Some("active") => deactivate(level, pos, state),
        Some("cooldown") => {
            level.set_block_and_update(pos, state.set("sculk_sensor_phase", "inactive"));
            if !state.waterlogged() {
                let pitch = level.random().next_float() * 0.2 + 0.8;
                let sound = sound_id("block.sculk_sensor.clicking_stop");
                level.play_block_sound(None, pos, sound, SOUND_BLOCKS, 1.0, pitch);
            }
        }
        _ => {}
    }
}

/// `SculkSensorBlock.deactivate`.
pub fn deactivate(level: &mut Level, pos: BlockPos, state: BlockState) {
    let cooldown = with_power(state.set("sculk_sensor_phase", "cooldown"), 0);
    level.set_block_and_update(pos, cooldown);
    level.schedule_block_tick(pos, state.block(), COOLDOWN_TICKS);
    update_neighbours(level, pos, state.block());
}

/// `SculkSensorBlock.activate`.
fn activate(
    level: &mut Level,
    source: Option<EntityId>,
    pos: BlockPos,
    state: BlockState,
    power: i32,
    frequency: i32,
) {
    let active = with_power(state.set("sculk_sensor_phase", "active"), power);
    level.set_block_and_update(pos, active);
    let ticks = if is_calibrated(state) {
        CALIBRATED_ACTIVE_TICKS
    } else {
        ACTIVE_TICKS
    };
    level.schedule_block_tick(pos, state.block(), ticks);
    update_neighbours(level, pos, state.block());
    try_resonate_vibration(level, source, pos, frequency);
    level.game_event_at(source, events().sculk_sensor_tendrils_clicking, pos);
    if !state.waterlogged() {
        let pitch = level.random().next_float() * 0.2 + 0.8;
        let sound = sound_id("block.sculk_sensor.clicking");
        level.play_block_sound(None, pos, sound, SOUND_BLOCKS, 1.0, pitch);
    }
}

/// `SculkSensorBlock.tryResonateVibration`: amethyst next to an active sensor resonates.
pub fn try_resonate_vibration(
    level: &mut Level,
    source: Option<EntityId>,
    pos: BlockPos,
    frequency: i32,
) {
    /// `RESONANCE_PITCH_BEND`'s tone map.
    const TONES: [i32; 16] = [0, 0, 2, 4, 6, 7, 9, 10, 12, 14, 15, 18, 19, 21, 22, 24];
    for direction in DIRECTIONS {
        let neighbour = pos.relative(direction);
        let state = level.block(neighbour);
        if tags().blocks.vibration_resonators.contains(state) {
            level.game_event(
                game_event::resonance_event(frequency),
                center(neighbour),
                GameEventContext::with_state(source, state),
            );
            let pitch = super::note::pitch_from_note(TONES[frequency.clamp(0, 15) as usize]);
            let sound = sound_id("block.amethyst_block.resonate");
            level.play_block_sound(None, neighbour, sound, SOUND_BLOCKS, 1.0, pitch);
        }
    }
}

/// `VibrationSystem.Listener.handleGameEvent` of the sensor at `pos`.
pub(crate) fn handle_game_event(
    level: &mut Level,
    pos: BlockPos,
    event: u16,
    context: GameEventContext,
    source: Vec3,
) {
    match level.block_entity(pos) {
        Some(BlockEntity::SculkSensor(sensor)) if sensor.current.is_none() => {}
        _ => return,
    }
    if !is_valid_vibration(level, event, context) {
        return;
    }
    let destination = center(pos);
    if !can_receive_vibration(level, pos, containing(source), event)
        || is_occluded(level, source, destination)
    {
        return;
    }
    schedule_vibration(level, pos, event, context.source, source, destination);
}

/// `Listener.scheduleVibration`: a candidate for the selector.
fn schedule_vibration(
    level: &mut Level,
    pos: BlockPos,
    event: u16,
    source: Option<EntityId>,
    origin: Vec3,
    destination: Vec3,
) {
    let time = level.game_time();
    if let Some(BlockEntity::SculkSensor(sensor)) = level.block_entity_mut(pos) {
        let distance = origin.distance_to_sqr(destination).sqrt() as f32;
        sensor.add_candidate(
            Vibration {
                event,
                distance,
                pos: origin,
                source,
            },
            time,
        );
    }
}

/// `VibrationSystem.User.isValidVibration`.
fn is_valid_vibration(level: &Level, event: u16, context: GameEventContext) -> bool {
    let event_tags = &tags().game_events;
    if !event_tags.vibrations.contains(event) {
        return false;
    }
    if let Some(entity) = context.source {
        if level
            .entities
            .player(entity)
            .is_some_and(|p| p.game_mode == GameMode::Spectator)
        {
            return false;
        }
        let crouching = level
            .entities
            .store
            .get(entity)
            .is_some_and(|e| e.flags().contains(EntityFlags::CROUCHING));
        if crouching && event_tags.ignore_vibrations_sneaking.contains(event) {
            return false;
        }
    }
    context
        .affected
        .is_none_or(|state| !tags().blocks.dampens_vibrations.contains(state))
}

/// `VibrationUser.canReceiveVibration(level, pos, event, context)` of the sensor at `listener`;
/// `pos` is the vibration's block.
fn can_receive_vibration(level: &Level, listener: BlockPos, pos: BlockPos, event: u16) -> bool {
    let state = level.block(listener);
    if is_calibrated(state) {
        // `getBackSignal`: the signal into the back selects one frequency.
        let back = state
            .direction("facing")
            .unwrap_or(Direction::North)
            .opposite();
        let wanted = level.signal(listener.relative(back), back);
        if wanted != 0 && frequency(event) != wanted {
            return false;
        }
    }
    let e = events();
    if pos == listener && (event == e.block_destroy || event == e.block_place) {
        return false;
    }
    frequency(event) != 0 && can_activate(state)
}

/// `Listener.isOccluded`: every line from the source block (nudged towards each face) to the
/// listener crosses a block that occludes vibrations.
fn is_occluded(level: &Level, origin: Vec3, destination: Vec3) -> bool {
    let centred = |v: Vec3| Vec3::new(v.x.floor() + 0.5, v.y.floor() + 0.5, v.z.floor() + 0.5);
    let from = centred(origin);
    let to = centred(destination);
    let nudge = f64::from(1.0e-5f32);
    for direction in DIRECTIONS {
        let [x, y, z] = direction.normal();
        let nudged = Vec3::new(
            from.x + f64::from(x) * nudge,
            from.y + f64::from(y) * nudge,
            from.z + f64::from(z) * nudge,
        );
        if !occluder_in_line(level, nudged, to) {
            return false;
        }
    }
    true
}

/// `isBlockInLine(new ClipBlockStateContext(from, to, OCCLUDES_VIBRATION_SIGNALS))` hits a
/// block (`BlockGetter.traverseBlocks`).
fn occluder_in_line(level: &Level, from: Vec3, to: Vec3) -> bool {
    if from == to {
        return false;
    }
    let occludes = |x: i32, y: i32, z: i32| {
        tags()
            .blocks
            .occludes_vibration_signals
            .contains(level.block(BlockPos::new(x, y, z)))
    };
    let lerp = |delta: f64, a: f64, b: f64| a + delta * (b - a);
    let (to_x, to_y, to_z) = (
        lerp(-1.0e-7, to.x, from.x),
        lerp(-1.0e-7, to.y, from.y),
        lerp(-1.0e-7, to.z, from.z),
    );
    let (from_x, from_y, from_z) = (
        lerp(-1.0e-7, from.x, to.x),
        lerp(-1.0e-7, from.y, to.y),
        lerp(-1.0e-7, from.z, to.z),
    );
    let (mut bx, mut by, mut bz) = (
        from_x.floor() as i32,
        from_y.floor() as i32,
        from_z.floor() as i32,
    );
    if occludes(bx, by, bz) {
        return true;
    }
    let (dx, dy, dz) = (to_x - from_x, to_y - from_y, to_z - from_z);
    let sign = |v: f64| {
        if v == 0.0 {
            0
        } else if v > 0.0 {
            1
        } else {
            -1
        }
    };
    let (sx, sy, sz) = (sign(dx), sign(dy), sign(dz));
    let delta = |s: i32, d: f64| {
        if s == 0 { f64::MAX } else { f64::from(s) / d }
    };
    let (tdx, tdy, tdz) = (delta(sx, dx), delta(sy, dy), delta(sz, dz));
    let frac = |v: f64| v - v.floor();
    let start = |s: i32, td: f64, v: f64| td * if s > 0 { 1.0 - frac(v) } else { frac(v) };
    let (mut tx, mut ty, mut tz) = (
        start(sx, tdx, from_x),
        start(sy, tdy, from_y),
        start(sz, tdz, from_z),
    );
    while tx <= 1.0 || ty <= 1.0 || tz <= 1.0 {
        if tx < ty {
            if tx < tz {
                bx += sx;
                tx += tdx;
            } else {
                bz += sz;
                tz += tdz;
            }
        } else if ty < tz {
            by += sy;
            ty += tdy;
        } else {
            bz += sz;
            tz += tdz;
        }
        if occludes(bx, by, bz) {
            return true;
        }
    }
    false
}

/// The block entity ticker (`VibrationSystem.Ticker.tick`): select the candidate of an earlier
/// tick, let it travel, and receive it on arrival.
pub fn tick_block_entity(level: &mut Level, pos: BlockPos, _: BlockState) {
    let time = level.game_time();
    let Some(BlockEntity::SculkSensor(sensor)) = level.block_entity_mut(pos) else {
        return;
    };
    if sensor.current.is_none() {
        // `trySelectAndScheduleVibration`.
        if let Some((vibration, tick)) = sensor.candidate
            && tick < time
        {
            sensor.current = Some(vibration);
            sensor.travel_time = vibration.distance.floor() as i32;
            sensor.candidate = None;
        }
    }
    let Some(vibration) = sensor.current else {
        return;
    };
    sensor.travel_time = (sensor.travel_time - 1).max(0);
    if sensor.travel_time > 0 {
        return;
    }
    // `receiveVibration`: only with the chunks around the listener ticking.
    let origin = containing(vibration.pos);
    let chunk = pos.chunk();
    for x in chunk.x - 1..=chunk.x + 1 {
        for z in chunk.z - 1..=chunk.z + 1 {
            let neighbour = ChunkPos::new(x, z);
            if !level.is_block_ticking(neighbour) || level.chunks.chunk(neighbour).is_none() {
                return;
            }
        }
    }
    let distance = dist_sqr(origin, pos).sqrt() as f32;
    on_receive_vibration(level, pos, vibration.event, vibration.source, distance);
    if let Some(BlockEntity::SculkSensor(sensor)) = level.block_entity_mut(pos) {
        sensor.current = None;
    }
}

/// `VibrationUser.onReceiveVibration`.
fn on_receive_vibration(
    level: &mut Level,
    pos: BlockPos,
    event: u16,
    source: Option<EntityId>,
    distance: f32,
) {
    let state = level.block(pos);
    if !can_activate(state) {
        return;
    }
    let frequency = frequency(event);
    let Some(BlockEntity::SculkSensor(sensor)) = level.block_entity_mut(pos) else {
        return;
    };
    sensor.last_vibration_frequency = frequency;
    // `VibrationSystem.getRedstoneStrengthForDistance`.
    let scale = 15.0 / f64::from(sensor.listener_radius());
    let power = (15 - (scale * f64::from(distance)).floor() as i32).max(1);
    activate(level, source, pos, state, power, frequency);
}

/// `SculkSensorBlock.stepOn`: an entity (not a warden) stepping on an inactive sensor triggers
/// it directly. Called by entity movement.
pub fn step_on(level: &mut Level, pos: BlockPos, entity: EntityId) {
    let state = level.block(pos);
    if !can_activate(state) {
        return;
    }
    let Some(entity_ref) = level.entities.store.get(entity) else {
        return;
    };
    if entity_ref.kind() == entity_ids::WARDEN {
        return;
    }
    let position = entity_ref.pos();
    let step = events().step;
    if !matches!(level.block_entity(pos), Some(BlockEntity::SculkSensor(_)))
        || !can_receive_vibration(level, pos, pos, step)
    {
        return;
    }
    // `forceScheduleVibration`.
    schedule_vibration(level, pos, step, Some(entity), position, center(pos));
}
