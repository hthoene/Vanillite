//! `WardenAi`: the warden's activities, behaviours and sensors on the mob brain
//! ([`crate::mob::brain`]). The shared behaviours (swimming, the look and walk sinks, the
//! random stroll, `DoNothing`, `RunOne`) are the brain's own; the warden's plug in as
//! [`FamBehavior::Warden`] and its entity sensor as [`FamSensor::Warden`].

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Vec3};
use vanillite_world::entity::{Dimensions, EntityFlags, EntityId, Pose};

use super::{
    DEFAULT_ANGER, can_target_entity, entity_angry_at, increase_anger_at, play_sound,
    set_attack_target, set_dig_cooldown, set_disturbance_location, set_sonic_boom_cooldown,
    voice_pitch,
};
use crate::mob::brain::behavior::{FamBehavior, Shape, Spec, StrollMode};
use crate::mob::brain::memory::MemoryStatus as S;
use crate::mob::brain::memory::MemoryType as M;
use crate::mob::brain::sensor::{FamSensor, SensorKind};
use crate::mob::brain::{
    Activity as A, ActivitySpec, Brain, BrainSpec, MemValue, Tracker, WalkTarget, brain, brain_ref,
    sensing,
};
use crate::mob::{MobData, Tick, combat, nav};

/// `WardenAi.EMERGE_DURATION` (`ceil(133.59999)`).
pub const EMERGE_DURATION: i32 = 134;
/// `DIGGING_DURATION`.
const DIGGING_DURATION: i32 = 100;
/// `ROAR_DURATION`.
const ROAR_DURATION: i32 = 84;
/// `SNIFFING_DURATION` (`ceil(83.2)`).
const SNIFFING_DURATION: i32 = 84;
/// `MELEE_ATTACK_COOLDOWN`.
const MELEE_ATTACK_COOLDOWN: i64 = 18;
/// `SonicBoom.DURATION`.
const SONIC_BOOM_DURATION: i32 = 60;
/// `SonicBoom.TICKS_BEFORE_PLAYING_SOUND`.
const SONIC_BOOM_SOUND_DELAY: i64 = 34;

/// The warden's sensors (`SensorType.NEAREST_PLAYERS` is the brain's own).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sensor {
    /// `SensorType.WARDEN_ENTITY_SENSOR`.
    WardenEntity,
}

pub fn sensor_scan_rate(_s: Sensor) -> i32 {
    20
}

pub fn sensor_requires(_s: &Sensor) -> &'static [M] {
    &[
        M::NearestLivingEntities,
        M::NearestVisibleLivingEntities,
        M::NearestAttackable,
    ]
}

/// `WardenEntitySensor.doTick`: the nearest living entities, then the nearest targetable
/// player, else the nearest targetable other entity.
pub fn sensor_tick(t: &mut Tick<'_, '_>, _s: Sensor) {
    crate::mob::brain::sensor::tick(t, SensorKind::NearestLivingEntities);
    let list = brain_ref(t).entities(M::NearestLivingEntities).to_vec();
    let pick = |player: bool| {
        list.iter()
            .copied()
            .find(|&e| can_target_entity(t.ents, Some(e)) && t.ents.player(e).is_some() == player)
    };
    let nearest = pick(true).or_else(|| pick(false));
    brain(t).set_opt(M::NearestAttackable, nearest.map(MemValue::Entity));
}

/// The warden's own behaviours.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Behavior {
    /// `SetWardenLookTarget`.
    SetWardenLookTarget,
    /// `Emerging(EMERGE_DURATION)`.
    Emerging,
    /// `ForceUnmount` (wardens never ride).
    ForceUnmount,
    /// `Digging(DIGGING_DURATION)`.
    Digging,
    /// `SetRoarTarget(Warden::getEntityAngryAt)`.
    SetRoarTarget,
    /// `TryToSniff`.
    TryToSniff,
    /// `Roar`.
    Roar,
    /// `DIG_COOLDOWN_SETTER`.
    DigCooldownSetter,
    /// `StopAttackingIfTargetInvalid` of the fight activity.
    StopAttackingIfTargetInvalid,
    /// `SetEntityLookTarget(isTarget, followRange)`.
    SetEntityLookTarget,
    /// `SetWalkTargetFromAttackTargetIfTargetOutOfReach(1.2)`.
    SetWalkTargetFromAttackTarget,
    /// `SonicBoom`.
    SonicBoom,
    /// `MeleeAttack(18)`.
    MeleeAttack,
    /// `GoToTargetLocation(DISTURBANCE_LOCATION, 2, 0.7)`.
    GoToTargetLocation,
    /// `Sniffing(SNIFFING_DURATION)`.
    Sniffing,
}

