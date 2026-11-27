//! The warden (`Warden`, `WardenAi`): a brain driven mob that hears vibrations, sniffs, gets
//! angry at suspects (`AngerManagement`), roars, fights with melee and the sonic boom, gives
//! darkness to the players around and digs back into the ground when left alone.
//!
//! Its vibration listener is an entity game event listener ([`Listener::Entity`]) registered in
//! the section of its eyes; the registration follows the warden after each of its ticks (as a
//! [`LevelAction`]) and is dropped lazily once the warden is gone.

pub mod ai;
pub mod anger;

use vanillite_base::random::RandomSource;
use vanillite_base::{Aabb, BlockPos, Vec3};
use vanillite_config::GameMode;
use vanillite_data::entity::ids as entity_ids;
use vanillite_nbt::{Compound, Tag};
use vanillite_world::entity::{EntityId, Pose, Rotation};

use self::anger::{AngerLevel, AngerManagement, SuspectState};
use super::data;
use crate::block::sculk_vibration::{self, Listenable, Vibration, VibrationData};
use crate::effect::{EffectInstance, MobEffect};
use crate::entities::Entities;
use crate::level::Level;
use crate::level::game_event::{EntityListener, GameEventContext, Listener, containing};
use crate::mob::brain::{Brain, MemValue, MemoryType as M, Tracker};

/// The warden's memories: its brain.
type Memories = Brain;
use crate::mob::{Kind, MobContext, MobData, Tick, WorldAction};
use crate::survival::damage::{DamageSource, tag};
use crate::survival::player::Env;

/// `Warden.VibrationUser.GAME_EVENT_LISTENER_RANGE`.
const LISTENER_RANGE: i32 = 16;
/// The warden's eye height (`EntityPositionSource(warden, getEyeHeight())` at creation).
const EYE_HEIGHT: f64 = 2.55;
/// `Warden.DARKNESS_DURATION`.
const DARKNESS_DURATION: i32 = 260;
/// `Warden.DARKNESS_DISPLAY_LIMIT`.
const DARKNESS_DISPLAY_LIMIT: i32 = 200;
/// `Warden.DARKNESS_RADIUS`.
const DARKNESS_RADIUS: f64 = 20.0;
/// `Warden.DEFAULT_ANGER`.
const DEFAULT_ANGER: i32 = 35;
/// `Warden.PROJECTILE_ANGER`.
const PROJECTILE_ANGER: i32 = 10;
/// `Warden.ON_HURT_ANGER_BOOST`.
const ON_HURT_ANGER_BOOST: i32 = 20;
/// `WardenAi.DIGGING_COOLDOWN`.
pub const DIGGING_COOLDOWN: i64 = 1200;
/// `WardenAi.DISTURBANCE_LOCATION_EXPIRY_TIME`.
const DISTURBANCE_EXPIRY: i64 = 100;

/// The kind state.
#[derive(Debug, Default)]
pub struct State {
    /// `angerManagement`.
    pub anger: AngerManagement,
    /// `vibrationData`.
    pub vibration: VibrationData,
    /// The registered game event listener.
    pub listener: Option<Listener>,
    /// `CLIENT_ANGER_LEVEL`.
    pub client_anger: i32,
}

impl Clone for State {
    /// Kind states are cloned only for copies of a whole mob (tests).
    fn clone(&self) -> Self {
        State {
            anger: self.anger.clone(),
            vibration: self.vibration,
            listener: None,
            client_anger: self.client_anger,
        }
    }
}

/// Level changes of the warden and sculk machinery, applied right after the mob tick that
/// queued them (the tick only reads the level).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LevelAction {
    /// `gameEvent(event, pos, context)`.
    GameEvent {
        event: u16,
        pos: Vec3,
        context: GameEventContext,
    },
    /// Moves a game event listener between sections.
    Listener {
        remove: Option<Listener>,
        add: Option<Listener>,
    },
    /// A mob died next to a sculk catalyst: the catalyst takes its experience.
    SculkDeath { pos: Vec3, experience: i32 },
    /// A lightning bolt at `pos` (`summon_entity` of Channeling).
    Lightning { pos: Vec3 },
}

