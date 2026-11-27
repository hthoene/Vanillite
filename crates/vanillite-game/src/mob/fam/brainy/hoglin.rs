//! Hoglins and zoglins (`Hoglin`, `HoglinBase`, `HoglinAi`, `Zoglin`, `HoglinSpecificSensor`):
//! herds that attack players, flee from repellents and outnumbering piglins, breed with
//! crimson fungi and turn into zoglins outside the Nether.

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Vec3};
use vanillite_nbt::Compound;
use vanillite_world::entity::{EntityFlags, EntityId};

use super::{SpawnContext, State, convert_to, make_sound, nbt_bool, nbt_int, state, state_mut};
use crate::level::Level;
use crate::mob::animal::Interaction;
use crate::mob::brain::behavior::{self as bh, Behavior, Spec, for_manhattan};
use crate::mob::brain::{
    self, Activity, ActivitySpec, Brain, BrainSpec, MemValue, MemoryType as M, SensorKind,
    brain_ref, sensing,
};
use crate::mob::fam::Interact;
use crate::mob::{Kind, MobData, Tick};
use crate::player::encode::DataWriter;

/// `HoglinSpecificSensor.requires`.
pub const SENSOR_MEMORIES: &[M] = &[
    M::NearestVisibleLivingEntities,
    M::NearestRepellent,
    M::NearestVisibleAdultPiglin,
    M::NearestVisibleAdultHoglins,
    M::VisibleAdultPiglinCount,
    M::VisibleAdultHoglinCount,
];

fn is_adult(t: &mut Tick<'_, '_>) -> bool {
    !t.mob_ref().baby
}

fn is_baby(t: &mut Tick<'_, '_>) -> bool {
    t.mob_ref().baby
}

fn idle_movement() -> Spec {
    Spec::run_one(vec![
        (Spec::stroll(0.4), 2),
        (Spec::walk_to_look_target(0.4, 3), 2),
        (Spec::do_nothing(30, 60), 1),
    ])
}

/// The brain of a hoglin (`Hoglin.BRAIN_PROVIDER`, `HoglinAi.getActivities`) or a zoglin.
pub(super) fn hoglin_brain() -> BrainSpec {
    let core = ActivitySpec::new(
        Activity::Core,
        0,
        vec![
            Spec::look_at_target_sink(45, 90),
            Spec::move_to_target_sink(),
        ],
    );
    let idle = ActivitySpec::new(
        Activity::Idle,
        10,
        vec![
            Spec::become_passive(M::NearestRepellent, 200),
            Spec::make_love(Kind::Hoglin, 0.6, 2),
            Spec::walk_away_from(M::NearestRepellent, 1.0, 8, true),
            Spec::start_attacking(None, find_attack_target),
            Spec::trigger_if(
                is_adult,
                Spec::walk_away_from(M::NearestVisibleAdultPiglin, 0.4, 8, false),
            ),
            Spec::look_at_entity_sometimes(None, 8.0, 30, 60),
            Spec::baby_follow_adult(5, 16, 0.6),
            idle_movement(),
        ],
    );
    let fight = ActivitySpec::with_memory(
        Activity::Fight,
        10,
        vec![
            Spec::become_passive(M::NearestRepellent, 200),
            Spec::make_love(Kind::Hoglin, 0.6, 2),
            Spec::walk_to_attack_target(1.0),
            Spec::trigger_if(is_adult, Spec::melee(40)),
            Spec::trigger_if(is_baby, Spec::melee(15)),
            Spec::stop_attacking_if_invalid(),
            Spec::erase_if(is_breeding, M::AttackTarget),
        ],
        M::AttackTarget,
    );
    let avoid = ActivitySpec::with_memory(
        Activity::Avoid,
        10,
        vec![
            Spec::walk_away_from(M::AvoidTarget, 1.3, 15, false),
            idle_movement(),
            Spec::look_at_entity_sometimes(None, 8.0, 30, 60),
            Spec::erase_if(wants_to_stop_fleeing, M::AvoidTarget),
        ],
        M::AvoidTarget,
    );
    BrainSpec {
        sensors: vec![
            SensorKind::NearestLivingEntities,
            SensorKind::NearestPlayers,
            SensorKind::NearestAdult,
            super::sensor(super::Sensor::HoglinSpecific),
        ],
        activities: vec![core, idle, fight, avoid],
        ..BrainSpec::default()
    }
}

