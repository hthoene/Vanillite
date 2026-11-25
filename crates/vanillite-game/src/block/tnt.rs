//! TNT (`TntBlock`) and the primed TNT entity (`PrimedTnt`).
//!
//! A TNT block primes when it receives a signal (on placement or a neighbour update) or when an
//! explosion hits it; priming replaces it with a `tnt` entity that falls with gravity 0.04 and
//! drag 0.98 and explodes with power 4 when its fuse (80 ticks) runs out.

use std::cell::RefCell;

use vanillite_base::math::{floor, lerp};
use vanillite_base::random::{LegacyRandom, RandomSource};
use vanillite_base::{Aabb, BlockPos, Vec3};
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_data::entity::ids as entity_ids;
use vanillite_world::entity::{EntityFlags, EntityId, NewEntity};
use vanillite_worldgen::behaviour::{Class, StateExt, class_of_block};

use super::{BlockBehaviour, falling, sound_id};
use crate::entities::PrimedTntData;
use crate::level::explosion::BlockInteraction;
use crate::level::{Level, SOUND_BLOCKS};

/// `PrimedTnt.DEFAULT_FUSE_TIME`.
pub const DEFAULT_FUSE: i32 = 80;
/// `PrimedTnt.DEFAULT_EXPLOSION_POWER`.
pub const DEFAULT_POWER: f32 = 4.0;
/// `getDefaultGravity()`.
const GRAVITY: f64 = 0.04;

/// Fills the TNT behaviours of `block`.
pub(super) fn fill(block: BlockId, b: &mut BlockBehaviour) {
    if block == ids::TNT {
        b.on_place = Some(on_place);
        b.neighbor_changed = Some(neighbor_changed);
    }
}

/// `TntBlock.onPlace`: a TNT block placed next to a signal primes at once.
fn on_place(level: &mut Level, state: BlockState, pos: BlockPos, old: BlockState, _moved: bool) {
    if old.block() != state.block() && level.has_neighbor_signal(pos) && prime(level, pos, None) {
        level.remove_block(pos, false);
    }
}

/// `TntBlock.neighborChanged`.
fn neighbor_changed(
    level: &mut Level,
    _state: BlockState,
    pos: BlockPos,
    _source: BlockId,
    _moved: bool,
) {
    if level.has_neighbor_signal(pos) && prime(level, pos, None) {
        level.remove_block(pos, false);
    }
}

/// `TntBlock.prime(level, pos, source, itemStack)`: a primed TNT entity at the block with the
/// priming sound. The caller removes the block. Fire (`FireBlock`), flint and steel and burning
/// projectiles call this too.
pub fn prime(level: &mut Level, pos: BlockPos, source: Option<EntityId>) -> bool {
    let at = Vec3::new(
        f64::from(pos.x) + 0.5,
        f64::from(pos.y),
        f64::from(pos.z) + 0.5,
    );
    spawn_primed(level, at, source, None);
    level.play_sound(
        None,
        at.x,
        at.y,
        at.z,
        sound_id("entity.tnt.primed"),
        SOUND_BLOCKS,
        1.0,
        1.0,
    );
    level.game_event_at(source, crate::level::game_event::events().prime_fuse, pos);
    true
}

/// `TntBlock.wasExploded`: TNT hit by an explosion primes with a short random fuse.
pub fn was_exploded(level: &mut Level, pos: BlockPos, source: Option<EntityId>) {
    let at = Vec3::new(
        f64::from(pos.x) + 0.5,
        f64::from(pos.y),
        f64::from(pos.z) + 0.5,
    );
    spawn_primed(level, at, source, Some(short_fuse));
}

/// `PrimedTnt.getRandomShortFuse(getFuse(), level.getRandom())` of a fresh TNT entity.
fn short_fuse(level: &mut Level) -> i32 {
    let fuse = DEFAULT_FUSE;
    level.random().next_int_bounded((fuse / 4).max(1)) + fuse / 8
}

/// `new PrimedTnt(level, x, y, z, owner)` and `addFreshEntity`: the constructor draws the
/// launch direction from the level random; `fuse` then replaces the default fuse.
fn spawn_primed(
    level: &mut Level,
    at: Vec3,
    owner: Option<EntityId>,
    fuse: Option<fn(&mut Level) -> i32>,
) -> EntityId {
    // The entity takes its own random (not drawn from at creation).
    let _ = level.new_entity_random();
    let rot = level.random().next_double() * f64::from(std::f32::consts::TAU);
    let velocity = Vec3::new(-rot.sin() * 0.02, f64::from(0.2_f32), -rot.cos() * 0.02);
    let fuse = fuse.map_or(DEFAULT_FUSE, |f| f(level));
    let network_id = level.network_ids().next();
    let mut new = NewEntity::new(entity_ids::TNT, network_id, insecure_uuid(), at);
    new.velocity = velocity;
    level.entities.spawn_primed_tnt(
        new,
        PrimedTntData {
            fuse,
            state: ids::TNT.default_state(),
            owner,
            explosion_power: DEFAULT_POWER,
        },
    )
}

