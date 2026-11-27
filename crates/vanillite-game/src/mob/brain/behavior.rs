//! Behaviours (`BehaviorControl`): timed behaviours (`Behavior` with entry conditions and a
//! random duration), one shot triggers (`OneShot`, the declarative `BehaviorBuilder` ones),
//! gates (`GateBehavior`, `RunOne`, `TriggerGate`) and the shared Vanilla behaviours. Family
//! behaviours plug in as [`Behavior::Fam`].

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Vec3};
use vanillite_config::GameMode;
use vanillite_protocol::SharedPacket;
use vanillite_protocol::packets::play::clientbound::Animate;
use vanillite_world::entity::EntityId;

use super::memory::MemoryStatus as S;
use super::memory::MemoryType as M;
use super::{
    Brain, MemValue, MemoryStatus, MemoryType, Tick, Tracker, WalkTarget, brain, brain_ref, sensing,
};
use crate::mob::{Kind, nav, randompos};
use crate::survival::Effect;
use crate::survival::damage::tag;

/// `Behavior.Status`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Stopped,
    Running,
}

/// A condition on the ticking mob.
pub type Cond = fn(&mut Tick<'_, '_>) -> bool;
/// A condition on another entity.
pub type EntityPred = fn(&Tick<'_, '_>, EntityId) -> bool;
/// Finds a target for the ticking mob.
pub type Finder = fn(&mut Tick<'_, '_>) -> Option<EntityId>;

/// Where a stroll goes (`RandomStroll.stroll`, `swim`, `fly`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StrollMode {
    /// `LandRandomPos.getPos(h, v)`; `from_water` false skips it in water.
    Land { from_water: bool },
    /// `getTargetSwimPos`, only in water.
    Swim,
    /// `AirAndWaterRandomPos` towards the view direction.
    Fly,
}

/// Where a panicking animal flees (`AnimalPanic` position getter).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanicMode {
    /// `LandRandomPos.getPos(5, 4)`.
    Land,
    /// `AirAndWaterRandomPos.getPos(5, 4, flyHeight, ...)`.
    Fly(i32),
}

/// A behaviour with its state.
#[derive(Clone, Copy, Debug)]
pub enum Behavior {
    /// `GateBehavior(entry, exitErased, order, running)` and `RunOne` (shuffled, run one).
    Gate {
        entry: &'static [(MemoryType, MemoryStatus)],
        exit: &'static [MemoryType],
        shuffled: bool,
        run_one: bool,
    },
    /// `TriggerGate.triggerGate`: a one shot that triggers its children.
    TriggerGate { shuffled: bool, run_one: bool },
    /// `BehaviorBuilder.triggerIf(predicate, behaviour)`: the child triggers when it holds.
    TriggerIf(Cond),
    /// `BehaviorBuilder.triggerIf(predicate)` alone.
    Check(Cond),
    /// `BehaviorBuilder.sequence(first, second)`: the children trigger in order.
    Sequence,
    /// `Swim(chance)`.
    Swim { chance: f32 },
    /// `LookAtTargetSink(min, max)`.
    LookAtTargetSink,
    /// `MoveToTargetSink(min, max)`.
    MoveToTargetSink {
        cooldown: i32,
        has_path: bool,
        last_target: Option<BlockPos>,
        speed: f32,
    },
    /// `CountDownCooldownTicks(memory)`.
    CountDownCooldown(MemoryType),
    /// `StartAttacking.create(condition, finder)`.
    StartAttacking { cond: Option<Cond>, finder: Finder },
    /// `StopAttackingIfTargetInvalid.create(stopWhen, onErased, tired)`.
    StopAttackingIfTargetInvalid {
        stop_when: Option<fn(&mut Tick<'_, '_>, EntityId) -> bool>,
        on_erased: Option<fn(&mut Tick<'_, '_>, EntityId)>,
        tired: bool,
    },
    /// `SetWalkTargetFromAttackTargetIfTargetOutOfReach(speed)`.
    WalkToAttackTarget {
        speed: f32,
        speed_fn: Option<fn(&Tick<'_, '_>) -> f32>,
    },
    /// `MeleeAttack.create(condition, cooldown)`.
    MeleeAttack { cond: Option<Cond>, cooldown: i32 },
    /// `SetEntityLookTarget.create(predicate, maxDist)`.
    LookAtEntity {
        filter: Option<EntityPred>,
        max_dist: f32,
    },
    /// `SetEntityLookTargetSometimes.create(maxDist, interval, predicate)`.
    LookAtEntitySometimes {
        filter: Option<EntityPred>,
        max_dist: f32,
        min: i32,
        max: i32,
        ticks: i32,
    },
    /// `RandomStroll`.
    Stroll {
        speed: f32,
        h: i32,
        v: i32,
        mode: StrollMode,
    },
    /// `SetWalkTargetFromLookTarget.create(condition, speed, closeEnough)`.
    WalkToLookTarget {
        cond: Option<Cond>,
        speed: f32,
        close: i32,
        speed_fn: Option<fn(&Tick<'_, '_>) -> f32>,
    },
    /// `DoNothing(min, max)`.
    DoNothing,
    /// `AnimalPanic(speed, ...)`.
    AnimalPanic {
        speed: f32,
        mode: PanicMode,
        /// The damage type tag bits that cause panic.
        causes: u32,
        /// Run before `start` (`ArmadilloPanic`, the sniffer's reset).
        on_start: Option<fn(&mut Tick<'_, '_>)>,
    },
    /// `AnimalMakeLove(partner, speed, closeEnough)`.
    AnimalMakeLove {
        partner: Kind,
        speed: f32,
        close: i32,
        spawn_at: i64,
        on_start: Option<fn(&mut Tick<'_, '_>)>,
    },
    /// `FollowTemptation(speed, closeEnough, lookInTheEyes)`.
    FollowTemptation {
        speed: fn(&Tick<'_, '_>) -> f32,
        close: f64,
        look_eyes: bool,
        close_fn: Option<fn(&Tick<'_, '_>) -> f64>,
        on_start: Option<fn(&mut Tick<'_, '_>)>,
    },
    /// `BabyFollowAdult.create(range, speed, memory, targetEye)`.
    BabyFollowAdult {
        min: i32,
        max: i32,
        speed: f32,
        memory: MemoryType,
        target_eye: bool,
        speed_fn: Option<fn(&Tick<'_, '_>) -> f32>,
    },
    /// `BecomePassiveIfMemoryPresent.create(memory, duration)`.
    BecomePassive { memory: MemoryType, duration: i32 },
    /// `EraseMemoryIf.create(predicate, memory)`.
    EraseMemoryIf { cond: Cond, memory: MemoryType },
    /// `CopyMemoryWithExpiry.create(predicate, source, target, duration)`.
    CopyMemoryWithExpiry {
        cond: Cond,
        source: MemoryType,
        target: MemoryType,
        min: i32,
        max: i32,
    },
    /// `GoToWantedItem.create(predicate, speed, interrupt, maxDist)`.
    GoToWantedItem {
        cond: Option<Cond>,
        speed: f32,
        interrupt: bool,
        max_dist: i32,
    },
    /// `SetWalkTargetAwayFrom.pos/entity(memory, speed, distance, interrupt)`.
    WalkAwayFrom {
        memory: MemoryType,
        speed: f32,
        distance: i32,
        interrupt: bool,
    },
    /// `StopBeingAngryIfTargetDead`.
    StopBeingAngryIfTargetDead,
    /// `TryFindLand.create(range, speed)`.
    TryFindLand {
        range: i32,
        speed: f32,
        next_ok: i64,
    },
    /// `TryFindLiquid.create(range, speed, WATER)`.
    TryFindWater {
        range: i32,
        speed: f32,
        next_ok: i64,
    },
    /// `RandomLookAround(interval, maxYaw, minPitch, maxPitch)`.
    RandomLookAround {
        min: i32,
        max: i32,
        max_yaw: f32,
        min_pitch: f32,
        max_pitch: f32,
    },
    /// `StrollToPoi.create(memory, speed, closeEnough, maxDistance)`.
    StrollToPoi {
        memory: MemoryType,
        speed: f32,
        close: i32,
        max_dist: i32,
        next_ok: i64,
    },
    /// `StrollAroundPoi.create(memory, speed, maxDistance)`.
    StrollAroundPoi {
        memory: MemoryType,
        speed: f32,
        max_dist: i32,
        next_ok: i64,
    },
    /// `InteractWith.of(type, range, memory, speed, stopDistance)`.
    InteractWith {
        filter: EntityPred,
        range: i32,
        memory: MemoryType,
        speed: f32,
        stop: i32,
    },
    /// `SetLookAndInteract.create(type, range)`.
    SetLookAndInteract { filter: EntityPred, range: i32 },
    /// `GoToTargetLocation.create(memory, closeEnough, speed)`.
    GoToTargetLocation {
        memory: MemoryType,
        close: i32,
        speed: f32,
    },
    /// `BackUpIfTooClose.create(distance, strafeSpeed)`.
    BackUpIfTooClose { distance: i32, speed: f32 },
    /// `Mount.create(speed)`.
    Mount { speed: f32 },
    /// `DismountOrSkipMounting.create(maxDist, dontRideIf)`.
    Dismount {
        max_dist: i32,
        dont_ride_if: fn(&mut Tick<'_, '_>, EntityId) -> bool,
    },
    /// A family behaviour.
    Fam(FamBehavior),
}

/// A family behaviour.
#[derive(Clone, Copy, Debug)]
pub enum FamBehavior {
    Brainy(crate::mob::fam::brainy::Behavior),
    Mount(crate::mob::fam::mount::Behavior),
    Village(crate::mob::fam::village::Behavior),
    Warden(crate::mob::boss::warden::ai::Behavior),
}

/// How a behaviour runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// `OneShot`: triggers once, stops on the next tick.
    OneShot,
    /// `Behavior`: entry conditions, a random duration, start/tick/stop.
    Timed,
    /// `DoNothing`.
    Idle,
    /// `GateBehavior`.
    Gate,
}

/// A behaviour to add to a brain: the value, its duration and its children.
#[derive(Debug)]
pub struct Spec {
    behavior: Behavior,
    min: i32,
    max: i32,
    children: Vec<(Spec, i32)>,
}

impl Spec {
    /// A behaviour with Vanilla's default duration (60 ticks).
    pub fn of(behavior: Behavior) -> Spec {
        Spec {
            behavior,
            min: 60,
            max: 60,
            children: Vec::new(),
        }
    }

    /// A timed behaviour with `minDuration` and `maxDuration`.
    pub fn timed(behavior: Behavior, min: i32, max: i32) -> Spec {
        Spec {
            behavior,
            min,
            max,
            children: Vec::new(),
        }
    }

    /// The behaviour value, to adjust its parameters.
    pub fn behavior_mut(&mut self) -> Option<&mut Behavior> {
        Some(&mut self.behavior)
    }

    /// Sets the duration of a timed behaviour.
    pub fn set_duration(&mut self, min: i32, max: i32) {
        self.min = min;
        self.max = max;
    }