impl LevelAction {
    /// Applies the change.
    pub fn apply(self, level: &mut Level) {
        match self {
            LevelAction::GameEvent {
                event,
                pos,
                context,
            } => level.game_event(event, pos, context),
            LevelAction::Listener { remove, add } => {
                let listeners = level.game_event_listeners();
                if let Some(l) = remove {
                    listeners.unregister(l);
                }
                if let Some(l) = add {
                    listeners.register(l);
                }
            }
            LevelAction::SculkDeath { pos, experience } => {
                crate::block::sculk_catalyst::on_mob_death(level, pos, experience);
            }
            LevelAction::Lightning { pos } => {
                crate::lightning::spawn(level, pos, false);
            }
        }
    }
}

fn state(ents: &Entities, id: EntityId) -> Option<&State> {
    ents.mob(id)?.boss.warden()
}

fn state_mut(ents: &mut Entities, id: EntityId) -> Option<&mut State> {
    ents.mob_mut(id)?.boss.warden_mut()
}

/// Runs `f` with the warden's memories taken out of its brain (outside the brain tick, where
/// the brain hands them to the behaviors itself).
fn with_memories<R>(
    ents: &mut Entities,
    id: EntityId,
    f: impl FnOnce(&mut Entities, &mut Memories) -> R,
) -> Option<R> {
    let mut mem = ents.mob_mut(id)?.brain.take()?;
    let r = f(ents, &mut mem);
    if let Some(m) = ents.mob_mut(id) {
        m.brain = Some(mem);
    }
    Some(r)
}

/// `registerGoals`: the warden has no goals; its brain is built here (`makeBrain`).
pub fn register_goals(m: &mut MobData) {
    m.nav.can_float = true;
    let delay = m.random.next_int_bounded(3);
    ai::make_brain(m);
    if let Some(s) = m.boss.warden_mut() {
        s.anger = AngerManagement::new(delay);
    }
}

/// Eye height of a warden listener (`None` if `id` is not a warden).
pub fn listener_eye_height(level: &Level, id: EntityId) -> Option<f64> {
    (level.entities.mob(id)?.kind == Kind::Warden).then_some(EYE_HEIGHT)
}

/// `isDiggingOrEmerging`.
fn is_digging_or_emerging(ents: &Entities, id: EntityId) -> bool {
    ents.store
        .get(id)
        .is_some_and(|e| matches!(e.pose(), Pose::Digging | Pose::Emerging))
}

/// `canTargetEntity(entity)`: a living entity that is not creative or spectating, not a
/// warden, alive.
pub fn can_target_entity(ents: &Entities, entity: Option<EntityId>) -> bool {
    let Some(entity) = entity else {
        return false;
    };
    if let Some(p) = ents.player(entity) {
        return !p.survival.is_dead_or_dying()
            && matches!(p.game_mode, GameMode::Survival | GameMode::Adventure);
    }
    ents.mob(entity)
        .is_some_and(|m| m.is_alive() && m.kind != Kind::Warden)
}

/// `getTarget()`: the attack target memory.
fn target(mem: &Memories) -> Option<EntityId> {
    mem.entity(M::AttackTarget)
}

/// `getAngerLevel()`.
fn anger_level(ents: &Entities, id: EntityId, mem: &Memories) -> AngerLevel {
    let anger = state(ents, id).map_or(0, |s| s.anger.active_anger(target(mem)));
    AngerLevel::by_anger(anger)
}

/// `getEntityAngryAt()`.
pub fn entity_angry_at(ents: &Entities, id: EntityId, mem: &Memories) -> Option<EntityId> {
    if !anger_level(ents, id, mem).is_angry() {
        return None;
    }
    state(ents, id)?
        .anger
        .top_suspect(|e| can_target_entity(ents, Some(e)))
}

