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
            if !t.mob_ref().kind.breathes_underwater() && !t.mob_ref().effects.has_water_breathing()
            {
                t.mob().air -= 1;
                if t.mob_ref().air <= -20 {
                    t.mob().air = 0;
                    let id = t.id;
                    entity_event(t, id, event::DROWN);
                    let source = DamageSource::of(t.env.types.drown);
                    hurt_self(t, source, 2.0);
                }
            } else if t.mob_ref().air < 300 && t.mob_ref().effects.refills_air() {
                let m = t.mob();
                m.air = (m.air + 4).min(300);
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
    // `tickEffects`.
    crate::effect::living::tick_mob(t);
    if !t.exists() || t.mob_ref().removed {
        return;
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
    if t.exists() && t.mob_ref().kind.family() != super::Family::Classic {
        super::fam::after_base_tick(t);
    }
}

/// `LivingEntity.tickDeath`.
fn tick_death(t: &mut Tick<'_, '_>) {
    if super::boss::tick_death(t) {
        return;
    }
    t.mob().death_time += 1;
    if t.mob_ref().death_time >= 20 {
        let id = t.id;
        entity_event(t, id, event::POOF);
        // `remove(KILLED)` → `triggerOnDeathMobEffects`.
        crate::effect::living::on_mob_killed(t);
        t.mob().removed = true;
    }
}

/// `LivingEntity.heal(amount)`.
pub fn heal(t: &mut Tick<'_, '_>, amount: f32) {
    let m = t.mob();
    if m.health > 0.0 {
        let max = m.max_health();
        m.health = (m.health + amount).clamp(0.0, max);
        t.core_mut().mark_data_dirty();
    }
}

/// Hurts the ticking mob itself (environment damage).
pub fn hurt_self(t: &mut Tick<'_, '_>, source: DamageSource, amount: f32) -> bool {
    let id = t.id;
    combat::hurt(t.level, t.ents, t.env, t.cx, id, source, amount)
}

/// `updateFluidInteraction` for water and lava: the heights, whether the eyes are inside and
/// the push of flowing fluids (`EntityFluidInteraction.applyCurrentTo`) unless the kind is
/// not pushed (`isPushedByFluid`).
fn update_fluids(t: &mut Tick<'_, '_>) {
    let p = t.pos();
    let eye = Vec3::new(p.x, t.eye_y(), p.z);
    let pushed = super::fam::pushed_by_fluid(t.mob_ref());
    let (contact, currents) = if pushed {
        fluid_currents(t.level, t.env.facts, t.aabb(), eye)
    } else {
        (
            fluid_contact(t.env.blocks, t.env.facts, t.aabb(), eye),
            Currents::default(),
        )
    };
    let m = t.mob();
    m.fluids = contact;
    if contact.in_water() {
        // `updateInWaterStateAndDoWaterCurrentPushing`: water ends the fall and the fire.
        m.fall_distance = 0.0;
        if m.fire_ticks > 0 {
            m.fire_ticks = 0;
        }
    }
    if pushed {
        if contact.in_water() {
            apply_current(t, currents.water, 0.014);
        }
        if contact.in_lava() {
            let scale = if t.level.settings.fast_lava {
                0.007
            } else {
                0.002_333_333_333_333_333_5
            };
            apply_current(t, currents.lava, scale);
        }
    }
}

/// The summed flows of the water and lava an entity touches
/// (`EntityFluidInteraction.CurrentAccumulator`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Currents {
    pub water: Current,
    pub lava: Current,
}

/// One fluid's accumulated current.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Current {
    pub sum: Vec3,
    pub count: u32,
}

/// `FluidState.getHeight`: full under the same fluid, else its own height.
fn fluid_height(
    level: &crate::level::Level,
    facts: &crate::survival::blocks::BlockFacts,
    pos: BlockPos,
    fluid: vanillite_data::fluid::FluidState,
) -> f32 {
    let above = level
        .block(BlockPos::new(pos.x, pos.y + 1, pos.z))
        .fluid_state();
    let same = (facts.is_water(fluid) && facts.is_water(above))
        || (facts.is_lava(fluid) && facts.is_lava(above));
    if same { 1.0 } else { fluid.info().own_height }
}

