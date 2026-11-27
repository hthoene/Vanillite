//! Camels and camel husks (`Camel`, `CamelHusk`, `AbstractHorse`): sitting and standing up,
//! dashing with a saddle, two riders, sand spawning; their brain is in [`super::ai`].

use vanillite_base::BlockPos;
use vanillite_base::RandomSource;
use vanillite_nbt::Compound;
use vanillite_world::entity::{Dimensions, EntityId, Pose};

use super::{SpawnContext, State, encode_ageable, load_ageable, long, save_ageable, spawn_time};
use crate::entities::Entities;
use crate::item::EquipmentSlot;
use crate::level::Level;
use crate::mob::animal::{self, Interaction};
use crate::mob::fam::Interact;
use crate::mob::{Kind, MobData, Tick};
use crate::player::encode::{DataWriter, serializer};
use crate::survival::player::Env;

/// `DASH_COOLDOWN_TICKS`.
pub const DASH_COOLDOWN_TICKS: i32 = 55;
/// `SITDOWN_DURATION_TICKS`.
pub const SITDOWN_DURATION_TICKS: i64 = 40;
/// `STANDUP_DURATION_TICKS`.
pub const STANDUP_DURATION_TICKS: i64 = 52;
/// `SITTING_HEIGHT_DIFFERENCE`.
const SITTING_HEIGHT_DIFFERENCE: f32 = 1.43;

/// The camel part of a mob.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Camel {
    /// `DASH`.
    pub dash: bool,
    /// `LAST_POSE_CHANGE_TICK`: negative while sitting.
    pub last_pose_change_tick: i64,
    pub dash_cooldown: i32,
    /// `AbstractHorse` flags (`DATA_ID_FLAGS`): tame 2, bred 8, eating 16.
    pub horse_flags: u8,
    /// `Temper`.
    pub temper: i32,
    /// `eatingCounter`.
    pub eating_counter: i32,
}

const EATING: u8 = 16;

/// `AbstractHorse.isImmobile`: eating (camels do not rear).
pub fn is_immobile(m: &MobData) -> bool {
    camel(m).is_some_and(|c| c.horse_flags & EATING != 0)
}

/// `AbstractHorse.aiStep` before `super.aiStep()`: the `moveTail` roll.
pub fn ai_step_start(t: &mut Tick<'_, '_>) {
    let _ = t.mob().random.next_int_bounded(200);
}

/// `AbstractHorse.aiStep` after `super.aiStep()`: healing and grazing. (`followMommy` only
/// computes a path it does not use.)
pub fn ai_step(t: &mut Tick<'_, '_>) {
    if !t.mob_ref().is_alive() {
        return;
    }
    if t.mob().random.next_int_bounded(900) == 0 && t.mob_ref().death_time == 0 {
        crate::mob::fam::companion::heal(t.mob(), 1.0);
        t.core_mut().mark_data_dirty();
    }
    let eating = camel(t.mob_ref()).is_some_and(|c| c.horse_flags & EATING != 0);
    let vehicle = !t.ents.riding.passengers_of(t.id).is_empty();
    if !eating && !vehicle && t.mob().random.next_int_bounded(300) == 0 {
        let p = t.block_pos();
        let below = t.level.block(BlockPos::new(p.x, p.y - 1, p.z));
        if below.block() == vanillite_data::block::ids::GRASS_BLOCK {
            camel_mut(t.mob()).horse_flags |= EATING;
            t.core_mut().mark_data_dirty();
        }
    }
    let c = camel_mut(t.mob());
    if c.horse_flags & EATING != 0 {
        c.eating_counter += 1;
        if c.eating_counter > 50 {
            c.eating_counter = 0;
            c.horse_flags &= !EATING;
            t.core_mut().mark_data_dirty();
        }
    }
}

/// The camel state of a mob.
pub fn camel(m: &MobData) -> Option<&Camel> {
    match super::state(m) {
        State::Camel(c) => Some(c),
        _ => None,
    }
}

fn camel_mut(m: &mut MobData) -> &mut Camel {
    let s = super::state_mut(m);
    if !matches!(s, State::Camel(_)) {
        *s = State::Camel(Camel::default());
    }
    match s {
        State::Camel(c) => c,
        _ => unreachable!("set above"),
    }
}

