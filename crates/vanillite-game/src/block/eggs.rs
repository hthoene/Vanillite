//! Blocks that hatch into mobs: turtle eggs (`TurtleEggBlock`), sniffer eggs
//! (`SnifferEggBlock`) and frogspawn (`FrogspawnBlock`).

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Vec3};
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_world::entity::{EntityId, Rotation};
use vanillite_worldgen::behaviour::{PosExt, StateExt};

use super::{BlockBehaviour, block_tags, sound_id};
use crate::level::game_event::{GameEventContext, events};
use crate::level::{Level, SOUND_BLOCKS, update};
use crate::mob::{Kind, MobData};

/// `levelEvent` of breaking a block (particles and sound).
const BREAK_PARTICLES: i32 = 2001;

pub(super) fn fill(block: BlockId, b: &mut BlockBehaviour) {
    if block == ids::TURTLE_EGG {
        b.on_place = Some(turtle_on_place);
        b.random_tick = Some(turtle_random_tick);
    } else if block == ids::SNIFFER_EGG {
        b.on_place = Some(sniffer_on_place);
        b.tick = Some(sniffer_tick);
    } else if block == ids::FROGSPAWN {
        b.on_place = Some(frogspawn_on_place);
        b.tick = Some(frogspawn_tick);
    } else if block == ids::DRIED_GHAST {
        b.tick = Some(dried_ghast_tick);
        b.random_tick = Some(dried_ghast_random_tick);
    }
}

// --- Dried ghasts -----------------------------------------------------------------------

/// `DriedGhastBlock.randomTick`: a wet or still hydrated block schedules its next hydration
/// step in 5000 ticks.
fn dried_ghast_random_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    if (state.flag("waterlogged") || state.int("hydration").unwrap_or(0) > 0)
        && !level.chunks.has_block_tick(pos, ids::DRIED_GHAST.0)
    {
        level.schedule_block_tick(pos, ids::DRIED_GHAST, 5000);
    }
}

/// `DriedGhastBlock.tick`: in water the block hydrates up to level 3, then hatches into a
/// ghastling; out of water it dries out a level.
fn dried_ghast_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    let hydration = state.int("hydration").unwrap_or(0);
    let context = GameEventContext::with_state(None, state);
    if !state.flag("waterlogged") {
        if hydration > 0 {
            level.set_block(
                pos,
                state.set_int("hydration", hydration - 1),
                update::CLIENTS,
            );
            level.game_event_block(events().block_change, pos, context);
        }
        return;
    }
    if hydration < 3 {
        let sound = sound_id("block.dried_ghast.transition");
        level.play_block_sound(None, pos, sound, SOUND_BLOCKS, 1.0, 1.0);
        level.set_block(
            pos,
            state.set_int("hydration", hydration + 1),
            update::CLIENTS,
        );
        level.game_event_block(events().block_change, pos, context);
        return;
    }
    // `spawnGhastling`.
    level.remove_block(pos, false);
    let ghastling = crate::mob::fam::mount::ghast::new_ghastling(crate::mob::new_entity_seed());
    let facing = state
        .direction("facing")
        .unwrap_or(vanillite_base::Direction::North);
    let y_rot = facing_y_rot(facing);
    let at = Vec3::new(
        f64::from(pos.x) + 0.5,
        f64::from(pos.y),
        f64::from(pos.z) + 0.5,
    );
    let rot = Rotation {
        y_rot,
        x_rot: 0.0,
        y_head_rot: y_rot,
    };
    let network_ids = std::sync::Arc::clone(level.network_ids_arc());
    let uuid = crate::mob::new_uuid();
    let id = crate::mob::spawn(&mut level.entities, &network_ids, uuid, at, rot, ghastling);
    if !level
        .entities
        .spawned(id, vanillite_abi::spawn_reason::BREEDING, 0)
    {
        return;
    }
    let sound = sound_id("entity.ghastling.spawn");
    level.play_sound(None, at.x, at.y, at.z, sound, SOUND_BLOCKS, 1.0, 1.0);
}

/// `Direction.getYRot(direction)`.
fn facing_y_rot(facing: vanillite_base::Direction) -> f32 {
    use vanillite_base::Direction as D;
    match facing {
        D::South => 0.0,
        D::West => 90.0,
        D::North => 180.0,
        D::East => -90.0,
        D::Up | D::Down => 0.0,
    }
}

