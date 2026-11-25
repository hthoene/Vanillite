//! Guardians: swimming, the laser beam, thorns, flopping on land.

use vanillite_base::random::{LegacyRandom, RandomSource};
use vanillite_base::{BlockPos, Vec3};
use vanillite_world::entity::EntityFlags;

use super::super::super::common::CommonGoal;
use super::super::super::goals::Goal as MobGoal;
use super::super::super::kinds::Kind;
use super::super::super::living::has_line_of_sight;
use super::super::super::nav::{Operation, rotlerp};
use super::super::super::{MobData, Tick, combat, living};
use super::super::{Hurt, SpawnReason};
use super::raider::{self, LookFilter};
use super::{Goal, dirty, fam, look_at_player, st, st_mut};
use crate::level::Level;
use crate::survival::damage::DamageSource;
use crate::survival::rules::Difficulty;

/// `Guardian.registerGoals`.
pub fn register_goals(m: &mut MobData) {
    let g = &mut m.goals;
    let tg = &mut m.targets;
    g.add(4, fam(Goal::GuardianAttack { attack_time: 0 }));
    // `MoveTowardsRestrictionGoal` and the stroll lock looking too.
    g.add(
        5,
        fam(Goal::Locked(CommonGoal::MoveTowardsRestriction {
            speed: 1.0,
            wanted: Vec3::ZERO,
        })),
    );
    g.add(
        7,
        fam(Goal::Stroll(raider::Stroll {
            speed: 1.0,
            interval: 80,
            wanted: Vec3::ZERO,
            look: true,
        })),
    );
    g.add(8, look_at_player(8.0, 0.02));
    g.add(8, raider::look_at_mobs(12.0, 0.01, LookFilter::Guardian));
    g.add(9, MobGoal::random_look());
    tg.add(
        1,
        MobGoal::Common(CommonGoal::nearest(10, true, |t, id| {
            let prey = t.ents.player(id).is_some()
                || t.ents.mob(id).is_some_and(|m| {
                    matches!(m.kind, Kind::Squid | Kind::GlowSquid | Kind::Axolotl)
                });
            prey && t
                .ents
                .store
                .get(id)
                .is_some_and(|e| e.pos().distance_to_sqr(t.pos()) > 9.0)
        })),
    );
}

/// `Guardian.checkGuardianSpawnRules`.
pub fn check_spawn_rules(
    level: &Level,
    pos: BlockPos,
    random: &mut LegacyRandom,
    reason: SpawnReason,
) -> bool {
    let water = |p: BlockPos| super::super::super::is_water(level.block(p));
    let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
    (random.next_int_bounded(20) == 0 || !can_see_sky_from_below_water(level, pos))
        && (matches!(reason, SpawnReason::Spawner | SpawnReason::TrialSpawner) || water(pos))
        && water(below)
}

/// The overworld sea level (guardians only spawn in overworld oceans and monuments).
const SEA_LEVEL: i32 = 63;

/// `LevelReader.canSeeSkyFromBelowWater`: the sky above sea level and nothing but liquids or
/// see-through blocks between.
fn can_see_sky_from_below_water(level: &Level, pos: BlockPos) -> bool {
    if pos.y >= SEA_LEVEL {
        return super::super::super::can_see_sky(level, pos);
    }
    let check = BlockPos::new(pos.x, SEA_LEVEL, pos.z);
    if !super::super::super::can_see_sky(level, check) {
        return false;
    }
    let mut y = check.y - 1;
    while y > pos.y {
        let s = level.block(BlockPos::new(pos.x, y, pos.z));
        if s.light_dampening() > 0 && s.fluid_state().is_empty() {
            return false;
        }
        y -= 1;
    }
    true
}

