//! Falling blocks (`FallingBlock`, `ConcretePowderBlock`, `AnvilBlock`, `BrushableBlock`) and
//! the `falling_block` entity (`FallingBlockEntity`).
//!
//! A falling block schedules a tick when it is placed or a neighbour changes; the tick turns it
//! into a `falling_block` entity if the block below is free. The entity falls with gravity
//! 0.04 and drag 0.98 and becomes a block again where it lands, or drops as an item where it
//! cannot.

use vanillite_base::{Aabb, BlockPos, Vec3};
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_data::entity::ids as entity_ids;
use vanillite_world::entity::{EntityFlags, EntityId, NewEntity};
use vanillite_worldgen::behaviour::{self as shared, Class, class_of};
use vanillite_worldgen::behaviour::{PosExt, StateExt};

use super::{can_be_replaced, drops, fluid, is_fire};
use crate::entities::FallingBlockData;
use crate::level::Level;

/// `LevelEvent.SOUND_ANVIL_LAND`.
const ANVIL_LAND: i32 = 1031;
/// `LevelEvent.SOUND_ANVIL_BROKEN`.
const ANVIL_BROKEN: i32 = 1029;

/// `getDelayAfterPlace()`.
fn delay_after_place(state: BlockState) -> i32 {
    if state.is(ids::DRAGON_EGG) { 5 } else { 2 }
}

/// `FallingBlock.onPlace`.
pub fn on_place(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    _old: BlockState,
    _moved: bool,
) {
    level.schedule_block_tick(pos, state.block(), delay_after_place(state));
}

/// `FallingBlock.tick`: falls if the block below is free.
pub fn tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    if is_free(level.block(pos.below())) && pos.y >= level.min_y() {
        fall(level, pos, state, true);
    }
}

/// `BrushableBlock.onPlace`.
pub fn brushable_on_place(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    _old: BlockState,
    _moved: bool,
) {
    level.schedule_block_tick(pos, state.block(), 2);
}

/// `BrushableBlock.tick`: falls without dropping an item.
pub fn brushable_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    if is_free(level.block(pos.below())) && pos.y >= level.min_y() {
        fall(level, pos, state, false);
    }
}

/// `FallingBlock.isFree(state)`.
pub fn is_free(state: BlockState) -> bool {
    state.is_air()
        || is_fire(state)
        || state.has(vanillite_data::block::flags::LIQUID)
        || can_be_replaced(state)
}

/// `FallingBlockEntity.fall(level, pos, state)`: the block becomes its fluid and an entity
/// takes its place.
pub fn fall(level: &mut Level, pos: BlockPos, state: BlockState, drop_item: bool) -> EntityId {
    let falling = state.with("waterlogged", "false").unwrap_or(state);
    let below = state.fluid_state().block_state();
    let spawn_pos = Vec3::new(
        f64::from(pos.x) + 0.5,
        f64::from(pos.y),
        f64::from(pos.z) + 0.5,
    );
    level.set_block_and_update(pos, below);
    let uuid = level.new_uuid();
    let network_id = level.network_ids().next();
    let new = NewEntity::new(entity_ids::FALLING_BLOCK, network_id, uuid, spawn_pos);
    level.entities.spawn_falling_block(
        new,
        FallingBlockData {
            state: falling,
            time: 0,
            drop_item,
            start: pos,
        },
    )
}

