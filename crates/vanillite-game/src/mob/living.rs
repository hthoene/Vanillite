//! The living entity tick of a mob in Vanilla's order: `Entity.baseTick`,
//! `LivingEntity.baseTick`, `Mob.baseTick`, `LivingEntity.aiStep` with `Mob.serverAiStep`,
//! `travel` and `Entity.move`, then the body rotation.

use vanillite_base::math::floor;
use vanillite_base::random::RandomSource;
use vanillite_base::{Aabb, BlockPos, Vec3};
use vanillite_data::block::BlockState;
use vanillite_protocol::SharedPacket;
use vanillite_protocol::packets::play::clientbound::EntityEvent;
use vanillite_world::entity::{EntityFlags, EntityId};

use super::physics::{self, Blocks};
use super::{Tick, combat, goals, kinds, nav};
use crate::survival::Effect;
use crate::survival::blocks::{Landing, fluid_contact, inside_blocks, is_in_wall};
use crate::survival::damage::DamageSource;
use crate::survival::rules::Rule;

/// `Entity.EntityEvent` ids used by mobs.
pub mod event {
    pub const DEATH: i8 = 3;
    pub const EAT_GRASS: i8 = 10;
    pub const POOF: i8 = 60;
    pub const DROWN: i8 = 67;
    pub const SPAWN_POOF: i8 = 20;
}

/// Broadcasts an entity event of the ticking mob to the players tracking it.
pub fn entity_event(t: &mut Tick<'_, '_>, id: EntityId, event: i8) {
    let Some(e) = t.ents.store.get(id) else {
        return;
    };
    let network_id = e.network_id().0;
    t.env.out.push(Effect::Tracking {
        entity: id,
        with_self: false,
        packet: SharedPacket::of(&EntityEvent {
            entity_id: network_id,
            event,
        }),
    });
}

/// `Mob.tick` (with the kind specific parts before and after `LivingEntity.tick`).
pub fn tick(t: &mut Tick<'_, '_>) {
    kinds::before_tick(t);
    if t.mob_ref().removed {
        return;
    }
    base_tick(t);
    if t.mob_ref().removed {
        return;
    }
    ai_step(t);
    if t.mob_ref().removed || !t.exists() {
        return;
    }
    // `tickHeadTurn` → `BodyRotationControl.clientTick`.
    nav::tick_body_rotation(t);
    super::tame::tick_leash(t);
    if !t.exists() {
        return;
    }
    kinds::after_tick(t);
}