/// `EntityFluidInteraction.update` with the currents: the contact like
/// [`fluid_contact`], and per fluid the flow of every block the box touches (scaled by the
/// height while it is below 0.4).
pub fn fluid_currents(
    level: &crate::level::Level,
    facts: &crate::survival::blocks::BlockFacts,
    aabb: Aabb,
    eye: Vec3,
) -> (crate::survival::blocks::FluidContact, Currents) {
    let mut contact = crate::survival::blocks::FluidContact::default();
    let mut currents = Currents::default();
    let b = aabb.inflate(-0.001, -0.001, -0.001);
    let ceil = |v: f64| vanillite_base::math::ceil(v);
    let (x0, y0, z0) = (floor(b.min_x), floor(b.min_y), floor(b.min_z));
    let (x1, y1, z1) = (ceil(b.max_x) - 1, ceil(b.max_y) - 1, ceil(b.max_z) - 1);
    // `hasFluidAndLoaded`: the chunks around the box are loaded.
    for (x, z) in [
        (x0 - 1, z0 - 1),
        (x1 + 1, z1 + 1),
        (x0 - 1, z1 + 1),
        (x1 + 1, z0 - 1),
    ] {
        if !level.is_loaded(BlockPos::new(x, y0, z)) {
            return (contact, currents);
        }
    }
    let (eye_x, eye_z) = (floor(eye.x), floor(eye.z));
    let (mut water_height, mut lava_height) = (0.0_f64, 0.0_f64);
    for x in x0..=x1 {
        for y in y0..=y1 {
            for z in z0..=z1 {
                let pos = BlockPos::new(x, y, z);
                let state = level.block(pos);
                let fluid = state.fluid_state();
                if fluid.is_empty() {
                    continue;
                }
                let water = facts.is_water(fluid);
                if !water && !facts.is_lava(fluid) {
                    continue;
                }
                let bottom = f64::from(y);
                let top = bottom + f64::from(fluid_height(level, facts, pos, fluid));
                if top < b.min_y {
                    continue;
                }
                let eyes = x == eye_x && z == eye_z && eye.y >= bottom && {
                    // `getHeightForCamera`: a source under a sturdy bottom face is full.
                    let above = level.block(BlockPos::new(x, y + 1, z));
                    let camera = if fluid.info().source && above.misc().sturdy_full & 1 != 0 {
                        1.0
                    } else {
                        fluid_height(level, facts, pos, fluid)
                    };
                    eye.y <= bottom + f64::from(camera)
                };
                let height = top - aabb.min_y;
                let (running, current) = if water {
                    contact.water_height = contact.water_height.max(height);
                    contact.eye_in_water |= eyes;
                    water_height = water_height.max(height);
                    (water_height, &mut currents.water)
                } else {
                    contact.lava_height = contact.lava_height.max(height);
                    contact.eye_in_lava |= eyes;
                    lava_height = lava_height.max(height);
                    (lava_height, &mut currents.lava)
                };
                let mut flow = fluid_flow(level, facts, pos, fluid, water);
                if running < 0.4 {
                    flow = flow * running;
                }
                current.sum = current.sum + flow;
                current.count += 1;
            }
        }
    }
    (contact, currents)
}