/// `getVoicePitch`.
fn voice_pitch(ents: &mut Entities, id: EntityId) -> f32 {
    let Some(m) = ents.mob_mut(id) else {
        return 1.0;
    };
    (m.random.next_float() - m.random.next_float()) * 0.2 + 1.0
}

/// `playSound(sound, volume, pitch)`.
fn play_sound(
    ents: &mut Entities,
    env: &mut Env<'_>,
    id: EntityId,
    sound: &str,
    volume: f32,
    pitch: f32,
) {
    let Some(pos) = ents.store.get(id).map(|e| e.pos()) else {
        return;
    };
    let Some(m) = ents.mob_mut(id) else {
        return;
    };
    if m.silent {
        return;
    }
    let seed = crate::survival::sound_seed();
    let source = m.kind.sound_source();
    crate::survival::play_sound(env.out, None, pos, sound, source, volume, pitch, seed);
}

/// Broadcasts an entity event of the warden.
fn entity_event(ents: &Entities, env: &mut Env<'_>, id: EntityId, event: i8) {
    let Some(e) = ents.store.get(id) else {
        return;
    };
    env.out.push(crate::survival::Effect::Tracking {
        entity: id,
        with_self: false,
        packet: vanillite_protocol::SharedPacket::of(
            &vanillite_protocol::packets::play::clientbound::EntityEvent {
                entity_id: e.network_id().0,
                event,
            },
        ),
    });
}

/// `WardenAi.setDigCooldown`.
pub fn set_dig_cooldown(mem: &mut Memories) {
    if mem.has(M::DigCooldown) {
        mem.set_with_expiry(M::DigCooldown, MemValue::Unit, DIGGING_COOLDOWN);
    }
}

/// `SonicBoom.setCooldown`.
pub fn set_sonic_boom_cooldown(mem: &mut Memories, cooldown: i64) {
    mem.set_with_expiry(M::SonicBoomCooldown, MemValue::Unit, cooldown);
}

/// `setAttackTarget(target)`.
pub fn set_attack_target(mem: &mut Memories, target: EntityId) {
    mem.erase(M::RoarTarget);
    mem.set(M::AttackTarget, MemValue::Entity(target));
    mem.erase(M::CantReachWalkTargetSince);
    set_sonic_boom_cooldown(mem, 200);
}

/// `increaseAngerAt(entity, amount, playSound)`.
pub fn increase_anger_at(
    ents: &mut Entities,
    env: &mut Env<'_>,
    id: EntityId,
    mem: &mut Memories,
    entity: Option<EntityId>,
    amount: i32,
    play_listening: bool,
) {
    let no_ai = ents.mob(id).is_none_or(|m| m.no_ai);
    if no_ai || !can_target_entity(ents, entity) {
        return;
    }
    let Some(entity) = entity else { return };
    set_dig_cooldown(mem);
    let maybe_switch = !target(mem).is_some_and(|t| ents.player(t).is_some());
    let uuid = ents.store.get(entity).map_or(0, |e| e.uuid());
    let player = ents.player(entity).is_some();
    let Some(s) = state_mut(ents, id) else { return };
    let new = s.anger.increase_anger(entity, uuid, player, amount);
    if player && maybe_switch && AngerLevel::by_anger(new).is_angry() {
        mem.erase(M::AttackTarget);
    }
    if play_listening {
        play_listening_sound(ents, env, id, mem);
    }
}

/// `playListeningSound`.
fn play_listening_sound(ents: &mut Entities, env: &mut Env<'_>, id: EntityId, mem: &Memories) {
    if ents
        .store
        .get(id)
        .is_some_and(|e| e.pose() == Pose::Roaring)
    {
        return;
    }
    let sound = anger_level(ents, id, mem).listening_sound();
    let pitch = voice_pitch(ents, id);
    play_sound(ents, env, id, sound, 10.0, pitch);
}

