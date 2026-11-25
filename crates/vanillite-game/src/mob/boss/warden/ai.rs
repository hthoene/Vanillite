//! `WardenAi`: the warden's activities, behaviors and sensors on the shared brain.

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Vec3};
use vanillite_world::entity::{Dimensions, EntityFlags, EntityId, Pose};

use super::{
    DEFAULT_ANGER, can_target_entity, entity_angry_at, increase_anger_at, play_sound,
    set_attack_target, set_dig_cooldown, set_disturbance_location, set_sonic_boom_cooldown,
    voice_pitch,
};
use crate::mob::brain::sensors::{
    find_closest_visible, sense_living_entities, sense_players, tracker_block, tracker_position,
    tracker_visible, visible_contains,
};
use crate::mob::brain::{
    Activity as A, ActivityData, Behavior, BehaviorInfo, Brain, Flavor, Gate, Memories,
    MemoryStatus as S, MemoryType as M, MemoryValue, Node, PositionTracker, Sensor, SensorInfo,
    WalkTarget,
};
use crate::mob::{Tick, combat, nav};

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

/// The warden's sensors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sense {
    /// `SensorType.NEAREST_PLAYERS`.
    NearestPlayers,
    /// `SensorType.WARDEN_ENTITY_SENSOR`.
    WardenEntity,
}

impl SensorInfo for Sense {
    fn requires(&self) -> &'static [M] {
        match self {
            Sense::NearestPlayers => &[
                M::NearestPlayers,
                M::NearestVisiblePlayer,
                M::NearestVisibleAttackablePlayer,
                M::NearestVisibleAttackablePlayers,
            ],
            Sense::WardenEntity => &[
                M::NearestLivingEntities,
                M::NearestVisibleLivingEntities,
                M::NearestAttackable,
            ],
        }
    }
}

impl Sensor<Tick<'_, '_>> for Sense {
    fn sense(&mut self, mem: &mut Memories, t: &mut Tick<'_, '_>) {
        match self {
            Sense::NearestPlayers => sense_players(t, mem),
            Sense::WardenEntity => {
                sense_living_entities(t, mem);
                // The nearest targetable player, else the nearest targetable other entity.
                let list = mem.entities(M::NearestLivingEntities);
                let pick = |player: bool| {
                    list.iter().copied().find(|&e| {
                        can_target_entity(t.ents, Some(e)) && t.ents.player(e).is_some() == player
                    })
                };
                let nearest = pick(true).or_else(|| pick(false));
                mem.set_opt(M::NearestAttackable, nearest.map(MemoryValue::Entity));
            }
        }
    }
}