fn fam(b: Behavior) -> Spec {
    Spec::fam(FamBehavior::Warden(b))
}

pub fn behavior_duration(b: &Behavior) -> (i32, i32) {
    match b {
        Behavior::Emerging => (EMERGE_DURATION, EMERGE_DURATION),
        Behavior::Digging => (DIGGING_DURATION, DIGGING_DURATION),
        Behavior::Roar => (ROAR_DURATION, ROAR_DURATION),
        Behavior::SonicBoom => (SONIC_BOOM_DURATION, SONIC_BOOM_DURATION),
        Behavior::Sniffing => (SNIFFING_DURATION, SNIFFING_DURATION),
        _ => (60, 60),
    }
}

pub fn behavior_shape(b: &Behavior) -> Shape {
    match b {
        Behavior::Emerging
        | Behavior::ForceUnmount
        | Behavior::Digging
        | Behavior::Roar
        | Behavior::SonicBoom
        | Behavior::Sniffing => Shape::Timed,
        _ => Shape::OneShot,
    }
}

const SET_ROAR_TARGET: &[(M, S)] = &[
    (M::RoarTarget, S::ValueAbsent),
    (M::AttackTarget, S::ValueAbsent),
    (M::CantReachWalkTargetSince, S::Registered),
];

pub fn behavior_requirements(b: &Behavior) -> &'static [(M, S)] {
    match b {
        Behavior::ForceUnmount => &[],
        Behavior::SetWardenLookTarget => &[
            (M::LookTarget, S::Registered),
            (M::DisturbanceLocation, S::Registered),
            (M::RoarTarget, S::Registered),
            (M::AttackTarget, S::ValueAbsent),
        ],
        Behavior::Emerging => &[
            (M::IsEmerging, S::ValuePresent),
            (M::WalkTarget, S::ValueAbsent),
            (M::LookTarget, S::Registered),
        ],
        Behavior::Digging => &[
            (M::AttackTarget, S::ValueAbsent),
            (M::WalkTarget, S::ValueAbsent),
        ],
        Behavior::SetRoarTarget => SET_ROAR_TARGET,
        Behavior::TryToSniff => &[
            (M::IsSniffing, S::Registered),
            (M::WalkTarget, S::Registered),
            (M::SniffCooldown, S::ValueAbsent),
            (M::NearestAttackable, S::ValuePresent),
            (M::DisturbanceLocation, S::ValueAbsent),
        ],
        Behavior::Roar => &[
            (M::RoarTarget, S::ValuePresent),
            (M::AttackTarget, S::ValueAbsent),
            (M::RoarSoundCooldown, S::Registered),
            (M::RoarSoundDelay, S::Registered),
        ],
        Behavior::DigCooldownSetter => &[(M::DigCooldown, S::Registered)],
        Behavior::StopAttackingIfTargetInvalid => &[
            (M::AttackTarget, S::ValuePresent),
            (M::CantReachWalkTargetSince, S::Registered),
        ],
        Behavior::SetEntityLookTarget => &[
            (M::LookTarget, S::ValueAbsent),
            (M::NearestVisibleLivingEntities, S::ValuePresent),
        ],
        Behavior::SetWalkTargetFromAttackTarget => &[
            (M::WalkTarget, S::Registered),
            (M::LookTarget, S::Registered),
            (M::AttackTarget, S::ValuePresent),
            (M::NearestVisibleLivingEntities, S::Registered),
        ],
        Behavior::SonicBoom => &[
            (M::AttackTarget, S::ValuePresent),
            (M::SonicBoomCooldown, S::ValueAbsent),
            (M::SonicBoomSoundCooldown, S::Registered),
            (M::SonicBoomSoundDelay, S::Registered),
        ],
        Behavior::MeleeAttack => &[
            (M::LookTarget, S::Registered),
            (M::AttackTarget, S::ValuePresent),
            (M::AttackCoolingDown, S::ValueAbsent),
            (M::NearestVisibleLivingEntities, S::ValuePresent),
        ],
        Behavior::GoToTargetLocation => &[
            (M::DisturbanceLocation, S::ValuePresent),
            (M::AttackTarget, S::ValueAbsent),
            (M::WalkTarget, S::ValueAbsent),
            (M::LookTarget, S::Registered),
        ],
        Behavior::Sniffing => &[
            (M::IsSniffing, S::ValuePresent),
            (M::AttackTarget, S::ValueAbsent),
            (M::WalkTarget, S::ValueAbsent),
            (M::LookTarget, S::Registered),
            (M::NearestAttackable, S::Registered),
            (M::DisturbanceLocation, S::Registered),
            (M::SniffCooldown, S::Registered),
        ],
    }
}

