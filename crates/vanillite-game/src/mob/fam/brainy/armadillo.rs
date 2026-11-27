//! Armadillos (`Armadillo`, `ArmadilloAi`): they roll up when scared by the undead, sprinting
//! or riding players and attackers, shed scutes and can be brushed.

use vanillite_base::BlockPos;
use vanillite_base::random::RandomSource;
use vanillite_nbt::{Compound, Tag};
use vanillite_world::entity::EntityId;

use super::{make_sound, nbt_int, spec, state, state_mut};
use crate::level::Level;
use crate::mob::animal::Interaction;
use crate::mob::brain::behavior::{self as bh, Shape, Spec};
use crate::mob::brain::memory::MemoryStatus as S;
use crate::mob::brain::{
    self, Activity, ActivitySpec, Brain, BrainSpec, MemValue, MemoryStatus, MemoryType as M,
    SensorKind, brain_ref,
};
use crate::mob::fam::Interact;
use crate::mob::{Environment, MobData, Tick};
use crate::player::encode::DataWriter;
use crate::survival::damage::tag;

/// `ArmadilloState` ids.
pub const IDLE: u8 = 0;
pub const ROLLING: u8 = 1;
pub const SCARED: u8 = 2;
pub const UNROLLING: u8 = 3;
const NAMES: [&str; 4] = ["idle", "rolling", "scared", "unrolling"];

/// The armadillo behaviours.
#[derive(Clone, Copy, Debug)]
pub enum Behavior {
    /// `ARMADILLO_ROLLING_OUT`.
    RollingOut,
    /// `ArmadilloBallUp`.
    BallUp {
        peek_timer: i32,
        danger_was_around: bool,
    },
}

fn ar(b: Behavior) -> Spec {
    spec(super::Behavior::Armadillo(b))
}

fn is_player(t: &Tick<'_, '_>, id: EntityId) -> bool {
    t.ents.player(id).is_some()
}

fn brain_spec() -> BrainSpec {
    let mut panic = Spec::animal_panic(2.0);
    // `ArmadilloPanic`: environmental causes only, rolling out first.
    if let Some(bh::Behavior::AnimalPanic { causes, .. }) = panic.behavior_mut() {
        *causes = tag::PANIC_ENVIRONMENTAL_CAUSES;
    }
    let core = ActivitySpec::new(
        Activity::Core,
        0,
        vec![
            Spec::swim(0.8),
            panic,
            Spec::look_at_target_sink(45, 90),
            Spec::move_to_target_sink(),
            Spec::count_down(M::TemptationCooldownTicks),
            Spec::count_down(M::GazeCooldownTicks),
            ar(Behavior::RollingOut),
        ],
    );
    let idle = ActivitySpec::with_priorities(
        Activity::Idle,
        vec![
            (
                0,
                Spec::look_at_entity_sometimes(Some(is_player), 6.0, 30, 60),
            ),
            (1, Spec::make_love(crate::mob::Kind::Armadillo, 1.0, 1)),
            (
                2,
                Spec::run_one(vec![
                    (Spec::follow_temptation(|_| 1.25, 2.0, false), 1),
                    (Spec::baby_follow_adult(5, 16, 1.25), 1),
                ]),
            ),
            (3, Spec::random_look_around(150, 250, 30.0, 0.0, 0.0)),
            (
                4,
                Spec::run_one_if(
                    &[(M::WalkTarget, S::ValueAbsent)],
                    vec![
                        (Spec::stroll(1.0), 1),
                        (Spec::walk_to_look_target(1.0, 3), 1),
                        (Spec::do_nothing(30, 60), 1),
                    ],
                ),
            ),
        ],
    );
    let scared = ActivitySpec::with_priorities(
        Activity::Panic,
        vec![(
            0,
            ar(Behavior::BallUp {
                peek_timer: 0,
                danger_was_around: false,
            }),
        )],
    )
    .requires(&[
        (M::DangerDetectedRecently, S::ValuePresent),
        (M::IsPanicking, S::ValueAbsent),
    ]);
    BrainSpec {
        sensors: vec![
            SensorKind::NearestLivingEntities,
            SensorKind::HurtBy,
            SensorKind::Tempting(None),
            SensorKind::NearestAdult,
            SensorKind::Mob {
                scan_rate: 5,
                test: is_scared_by,
                ready: can_stay_rolled_up,
                memory: M::DangerDetectedRecently,
                ttl: 80,
            },
        ],
        activities: vec![core, idle, scared],
        ..BrainSpec::default()
    }
}