/// `Entity.baseTick` + `LivingEntity.baseTick` + `Mob.baseTick`.
fn base_tick(t: &mut Tick<'_, '_>) {
    update_fluids(t);
    let fire_immune = t.core().kind().info().fire_immune;
    let in_lava = t.mob_ref().fluids.in_lava();
    if t.mob_ref().fire_ticks > 0 {
        if fire_immune {
            t.mob().fire_ticks = 0;
        } else {
            if t.mob_ref().fire_ticks % 20 == 0 && !in_lava {
                let source = DamageSource::of(t.env.types.on_fire);
                hurt_self(t, source, 1.0);
            }
            t.mob().fire_ticks -= 1;
        }
    }
    if in_lava {
        t.mob().fall_distance *= 0.5;
    }
    if t.pos().y < f64::from(t.level.min_y() - 64) {
        let source = DamageSource::of(t.env.types.out_of_world);
        hurt_self(t, source, 4.0);
    }
    let on_fire = t.mob_ref().fire_ticks > 0;
    t.core_mut().set_flags(EntityFlags::ON_FIRE, on_fire);
    if !t.exists() {
        return;
    }
    // `LivingEntity.baseTick`.
    if t.mob_ref().is_alive() {
        let eye = Vec3::new(t.pos().x, t.eye_y(), t.pos().z);
        let width = t.core().dims().width;
        if is_in_wall(t.env.blocks, t.env.facts, eye, width) {
            let source = DamageSource::of(t.env.types.in_wall);
            hurt_self(t, source, 1.0);
        }
        if !t.exists() {
            return;
        }
        let eye_in_water = t.mob_ref().fluids.eye_in_water;
        let bubble = {
            let p = t.pos();
            t.env
                .facts
                .is_bubble_column(t.level.block(BlockPos::containing(p.x, t.eye_y(), p.z)))
        };
        if eye_in_water && !bubble {
            if !t.mob_ref().kind.breathes_underwater() {
                t.mob().air -= 1;
                if t.mob_ref().air <= -20 {
                    t.mob().air = 0;
                    let id = t.id;
                    entity_event(t, id, event::DROWN);
                    let source = DamageSource::of(t.env.types.drown);
                    hurt_self(t, source, 2.0);
                }
            }
        } else if t.mob_ref().air < 300 {
            let m = t.mob();
            m.air = (m.air + 4).min(300);
        }
        if !t.exists() {
            return;
        }
        let p = t.block_pos();
        t.mob().last_pos = p;
    }
    {
        let m = t.mob();
        if m.hurt_time > 0 {
            m.hurt_time -= 1;
        }
        if m.damage_cooldown > 0 {
            m.damage_cooldown -= 1;
        }
    }
    if t.mob_ref().is_dead_or_dying() {
        tick_death(t);
        if t.mob_ref().removed {
            return;
        }
    }
    {
        let m = t.mob();
        if m.last_hurt_by_player_time > 0 {
            m.last_hurt_by_player_time -= 1;
        } else {
            m.last_hurt_by_player = None;
        }
    }
    if let Some(mob) = t.mob_ref().last_hurt_mob
        && t.other(mob).is_none()
    {
        t.mob().last_hurt_mob = None;
    }
    if let Some(by) = t.mob_ref().last_hurt_by_mob {
        let expired = t.mob_ref().tick_count - t.mob_ref().last_hurt_by_mob_time > 100;
        if t.other(by).is_none() || expired {
            t.mob().last_hurt_by_mob = None;
        }
    }
    // `Mob.baseTick`: the ambient sound.
    if t.mob_ref().is_alive() {
        let m = t.mob();
        let roll = m.random.next_int_bounded(1000);
        let time = m.ambient_sound_time;
        m.ambient_sound_time += 1;
        if roll < time {
            m.ambient_sound_time = -m.kind.ambient_sound_interval();
            kinds::play_ambient_sound(t);
        }
    }
}

/// `LivingEntity.tickDeath`.
fn tick_death(t: &mut Tick<'_, '_>) {
    t.mob().death_time += 1;
    if t.mob_ref().death_time >= 20 {
        let id = t.id;
        entity_event(t, id, event::POOF);
        t.mob().removed = true;
    }
}

/// Hurts the ticking mob itself (environment damage).
fn hurt_self(t: &mut Tick<'_, '_>, source: DamageSource, amount: f32) -> bool {
    let id = t.id;
    combat::hurt(t.level, t.ents, t.env, t.cx, id, source, amount)
}

/// `updateFluidInteraction` for water and lava (heights and whether the eyes are inside;
/// the push by flowing fluids is not simulated).
fn update_fluids(t: &mut Tick<'_, '_>) {
    let p = t.pos();
    let eye = Vec3::new(p.x, t.eye_y(), p.z);
    let contact = fluid_contact(t.env.blocks, t.env.facts, t.aabb(), eye);
    let m = t.mob();
    m.fluids = contact;
    if contact.in_water() {
        // `updateInWaterStateAndDoWaterCurrentPushing`: water ends the fall and the fire.
        m.fall_distance = 0.0;
        if m.fire_ticks > 0 {
            m.fire_ticks = 0;
        }
    }
}

/// `getFluidJumpThreshold`: 0.4 blocks, or the eye height for small mobs.
fn fluid_jump_threshold(t: &Tick<'_, '_>) -> f64 {
    let eye = super::eye_height(t.mob_ref());
    if eye < 0.4 { 0.0 } else { 0.4 }
}