pub fn behavior_no_timeout(_b: &Behavior) -> bool {
    false
}

/// `setPose(pose)` with the warden's dimensions (1 block high while digging or emerging).
pub(super) fn set_pose(t: &mut Tick<'_, '_>, pose: Pose) {
    let kind = t.core().kind();
    let mut dims = crate::mob::kinds::dimensions(kind, false);
    if matches!(pose, Pose::Digging | Pose::Emerging) {
        dims = Dimensions {
            width: dims.width,
            height: 1.0,
        };
    }
    let mut e = t.core_mut();
    e.set_pose(pose);
    e.set_dims(dims);
}

fn pose(t: &Tick<'_, '_>) -> Pose {
    t.core().pose()
}

fn sound(t: &mut Tick<'_, '_>, name: &str, volume: f32, pitch: f32) {
    let id = t.id;
    play_sound(t.ents, t.env, id, name, volume, pitch);
}

/// Runs `f` with the warden's brain taken out of it, for the warden functions that work on
/// the entities and the memories together (they do not read the brain through the mob).
fn with_mem<R>(t: &mut Tick<'_, '_>, f: impl FnOnce(&mut Tick<'_, '_>, &mut Brain) -> R) -> R {
    let mut b = t.mob().brain.take().unwrap_or_default();
    let r = f(t, &mut b);
    t.mob().brain = Some(b);
    r
}

/// `Entity.closerThan(other, horizontal, vertical)`.
fn closer_than(t: &Tick<'_, '_>, other: EntityId, h: f64, v: f64) -> bool {
    let Some(o) = t.ents.store.get(other).map(|e| e.pos()) else {
        return false;
    };
    let p = t.pos();
    let (dx, dy, dz) = (o.x - p.x, o.y - p.y, o.z - p.z);
    dx * dx + dz * dz < h * h && dy * dy < v * v
}

/// `Mob.isWithinMeleeAttackRange`.
fn within_melee_range(t: &Tick<'_, '_>, target: EntityId) -> bool {
    let Some(o) = t.other(target) else {
        return false;
    };
    let reach = f64::from(2.04_f32).sqrt() - f64::from(0.6_f32);
    t.aabb().inflate(reach, 0.0, reach).intersects(&o.aabb)
}

fn block_pos_of(t: &Tick<'_, '_>, e: EntityId) -> Option<BlockPos> {
    let p = t.ents.store.get(e)?.pos();
    Some(BlockPos::containing(p.x, p.y, p.z))
}

/// `checkExtraStartConditions`.
pub fn behavior_check(t: &mut Tick<'_, '_>, b: &mut Behavior) -> bool {
    match b {
        Behavior::ForceUnmount => false,
        Behavior::Digging => {
            t.on_ground() || {
                let f = t.mob_ref().fluids;
                f.in_water() || f.in_lava()
            }
        }
        Behavior::SonicBoom => brain_ref(t)
            .entity(M::AttackTarget)
            .is_some_and(|target| closer_than(t, target, 15.0, 20.0)),
        _ => true,
    }
}

