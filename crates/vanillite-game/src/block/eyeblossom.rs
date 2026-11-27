//! Eyeblossoms (`EyeblossomBlock`): they open at night and close at day
//! (`EnvironmentAttributes.EYEBLOSSOM_OPEN`); a switching blossom makes the same blossoms
//! around it follow after a delay that grows with their distance.

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Vec3};
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_worldgen::behaviour::StateExt;

use super::{BlockBehaviour, sound_id};
use crate::level::game_event::{GameEventContext, events};
use crate::level::{Level, SOUND_BLOCKS};

pub(super) fn fill(block: BlockId, b: &mut BlockBehaviour) {
    if block == ids::OPEN_EYEBLOSSOM || block == ids::CLOSED_EYEBLOSSOM {
        b.random_tick = Some(random_tick);
        b.tick = Some(tick);
    } else if block == ids::POTTED_OPEN_EYEBLOSSOM || block == ids::POTTED_CLOSED_EYEBLOSSOM {
        b.random_tick = Some(potted_random_tick);
    }
}

/// `FlowerPotBlock.randomTick` of a potted eyeblossom: it switches alone, with the long sound.
fn potted_random_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    let open = state.is(ids::POTTED_OPEN_EYEBLOSSOM);
    if level.spawner_env.env.eyeblossom_open.unwrap_or(open) == open {
        return;
    }
    let opposite = if open {
        ids::POTTED_CLOSED_EYEBLOSSOM
    } else {
        ids::POTTED_OPEN_EYEBLOSSOM
    };
    level.set_block_and_update(pos, opposite.default_state());
    spawn_transform_particle(level, pos, !open);
    let sound = if open {
        "block.eyeblossom.close_long"
    } else {
        "block.eyeblossom.open_long"
    };
    level.play_block_sound(None, pos, sound_id(sound), SOUND_BLOCKS, 1.0, 1.0);
}

/// `randomTick`: the long switch sound.
fn random_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    switch(level, state, pos, true);
}

/// `tick` (scheduled by a neighbour that switched): the short switch sound.
fn tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    switch(level, state, pos, false);
}

/// `tryChangingState` and the switch sound of the new type.
fn switch(level: &mut Level, state: BlockState, pos: BlockPos, long: bool) {
    let open = state.is(ids::OPEN_EYEBLOSSOM);
    let should_be_open = level.spawner_env.env.eyeblossom_open.unwrap_or(open);
    if should_be_open == open {
        return;
    }
    let new = if open {
        ids::CLOSED_EYEBLOSSOM
    } else {
        ids::OPEN_EYEBLOSSOM
    };
    level.set_block_and_update(pos, new.default_state());
    let context = GameEventContext::with_state(None, state);
    level.game_event_block(events().block_change, pos, context);
    spawn_transform_particle(level, pos, !open);
    let neighbours =
        super::find_blocks_in(level, pos.offset(-3, -2, -3), pos.offset(3, 2, 3), |s| {
            s == state
        });
    for nearby in neighbours {
        let distance = pos.dist_sqr(nearby).sqrt();
        #[allow(clippy::cast_possible_truncation)]
        let delay = level
            .random()
            .next_int_between_inclusive((distance * 5.0) as i32, (distance * 10.0) as i32);
        level.schedule_block_tick(nearby, state.block(), delay);
    }
    let sound = match (!open, long) {
        (true, true) => "block.eyeblossom.open_long",
        (true, false) => "block.eyeblossom.open",
        (false, true) => "block.eyeblossom.close_long",
        (false, false) => "block.eyeblossom.close",
    };
    level.play_block_sound(None, pos, sound_id(sound), SOUND_BLOCKS, 1.0, 1.0);
}

/// `Type.spawnTransformParticle`: a trail rising from the blossom in the new type's colour.
fn spawn_transform_particle(level: &mut Level, pos: BlockPos, open: bool) {
    let start = Vec3::new(
        f64::from(pos.x) + 0.5,
        f64::from(pos.y) + 0.5,
        f64::from(pos.z) + 0.5,
    );
    let random = level.random();
    let lifetime = 0.5 + random.next_double();
    let vx = random.next_double() - 0.5;
    let vy = random.next_double() + 1.0;
    let vz = random.next_double() - 0.5;
    let target = Vec3::new(
        start.x + vx * lifetime,
        start.y + vy * lifetime,
        start.z + vz * lifetime,
    );
    let color = if open { 16_545_810 } else { 6_250_335 };
    #[allow(clippy::cast_possible_truncation)]
    let duration = (20.0 * lifetime) as i32;
    let particle = crate::level::particles::trail(target, color, duration);
    level.send_particles(particle, start, 1, Vec3::ZERO, 0.0);
}