pub fn init(m: &mut MobData) {
    camel_mut(m);
    // `GroundPathNavigation.setCanFloat(true)`, `setCanWalkOverFences(true)`.
    m.nav.can_float = true;
    super::ai::camel_brain(m);
}

/// `checkCamelSpawnRules`: `#camels_spawnable_on` below and bright enough.
pub fn check_spawn_rules(level: &Level, pos: BlockPos) -> bool {
    let below = level.block(BlockPos::new(pos.x, pos.y - 1, pos.z));
    let (sky, block) = level.light(pos);
    crate::mob::spawn::in_block_tag(below, "minecraft:camels_spawnable_on") && sky.max(block) > 8
}

/// `finalizeSpawn`: the camel stands fully.
pub fn finalize(m: &mut MobData, ctx: &SpawnContext<'_>) {
    let now = spawn_time(ctx);
    camel_mut(m).last_pose_change_tick = full_stand_tick(now);
}

/// `resetLastPoseChangeTickToFullStand`.
fn full_stand_tick(now: i64) -> i64 {
    (now - STANDUP_DURATION_TICKS - 1).max(0)
}

pub fn ambient_sound(m: &MobData) -> &'static str {
    if m.kind == Kind::CamelHusk {
        "minecraft:entity.camel_husk.ambient"
    } else {
        "minecraft:entity.camel.ambient"
    }
}

fn sound(m: &MobData, what: &str) -> &'static str {
    let husk = m.kind == Kind::CamelHusk;
    match (husk, what) {
        (false, "dash") => "minecraft:entity.camel.dash",
        (true, "dash") => "minecraft:entity.camel_husk.dash",
        (false, "dash_ready") => "minecraft:entity.camel.dash_ready",
        (true, "dash_ready") => "minecraft:entity.camel_husk.dash_ready",
        (false, "sit") => "minecraft:entity.camel.sit",
        (true, "sit") => "minecraft:entity.camel_husk.sit",
        (false, "stand") => "minecraft:entity.camel.stand",
        (true, "stand") => "minecraft:entity.camel_husk.stand",
        (false, _) => "minecraft:entity.camel.eat",
        (true, _) => "minecraft:entity.camel_husk.eat",
    }
}

/// `isCamelSitting`.
pub fn is_sitting(m: &MobData) -> bool {
    camel(m).is_some_and(|c| c.last_pose_change_tick < 0)
}

/// `getPoseTime`.
pub fn pose_time(m: &MobData, now: i64) -> i64 {
    now - camel(m).map_or(0, |c| c.last_pose_change_tick.abs())
}

/// `isInPoseTransition`.
pub fn in_pose_transition(m: &MobData, now: i64) -> bool {
    let limit = if is_sitting(m) {
        SITDOWN_DURATION_TICKS
    } else {
        STANDUP_DURATION_TICKS
    };
    pose_time(m, now) < limit
}

/// `refuseToMove`.
pub fn refuse_to_move(m: &MobData, now: i64) -> bool {
    is_sitting(m) || in_pose_transition(m, now)
}

/// Wears a saddle in the saddle slot.
pub fn is_saddled(m: &MobData) -> bool {
    !m.equipment.get(EquipmentSlot::Saddle).is_empty()
}

/// `getDefaultDimensions(pose)`.
pub fn dimensions(m: &MobData) -> Dimensions {
    let d = Dimensions::of(m.kind.type_id());
    match (is_sitting(m), m.baby) {
        (true, true) => Dimensions {
            width: 0.95,
            height: 0.425,
        },
        (true, false) => Dimensions {
            width: d.width,
            height: d.height - SITTING_HEIGHT_DIFFERENCE,
        },
        (false, true) => Dimensions {
            width: 0.95,
            height: 1.4,
        },
        (false, false) => d,
    }
}

/// Applies the pose and dimensions after the sitting state changed.
fn apply_pose(t: &mut Tick<'_, '_>) {
    let sitting = is_sitting(t.mob_ref());
    let dims = dimensions(t.mob_ref());
    let mut e = t.core_mut();
    e.set_pose(if sitting {
        Pose::Sitting
    } else {
        Pose::Standing
    });
    e.set_dims(dims);
    e.mark_data_dirty();
}