/// `canStillUse`.
pub fn behavior_can_still_use(t: &mut Tick<'_, '_>, b: &mut Behavior, _now: i64) -> bool {
    match b {
        Behavior::Digging => !t.mob_ref().removed,
        Behavior::Emerging | Behavior::Roar | Behavior::SonicBoom | Behavior::Sniffing => true,
        _ => false,
    }
}

/// `start` of the timed behaviours.
pub fn behavior_start(t: &mut Tick<'_, '_>, b: &mut Behavior, now: i64) {
    let id = t.id;
    match b {
        Behavior::Emerging => {
            set_pose(t, Pose::Emerging);
            sound(t, "minecraft:entity.warden.emerge", 5.0, 1.0);
        }
        Behavior::Digging => {
            if t.on_ground() {
                set_pose(t, Pose::Digging);
                sound(t, "minecraft:entity.warden.dig", 5.0, 1.0);
            } else {
                sound(t, "minecraft:entity.warden.agitated", 5.0, 1.0);
                behavior_stop(t, b, now);
            }
        }
        Behavior::Roar => {
            let br = brain(t);
            br.set_with_expiry(M::RoarSoundDelay, MemValue::Unit, 25);
            br.erase(M::WalkTarget);
            if let Some(target) = br.entity(M::RoarTarget) {
                br.set(M::LookTarget, MemValue::Look(Tracker::entity(target, true)));
                set_pose(t, Pose::Roaring);
                with_mem(t, |t, mem| {
                    let Tick { ents, env, .. } = t;
                    increase_anger_at(ents, env, id, mem, Some(target), 20, false);
                });
            }
        }
        Behavior::SonicBoom => {
            let br = brain(t);
            br.set_with_expiry(
                M::AttackCoolingDown,
                MemValue::Bool(true),
                i64::from(SONIC_BOOM_DURATION),
            );
            br.set_with_expiry(
                M::SonicBoomSoundDelay,
                MemValue::Unit,
                SONIC_BOOM_SOUND_DELAY,
            );
            super::entity_event(t.ents, t.env, id, 62);
            sound(t, "minecraft:entity.warden.sonic_charge", 3.0, 1.0);
        }
        Behavior::Sniffing => sound(t, "minecraft:entity.warden.sniff", 5.0, 1.0),
        _ => {}
    }
}

/// `tick` of the timed behaviours.
pub fn behavior_tick(t: &mut Tick<'_, '_>, b: &mut Behavior, _now: i64) {
    match b {
        Behavior::Roar => {
            let br = brain(t);
            if !br.has(M::RoarSoundDelay) && !br.has(M::RoarSoundCooldown) {
                br.set_with_expiry(
                    M::RoarSoundCooldown,
                    MemValue::Unit,
                    i64::from(ROAR_DURATION - 25),
                );
                sound(t, "minecraft:entity.warden.roar", 3.0, 1.0);
            }
        }
        Behavior::SonicBoom => sonic_boom_tick(t),
        _ => {}
    }
}

/// `stop` of the timed behaviours.
pub fn behavior_stop(t: &mut Tick<'_, '_>, b: &mut Behavior, _now: i64) {
    let id = t.id;
    match b {
        Behavior::Emerging => {
            if pose(t) == Pose::Emerging {
                set_pose(t, Pose::Standing);
            }
        }
        Behavior::Digging => {
            if !t.mob_ref().removed {
                t.mob().removed = true;
            }
        }
        Behavior::Roar => {
            if pose(t) == Pose::Roaring {
                set_pose(t, Pose::Standing);
            }
            let br = brain(t);
            if let Some(target) = br.entity(M::RoarTarget) {
                set_attack_target(br, target);
            }
            br.erase(M::RoarTarget);
        }
        Behavior::SonicBoom => set_sonic_boom_cooldown(brain(t), 40),
        Behavior::Sniffing => {
            if pose(t) == Pose::Sniffing {
                set_pose(t, Pose::Standing);
            }
            brain(t).erase(M::IsSniffing);
            if let Some(e) = brain_ref(t).entity(M::NearestAttackable)
                && can_target_entity(t.ents, Some(e))
            {
                if closer_than(t, e, 6.0, 20.0) {
                    with_mem(t, |t, mem| {
                        let Tick { ents, env, .. } = t;
                        increase_anger_at(ents, env, id, mem, Some(e), DEFAULT_ANGER, true);
                    });
                }
                if !brain_ref(t).has(M::DisturbanceLocation)
                    && let Some(p) = block_pos_of(t, e)
                {
                    with_mem(t, |t, mem| set_disturbance_location(t.ents, id, mem, p));
                }
            }
        }
        _ => {}
    }
}