/// `Mth.createInsecureUUID(entity.random)`: entities draw their UUID from their own random,
/// not the level's, so spawning one does not shift the level random.
pub fn insecure_uuid() -> u128 {
    thread_local! {
        static RANDOM: RefCell<LegacyRandom> = RefCell::new(LegacyRandom::new(seed()));
    }
    fn seed() -> i64 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        nanos as i64 ^ 0x5DEE_CE66
    }
    RANDOM.with_borrow_mut(|random| {
        let most = random.next_long() as u64;
        let least = random.next_long() as u64;
        let most = (most & !0xF000) | 0x4000;
        let least = (least & !0xC000_0000_0000_0000) | 0x8000_0000_0000_0000;
        (u128::from(most) << 64) | u128::from(least)
    })
}

/// The state a piston moves for a pushed block (`PistonBaseBlock.moveBlocks` puts the state it
/// read before the move into the moving block entity). With the `tnt-duplication` rule off, TNT
/// that was primed while the structure moved (its position no longer holds TNT) moves as air,
/// so the moving piston does not bring back a copy of it.
///
/// `cached` is the state read before the move (`toPushShapes.get(i)`), `current` the state read
/// at the same position in the move loop.
pub fn piston_moved_state(level: &Level, cached: BlockState, current: BlockState) -> BlockState {
    if level.settings.exploits.tnt_duplication || !cached.is(ids::TNT) || current.is(ids::TNT) {
        cached
    } else {
        BlockState::AIR
    }
}

/// `PrimedTnt.tick`.
pub fn tick_entity(level: &mut Level, id: EntityId) {
    let Some(data) = level.entities.primed_tnt(id).copied() else {
        return;
    };
    let Some(entity) = level.entities.store.get(id) else {
        return;
    };
    let (pos, aabb, flags) = (entity.pos(), entity.aabb(), entity.flags());
    let mut velocity = entity.velocity();
    // `applyGravity`.
    let gravity = if flags.contains(EntityFlags::NO_GRAVITY) {
        0.0
    } else {
        GRAVITY
    };
    velocity.y -= gravity;
    let moved = move_self(level, id, pos, aabb, velocity, gravity);
    let Some(entity) = level.entities.store.get(id) else {
        return;
    };
    let (new_pos, new_aabb) = (entity.pos(), entity.aabb());
    // `applyEffectsFromBlocks`.
    apply_effects_from_blocks(level, id, new_aabb);
    let Some(entity) = level.entities.store.get(id) else {
        return;
    };
    let on_ground = entity.flags().contains(EntityFlags::ON_GROUND);
    // `getAirDrag()`, then the ground friction.
    let drag = f64::from(0.98_f32);
    let mut velocity = entity.velocity() * drag;
    if on_ground {
        velocity = velocity.multiply(0.7, -0.5, 0.7);
    }
    let fuse = data.fuse - 1;
    if let Some(d) = level.entities.primed_tnt_mut(id) {
        d.fuse = fuse;
    }
    if let Some(mut entity) = level.entities.store.get_mut(id) {
        entity.set_velocity(velocity);
        // `setFuse` changes synched data every tick.
        entity.mark_data_dirty();
        entity.set_flags(
            EntityFlags::PRECISE_POSITION,
            moved.horizontal || moved.vertical,
        );
    }
    if fuse <= 0 {
        level.entities.despawn(id);
        explode(level, id, new_pos, data);
    }
}

/// `PrimedTnt.explode`: power 4 at a sixteenth of its height, breaking blocks like TNT.
fn explode(level: &mut Level, id: EntityId, pos: Vec3, data: PrimedTntData) {
    let height = f64::from(entity_ids::TNT.info().height);
    let center = Vec3::new(pos.x, pos.y + height * 0.0625, pos.z);
    level.explode(
        Some(id),
        data.owner,
        center,
        data.explosion_power,
        BlockInteraction::Destroy,
    );
}

/// The collisions of one move.
#[derive(Clone, Copy, Debug)]
struct Moved {
    horizontal: bool,
    vertical: bool,
}

