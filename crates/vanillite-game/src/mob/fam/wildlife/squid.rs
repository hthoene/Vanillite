//! Squids and glow squids (`Squid`, `GlowSquid`): they jet through water by their tentacle
//! cycle, flee what hurt them with a cloud of ink, sink on land; glow squids go dark when hurt.

use vanillite_base::math::sin;
use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Vec3};
use vanillite_world::entity::Rotation;

use super::{Goal, st, st_mut};
use crate::effect::MobEffect;
use crate::level::Level;
use crate::mob::fam::{FamGoal, Hurt};
use crate::mob::goals::{Goal as MobGoal, reduced};
use crate::mob::kinds::Kind;
use crate::mob::{MobData, Tick, combat, living};

/// `Squid.registerGoals`.
pub(super) fn register_goals(m: &mut MobData) {
    m.goals.add(
        0,
        MobGoal::Fam(FamGoal::Wildlife(Goal::SquidRandomMovement)),
    );
    m.goals.add(
        1,
        MobGoal::Fam(FamGoal::Wildlife(Goal::SquidFlee { ticks: 0 })),
    );
}

/// `GlowSquid.checkGlowSquidSpawnRules`: deep, dark water.
pub(super) fn glow_squid_rules(level: &Level, pos: BlockPos) -> bool {
    pos.y <= crate::mob::sea_level(level) - 33
        && level.raw_brightness(pos) == 0
        && level.block(pos).block() == vanillite_data::block::ids::WATER
}

const PI: f32 = std::f32::consts::PI;
/// `Mth.RAD_TO_DEG` (`180.0F / (float) Math.PI`).
const RAD_TO_DEG: f32 = 180.0 / PI;

/// `Squid.aiStep` after `super.aiStep()`: the tentacle cycle drives the movement.
pub(super) fn ai_step(t: &mut Tick<'_, '_>) {
    {
        let m = t.mob();
        let s = st_mut(m);
        s.x_body_rot_o = s.x_body_rot;
        s.tentacle_movement += s.tentacle_speed;
    }
    if st(t.mob_ref()).tentacle_movement > PI * 2.0 {
        let m = t.mob();
        st_mut(m).tentacle_movement -= PI * 2.0;
        if m.random.next_int_bounded(10) == 0 {
            let speed = 1.0 / (m.random.next_float() + 1.0) * 0.2;
            st_mut(m).tentacle_speed = speed;
        }
        let id = t.id;
        living::entity_event(t, id, 19);
    }
    if t.mob_ref().fluids.in_water() {
        let tm = st(t.mob_ref()).tentacle_movement;
        if tm < PI {
            let scale = tm / PI;
            if scale > 0.75 {
                let v = st(t.mob_ref()).movement_vector;
                t.set_velocity(v);
                st_mut(t.mob()).rotate_speed = 1.0;
            } else {
                st_mut(t.mob()).rotate_speed *= 0.8;
            }
        } else {
            let v = t.velocity() * 0.9;
            t.set_velocity(v);
            st_mut(t.mob()).rotate_speed *= 0.99;
        }
        let v = t.velocity();
        let horizontal = (v.x * v.x + v.z * v.z).sqrt();
        // `-((float) Mth.atan2(x, z)) * RAD_TO_DEG`: a float product.
        #[allow(clippy::cast_possible_truncation)]
        let target_y = -(vanillite_base::math::atan2(v.x, v.z) as f32) * RAD_TO_DEG;
        let m = t.mob();
        m.y_body_rot += (target_y - m.y_body_rot) * 0.1;
        let body = m.y_body_rot;
        #[allow(clippy::cast_possible_truncation)]
        let target_x = -(vanillite_base::math::atan2(horizontal, v.y) as f32) * RAD_TO_DEG;
        let s = st_mut(m);
        s.x_body_rot += (target_x - s.x_body_rot) * 0.1;
        let rot = t.core().rot();
        t.core_mut().set_rot(Rotation { y_rot: body, ..rot });
    } else {
        let mut yd = t.velocity().y;
        if let Some(a) = t.mob_ref().effects.amplifier(MobEffect::LEVITATION) {
            yd = 0.05 * (f64::from(a) + 1.0);
        } else {
            yd -= living::gravity(t);
        }
        #[allow(clippy::cast_possible_truncation)]
        let modifier = t.mob_ref().attrs.air_drag_modifier as f32;
        let drag = living::modified_friction(0.98, modifier);
        t.set_velocity(Vec3::new(0.0, yd * f64::from(drag), 0.0));
        let s = st_mut(t.mob());
        s.x_body_rot += (-90.0 - s.x_body_rot) * 0.02;
    }
    if t.mob_ref().kind == Kind::GlowSquid {
        let s = st_mut(t.mob());
        if s.dark_ticks > 0 {
            s.dark_ticks -= 1;
            t.core_mut().mark_data_dirty();
        }
        // `GlowSquid.aiStep`: the glow particle's position (`getRandomX/Y/Z`), the client's
        // particle, the server's draws.
        let r = &mut t.mob().random;
        let _ = (r.next_double(), r.next_double(), r.next_double());
    }
}