/// `WardenAi.setDisturbanceLocation`.
pub fn set_disturbance_location(ents: &Entities, id: EntityId, mem: &mut Memories, pos: BlockPos) {
    if entity_angry_at(ents, id, mem).is_some() || mem.has(M::AttackTarget) {
        return;
    }
    set_dig_cooldown(mem);
    mem.set_with_expiry(M::SniffCooldown, MemValue::Unit, DISTURBANCE_EXPIRY);
    mem.set_with_expiry(
        M::LookTarget,
        MemValue::Look(Tracker::block(pos)),
        DISTURBANCE_EXPIRY,
    );
    mem.set_with_expiry(
        M::DisturbanceLocation,
        MemValue::Pos(pos),
        DISTURBANCE_EXPIRY,
    );
    mem.erase(M::WalkTarget);
}

/// `Warden.applyDarknessAround` (`MobEffectUtil.addEffectToPlayersAround` with darkness for
/// 260 ticks, refreshed when less than 200 are left).
pub fn apply_darkness_around(
    ents: &mut Entities,
    position: Vec3,
    source: Option<EntityId>,
    radius: f64,
) {
    let _ = source;
    let darkness = EffectInstance::with_flags(
        MobEffect::DARKNESS,
        DARKNESS_DURATION,
        0,
        false,
        false,
        false,
    );
    let mut targets = Vec::new();
    for (id, p) in ents.players.iter() {
        if !matches!(p.game_mode, GameMode::Survival | GameMode::Adventure) {
            continue;
        }
        let Some(e) = ents.store.get(id) else {
            continue;
        };
        if position.distance_to_sqr(e.pos()) >= radius * radius {
            continue;
        }
        let refresh = p.survival.effects.get(darkness.effect).is_none_or(|have| {
            have.amplifier < darkness.amplifier || have.ends_within(DARKNESS_DISPLAY_LIMIT - 1)
        });
        if refresh {
            targets.push(id);
        }
    }
    for id in targets {
        ents.add_effect(id, darkness.clone(), true);
    }
}

/// `Warden.tick` before `super.tick()`: the vibration ticker, and persistent wardens never
/// dig.
pub fn before_tick(t: &mut Tick<'_, '_>) {
    tick_vibrations(t);
    if !t.exists() {
        return;
    }
    if t.mob_ref().persistent {
        let id = t.id;
        with_memories(t.ents, id, |_, mem| set_dig_cooldown(mem));
    }
}

/// `VibrationSystem.Ticker.tick` of the warden.
fn tick_vibrations(t: &mut Tick<'_, '_>) {
    let time = t.game_time();
    let Some(s) = t.mob().boss.warden_mut() else {
        return;
    };
    s.vibration.try_select(time);
    let Some(vibration) = s.vibration.current else {
        return;
    };
    s.vibration.travel_time = (s.vibration.travel_time - 1).max(0);
    if s.vibration.travel_time > 0 {
        return;
    }
    // `receiveVibration` (no adjacent chunk requirement for wardens).
    let origin = containing(vibration.pos);
    let p = t.pos();
    let destination = BlockPos::containing(p.x, p.y + EYE_HEIGHT, p.z);
    let source = vibration.source.filter(|&e| t.ents.store.get(e).is_some());
    let owner = vibration
        .projectile_owner
        .filter(|&e| t.ents.store.get(e).is_some());
    let distance = crate::level::game_event::dist_sqr(origin, destination).sqrt() as f32;
    on_receive_vibration(t, origin, source, owner, distance);
    if let Some(s) = t.mob().boss.warden_mut() {
        s.vibration.current = None;
    }
}