pub(super) fn zoglin_brain() -> BrainSpec {
    let core = ActivitySpec::new(
        Activity::Core,
        0,
        vec![
            Spec::look_at_target_sink(45, 90),
            Spec::move_to_target_sink(),
        ],
    );
    let idle = ActivitySpec::new(
        Activity::Idle,
        10,
        vec![
            Spec::start_attacking(None, zoglin_target),
            Spec::look_at_entity_sometimes(None, 8.0, 30, 60),
            idle_movement(),
        ],
    );
    let fight = ActivitySpec::with_memory(
        Activity::Fight,
        10,
        vec![
            Spec::walk_to_attack_target(1.0),
            Spec::trigger_if(is_adult, Spec::melee(40)),
            Spec::trigger_if(is_baby, Spec::melee(15)),
            Spec::stop_attacking_if_invalid(),
        ],
        M::AttackTarget,
    );
    BrainSpec {
        sensors: vec![
            SensorKind::NearestLivingEntities,
            SensorKind::NearestPlayers,
        ],
        activities: vec![core, idle, fight],
        ..BrainSpec::default()
    }
}

pub fn init(m: &mut MobData) {
    let spec = if m.kind == Kind::Hoglin {
        hoglin_brain()
    } else {
        zoglin_brain()
    };
    m.brain = Some(Box::new(Brain::new(spec, &mut m.random)));
    m.nav.can_float = true;
}

/// `finalizeSpawn`: 20 % babies.
pub fn finalize_spawn(m: &mut MobData, _ctx: &mut SpawnContext<'_>) {
    if m.random.next_float() < 0.2 {
        m.baby = true;
        if m.kind == Kind::Hoglin {
            m.age = -24000;
        }
    }
    age_boundary(m);
}

/// `ageBoundaryReached` / `Zoglin.setBaby`: babies hit for 0.5.
fn age_boundary(m: &mut MobData) {
    m.attrs.attack_damage = if m.baby { 0.5 } else { 6.0 };
}

/// `HoglinAi.findNearestValidAttackTarget`.
fn find_attack_target(t: &mut Tick<'_, '_>) -> Option<EntityId> {
    if is_pacified(t) || is_breeding(t) {
        return None;
    }
    brain_ref(t).entity(M::NearestVisibleAttackablePlayer)
}

/// `Zoglin.findNearestValidAttackTarget`: anything attackable but zoglins and creepers.
fn zoglin_target(t: &mut Tick<'_, '_>) -> Option<EntityId> {
    let mut candidates = Vec::new();
    sensing::visible_find_all(
        t,
        |t, id| {
            !t.ents
                .mob(id)
                .is_some_and(|m| matches!(m.kind, Kind::Zoglin | Kind::Creeper))
        },
        &mut candidates,
    );
    candidates
        .into_iter()
        .find(|&id| sensing::is_attackable(t, id))
}

fn is_breeding(t: &mut Tick<'_, '_>) -> bool {
    brain_ref(t).has(M::BreedTarget)
}

fn is_pacified(t: &Tick<'_, '_>) -> bool {
    brain_ref(t).has(M::Pacified)
}

/// `piglinsOutnumberHoglins`.
fn piglins_outnumber(t: &Tick<'_, '_>) -> bool {
    if t.mob_ref().baby {
        return false;
    }
    let b = brain_ref(t);
    let piglins = b.int(M::VisibleAdultPiglinCount).unwrap_or(0);
    let hoglins = b.int(M::VisibleAdultHoglinCount).unwrap_or(0) + 1;
    piglins > hoglins
}

fn wants_to_stop_fleeing(t: &mut Tick<'_, '_>) -> bool {
    !t.mob_ref().baby && !piglins_outnumber(t)
}