    /// Sets the start hook of `AnimalPanic`, `AnimalMakeLove` or `FollowTemptation`.
    pub fn set_on_start(&mut self, f: fn(&mut Tick<'_, '_>)) {
        match &mut self.behavior {
            Behavior::AnimalPanic { on_start, .. }
            | Behavior::AnimalMakeLove { on_start, .. }
            | Behavior::FollowTemptation { on_start, .. } => *on_start = Some(f),
            _ => {}
        }
    }

    /// Sets the close enough distance function of `FollowTemptation`.
    pub fn set_close_fn(&mut self, f: fn(&Tick<'_, '_>) -> f64) {
        if let Behavior::FollowTemptation { close_fn, .. } = &mut self.behavior {
            *close_fn = Some(f);
        }
    }

    /// Sets the speed function of the walking behaviours.
    pub fn set_speed_fn(&mut self, f: fn(&Tick<'_, '_>) -> f32) {
        match &mut self.behavior {
            Behavior::WalkToAttackTarget { speed_fn, .. }
            | Behavior::WalkToLookTarget { speed_fn, .. }
            | Behavior::BabyFollowAdult { speed_fn, .. } => *speed_fn = Some(f),
            _ => {}
        }
    }

    pub(super) fn into_parts(self) -> (Behavior, i32, i32, Vec<(Spec, i32)>) {
        (self.behavior, self.min, self.max, self.children)
    }

    /// `RunOne(weighted)`: shuffled, the first that starts runs.
    pub fn run_one(children: Vec<(Spec, i32)>) -> Spec {
        Spec::gate(&[], &[], true, true, children)
    }

    /// `RunOne(entry, weighted)`.
    pub fn run_one_if(
        entry: &'static [(MemoryType, MemoryStatus)],
        children: Vec<(Spec, i32)>,
    ) -> Spec {
        Spec::gate(entry, &[], true, true, children)
    }

    /// `GateBehavior(entry, exitErased, order, running, behaviours)`.
    pub fn gate(
        entry: &'static [(MemoryType, MemoryStatus)],
        exit: &'static [MemoryType],
        shuffled: bool,
        run_one: bool,
        children: Vec<(Spec, i32)>,
    ) -> Spec {
        Spec {
            behavior: Behavior::Gate {
                entry,
                exit,
                shuffled,
                run_one,
            },
            min: 60,
            max: 60,
            children,
        }
    }

    /// `TriggerGate.triggerOneShuffled(weighted)`.
    pub fn trigger_one_shuffled(children: Vec<(Spec, i32)>) -> Spec {
        Spec {
            behavior: Behavior::TriggerGate {
                shuffled: true,
                run_one: true,
            },
            min: 60,
            max: 60,
            children,
        }
    }

    /// `BehaviorBuilder.triggerIf(predicate, behaviour)`.
    pub fn trigger_if(cond: Cond, inner: Spec) -> Spec {
        Spec {
            behavior: Behavior::TriggerIf(cond),
            min: 60,
            max: 60,
            children: vec![(inner, 1)],
        }
    }

    /// `BehaviorBuilder.sequence(first, second)`.
    pub fn sequence(first: Spec, second: Spec) -> Spec {
        Spec {
            behavior: Behavior::Sequence,
            min: 60,
            max: 60,
            children: vec![(first, 1), (second, 1)],
        }
    }

    // Constructors of the shared behaviours with Vanilla's parameters.

    pub fn swim(chance: f32) -> Spec {
        Spec::of(Behavior::Swim { chance })
    }
    pub fn look_at_target_sink(min: i32, max: i32) -> Spec {
        Spec::timed(Behavior::LookAtTargetSink, min, max)
    }
    pub fn move_to_target_sink() -> Spec {
        Spec::move_to_target_sink_for(150, 250)
    }
    pub fn move_to_target_sink_for(min: i32, max: i32) -> Spec {
        Spec::timed(
            Behavior::MoveToTargetSink {
                cooldown: 0,
                has_path: false,
                last_target: None,
                speed: 0.0,
            },
            min,
            max,
        )
    }
    pub fn count_down(memory: MemoryType) -> Spec {
        Spec::of(Behavior::CountDownCooldown(memory))
    }
    pub fn start_attacking(cond: Option<Cond>, finder: Finder) -> Spec {
        Spec::of(Behavior::StartAttacking { cond, finder })
    }
    pub fn stop_attacking_if_invalid() -> Spec {
        Spec::of(Behavior::StopAttackingIfTargetInvalid {
            stop_when: None,
            on_erased: None,
            tired: true,
        })
    }
    pub fn walk_to_attack_target(speed: f32) -> Spec {
        Spec::of(Behavior::WalkToAttackTarget {
            speed,
            speed_fn: None,
        })
    }
    pub fn melee(cooldown: i32) -> Spec {
        Spec::of(Behavior::MeleeAttack {
            cond: None,
            cooldown,
        })
    }
    pub fn look_at_entity(filter: Option<EntityPred>, max_dist: f32) -> Spec {
        Spec::of(Behavior::LookAtEntity { filter, max_dist })
    }
    pub fn look_at_entity_sometimes(
        filter: Option<EntityPred>,
        max_dist: f32,
        min: i32,
        max: i32,
    ) -> Spec {
        Spec::of(Behavior::LookAtEntitySometimes {
            filter,
            max_dist,
            min,
            max,
            ticks: 0,
        })
    }
    pub fn stroll(speed: f32) -> Spec {
        Spec::of(Behavior::Stroll {
            speed,
            h: 10,
            v: 7,
            mode: StrollMode::Land { from_water: true },
        })
    }
    pub fn stroll_mode(speed: f32, h: i32, v: i32, mode: StrollMode) -> Spec {
        Spec::of(Behavior::Stroll { speed, h, v, mode })
    }
    pub fn walk_to_look_target(speed: f32, close: i32) -> Spec {
        Spec::of(Behavior::WalkToLookTarget {
            cond: None,
            speed,
            close,
            speed_fn: None,
        })
    }
    pub fn do_nothing(min: i32, max: i32) -> Spec {
        Spec::timed(Behavior::DoNothing, min, max)
    }
    pub fn animal_panic(speed: f32) -> Spec {
        Spec::timed(
            Behavior::AnimalPanic {
                speed,
                mode: PanicMode::Land,
                causes: tag::PANIC_CAUSES,
                on_start: None,
            },
            100,
            120,
        )
    }
    pub fn make_love(partner: Kind, speed: f32, close: i32) -> Spec {
        Spec::timed(
            Behavior::AnimalMakeLove {
                partner,
                speed,
                close,
                spawn_at: 0,
                on_start: None,
            },
            110,
            110,
        )
    }
    pub fn follow_temptation(speed: fn(&Tick<'_, '_>) -> f32, close: f64, look_eyes: bool) -> Spec {
        Spec::of(Behavior::FollowTemptation {
            speed,
            close,
            look_eyes,
            close_fn: None,
            on_start: None,
        })
    }
    pub fn baby_follow_adult(min: i32, max: i32, speed: f32) -> Spec {
        Spec::of(Behavior::BabyFollowAdult {
            min,
            max,
            speed,
            memory: M::NearestVisibleAdult,
            target_eye: false,
            speed_fn: None,
        })
    }
    pub fn become_passive(memory: MemoryType, duration: i32) -> Spec {
        Spec::of(Behavior::BecomePassive { memory, duration })
    }
    pub fn erase_if(cond: Cond, memory: MemoryType) -> Spec {
        Spec::of(Behavior::EraseMemoryIf { cond, memory })
    }
    pub fn copy_with_expiry(
        cond: Cond,
        source: MemoryType,
        target: MemoryType,
        min: i32,
        max: i32,
    ) -> Spec {
        Spec::of(Behavior::CopyMemoryWithExpiry {
            cond,
            source,
            target,
            min,
            max,
        })
    }
    pub fn go_to_wanted_item(
        cond: Option<Cond>,
        speed: f32,
        interrupt: bool,
        max_dist: i32,
    ) -> Spec {
        Spec::of(Behavior::GoToWantedItem {
            cond,
            speed,
            interrupt,
            max_dist,
        })
    }
    pub fn walk_away_from(memory: MemoryType, speed: f32, distance: i32, interrupt: bool) -> Spec {
        Spec::of(Behavior::WalkAwayFrom {
            memory,
            speed,
            distance,
            interrupt,
        })
    }
    pub fn try_find_land(range: i32, speed: f32) -> Spec {
        Spec::of(Behavior::TryFindLand {
            range,
            speed,
            next_ok: 0,
        })
    }
    pub fn try_find_water(range: i32, speed: f32) -> Spec {
        Spec::of(Behavior::TryFindWater {
            range,
            speed,
            next_ok: 0,
        })
    }
    pub fn random_look_around(
        min: i32,
        max: i32,
        max_yaw: f32,
        min_pitch: f32,
        max_pitch: f32,
    ) -> Spec {
        Spec::of(Behavior::RandomLookAround {
            min,
            max,
            max_yaw,
            min_pitch,
            max_pitch,
        })
    }
    pub fn fam(b: FamBehavior) -> Spec {
        let (min, max) = fam_duration(&b);
        Spec::timed(Behavior::Fam(b), min, max)
    }
}

fn fam_duration(b: &FamBehavior) -> (i32, i32) {
    match b {
        FamBehavior::Brainy(b) => crate::mob::fam::brainy::behavior_duration(b),
        FamBehavior::Mount(b) => crate::mob::fam::mount::behavior_duration(b),
        FamBehavior::Village(b) => crate::mob::fam::village::behavior_duration(b),
        FamBehavior::Warden(b) => crate::mob::boss::warden::ai::behavior_duration(b),
    }
}

/// The shape of a behaviour.
pub fn shape(b: &Behavior) -> Shape {
    match b {
        Behavior::Gate { .. } => Shape::Gate,
        Behavior::DoNothing => Shape::Idle,
        Behavior::Swim { .. }
        | Behavior::LookAtTargetSink
        | Behavior::MoveToTargetSink { .. }
        | Behavior::CountDownCooldown(_)
        | Behavior::AnimalPanic { .. }
        | Behavior::AnimalMakeLove { .. }
        | Behavior::FollowTemptation { .. }
        | Behavior::RandomLookAround { .. } => Shape::Timed,
        Behavior::Fam(FamBehavior::Brainy(b)) => crate::mob::fam::brainy::behavior_shape(b),
        Behavior::Fam(FamBehavior::Mount(b)) => crate::mob::fam::mount::behavior_shape(b),
        Behavior::Fam(FamBehavior::Village(b)) => crate::mob::fam::village::behavior_shape(b),
        Behavior::Fam(FamBehavior::Warden(b)) => crate::mob::boss::warden::ai::behavior_shape(b),
        _ => Shape::OneShot,
    }
}