/// The trigger of the one shot behaviours.
pub fn behavior_trigger(t: &mut Tick<'_, '_>, b: &mut Behavior, _now: i64) -> bool {
    let id = t.id;
    match b {
        Behavior::SetWardenLookTarget => {
            let target = brain_ref(t)
                .entity(M::RoarTarget)
                .and_then(|e| block_pos_of(t, e))
                .or_else(|| brain_ref(t).pos(M::DisturbanceLocation));
            match target {
                Some(p) => {
                    brain(t).set(M::LookTarget, MemValue::Look(Tracker::block(p)));
                    true
                }
                None => false,
            }
        }
        Behavior::SetRoarTarget => {
            let angry = with_mem(t, |t, mem| entity_angry_at(t.ents, id, mem));
            match angry.filter(|&e| can_target_entity(t.ents, Some(e))) {
                Some(e) => {
                    let br = brain(t);
                    br.set(M::RoarTarget, MemValue::Entity(e));
                    br.erase(M::CantReachWalkTargetSince);
                    true
                }
                None => false,
            }
        }
        Behavior::TryToSniff => {
            brain(t).set(M::IsSniffing, MemValue::Unit);
            let cooldown = 100 + t.env.random.next_int_bounded(101);
            let br = brain(t);
            br.set_with_expiry(M::SniffCooldown, MemValue::Unit, i64::from(cooldown));
            br.erase(M::WalkTarget);
            set_pose(t, Pose::Sniffing);
            true
        }
        Behavior::DigCooldownSetter => {
            set_dig_cooldown(brain(t));
            true
        }
        Behavior::StopAttackingIfTargetInvalid => {
            let Some(target) = brain_ref(t).entity(M::AttackTarget) else {
                return false;
            };
            let valid = can_target_entity(t.ents, Some(target));
            let angry = with_mem(t, |t, mem| super::anger_level(t.ents, id, mem).is_angry());
            if valid && angry {
                return true;
            }
            // `WardenAi.onTargetInvalid`.
            if !valid && let Some(s) = super::state_mut(t.ents, id) {
                s.anger.clear_anger(target);
            }
            let br = brain(t);
            set_dig_cooldown(br);
            br.erase(M::AttackTarget);
            true
        }
        Behavior::SetEntityLookTarget => {
            let Some(attack) = brain_ref(t).entity(M::AttackTarget) else {
                return false;
            };
            let range = t.mob_ref().attrs.follow_range as f32;
            let max = f64::from(range * range);
            let me = t.pos();
            let found = sensing::visible_find_closest(t, |t, e| {
                e == attack
                    && t.ents
                        .store
                        .get(e)
                        .is_some_and(|x| x.pos().distance_to_sqr(me) <= max)
            });
            match found {
                Some(e) => {
                    brain(t).set(M::LookTarget, MemValue::Look(Tracker::entity(e, true)));
                    true
                }
                None => false,
            }
        }
        Behavior::SetWalkTargetFromAttackTarget => {
            let Some(target) = brain_ref(t).entity(M::AttackTarget) else {
                return false;
            };
            if sensing::visible_contains(t, target) && within_melee_range(t, target) {
                brain(t).erase(M::WalkTarget);
            } else {
                let br = brain(t);
                br.set(M::LookTarget, MemValue::Look(Tracker::entity(target, true)));
                br.set(
                    M::WalkTarget,
                    MemValue::Walk(WalkTarget::entity(target, 1.2, 0)),
                );
            }
            true
        }
        Behavior::MeleeAttack => {
            let Some(target) = brain_ref(t).entity(M::AttackTarget) else {
                return false;
            };
            if !within_melee_range(t, target) || !sensing::visible_contains(t, target) {
                return false;
            }
            brain(t).set(M::LookTarget, MemValue::Look(Tracker::entity(target, true)));
            // `Warden.doHurtTarget`.
            super::entity_event(t.ents, t.env, id, 4);
            let pitch = voice_pitch(t.ents, id);
            sound(t, "minecraft:entity.warden.attack_impact", 10.0, pitch);
            set_sonic_boom_cooldown(brain(t), 40);
            combat::mob_attack(t, target);
            if t.exists()
                && let Some(br) = t.mob().brain.as_deref_mut()
            {
                br.set_with_expiry(
                    M::AttackCoolingDown,
                    MemValue::Bool(true),
                    MELEE_ATTACK_COOLDOWN,
                );
            }
            true
        }
        Behavior::GoToTargetLocation => {
            let Some(location) = brain_ref(t).pos(M::DisturbanceLocation) else {
                return false;
            };
            let here = t.block_pos();
            let close = location.dist_sqr(here) < 4.0;
            if !close {
                let r = &mut *t.env.random;
                let dx = r.next_int_bounded(3) - 1;
                let dz = r.next_int_bounded(3) - 1;
                let nearby = location.offset(dx, 0, dz);
                let tracker = Tracker::block(nearby);
                let br = brain(t);
                br.set(M::LookTarget, MemValue::Look(tracker));
                br.set(
                    M::WalkTarget,
                    MemValue::Walk(WalkTarget {
                        target: tracker,
                        speed: 0.7,
                        close_enough: 2,
                    }),
                );
            }
            true
        }
        _ => false,
    }
}