/// `VibrationUser.onReceiveVibration`.
fn on_receive_vibration(
    t: &mut Tick<'_, '_>,
    pos: BlockPos,
    source: Option<EntityId>,
    owner: Option<EntityId>,
    _distance: f32,
) {
    if t.mob_ref().is_dead_or_dying() {
        return;
    }
    let id = t.id;
    let my_pos = t.pos();
    let Tick { ents, env, .. } = t;
    with_memories(ents, id, |ents, mem| {
        mem.set_with_expiry(M::VibrationCooldown, MemValue::Unit, 40);
        entity_event(ents, env, id, 61);
        let pitch = voice_pitch(ents, id);
        play_sound(
            ents,
            env,
            id,
            "minecraft:entity.warden.tendril_clicks",
            5.0,
            pitch,
        );
        let mut suspicious = pos;
        if let Some(owner) = owner {
            let owner_pos = ents.store.get(owner).map(|e| e.pos());
            if owner_pos.is_some_and(|o| o.distance_to_sqr(my_pos) < 30.0 * 30.0) {
                if mem.has(M::RecentProjectile) {
                    if can_target_entity(ents, Some(owner))
                        && let Some(o) = owner_pos
                    {
                        suspicious = BlockPos::containing(o.x, o.y, o.z);
                    }
                    increase_anger_at(ents, env, id, mem, Some(owner), DEFAULT_ANGER, true);
                } else {
                    increase_anger_at(ents, env, id, mem, Some(owner), PROJECTILE_ANGER, true);
                }
            }
            mem.set_with_expiry(M::RecentProjectile, MemValue::Unit, 100);
        } else {
            increase_anger_at(ents, env, id, mem, source, DEFAULT_ANGER, true);
        }
        if !anger_level(ents, id, mem).is_angry() {
            let active = state(ents, id)
                .and_then(|s| s.anger.top_suspect(|e| can_target_entity(ents, Some(e))));
            if owner.is_some() || active.is_none() || active == source {
                set_disturbance_location(ents, id, mem, suspicious);
            }
        }
    });
}

/// The warden's `VibrationSystem.Listener.handleGameEvent` (from the level's dispatch).
pub(crate) fn handle_game_event(
    level: &mut Level,
    id: EntityId,
    event: u16,
    context: GameEventContext,
    source: Vec3,
) {
    let Some(s) = state(&level.entities, id) else {
        return;
    };
    if s.vibration.current.is_some() {
        return;
    }
    if !sculk_vibration::is_valid_vibration(level, event, context, Listenable::Warden) {
        return;
    }
    let Some(destination) = level.listener_position(Listener::Entity {
        id,
        section: BlockPos::ZERO.section(),
        radius: LISTENER_RANGE,
        kind: EntityListener::Vibration,
    }) else {
        return;
    };
    // `canReceiveVibration`.
    let ents = &level.entities;
    let Some(m) = ents.mob(id) else { return };
    let cooldown = m
        .brain
        .as_deref()
        .is_some_and(|b| b.has(M::VibrationCooldown));
    if m.no_ai || m.is_dead_or_dying() || cooldown || is_digging_or_emerging(ents, id) {
        return;
    }
    if let Some(src) = context.source {
        let living = ents.player(src).is_some() || ents.mob(src).is_some();
        if living && !can_target_entity(ents, Some(src)) {
            return;
        }
    }
    if sculk_vibration::is_occluded(level, source, destination) {
        return;
    }
    let time = level.game_time();
    let projectile_owner = sculk_vibration::projectile_owner(level, context.source);
    let distance = source.distance_to_sqr(destination).sqrt() as f32;
    if let Some(s) = state_mut(&mut level.entities, id) {
        s.vibration.add_candidate(
            Vibration {
                event,
                distance,
                pos: source,
                source: context.source,
                projectile_owner,
            },
            time,
        );
    }
}

/// After the warden's tick: its listener follows it into the section of its eyes.
pub fn after_tick(t: &mut Tick<'_, '_>) {
    let p = t.pos();
    let section = BlockPos::containing(p.x, p.y + EYE_HEIGHT, p.z).section();
    let id = t.id;
    let Some(s) = t.mob().boss.warden_mut() else {
        return;
    };
    let wanted = Listener::Entity {
        id,
        section,
        radius: LISTENER_RANGE,
        kind: EntityListener::Vibration,
    };
    if s.listener == Some(wanted) {
        return;
    }
    let remove = s.listener.replace(wanted);
    t.cx.actions.push(WorldAction::Level(LevelAction::Listener {
        remove,
        add: Some(wanted),
    }));
}