/// The memories a behaviour requires (`getRequiredMemories` / the declarative instances).
pub fn requirements(b: &Behavior) -> &'static [(MemoryType, MemoryStatus)] {
    match b {
        Behavior::Gate { entry, .. } => entry,
        Behavior::TriggerGate { .. }
        | Behavior::TriggerIf(_)
        | Behavior::Check(_)
        | Behavior::Sequence
        | Behavior::Swim { .. }
        | Behavior::DoNothing
        | Behavior::BecomePassive { .. }
        | Behavior::EraseMemoryIf { .. }
        | Behavior::CopyMemoryWithExpiry { .. }
        | Behavior::WalkAwayFrom { .. }
        | Behavior::CountDownCooldown(_) => dynamic_requirements(b),
        Behavior::LookAtTargetSink => &[(M::LookTarget, S::ValuePresent)],
        Behavior::MoveToTargetSink { .. } => &[
            (M::CantReachWalkTargetSince, S::Registered),
            (M::Path, S::ValueAbsent),
            (M::WalkTarget, S::ValuePresent),
        ],
        Behavior::StartAttacking { .. } => &[
            (M::AttackTarget, S::ValueAbsent),
            (M::CantReachWalkTargetSince, S::Registered),
        ],
        Behavior::StopAttackingIfTargetInvalid { .. } => &[
            (M::AttackTarget, S::ValuePresent),
            (M::CantReachWalkTargetSince, S::Registered),
        ],
        Behavior::WalkToAttackTarget { .. } => &[
            (M::WalkTarget, S::Registered),
            (M::LookTarget, S::Registered),
            (M::AttackTarget, S::ValuePresent),
            (M::NearestVisibleLivingEntities, S::Registered),
        ],
        Behavior::MeleeAttack { .. } => &[
            (M::LookTarget, S::Registered),
            (M::AttackTarget, S::ValuePresent),
            (M::AttackCoolingDown, S::ValueAbsent),
            (M::NearestVisibleLivingEntities, S::ValuePresent),
        ],
        Behavior::LookAtEntity { .. } | Behavior::LookAtEntitySometimes { .. } => &[
            (M::LookTarget, S::ValueAbsent),
            (M::NearestVisibleLivingEntities, S::ValuePresent),
        ],
        Behavior::Stroll { .. } => &[(M::WalkTarget, S::ValueAbsent)],
        Behavior::WalkToLookTarget { .. } => &[
            (M::WalkTarget, S::ValueAbsent),
            (M::LookTarget, S::ValuePresent),
        ],
        Behavior::AnimalPanic { .. } => {
            &[(M::IsPanicking, S::Registered), (M::HurtBy, S::Registered)]
        }
        Behavior::AnimalMakeLove { .. } => &[
            (M::NearestVisibleLivingEntities, S::ValuePresent),
            (M::BreedTarget, S::ValueAbsent),
            (M::WalkTarget, S::Registered),
            (M::LookTarget, S::Registered),
            (M::IsPanicking, S::ValueAbsent),
        ],
        Behavior::FollowTemptation { .. } => &[
            (M::LookTarget, S::Registered),
            (M::WalkTarget, S::Registered),
            (M::TemptationCooldownTicks, S::ValueAbsent),
            (M::TemptingPlayer, S::ValuePresent),
            (M::BreedTarget, S::ValueAbsent),
            (M::IsPanicking, S::ValueAbsent),
        ],
        Behavior::BabyFollowAdult { .. } => &[
            (M::NearestVisibleAdult, S::ValuePresent),
            (M::LookTarget, S::Registered),
            (M::WalkTarget, S::ValueAbsent),
        ],
        Behavior::GoToWantedItem { interrupt, .. } => {
            if *interrupt {
                &[
                    (M::LookTarget, S::Registered),
                    (M::WalkTarget, S::Registered),
                    (M::NearestVisibleWantedItem, S::ValuePresent),
                    (M::ItemPickupCooldownTicks, S::Registered),
                ]
            } else {
                &[
                    (M::LookTarget, S::Registered),
                    (M::WalkTarget, S::ValueAbsent),
                    (M::NearestVisibleWantedItem, S::ValuePresent),
                    (M::ItemPickupCooldownTicks, S::Registered),
                ]
            }
        }
        Behavior::StopBeingAngryIfTargetDead => &[(M::AngryAt, S::ValuePresent)],
        Behavior::TryFindLand { .. } | Behavior::TryFindWater { .. } => &[
            (M::AttackTarget, S::ValueAbsent),
            (M::WalkTarget, S::ValueAbsent),
            (M::LookTarget, S::Registered),
        ],
        Behavior::RandomLookAround { .. } => &[
            (M::LookTarget, S::ValueAbsent),
            (M::GazeCooldownTicks, S::ValueAbsent),
        ],
        Behavior::StrollToPoi { .. }
        | Behavior::StrollAroundPoi { .. }
        | Behavior::InteractWith { .. }
        | Behavior::GoToTargetLocation { .. } => dynamic_requirements(b),
        Behavior::SetLookAndInteract { .. } => &[
            (M::LookTarget, S::Registered),
            (M::InteractionTarget, S::ValueAbsent),
            (M::NearestVisibleLivingEntities, S::ValuePresent),
        ],
        Behavior::BackUpIfTooClose { .. } => &[
            (M::WalkTarget, S::ValueAbsent),
            (M::LookTarget, S::Registered),
            (M::AttackTarget, S::ValuePresent),
            (M::NearestVisibleLivingEntities, S::ValuePresent),
        ],
        Behavior::Mount { .. } => &[
            (M::LookTarget, S::Registered),
            (M::WalkTarget, S::ValueAbsent),
            (M::RideTarget, S::ValuePresent),
        ],
        Behavior::Dismount { .. } => &[(M::RideTarget, S::Registered)],
        Behavior::Fam(FamBehavior::Brainy(b)) => crate::mob::fam::brainy::behavior_requirements(b),
        Behavior::Fam(FamBehavior::Mount(b)) => crate::mob::fam::mount::behavior_requirements(b),
        Behavior::Fam(FamBehavior::Village(b)) => {
            crate::mob::fam::village::behavior_requirements(b)
        }
        Behavior::Fam(FamBehavior::Warden(b)) => {
            crate::mob::boss::warden::ai::behavior_requirements(b)
        }
    }
}

/// Requirements that depend on the behaviour's memory parameters.
fn dynamic_requirements(b: &Behavior) -> &'static [(MemoryType, MemoryStatus)] {
    match *b {
        Behavior::BecomePassive { memory, .. } => passive_requirements(memory),
        Behavior::EraseMemoryIf { memory, .. } | Behavior::CountDownCooldown(memory) => {
            present(memory)
        }
        Behavior::CopyMemoryWithExpiry { source, target, .. } => copy_requirements(source, target),
        Behavior::WalkAwayFrom { memory, .. }
        | Behavior::StrollToPoi { memory, .. }
        | Behavior::StrollAroundPoi { memory, .. } => away_requirements(memory),
        Behavior::InteractWith { memory, .. } => interact_requirements(memory),
        Behavior::GoToTargetLocation { memory, .. } => location_requirements(memory),
        _ => &[],
    }
}

/// `[(memory, VALUE_PRESENT)]` for any memory type.
pub fn present(m: MemoryType) -> &'static [(MemoryType, MemoryStatus)] {
    static TABLE: std::sync::OnceLock<Vec<[(MemoryType, MemoryStatus); 1]>> =
        std::sync::OnceLock::new();
    let t = TABLE.get_or_init(|| {
        MemoryType::ALL
            .iter()
            .map(|&m| [(m, S::ValuePresent)])
            .collect()
    });
    &t[m as usize]
}

fn passive_requirements(m: MemoryType) -> &'static [(MemoryType, MemoryStatus)] {
    static TABLE: std::sync::OnceLock<Vec<[(MemoryType, MemoryStatus); 3]>> =
        std::sync::OnceLock::new();
    let t = TABLE.get_or_init(|| {
        MemoryType::ALL
            .iter()
            .map(|&m| {
                [
                    (M::AttackTarget, S::Registered),
                    (M::Pacified, S::ValueAbsent),
                    (m, S::ValuePresent),
                ]
            })
            .collect()
    });
    &t[m as usize]
}

fn interact_requirements(m: MemoryType) -> &'static [(MemoryType, MemoryStatus)] {
    static TABLE: std::sync::OnceLock<Vec<[(MemoryType, MemoryStatus); 4]>> =
        std::sync::OnceLock::new();
    let t = TABLE.get_or_init(|| {
        MemoryType::ALL
            .iter()
            .map(|&m| {
                [
                    (m, S::Registered),
                    (M::LookTarget, S::Registered),
                    (M::WalkTarget, S::ValueAbsent),
                    (M::NearestVisibleLivingEntities, S::ValuePresent),
                ]
            })
            .collect()
    });
    &t[m as usize]
}

fn location_requirements(m: MemoryType) -> &'static [(MemoryType, MemoryStatus)] {
    static TABLE: std::sync::OnceLock<Vec<[(MemoryType, MemoryStatus); 4]>> =
        std::sync::OnceLock::new();
    let t = TABLE.get_or_init(|| {
        MemoryType::ALL
            .iter()
            .map(|&m| {
                [
                    (m, S::ValuePresent),
                    (M::AttackTarget, S::ValueAbsent),
                    (M::WalkTarget, S::ValueAbsent),
                    (M::LookTarget, S::Registered),
                ]
            })
            .collect()
    });
    &t[m as usize]
}

fn away_requirements(m: MemoryType) -> &'static [(MemoryType, MemoryStatus)] {
    static TABLE: std::sync::OnceLock<Vec<[(MemoryType, MemoryStatus); 2]>> =
        std::sync::OnceLock::new();
    let t = TABLE.get_or_init(|| {
        MemoryType::ALL
            .iter()
            .map(|&m| [(M::WalkTarget, S::Registered), (m, S::ValuePresent)])
            .collect()
    });
    &t[m as usize]
}

/// The requirements of a `CopyMemoryWithExpiry` pair, made once per pair.
type Requirements = &'static [(MemoryType, MemoryStatus)];
type CopyTable = Vec<(MemoryType, MemoryType, Requirements)>;

fn copy_requirements(source: MemoryType, target: MemoryType) -> Requirements {
    static TABLE: std::sync::Mutex<CopyTable> = std::sync::Mutex::new(Vec::new());
    let mut t = TABLE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(e) = t.iter().find(|e| e.0 == source && e.1 == target) {
        return e.2;
    }
    let r: Requirements = Box::leak(Box::new([
        (source, S::ValuePresent),
        (target, S::ValueAbsent),
    ]));
    t.push((source, target, r));
    r
}

// --- The protocol ------------------------------------------------------------------------

fn node_copy(t: &Tick<'_, '_>, node: u16) -> Option<super::Node> {
    let b = t.mob_ref().brain.as_deref()?;
    let n = b.nodes.get(usize::from(node))?;
    Some(super::Node {
        behavior: n.behavior,
        status: n.status,
        end: n.end,
        min_duration: n.min_duration,
        max_duration: n.max_duration,
        children: Vec::new(),
        random: None,
    })
}

fn children(t: &Tick<'_, '_>, node: u16) -> ([super::Child; 8], usize) {
    let mut out = [super::Child {
        node: 0,
        weight: 0,
        rand_weight: 0.0,
    }; 8];
    let Some(n) = t
        .mob_ref()
        .brain
        .as_deref()
        .and_then(|b| b.nodes.get(usize::from(node)))
    else {
        return (out, 0);
    };
    let len = n.children.len().min(8);
    out[..len].copy_from_slice(&n.children[..len]);
    (out, len)
}