/// `FallingBlockEntity.tick`.
pub fn tick_entity(level: &mut Level, id: EntityId) {
    let Some(data) = level.entities.falling_block(id).copied() else {
        return;
    };
    if data.state.is_air() {
        level.entities.despawn(id);
        return;
    }
    let time = data.time + 1;
    if let Some(d) = level.entities.falling_block_mut(id) {
        d.time = time;
    }
    let Some(entity) = level.entities.store.get(id) else {
        return;
    };
    let (pos, aabb) = (entity.pos(), entity.aabb());
    // `applyGravity` and `move(SELF, deltaMovement)`.
    let mut velocity = entity.velocity();
    velocity.y -= 0.04;
    let moved = collide(level, aabb, velocity);
    let vertical = moved.y != velocity.y;
    let horizontal = moved.x != velocity.x || moved.z != velocity.z;
    let on_ground = vertical && velocity.y < 0.0;
    let new_pos = Vec3::new(pos.x + moved.x, pos.y + moved.y, pos.z + moved.z);
    if moved.x != velocity.x {
        velocity.x = 0.0;
    }
    if moved.z != velocity.z {
        velocity.z = 0.0;
    }
    if vertical {
        // `Block.updateEntityMovementAfterFallOn`.
        velocity.y = 0.0;
    }
    if let Some(mut entity) = level.entities.store.get_mut(id) {
        entity.set_pos(new_pos);
        entity.set_flags(EntityFlags::ON_GROUND, on_ground);
    }
    let block_pos = BlockPos::containing(new_pos.x, new_pos.y, new_pos.z);
    let block = data.state.block();
    let concrete = class_of(data.state) == Class::ConcretePowder;
    // `isStuckInWater`; Vanilla also clips fast concrete powder against water it passed.
    let stuck_in_water = concrete && fluid::is_water(level.fluid(block_pos));
    let mut removed = false;
    if !on_ground && !stuck_in_water {
        if time > 100 && (block_pos.y <= level.min_y() || block_pos.y > level.max_y()) || time > 600
        {
            if data.drop_item && level.settings.entity_drops {
                drops::spawn_block_item(level, new_pos, block);
            }
            level.entities.despawn(id);
            removed = true;
        }
    } else {
        let current = level.block(block_pos);
        velocity = Vec3::new(velocity.x * 0.7, velocity.y * -0.5, velocity.z * 0.7);
        if !current.is(ids::MOVING_PISTON) {
            let may_replace = can_be_replaced_by_falling(current);
            let would_continue =
                is_free(level.block(block_pos.below())) && (!concrete || !stuck_in_water);
            let would_survive = level.can_survive(data.state, block_pos) && !would_continue;
            level.entities.despawn(id);
            removed = true;
            if may_replace && would_survive {
                let mut state = data.state;
                if level.fluid(block_pos).info().fluid == fluid::WATER
                    && let Some(waterlogged) = state.with("waterlogged", "true")
                {
                    state = waterlogged;
                }
                if level.set_block_and_update(block_pos, state) {
                    on_land(level, block_pos, state, current);
                } else if data.drop_item && level.settings.entity_drops {
                    on_broken_after_fall(level, block, block_pos);
                    drops::spawn_block_item(level, new_pos, block);
                }
            } else if data.drop_item && level.settings.entity_drops {
                on_broken_after_fall(level, block, block_pos);
                drops::spawn_block_item(level, new_pos, block);
            }
        } else {
            level.entities.despawn(id);
            removed = true;
            on_broken_after_fall(level, block, block_pos);
        }
    }
    if !removed && let Some(mut entity) = level.entities.store.get_mut(id) {
        // `getAirDrag()`.
        entity.set_velocity(Vec3::new(
            velocity.x * 0.98,
            velocity.y * 0.98,
            velocity.z * 0.98,
        ));
        entity.set_flags(EntityFlags::PRECISE_POSITION, horizontal || vertical);
    }
}

/// `currentState.canBeReplaced(new DirectionalPlaceContext(..., ItemStack.EMPTY, ...))`.
fn can_be_replaced_by_falling(state: BlockState) -> bool {
    if state.is(ids::SNOW) {
        // `SnowLayerBlock.canBeReplaced`: without the snow item only a single layer.
        return state.value("layers") == Some("1");
    }
    can_be_replaced(state)
}