/// `SonicBoom.tick`.
fn sonic_boom_tick(t: &mut Tick<'_, '_>) {
    let id = t.id;
    let Some(target) = brain_ref(t).entity(M::AttackTarget) else {
        return;
    };
    if let Some(o) = t.other(target) {
        nav::look_at(t, o.pos);
    }
    let br = brain(t);
    if br.has(M::SonicBoomSoundDelay) || br.has(M::SonicBoomSoundCooldown) {
        return;
    }
    br.set_with_expiry(
        M::SonicBoomSoundCooldown,
        MemValue::Unit,
        i64::from(SONIC_BOOM_DURATION) - SONIC_BOOM_SOUND_DELAY,
    );
    if !can_target_entity(t.ents, Some(target)) || !closer_than(t, target, 15.0, 20.0) {
        return;
    }
    let Some(o) = t.other(target) else { return };
    // `WARDEN_CHEST` attachment: 1.6 blocks up.
    let p = t.pos();
    let source = Vec3::new(p.x, p.y + 1.6, p.z);
    let delta = Vec3::new(o.pos.x - source.x, o.eye_y - source.y, o.pos.z - source.z);
    let len = delta.distance_to_sqr(Vec3::ZERO).sqrt();
    let n = if len < 1.0e-4 {
        Vec3::ZERO
    } else {
        Vec3::new(delta.x / len, delta.y / len, delta.z / len)
    };
    // The beam: one particle per block from the chest to the target's eyes and beyond.
    #[allow(clippy::cast_possible_truncation)]
    let steps = len.floor() as i32 + 7;
    for i in 1..steps {
        let at = source + n * f64::from(i);
        let boom = crate::level::particles::simple("minecraft:sonic_boom");
        t.cx.send_particles(boom, at, 1, Vec3::ZERO, 0.0);
    }
    sound(t, "minecraft:entity.warden.sonic_boom", 3.0, 1.0);
    let damage = crate::survival::damage::DamageSource {
        kind: t.env.types.sonic_boom,
        cause: Some(id),
        direct: Some(id),
        position: Some(p),
        cause_is_mob: true,
        cause_name: Some(crate::mob::Kind::Warden.translation_key()),
        disable_shield_seconds: 0.0,
        piercing_arrow: false,
        projectile_motion: None,
        responsible_player: None,
        own_blocked_by_item: false,
    };
    let hurt = combat::hurt(t.level, t.ents, t.env, t.cx, target, damage, 10.0);
    if hurt {
        let resistance = t
            .ents
            .mob(target)
            .map_or(0.0, |m| m.attrs.knockback_resistance);
        let v = 0.5 * (1.0 - resistance);
        let h = 2.5 * (1.0 - resistance);
        let is_player = t.ents.player(target).is_some();
        if let Some(mut e) = t.ents.store.get_mut(target) {
            let vel = e.as_ref().velocity();
            e.set_velocity(Vec3::new(vel.x + n.x * h, vel.y + n.y * v, vel.z + n.z * h));
            e.set_flags(
                if is_player {
                    EntityFlags::HURT_MARKED
                } else {
                    EntityFlags::HAS_IMPULSE
                },
                true,
            );
        }
    }
}