/// The warden's behaviors (each variant one Vanilla behavior with its state).
#[derive(Clone, Debug, PartialEq)]
pub enum Behave {
    /// `Swim(0.8)`.
    Swim,
    /// `SetWardenLookTarget`.
    SetWardenLookTarget,
    /// `LookAtTargetSink(45, 90)`.
    LookAtTargetSink,
    /// `MoveToTargetSink()`.
    MoveToTargetSink {
        cooldown: i32,
        last_target: Option<BlockPos>,
        speed: f32,
        path: Option<Box<crate::mob::path::Path>>,
    },
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
    /// `RandomStroll.stroll(0.5)`.
    RandomStroll,
    /// `DoNothing(30, 60)`.
    DoNothing,
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

fn move_to_target_sink() -> Behave {
    Behave::MoveToTargetSink {
        cooldown: 0,
        last_target: None,
        speed: 0.0,
        path: None,
    }
}

const SET_ROAR_TARGET: &[(M, S)] = &[
    (M::RoarTarget, S::Absent),
    (M::AttackTarget, S::Absent),
    (M::CantReachWalkTargetSince, S::Registered),
];

impl BehaviorInfo for Behave {
    fn conditions(&self) -> &'static [(M, S)] {
        match self {
            Behave::Swim | Behave::ForceUnmount | Behave::DoNothing => &[],
            Behave::SetWardenLookTarget => &[
                (M::LookTarget, S::Registered),
                (M::DisturbanceLocation, S::Registered),
                (M::RoarTarget, S::Registered),
                (M::AttackTarget, S::Absent),
            ],
            Behave::LookAtTargetSink => &[(M::LookTarget, S::Present)],
            Behave::MoveToTargetSink { .. } => &[
                (M::CantReachWalkTargetSince, S::Registered),
                (M::Path, S::Absent),
                (M::WalkTarget, S::Present),
            ],
            Behave::Emerging => &[
                (M::IsEmerging, S::Present),
                (M::WalkTarget, S::Absent),
                (M::LookTarget, S::Registered),
            ],
            Behave::Digging => &[(M::AttackTarget, S::Absent), (M::WalkTarget, S::Absent)],
            Behave::SetRoarTarget => SET_ROAR_TARGET,
            Behave::TryToSniff => &[
                (M::IsSniffing, S::Registered),
                (M::WalkTarget, S::Registered),
                (M::SniffCooldown, S::Absent),
                (M::NearestAttackable, S::Present),
                (M::DisturbanceLocation, S::Absent),
            ],
            Behave::RandomStroll => &[(M::WalkTarget, S::Absent)],
            Behave::Roar => &[
                (M::RoarTarget, S::Present),
                (M::AttackTarget, S::Absent),
                (M::RoarSoundCooldown, S::Registered),
                (M::RoarSoundDelay, S::Registered),
            ],
            Behave::DigCooldownSetter => &[(M::DigCooldown, S::Registered)],
            Behave::StopAttackingIfTargetInvalid => &[
                (M::AttackTarget, S::Present),
                (M::CantReachWalkTargetSince, S::Registered),
            ],
            Behave::SetEntityLookTarget => &[
                (M::LookTarget, S::Absent),
                (M::NearestVisibleLivingEntities, S::Present),
            ],
            Behave::SetWalkTargetFromAttackTarget => &[
                (M::WalkTarget, S::Registered),
                (M::LookTarget, S::Registered),
                (M::AttackTarget, S::Present),
                (M::NearestVisibleLivingEntities, S::Registered),
            ],
            Behave::SonicBoom => &[
                (M::AttackTarget, S::Present),
                (M::SonicBoomCooldown, S::Absent),
                (M::SonicBoomSoundCooldown, S::Registered),
                (M::SonicBoomSoundDelay, S::Registered),
            ],
            Behave::MeleeAttack => &[
                (M::LookTarget, S::Registered),
                (M::AttackTarget, S::Present),
                (M::AttackCoolingDown, S::Absent),
                (M::NearestVisibleLivingEntities, S::Present),
            ],
            Behave::GoToTargetLocation => &[
                (M::DisturbanceLocation, S::Present),
                (M::AttackTarget, S::Absent),
                (M::WalkTarget, S::Absent),
                (M::LookTarget, S::Registered),
            ],
            Behave::Sniffing => &[
                (M::IsSniffing, S::Present),
                (M::AttackTarget, S::Absent),
                (M::WalkTarget, S::Absent),
                (M::LookTarget, S::Registered),
                (M::NearestAttackable, S::Registered),
                (M::DisturbanceLocation, S::Registered),
                (M::SniffCooldown, S::Registered),
            ],
        }
    }