pub fn init(m: &mut MobData) {
    m.brain = Some(Box::new(Brain::new(brain_spec(), &mut m.random)));
    m.nav.can_float = true;
    let time = next_scute_time(m);
    state_mut(m).scute_time = time;
}

/// `pickNextScuteDropTime`: 5 to 10 minutes.
fn next_scute_time(m: &mut MobData) -> i32 {
    m.random.next_int_bounded(6000) + 6000
}

fn armadillo_state(m: &MobData) -> u8 {
    state(m).armadillo_state
}

/// `isScared`.
pub fn is_scared(m: &MobData) -> bool {
    armadillo_state(m) != IDLE
}

/// `switchToState`.
fn switch_to(t: &mut Tick<'_, '_>, s: u8) {
    let st = state_mut(t.mob());
    if st.armadillo_state != s {
        st.armadillo_state = s;
        st.in_state_ticks = 0;
        t.core_mut().mark_data_dirty();
    }
}

/// `isScaredBy`.
fn is_scared_by(t: &Tick<'_, '_>, id: EntityId) -> bool {
    let Some(e) = t.ents.store.get(id) else {
        return false;
    };
    if !t.aabb().inflate(7.0, 2.0, 7.0).intersects(&e.aabb()) {
        return false;
    }
    if brain::sensor::in_entity_tag(t, id, "minecraft:undead") {
        return true;
    }
    if t.mob_ref().last_hurt_by_mob == Some(id) {
        return true;
    }
    if let Some(p) = t.ents.player(id) {
        if p.game_mode == vanillite_config::GameMode::Spectator {
            return false;
        }
        let sprinting = e
            .flags()
            .contains(vanillite_world::entity::EntityFlags::SPRINTING);
        return sprinting || t.ents.riding.vehicle_of(id).is_some();
    }
    false
}

/// `canStayRolledUp`.
fn can_stay_rolled_up(t: &Tick<'_, '_>) -> bool {
    let f = t.mob_ref().fluids;
    !brain_ref(t).has(M::IsPanicking)
        && !(f.in_water() || f.in_lava())
        && crate::mob::tame::leash_holder(t.ents, t.id).is_none()
        && t.ents.riding.vehicle_of(t.id).is_none()
        && t.ents.riding.passengers_of(t.id).is_empty()
}

/// `rollUp`.
fn roll_up(t: &mut Tick<'_, '_>) {
    if is_scared(t.mob_ref()) {
        return;
    }
    // `stopInPlace`.
    t.mob().nav.stop();
    let m = t.mob();
    m.xxa = 0.0;
    m.zza = 0.0;
    m.speed = 0.0;
    m.in_love = 0;
    t.set_velocity(vanillite_base::Vec3::new(0.0, 0.0, 0.0));
    make_sound(t, "minecraft:entity.armadillo.roll");
    switch_to(t, ROLLING);
}

/// `rollOut`.
fn roll_out(t: &mut Tick<'_, '_>) {
    if !is_scared(t.mob_ref()) {
        return;
    }
    make_sound(t, "minecraft:entity.armadillo.unroll_finish");
    switch_to(t, IDLE);
}

/// `ArmadilloAi.updateActivity`, then the scute shedding of `customServerAiStep`.
pub fn update_activity(t: &mut Tick<'_, '_>) {
    brain::brain(t).set_active_activity_to_first_valid(&[Activity::Panic, Activity::Idle]);
    if !t.mob_ref().is_alive() {
        return;
    }
    let s = state_mut(t.mob());
    s.scute_time -= 1;
    if s.scute_time <= 0 {
        if t.env.rules.bool(crate::survival::rules::Rule::MOB_DROPS) {
            let items = super::gift_loot(t, "minecraft:gameplay/armadillo_shed");
            if !items.is_empty() {
                for i in items {
                    crate::mob::equipment::spawn_at_location(t, i, 0.0);
                }
                let r = &mut t.mob().random;
                let pitch = (r.next_float() - r.next_float()) * 0.2 + 1.0;
                crate::mob::kinds::play_sound(
                    t,
                    "minecraft:entity.armadillo.scute_drop",
                    1.0,
                    pitch,
                );
            }
        }
        let time = next_scute_time(t.mob());
        state_mut(t.mob()).scute_time = time;
    }
}