/// `Entity.move(MoverType.SELF, delta)` for an entity without step-up, movement emission or
/// fall damage: the collision, the ground flag, the bounce of beds and slime blocks
/// (`restituteMovementAfterCollisions`) and the block speed factor.
fn move_self(
    level: &mut Level,
    id: EntityId,
    pos: Vec3,
    aabb: Aabb,
    delta: Vec3,
    gravity: f64,
) -> Moved {
    let movement = falling::collide(level, aabb, delta);
    let movement_length = movement.length_sqr();
    let mut new_pos = pos;
    if movement_length > 1.0e-7 || delta.length_sqr() - movement_length < 1.0e-7 {
        new_pos = pos + movement;
    }
    let x_collision = !mth_equal(delta.x, movement.x);
    let z_collision = !mth_equal(delta.z, movement.z);
    let horizontal = x_collision || z_collision;
    let moved_vertically = delta.y.abs() > 0.0;
    let vertical = moved_vertically && delta.y != movement.y;
    let below = vertical && delta.y < 0.0;
    // `getOnPosLegacy()`.
    let effect_pos = BlockPos::new(
        floor(new_pos.x),
        floor(new_pos.y - f64::from(0.2_f32)),
        floor(new_pos.z),
    );
    let effect_state = level.block(effect_pos);
    let mut velocity = delta;
    if (moved_vertically && vertical) || horizontal {
        // `restituteMovementAfterCollisions` with the entity's bounciness of 0.
        let current = delta;
        if x_collision {
            velocity.x = -current.x * 0.0;
        }
        if z_collision {
            velocity.z = -current.z * 0.0;
        }
        if vertical {
            let mut restitution = 0.0;
            if below {
                restitution = if -current.y > gravity
                    && !super::block_tags().suppresses_bounce.contains(effect_state)
                {
                    block_bounciness(effect_state.block())
                } else {
                    0.0
                };
            }
            velocity.y = if restitution > 0.0 {
                let portion = movement.y / current.y;
                let compensation = portion * gravity;
                let drag = lerp(portion, 1.0, f64::from(0.98_f32));
                (compensation - current.y) * drag * restitution
            } else {
                (0.0 - current.y) * 1.0 * restitution
            };
        }
    }
    // `getBlockSpeedFactor()`.
    let here = level.block(BlockPos::containing(new_pos.x, new_pos.y, new_pos.z));
    let mut speed = here.block().info().speed_factor;
    if speed == 1.0 && !here.is(ids::WATER) && !here.is(ids::BUBBLE_COLUMN) {
        let below_pos = BlockPos::new(
            floor(new_pos.x),
            floor(new_pos.y - f64::from(0.500_001_f32)),
            floor(new_pos.z),
        );
        speed = level.block(below_pos).block().info().speed_factor;
    }
    let speed = f64::from(speed);
    velocity = velocity.multiply(speed, 1.0, speed);
    if let Some(mut entity) = level.entities.store.get_mut(id) {
        entity.set_pos(new_pos);
        if moved_vertically {
            entity.set_flags(EntityFlags::ON_GROUND, below);
        }
        entity.set_velocity(velocity);
    }
    Moved {
        horizontal,
        vertical,
    }
}

/// `getBlockBounciness(block)` of a non-living entity: the block's bounce restitution times
/// 0.8 (slime blocks 1.0, beds and shelf mushrooms 0.75).
fn block_bounciness(block: BlockId) -> f64 {
    let restitution: f32 = if block == ids::SLIME_BLOCK {
        1.0
    } else if block == ids::SHELF_MUSHROOM
        || (class_of_block(block) == Class::AbstractBed && block != ids::STRAW_BED)
    {
        0.75
    } else {
        0.0
    };
    f64::from(restitution * 0.8)
}

/// `Mth.equal(a, b)`: `|b - a| < 1.0E-5F`.
fn mth_equal(a: f64, b: f64) -> bool {
    (b - a).abs() < f64::from(1.0e-5_f32)
}

/// `applyEffectsFromBlocks` for an entity that stands still in its block: `entityInside` of
/// every non-air block its box (shrunk by 1.0E-5) touches, X fastest, then Y, then Z.
fn apply_effects_from_blocks(level: &mut Level, id: EntityId, aabb: Aabb) {
    let e = 1.0e-5;
    let min = BlockPos::containing(aabb.min_x + e, aabb.min_y + e, aabb.min_z + e);
    let max = BlockPos::containing(aabb.max_x - e, aabb.max_y - e, aabb.max_z - e);
    for z in min.z..=max.z {
        for y in min.y..=max.y {
            for x in min.x..=max.x {
                let pos = BlockPos::new(x, y, z);
                if !level.entities.store.contains(id) {
                    return;
                }
                if !level.block(pos).is_air() {
                    level.entity_inside(pos, id);
                }
            }
        }
    }
}