fn alive(t: &Tick<'_, '_>) -> bool {
    t.exists() && t.mob_ref().brain.is_some()
}

fn write(t: &mut Tick<'_, '_>, node: u16, f: impl FnOnce(&mut super::Node)) {
    if !alive(t) {
        return;
    }
    if let Some(n) = brain(t).nodes.get_mut(usize::from(node)) {
        f(n);
    }
}

fn status(t: &Tick<'_, '_>, node: u16) -> Status {
    t.mob_ref()
        .brain
        .as_deref()
        .and_then(|b| b.nodes.get(usize::from(node)))
        .map_or(Status::Stopped, |n| n.status)
}

/// `ShufflingList.shuffle` with the list's own random.
fn shuffle(t: &mut Tick<'_, '_>, node: u16) {
    if !alive(t) {
        return;
    }
    let Some(n) = brain(t).nodes.get_mut(usize::from(node)) else {
        return;
    };
    let Some(random) = n.random.as_deref_mut() else {
        return;
    };
    for c in &mut n.children {
        let r = random.next_float();
        #[allow(clippy::cast_precision_loss)]
        let w = c.weight as f32;
        c.rand_weight = -f64::from(r).powf(f64::from(1.0 / w));
    }
    // `List.sort` is stable, like `sort_by`.
    n.children
        .sort_by(|a, b| a.rand_weight.total_cmp(&b.rand_weight));
}

/// `tryStart`.
pub fn try_start(t: &mut Tick<'_, '_>, node: u16, now: i64) -> bool {
    let Some(n) = node_copy(t, node) else {
        return false;
    };
    match shape(&n.behavior) {
        Shape::OneShot => {
            if trigger(t, node, now) {
                write(t, node, |n| n.status = Status::Running);
                true
            } else {
                false
            }
        }
        Shape::Idle => {
            let d = n.min_duration
                + t.env
                    .random
                    .next_int_bounded(n.max_duration + 1 - n.min_duration);
            write(t, node, |x| {
                x.status = Status::Running;
                x.end = now + i64::from(d);
            });
            true
        }
        Shape::Timed => {
            let mut b = n.behavior;
            if !brain_ref(t).check_all(requirements(&b)) {
                return false;
            }
            let ok = check_extra(t, &mut b);
            if !alive(t) {
                return false;
            }
            if !ok {
                write(t, node, |x| x.behavior = b);
                return false;
            }
            let d = n.min_duration
                + t.env
                    .random
                    .next_int_bounded(n.max_duration + 1 - n.min_duration);
            write(t, node, |x| {
                x.status = Status::Running;
                x.end = now + i64::from(d);
            });
            start(t, &mut b, now);
            write(t, node, |x| x.behavior = b);
            true
        }
        Shape::Gate => {
            let Behavior::Gate {
                entry,
                shuffled,
                run_one,
                ..
            } = n.behavior
            else {
                return false;
            };
            if !brain_ref(t).check_all(entry) {
                return false;
            }
            write(t, node, |x| x.status = Status::Running);
            if shuffled {
                shuffle(t, node);
            }
            let (list, len) = children(t, node);
            for c in &list[..len] {
                if !alive(t) {
                    break;
                }
                if status(t, c.node) == Status::Stopped && try_start(t, c.node, now) && run_one {
                    break;
                }
            }
            true
        }
    }
}

/// `tickOrStop`.
pub fn tick_or_stop(t: &mut Tick<'_, '_>, node: u16, now: i64) {
    let Some(n) = node_copy(t, node) else {
        return;
    };
    match shape(&n.behavior) {
        Shape::OneShot => do_stop(t, node, now),
        Shape::Idle => {
            if now > n.end {
                do_stop(t, node, now);
            }
        }
        Shape::Timed => {
            let mut b = n.behavior;
            let timed_out = !no_timeout(&b) && now > n.end;
            if !timed_out && can_still_use(t, &mut b, now) {
                tick(t, &mut b, now);
                write(t, node, |x| x.behavior = b);
            } else {
                write(t, node, |x| x.behavior = b);
                do_stop(t, node, now);
            }
        }
        Shape::Gate => {
            let (list, len) = children(t, node);
            for c in &list[..len] {
                if !alive(t) {
                    return;
                }
                if status(t, c.node) == Status::Running {
                    tick_or_stop(t, c.node, now);
                }
            }
            if alive(t)
                && list[..len]
                    .iter()
                    .all(|c| status(t, c.node) != Status::Running)
            {
                do_stop(t, node, now);
            }
        }
    }
}

/// `doStop`.
pub fn do_stop(t: &mut Tick<'_, '_>, node: u16, now: i64) {
    let Some(n) = node_copy(t, node) else {
        return;
    };
    write(t, node, |x| x.status = Status::Stopped);
    match shape(&n.behavior) {
        Shape::OneShot | Shape::Idle => {}
        Shape::Timed => {
            let mut b = n.behavior;
            if alive(t) {
                brain(t).stop_timed_out = now > n.end;
            }
            stop(t, &mut b, now);
            write(t, node, |x| x.behavior = b);
        }
        Shape::Gate => {
            let (list, len) = children(t, node);
            for c in &list[..len] {
                if alive(t) && status(t, c.node) == Status::Running {
                    do_stop(t, c.node, now);
                }
            }
            if let Behavior::Gate { exit, .. } = n.behavior
                && alive(t)
            {
                for &m in exit {
                    brain(t).erase(m);
                }
            }
        }
    }
}

/// `Trigger.trigger` of a one shot: the declarative memory checks, then the body.
pub fn trigger(t: &mut Tick<'_, '_>, node: u16, now: i64) -> bool {
    let Some(n) = node_copy(t, node) else {
        return false;
    };
    let mut b = n.behavior;
    if !brain_ref(t).check_all(requirements(&b)) {
        return false;
    }
    let r = match b {
        Behavior::TriggerIf(cond) => {
            if !cond(t) {
                return false;
            }
            let (list, len) = children(t, node);
            len > 0 && trigger(t, list[0].node, now)
        }
        Behavior::Check(cond) => cond(t),
        Behavior::Sequence => {
            let (list, len) = children(t, node);
            list[..len]
                .iter()
                .all(|c| alive(t) && trigger(t, c.node, now))
        }
        Behavior::TriggerGate { shuffled, run_one } => {
            if shuffled {
                shuffle(t, node);
            }
            let (list, len) = children(t, node);
            for c in &list[..len] {
                if !alive(t) {
                    break;
                }
                if trigger(t, c.node, now) && run_one {
                    break;
                }
            }
            true
        }
        _ => one_shot(t, &mut b, now),
    };
    write(t, node, |x| x.behavior = b);
    r
}

fn no_timeout(b: &Behavior) -> bool {
    match b {
        Behavior::CountDownCooldown(_) | Behavior::FollowTemptation { .. } => true,
        Behavior::Fam(FamBehavior::Brainy(b)) => crate::mob::fam::brainy::behavior_no_timeout(b),
        Behavior::Fam(FamBehavior::Mount(b)) => crate::mob::fam::mount::behavior_no_timeout(b),
        Behavior::Fam(FamBehavior::Village(b)) => crate::mob::fam::village::behavior_no_timeout(b),
        Behavior::Fam(FamBehavior::Warden(b)) => {
            crate::mob::boss::warden::ai::behavior_no_timeout(b)
        }
        _ => false,
    }
}

// --- Helpers --------------------------------------------------------------------------------

/// The position of a living entity or item (`position()`).
pub fn entity_pos(t: &Tick<'_, '_>, id: EntityId) -> Option<Vec3> {
    t.ents.store.get(id).map(|e| e.pos())
}

/// Whether the entity exists and is alive.
pub fn is_alive(t: &Tick<'_, '_>, id: EntityId) -> bool {
    if let Some(m) = t.ents.mob(id) {
        return m.is_alive();
    }
    if let Some(p) = t.ents.player(id) {
        return !p.survival.is_dead_or_dying();
    }
    t.ents.store.get(id).is_some()
}

/// `PositionTracker.currentPosition`.
pub fn tracker_pos(t: &Tick<'_, '_>, tr: &Tracker) -> Option<Vec3> {
    match *tr {
        Tracker::Block { center, .. } => Some(center),
        Tracker::Entity { id, track_eye, .. } => {
            let e = t.ents.store.get(id)?;
            if track_eye {
                let eye = t.other(id).map_or(e.pos().y, |o| o.eye_y);
                Some(Vec3::new(e.pos().x, eye, e.pos().z))
            } else {
                Some(e.pos())
            }
        }
    }
}

/// `PositionTracker.currentBlockPosition`.
pub fn tracker_block(t: &Tick<'_, '_>, tr: &Tracker) -> Option<BlockPos> {
    match *tr {
        Tracker::Block { pos, .. } => Some(pos),
        Tracker::Entity { id, target_eye, .. } => {
            let e = t.ents.store.get(id)?;
            let p = e.pos();
            let y = if target_eye {
                t.other(id).map_or(p.y, |o| o.eye_y)
            } else {
                p.y
            };
            Some(BlockPos::containing(p.x, y, p.z))
        }
    }
}

/// `PositionTracker.isVisibleBy(body)`.
pub fn tracker_visible(t: &mut Tick<'_, '_>, tr: &Tracker) -> bool {
    match *tr {
        Tracker::Block { .. } => true,
        Tracker::Entity { id, .. } => {
            let living = t.ents.player(id).is_some() || t.ents.mob(id).is_some();
            if !living {
                return t.ents.store.get(id).is_some();
            }
            is_alive(t, id) && sensing::visible_contains(t, id)
        }
    }
}

/// `distanceToSqr(entity)`.
pub fn dist_sqr(t: &Tick<'_, '_>, id: EntityId) -> f64 {
    entity_pos(t, id).map_or(f64::MAX, |p| t.pos().distance_to_sqr(p))
}

/// `closerThan(entity, distance)`.
pub fn closer_than(t: &Tick<'_, '_>, id: EntityId, d: f64) -> bool {
    dist_sqr(t, id) < d * d
}

/// The mob's brain is in another mob: run `f` on it.
pub fn other_brain(t: &mut Tick<'_, '_>, id: EntityId, f: impl FnOnce(&mut Brain)) {
    if let Some(b) = t.ents.mob_mut(id).and_then(|m| m.brain.as_deref_mut()) {
        f(b);
    }
}

/// `BehaviorUtils.setWalkAndLookTargetMemories(walker, tracker, speed, closeEnough)` for the
/// ticking mob.
pub fn set_walk_and_look(t: &mut Tick<'_, '_>, target: Tracker, speed: f32, close: i32) {
    let b = brain(t);
    b.set(M::LookTarget, MemValue::Look(target));
    b.set(
        M::WalkTarget,
        MemValue::Walk(WalkTarget {
            target,
            speed,
            close_enough: close,
        }),
    );
}