/// `sitDown`.
pub fn sit_down(t: &mut Tick<'_, '_>) {
    if is_sitting(t.mob_ref()) {
        return;
    }
    let s = sound(t.mob_ref(), "sit");
    super::make_sound(t, s);
    let now = t.game_time();
    camel_mut(t.mob()).last_pose_change_tick = -now;
    apply_pose(t);
}

/// `standUp`.
pub fn stand_up(t: &mut Tick<'_, '_>) {
    if !is_sitting(t.mob_ref()) {
        return;
    }
    let s = sound(t.mob_ref(), "stand");
    super::make_sound(t, s);
    let now = t.game_time();
    camel_mut(t.mob()).last_pose_change_tick = now;
    apply_pose(t);
}

/// `standUpInstantly` (from outside a tick: when hurt).
pub fn stand_up_instantly(ents: &mut Entities, env: &mut Env<'_>, id: EntityId) {
    let now = env.game_time;
    let Some(m) = ents.mob_mut(id) else {
        return;
    };
    camel_mut(m).last_pose_change_tick = full_stand_tick(now);
    let dims = dimensions(m);
    if let Some(mut e) = ents.store.get_mut(id) {
        e.set_pose(Pose::Standing);
        e.set_dims(dims);
        e.mark_data_dirty();
    }
}

/// `canCamelChangePose`: the box at the other pose is free of blocks.
pub fn can_change_pose(t: &Tick<'_, '_>) -> bool {
    let m = t.mob_ref();
    let mut other = m.clone();
    camel_mut(&mut other).last_pose_change_tick = if is_sitting(m) { 0 } else { -1 };
    let dims = dimensions(&other);
    let aabb = dims
        .bounding_box(t.pos())
        .inflate(-1.0e-7, -1.0e-7, -1.0e-7);
    let mut scratch = Vec::new();
    crate::mob::physics::no_collision(&t.level.view(), &aabb, &mut scratch)
}

/// `Camel.tick` after `LivingEntity.tick`: the dash ends on the ground, its cooldown runs
/// out, sitting camels stand up in water.
pub fn after_tick(t: &mut Tick<'_, '_>) {
    // A camel loaded sitting gets its pose and box (`readAdditionalSaveData`).
    if t.core().pose() != loaded_pose(t.mob_ref()) {
        apply_pose(t);
    }
    let on_ground = t.on_ground();
    let in_liquid = t.mob_ref().fluids.in_water() || t.mob_ref().fluids.in_lava();
    let passenger = t.ents.riding.vehicle_of(t.id).is_some();
    let c = camel_mut(t.mob());
    if c.dash && c.dash_cooldown < 50 && (on_ground || in_liquid || passenger) {
        c.dash = false;
        t.core_mut().mark_data_dirty();
    }
    let c = camel_mut(t.mob());
    if c.dash_cooldown > 0 {
        c.dash_cooldown -= 1;
        if c.dash_cooldown == 0 {
            let s = sound(t.mob_ref(), "dash_ready");
            let pos = t.pos();
            let seed = {
                use vanillite_base::random::RandomSource;
                t.env.random.next_long()
            };
            crate::survival::play_sound(
                t.env.out,
                None,
                pos,
                s,
                vanillite_protocol::packets::play::clientbound::SoundSource::Neutral,
                1.0,
                1.0,
                seed,
            );
        }
    }
    if refuse_to_move(t.mob_ref(), t.game_time()) {
        crate::mob::kinds::clamp_head_rotation_to_body(t);
    }
    if is_sitting(t.mob_ref()) && t.mob_ref().fluids.in_water() {
        let id = t.id;
        stand_up_instantly(t.ents, t.env, id);
    }
}

/// `BodyRotationControl` of a camel: none while it refuses to move.
pub fn turns_body(t: &Tick<'_, '_>) -> bool {
    !refuse_to_move(t.mob_ref(), t.game_time())
}