/// The warden's additions to `aiStep`: none.
pub fn ai_step(t: &mut Tick<'_, '_>) {
    let _ = t;
}

/// `customServerAiStep`: the brain, the darkness pulse, the anger tick and the activity.
pub fn custom_server_ai_step(t: &mut Tick<'_, '_>) {
    let id = t.id;
    if t.mob_ref().brain.is_none() {
        return;
    }
    crate::mob::brain::tick(t);
    if !t.exists() || t.mob_ref().brain.is_none() {
        return;
    }
    let tick_count = t.mob_ref().tick_count;
    let network_id = t.core().network_id().0;
    if (tick_count.wrapping_add(network_id)) % 120 == 0 {
        let pos = t.pos();
        apply_darkness_around(t.ents, pos, Some(id), DARKNESS_RADIUS);
    }
    if tick_count % 20 == 0 {
        tick_anger(t.ents, id);
        sync_client_anger(t);
    }
    // `WardenAi.updateActivity`.
    use crate::mob::brain::Activity as A;
    let brain = crate::mob::brain::brain(t);
    brain.set_active_activity_to_first_valid(&[
        A::Emerge,
        A::Dig,
        A::Roar,
        A::Fight,
        A::Investigate,
        A::Sniff,
        A::Idle,
    ]);
    let attack = target(brain);
    t.mob().target = attack;
}

/// `angerManagement.tick(level, canTargetEntity)`.
fn tick_anger(ents: &mut Entities, id: EntityId) {
    let Some(mut anger) = state_mut(ents, id).map(|s| std::mem::take(&mut s.anger)) else {
        return;
    };
    let ents_ref: &Entities = ents;
    anger.tick(
        |uuid| {
            ents_ref
                .players
                .iter()
                .map(|(pid, _)| pid)
                .find(|&pid| ents_ref.store.get(pid).is_some_and(|e| e.uuid() == uuid))
                .map(|pid| (pid, true))
        },
        |e| {
            if ents_ref.store.get(e).is_none() {
                SuspectState::Gone { keep: true }
            } else if can_target_entity(ents_ref, Some(e)) {
                SuspectState::Valid
            } else {
                SuspectState::Invalid
            }
        },
    );
    if let Some(s) = state_mut(ents, id) {
        s.anger = anger;
    }
}

/// `syncClientAngerLevel`.
fn sync_client_anger(t: &mut Tick<'_, '_>) {
    let id = t.id;
    let attack = t.mob_ref().brain.as_deref().and_then(target);
    let anger = state(t.ents, id).map_or(0, |s| s.anger.active_anger(attack));
    if let Some(s) = t.mob().boss.warden_mut()
        && s.client_anger != anger
    {
        s.client_anger = anger;
        t.core_mut().mark_data_dirty();
    }
}

/// The warden's ambient sound by anger level; none while roaring, digging or emerging.
pub fn play_ambient_sound(t: &mut Tick<'_, '_>) {
    let pose = t.core().pose();
    if matches!(pose, Pose::Roaring | Pose::Digging | Pose::Emerging) {
        return;
    }
    let id = t.id;
    let level = with_memories(t.ents, id, |ents, mem| anger_level(ents, id, mem));
    if let Some(level) = level {
        crate::mob::combat::mob_sound(t.ents, t.env, id, level.ambient_sound());
    }
}

