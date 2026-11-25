//! Blazes: hovering towards their target, volleys of small fireballs.

use vanillite_base::Vec3;
use vanillite_base::random::RandomSource;
use vanillite_world::entity::Rotation;

use super::super::super::common::CommonGoal;
use super::super::super::goals::{self, Goal as MobGoal};
use super::super::super::living::has_line_of_sight;
use super::super::super::missile::{self, MissileData, MissileKind};
use super::super::super::{MobData, Tick, WorldAction, combat};
use super::{Goal, dirty, fam, look_at_player, st, st_mut, target_players};

/// `BlazeAttackGoal` state.
#[derive(Clone, Copy, Debug, Default)]
pub struct Attack {
    pub attack_step: i32,
    pub attack_time: i32,
    pub last_seen: i32,
}

/// `Blaze.registerGoals`.
pub fn register_goals(m: &mut MobData) {
    let g = &mut m.goals;
    let tg = &mut m.targets;
    g.add(4, fam(Goal::BlazeAttack(Attack::default())));
    g.add(
        5,
        MobGoal::Common(CommonGoal::MoveTowardsRestriction {
            speed: 1.0,
            wanted: Vec3::ZERO,
        }),
    );
    g.add(7, MobGoal::stroll_with(1.0, 0.0));
    g.add(8, look_at_player(8.0, 0.02));
    g.add(8, MobGoal::random_look());
    tg.add(1, MobGoal::hurt_by(true));
    tg.add(2, target_players(true));
}

/// `Blaze.aiStep` before `super.aiStep()`: falls slowly.
pub fn ai_step_start(t: &mut Tick<'_, '_>) {
    let v = t.velocity();
    if !t.on_ground() && v.y < 0.0 {
        t.set_velocity(Vec3::new(v.x, v.y * 0.6, v.z));
    }
}

/// `Blaze.customServerAiStep`: rises towards a target above it.
pub fn custom_server_ai_step(t: &mut Tick<'_, '_>) {
    let s = st_mut(t.mob());
    s.next_height_offset_change_tick -= 1;
    if s.next_height_offset_change_tick <= 0 {
        s.next_height_offset_change_tick = 100;
        #[allow(clippy::cast_possible_truncation)]
        let offset = t.mob().random.triangle(0.5, 6.891) as f32;
        st_mut(t.mob()).allowed_height_offset = offset;
    }
    let Some(target) = t.mob_ref().target else {
        return;
    };
    let Some(o) = t.other(target) else {
        return;
    };
    let offset = f64::from(st(t.mob_ref()).allowed_height_offset);
    if o.eye_y > t.eye_y() + offset && goals::can_attack(t, target) {
        let v = t.velocity();
        t.set_velocity(Vec3::new(
            v.x,
            v.y + (f64::from(0.3_f32) - v.y) * f64::from(0.3_f32),
            v.z,
        ));
    }
}

pub fn set_charged(t: &mut Tick<'_, '_>, on: bool) {
    let s = st_mut(t.mob());
    let flags = if on {
        s.blaze_flags | 1
    } else {
        s.blaze_flags & !1
    };
    if flags != s.blaze_flags {
        s.blaze_flags = flags;
        dirty(t);
    }
}

pub fn attack_can_use(t: &mut Tick<'_, '_>) -> bool {
    t.mob_ref()
        .target
        .is_some_and(|id| t.other(id).is_some() && goals::can_attack(t, id))
}

/// `BlazeAttackGoal.tick`: melee up close, three fireballs after a charge further away.
pub fn attack_tick(t: &mut Tick<'_, '_>, a: &mut Attack) {
    a.attack_time -= 1;
    let Some(target) = t.mob_ref().target else {
        return;
    };
    let Some(o) = t.other(target) else {
        return;
    };
    let sees = has_line_of_sight(t, target);
    if sees {
        a.last_seen = 0;
    } else {
        a.last_seen += 1;
    }
    let p = t.pos();
    let dist = p.distance_to_sqr(o.pos);
    let follow = t.mob_ref().attrs.follow_range;
    if dist < 4.0 {
        if !sees {
            return;
        }
        if a.attack_time <= 0 {
            a.attack_time = 20;
            combat::mob_attack(t, target);
            if !t.exists() {
                return;
            }
        }
        t.mob().move_ctl.set_wanted_position(o.pos, 1.0);
    } else if dist < follow * follow && sees {
        let half = f64::from(t.core().dims().height) * 0.5;
        let xd = o.pos.x - p.x;
        let yd = (o.aabb.min_y + (o.aabb.max_y - o.aabb.min_y) * 0.5) - (p.y + half);
        let zd = o.pos.z - p.z;
        if a.attack_time <= 0 {
            a.attack_step += 1;
            if a.attack_step == 1 {
                a.attack_time = 60;
                set_charged(t, true);
            } else if a.attack_step <= 4 {
                a.attack_time = 6;
            } else {
                a.attack_time = 100;
                a.attack_step = 0;
                set_charged(t, false);
            }
            if a.attack_step > 1 {
                let sqd = dist.sqrt().sqrt() * 0.5;
                if !t.mob_ref().silent {
                    let here = t.block_pos();
                    t.cx.actions.push(WorldAction::LevelEvent(1018, here, 0));
                }
                let r = &mut t.mob().random;
                let dir = Vec3::new(r.triangle(xd, 2.297 * sqd), yd, r.triangle(zd, 2.297 * sqd));
                let mut d = MissileData::new(MissileKind::SmallFireball);
                d.item = super::super::super::equipment::stack("minecraft:fire_charge");
                // `new SmallFireball(level, blaze, direction.normalize())` normalizes again.
                let velocity = dir.normalize().normalize() * d.acceleration;
                let start = Vec3::new(p.x, p.y + half + 0.5, p.z);
                let rot = t.core().rot();
                missile::spawn_from(
                    t,
                    start,
                    velocity,
                    Rotation {
                        y_head_rot: 0.0,
                        ..rot
                    },
                    d,
                );
            }
        }
        t.mob()
            .look
            .set_look_at(Vec3::new(o.pos.x, o.eye_y, o.pos.z), 10.0, 10.0);
    } else if a.last_seen < 5 {
        t.mob().move_ctl.set_wanted_position(o.pos, 1.0);
    }
}