/// `HoglinAi.isPosNearNearestRepellent`.
fn near_repellent(m: &MobData, pos: BlockPos) -> bool {
    m.brain
        .as_deref()
        .and_then(|b| b.pos(M::NearestRepellent))
        .is_some_and(|r| r.dist_sqr(pos) < 64.0)
}

/// `Hoglin.getWalkTargetValue`: crimson nylium is home (repellents are checked in the
/// stroll positions through the brain).
pub fn walk_target_value(level: &Level, pos: BlockPos) -> f64 {
    let below = level.block(BlockPos::new(pos.x, pos.y - 1, pos.z));
    if below.block().name() == "minecraft:crimson_nylium" {
        10.0
    } else {
        0.0
    }
}

/// `HoglinSpecificSensor`.
pub fn specific_sensor(t: &mut Tick<'_, '_>) {
    let me = t.block_pos();
    let tags = crate::block::block_tags();
    let mut repellent = None;
    for_manhattan(me, 8, 4, |p| {
        if tags.hoglin_repellents.contains(t.level.block(p)) {
            repellent = Some(p);
            true
        } else {
            false
        }
    });
    brain::brain(t).set_opt(M::NearestRepellent, repellent.map(MemValue::Pos));
    let mut found = Vec::new();
    sensing::visible_find_all(
        t,
        |t, id| {
            t.ents
                .mob(id)
                .is_some_and(|m| !m.baby && matches!(m.kind, Kind::Piglin | Kind::Hoglin))
        },
        &mut found,
    );
    let mut piglin = None;
    let mut piglins = 0;
    let mut hoglins = Vec::new();
    for id in found {
        match t.ents.mob(id).map(|m| m.kind) {
            Some(Kind::Piglin) => {
                piglins += 1;
                piglin.get_or_insert(id);
            }
            Some(Kind::Hoglin) => hoglins.push(id),
            _ => {}
        }
    }
    let b = brain::brain(t);
    b.set_opt(M::NearestVisibleAdultPiglin, piglin.map(MemValue::Entity));
    let n = i32::try_from(hoglins.len()).unwrap_or(i32::MAX);
    b.set_entities(M::NearestVisibleAdultHoglins, hoglins.into_iter());
    b.set(M::VisibleAdultPiglinCount, MemValue::Int(piglins));
    b.set(M::VisibleAdultHoglinCount, MemValue::Int(n));
}

/// `HoglinAi.updateActivity` (with the conversion of `customServerAiStep`) or
/// `Zoglin.updateActivity`.
pub fn update_activity(t: &mut Tick<'_, '_>) {
    let old = brain_ref(t).active_non_core();
    let order: &[Activity] = if t.mob_ref().kind == Kind::Hoglin {
        &[Activity::Fight, Activity::Avoid, Activity::Idle]
    } else {
        &[Activity::Fight, Activity::Idle]
    };
    brain::brain(t).set_active_activity_to_first_valid(order);
    let new = brain_ref(t).active_non_core();
    let attacking = brain_ref(t).has(M::AttackTarget);
    if t.mob_ref().kind == Kind::Zoglin {
        if new == Some(Activity::Fight) && old != Some(Activity::Fight) {
            make_sound(t, "minecraft:entity.zoglin.angry");
        }
        super::set_aggressive(t, attacking);
        return;
    }
    if old != new
        && let Some(sound) = ambient_sound(t.mob_ref())
    {
        make_sound(t, sound);
    }
    super::set_aggressive(t, attacking);
    if is_converting(t) {
        let s = state_mut(t.mob());
        s.time_in_overworld += 1;
        if s.time_in_overworld > 300 {
            make_sound(t, "minecraft:entity.hoglin.converted_to_zombified");
            if let Some(z) = convert_to(
                t,
                Kind::Zoglin,
                true,
                false,
                vanillite_abi::transform_reason::PIGLIN_ZOMBIFIED,
            ) && let Some(m) = t.ents.mob_mut(z)
            {
                crate::effect::living::add_to_mob(
                    m,
                    crate::effect::EffectInstance::new(crate::effect::MobEffect::NAUSEA, 200, 0),
                );
            }
        }
    } else {
        state_mut(t.mob()).time_in_overworld = 0;
    }
}