/// `0.9F + random.nextFloat() * 0.2F` of the egg sounds.
fn egg_pitch(level: &mut Level) -> f32 {
    0.9 + level.random().next_float() * 0.2
}

/// Adds a hatched mob like `EntityType.create(level, BREEDING)`, `snapTo(x, y, z, yRot, 0)`
/// and `addFreshEntity`: no `finalizeSpawn`, head and body keep their zero rotation.
fn add_hatchling(level: &mut Level, mob: MobData, pos: Vec3, y_rot: f32) -> EntityId {
    let rot = Rotation {
        y_rot,
        x_rot: 0.0,
        y_head_rot: 0.0,
    };
    let network_ids = std::sync::Arc::clone(level.network_ids_arc());
    let uuid = crate::mob::new_uuid();
    let id = crate::mob::spawn(&mut level.entities, &network_ids, uuid, pos, rot, mob);
    level
        .entities
        .spawned(id, vanillite_abi::spawn_reason::BREEDING, 0);
    id
}

// --- Turtle eggs ------------------------------------------------------------------------

/// `TurtleEggBlock.onSand`.
fn on_sand(level: &Level, pos: BlockPos) -> bool {
    block_tags().sand.contains(level.block(pos.below()))
}

/// `TurtleEggBlock.onPlace`: the egg on sand shows its particles.
fn turtle_on_place(
    level: &mut Level,
    _state: BlockState,
    pos: BlockPos,
    _old: BlockState,
    _: bool,
) {
    if on_sand(level, pos) {
        level.level_event(None, 2012, pos, 15);
    }
}

/// `TurtleEggBlock.randomTick`: the eggs crack twice, then hatch into baby turtles.
fn turtle_random_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    // `shouldUpdateHatchLevel`: the environment's hatch chance (all night long near dawn).
    let chance = level.spawner_env.env.turtle_egg_hatch_chance;
    if !(chance > 0.0 && level.random().next_float() < chance) || !on_sand(level, pos) {
        return;
    }
    let hatch = state.int("hatch").unwrap_or(0);
    let context = GameEventContext::with_state(None, state);
    if hatch < 2 {
        let pitch = egg_pitch(level);
        let sound = sound_id("block.turtle_egg.crack");
        level.play_block_sound(None, pos, sound, SOUND_BLOCKS, 0.7, pitch);
        level.set_block(pos, state.set_int("hatch", hatch + 1), update::CLIENTS);
        level.game_event_block(events().block_change, pos, context);
        return;
    }
    let pitch = egg_pitch(level);
    let sound = sound_id("block.turtle_egg.hatch");
    level.play_block_sound(None, pos, sound, SOUND_BLOCKS, 0.7, pitch);
    level.remove_block(pos, false);
    level.game_event_block(events().block_destroy, pos, context);
    let eggs = state.int("eggs").unwrap_or(1);
    for i in 0..eggs {
        level.level_event(None, BREAK_PARTICLES, pos, i32::from(state.0));
        let mut turtle = MobData::new(Kind::Turtle, crate::mob::new_entity_seed());
        turtle.age = -24000;
        turtle.baby = true;
        crate::mob::fam::wildlife::st_mut(&mut turtle).home_pos = pos;
        let at = Vec3::new(
            f64::from(pos.x) + 0.3 + f64::from(i) * 0.2,
            f64::from(pos.y),
            f64::from(pos.z) + 0.3,
        );
        add_hatchling(level, turtle, at, 0.0);
    }
}

/// `TurtleEggBlock.destroyEgg`: an entity stepping (`randomness` 100) or falling (3) on turtle
/// eggs breaks one of them now and then. `can_destroy` is `canDestroyEgg`: players (outside
/// spawn protection) and, with `mob_griefing`, living entities other than turtles and bats.
pub fn trample_turtle_egg(level: &mut Level, pos: BlockPos, can_destroy: bool, randomness: i32) {
    let state = level.block(pos);
    if !state.is(ids::TURTLE_EGG) || !can_destroy {
        return;
    }
    if level.random().next_int_bounded(randomness) == 0 {
        decrease_eggs(level, pos, state);
    }
}