/// `BehaviorUtils.lockGazeAndWalkToEachOther`.
pub fn lock_gaze_and_walk_to_each_other(
    t: &mut Tick<'_, '_>,
    other: EntityId,
    speed: f32,
    close: i32,
) {
    let me = t.id;
    brain(t).set(M::LookTarget, MemValue::Look(Tracker::entity(other, true)));
    other_brain(t, other, |b| {
        b.set(M::LookTarget, MemValue::Look(Tracker::entity(me, true)));
    });
    set_walk_and_look(t, Tracker::entity(other, true), speed, close);
    other_brain(t, other, |b| {
        let target = Tracker::entity(me, true);
        b.set(M::LookTarget, MemValue::Look(target));
        b.set(
            M::WalkTarget,
            MemValue::Walk(WalkTarget {
                target,
                speed,
                close_enough: close,
            }),
        );
    });
}

/// `BehaviorUtils.getLivingEntityFromUUIDMemory`: a player by UUID, or a mob within the
/// follow range.
pub fn living_from_uuid(t: &mut Tick<'_, '_>, uuid: u128) -> Option<EntityId> {
    if let Some(p) = t.ents.player_by_uuid(uuid) {
        return Some(p);
    }
    let r = t.mob_ref().attrs.follow_range.max(16.0) * 2.0;
    let area = t.aabb().inflate(r, r, r);
    let mut found = None;
    let ents = &*t.ents;
    ents.store.for_each_in(&area, false, |id| {
        if found.is_none()
            && ents.mob(id).is_some()
            && ents.store.get(id).is_some_and(|e| e.uuid() == uuid)
        {
            found = Some(id);
        }
    });
    found
}

/// `swing(MAIN_HAND)`.
pub fn swing(t: &mut Tick<'_, '_>, off_hand: bool) {
    let Some(e) = t.ents.store.get(t.id) else {
        return;
    };
    let entity_id = e.network_id().0;
    let id = t.id;
    t.env.out.push(Effect::Tracking {
        entity: id,
        with_self: false,
        packet: SharedPacket::of(&Animate {
            entity_id,
            action: if off_hand { 3 } else { 0 },
        }),
    });
}

/// The range of a held projectile weapon the mob can use (`isWithinAttackRange`): crossbow
/// piglins 8.
fn projectile_range(t: &Tick<'_, '_>) -> Option<f64> {
    let m = t.mob_ref();
    let held = m.equipment.get(crate::item::EquipmentSlot::MainHand);
    if held.is_empty() {
        return None;
    }
    let name = crate::item::item_id(held).name();
    (m.kind == Kind::Piglin && name == "minecraft:crossbow").then_some(8.0)
}

/// `BehaviorUtils.isWithinAttackRange(body, target, margin)`.
pub fn within_attack_range(t: &Tick<'_, '_>, target: EntityId, margin: i32) -> bool {
    match projectile_range(t) {
        Some(range) => closer_than(t, target, range - f64::from(margin)),
        None => crate::mob::goals::within_melee_range(t, target),
    }
}

/// `isHoldingUsableNonMeleeWeapon`.
fn holding_non_melee_weapon(t: &Tick<'_, '_>) -> bool {
    projectile_range(t).is_some()
}

/// `BehaviorUtils.isOtherTargetMuchFurtherAwayThanCurrentAttackTarget`.
pub fn much_further_than_target(t: &Tick<'_, '_>, other: EntityId, further: f64) -> bool {
    let Some(current) = brain_ref(t).entity(M::AttackTarget) else {
        return false;
    };
    let d_current = dist_sqr(t, current);
    let d_other = dist_sqr(t, other);
    d_other > d_current + further * further
}

/// `BehaviorUtils.getNearestTarget(body, optional, other)`.
pub fn nearest_target(t: &Tick<'_, '_>, a: Option<EntityId>, b: EntityId) -> EntityId {
    match a {
        None => b,
        Some(a) => {
            if dist_sqr(t, a) < dist_sqr(t, b) {
                a
            } else {
                b
            }
        }
    }
}

/// `LivingEntity.canAttack` for the brain (players in survival/adventure outside peaceful,
/// living mobs).
pub fn can_attack(t: &Tick<'_, '_>, target: EntityId) -> bool {
    crate::mob::goals::can_attack(t, target)
}

// --- Behaviour bodies -----------------------------------------------------------------------

fn check_extra(t: &mut Tick<'_, '_>, b: &mut Behavior) -> bool {
    match b {
        Behavior::Swim { .. } => {
            let f = t.mob_ref().fluids;
            let threshold = if crate::mob::eye_height(t.mob_ref()) < 0.4 {
                0.0
            } else {
                0.4
            };
            f.water_height > threshold || f.in_lava()
        }
        Behavior::MoveToTargetSink {
            cooldown,
            has_path,
            last_target,
            speed,
        } => {
            if *cooldown > 0 {
                *cooldown -= 1;
                return false;
            }
            let Some(walk) = brain_ref(t).walk_target() else {
                return false;
            };
            let reached = reached_target(t, &walk);
            if !reached && try_compute_path(t, &walk, has_path, speed) {
                *last_target = tracker_block(t, &walk.target);
                true
            } else {
                let b = brain(t);
                b.erase(M::WalkTarget);
                if reached {
                    b.erase(M::CantReachWalkTargetSince);
                }
                false
            }
        }
        Behavior::AnimalPanic { causes, .. } => {
            let hurt = match brain_ref(t).get(M::HurtBy) {
                Some(MemValue::Damage(kind, _)) => t.env.types.is(*kind, *causes),
                _ => false,
            };
            hurt || brain_ref(t).has(M::IsPanicking)
        }
        Behavior::AnimalMakeLove { partner, .. } => {
            let partner = *partner;
            t.mob_ref().in_love > 0 && find_breed_partner(t, partner).is_some()
        }
        Behavior::Fam(FamBehavior::Brainy(f)) => crate::mob::fam::brainy::behavior_check(t, f),
        Behavior::Fam(FamBehavior::Mount(f)) => crate::mob::fam::mount::behavior_check(t, f),
        Behavior::Fam(FamBehavior::Village(f)) => crate::mob::fam::village::behavior_check(t, f),
        Behavior::Fam(FamBehavior::Warden(f)) => crate::mob::boss::warden::ai::behavior_check(t, f),
        _ => true,
    }
}

fn can_still_use(t: &mut Tick<'_, '_>, b: &mut Behavior, now: i64) -> bool {
    match b {
        Behavior::Swim { .. } => check_extra(t, b),
        Behavior::LookAtTargetSink => {
            let Some(tr) = brain_ref(t).look_target() else {
                return false;
            };
            tracker_visible(t, &tr)
        }
        Behavior::MoveToTargetSink {
            has_path,
            last_target,
            ..
        } => {
            if !*has_path || last_target.is_none() {
                return false;
            }
            let walk = brain_ref(t).walk_target();
            let spectator = walk
                .and_then(|w| w.target.entity_id())
                .and_then(|id| t.ents.player(id))
                .is_some_and(|p| p.game_mode == GameMode::Spectator);
            !t.nav_ref().is_done() && walk.is_some_and(|w| !reached_target(t, &w)) && !spectator
        }
        Behavior::CountDownCooldown(m) => brain_ref(t).int(*m).is_some_and(|v| v > 0),
        Behavior::AnimalPanic { .. } => true,
        Behavior::AnimalMakeLove {
            partner, spawn_at, ..
        } => {
            let Some(target) = brain_ref(t).entity(M::BreedTarget) else {
                return false;
            };
            if t.ents.mob(target).map(|m| m.kind) != Some(*partner) {
                return false;
            }
            is_alive(t, target)
                && can_mate(t, target)
                && sensing::entity_is_visible(t, target)
                && now <= *spawn_at
                && !brain_ref(t).has(M::IsPanicking)
                && !t
                    .ents
                    .mob(target)
                    .and_then(|m| m.brain.as_deref())
                    .is_some_and(|b| b.has(M::IsPanicking))
        }
        Behavior::FollowTemptation { .. } => {
            let b = brain_ref(t);
            b.has(M::TemptingPlayer) && !b.has(M::BreedTarget) && !b.has(M::IsPanicking)
        }
        Behavior::Fam(FamBehavior::Brainy(f)) => {
            crate::mob::fam::brainy::behavior_can_still_use(t, f, now)
        }
        Behavior::Fam(FamBehavior::Mount(f)) => {
            crate::mob::fam::mount::behavior_can_still_use(t, f, now)
        }
        Behavior::Fam(FamBehavior::Village(f)) => {
            crate::mob::fam::village::behavior_can_still_use(t, f, now)
        }
        Behavior::Fam(FamBehavior::Warden(f)) => {
            crate::mob::boss::warden::ai::behavior_can_still_use(t, f, now)
        }
        _ => false,
    }
}

fn start(t: &mut Tick<'_, '_>, b: &mut Behavior, now: i64) {
    match b {
        Behavior::MoveToTargetSink { speed, .. } => {
            let speed = *speed;
            brain(t).set(M::Path, MemValue::Unit);
            let path = t.mob().goal_path.take();
            nav::move_to_path(t, path, f64::from(speed));
        }
        Behavior::AnimalPanic { on_start, .. } => {
            if let Some(f) = on_start {
                f(t);
            }
            let b = brain(t);
            b.set(M::IsPanicking, MemValue::Bool(true));
            b.erase(M::WalkTarget);
            t.nav().stop();
        }
        Behavior::AnimalMakeLove {
            partner,
            speed,
            close,
            spawn_at,
            on_start,
        } => {
            if let Some(f) = on_start {
                f(t);
            }
            let Some(p) = find_breed_partner(t, *partner) else {
                return;
            };
            let me = t.id;
            brain(t).set(M::BreedTarget, MemValue::Entity(p));
            other_brain(t, p, |b| b.set(M::BreedTarget, MemValue::Entity(me)));
            lock_gaze_and_walk_to_each_other(t, p, *speed, *close);
            let d = 60 + t.mob().random.next_int_bounded(50);
            *spawn_at = now + i64::from(d);
        }
        Behavior::FollowTemptation {
            on_start: Some(f), ..
        } => f(t),
        Behavior::RandomLookAround {
            min,
            max,
            max_yaw,
            min_pitch,
            max_pitch,
        } => {
            let y_rot = t.core().rot().y_rot;
            let r = &mut t.mob().random;
            let pitch =
                (r.next_float() * (*max_pitch - *min_pitch) + *min_pitch).clamp(-90.0, 90.0);
            let rotation = vanillite_base::math::wrap_degrees_f(
                y_rot + 2.0 * r.next_float() * *max_yaw - *max_yaw,
            );
            let cooldown = r.next_int_between_inclusive(*min, *max);
            let dir = direction_from_rotation(pitch, rotation);
            let p = t.pos();
            let eye = Vec3::new(p.x, t.eye_y(), p.z);
            let b = brain(t);
            b.set(
                M::LookTarget,
                MemValue::Look(Tracker::at(Vec3::new(
                    eye.x + dir.x,
                    eye.y + dir.y,
                    eye.z + dir.z,
                ))),
            );
            b.set(M::GazeCooldownTicks, MemValue::Int(cooldown));
        }
        Behavior::Fam(FamBehavior::Brainy(f)) => crate::mob::fam::brainy::behavior_start(t, f, now),
        Behavior::Fam(FamBehavior::Mount(f)) => crate::mob::fam::mount::behavior_start(t, f, now),
        Behavior::Fam(FamBehavior::Village(f)) => {
            crate::mob::fam::village::behavior_start(t, f, now)
        }
        Behavior::Fam(FamBehavior::Warden(f)) => {
            crate::mob::boss::warden::ai::behavior_start(t, f, now)
        }
        _ => {}
    }
}