/// `Guardian.aiStep` before `super.aiStep()`: air, and flopping on land.
pub fn ai_step_start(t: &mut Tick<'_, '_>) {
    if !t.mob_ref().is_alive() {
        return;
    }
    if t.mob_ref().fluids.in_water() {
        t.mob().air = 300;
    } else if t.on_ground() {
        let r = &mut t.mob().random;
        let (a, b) = (r.next_float(), r.next_float());
        let v = t.velocity();
        t.set_velocity(Vec3::new(
            v.x + f64::from((a * 2.0 - 1.0) * 0.4),
            v.y + 0.5,
            v.z + f64::from((b * 2.0 - 1.0) * 0.4),
        ));
        let y_rot = t.mob().random.next_float() * 360.0;
        let mut rot = t.core().rot();
        rot.y_rot = y_rot;
        t.core_mut().set_rot(rot);
        t.core_mut().set_flags(EntityFlags::ON_GROUND, false);
        t.core_mut().set_flags(EntityFlags::HAS_IMPULSE, true);
    }
    if st(t.mob_ref()).beam_target != 0 {
        let mut rot = t.core().rot();
        rot.y_rot = rot.y_head_rot;
        t.core_mut().set_rot(rot);
    }
}

/// `Guardian.travelInWater`.
pub fn travel_in_water(t: &mut Tick<'_, '_>, input: Vec3) {
    living::move_relative(t, 0.1, input);
    let v = t.velocity();
    living::move_self(t, v);
    if !t.exists() {
        return;
    }
    let mut v = t.velocity() * 0.9;
    if !st(t.mob_ref()).moving && t.mob_ref().target.is_none() {
        v.y -= 0.005;
    }
    t.set_velocity(v);
}

fn set_moving(t: &mut Tick<'_, '_>, on: bool) {
    if st(t.mob_ref()).moving != on {
        st_mut(t.mob()).moving = on;
        dirty(t);
    }
}

/// `GuardianMoveControl.tick`.
pub fn move_control(t: &mut Tick<'_, '_>) {
    let ctl = t.mob_ref().move_ctl;
    if ctl.operation != Operation::MoveTo || t.mob_ref().nav.is_done() {
        t.mob().speed = 0.0;
        set_moving(t, false);
        return;
    }
    let p = t.pos();
    let delta = ctl.wanted - p;
    let length = delta.length();
    let (xd, yd, zd) = (delta.x / length, delta.y / length, delta.z / length);
    #[allow(clippy::cast_possible_truncation)]
    let y_rot_d = (delta.z.atan2(delta.x) * 180.0 / f64::from(std::f32::consts::PI)) as f32 - 90.0;
    let mut rot = t.core().rot();
    rot.y_rot = rotlerp(rot.y_rot, y_rot_d, 90.0);
    t.core_mut().set_rot(rot);
    t.mob().y_body_rot = rot.y_rot;
    #[allow(clippy::cast_possible_truncation)]
    let target_speed = (ctl.speed_modifier * t.mob_ref().attrs.movement_speed) as f32;
    let speed = t.mob_ref().speed + 0.125 * (target_speed - t.mob_ref().speed);
    t.mob().speed = speed;
    let tick = f64::from(t.mob_ref().tick_count + t.core().network_id().0);
    let push = (tick * 0.5).sin() * 0.05;
    let rad = f64::from(rot.y_rot) * std::f64::consts::PI / 180.0;
    let (cos, sin) = (rad.cos(), rad.sin());
    let y_push = (tick * 0.75).sin() * 0.05;
    let v = t.velocity();
    t.set_velocity(
        v + Vec3::new(
            push * cos,
            y_push * (sin + cos) * 0.25 + f64::from(speed) * yd * 0.1,
            push * sin,
        ),
    );
    let eye = t.eye_y();
    let new_look = Vec3::new(p.x + xd * 2.0, eye + yd / length, p.z + zd * 2.0);
    let look = t.mob_ref().look;
    let old = if look.is_looking() {
        look.wanted
    } else {
        new_look
    };
    let lerp = |a: f64, b: f64| a + 0.125 * (b - a);
    t.mob().look.set_look_at(
        Vec3::new(
            lerp(old.x, new_look.x),
            lerp(old.y, new_look.y),
            lerp(old.z, new_look.z),
        ),
        10.0,
        40.0,
    );
    set_moving(t, true);
}