/// `TurtleEggBlock.decreaseEggs`.
pub fn decrease_eggs(level: &mut Level, pos: BlockPos, state: BlockState) {
    let pitch = egg_pitch(level);
    let sound = sound_id("block.turtle_egg.break");
    level.play_block_sound(None, pos, sound, SOUND_BLOCKS, 0.7, pitch);
    let eggs = state.int("eggs").unwrap_or(1);
    if eggs <= 1 {
        level.destroy_block(pos, false, None);
    } else {
        level.set_block(pos, state.set_int("eggs", eggs - 1), update::CLIENTS);
        level.game_event_block(
            events().block_destroy,
            pos,
            GameEventContext::with_state(None, state),
        );
        level.level_event(None, BREAK_PARTICLES, pos, i32::from(state.0));
    }
}

// --- Sniffer eggs -----------------------------------------------------------------------

/// `SnifferEggBlock.onPlace` (every state change): the next hatch stage in a third of the hatch
/// time (halved on moss) plus up to 300 ticks.
fn sniffer_on_place(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    _old: BlockState,
    _: bool,
) {
    let boosted = block_tags()
        .sniffer_egg_hatch_boost
        .contains(level.block(pos.below()));
    if boosted {
        level.level_event(None, 3009, pos, 0);
    }
    let hatch_time = if boosted { 12000 } else { 24000 };
    level.game_event_block(
        events().block_place,
        pos,
        GameEventContext::with_state(None, state),
    );
    let delay = hatch_time / 3 + level.random().next_int_bounded(300);
    level.schedule_block_tick(pos, ids::SNIFFER_EGG, delay);
}

/// `SnifferEggBlock.tick`: cracks, then hatches into a baby sniffer.
fn sniffer_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    let hatch = state.int("hatch").unwrap_or(0);
    if hatch != 2 {
        let pitch = egg_pitch(level);
        let sound = sound_id("block.sniffer_egg.crack");
        level.play_block_sound(None, pos, sound, SOUND_BLOCKS, 0.7, pitch);
        level.set_block(pos, state.set_int("hatch", hatch + 1), update::CLIENTS);
        return;
    }
    let pitch = egg_pitch(level);
    let sound = sound_id("block.sniffer_egg.hatch");
    level.play_block_sound(None, pos, sound, SOUND_BLOCKS, 0.7, pitch);
    level.destroy_block(pos, false, None);
    let mut sniffer = MobData::new(Kind::Sniffer, crate::mob::new_entity_seed());
    sniffer.age = -24000;
    sniffer.baby = true;
    let at = Vec3::new(
        f64::from(pos.x) + 0.5,
        f64::from(pos.y) + 0.5,
        f64::from(pos.z) + 0.5,
    );
    let y_rot = vanillite_base::math::wrap_degrees_f(level.random().next_float() * 360.0);
    add_hatchling(level, sniffer, at, y_rot);
}

// --- Frogspawn --------------------------------------------------------------------------

/// `FrogspawnBlock.onPlace`: hatches in 3600 to 11999 ticks.
fn frogspawn_on_place(
    level: &mut Level,
    _state: BlockState,
    pos: BlockPos,
    _old: BlockState,
    _: bool,
) {
    let delay = level.random().next_int_between(3600, 12000);
    level.schedule_block_tick(pos, ids::FROGSPAWN, delay);
}

/// `FrogspawnBlock.tick`: breaks without support, else hatches into two to five tadpoles.
fn frogspawn_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    if !level.can_survive(state, pos) {
        level.destroy_block(pos, false, None);
        return;
    }
    level.destroy_block(pos, false, None);
    let sound = sound_id("block.frogspawn.hatch");
    level.play_block_sound(None, pos, sound, SOUND_BLOCKS, 1.0, 1.0);
    let count = level.random().next_int_between(2, 6);
    for _ in 0..count {
        let mut tadpole = MobData::new(Kind::Tadpole, crate::mob::new_entity_seed());
        let x = f64::from(pos.x) + tadpole_offset(level);
        let z = f64::from(pos.z) + tadpole_offset(level);
        #[allow(clippy::cast_precision_loss)]
        let y_rot = level.random().next_int_between(1, 361) as f32;
        tadpole.persistent = true;
        add_hatchling(
            level,
            tadpole,
            Vec3::new(x, f64::from(pos.y) - 0.5, z),
            y_rot,
        );
    }
}

/// `getRandomTadpolePositionOffset`: `Mth.clamp(nextDouble(), 0.2F, 0.7999999970197678)`.
fn tadpole_offset(level: &mut Level) -> f64 {
    level
        .random()
        .next_double()
        .clamp(f64::from(0.2_f32), 0.799_999_997_019_767_8)
}