/// `isConverting`.
fn is_converting(t: &Tick<'_, '_>) -> bool {
    !state(t.mob_ref()).immune_to_zombification && !t.mob_ref().no_ai && super::zombifies(t)
}

/// `getSoundForCurrentActivity`.
pub fn ambient_sound(m: &MobData) -> Option<&'static str> {
    let b = m.brain.as_deref()?;
    let activity = b.active_non_core()?;
    Some(if activity == Activity::Avoid {
        "minecraft:entity.hoglin.retreat"
    } else if activity == Activity::Fight {
        "minecraft:entity.hoglin.angry"
    } else if b.has(M::NearestRepellent) {
        "minecraft:entity.hoglin.retreat"
    } else {
        "minecraft:entity.hoglin.ambient"
    })
}

/// `Zoglin.getAmbientSound`.
pub fn zoglin_ambient_sound(m: &MobData) -> &'static str {
    if m.brain.as_deref().is_some_and(|b| b.has(M::AttackTarget)) {
        "minecraft:entity.zoglin.angry"
    } else {
        "minecraft:entity.zoglin.ambient"
    }
}

/// `aiStep`: the attack animation countdown; babies of hoglins grow (`ageBoundaryReached`).
pub fn ai_step(t: &mut Tick<'_, '_>) {
    let s = state_mut(t.mob());
    if s.attack_anim > 0 {
        s.attack_anim -= 1;
    }
    let m = t.mob();
    let expected = if m.baby { 0.5 } else { 6.0 };
    #[allow(clippy::float_cmp)]
    if m.attrs.attack_damage != expected && !m.effects.has(crate::effect::MobEffect::STRENGTH) {
        age_boundary(m);
    }
}

/// `doHurtTarget`: the attack animation and sound, then `HoglinBase.hurtAndThrowTarget`.
pub fn do_hurt_target(t: &mut Tick<'_, '_>, target: EntityId) -> bool {
    state_mut(t.mob()).attack_anim = 10;
    let id = t.id;
    crate::mob::living::entity_event(t, id, 4);
    let kind = t.mob_ref().kind;
    if kind == Kind::Hoglin {
        make_sound(t, "minecraft:entity.hoglin.attack");
        on_hit_target(t, target);
    } else {
        make_sound(t, "minecraft:entity.zoglin.attack");
    }
    hurt_and_throw(t, target)
}

/// `HoglinBase.hurtAndThrowTarget`.
fn hurt_and_throw(t: &mut Tick<'_, '_>, target: EntityId) -> bool {
    #[allow(clippy::cast_possible_truncation)]
    let attack = t.mob_ref().attrs.attack_damage as f32;
    #[allow(clippy::cast_possible_truncation)]
    let whole = attack as i32;
    let damage = if !t.mob_ref().baby && whole > 0 {
        #[allow(clippy::cast_precision_loss)]
        let extra = t.env.random.next_int_bounded(whole) as f32;
        attack / 2.0 + extra
    } else {
        attack
    };
    let source = crate::mob::combat::entity_source(t.ents, t.env.types.mob_attack, t.id, t.id);
    let hurt = crate::mob::combat::hurt(t.level, t.ents, t.env, t.cx, target, source, damage);
    if hurt && t.exists() && !t.mob_ref().baby {
        throw_target(t, target);
    }
    hurt
}