/// `WardenAi.getActivities` on `Brain.provider(memories, sensors)`: the warden's brain.
pub fn make_brain(m: &mut MobData) {
    let core = ActivitySpec::new(
        A::Core,
        0,
        vec![
            Spec::swim(0.8),
            fam(Behavior::SetWardenLookTarget),
            Spec::look_at_target_sink(45, 90),
            Spec::move_to_target_sink(),
        ],
    );
    let emerge =
        ActivitySpec::with_memory(A::Emerge, 5, vec![fam(Behavior::Emerging)], M::IsEmerging);
    let dig = ActivitySpec::with_priorities(
        A::Dig,
        vec![
            (0, fam(Behavior::ForceUnmount)),
            (1, fam(Behavior::Digging)),
        ],
    )
    .requires(&[
        (M::RoarTarget, S::ValueAbsent),
        (M::DigCooldown, S::ValueAbsent),
    ]);
    let idle = ActivitySpec::new(
        A::Idle,
        10,
        vec![
            fam(Behavior::SetRoarTarget),
            fam(Behavior::TryToSniff),
            Spec::run_one_if(
                &[(M::IsSniffing, S::ValueAbsent)],
                vec![
                    (
                        Spec::stroll_mode(0.5, 10, 7, StrollMode::Land { from_water: true }),
                        2,
                    ),
                    (Spec::do_nothing(30, 60), 1),
                ],
            ),
        ],
    );
    let roar = ActivitySpec::with_memory(A::Roar, 10, vec![fam(Behavior::Roar)], M::RoarTarget);
    let fight = ActivitySpec::with_memory(
        A::Fight,
        10,
        vec![
            fam(Behavior::DigCooldownSetter),
            fam(Behavior::StopAttackingIfTargetInvalid),
            fam(Behavior::SetEntityLookTarget),
            fam(Behavior::SetWalkTargetFromAttackTarget),
            fam(Behavior::SonicBoom),
            fam(Behavior::MeleeAttack),
        ],
        M::AttackTarget,
    );
    let investigate = ActivitySpec::with_memory(
        A::Investigate,
        5,
        vec![
            fam(Behavior::SetRoarTarget),
            fam(Behavior::GoToTargetLocation),
        ],
        M::DisturbanceLocation,
    );
    let sniff = ActivitySpec::with_memory(
        A::Sniff,
        5,
        vec![fam(Behavior::SetRoarTarget), fam(Behavior::Sniffing)],
        M::IsSniffing,
    );
    let spec = BrainSpec {
        memories: vec![
            M::NearestVisibleNemesis,
            M::RecentProjectile,
            M::TouchCooldown,
            M::VibrationCooldown,
        ],
        sensors: vec![
            SensorKind::NearestPlayers,
            SensorKind::Fam(FamSensor::Warden(Sensor::WardenEntity)),
        ],
        activities: vec![core, emerge, dig, idle, roar, fight, investigate, sniff],
        ..BrainSpec::default()
    };
    m.brain = Some(Box::new(Brain::new(spec, &mut m.random)));
}