/// `LivingEntity.aiStep` with `Mob.aiStep`'s additions.
fn ai_step(t: &mut Tick<'_, '_>) {
    {
        let m = t.mob();
        if m.no_jump_delay > 0 {
            m.no_jump_delay -= 1;
        }
    }
    let mut v = t.velocity();
    if v.x.abs() < 0.003 {
        v.x = 0.0;
    }
    if v.z.abs() < 0.003 {
        v.z = 0.0;
    }
    if v.y.abs() < 0.003 {
        v.y = 0.0;
    }
    t.set_velocity(v);
    {
        let m = t.mob();
        m.xxa *= 0.98;
        m.zza *= 0.98;
    }
    let effective_ai = !t.mob_ref().no_ai;
    if t.mob_ref().is_dead_or_dying() {
        let m = t.mob();
        m.jumping = false;
        m.xxa = 0.0;
        m.zza = 0.0;
    } else if effective_ai {
        server_ai_step(t);
        if !t.exists() || t.mob_ref().removed {
            return;
        }
    }
    // Jumping.
    if t.mob_ref().jumping {
        let fluids = t.mob_ref().fluids;
        let height = if fluids.in_lava() {
            fluids.lava_height
        } else {
            fluids.water_height
        };
        let in_water_with_height = fluids.in_water() && height > 0.0;
        let threshold = fluid_jump_threshold(t);
        let on_ground = t.on_ground();
        if !in_water_with_height || on_ground && height <= threshold {
            if !fluids.in_lava() || on_ground && fluids.lava_height <= threshold {
                if (on_ground || in_water_with_height && height <= threshold)
                    && t.mob_ref().no_jump_delay == 0
                {
                    jump_from_ground(t);
                    t.mob().no_jump_delay = 10;
                }
            } else {
                jump_in_liquid(t);
            }
        } else {
            jump_in_liquid(t);
        }
    } else {
        t.mob().no_jump_delay = 0;
    }
    // A mob its rider steers moves on the rider's client (`isControlledByLocalInstance`).
    let steered = t
        .ents
        .riding
        .passengers_of(t.id)
        .first()
        .is_some_and(|&p| crate::riding::controls(t.ents, p, t.id));
    if effective_ai && !steered {
        let m = t.mob_ref();
        let input = Vec3::new(f64::from(m.xxa), f64::from(m.yya), f64::from(m.zza));
        travel(t, input);
    }
    apply_effects_from_blocks(t);
    if !t.exists() {
        return;
    }
    push_entities(t);
    if !t.exists() {
        return;
    }
    if t.mob_ref().kind.sensitive_to_water() {
        let p = t.pos();
        let rain = (t.env.raining_at)(BlockPos::containing(p.x, p.y, p.z))
            || (t.env.raining_at)(BlockPos::containing(p.x, t.aabb().max_y, p.z));
        if t.mob_ref().fluids.in_water() || rain {
            let source = DamageSource::of(t.env.types.drown);
            hurt_self(t, source, 1.0);
        }
    }
    if !t.exists() {
        return;
    }
    if t.mob_ref().kind.burns_in_daylight() && is_sun_burn_tick(t) {
        // `igniteForSeconds(8)`.
        let m = t.mob();
        m.fire_ticks = m.fire_ticks.max(160);
    }
    kinds::ai_step(t);
}

/// `Mob.jumpInLiquid`.
fn jump_in_liquid(t: &mut Tick<'_, '_>) {
    let dy = if t.mob_ref().nav.can_float {
        f64::from(0.04_f32)
    } else {
        0.3
    };
    let v = t.velocity();
    t.set_velocity(Vec3::new(v.x, v.y + dy, v.z));
}

/// `jumpFromGround`.
fn jump_from_ground(t: &mut Tick<'_, '_>) {
    #[allow(clippy::cast_possible_truncation)]
    let power = t.mob_ref().attrs.jump_strength as f32 * block_jump_factor(t);
    if power <= 1.0e-5 {
        return;
    }
    let v = t.velocity();
    t.set_velocity(Vec3::new(v.x, f64::from(power).max(v.y), v.z));
    t.core_mut().set_flags(EntityFlags::HAS_IMPULSE, true);
}

fn block_jump_factor(t: &Tick<'_, '_>) -> f32 {
    let here = t.level.block(t.block_pos()).block().info().jump_factor;
    let below = t
        .level
        .block(pos_below_affecting_movement(t))
        .block()
        .info()
        .jump_factor;
    if here == 1.0 { below } else { here }
}

fn block_speed_factor(t: &Tick<'_, '_>) -> f32 {
    let state = t.level.block(t.block_pos());
    let here = state.block().info().speed_factor;
    let water = super::is_water(state) && state.block().name() == "minecraft:water";
    let factor = if water || t.env.facts.is_bubble_column(state) || here != 1.0 {
        here
    } else {
        t.level
            .block(pos_below_affecting_movement(t))
            .block()
            .info()
            .speed_factor
    };
    #[allow(clippy::cast_possible_truncation)]
    let efficiency = t.mob_ref().attrs.movement_efficiency as f32;
    factor + efficiency * (1.0 - factor)
}