    fn flavor(&self) -> Flavor {
        let timed = |d: i32| Flavor::Timed { min: d, max: d };
        match self {
            Behave::Swim | Behave::ForceUnmount => timed(60),
            Behave::LookAtTargetSink => Flavor::Timed { min: 45, max: 90 },
            Behave::MoveToTargetSink { .. } => Flavor::Timed { min: 150, max: 250 },
            Behave::Emerging => timed(EMERGE_DURATION),
            Behave::Digging => timed(DIGGING_DURATION),
            Behave::DoNothing => Flavor::Timed { min: 30, max: 60 },
            Behave::Roar => timed(ROAR_DURATION),
            Behave::SonicBoom => timed(SONIC_BOOM_DURATION),
            Behave::Sniffing => timed(SNIFFING_DURATION),
            Behave::SetWardenLookTarget
            | Behave::SetRoarTarget
            | Behave::TryToSniff
            | Behave::RandomStroll
            | Behave::DigCooldownSetter
            | Behave::StopAttackingIfTargetInvalid
            | Behave::SetEntityLookTarget
            | Behave::SetWalkTargetFromAttackTarget
            | Behave::MeleeAttack
            | Behave::GoToTargetLocation => Flavor::OneShot,
        }
    }
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

/// `PathNavigation.createPath(pos, 0)`.
fn compute_path(t: &mut Tick<'_, '_>, target: BlockPos) -> Option<crate::mob::path::Path> {
    nav::create_path_to(t, target, 0)
}

impl Behavior<Tick<'_, '_>> for Behave {
    fn check_extra(&mut self, mem: &mut Memories, t: &mut Tick<'_, '_>) -> bool {
        match self {
            Behave::Swim => swim_condition(t),
            Behave::ForceUnmount => false,
            Behave::Digging => {
                t.on_ground() || {
                    let f = t.mob_ref().fluids;
                    f.in_water() || f.in_lava()
                }
            }
            Behave::SonicBoom => mem
                .entity(M::AttackTarget)
                .is_some_and(|target| closer_than(t, target, 15.0, 20.0)),
            Behave::MoveToTargetSink {
                cooldown,
                last_target,
                speed,
                path,
            } => {
                if *cooldown > 0 {
                    *cooldown -= 1;
                    return false;
                }
                let Some(walk) = mem.walk_target() else {
                    return false;
                };
                let reached = reached_target(t, walk);
                let time = t.env.game_time;
                if !reached && try_compute_path(t, mem, walk, time, speed, path) {
                    *last_target = tracker_block(t, walk.target);
                    true
                } else {
                    mem.erase(M::WalkTarget);
                    if reached {
                        mem.erase(M::CantReachWalkTargetSince);
                    }
                    false
                }
            }
            _ => true,
        }
    }