/// `Vec3.directionFromRotation(xRot, yRot)`.
pub fn direction_from_rotation(x_rot: f32, y_rot: f32) -> Vec3 {
    use vanillite_base::math::{cos, sin};
    let rad = std::f64::consts::PI / 180.0;
    let ycos = cos(-f64::from(y_rot) * rad - std::f64::consts::PI);
    let ysin = sin(-f64::from(y_rot) * rad - std::f64::consts::PI);
    let xcos = -cos(-f64::from(x_rot) * rad);
    let xsin = sin(-f64::from(x_rot) * rad);
    Vec3::new(
        f64::from(ysin * xcos),
        f64::from(xsin),
        f64::from(ycos * xcos),
    )
}

fn tick(t: &mut Tick<'_, '_>, b: &mut Behavior, now: i64) {
    match b {
        Behavior::Swim { chance } => {
            if t.mob().random.next_float() < *chance {
                t.mob().jump = true;
            }
        }
        Behavior::LookAtTargetSink => {
            if let Some(tr) = brain_ref(t).look_target()
                && let Some(p) = tracker_pos(t, &tr)
            {
                nav::look_at(t, p);
            }
        }
        Behavior::MoveToTargetSink {
            has_path,
            last_target,
            speed,
            ..
        } => {
            let nav_has = t.nav_ref().path.is_some();
            if nav_has != *has_path {
                *has_path = nav_has;
                if nav_has {
                    brain(t).set(M::Path, MemValue::Unit);
                } else {
                    brain(t).erase(M::Path);
                }
            }
            if nav_has && let Some(last) = *last_target {
                let Some(walk) = brain_ref(t).walk_target() else {
                    return;
                };
                let Some(pos) = tracker_block(t, &walk.target) else {
                    return;
                };
                if pos.dist_sqr(last) > 4.0 && try_compute_path(t, &walk, has_path, speed) {
                    *last_target = Some(pos);
                    let mut again = *b;
                    start(t, &mut again, now);
                    *b = again;
                }
            }
        }
        Behavior::CountDownCooldown(m) => {
            if let Some(v) = brain_ref(t).int(*m) {
                brain(t).set(*m, MemValue::Int(v - 1));
            }
        }
        Behavior::AnimalPanic { speed, mode, .. } => {
            if t.nav_ref().is_done()
                && let Some(p) = panic_pos(t, *mode)
            {
                brain(t).set(M::WalkTarget, MemValue::Walk(WalkTarget::at(p, *speed, 0)));
            }
        }
        Behavior::AnimalMakeLove {
            speed,
            close,
            spawn_at,
            ..
        } => {
            let Some(partner) = brain_ref(t).entity(M::BreedTarget) else {
                return;
            };
            lock_gaze_and_walk_to_each_other(t, partner, *speed, *close);
            if closer_than(t, partner, 3.0) && now >= *spawn_at {
                if !crate::mob::fam::brainy::custom_breed(t, partner) {
                    crate::mob::animal::breed(t.level, t.ents, t.env, t.id, partner);
                }
                if alive(t) {
                    brain(t).erase(M::BreedTarget);
                }
                other_brain(t, partner, |b| b.erase(M::BreedTarget));
            }
        }
        Behavior::FollowTemptation {
            speed,
            close,
            look_eyes,
            close_fn,
            ..
        } => {
            let Some(player) = brain_ref(t).entity(M::TemptingPlayer) else {
                return;
            };
            brain(t).set(M::LookTarget, MemValue::Look(Tracker::entity(player, true)));
            let close = close_fn.map_or(*close, |f| f(t));
            if dist_sqr(t, player) < close * close {
                brain(t).erase(M::WalkTarget);
            } else {
                let s = speed(t);
                brain(t).set(
                    M::WalkTarget,
                    MemValue::Walk(WalkTarget {
                        target: Tracker::Entity {
                            id: player,
                            track_eye: *look_eyes,
                            target_eye: *look_eyes,
                        },
                        speed: s,
                        close_enough: 2,
                    }),
                );
            }
        }
        Behavior::Fam(FamBehavior::Brainy(f)) => crate::mob::fam::brainy::behavior_tick(t, f, now),
        Behavior::Fam(FamBehavior::Mount(f)) => crate::mob::fam::mount::behavior_tick(t, f, now),
        Behavior::Fam(FamBehavior::Village(f)) => {
            crate::mob::fam::village::behavior_tick(t, f, now)
        }
        Behavior::Fam(FamBehavior::Warden(f)) => {
            crate::mob::boss::warden::ai::behavior_tick(t, f, now)
        }
        _ => {}
    }
}

fn stop(t: &mut Tick<'_, '_>, b: &mut Behavior, now: i64) {
    match b {
        Behavior::LookAtTargetSink => brain(t).erase(M::LookTarget),
        Behavior::MoveToTargetSink {
            cooldown, has_path, ..
        } => {
            let walk = brain_ref(t).walk_target();
            if let Some(w) = walk
                && !reached_target(t, &w)
                && t.nav_ref().is_stuck
            {
                *cooldown = t.env.random.next_int_bounded(40);
            }
            t.nav().stop();
            let b = brain(t);
            b.erase(M::WalkTarget);
            b.erase(M::Path);
            *has_path = false;
            t.mob().goal_path = None;
        }
        Behavior::CountDownCooldown(m) => brain(t).erase(*m),
        Behavior::AnimalPanic { .. } => brain(t).erase(M::IsPanicking),
        Behavior::AnimalMakeLove { spawn_at, .. } => {
            let b = brain(t);
            b.erase(M::BreedTarget);
            b.erase(M::WalkTarget);
            b.erase(M::LookTarget);
            *spawn_at = 0;
        }
        Behavior::FollowTemptation { .. } => {
            let b = brain(t);
            b.set(M::TemptationCooldownTicks, MemValue::Int(100));
            b.erase(M::WalkTarget);
            b.erase(M::LookTarget);
        }
        Behavior::Fam(FamBehavior::Brainy(f)) => crate::mob::fam::brainy::behavior_stop(t, f, now),
        Behavior::Fam(FamBehavior::Mount(f)) => crate::mob::fam::mount::behavior_stop(t, f, now),
        Behavior::Fam(FamBehavior::Village(f)) => {
            crate::mob::fam::village::behavior_stop(t, f, now)
        }
        Behavior::Fam(FamBehavior::Warden(f)) => {
            crate::mob::boss::warden::ai::behavior_stop(t, f, now)
        }
        _ => {}
    }
}

/// `MoveToTargetSink.reachedTarget`: within the close enough Manhattan distance.
fn reached_target(t: &Tick<'_, '_>, walk: &WalkTarget) -> bool {
    tracker_block(t, &walk.target).is_some_and(|p| {
        let me = t.block_pos();
        (p.x - me.x).abs() + (p.y - me.y).abs() + (p.z - me.z).abs() <= walk.close_enough
    })
}

/// `MoveToTargetSink.tryComputePath`; the path is kept in the mob's `goal_path`.
fn try_compute_path(
    t: &mut Tick<'_, '_>,
    walk: &WalkTarget,
    has_path: &mut bool,
    speed: &mut f32,
) -> bool {
    let Some(target) = tracker_block(t, &walk.target) else {
        return false;
    };
    let path = nav::create_path_to(t, target, 0);
    *speed = walk.speed;
    let reached = reached_target(t, walk);
    let now = t.game_time();
    if reached {
        brain(t).erase(M::CantReachWalkTargetSince);
    } else {
        let can_reach = path.as_ref().is_some_and(|p| p.reached);
        let b = brain(t);
        if can_reach {
            b.erase(M::CantReachWalkTargetSince);
        } else if !b.has(M::CantReachWalkTargetSince) {
            b.set(M::CantReachWalkTargetSince, MemValue::Long(now));
        }
        if path.is_some() {
            t.mob().goal_path = path;
            *has_path = true;
            return true;
        }
        let towards = randompos::bottom_center(target);
        if let Some(step) = randompos::default_pos_towards(t, 10, 7, towards, randompos::half_pi())
        {
            let p = nav::create_path_to(t, BlockPos::containing(step.x, step.y, step.z), 0);
            let ok = p.is_some();
            t.mob().goal_path = p;
            *has_path = ok;
            return ok;
        }
    }
    t.mob().goal_path = path;
    *has_path = t.mob_ref().goal_path.is_some();
    false
}

/// `AnimalPanic.getPanicPos`: water nearby when burning, else a random position.
fn panic_pos(t: &mut Tick<'_, '_>, mode: PanicMode) -> Option<Vec3> {
    if t.mob_ref().is_on_fire()
        && let Some(w) = look_for_water(t)
    {
        return Some(randompos::bottom_center(w));
    }
    match mode {
        PanicMode::Land => randompos::land_pos(t, 5, 4),
        PanicMode::Fly(_) => randompos::default_pos(t, 5, 4),
    }
}

/// `AnimalPanic.lookForWater`: the nearest water by Manhattan distance (5 wide, 1 high).
fn look_for_water(t: &Tick<'_, '_>) -> Option<BlockPos> {
    let me = t.block_pos();
    if !t.level.block(me).collision_shape().is_empty() {
        return None;
    }
    let wide = t.core().dims().width.ceil() >= 2.0;
    let water = |p: BlockPos| crate::mob::is_water(t.level.block(p));
    let mut found = None;
    for_manhattan(me, 5, 1, |p| {
        if !water(p) {
            return false;
        }
        if wide {
            let s = BlockPos::new(p.x, p.y, p.z + 1);
            let e = BlockPos::new(p.x + 1, p.y, p.z);
            let se = BlockPos::new(p.x + 1, p.y, p.z + 1);
            if !(water(s) && water(e) && water(se)) {
                return false;
            }
        }
        found = Some(p);
        true
    });
    found
}

/// `BlockPos.withinBoxByManhattanDistance(center, h, v, h)` order: by increasing Manhattan
/// distance, x, then y, then z and its mirror; stops when `f` returns `true`.
pub fn for_manhattan(center: BlockPos, h: i32, v: i32, f: impl FnMut(BlockPos) -> bool) {
    manhattan_ordered(center, h, v, h, h + v + h, f);
}

/// `BlockPos.manhattanOrdered(origin, reachX, reachY, reachZ, maxDepth)`.
pub fn manhattan_ordered(
    center: BlockPos,
    rx: i32,
    ry: i32,
    rz: i32,
    max_depth: i32,
    mut f: impl FnMut(BlockPos) -> bool,
) {
    for d in 0..=max_depth {
        let mx = rx.min(d);
        for x in -mx..=mx {
            let my = ry.min(d - x.abs());
            for y in -my..=my {
                let z = d - x.abs() - y.abs();
                if z > rz {
                    continue;
                }
                if f(BlockPos::new(center.x + x, center.y + y, center.z + z)) {
                    return;
                }
                if z != 0 && f(BlockPos::new(center.x + x, center.y + y, center.z - z)) {
                    return;
                }
            }
        }
    }
}