/// `getOnPos(offset)` without the supporting block search.
fn on_pos(t: &Tick<'_, '_>, offset: f64) -> BlockPos {
    let p = t.pos();
    BlockPos::new(floor(p.x), floor(p.y - offset), floor(p.z))
}

/// `getBlockPosBelowThatAffectsMyMovement`.
fn pos_below_affecting_movement(t: &Tick<'_, '_>) -> BlockPos {
    on_pos(t, f64::from(0.500_001_f32))
}

/// `computeModifiedFriction`.
fn modified_friction(friction: f32, modifier: f32) -> f32 {
    (1.0 - (1.0 - friction) * modifier).clamp(0.0, 1.0)
}

/// `moveRelative(speed, input)`.
fn move_relative(t: &mut Tick<'_, '_>, speed: f32, input: Vec3) {
    let length = input.length_sqr();
    if length < 1.0e-7 {
        return;
    }
    let movement = if length > 1.0 {
        input.normalize()
    } else {
        input
    } * f64::from(speed);
    let rad = f64::from(t.core().rot().y_rot * super::DEG_TO_RAD);
    let sin = f64::from(vanillite_base::math::sin(rad));
    let cos = f64::from(vanillite_base::math::cos(rad));
    let delta = Vec3::new(
        movement.x * cos - movement.z * sin,
        movement.y,
        movement.z * cos + movement.x * sin,
    );
    let v = t.velocity();
    t.set_velocity(v + delta);
}

/// `onClimbable`: spiders climbing walls, or a `#climbable` block at the feet.
pub fn on_climbable(t: &Tick<'_, '_>) -> bool {
    if t.mob_ref().kind == super::Kind::Spider {
        return t.mob_ref().climbing;
    }
    let state = t.level.block(t.block_pos());
    t.env.facts.is_climbable(state.block())
}

/// `LivingEntity.travel`.
fn travel(t: &mut Tick<'_, '_>, input: Vec3) {
    let fluids = t.mob_ref().fluids;
    if fluids.in_water() || fluids.in_lava() {
        travel_in_fluid(t, input);
    } else {
        travel_in_air(t, input);
    }
}

fn gravity(t: &Tick<'_, '_>) -> f64 {
    if t.core().flags().contains(EntityFlags::NO_GRAVITY) {
        0.0
    } else {
        t.mob_ref().attrs.gravity
    }
}

fn travel_in_air(t: &mut Tick<'_, '_>, input: Vec3) {
    let below = pos_below_affecting_movement(t);
    #[allow(clippy::cast_possible_truncation)]
    let friction_modifier = t.mob_ref().attrs.friction_modifier as f32;
    let block_friction = if t.on_ground() {
        modified_friction(
            t.level.block(below).block().info().friction,
            friction_modifier,
        )
    } else {
        1.0
    };
    // `handleRelativeFrictionAndCalculateMovement`.
    let speed = if t.on_ground() {
        let s = t.mob_ref().speed;
        if block_friction > 0.6 {
            s * (0.216_000_02 / (block_friction * block_friction * block_friction))
        } else {
            s
        }
    } else {
        0.02
    };
    move_relative(t, speed, input);
    let climbable = on_climbable(t);
    if climbable {
        t.mob().fall_distance = 0.0;
        let v = t.velocity();
        let max = f64::from(0.15_f32);
        t.set_velocity(Vec3::new(
            v.x.clamp(-max, max),
            v.y.max(-max),
            v.z.clamp(-max, max),
        ));
    }
    let v = t.velocity();
    move_self(t, v);
    if !t.exists() {
        return;
    }
    let mut movement = t.velocity();
    if (t.mob_ref().horizontal_collision || t.mob_ref().jumping) && climbable {
        movement.y = 0.2;
    }
    let mut y = movement.y;
    if t.level.is_loaded(below) {
        y -= gravity(t);
    } else if t.pos().y > f64::from(t.level.min_y()) {
        y = -0.1;
    } else {
        y = 0.0;
    }
    #[allow(clippy::cast_possible_truncation)]
    let drag_modifier = t.mob_ref().attrs.air_drag_modifier as f32;
    let air_drag = modified_friction(0.91, drag_modifier);
    let friction = block_friction * air_drag;
    let vertical = modified_friction(0.98, drag_modifier);
    t.set_velocity(Vec3::new(
        movement.x * f64::from(friction),
        y * f64::from(vertical),
        movement.z * f64::from(friction),
    ));
}