/// `Squid.hurtServer` after the damage: ink when a mob hurt it; glow squids go dark.
pub(super) fn after_hurt(h: &mut Hurt<'_, '_>) {
    let Some(m) = h.ents.mob_mut(h.target) else {
        return;
    };
    if m.kind == Kind::GlowSquid {
        st_mut(m).dark_ticks = 100;
        if let Some(mut e) = h.ents.store.get_mut(h.target) {
            e.mark_data_dirty();
        }
    }
    let Some(m) = h.ents.mob(h.target) else {
        return;
    };
    if m.last_hurt_by_mob.is_none() {
        return;
    }
    // `spawnInk`: the squirt (the ink particles are client side effects of the server's
    // particle packets, not simulated here).
    let sound = if m.kind == Kind::GlowSquid {
        "minecraft:entity.glow_squid.squirt"
    } else {
        "minecraft:entity.squid.squirt"
    };
    combat::mob_sound(h.ents, h.env, h.target, sound);
}

pub(super) fn can_use(t: &mut Tick<'_, '_>, g: &mut Goal) -> bool {
    match g {
        Goal::SquidRandomMovement => true,
        Goal::SquidFlee { .. } => {
            let m = t.mob_ref();
            if !m.fluids.in_water() {
                return false;
            }
            m.last_hurt_by_mob
                .and_then(|by| t.other(by))
                .is_some_and(|o| t.pos().distance_to_sqr(o.pos) < 100.0)
        }
        _ => false,
    }
}

pub(super) fn tick(t: &mut Tick<'_, '_>, g: &mut Goal) {
    match g {
        Goal::SquidRandomMovement => {
            if t.mob_ref().no_action_time > 100 {
                st_mut(t.mob()).movement_vector = Vec3::ZERO;
                return;
            }
            let in_water = t.mob_ref().fluids.in_water();
            let has = st(t.mob_ref()).movement_vector.length_sqr() > f64::from(1.0e-5_f32);
            let m = t.mob();
            if m.random.next_int_bounded(reduced(50)) == 0 || !in_water || !has {
                let angle = m.random.next_float() * (PI * 2.0);
                let y = -0.1 + m.random.next_float() * 0.2;
                let v = Vec3::new(
                    f64::from(vanillite_base::math::cos(f64::from(angle)) * 0.2),
                    f64::from(y),
                    f64::from(sin(f64::from(angle)) * 0.2),
                );
                st_mut(m).movement_vector = v;
            }
        }
        Goal::SquidFlee { ticks } => {
            *ticks += 1;
            let Some(o) = t.mob_ref().last_hurt_by_mob.and_then(|by| t.other(by)) else {
                return;
            };
            let p = t.pos();
            let mut flee = Vec3::new(p.x - o.pos.x, p.y - o.pos.y, p.z - o.pos.z);
            let at = BlockPos::containing(p.x + flee.x, p.y + flee.y, p.z + flee.z);
            let state = t.level.block(at);
            if crate::mob::is_water(state) || state.is_air() {
                let length = flee.length();
                if length > 0.0 {
                    // `fleeTo.normalize()` discards its result in Vanilla.
                    let mut avoid = 3.0;
                    if length > 5.0 {
                        avoid -= (length - 5.0) / 5.0;
                    }
                    if avoid > 0.0 {
                        flee = flee * avoid;
                    }
                }
                if state.is_air() {
                    flee.y = 0.0;
                }
                st_mut(t.mob()).movement_vector =
                    Vec3::new(flee.x / 20.0, flee.y / 20.0, flee.z / 20.0);
            }
        }
        _ => {}
    }
}