/// `HoglinBase.throwTarget`.
pub fn throw_target(t: &mut Tick<'_, '_>, target: EntityId) {
    let power = t.mob_ref().attrs.attack_knockback;
    let resistance = t
        .ents
        .mob(target)
        .map_or(0.0, |m| m.attrs.knockback_resistance);
    let effective = power - resistance;
    if effective <= 0.0 {
        return;
    }
    let (Some(me), Some(them)) = (bh::entity_pos(t, t.id), bh::entity_pos(t, target)) else {
        return;
    };
    let (xd, zd) = (them.x - me.x, them.z - me.z);
    let r = &mut *t.env.random;
    #[allow(clippy::cast_precision_loss)]
    let angle = (r.next_int_bounded(21) - 10) as f32;
    let horizontal = effective * f64::from(r.next_float() * 0.5 + 0.2);
    let len = (xd * xd + zd * zd).sqrt();
    let (nx, nz) = if len < 1.0e-4 {
        (0.0, 0.0)
    } else {
        (xd / len, zd / len)
    };
    let (hx, hz) = (nx * horizontal, nz * horizontal);
    let (cos, sin) = (
        f64::from(vanillite_base::math::cos(f64::from(angle))),
        f64::from(vanillite_base::math::sin(f64::from(angle))),
    );
    let px = hx * cos + hz * sin;
    let pz = hz * cos - hx * sin;
    let vertical = effective * f64::from(r.next_float()) * 0.5;
    if let Some(mut e) = t.ents.store.get_mut(target) {
        let v = e.as_ref().velocity();
        e.set_velocity(Vec3::new(v.x + px, v.y + vertical, v.z + pz));
        e.set_flags(EntityFlags::HURT_MARKED, true);
        e.set_flags(EntityFlags::HAS_IMPULSE, true);
    }
}

/// `HoglinAi.onHitTarget`.
fn on_hit_target(t: &mut Tick<'_, '_>, target: EntityId) {
    if t.mob_ref().baby {
        return;
    }
    let is_piglin = t.ents.mob(target).is_some_and(|m| m.kind == Kind::Piglin);
    if is_piglin && piglins_outnumber(t) {
        set_avoid_target(t, target);
        broadcast_retreat(t, target);
    } else {
        broadcast_attack_target(t, target);
    }
}

fn visible_adult_hoglins(t: &Tick<'_, '_>) -> Vec<EntityId> {
    brain_ref(t)
        .entities(M::NearestVisibleAdultHoglins)
        .to_vec()
}

fn broadcast_retreat(t: &mut Tick<'_, '_>, target: EntityId) {
    for h in visible_adult_hoglins(t) {
        with_other(t, h, |t| retreat_from_nearest(t, target));
    }
}

fn broadcast_attack_target(t: &mut Tick<'_, '_>, target: EntityId) {
    for h in visible_adult_hoglins(t) {
        with_other(t, h, |t| {
            if !is_pacified(t) {
                let current = brain_ref(t).entity(M::AttackTarget);
                let nearest = bh::nearest_target(t, current, target);
                set_attack_target(t, nearest);
            }
        });
    }
}

/// Runs `f` as if `other` were ticking (its brain and position).
pub fn with_other(t: &mut Tick<'_, '_>, other: EntityId, f: impl FnOnce(&mut Tick<'_, '_>)) {
    if t.ents.mob(other).is_none_or(|m| m.brain.is_none()) {
        return;
    }
    let mut o = Tick {
        level: t.level,
        ents: &mut *t.ents,
        env: &mut *t.env,
        cx: &mut *t.cx,
        id: other,
    };
    f(&mut o);
}

fn retreat_from_nearest(t: &mut Tick<'_, '_>, avoid: EntityId) {
    let b = brain_ref(t);
    let (current_avoid, current_attack) = (b.entity(M::AvoidTarget), b.entity(M::AttackTarget));
    let nearest = bh::nearest_target(t, current_avoid, avoid);
    let nearest = bh::nearest_target(t, current_attack, nearest);
    set_avoid_target(t, nearest);
}

fn set_avoid_target(t: &mut Tick<'_, '_>, avoid: EntityId) {
    let ttl = t.env.random.next_int_between_inclusive(100, 400);
    let b = brain::brain(t);
    b.erase(M::AttackTarget);
    b.erase(M::WalkTarget);
    b.set_with_expiry(M::AvoidTarget, MemValue::Entity(avoid), i64::from(ttl));
}

fn set_attack_target(t: &mut Tick<'_, '_>, target: EntityId) {
    let b = brain::brain(t);
    b.erase(M::CantReachWalkTargetSince);
    b.erase(M::BreedTarget);
    b.set_with_expiry(M::AttackTarget, MemValue::Entity(target), 200);
}