fn travel_in_fluid(t: &mut Tick<'_, '_>, input: Vec3) {
    let falling = t.velocity().y <= 0.0;
    let old_y = t.pos().y;
    let base_gravity = gravity(t);
    if t.mob_ref().fluids.in_water() {
        let mut slow_down = 0.8_f32;
        let mut speed = 0.02_f32;
        #[allow(clippy::cast_possible_truncation)]
        let mut walker = t.mob_ref().attrs.water_movement_efficiency as f32;
        if !t.on_ground() {
            walker *= 0.5;
        }
        if walker > 0.0 {
            slow_down += (0.546_000_06 - slow_down) * walker;
            speed += (t.mob_ref().speed - speed) * walker;
        }
        move_relative(t, speed, input);
        let v = t.velocity();
        move_self(t, v);
        if !t.exists() {
            return;
        }
        let mut movement = t.velocity();
        if t.mob_ref().horizontal_collision && on_climbable(t) {
            movement.y = 0.2;
        }
        movement = movement.multiply(
            f64::from(slow_down),
            f64::from(0.8_f32),
            f64::from(slow_down),
        );
        t.set_velocity(fluid_falling_adjusted(base_gravity, falling, movement));
    } else {
        move_relative(t, 0.02, input);
        let v = t.velocity();
        move_self(t, v);
        if !t.exists() {
            return;
        }
        let threshold = fluid_jump_threshold(t);
        if t.mob_ref().fluids.lava_height <= threshold {
            let v = t.velocity().multiply(0.5, f64::from(0.8_f32), 0.5);
            t.set_velocity(fluid_falling_adjusted(base_gravity, falling, v));
        } else {
            let v = t.velocity() * 0.5;
            t.set_velocity(v);
        }
        if base_gravity != 0.0 {
            let v = t.velocity();
            t.set_velocity(Vec3::new(v.x, v.y - base_gravity / 4.0, v.z));
        }
    }
    // `jumpOutOfFluid`.
    let v = t.velocity();
    if t.mob_ref().horizontal_collision {
        let probe = t
            .aabb()
            .offset(v.x, v.y + f64::from(0.6_f32) - t.pos().y + old_y, v.z);
        if physics::no_collision(t.level, &probe, &mut t.cx.boxes)
            && !physics::contains_any_liquid(t.level, &probe)
        {
            t.set_velocity(Vec3::new(v.x, f64::from(0.3_f32), v.z));
        }
    }
}

/// `getFluidFallingAdjustedMovement`.
fn fluid_falling_adjusted(gravity: f64, falling: bool, movement: Vec3) -> Vec3 {
    if gravity == 0.0 {
        return movement;
    }
    let y = if falling
        && (movement.y - 0.005).abs() >= 0.003
        && (movement.y - gravity / 16.0).abs() < 0.003
    {
        -0.003
    } else {
        movement.y - gravity / 16.0
    };
    Vec3::new(movement.x, y, movement.z)
}

/// `Entity.move(MoverType.SELF, delta)`.
pub fn move_self(t: &mut Tick<'_, '_>, delta: Vec3) {
    let mut delta = delta;
    let stuck = t.mob_ref().stuck_speed;
    if stuck.length_sqr() > 1.0e-7 {
        delta = delta.multiply(stuck.x, stuck.y, stuck.z);
        let m = t.mob();
        m.stuck_speed = Vec3::ZERO;
        t.set_velocity(Vec3::ZERO);
    }
    #[allow(clippy::cast_possible_truncation)]
    let step = t.mob_ref().attrs.step_height as f32;
    let on_ground = t.on_ground();
    let aabb = t.aabb();
    let movement = physics::collide(t.level, aabb, delta, step, on_ground, &mut t.cx.boxes);
    let length = movement.length_sqr();
    if length > 1.0e-7 || delta.length_sqr() - length < 1.0e-7 {
        let p = t.pos();
        t.core_mut().set_pos(p + movement);
    }
    let x_collision = !java_equal(delta.x, movement.x);
    let z_collision = !java_equal(delta.z, movement.z);
    let horizontal = x_collision || z_collision;
    t.mob().horizontal_collision = horizontal;
    let moved_vertically = delta.y.abs() > 0.0;
    let mut vertical = t.mob_ref().vertical_collision;
    if moved_vertically {
        vertical = delta.y != movement.y;
        let below = vertical && delta.y < 0.0;
        t.mob().vertical_collision = vertical;
        t.core_mut().set_flags(EntityFlags::ON_GROUND, below);
    }
    let effect_pos = on_pos(t, f64::from(0.2_f32));
    let effect_state = t.level.block(effect_pos);
    check_fall_damage(t, movement.y, t.on_ground(), effect_state);
    if !t.exists() || t.mob_ref().removed {
        return;
    }
    if moved_vertically && vertical || horizontal {
        // `restituteMovementAfterCollisions` without bouncy blocks: the colliding axes stop.
        let mut v = t.velocity();
        if x_collision {
            v.x = -v.x * 0.0;
        }
        if z_collision {
            v.z = -v.z * 0.0;
        }
        if vertical {
            v.y = 0.0;
        }
        t.set_velocity(v);
    }
    let factor = f64::from(block_speed_factor(t));
    let v = t.velocity();
    t.set_velocity(v.multiply(factor, 1.0, factor));
}