/// `Camel.travel`: a sitting camel only falls.
pub fn travel(t: &mut Tick<'_, '_>, input: vanillite_base::Vec3) -> bool {
    let now = t.game_time();
    if refuse_to_move(t.mob_ref(), now) && t.on_ground() {
        let v = t.velocity();
        t.set_velocity(vanillite_base::Vec3::new(0.0, v.y, 0.0));
        let m = t.mob();
        m.xxa = 0.0;
        m.zza = 0.0;
        let _ = input;
    }
    false
}

/// `CamelMoveControl`: a camel told to move stands up first.
pub fn move_control(t: &mut Tick<'_, '_>) -> bool {
    let now = t.game_time();
    let leashed = t.ents.leashes.contains_key(&t.id);
    if t.mob_ref().move_ctl.operation == crate::mob::nav::Operation::MoveTo
        && !leashed
        && is_sitting(t.mob_ref())
        && !in_pose_transition(t.mob_ref(), now)
        && can_change_pose(t)
    {
        stand_up(t);
    }
    false
}

/// `CamelLookControl`: no looking around while ridden.
pub fn look_control(t: &mut Tick<'_, '_>) -> bool {
    !t.ents.riding.passengers_of(t.id).is_empty() && is_saddled(t.mob_ref())
}

/// `customServerAiStep`: the brain and `CamelAi.updateActivity`.
pub fn custom_server_ai_step(t: &mut Tick<'_, '_>) {
    super::ai::tick(t);
}

/// `handleStartJump`: the rider dashes (sound and the dash flag; the client moves it).
pub fn handle_start_jump(ents: &mut Entities, env: &mut Env<'_>, id: EntityId, strength: i32) {
    let now = env.game_time;
    let Some(m) = ents.mob(id) else {
        return;
    };
    // `canJump`: saddled and not sitting.
    if strength <= 0 || !is_saddled(m) || refuse_to_move(m, now) {
        return;
    }
    let s = sound(m, "dash");
    crate::mob::combat::mob_sound(ents, env, id, s);
    if let Some(m) = ents.mob_mut(id) {
        let c = camel_mut(m);
        c.dash = true;
        // `onSyncedDataUpdated(DASH)`.
        if c.dash_cooldown == 0 {
            c.dash_cooldown = DASH_COOLDOWN_TICKS;
        }
    }
    if let Some(mut e) = ents.store.get_mut(id) {
        e.mark_data_dirty();
    }
}

/// `Camel.mobInteract`.
pub fn interact(ix: &mut Interact<'_, '_>) -> Interaction {
    let Some(m) = ix.ents.mob(ix.target) else {
        return Interaction::Pass;
    };
    let baby = m.baby;
    let kind = m.kind;
    if ix.sneaking && !baby {
        crate::mob::fam::companion::horse::open_inventory_screen(ix);
        return Interaction::Success;
    }
    // `ItemStack.interactLivingEntity`: saddles (`Equippable.equipOnTarget`).
    let r = animal::item_interact_pub(ix);
    if r.consumes() {
        return r;
    }
    if crate::mob::animal::is_food(kind, ix.stack.item) {
        return fed_food(ix);
    }
    let riders = ix.ents.riding.passengers_of(ix.target).len();
    if riders < 2 && !baby {
        crate::riding::start_riding(ix.ents, ix.env, ix.player, ix.target, false);
        return Interaction::Consume;
    }
    if baby {
        // `AbstractHorse.mobInteract` -> `AgeableMob.mobInteract`: the golden dandelion.
        return crate::mob::age_lock::interact_pub(ix).unwrap_or(Interaction::Pass);
    }
    Interaction::Pass
}