/// `Warden.doPush(entity)`: touching the warden angers it.
pub fn do_push(t: &mut Tick<'_, '_>, other: EntityId) {
    if t.mob_ref().no_ai {
        return;
    }
    let id = t.id;
    let other_pos = t.ents.store.get(other).map(|e| e.pos());
    let Tick { ents, env, .. } = t;
    with_memories(ents, id, |ents, mem| {
        if mem.has(M::TouchCooldown) {
            return;
        }
        mem.set_with_expiry(M::TouchCooldown, MemValue::Unit, 20);
        increase_anger_at(ents, env, id, mem, Some(other), DEFAULT_ANGER, true);
        if let Some(p) = other_pos {
            set_disturbance_location(ents, id, mem, BlockPos::containing(p.x, p.y, p.z));
        }
    });
}

/// `tickDeath`: the common one.
pub fn tick_death(t: &mut Tick<'_, '_>) -> bool {
    let _ = t;
    false
}

/// `isInvulnerableTo`: digging or emerging wardens only take damage that bypasses
/// invulnerability.
pub fn filter_hurt(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    target: EntityId,
    source: &DamageSource,
    amount: f32,
) -> Option<f32> {
    let _ = (level, cx);
    if is_digging_or_emerging(ents, target)
        && !env.types.is(source.kind, tag::BYPASSES_INVULNERABILITY)
    {
        return None;
    }
    Some(amount)
}

/// `Warden.hurtServer` after the damage: anger at the attacker, which becomes the attack target
/// when it hit directly or stands within 5 blocks.
pub fn after_hurt(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    target: EntityId,
    source: &DamageSource,
    took: bool,
) {
    let _ = (level, cx, took);
    let no_ai = ents.mob(target).is_none_or(|m| m.no_ai);
    if no_ai || is_digging_or_emerging(ents, target) {
        return;
    }
    let attacker = source.cause;
    let direct = source.cause.is_some() && source.cause == source.direct;
    let my_pos = ents.store.get(target).map(|e| e.pos());
    with_memories(ents, target, |ents, mem| {
        increase_anger_at(
            ents,
            env,
            target,
            mem,
            attacker,
            AngerLevel::Angry.minimum_anger() + ON_HURT_ANGER_BOOST,
            false,
        );
        let Some(attacker) = attacker else { return };
        let living = ents.player(attacker).is_some() || ents.mob(attacker).is_some();
        let close = match (ents.store.get(attacker).map(|e| e.pos()), my_pos) {
            (Some(a), Some(m)) => a.distance_to_sqr(m) < 25.0,
            _ => false,
        };
        if !mem.has(M::AttackTarget) && living && (direct || close) {
            set_attack_target(mem, attacker);
        }
    });
}

/// `entities/warden`: one sculk catalyst.
pub fn death_loot(m: &mut MobData, killed_by_player: bool) -> Vec<crate::item::ItemStack> {
    let _ = (m, killed_by_player);
    vanillite_data::registry::ITEM
        .id("minecraft:sculk_catalyst")
        .map(|id| vec![crate::item::ItemStack::new(id, 1)])
        .unwrap_or_default()
}

/// `CLIENT_ANGER_LEVEL` (id 16).
pub fn entity_data(out: &mut Vec<u8>, s: &State, all: bool) {
    data::int(out, 16, s.client_anger, 0, all);
}

/// `addAdditionalSaveData`: `anger`, `listener` and the brain's memories.
pub fn save(s: &State, out: &mut Compound) {
    out.insert("anger", Tag::Compound(s.anger.save()));
    out.insert("listener", Tag::Compound(s.vibration.save(|_| None)));
}

/// `readAdditionalSaveData`.
pub fn load(s: &mut State, tag: &Compound) {
    if let Some(Tag::Compound(a)) = tag.get("anger") {
        s.anger.load(a);
    }
    if let Some(Tag::Compound(l)) = tag.get("listener") {
        s.vibration = VibrationData::load(l, |_| None);
    }
}