/// `Animal.canMate(other)`: another animal of the same kind, both in love.
fn can_mate(t: &Tick<'_, '_>, other: EntityId) -> bool {
    let me = t.mob_ref();
    let Some(o) = t.ents.mob(other) else {
        return false;
    };
    other != t.id
        && o.kind == me.kind
        && me.in_love > 0
        && o.in_love > 0
        && crate::mob::fam::brainy::can_mate(me, o)
}

/// `AnimalMakeLove.findValidBreedPartner`.
fn find_breed_partner(t: &mut Tick<'_, '_>, partner: Kind) -> Option<EntityId> {
    sensing::visible_find_closest(t, |t, id| {
        t.ents.mob(id).is_some_and(|m| {
            m.kind == partner && !m.brain.as_deref().is_some_and(|b| b.has(M::IsPanicking))
        }) && can_mate(t, id)
    })
}

fn one_shot(t: &mut Tick<'_, '_>, b: &mut Behavior, now: i64) -> bool {
    match b {
        Behavior::StartAttacking { cond, finder } => {
            if cond.is_some_and(|c| !c(t)) {
                return false;
            }
            let Some(target) = finder(t) else {
                return false;
            };
            if !can_attack(t, target) {
                return false;
            }
            let b = brain(t);
            b.set(M::AttackTarget, MemValue::Entity(target));
            b.erase(M::CantReachWalkTargetSince);
            true
        }
        Behavior::StopAttackingIfTargetInvalid {
            stop_when,
            on_erased,
            tired,
        } => {
            let Some(target) = brain_ref(t).entity(M::AttackTarget) else {
                return false;
            };
            let tired_out = *tired
                && brain_ref(t)
                    .long(M::CantReachWalkTargetSince)
                    .is_some_and(|since| now - since > 200);
            let valid = can_attack(t, target)
                && !tired_out
                && is_alive(t, target)
                && !stop_when.is_some_and(|f| f(t, target));
            if !valid {
                if let Some(f) = on_erased {
                    f(t, target);
                }
                if alive(t) {
                    brain(t).erase(M::AttackTarget);
                }
            }
            true
        }
        Behavior::WalkToAttackTarget { speed, speed_fn } => {
            let speed = &speed_fn.map_or(*speed, |f| f(t));
            let Some(target) = brain_ref(t).entity(M::AttackTarget) else {
                return false;
            };
            let visible = brain_ref(t).has(M::NearestVisibleLivingEntities)
                && sensing::visible_contains(t, target);
            if visible && within_attack_range(t, target, 1) {
                brain(t).erase(M::WalkTarget);
            } else {
                let b = brain(t);
                b.set(M::LookTarget, MemValue::Look(Tracker::entity(target, true)));
                b.set(
                    M::WalkTarget,
                    MemValue::Walk(WalkTarget::entity(target, *speed, 0)),
                );
            }
            true
        }
        Behavior::MeleeAttack { cond, cooldown } => {
            let Some(target) = brain_ref(t).entity(M::AttackTarget) else {
                return false;
            };
            if cond.is_some_and(|c| !c(t))
                || holding_non_melee_weapon(t)
                || !crate::mob::goals::within_melee_range(t, target)
                || !sensing::visible_contains(t, target)
            {
                return false;
            }
            brain(t).set(M::LookTarget, MemValue::Look(Tracker::entity(target, true)));
            swing(t, false);
            crate::mob::combat::mob_attack(t, target);
            if alive(t) {
                brain(t).set_with_expiry(
                    M::AttackCoolingDown,
                    MemValue::Bool(true),
                    i64::from(*cooldown),
                );
            }
            true
        }
        Behavior::LookAtEntity { filter, max_dist } => {
            let max_sqr = f64::from(*max_dist * *max_dist);
            let filter = *filter;
            let me = t.id;
            let target = sensing::visible_find_closest(t, |t, id| {
                filter.is_none_or(|f| f(t, id))
                    && dist_sqr(t, id) <= max_sqr
                    && !t.ents.riding.passengers_of(me).contains(&id)
            });
            match target {
                Some(id) => {
                    brain(t).set(M::LookTarget, MemValue::Look(Tracker::entity(id, true)));
                    true
                }
                None => false,
            }
        }
        Behavior::LookAtEntitySometimes {
            filter,
            max_dist,
            min,
            max,
            ticks,
        } => {
            let max_sqr = f64::from(*max_dist * *max_dist);
            let filter = *filter;
            let target = sensing::visible_find_closest(t, |t, id| {
                filter.is_none_or(|f| f(t, id)) && dist_sqr(t, id) <= max_sqr
            });
            let Some(id) = target else {
                return false;
            };
            // `Ticker.tickDownAndCheck` with the level random.
            if *ticks == 0 {
                *ticks = t.env.random.next_int_between_inclusive(*min, *max) - 1;
                return false;
            }
            *ticks -= 1;
            if *ticks != 0 {
                return false;
            }
            brain(t).set(M::LookTarget, MemValue::Look(Tracker::entity(id, true)));
            true
        }
        Behavior::Stroll { speed, h, v, mode } => {
            let in_water = t.mob_ref().fluids.in_water();
            let pos = match *mode {
                StrollMode::Land { from_water } => {
                    if !from_water && in_water {
                        return false;
                    }
                    randompos::land_pos(t, *h, *v)
                }
                StrollMode::Swim => {
                    if !in_water {
                        return false;
                    }
                    swim_pos(t)
                }
                StrollMode::Fly => {
                    // `getTargetFlyPos`: within a quarter turn of the view direction.
                    let rot = t.core().rot();
                    let dir = randompos::view_vector(rot.x_rot, rot.y_head_rot);
                    let half = f64::from(std::f32::consts::FRAC_PI_2);
                    randompos::air_and_water_pos(t, *h, *v, -2, dir.x, dir.z, half)
                }
            };
            brain(t).set_opt(
                M::WalkTarget,
                pos.map(|p| MemValue::Walk(WalkTarget::at(p, *speed, 0))),
            );
            true
        }
        Behavior::WalkToLookTarget {
            cond,
            speed,
            close,
            speed_fn,
        } => {
            if cond.is_some_and(|c| !c(t)) {
                return false;
            }
            let speed = &speed_fn.map_or(*speed, |f| f(t));
            let Some(look) = brain_ref(t).look_target() else {
                return false;
            };
            brain(t).set(
                M::WalkTarget,
                MemValue::Walk(WalkTarget {
                    target: look,
                    speed: *speed,
                    close_enough: *close,
                }),
            );
            true
        }
        Behavior::BabyFollowAdult {
            min,
            max,
            speed,
            memory,
            target_eye,
            speed_fn,
        } => {
            if !t.mob_ref().baby {
                return false;
            }
            let speed = &speed_fn.map_or(*speed, |f| f(t));
            let Some(adult) = brain_ref(t).entity(*memory) else {
                return false;
            };
            if closer_than(t, adult, f64::from(*max + 1)) && !closer_than(t, adult, f64::from(*min))
            {
                let b = brain(t);
                b.set(
                    M::LookTarget,
                    MemValue::Look(Tracker::Entity {
                        id: adult,
                        track_eye: true,
                        target_eye: *target_eye,
                    }),
                );
                b.set(
                    M::WalkTarget,
                    MemValue::Walk(WalkTarget {
                        target: Tracker::Entity {
                            id: adult,
                            track_eye: *target_eye,
                            target_eye: *target_eye,
                        },
                        speed: *speed,
                        close_enough: *min - 1,
                    }),
                );
                true
            } else {
                false
            }
        }
        Behavior::BecomePassive { duration, .. } => {
            let b = brain(t);
            b.set_with_expiry(M::Pacified, MemValue::Bool(true), i64::from(*duration));
            b.erase(M::AttackTarget);
            true
        }
        Behavior::EraseMemoryIf { cond, memory } => {
            if cond(t) {
                if alive(t) {
                    brain(t).erase(*memory);
                }
                true
            } else {
                false
            }
        }
        Behavior::CopyMemoryWithExpiry {
            cond,
            source,
            target,
            min,
            max,
        } => {
            if !cond(t) {
                return false;
            }
            let Some(v) = brain_ref(t).get(*source).cloned() else {
                return false;
            };
            let ttl = t.env.random.next_int_between_inclusive(*min, *max);
            brain(t).set_with_expiry(*target, v, i64::from(ttl));
            true
        }
        Behavior::GoToWantedItem {
            cond,
            speed,
            max_dist,
            ..
        } => {
            let Some(item) = brain_ref(t).entity(M::NearestVisibleWantedItem) else {
                return false;
            };
            if brain_ref(t).has(M::ItemPickupCooldownTicks)
                || cond.is_some_and(|c| !c(t))
                || !closer_than(t, item, f64::from(*max_dist))
                || !t.mob_ref().can_pick_up_loot
            {
                return false;
            }
            let b = brain(t);
            b.set(M::LookTarget, MemValue::Look(Tracker::entity(item, true)));
            b.set(
                M::WalkTarget,
                MemValue::Walk(WalkTarget::entity(item, *speed, 0)),
            );
            true
        }
        Behavior::WalkAwayFrom {
            memory,
            speed,
            distance,
            interrupt,
        } => {
            let current = brain_ref(t).walk_target();
            if current.is_some() && !*interrupt {
                return false;
            }
            let avoid = match brain_ref(t).get(*memory) {
                Some(MemValue::Pos(p)) => randompos::bottom_center(*p),
                Some(MemValue::Entity(e)) => {
                    let Some(p) = entity_pos(t, *e) else {
                        return false;
                    };
                    p
                }
                _ => return false,
            };
            let me = t.pos();
            let d = f64::from(*distance);
            if me.distance_to_sqr(avoid) >= d * d {
                return false;
            }
            if let Some(w) = current
                && w.speed == *speed
                && let Some(cur) = tracker_pos(t, &w.target)
            {
                let cd = Vec3::new(cur.x - me.x, cur.y - me.y, cur.z - me.z);
                let ad = Vec3::new(avoid.x - me.x, avoid.y - me.y, avoid.z - me.z);
                if cd.x * ad.x + cd.y * ad.y + cd.z * ad.z < 0.0 {
                    return false;
                }
            }
            for _ in 0..10 {
                if let Some(p) = randompos::land_pos_away(t, 0.0, 16.0, 7, avoid) {
                    brain(t).set(M::WalkTarget, MemValue::Walk(WalkTarget::at(p, *speed, 0)));
                    break;
                }
            }
            true
        }
        Behavior::StopBeingAngryIfTargetDead => {
            let Some(uuid) = brain_ref(t).uuid(M::AngryAt) else {
                return false;
            };
            if let Some(id) = living_from_uuid(t, uuid) {
                let dead = t
                    .ents
                    .mob(id)
                    .map(crate::mob::MobData::is_dead_or_dying)
                    .or_else(|| t.ents.player(id).map(|p| p.survival.is_dead_or_dying()))
                    .unwrap_or(false);
                let is_player = t.ents.player(id).is_some();
                if dead
                    && (!is_player
                        || t.env
                            .rules
                            .bool(crate::survival::rules::Rule::FORGIVE_DEAD_PLAYERS))
                {
                    brain(t).erase(M::AngryAt);
                }
            }
            true
        }
        Behavior::TryFindLand {
            range,
            speed,
            next_ok,
        } => {
            if !crate::mob::is_water(t.level.block(t.block_pos())) {
                return false;
            }
            if now < *next_ok {
                *next_ok = now + 60;
                return true;
            }
            let me = t.block_pos();
            let mut found = None;
            for_manhattan(me, *range, *range, |p| {
                if p.x == me.x && p.z == me.z {
                    return false;
                }
                let state = t.level.block(p);
                if !state.fluid_state().is_empty() || !state.collision_shape().is_empty() {
                    return false;
                }
                let below = t.level.block(BlockPos::new(p.x, p.y - 1, p.z));
                if below.misc().sturdy_full & 2 == 0 {
                    return false;
                }
                found = Some(p);
                true
            });
            if let Some(p) = found {
                let b = brain(t);
                b.set(M::LookTarget, MemValue::Look(Tracker::block(p)));
                b.set(
                    M::WalkTarget,
                    MemValue::Walk(WalkTarget::block(p, *speed, 1)),
                );
            }
            *next_ok = now + 60;
            true
        }
        Behavior::TryFindWater {
            range,
            speed,
            next_ok,
        } => {
            if crate::mob::is_water(t.level.block(t.block_pos())) {
                return false;
            }
            if now < *next_ok {
                *next_ok = now + 22;
                return true;
            }
            let me = t.block_pos();
            let pos = t.pos();
            let mut found = None;
            for_manhattan(me, *range, *range, |p| {
                if p.x == me.x && p.z == me.z {
                    return false;
                }
                let state = t.level.block(p);
                if !(crate::mob::is_water(state) && state.block().name().ends_with("water")) {
                    return false;
                }
                if t.level.block(BlockPos::new(p.x, p.y + 1, p.z)).is_air() {
                    found = Some(p);
                    return true;
                }
                let c = randompos::bottom_center(p);
                let center = Vec3::new(c.x, c.y + 0.5, c.z);
                if found.is_none() && center.distance_to_sqr(pos) >= 1.5 * 1.5 {
                    found = Some(p);
                }
                false
            });
            if let Some(p) = found {
                let b = brain(t);
                b.set(M::LookTarget, MemValue::Look(Tracker::block(p)));
                b.set(
                    M::WalkTarget,
                    MemValue::Walk(WalkTarget::block(p, *speed, 0)),
                );
            }
            *next_ok = now + 40;
            true
        }
        Behavior::StrollToPoi {
            memory,
            speed,
            close,
            max_dist,
            next_ok,
        } => {
            let Some(MemValue::GlobalPos(g)) = brain_ref(t).get(*memory).cloned() else {
                return false;
            };
            let c = g.pos;
            let center = Vec3::new(
                f64::from(c.x) + 0.5,
                f64::from(c.y) + 0.5,
                f64::from(c.z) + 0.5,
            );
            let max = f64::from(*max_dist);
            if *g.dimension != *t.env.dimension || center.distance_to_sqr(t.pos()) >= max * max {
                return false;
            }
            if now <= *next_ok {
                return true;
            }
            brain(t).set(
                M::WalkTarget,
                MemValue::Walk(WalkTarget::block(c, *speed, *close)),
            );
            *next_ok = now + 80;
            true
        }
        Behavior::StrollAroundPoi {
            memory,
            speed,
            max_dist,
            next_ok,
        } => {
            let Some(MemValue::GlobalPos(g)) = brain_ref(t).get(*memory).cloned() else {
                return false;
            };
            let c = g.pos;
            let center = Vec3::new(
                f64::from(c.x) + 0.5,
                f64::from(c.y) + 0.5,
                f64::from(c.z) + 0.5,
            );
            let max = f64::from(*max_dist);
            if *g.dimension != *t.env.dimension || center.distance_to_sqr(t.pos()) >= max * max {
                return false;
            }
            if now <= *next_ok {
                return true;
            }
            let p = randompos::land_pos(t, 8, 6);
            brain(t).set_opt(
                M::WalkTarget,
                p.map(|p| MemValue::Walk(WalkTarget::at(p, *speed, 1))),
            );
            *next_ok = now + 180;
            true
        }
        Behavior::InteractWith {
            filter,
            range,
            memory,
            speed,
            stop,
        } => {
            let filter = *filter;
            let r_sqr = f64::from(*range * *range);
            if sensing::visible_find_closest(t, filter).is_none() {
                return false;
            }
            if let Some(id) =
                sensing::visible_find_closest(t, |t, id| dist_sqr(t, id) <= r_sqr && filter(t, id))
            {
                let b = brain(t);
                b.set(*memory, MemValue::Entity(id));
                b.set(M::LookTarget, MemValue::Look(Tracker::entity(id, true)));
                b.set(
                    M::WalkTarget,
                    MemValue::Walk(WalkTarget::entity(id, *speed, *stop)),
                );
            }
            true
        }
        Behavior::SetLookAndInteract { filter, range } => {
            let filter = *filter;
            let r_sqr = f64::from(*range * *range);
            let Some(id) =
                sensing::visible_find_closest(t, |t, id| dist_sqr(t, id) <= r_sqr && filter(t, id))
            else {
                return false;
            };
            let b = brain(t);
            b.set(M::InteractionTarget, MemValue::Entity(id));
            b.set(M::LookTarget, MemValue::Look(Tracker::entity(id, true)));
            true
        }
        Behavior::GoToTargetLocation {
            memory,
            close,
            speed,
        } => {
            let Some(loc) = brain_ref(t).pos(*memory) else {
                return false;
            };
            let me = t.block_pos();
            let c = f64::from(*close);
            if loc.dist_sqr(me) >= c * c {
                let r = &mut *t.env.random;
                let dx = r.next_int_bounded(3) - 1;
                let dz = r.next_int_bounded(3) - 1;
                let near = BlockPos::new(loc.x + dx, loc.y, loc.z + dz);
                set_walk_and_look(t, Tracker::block(near), *speed, *close);
            }
            true
        }
        Behavior::BackUpIfTooClose { distance, speed } => {
            let Some(target) = brain_ref(t).entity(M::AttackTarget) else {
                return false;
            };
            if !(closer_than(t, target, f64::from(*distance))
                && sensing::visible_contains(t, target))
            {
                return false;
            }
            brain(t).set(M::LookTarget, MemValue::Look(Tracker::entity(target, true)));
            t.move_ctl().strafe(-*speed, 0.0);
            let rot = t.core().rot();
            let mut r = rot;
            r.y_rot = nav::rotate_if_necessary(rot.y_rot, rot.y_head_rot, 0.0);
            t.core_mut().set_rot(r);
            true
        }
        Behavior::Mount { speed } => {
            if t.ents.riding.vehicle_of(t.id).is_some() {
                return false;
            }
            let Some(vehicle) = brain_ref(t).entity(M::RideTarget) else {
                return false;
            };
            if closer_than(t, vehicle, 1.0) {
                let me = t.id;
                t.ents.riding.add(me, vehicle, false);
            } else {
                let b = brain(t);
                b.set(
                    M::LookTarget,
                    MemValue::Look(Tracker::entity(vehicle, true)),
                );
                b.set(
                    M::WalkTarget,
                    MemValue::Walk(WalkTarget::entity(vehicle, *speed, 1)),
                );
            }
            true
        }
        Behavior::Dismount {
            max_dist,
            dont_ride_if,
        } => {
            let current = t.ents.riding.vehicle_of(t.id);
            let target = brain_ref(t).entity(M::RideTarget);
            let Some(vehicle) = current.or(target) else {
                return false;
            };
            let valid = is_alive(t, vehicle) && closer_than(t, vehicle, f64::from(*max_dist));
            if valid && !dont_ride_if(t, vehicle) {
                return false;
            }
            let me = t.id;
            t.ents.riding.remove(me);
            brain(t).erase(M::RideTarget);
            true
        }
        Behavior::Fam(FamBehavior::Brainy(f)) => {
            crate::mob::fam::brainy::behavior_trigger(t, f, now)
        }
        Behavior::Fam(FamBehavior::Mount(f)) => crate::mob::fam::mount::behavior_trigger(t, f, now),
        Behavior::Fam(FamBehavior::Village(f)) => {
            crate::mob::fam::village::behavior_trigger(t, f, now)
        }
        Behavior::Fam(FamBehavior::Warden(f)) => {
            crate::mob::boss::warden::ai::behavior_trigger(t, f, now)
        }
        _ => false,
    }
}