/// `AbstractHorse.fedFood` with `Camel.handleEating`: heals 2, falls in love, ages babies.
fn fed_food(ix: &mut Interact<'_, '_>) -> Interaction {
    let Some(m) = ix.ents.mob(ix.target) else {
        return Interaction::Pass;
    };
    let kind = m.kind;
    let could_heal = m.health < m.max_health();
    let can_love = kind == Kind::Camel && m.age == 0 && m.in_love <= 0;
    let can_age_up = m.age < 0 && !m.age_locked;
    if !could_heal && !can_love && !can_age_up {
        return Interaction::Pass;
    }
    let player_uuid = ix.ents.store.get(ix.player).map(|e| e.uuid());
    if let Some(m) = ix.ents.mob_mut(ix.target) {
        if could_heal {
            m.health = (m.health + 2.0).min(m.max_health());
        }
        if can_love {
            m.in_love = 600;
            m.love_cause = player_uuid;
        }
        if can_age_up {
            // `ageUp(10)`.
            m.age = (m.age + 10 * 20).min(0);
        }
    }
    if can_love {
        animal::entity_event(ix.ents, ix.env, ix.target, 18);
    }
    if let Some(mut e) = ix.ents.store.get_mut(ix.target) {
        e.mark_data_dirty();
    }
    let s = ix.ents.mob(ix.target).map(|m| sound(m, "eat"));
    if let Some(s) = s {
        crate::mob::combat::mob_sound(ix.ents, ix.env, ix.target, s);
    }
    let event = crate::level::game_event::events().eat;
    animal::entity_game_event(ix.ents, ix.env, ix.target, event, Some(ix.target));
    animal::use_item(ix.ents, ix.player, ix.hand);
    Interaction::Success
}

/// The synched data: `AgeableMob`, horse flags 18, dash 19, last pose change tick 20.
pub fn encode(w: &mut DataWriter<'_>, m: &MobData) {
    encode_ageable(w, m);
    let c = camel(m).cloned().unwrap_or_default();
    w.byte(18, c.horse_flags, 0);
    w.boolean(19, c.dash, false);
    let tick = c.last_pose_change_tick;
    w.raw(20, serializer::LONG, tick == 0, |o| {
        vanillite_protocol::codec::write_varlong(o, tick);
    });
}

pub fn save(m: &MobData, out: &mut Compound) {
    let can_be_baby = m.kind == Kind::Camel;
    save_ageable(m, out, can_be_baby);
    let c = camel(m).cloned().unwrap_or_default();
    out.insert("EatingHaystack", c.horse_flags & 16 != 0);
    out.insert("Bred", c.horse_flags & 8 != 0);
    out.insert("Temper", c.temper);
    out.insert("Tame", c.horse_flags & 2 != 0);
    out.insert("LastPoseTick", c.last_pose_change_tick);
}

pub fn load(m: &mut MobData, tag: &Compound) {
    let can_be_baby = m.kind == Kind::Camel;
    load_ageable(m, tag, can_be_baby);
    let flags = (u8::from(super::boolean(tag, "Tame")) << 1)
        | (u8::from(super::boolean(tag, "Bred")) << 3)
        | (u8::from(super::boolean(tag, "EatingHaystack")) << 4);
    let temper = super::int(tag, "Temper").unwrap_or(0);
    let tick = long(tag, "LastPoseTick").unwrap_or(0);
    let c = camel_mut(m);
    c.horse_flags = flags;
    c.temper = temper;
    c.last_pose_change_tick = tick;
}

/// The camel's pose for the entity after loading (`setPose(SITTING)` when sitting).
pub fn loaded_pose(m: &MobData) -> Pose {
    if is_sitting(m) {
        Pose::Sitting
    } else {
        Pose::Standing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sitting_dimensions_and_nbt() {
        let mut m = MobData::new(Kind::Camel, 1);
        assert!(!is_sitting(&m));
        camel_mut(&mut m).last_pose_change_tick = -100;
        assert!(is_sitting(&m));
        let d = dimensions(&m);
        assert!((d.height - (2.375 - 1.43)).abs() < 1e-4);
        let mut out = Compound::new();
        save(&m, &mut out);
        let mut n = MobData::new(Kind::Camel, 2);
        load(&mut n, &out);
        assert_eq!(camel(&n), camel(&m));
        assert_eq!(loaded_pose(&n), Pose::Sitting);
    }

    #[test]
    fn pose_transition() {
        let mut m = MobData::new(Kind::Camel, 1);
        camel_mut(&mut m).last_pose_change_tick = full_stand_tick(1000);
        assert!(!in_pose_transition(&m, 1000));
        camel_mut(&mut m).last_pose_change_tick = -1000;
        assert!(in_pose_transition(&m, 1020));
        assert!(!in_pose_transition(&m, 1040));
    }
}