/// `FlowingFluid.getFlow(level, pos, fluid)`.
pub fn fluid_flow(
    level: &dyn Blocks,
    facts: &crate::survival::blocks::BlockFacts,
    pos: BlockPos,
    fluid: vanillite_data::fluid::FluidState,
    water: bool,
) -> Vec3 {
    let same = |f: vanillite_data::fluid::FluidState| {
        if water {
            facts.is_water(f)
        } else {
            facts.is_lava(f)
        }
    };
    // `affectsFlow`: empty or the same fluid.
    let affects = |f: vanillite_data::fluid::FluidState| f.is_empty() || same(f);
    let own = fluid.info().own_height;
    // `Direction.Plane.HORIZONTAL`: north, east, south, west.
    const HORIZONTAL: [(i32, i32); 4] = [(0, -1), (1, 0), (0, 1), (-1, 0)];
    let (mut fx, mut fz) = (0.0_f64, 0.0_f64);
    for (dx, dz) in HORIZONTAL {
        let n = BlockPos::new(pos.x + dx, pos.y, pos.z + dz);
        let state = level.state(n);
        let nf = state.fluid_state();
        if !affects(nf) {
            continue;
        }
        let mut height = if nf.is_empty() {
            0.0
        } else {
            nf.info().own_height
        };
        let mut distance = 0.0_f32;
        if height == 0.0 {
            if !blocks_fluid_flow(state) {
                let below = level.state(BlockPos::new(n.x, n.y - 1, n.z)).fluid_state();
                if affects(below) {
                    height = if below.is_empty() {
                        0.0
                    } else {
                        below.info().own_height
                    };
                    if height > 0.0 {
                        distance = own - (height - 0.888_888_9);
                    }
                }
            }
        } else if height > 0.0 {
            distance = own - height;
        }
        if distance != 0.0 {
            #[allow(clippy::cast_precision_loss)]
            {
                fx += f64::from(dx as f32 * distance);
                fz += f64::from(dz as f32 * distance);
            }
        }
    }
    let mut flow = Vec3::new(fx, 0.0, fz);
    if fluid.info().falling {
        // `isSolidFace` towards each side at the block or the one above.
        let solid_face = |p: BlockPos, face_bit: u8| {
            let state = level.state(p);
            let f = state.fluid_state();
            if same(f) {
                return false;
            }
            // `IceBlock` (ice, frosted ice) never counts.
            if matches!(
                state.block().name(),
                "minecraft:ice" | "minecraft:frosted_ice"
            ) {
                return false;
            }
            state.misc().sturdy_full & face_bit != 0
        };
        // `Direction` 3D data values: north 2, south 3, west 4, east 5.
        const FACES: [(i32, i32, u8); 4] = [
            (0, -1, 1 << 2),
            (1, 0, 1 << 5),
            (0, 1, 1 << 3),
            (-1, 0, 1 << 4),
        ];
        for (dx, dz, bit) in FACES {
            let p = BlockPos::new(pos.x + dx, pos.y, pos.z + dz);
            let up = BlockPos::new(p.x, p.y + 1, p.z);
            if solid_face(p, bit) || solid_face(up, bit) {
                flow = flow.normalize() + Vec3::new(0.0, -6.0, 0.0);
                break;
            }
        }
    }
    flow.normalize()
}

/// `#minecraft:blocks_fluid_flow` (`#blocks_motion` and the signs): blocks that stop the flow
/// towards a lower neighbour.
fn blocks_fluid_flow(state: BlockState) -> bool {
    let block = state.block();
    let name = block.name();
    if name.ends_with("_sign") {
        return true;
    }
    if block == vanillite_data::block::ids::COBWEB || name == "minecraft:bamboo_sapling" {
        return false;
    }
    state.has(vanillite_data::block::flags::SOLID)
}

/// `CurrentAccumulator.applyTo(entity, scale)` for a mob: the normalized summed flow, at least
/// 0.0045 long when the mob barely moves.
fn apply_current(t: &mut Tick<'_, '_>, current: Current, scale: f64) {
    if current.count == 0 || current.sum.length_sqr() < f64::from(1.0e-5_f32) {
        return;
    }
    let mut impulse = current.sum.normalize() * scale;
    let old = t.velocity();
    if old.x.abs() < 0.003 && old.z.abs() < 0.003 && impulse.length() < 0.004_500_000_000_000_000_5
    {
        impulse = impulse.normalize() * 0.004_500_000_000_000_000_5;
    }
    t.set_velocity(old + impulse);
}

/// `LivingEntity.travelFlying(input, waterSpeed, lavaSpeed, airSpeed)`: flying mobs without
/// gravity (ghasts, phantoms, allays, happy ghasts).
pub fn travel_flying(
    t: &mut Tick<'_, '_>,
    input: Vec3,
    water_speed: f32,
    lava_speed: f32,
    air_speed: f32,
) {
    let fluids = t.mob_ref().fluids;
    let (speed, drag) = if fluids.in_water() {
        (water_speed, f64::from(0.8_f32))
    } else if fluids.in_lava() {
        (lava_speed, 0.5)
    } else {
        (air_speed, f64::from(0.91_f32))
    };
    move_relative(t, speed, input);
    let v = t.velocity();
    move_self(t, v);
    if !t.exists() {
        return;
    }
    let v = t.velocity();
    t.set_velocity(v * drag);
}

/// `getFluidJumpThreshold`: 0.4 blocks, or the eye height for small mobs.
fn fluid_jump_threshold(t: &Tick<'_, '_>) -> f64 {
    let eye = super::eye_height(t.mob_ref());
    if eye < 0.4 { 0.0 } else { 0.4 }
}