/// `SpawnUtil.trySpawnMob(WARDEN, TRIGGERED, level, pos, attempts, xz, y, ON_TOP_OF_COLLIDER,
/// false)` and `Warden.finalizeSpawn(TRIGGERED)`: an emerging warden on a block top near
/// `start`. Returns whether one spawned.
pub fn try_spawn_triggered(
    level: &mut Level,
    start: BlockPos,
    attempts: i32,
    range_xz: i32,
    range_y: i32,
) -> bool {
    for _ in 0..attempts {
        let dx = level.random().next_int_bounded(2 * range_xz + 1) - range_xz;
        let dz = level.random().next_int_bounded(2 * range_xz + 1) - range_xz;
        let search = start.offset(dx, range_y, dz);
        let Some(pos) = possible_spawn_position(level, search, range_y) else {
            continue;
        };
        if spawn_emerging(level, pos) {
            return true;
        }
    }
    false
}

/// `moveToPossibleSpawnPosition` with `ON_TOP_OF_COLLIDER`.
fn possible_spawn_position(level: &Level, search: BlockPos, range_y: i32) -> Option<BlockPos> {
    let mut pos = search;
    let mut above = level.block(pos);
    for _ in (-range_y..=range_y).rev() {
        pos = pos.offset(0, -1, 0);
        let current = level.block(pos);
        if above.collision_shape().is_empty()
            && crate::block::place::util::face_full(
                current.collision_shape(),
                vanillite_base::Direction::Up,
            )
        {
            return Some(pos.offset(0, 1, 0));
        }
        above = current;
    }
    None
}

/// Creates the warden at `pos` if its box is free (`checkSpawnObstruction`).
fn spawn_emerging(level: &mut Level, pos: BlockPos) -> bool {
    let mut m = MobData::new(Kind::Warden, crate::mob::new_entity_seed());
    let yaw = (level.random().next_float() * 360.0 + 180.0).rem_euclid(360.0) - 180.0;
    let at = Vec3::new(
        f64::from(pos.x) + 0.5,
        f64::from(pos.y),
        f64::from(pos.z) + 0.5,
    );
    let dims = crate::mob::kinds::dimensions(entity_ids::WARDEN, false);
    let half = f64::from(dims.width) / 2.0;
    let aabb = Aabb::new(
        at.x - half,
        at.y,
        at.z - half,
        at.x + half,
        at.y + f64::from(dims.height),
        at.z + half,
    );
    let mut scratch = Vec::new();
    if crate::mob::physics::contains_any_liquid(level, &aabb)
        || !crate::mob::physics::no_collision(&level.view(), &aabb, &mut scratch)
    {
        return false;
    }
    // `Mob.finalizeSpawn` (follow range bonus, left handedness, from the level random) after
    // the warden's own part.
    let bonus = level.random().triangle(0.0, 0.114_850_000_000_000_01);
    m.attrs.follow_range *= 1.0 + bonus;
    m.left_handed = level.random().next_float() < 0.05;
    if let Some(mem) = m.brain.as_deref_mut() {
        mem.set_with_expiry(M::DigCooldown, MemValue::Unit, DIGGING_COOLDOWN);
        mem.set_with_expiry(
            M::IsEmerging,
            MemValue::Unit,
            i64::from(ai::EMERGE_DURATION),
        );
    }
    let uuid = crate::mob::new_uuid();
    let network_ids = std::sync::Arc::clone(level.network_ids_arc());
    let id = crate::mob::spawn(
        &mut level.entities,
        &network_ids,
        uuid,
        at,
        Rotation {
            y_rot: yaw,
            x_rot: 0.0,
            y_head_rot: yaw,
        },
        m,
    );
    if let Some(mut e) = level.entities.store.get_mut(id) {
        e.set_pose(Pose::Emerging);
        e.set_dims(vanillite_world::entity::Dimensions {
            width: dims.width,
            height: 1.0,
        });
    }
    let pitch = 1.0;
    level.play_sound(
        None,
        at.x,
        at.y,
        at.z,
        crate::block::sound_id("entity.warden.agitated"),
        crate::level::SOUND_HOSTILE,
        5.0,
        pitch,
    );
    true
}

#[cfg(test)]
mod tests;