/// `tick`: the time in the current state.
pub fn before_tick(t: &mut Tick<'_, '_>) {
    state_mut(t.mob()).in_state_ticks += 1;
}

/// `tick` after `super.tick`: a scared armadillo keeps its head with its body.
pub fn after_tick(t: &mut Tick<'_, '_>) {
    if is_scared(t.mob_ref()) {
        crate::mob::kinds::clamp_head_rotation_to_body(t);
    }
}

pub fn ai_step(_t: &mut Tick<'_, '_>) {}

/// `Animal.getWalkTargetValue`.
pub fn walk_target_value(level: &Level, env: &Environment, pos: BlockPos) -> f64 {
    crate::mob::kinds::base_walk_target_value(false, level, env, pos)
}

/// `hurtServer`: rolled up armadillos take `(damage - 1) / 2`.
pub fn hurt_filter(h: &mut crate::mob::fam::Hurt<'_, '_>, amount: f32) -> Option<f32> {
    let scared = h.ents.mob(h.target).is_some_and(is_scared);
    Some(if scared { (amount - 1.0) / 2.0 } else { amount })
}

/// `actuallyHurt`: living attackers make it roll up, environmental damage unrolls it.
pub fn actually_hurt(
    t: &mut Tick<'_, '_>,
    living: bool,
    kind: crate::survival::damage::DamageKind,
) {
    if t.mob_ref().no_ai || t.mob_ref().is_dead_or_dying() {
        return;
    }
    if living {
        brain::brain(t).set_with_expiry(M::DangerDetectedRecently, MemValue::Bool(true), 80);
        if can_stay_rolled_up(t) {
            roll_up(t);
        }
    } else if t.env.types.is(kind, tag::PANIC_ENVIRONMENTAL_CAUSES) {
        roll_out(t);
    }
}

pub fn ambient_sound(m: &MobData) -> Option<&'static str> {
    (!is_scared(m)).then_some("minecraft:entity.armadillo.ambient")
}

pub fn hurt_sound(m: &MobData) -> &'static str {
    if is_scared(m) {
        "minecraft:entity.armadillo.hurt_reduced"
    } else {
        "minecraft:entity.armadillo.hurt"
    }
}

/// `mobInteract`: brushing off a scute; scared armadillos refuse the rest.
pub fn interact(ix: &mut Interact<'_, '_>) -> Interaction {
    let name = crate::item::item_id(ix.stack).name();
    let Some(m) = ix.ents.mob(ix.target) else {
        return Interaction::Pass;
    };
    if name == "minecraft:brush" && !m.baby {
        let target = ix.target;
        let mut t = Tick {
            level: ix.level,
            ents: &mut *ix.ents,
            env: &mut *ix.env,
            cx: &mut *ix.cx,
            id: target,
        };
        let items = super::gift_loot(&mut t, "minecraft:entities/armadillo_brush");
        for i in items {
            crate::mob::equipment::spawn_at_location(&mut t, i, 0.0);
        }
        crate::mob::kinds::play_sound(&mut t, "minecraft:entity.armadillo.brush", 1.0, 1.0);
        crate::mob::animal::hurt_held_pub(ix, 16);
        return Interaction::Success;
    }
    if is_scared(m) {
        return Interaction::Consume;
    }
    let food = crate::mob::animal::is_food(crate::mob::Kind::Armadillo, ix.stack.item);
    let r = crate::mob::animal::animal_interact_pub(ix);
    if r.consumes() && food {
        let target = ix.target;
        let mut t = Tick {
            level: ix.level,
            ents: &mut *ix.ents,
            env: &mut *ix.env,
            cx: &mut *ix.cx,
            id: target,
        };
        make_sound(&mut t, "minecraft:entity.armadillo.eat");
    }
    r
}

/// Synched data: baby (16), state (17).
pub fn encode(w: &mut DataWriter<'_>, m: &MobData) {
    w.boolean(16, m.baby, false);
    w.varint(
        17,
        crate::player::encode::serializer::ARMADILLO_STATE,
        i32::from(armadillo_state(m)),
        0,
    );
}

pub fn save(m: &MobData, out: &mut Compound) {
    out.insert("state", NAMES[usize::from(armadillo_state(m).min(3))]);
    out.insert("scute_time", state(m).scute_time);
}