    fn start(&mut self, mem: &mut Memories, t: &mut Tick<'_, '_>, time: i64) -> bool {
        let id = t.id;
        match self {
            // Timed behaviors (the return value is ignored).
            Behave::Swim | Behave::LookAtTargetSink | Behave::ForceUnmount | Behave::DoNothing => {
                true
            }
            Behave::MoveToTargetSink { speed, path, .. } => {
                mem.set(M::Path, MemoryValue::Unit);
                let p = path.as_deref().cloned();
                nav::move_to_path(t, p, f64::from(*speed));
                true
            }
            Behave::Emerging => {
                set_pose(t, Pose::Emerging);
                sound(t, "minecraft:entity.warden.emerge", 5.0, 1.0);
                true
            }
            Behave::Digging => {
                if t.on_ground() {
                    set_pose(t, Pose::Digging);
                    sound(t, "minecraft:entity.warden.dig", 5.0, 1.0);
                } else {
                    sound(t, "minecraft:entity.warden.agitated", 5.0, 1.0);
                    self.stop(mem, t, time);
                }
                true
            }
            Behave::Roar => {
                mem.set_with_expiry(M::RoarSoundDelay, MemoryValue::Unit, 25);
                mem.erase(M::WalkTarget);
                if let Some(target) = mem.entity(M::RoarTarget) {
                    mem.set(
                        M::LookTarget,
                        MemoryValue::Tracker(PositionTracker::Entity {
                            id: target,
                            eye: true,
                        }),
                    );
                    set_pose(t, Pose::Roaring);
                    let Tick { ents, env, .. } = t;
                    increase_anger_at(ents, env, id, mem, Some(target), 20, false);
                }
                true
            }
            Behave::SonicBoom => {
                mem.set_with_expiry(
                    M::AttackCoolingDown,
                    MemoryValue::Bool(true),
                    i64::from(SONIC_BOOM_DURATION),
                );
                mem.set_with_expiry(
                    M::SonicBoomSoundDelay,
                    MemoryValue::Unit,
                    SONIC_BOOM_SOUND_DELAY,
                );
                super::entity_event(t.ents, t.env, id, 62);
                sound(t, "minecraft:entity.warden.sonic_charge", 3.0, 1.0);
                true
            }
            Behave::Sniffing => {
                sound(t, "minecraft:entity.warden.sniff", 5.0, 1.0);
                true
            }
            // One shots: the trigger.
            Behave::SetWardenLookTarget => {
                let target = mem
                    .entity(M::RoarTarget)
                    .and_then(|e| block_pos_of(t, e))
                    .or_else(|| mem.pos(M::DisturbanceLocation));
                match target {
                    Some(p) => {
                        mem.set(
                            M::LookTarget,
                            MemoryValue::Tracker(PositionTracker::Block(p)),
                        );
                        true
                    }
                    None => false,
                }
            }
            Behave::SetRoarTarget => {
                let angry = entity_angry_at(t.ents, id, mem);
                match angry.filter(|&e| can_target_entity(t.ents, Some(e))) {
                    Some(e) => {
                        mem.set(M::RoarTarget, MemoryValue::Entity(e));
                        mem.erase(M::CantReachWalkTargetSince);
                        true
                    }
                    None => false,
                }
            }
            Behave::TryToSniff => {
                mem.set(M::IsSniffing, MemoryValue::Unit);
                let cooldown = 100 + t.env.random.next_int_bounded(101);
                mem.set_with_expiry(M::SniffCooldown, MemoryValue::Unit, i64::from(cooldown));
                mem.erase(M::WalkTarget);
                set_pose(t, Pose::Sniffing);
                true
            }
            Behave::RandomStroll => {
                let pos = crate::mob::goals::land_random_pos(t, 10, 7);
                mem.set_opt(
                    M::WalkTarget,
                    pos.map(|p| {
                        MemoryValue::Walk(WalkTarget {
                            target: PositionTracker::Block(BlockPos::containing(p.x, p.y, p.z)),
                            speed: 0.5,
                            close_enough: 0,
                        })
                    }),
                );
                true
            }
            Behave::DigCooldownSetter => {
                set_dig_cooldown(mem);
                true
            }
            Behave::StopAttackingIfTargetInvalid => {
                let Some(target) = mem.entity(M::AttackTarget) else {
                    return false;
                };
                let valid = can_target_entity(t.ents, Some(target));
                let angry = super::anger_level(t.ents, id, mem).is_angry();
                if valid && angry {
                    return true;
                }
                // `WardenAi.onTargetInvalid`.
                if !valid && let Some(s) = super::state_mut(t.ents, id) {
                    s.anger.clear_anger(target);
                }
                set_dig_cooldown(mem);
                mem.erase(M::AttackTarget);
                true
            }
            Behave::SetEntityLookTarget => {
                let Some(attack) = mem.entity(M::AttackTarget) else {
                    return false;
                };
                let range = t.mob_ref().attrs.follow_range as f32;
                let max = f64::from(range * range);
                let me = t.pos();
                let found = find_closest_visible(t, mem, |t, e| {
                    e == attack
                        && t.ents
                            .store
                            .get(e)
                            .is_some_and(|x| x.pos().distance_to_sqr(me) <= max)
                });
                match found {
                    Some(e) => {
                        mem.set(
                            M::LookTarget,
                            MemoryValue::Tracker(PositionTracker::Entity { id: e, eye: true }),
                        );
                        true
                    }
                    None => false,
                }
            }
            Behave::SetWalkTargetFromAttackTarget => {
                let Some(target) = mem.entity(M::AttackTarget) else {
                    return false;
                };
                if visible_contains(t, mem, target) && within_melee_range(t, target) {
                    mem.erase(M::WalkTarget);
                } else {
                    mem.set(
                        M::LookTarget,
                        MemoryValue::Tracker(PositionTracker::Entity {
                            id: target,
                            eye: true,
                        }),
                    );
                    mem.set(
                        M::WalkTarget,
                        MemoryValue::Walk(WalkTarget {
                            target: PositionTracker::Entity {
                                id: target,
                                eye: false,
                            },
                            speed: 1.2,
                            close_enough: 0,
                        }),
                    );
                }
                true
            }
            Behave::MeleeAttack => {
                let Some(target) = mem.entity(M::AttackTarget) else {
                    return false;
                };
                if !within_melee_range(t, target) || !visible_contains(t, mem, target) {
                    return false;
                }
                mem.set(
                    M::LookTarget,
                    MemoryValue::Tracker(PositionTracker::Entity {
                        id: target,
                        eye: true,
                    }),
                );
                // `Warden.doHurtTarget`.
                super::entity_event(t.ents, t.env, id, 4);
                let pitch = voice_pitch(t.ents, id);
                sound(t, "minecraft:entity.warden.attack_impact", 10.0, pitch);
                set_sonic_boom_cooldown(mem, 40);
                combat::mob_attack(t, target);
                mem.set_with_expiry(
                    M::AttackCoolingDown,
                    MemoryValue::Bool(true),
                    MELEE_ATTACK_COOLDOWN,
                );
                true
            }
            Behave::GoToTargetLocation => {
                let Some(location) = mem.pos(M::DisturbanceLocation) else {
                    return false;
                };
                let here = t.block_pos();
                let close = location.dist_sqr(here) < 4.0;
                if !close {
                    let r = &mut *t.env.random;
                    let dx = r.next_int_bounded(3) - 1;
                    let dz = r.next_int_bounded(3) - 1;
                    let nearby = location.offset(dx, 0, dz);
                    let tracker = PositionTracker::Block(nearby);
                    mem.set(M::LookTarget, MemoryValue::Tracker(tracker));
                    mem.set(
                        M::WalkTarget,
                        MemoryValue::Walk(WalkTarget {
                            target: tracker,
                            speed: 0.7,
                            close_enough: 2,
                        }),
                    );
                }
                true
            }
        }
    }