/// `Mth.equal(double, double)`.
fn java_equal(a: f64, b: f64) -> bool {
    (b - a).abs() < 1.0e-5
}

/// `LivingEntity.checkFallDamage` → `Entity.checkFallDamage` → `Block.fallOn`.
fn check_fall_damage(t: &mut Tick<'_, '_>, ya: f64, on_ground: bool, on_state: BlockState) {
    if on_ground {
        let distance = t.mob_ref().fall_distance;
        if distance > 0.0 {
            let multiplier = match t.env.facts.landing(on_state) {
                Landing::Fall(m) => Some(m),
                Landing::Slime => None,
                Landing::Honey => Some(0.2),
                Landing::Stalagmite => Some(2.0),
            };
            if let Some(multiplier) = multiplier {
                let extra = if matches!(t.env.facts.landing(on_state), Landing::Stalagmite) {
                    2.5
                } else {
                    0.0
                };
                cause_fall_damage(t, distance + extra, multiplier);
            }
        }
        if t.exists() {
            t.mob().fall_distance = 0.0;
        }
    } else if ya < 0.0 {
        t.mob().fall_distance -= ya;
    }
}

/// `LivingEntity.causeFallDamage`.
fn cause_fall_damage(t: &mut Tick<'_, '_>, distance: f64, multiplier: f32) {
    if t.mob_ref().kind.fall_damage_immune() {
        return;
    }
    let attrs = t.mob_ref().attrs;
    let power = distance + 1.0e-6 - attrs.safe_fall_distance;
    let damage = floor(power * f64::from(multiplier) * attrs.fall_damage_multiplier);
    if damage > 0 {
        let sound = if damage > 4 {
            t.mob_ref().kind.fall_sound_big()
        } else {
            t.mob_ref().kind.fall_sound_small()
        };
        kinds::play_sound(t, sound, 1.0, 1.0);
        let source = DamageSource::of(t.env.types.fall);
        #[allow(clippy::cast_precision_loss)]
        hurt_self(t, source, damage as f32);
    }
    kinds::on_fall_damage(t, distance);
}