/// `LivingEntity.aiStep` with `Mob.aiStep`'s additions.
fn ai_step(t: &mut Tick<'_, '_>) {
    // The kind's `aiStep` override before it calls `super.aiStep()`.
    if t.mob_ref().kind.family() != super::Family::Classic {
        super::fam::ai_step_start(t);
        if !t.exists() || t.mob_ref().removed {
            return;
        }
    }
    // `Monster.updateNoActionTime` (`Raider`: always).
    let kind = t.mob_ref().kind;
    if kind.extends_monster() && (kind.is_raider() || t.light_magic_value() > 0.5) {
        t.mob().no_action_time += 2;
    }
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
    if super::boss::ai_step_override(t) {
        return;
    }
    let effective_ai = !t.mob_ref().no_ai;
    // `isImmobile`: dead or held by the kind (a stunned ravager).
    if t.mob_ref().is_dead_or_dying() || super::fam::is_immobile(t.mob_ref()) {
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
    {
        // Slow Falling and Levitation reset the fall distance before travelling.
        let m = t.mob();
        if m.effects.has(crate::effect::MobEffect::SLOW_FALLING)
            || m.effects.has(crate::effect::MobEffect::LEVITATION)
        {
            m.fall_distance = 0.0;
        }
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
        // `burnUndead`: a helmet takes the sun (and wears), else `igniteForSeconds(8)`.
        let head = crate::item::EquipmentSlot::Head;
        let helmet = t.mob_ref().equipment.get(head).clone();
        if helmet.is_empty() {
            let m = t.mob();
            m.fire_ticks = m.fire_ticks.max(160);
        } else if crate::item::is_damageable(&helmet) {
            let amount = t.mob().random.next_int_bounded(2);
            hurt_equipment(t, head, amount);
        }
    }
    // `Mob.aiStep`'s looting.
    if t.mob_ref().kind.family() == super::Family::Classic {
        let rules = super::equipment::PickupRules::default();
        super::equipment::pick_up_items(t, &rules, None, (1.0, 0.0, 1.0));
    } else {
        super::fam::pick_up_items(t);
    }
    if !t.exists() {
        return;
    }
    kinds::ai_step(t);
}

/// `hurtAndBreak(amount, mob, slot)` of a mob's equipment (without Unbreaking).
pub fn hurt_equipment(t: &mut Tick<'_, '_>, slot: crate::item::EquipmentSlot, amount: i32) {
    let mut s = t.mob_ref().equipment.get(slot).clone();
    if amount <= 0 || !crate::item::is_damageable(&s) {
        return;
    }
    let d = crate::item::damage(&s) + amount;
    if d >= crate::item::max_damage(&s) {
        s = crate::item::ItemStack::default();
        let sound = "minecraft:entity.item.break";
        super::kinds::play_sound(t, sound, 0.8, 0.8);
    } else {
        crate::item::set_damage(&mut s, d);
    }
    t.mob().equipment.set(slot, s);
    super::equipment::broadcast(t);
}

/// `Mob.jumpInLiquid`.
fn jump_in_liquid(t: &mut Tick<'_, '_>) {
    if t.mob_ref().kind.family() != super::Family::Classic && super::fam::jump_in_liquid(t) {
        return;
    }
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
    let mut power = t.mob_ref().attrs.jump_strength as f32 * block_jump_factor(t)
        + t.mob_ref().effects.jump_boost_power();
    if power <= 1.0e-5 {
        return;
    }
    if t.mob_ref().kind.family() != super::Family::Classic {
        power += super::fam::jump_power_bonus(t.mob_ref());
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
pub fn modified_friction(friction: f32, modifier: f32) -> f32 {
    (1.0 - (1.0 - friction) * modifier).clamp(0.0, 1.0)
}

/// `moveRelative(speed, input)`.
pub fn move_relative(t: &mut Tick<'_, '_>, speed: f32, input: Vec3) {
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
    if matches!(
        t.mob_ref().kind,
        super::Kind::Spider | super::Kind::CaveSpider
    ) {
        return t.mob_ref().climbing;
    }
    let state = t.level.block(t.block_pos());
    t.env.facts.is_climbable(state.block())
}

/// `LivingEntity.travel`.
fn travel(t: &mut Tick<'_, '_>, input: Vec3) {
    if t.mob_ref().kind.family() != super::Family::Classic && super::fam::travel(t, input) {
        return;
    }
    base_travel(t, input);
}

/// `LivingEntity.travel` without the kind's override (`super.travel(input)`).
pub fn base_travel(t: &mut Tick<'_, '_>, input: Vec3) {
    let fluids = t.mob_ref().fluids;
    // `shouldTravelInFluid`: in a liquid it cannot stand on (striders walk on lava).
    let stands = t.mob_ref().nav.stands_on_lava && {
        let p = t.pos();
        super::is_lava(t.level.block(BlockPos::containing(p.x, p.y, p.z)))
    };
    if (fluids.in_water() || fluids.in_lava()) && !stands {
        travel_in_fluid(t, input);
        float_in_liquid_while_ridden(t);
    } else {
        travel_in_air(t, input);
    }
}

/// `floatInLiquidWhileRidden`: ridden horses and camels (`#can_float_while_ridden`) rise.
fn float_in_liquid_while_ridden(t: &mut Tick<'_, '_>) {
    use super::Kind;
    let floats = matches!(
        t.mob_ref().kind,
        Kind::Horse | Kind::ZombieHorse | Kind::Mule | Kind::Donkey | Kind::Camel | Kind::CamelHusk
    );
    if !floats || t.ents.riding.passengers_of(t.id).is_empty() {
        return;
    }
    if t.mob_ref().fluids.water_height > fluid_jump_threshold(t) {
        let v = t.velocity();
        t.set_velocity(Vec3::new(v.x, v.y + f64::from(0.04_f32), v.z));
    }
}

/// `getEffectiveGravity`: Slow Falling caps it while falling.
pub fn gravity(t: &Tick<'_, '_>) -> f64 {
    let gravity = if t.core().flags().contains(EntityFlags::NO_GRAVITY) {
        0.0
    } else {
        t.mob_ref().attrs.gravity
    };
    let falling = t.velocity().y <= 0.0;
    t.mob_ref().effects.effective_gravity(gravity, falling)
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
    if let Some(a) = t
        .mob_ref()
        .effects
        .amplifier(crate::effect::MobEffect::LEVITATION)
    {
        y += (0.05 * f64::from(a + 1) - movement.y) * 0.2;
    } else if t.level.is_loaded(below) {
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
    if t.mob_ref().discard_friction {
        // `shouldDiscardFriction` (long jumps).
        t.set_velocity(Vec3::new(movement.x, y, movement.z));
        return;
    }
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
        let mut slow_down = super::fam::water_slow_down(t.mob_ref().kind);
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
        if t.mob_ref()
            .effects
            .has(crate::effect::MobEffect::DOLPHINS_GRACE)
        {
            slow_down = 0.96;
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
    if t.mob_ref().kind != super::Kind::EnderDragon {
        movement_emission(t, movement, effect_pos, effect_state);
    }
    let factor = f64::from(block_speed_factor(t));
    let v = t.velocity();
    t.set_velocity(v.multiply(factor, 1.0, factor));
}

/// Queues `gameEvent(event, pos, Context.of(this mob, affected))`.
fn post_game_event(t: &mut Tick<'_, '_>, event: u16, pos: Vec3, affected: Option<BlockState>) {
    let context = crate::level::game_event::GameEventContext {
        source: Some(t.id),
        affected,
    };
    t.cx.actions.push(super::WorldAction::Level(
        super::boss::warden::LevelAction::GameEvent {
            event,
            pos,
            context,
        },
    ));
}

/// `Entity.applyMovementEmissionAndPlaySound`: the `STEP` and `SWIM` game events of a mob's
/// move (the step sounds are not simulated).
fn movement_emission(
    t: &mut Tick<'_, '_>,
    movement: Vec3,
    effect_pos: BlockPos,
    effect_state: BlockState,
) {
    #[allow(clippy::cast_possible_truncation)]
    let (moved, horizontal) = (
        (movement.length() * f64::from(0.6_f32)) as f32,
        ((movement.x * movement.x + movement.z * movement.z).sqrt() * f64::from(0.6_f32)) as f32,
    );
    let supporting_pos = on_pos(t, 1.0e-5);
    let supporting = t.level.block(supporting_pos);
    let climbing = t.env.facts.is_climbable(supporting.block());
    {
        let m = t.mob();
        m.move_dist += if climbing { moved } else { horizontal };
        if m.move_dist <= m.next_step || supporting.is_air() {
            return;
        }
    }
    let on_ground = t.on_ground();
    let pos = t.pos();
    let steps = |t: &Tick<'_, '_>, st: BlockState| {
        !st.is_air() && (on_ground || t.env.facts.is_climbable(st.block()))
    };
    let only_effect = supporting_pos == effect_pos;
    let mut produced = false;
    if steps(t, effect_state) {
        produced = true;
        if only_effect {
            post_game_event(
                t,
                crate::level::game_event::events().step,
                pos,
                Some(effect_state),
            );
        }
    }
    if !only_effect && steps(t, supporting) {
        produced = true;
        post_game_event(
            t,
            crate::level::game_event::events().step,
            pos,
            Some(supporting),
        );
    }
    let in_water = t.mob_ref().fluids.in_water();
    let m = t.mob();
    if produced || in_water {
        #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
        let next = (m.move_dist as i32).saturating_add(1) as f32;
        m.next_step = next;
    }
    if !produced && in_water {
        post_game_event(t, crate::level::game_event::events().swim, pos, None);
    }
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
            // `TurtleEggBlock.fallOn`: zombies land on eggs without breaking them.
            if on_state.block() == vanillite_data::block::ids::TURTLE_EGG
                && !crate::mob::fam::undead::zombies::is_zombie(t.mob_ref().kind)
            {
                trample_turtle_egg(t, 3);
            }
            let pos = t.pos();
            post_game_event(
                t,
                crate::level::game_event::events().hit_ground,
                pos,
                Some(on_state),
            );
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

/// Queues `TurtleEggBlock.destroyEgg` for the eggs the mob stands on.
fn trample_turtle_egg(t: &mut Tick<'_, '_>, randomness: i32) {
    let pos = on_pos(t, f64::from(0.2_f32));
    // `canDestroyEgg`: living entities but turtles and bats, with `mob_griefing`.
    let can_destroy = !matches!(t.mob_ref().kind, super::Kind::Turtle | super::Kind::Bat)
        && t.env.rules.bool(Rule::MOB_GRIEFING);
    t.cx.actions.push(super::WorldAction::TrampleTurtleEgg {
        pos,
        can_destroy,
        randomness,
    });
}

/// `applyEffectsFromBlocks` / `checkInsideBlocks` for fire, lava, cobwebs and berry bushes.
fn apply_effects_from_blocks(t: &mut Tick<'_, '_>) {
    // `stepOn` of the block below (turtle eggs; mobs are never careful).
    if t.on_ground()
        && t.level.block(on_pos(t, f64::from(0.2_f32))).block()
            == vanillite_data::block::ids::TURTLE_EGG
    {
        trample_turtle_egg(t, 100);
    }
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
                if block == vanillite_data::block::ids::BIG_DRIPLEAF {
                    // `BigDripleafBlock.entityInside`.
                    t.cx.actions.push(super::WorldAction::EntityInside(
                        BlockPos::new(x, y, z),
                        t.id,
                    ));
                }
                if block == vanillite_data::block::ids::COBWEB {
                    stuck = Some(Vec3::new(0.25, f64::from(0.05_f32), 0.25));
                } else if block == vanillite_data::block::ids::SWEET_BERRY_BUSH {
                    stuck = Some(Vec3::new(f64::from(0.8_f32), 0.75, f64::from(0.8_f32)));
                }
            }
        }
    }
    // `WitherBoss.makeStuckInBlock` does nothing.
    if let Some(s) = stuck
        && t.mob_ref().kind != super::Kind::Wither
    {
        t.mob().fall_distance = 0.0;
        t.mob().stuck_speed = s;
    }
    bubble_columns(t, x0..=x1, y0..=y1, z0..=z1);
}

/// `BubbleColumnBlock.entityInside`: an upward or downward drag, stronger at the top
/// (`onAboveBubbleColumn`) than inside (`onInsideBubbleColumn`).
fn bubble_columns(
    t: &mut Tick<'_, '_>,
    xs: std::ops::RangeInclusive<i32>,
    ys: std::ops::RangeInclusive<i32>,
    zs: std::ops::RangeInclusive<i32>,
) {
    for x in xs {
        for y in ys.clone() {
            for z in zs.clone() {
                let pos = BlockPos::new(x, y, z);
                let state = t.level.block(pos);
                if !t.env.facts.is_bubble_column(state) {
                    continue;
                }
                let drag_down = state.value("drag") == Some("true");
                let above = t.level.block(BlockPos::new(x, y + 1, z));
                let nothing_above =
                    above.collision_shape().is_empty() && above.fluid_state().is_empty();
                let v = t.velocity();
                let yd = match (nothing_above, drag_down) {
                    (true, true) => (v.y - 0.03).max(-0.9),
                    (true, false) => (v.y + 0.1).min(1.8),
                    (false, true) => (v.y - 0.03).max(-0.3),
                    (false, false) => (v.y + 0.06).min(0.7),
                };
                t.set_velocity(Vec3::new(v.x, yd, v.z));
                if !nothing_above {
                    t.mob().fall_distance = 0.0;
                }
            }
        }
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
        if t.mob_ref().kind == super::Kind::Warden {
            // `Warden.doPush`.
            super::boss::warden::do_push(t, other);
        }
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
    if !super::boss::move_control(t) {
        nav::tick_move_control(t);
    }
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
    // `isPersistenceRequired || requiresCustomPersistence` (a passenger or leashed).
    let persistent = t.mob_ref().persistent
        || super::fam::requires_custom_persistence(t.mob_ref())
        || t.ents.riding.vehicle_of(t.id).is_some()
        || t.ents.leashes.contains_key(&t.id);
    let category = super::spawn::Category::of(t.core().kind());
    let despawn = f64::from(category.map_or(128, super::spawn::Category::despawn_distance));
    let p = t.pos();
    let r = 128.0;
    let area = Aabb::new(p.x - r, p.y - r, p.z - r, p.x + r, p.y + r, p.z + r);
    let mut nearest = f64::MAX;
    t.ents.store.for_each_in(&area, true, |player| {
        // `getNearestPlayer` skips spectators.
        if t.ents
            .player(player)
            .is_some_and(|p| p.game_mode == vanillite_config::GameMode::Spectator)
        {
            return;
        }
        if let Some(e) = t.ents.store.get(player) {
            let d = e.pos().distance_to_sqr(p);
            if d < nearest {
                nearest = d;
            }
        }
    });
    let far_away_ok = super::fam::remove_when_far_away_mob(t.mob_ref(), nearest);
    if !persistent && nearest > despawn * despawn && far_away_ok {
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

#[cfg(test)]
mod tests {
    use super::*;
    use vanillite_data::block::ids;

    /// A water source at the origin flowing east into a level 1 block, on stone.
    struct Stream;
    impl Blocks for Stream {
        fn state(&self, pos: BlockPos) -> BlockState {
            if pos.y < 64 {
                return ids::STONE.default_state();
            }
            if pos.y == 64 && pos.z == 0 {
                let water = ids::WATER.default_state();
                match pos.x {
                    0 => return water,
                    1 => return water.with("level", "1").unwrap_or(water),
                    _ => {}
                }
            }
            BlockState::AIR
        }
    }

    fn facts() -> crate::survival::blocks::BlockFacts {
        use crate::block::fluid::{FLOWING_LAVA, FLOWING_WATER, LAVA, WATER};
        crate::survival::blocks::BlockFacts::new(
            &[],
            &[],
            &[],
            [FLOWING_WATER, WATER],
            [FLOWING_LAVA, LAVA],
        )
    }

    #[test]
    fn water_flows_down_its_slope() {
        let facts = facts();
        let pos = BlockPos::new(0, 64, 0);
        let fluid = Stream.state(pos).fluid_state();
        let flow = fluid_flow(&Stream, &facts, pos, fluid, true);
        assert!((flow.x - 1.0).abs() < 1.0e-9, "{flow:?}");
        assert!(flow.y.abs() < 1.0e-9 && flow.z.abs() < 1.0e-9);
        // The flowing block pushes on to the east too (its east neighbour is lower).
        let next = BlockPos::new(1, 64, 0);
        let flow = fluid_flow(
            &Stream,
            &facts,
            next,
            Stream.state(next).fluid_state(),
            true,
        );
        assert!(flow.x > 0.0, "{flow:?}");
    }
}