/// `RandomStroll.getTargetSwimPos`.
fn swim_pos(t: &mut Tick<'_, '_>) -> Option<Vec3> {
    const TIERS: [(i32, i32); 6] = [(1, 1), (3, 3), (5, 5), (6, 5), (7, 7), (10, 7)];
    let mut fallback: Option<Vec3> = None;
    let mut target = None;
    for (h, v) in TIERS {
        target = match fallback {
            None => swimmable_pos(t, h, v),
            Some(f) => {
                let p = t.pos();
                let d = Vec3::new(f.x - p.x, f.y - p.y, f.z - p.z);
                let len = d.length();
                if len < 1.0e-4 {
                    Some(p)
                } else {
                    Some(Vec3::new(
                        p.x + d.x / len * f64::from(h),
                        p.y + d.y / len * f64::from(v),
                        p.z + d.z / len * f64::from(h),
                    ))
                }
            }
        };
        let restrict = randompos::mob_restricted(t, f64::from(h));
        let Some(tp) = target else {
            return fallback;
        };
        let block = BlockPos::containing(tp.x, tp.y, tp.z);
        if t.level.block(block).fluid_state().is_empty()
            || randompos::is_restricted(t, restrict, block)
        {
            return fallback;
        }
        fallback = target;
    }
    target
}

/// `BehaviorUtils.getRandomSwimmablePos`.
fn swimmable_pos(t: &mut Tick<'_, '_>, h: i32, v: i32) -> Option<Vec3> {
    let mut p = randompos::default_pos(t, h, v);
    let mut count = 0;
    while let Some(tp) = p {
        let state = t.level.block(BlockPos::containing(tp.x, tp.y, tp.z));
        let swimmable = crate::mob::is_water(state) && state.collision_shape().is_empty();
        if swimmable || count >= 10 {
            break;
        }
        count += 1;
        p = randompos::default_pos(t, h, v);
    }
    p
}

/// Whether two `Behavior` values are of the same variant (tests).
pub fn same_variant(a: &Behavior, b: &Behavior) -> bool {
    std::mem::discriminant(a) == std::mem::discriminant(b)
}

/// Unused-binding guard for readers of [`Brain`].
#[allow(dead_code)]
fn _brain(_: &Brain) {}