/// `applyEffectsFromBlocks` / `checkInsideBlocks` for fire, lava, cobwebs and berry bushes.
fn apply_effects_from_blocks(t: &mut Tick<'_, '_>) {
    let inside = inside_blocks(t.env.blocks, t.env.facts, t.aabb());
    let fire_immune = t.core().kind().info().fire_immune;
    if let Some(damage) = inside.fire
        && !fire_immune
    {
        let m = t.mob();
        if m.fire_ticks < 0 {
            m.fire_ticks += 1;
        } else if m.fire_ticks == 0 {
            m.fire_ticks = 160;
        }
        let source = DamageSource::of(t.env.types.in_fire);
        hurt_self(t, source, damage);
    }
    if !t.exists() {
        return;
    }
    if inside.lava_blocks > 0 && !fire_immune {
        // `lavaIgnite` and `lavaHurt`.
        let m = t.mob();
        m.fire_ticks = m.fire_ticks.max(300);
        let source = DamageSource::of(t.env.types.lava);
        if hurt_self(t, source, 4.0) {
            kinds::play_sound(t, "minecraft:entity.generic.burn", 0.4, 2.0);
        }
    }
    if !t.exists() {
        return;
    }
    if inside.water {
        let m = t.mob();
        if m.fire_ticks > 0 {
            m.fire_ticks = 0;
        }
    }
    // Cobwebs and sweet berry bushes slow the next move (`makeStuckInBlock`).
    let b = t.aabb().inflate(-1.0e-5, -1.0e-5, -1.0e-5);
    let (x0, y0, z0) = (floor(b.min_x), floor(b.min_y), floor(b.min_z));
    let (x1, y1, z1) = (floor(b.max_x), floor(b.max_y), floor(b.max_z));
    let mut stuck = None;
    for x in x0..=x1 {
        for y in y0..=y1 {
            for z in z0..=z1 {
                let state = t.level.block(BlockPos::new(x, y, z));
                let block = state.block();
                if block == vanillite_data::block::ids::COBWEB {
                    stuck = Some(Vec3::new(0.25, f64::from(0.05_f32), 0.25));
                } else if block == vanillite_data::block::ids::SWEET_BERRY_BUSH {
                    stuck = Some(Vec3::new(f64::from(0.8_f32), 0.75, f64::from(0.8_f32)));
                }
            }
        }
    }
    if let Some(s) = stuck {
        t.mob().fall_distance = 0.0;
        t.mob().stuck_speed = s;
    }
}

/// `pushEntities`: pushes and is pushed by the entities in its box; too many hurt
/// (`maxEntityCramming`).
fn push_entities(t: &mut Tick<'_, '_>) {
    let aabb = t.aabb();
    let id = t.id;
    let mut ids = std::mem::take(&mut t.cx.ids);
    ids.clear();
    t.ents.store.for_each_in(&aabb, false, |other| {
        if other != id {
            ids.push(other);
        }
    });
    ids.retain(|&o| {
        t.ents.mob(o).is_some_and(super::MobData::is_alive)
            || t.ents.minecart(o).is_some()
            || t.ents.player(o).is_some_and(|p| {
                !p.survival.is_dead_or_dying()
                    && p.game_mode != vanillite_config::GameMode::Spectator
            })
    });
    if ids.is_empty() {
        t.cx.ids = ids;
        return;
    }
    let max_cramming = t.env.rules.get(Rule::MAX_ENTITY_CRAMMING);
    if max_cramming > 0
        && ids.len() > usize::try_from(max_cramming - 1).unwrap_or(usize::MAX)
        && t.mob().random.next_int_bounded(4) == 0
    {
        let source = DamageSource::of(t.env.types.cramming);
        hurt_self(t, source, 6.0);
        if !t.exists() {
            t.cx.ids = ids;
            return;
        }
    }
    let climbing = on_climbable(t);
    for &other in &ids {
        if t.ents.minecart(other).is_some() {
            // `AbstractMinecart.push(this)`.
            crate::minecart::push_by(t.ents, other, id);
            continue;
        }
        let Some(o) = t.ents.store.get(other) else {
            continue;
        };
        let op = o.pos();
        let p = t.pos();
        // `other.push(this)`.
        let mut xa = p.x - op.x;
        let mut za = p.z - op.z;
        let mut dd = xa.abs().max(za.abs());
        if dd < f64::from(0.01_f32) {
            continue;
        }
        dd = dd.sqrt();
        xa /= dd;
        za /= dd;
        let pow = (1.0 / dd).min(1.0);
        xa *= pow * f64::from(0.05_f32);
        za *= pow * f64::from(0.05_f32);
        if t.ents.mob(other).is_some()
            && let Some(mut e) = t.ents.store.get_mut(other)
        {
            let v = e.as_ref().velocity();
            e.set_velocity(Vec3::new(v.x - xa, v.y, v.z - za));
            e.set_flags(EntityFlags::HAS_IMPULSE, true);
        }
        if !climbing {
            let v = t.velocity();
            t.set_velocity(Vec3::new(v.x + xa, v.y, v.z + za));
            t.core_mut().set_flags(EntityFlags::HAS_IMPULSE, true);
        }
    }
    t.cx.ids = ids;
}

