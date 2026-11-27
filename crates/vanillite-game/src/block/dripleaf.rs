//! The tilting leaf of big dripleaves (`BigDripleafBlock`): an entity standing on it tips it
//! over step by step until it lets the entity through; it rights itself later or when powered.

use vanillite_base::BlockPos;
use vanillite_base::random::RandomSource;
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_world::entity::EntityId;

use super::{BlockBehaviour, sound_id};
use crate::level::game_event::events;
use crate::level::{Level, SOUND_BLOCKS, update};

pub(super) fn fill(block: BlockId, b: &mut BlockBehaviour) {
    if block == ids::BIG_DRIPLEAF {
        b.tick = Some(tick);
        b.neighbor_changed = Some(neighbor_changed);
        b.entity_inside = Some(entity_inside);
    }
}

/// `DELAY_UNTIL_NEXT_TILT_STATE`.
fn delay(tilt: &str) -> Option<i32> {
    match tilt {
        "unstable" | "partial" => Some(10),
        "full" => Some(100),
        _ => None,
    }
}

/// `BigDripleafBlock.entityInside`: an entity standing on the leaf (`canEntityTilt`) makes it
/// unstable.
fn entity_inside(level: &mut Level, state: BlockState, pos: BlockPos, entity: EntityId) {
    if state.value("tilt") != Some("none") || level.has_neighbor_signal(pos) {
        return;
    }
    let Some(e) = level.entities.store.get(entity) else {
        return;
    };
    let on_ground = e
        .flags()
        .contains(vanillite_world::entity::EntityFlags::ON_GROUND);
    if on_ground && e.pos().y > f64::from(pos.y) + f64::from(0.6875_f32) {
        set_tilt_and_schedule(level, state, pos, "unstable", None);
    }
}

/// `BigDripleafBlock.tick`.
fn tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    if level.has_neighbor_signal(pos) {
        reset_tilt(level, state, pos);
        return;
    }
    let down = sound_id("block.big_dripleaf.tilt_down");
    match state.value("tilt") {
        Some("unstable") => set_tilt_and_schedule(level, state, pos, "partial", Some(down)),
        Some("partial") => set_tilt_and_schedule(level, state, pos, "full", Some(down)),
        Some("full") => reset_tilt(level, state, pos),
        _ => {}
    }
}

/// `BigDripleafBlock.neighborChanged`: power rights the leaf.
fn neighbor_changed(level: &mut Level, state: BlockState, pos: BlockPos, _: BlockId, _: bool) {
    if level.has_neighbor_signal(pos) {
        reset_tilt(level, state, pos);
    }
}

/// `setTiltAndScheduleTick`.
fn set_tilt_and_schedule(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    tilt: &str,
    sound: Option<u16>,
) {
    set_tilt(level, state, pos, tilt);
    if let Some(sound) = sound {
        play_tilt_sound(level, pos, sound);
    }
    if let Some(d) = delay(tilt) {
        level.schedule_block_tick(pos, ids::BIG_DRIPLEAF, d);
    }
}

/// `BigDripleafBlock.onProjectileHit`: the leaf tips over at once.
pub(super) fn projectile_hit(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    _: &super::projectile_hit::ProjectileHit,
) {
    set_tilt_and_schedule(
        level,
        state,
        pos,
        "full",
        Some(sound_id("block.big_dripleaf.tilt_down")),
    );
}

/// `resetTilt`.
fn reset_tilt(level: &mut Level, state: BlockState, pos: BlockPos) {
    set_tilt(level, state, pos, "none");
    if state.value("tilt") != Some("none") {
        play_tilt_sound(level, pos, sound_id("block.big_dripleaf.tilt_up"));
    }
}

/// `setTilt`: every tilt but `unstable` makes a vibration when it changes.
fn set_tilt(level: &mut Level, state: BlockState, pos: BlockPos, tilt: &str) {
    let previous = state.value("tilt");
    let new = state.with("tilt", tilt).unwrap_or(state);
    level.set_block(pos, new, update::CLIENTS);
    if tilt != "unstable" && previous != Some(tilt) {
        level.game_event_at(None, events().block_change, pos);
    }
}

/// `playTiltSound`: `Mth.randomBetween(random, 0.8F, 1.2F)` pitch.
fn play_tilt_sound(level: &mut Level, pos: BlockPos, sound: u16) {
    let pitch = level.random().next_float() * (1.2_f32 - 0.8_f32) + 0.8;
    level.play_block_sound(None, pos, sound, SOUND_BLOCKS, 1.0, pitch);
}