    fn can_still_use(&mut self, mem: &mut Memories, t: &mut Tick<'_, '_>, _time: i64) -> bool {
        match self {
            Behave::Swim => swim_condition(t),
            Behave::LookAtTargetSink => mem
                .tracker(M::LookTarget)
                .is_some_and(|tr| tracker_visible(t, mem, tr)),
            Behave::MoveToTargetSink {
                last_target, path, ..
            } => {
                if path.is_none() || last_target.is_none() {
                    return false;
                }
                let Some(walk) = mem.walk_target() else {
                    return false;
                };
                let spectator = match walk.target {
                    PositionTracker::Entity { id, .. } => t
                        .ents
                        .player(id)
                        .is_some_and(|p| p.game_mode == vanillite_config::GameMode::Spectator),
                    PositionTracker::Block(_) => false,
                };
                !t.mob_ref().nav.is_done() && !reached_target(t, walk) && !spectator
            }
            Behave::Digging => !t.mob_ref().removed,
            Behave::Emerging
            | Behave::DoNothing
            | Behave::Roar
            | Behave::SonicBoom
            | Behave::Sniffing => true,
            _ => false,
        }
    }

    fn tick(&mut self, mem: &mut Memories, t: &mut Tick<'_, '_>, time: i64) {
        let id = t.id;
        match self {
            Behave::Swim => {
                if t.mob().random.next_float() < 0.8 {
                    t.mob().jump = true;
                }
            }
            Behave::LookAtTargetSink => {
                if let Some(tr) = mem.tracker(M::LookTarget)
                    && let Some(at) = tracker_position(t, tr)
                {
                    nav::look_at(t, at);
                }
            }
            Behave::MoveToTargetSink {
                last_target,
                speed,
                path,
                ..
            } => {
                let current_path = t.mob_ref().nav.path.as_ref();
                if path.as_deref() != current_path {
                    *path = current_path.cloned().map(Box::new);
                    mem.set_opt(M::Path, path.as_ref().map(|_| MemoryValue::Unit));
                }
                if path.is_some()
                    && let Some(last) = *last_target
                    && let Some(walk) = mem.walk_target()
                    && let Some(current) = tracker_block(t, walk.target)
                    && current.dist_sqr(last) > 4.0
                    && try_compute_path(t, mem, walk, time, speed, path)
                {
                    *last_target = Some(current);
                    mem.set(M::Path, MemoryValue::Unit);
                    let p = path.as_deref().cloned();
                    nav::move_to_path(t, p, f64::from(*speed));
                }
            }
            Behave::Roar => {
                if !mem.has(M::RoarSoundDelay) && !mem.has(M::RoarSoundCooldown) {
                    mem.set_with_expiry(
                        M::RoarSoundCooldown,
                        MemoryValue::Unit,
                        i64::from(ROAR_DURATION - 25),
                    );
                    sound(t, "minecraft:entity.warden.roar", 3.0, 1.0);
                }
            }
            Behave::SonicBoom => sonic_boom_tick(t, mem, id),
            _ => {}
        }
    }