/// `Fallable.onLand`.
fn on_land(level: &mut Level, pos: BlockPos, state: BlockState, replaced: BlockState) {
    match class_of(state) {
        Class::Anvil => level.level_event(None, ANVIL_LAND, pos, 0),
        Class::ConcretePowder => {
            // `shouldSolidify`.
            if (fluid::is_water(replaced.fluid_state()) || shared::touches_liquid(level, pos))
                && let Some(concrete) = shared::concrete_of(state)
            {
                level.set_block_and_update(pos, concrete.default_state());
            }
        }
        _ => {}
    }
}

/// `Fallable.onBrokenAfterFall`.
fn on_broken_after_fall(level: &mut Level, block: BlockId, pos: BlockPos) {
    if class_of(block.default_state()) == Class::Anvil {
        level.level_event(None, ANVIL_BROKEN, pos, 0);
    }
}

/// `Entity.collide` against block collision shapes: the movement left after the boxes in the
/// way, Y first, then X and Z (larger first).
pub(crate) fn collide(level: &Level, aabb: Aabb, movement: Vec3) -> Vec3 {
    let mut aabb = aabb;
    let dy = collide_axis(level, &aabb, 1, movement.y);
    aabb = aabb.offset(0.0, dy, 0.0);
    let (first, second) = if movement.x.abs() < movement.z.abs() {
        (2, 0)
    } else {
        (0, 2)
    };
    let mut out = [0.0, dy, 0.0];
    for axis in [first, second] {
        let delta = collide_axis(level, &aabb, axis, [movement.x, 0.0, movement.z][axis]);
        out[axis] = delta;
        aabb = match axis {
            0 => aabb.offset(delta, 0.0, 0.0),
            _ => aabb.offset(0.0, 0.0, delta),
        };
    }
    Vec3::new(out[0], out[1], out[2])
}

/// The movement along one axis (0 = X, 1 = Y, 2 = Z) until `aabb` touches a collision box
/// (`Shapes.collide` with its 1.0E-7 tolerance).
fn collide_axis(level: &Level, aabb: &Aabb, axis: usize, delta: f64) -> f64 {
    const EPSILON: f64 = 1.0e-7;
    if delta.abs() < EPSILON {
        return 0.0;
    }
    let min = [aabb.min_x, aabb.min_y, aabb.min_z];
    let max = [aabb.max_x, aabb.max_y, aabb.max_z];
    let mut lo = min;
    let mut hi = max;
    if delta > 0.0 {
        hi[axis] += delta;
    } else {
        lo[axis] += delta;
    }
    // One block of margin below for shapes taller than a block (fences, walls).
    let start = BlockPos::containing(lo[0] - EPSILON, lo[1] - 1.0, lo[2] - EPSILON);
    let end = BlockPos::containing(hi[0] + EPSILON, hi[1] + EPSILON, hi[2] + EPSILON);
    let mut delta = delta;
    for y in start.y..=end.y {
        for z in start.z..=end.z {
            for x in start.x..=end.x {
                let pos = BlockPos::new(x, y, z);
                let state = level.block(pos);
                let origin = [f64::from(x), f64::from(y), f64::from(z)];
                for b in state.collision_shape() {
                    let box_min = [b[0] + origin[0], b[1] + origin[1], b[2] + origin[2]];
                    let box_max = [b[3] + origin[0], b[4] + origin[1], b[5] + origin[2]];
                    let overlaps = (0..3)
                        .filter(|&a| a != axis)
                        .all(|a| box_max[a] - EPSILON > min[a] && box_min[a] + EPSILON < max[a]);
                    if !overlaps {
                        continue;
                    }
                    if delta > 0.0 && box_min[axis] >= max[axis] - EPSILON {
                        delta = delta.min(box_min[axis] - max[axis]);
                    } else if delta < 0.0 && box_max[axis] <= min[axis] + EPSILON {
                        delta = delta.max(box_max[axis] - min[axis]);
                    }
                }
            }
        }
    }
    delta
}