/// `HoglinAi.wasHurtBy`.
pub fn was_hurt_by(t: &mut Tick<'_, '_>, attacker: EntityId) {
    {
        let b = brain::brain(t);
        b.erase(M::Pacified);
        b.erase(M::BreedTarget);
    }
    if t.mob_ref().baby {
        retreat_from_nearest(t, attacker);
        return;
    }
    // `maybeRetaliate`.
    let attacker_kind = t.ents.mob(attacker).map(|m| m.kind);
    if brain_ref(t).is_active(Activity::Avoid) && attacker_kind == Some(Kind::Piglin) {
        return;
    }
    if attacker_kind == Some(Kind::Hoglin) || bh::much_further_than_target(t, attacker, 4.0) {
        return;
    }
    if sensing::is_attackable(t, attacker) {
        set_attack_target(t, attacker);
        broadcast_attack_target(t, attacker);
    }
}

/// `Zoglin.hurtServer`: retaliates unless the attacker is much farther than the target.
pub fn zoglin_was_hurt_by(t: &mut Tick<'_, '_>, attacker: EntityId) {
    if bh::can_attack(t, attacker) && !bh::much_further_than_target(t, attacker, 4.0) {
        let b = brain::brain(t);
        b.erase(M::CantReachWalkTargetSince);
        b.set_with_expiry(M::AttackTarget, MemValue::Entity(attacker), 200);
    }
}

/// `Hoglin.mobInteract`: `Animal.mobInteract` (feeding makes it persistent).
pub fn interact(ix: &mut Interact<'_, '_>) -> Interaction {
    let pacified = ix
        .ents
        .mob(ix.target)
        .and_then(|m| m.brain.as_deref())
        .is_some_and(|b| b.has(M::Pacified));
    let in_love_before = ix.ents.mob(ix.target).map_or(0, |m| m.in_love);
    let r = crate::mob::animal::animal_interact_pub(ix);
    if let Some(m) = ix.ents.mob_mut(ix.target) {
        // `canFallInLove` is false while pacified.
        if pacified && m.in_love > in_love_before {
            m.in_love = in_love_before;
        }
        if r.consumes() {
            m.persistent = true;
        }
    }
    r
}

/// Synched data: `AgeableMob` baby (16), age locked (17) and immunity (18) for hoglins, baby
/// (16) for zoglins.
pub fn encode(w: &mut DataWriter<'_>, m: &MobData) {
    w.boolean(16, m.baby, false);
    if m.kind == Kind::Hoglin {
        w.boolean(17, m.age_locked, false);
        w.boolean(18, state(m).immune_to_zombification, false);
    }
}

pub fn save(m: &MobData, out: &mut Compound) {
    let s = state(m);
    if m.kind == Kind::Hoglin {
        out.insert("IsImmuneToZombification", s.immune_to_zombification);
        out.insert("TimeInOverworld", s.time_in_overworld);
        out.insert("CannotBeHunted", s.cannot_hunt);
    } else {
        out.insert("IsBaby", m.baby);
    }
}

pub fn load(m: &mut MobData, tag: &Compound) {
    if m.kind == Kind::Hoglin {
        let s = state_mut(m);
        s.immune_to_zombification = nbt_bool(tag, "IsImmuneToZombification", false);
        s.time_in_overworld = nbt_int(tag, "TimeInOverworld", 0);
        s.cannot_hunt = nbt_bool(tag, "CannotBeHunted", false);
    } else {
        m.baby = nbt_bool(tag, "IsBaby", false);
    }
    age_boundary(m);
}

/// `Hoglin.canBeHunted`.
pub fn can_be_hunted(m: &MobData) -> bool {
    !m.baby && !state(m).cannot_hunt
}

/// Whether the position is near the hoglin's repellent (`isPosNearNearestRepellent`).
pub fn is_near_repellent(m: &MobData, pos: BlockPos) -> bool {
    near_repellent(m, pos)
}

#[allow(dead_code)]
fn _state(_: &State, _: Behavior) {}