    fn stop(&mut self, mem: &mut Memories, t: &mut Tick<'_, '_>, _time: i64) {
        let id = t.id;
        match self {
            Behave::LookAtTargetSink => mem.erase(M::LookTarget),
            Behave::MoveToTargetSink { cooldown, path, .. } => {
                if let Some(walk) = mem.walk_target()
                    && !reached_target(t, walk)
                    && t.mob_ref().nav.is_stuck
                {
                    *cooldown = t.env.random.next_int_bounded(40);
                }
                t.mob().nav.stop();
                mem.erase(M::WalkTarget);
                mem.erase(M::Path);
                *path = None;
            }
            Behave::Emerging => {
                if pose(t) == Pose::Emerging {
                    set_pose(t, Pose::Standing);
                }
            }
            Behave::Digging => {
                if !t.mob_ref().removed {
                    t.mob().removed = true;
                }
            }
            Behave::Roar => {
                if pose(t) == Pose::Roaring {
                    set_pose(t, Pose::Standing);
                }
                if let Some(target) = mem.entity(M::RoarTarget) {
                    set_attack_target(mem, target);
                }
                mem.erase(M::RoarTarget);
            }
            Behave::SonicBoom => set_sonic_boom_cooldown(mem, 40),
            Behave::Sniffing => {
                if pose(t) == Pose::Sniffing {
                    set_pose(t, Pose::Standing);
                }
                mem.erase(M::IsSniffing);
                if let Some(e) = mem.entity(M::NearestAttackable)
                    && can_target_entity(t.ents, Some(e))
                {
                    if closer_than(t, e, 6.0, 20.0) {
                        let Tick { ents, env, .. } = t;
                        increase_anger_at(ents, env, id, mem, Some(e), DEFAULT_ANGER, true);
                    }
                    if !mem.has(M::DisturbanceLocation)
                        && let Some(p) = block_pos_of(t, e)
                    {
                        set_disturbance_location(t.ents, id, mem, p);
                    }
                }
            }
            _ => {}
        }
    }
}

/// `Swim.checkExtraStartConditions`: deeper in water than the jump threshold, or in lava.
fn swim_condition(t: &Tick<'_, '_>) -> bool {
    let f = t.mob_ref().fluids;
    let eye = crate::mob::eye_height(t.mob_ref());
    let threshold = if eye < 0.4 { 0.0 } else { 0.4 };
    f.in_water() && f.water_height > threshold || f.in_lava()
}

/// `MoveToTargetSink.reachedTarget`.
fn reached_target(t: &Tick<'_, '_>, walk: WalkTarget) -> bool {
    let Some(target) = tracker_block(t, walk.target) else {
        return false;
    };
    let here = t.block_pos();
    let manhattan =
        (target.x - here.x).abs() + (target.y - here.y).abs() + (target.z - here.z).abs();
    manhattan <= walk.close_enough
}

/// `MoveToTargetSink.tryComputePath` (the partial step towards an unreachable target is not
/// tried).
fn try_compute_path(
    t: &mut Tick<'_, '_>,
    mem: &mut Memories,
    walk: WalkTarget,
    time: i64,
    speed: &mut f32,
    path: &mut Option<Box<crate::mob::path::Path>>,
) -> bool {
    let Some(target) = tracker_block(t, walk.target) else {
        return false;
    };
    *path = compute_path(t, target).map(Box::new);
    *speed = walk.speed;
    if reached_target(t, walk) {
        mem.erase(M::CantReachWalkTargetSince);
        return false;
    }
    if path.as_ref().is_some_and(|p| p.reached) {
        mem.erase(M::CantReachWalkTargetSince);
    } else if !mem.has(M::CantReachWalkTargetSince) {
        mem.set(M::CantReachWalkTargetSince, MemoryValue::Long(time));
    }
    path.is_some()
}

/// `SonicBoom.tick`.
fn sonic_boom_tick(t: &mut Tick<'_, '_>, mem: &mut Memories, id: EntityId) {
    let Some(target) = mem.entity(M::AttackTarget) else {
        return;
    };
    if let Some(o) = t.other(target) {
        nav::look_at(t, o.pos);
    }
    if mem.has(M::SonicBoomSoundDelay) || mem.has(M::SonicBoomSoundCooldown) {
        return;
    }
    mem.set_with_expiry(
        M::SonicBoomSoundCooldown,
        MemoryValue::Unit,
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
    sound(t, "minecraft:entity.warden.sonic_boom", 3.0, 1.0);
    let damage = crate::survival::damage::DamageSource {
        kind: t.env.types.sonic_boom,
        cause: Some(id),
        direct: Some(id),
        position: Some(p),
        cause_is_mob: true,
        cause_name: Some(crate::mob::Kind::Warden.translation_key()),
        disable_shield_seconds: 0.0,
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

/// `WardenAi.getActivities` on `Brain.provider(memories, sensors)`.
pub fn make_brain(random_int: impl FnMut(i32) -> i32) -> Brain<Behave, Sense> {
    let leaf = Node::leaf;
    let core = ActivityData::create(
        A::Core,
        0,
        vec![
            leaf(Behave::Swim),
            leaf(Behave::SetWardenLookTarget),
            leaf(Behave::LookAtTargetSink),
            leaf(move_to_target_sink()),
        ],
        &[],
    );
    let emerge = ActivityData::create(
        A::Emerge,
        5,
        vec![leaf(Behave::Emerging)],
        &[(M::IsEmerging, S::Present)],
    )
    .erase(&[M::IsEmerging]);
    let dig = ActivityData {
        activity: A::Dig,
        behaviors: vec![(0, leaf(Behave::ForceUnmount)), (1, leaf(Behave::Digging))],
        requirements: &[(M::RoarTarget, S::Absent), (M::DigCooldown, S::Absent)],
        erase_on_stop: &[],
    };
    let idle = ActivityData::create(
        A::Idle,
        10,
        vec![
            leaf(Behave::SetRoarTarget),
            leaf(Behave::TryToSniff),
            Node::Gate(Box::new(Gate::run_one(
                &[(M::IsSniffing, S::Absent)],
                vec![
                    (leaf(Behave::RandomStroll), 2),
                    (leaf(Behave::DoNothing), 1),
                ],
            ))),
        ],
        &[],
    );
    let roar = ActivityData::create(
        A::Roar,
        10,
        vec![leaf(Behave::Roar)],
        &[(M::RoarTarget, S::Present)],
    )
    .erase(&[M::RoarTarget]);
    let fight = ActivityData::create(
        A::Fight,
        10,
        vec![
            leaf(Behave::DigCooldownSetter),
            leaf(Behave::StopAttackingIfTargetInvalid),
            leaf(Behave::SetEntityLookTarget),
            leaf(Behave::SetWalkTargetFromAttackTarget),
            leaf(Behave::SonicBoom),
            leaf(Behave::MeleeAttack),
        ],
        &[(M::AttackTarget, S::Present)],
    )
    .erase(&[M::AttackTarget]);
    let investigate = ActivityData::create(
        A::Investigate,
        5,
        vec![
            leaf(Behave::SetRoarTarget),
            leaf(Behave::GoToTargetLocation),
        ],
        &[(M::DisturbanceLocation, S::Present)],
    )
    .erase(&[M::DisturbanceLocation]);
    let sniff = ActivityData::create(
        A::Sniff,
        5,
        vec![leaf(Behave::SetRoarTarget), leaf(Behave::Sniffing)],
        &[(M::IsSniffing, S::Present)],
    )
    .erase(&[M::IsSniffing]);
    Brain::build(
        &[
            M::NearestVisibleNemesis,
            M::RecentProjectile,
            M::TouchCooldown,
            M::VibrationCooldown,
        ],
        vec![Sense::NearestPlayers, Sense::WardenEntity],
        vec![core, emerge, dig, idle, roar, fight, investigate, sniff],
        random_int,
    )
}