/// `Mob.serverAiStep`.
fn server_ai_step(t: &mut Tick<'_, '_>) {
    {
        let m = t.mob();
        m.no_action_time += 1;
        m.seen.clear();
        m.unseen.clear();
    }
    let id_based = t.mob_ref().tick_count + t.core().network_id().0;
    let full = id_based % 2 == 0 || t.mob_ref().tick_count <= 1;
    if full {
        goals::tick_selector(t, goals::Which::Targets);
        goals::tick_selector(t, goals::Which::Goals);
    } else {
        goals::tick_running(t, goals::Which::Targets, false);
        goals::tick_running(t, goals::Which::Goals, false);
    }
    if !t.exists() || t.mob_ref().removed {
        return;
    }
    nav::tick(t);
    kinds::custom_server_ai_step(t);
    if !t.exists() || t.mob_ref().removed {
        return;
    }
    nav::tick_move_control(t);
    nav::tick_look_control(t);
    let m = t.mob();
    m.jumping = m.jump;
    m.jump = false;
}

/// `Mob.isSunBurnTick`.
fn is_sun_burn_tick(t: &mut Tick<'_, '_>) -> bool {
    if !t.cx.env.monsters_burn || !t.cx.env.has_sky_light {
        return false;
    }
    let br = t.light_magic_value();
    let p = t.pos();
    let rounded = BlockPos::containing(p.x, t.eye_y(), p.z);
    let rain = (t.env.raining_at)(t.block_pos())
        || (t.env.raining_at)(BlockPos::containing(p.x, t.aabb().max_y, p.z));
    let wet = t.mob_ref().fluids.in_water() || rain;
    let roll = t.mob().random.next_float() * 30.0;
    br > 0.5 && roll < (br - 0.4) * 2.0 && !wet && super::can_see_sky(t.level, rounded)
}

/// `Mob.checkDespawn`.
pub fn check_despawn(t: &mut Tick<'_, '_>) {
    if t.env.difficulty == crate::survival::rules::Difficulty::Peaceful
        && !t.mob_ref().kind.allowed_in_peaceful()
    {
        t.mob().removed = true;
        return;
    }
    if !t.cx.has_players {
        return;
    }
    let persistent = t.mob_ref().persistent;
    let p = t.pos();
    let r = 128.0;
    let area = Aabb::new(p.x - r, p.y - r, p.z - r, p.x + r, p.y + r, p.z + r);
    let mut nearest = f64::MAX;
    t.ents.store.for_each_in(&area, true, |player| {
        if let Some(e) = t.ents.store.get(player) {
            let d = e.pos().distance_to_sqr(p);
            if d < nearest {
                nearest = d;
            }
        }
    });
    let far_away_ok = t.mob_ref().kind.remove_when_far_away();
    if !persistent && nearest > 128.0 * 128.0 && far_away_ok {
        t.mob().removed = true;
        return;
    }
    let m = t.mob();
    if !persistent
        && m.no_action_time > 600
        && m.random.next_int_bounded(800) == 0
        && nearest > 32.0 * 32.0
        && far_away_ok
    {
        m.removed = true;
    } else if nearest < 32.0 * 32.0 {
        m.no_action_time = 0;
    }
}

/// `Mob.getMaxFallDistance`.
pub fn max_fall_distance(t: &Tick<'_, '_>) -> i32 {
    let m = t.mob_ref();
    if m.target.is_none() {
        return 3;
    }
    if m.kind == super::Kind::Creeper {
        return floor(f64::from(m.health - 1.0 + 3.0));
    }
    #[allow(clippy::cast_possible_truncation)]
    let mut sacrifice = (m.health - m.max_health() * 0.33) as i32;
    sacrifice -= (3 - t.env.difficulty as i32) * 4;
    if sacrifice < 0 {
        sacrifice = 0;
    }
    floor(f64::from(sacrifice as f32 + 3.0))
}

/// `Sensing.hasLineOfSight` with its per-tick cache.
pub fn has_line_of_sight(t: &mut Tick<'_, '_>, target: EntityId) -> bool {
    if t.mob_ref().seen.contains(&target) {
        return true;
    }
    if t.mob_ref().unseen.contains(&target) {
        return false;
    }
    let Some(other) = t.other(target) else {
        return false;
    };
    let p = t.pos();
    let from = Vec3::new(p.x, t.eye_y(), p.z);
    let to = Vec3::new(other.pos.x, other.eye_y, other.pos.z);
    let can_see = to.distance_to_sqr(from) <= 128.0 * 128.0
        && physics::clip(t.level as &dyn Blocks, from, to).is_none();
    let m = t.mob();
    if can_see {
        m.seen.push(target);
    } else {
        m.unseen.push(target);
    }
    can_see
}