/// `GuardianAttackGoal.canContinueToUse` (not closer than three blocks).
pub fn attack_can_continue(t: &mut Tick<'_, '_>) -> bool {
    let Some(target) = t.mob_ref().target else {
        return false;
    };
    let Some(o) = t.other(target) else {
        return false;
    };
    t.pos().distance_to_sqr(o.pos) > 9.0
}

pub fn attack_start(t: &mut Tick<'_, '_>, attack_time: &mut i32) {
    *attack_time = -10;
    t.mob().nav.stop();
    if let Some(o) = t.mob_ref().target.and_then(|id| t.other(id)) {
        t.mob()
            .look
            .set_look_at(Vec3::new(o.pos.x, o.eye_y, o.pos.z), 90.0, 90.0);
    }
}

fn set_beam_target(t: &mut Tick<'_, '_>, network_id: i32) {
    if st(t.mob_ref()).beam_target != network_id {
        st_mut(t.mob()).beam_target = network_id;
        dirty(t);
    }
}

pub fn attack_stop(t: &mut Tick<'_, '_>) {
    set_beam_target(t, 0);
    t.mob().target = None;
    st_mut(t.mob()).stroll_triggered = true;
}

/// `GuardianAttackGoal.tick`: the beam charges for 80 ticks, then hits.
pub fn attack_tick(t: &mut Tick<'_, '_>, attack_time: &mut i32) {
    let Some(target) = t.mob_ref().target else {
        return;
    };
    let Some(o) = t.other(target) else {
        return;
    };
    t.mob().nav.stop();
    t.mob()
        .look
        .set_look_at(Vec3::new(o.pos.x, o.eye_y, o.pos.z), 90.0, 90.0);
    if !has_line_of_sight(t, target) {
        t.mob().target = None;
        return;
    }
    *attack_time += 1;
    if *attack_time == 0 {
        let net = t.ents.store.get(target).map_or(0, |e| e.network_id().0);
        set_beam_target(t, net);
        if !t.mob_ref().silent {
            let id = t.id;
            living::entity_event(t, id, 21);
        }
    } else if *attack_time >= 80 {
        let mut magic = 1.0;
        if t.env.difficulty == Difficulty::Hard {
            magic += 2.0;
        }
        let source = combat::entity_source(t.ents, t.env.types.indirect_magic, t.id, t.id);
        combat::hurt(t.level, t.ents, t.env, t.cx, target, source, magic);
        if !t.exists() {
            return;
        }
        combat::mob_attack(t, target);
        if !t.exists() {
            return;
        }
        t.mob().target = None;
    }
}

/// `Guardian.hurtServer`: spikes hurt a living attacker unless the guardian swims.
pub fn thorns(h: &mut Hurt<'_, '_>) {
    let moving = h.ents.mob(h.target).is_some_and(|m| st(m).moving);
    if moving {
        return;
    }
    let name = h.env.types.get(h.source.kind).id.as_str();
    let avoids = matches!(name, "minecraft:magic" | "minecraft:thorns")
        || h.env
            .types
            .is(h.source.kind, crate::survival::damage::tag::IS_EXPLOSION);
    if avoids {
        return;
    }
    let Some(direct) = h.source.direct else {
        return;
    };
    let living = h.ents.player(direct).is_some() || h.ents.mob(direct).is_some();
    if !living || direct == h.target {
        return;
    }
    let mut source = DamageSource::of(h.env.types.thorns);
    source.cause = Some(h.target);
    source.direct = Some(h.target);
    source.position = h.ents.store.get(h.target).map(|e| e.pos());
    combat::hurt(h.level, h.ents, h.env, h.cx, direct, source, 2.0);
}