pub fn load(m: &mut MobData, tag: &Compound) {
    if let Some(Tag::String(s)) = tag.get("state")
        && let Some(i) = NAMES.iter().position(|n| n == s)
    {
        state_mut(m).armadillo_state = u8::try_from(i).unwrap_or(0);
    }
    if tag.get("scute_time").is_some() {
        state_mut(m).scute_time = nbt_int(tag, "scute_time", 6000);
    }
}

// --- Behaviours -----------------------------------------------------------------------------

pub fn duration(b: &Behavior) -> (i32, i32) {
    match b {
        Behavior::BallUp { .. } => (6000, 6000),
        Behavior::RollingOut => (60, 60),
    }
}

pub fn shape(b: &Behavior) -> Shape {
    match b {
        Behavior::BallUp { .. } => Shape::Timed,
        Behavior::RollingOut => Shape::OneShot,
    }
}

pub fn requirements(b: &Behavior) -> &'static [(M, MemoryStatus)] {
    match b {
        Behavior::RollingOut => &[(M::DangerDetectedRecently, S::ValueAbsent)],
        Behavior::BallUp { .. } => &[],
    }
}

pub fn no_timeout(_b: &Behavior) -> bool {
    false
}

pub fn check(b: &mut Behavior, t: &mut Tick<'_, '_>) -> bool {
    match b {
        Behavior::BallUp { .. } => {
            let f = t.mob_ref().fluids;
            t.on_ground() && !(f.in_water() || f.in_lava())
        }
        Behavior::RollingOut => true,
    }
}

pub fn can_still_use(b: &mut Behavior, t: &mut Tick<'_, '_>, _now: i64) -> bool {
    match b {
        // `ArmadilloState.isThreatened`.
        Behavior::BallUp { .. } => armadillo_state(t.mob_ref()) != IDLE,
        Behavior::RollingOut => false,
    }
}

pub fn start(b: &mut Behavior, t: &mut Tick<'_, '_>, _now: i64) {
    if let Behavior::BallUp { .. } = b {
        roll_up(t);
    }
}

/// `pickNextPeekTimer`.
fn next_peek(t: &mut Tick<'_, '_>) -> i32 {
    50 + t.mob().random.next_int_between_inclusive(100, 400)
}

pub fn tick(b: &mut Behavior, t: &mut Tick<'_, '_>, _now: i64) {
    let Behavior::BallUp {
        peek_timer,
        danger_was_around,
    } = b
    else {
        return;
    };
    if *peek_timer > 0 {
        *peek_timer -= 1;
    }
    let s = state(t.mob_ref());
    // `shouldSwitchToScaredState`: rolled for longer than the rolling animation.
    if s.armadillo_state == ROLLING && s.in_state_ticks > 10 {
        switch_to(t, SCARED);
        if t.on_ground() {
            crate::mob::kinds::play_sound(t, "minecraft:entity.armadillo.land", 1.0, 1.0);
        }
        return;
    }
    let danger_ticks = brain_ref(t).ttl(M::DangerDetectedRecently);
    let danger = danger_ticks > 75;
    if danger != *danger_was_around {
        *peek_timer = next_peek(t);
    }
    *danger_was_around = danger;
    match armadillo_state(t.mob_ref()) {
        SCARED => {
            if *peek_timer == 0 && t.on_ground() && danger {
                super::entity_event(t, 64);
                *peek_timer = next_peek(t);
            }
            if danger_ticks < 30 {
                crate::mob::kinds::play_sound(
                    t,
                    "minecraft:entity.armadillo.unroll_start",
                    1.0,
                    1.0,
                );
                switch_to(t, UNROLLING);
            }
        }
        UNROLLING if danger_ticks > 30 => switch_to(t, SCARED),
        _ => {}
    }
}

pub fn stop(b: &mut Behavior, t: &mut Tick<'_, '_>, _now: i64) {
    if let Behavior::BallUp { .. } = b
        && !can_stay_rolled_up(t)
    {
        roll_out(t);
    }
}

pub fn trigger(b: &mut Behavior, t: &mut Tick<'_, '_>, _now: i64) -> bool {
    match b {
        Behavior::RollingOut => {
            if is_scared(t.mob_ref()) {
                roll_out(t);
                true
            } else {
                false
            }
        }
        Behavior::BallUp { .. } => false,
    }
}

#[allow(dead_code)]
fn _unused(t: &Tick<'_, '_>) -> bool {
    bh::is_alive(t, t.id)
}
