//! Brain AI mounts and helpers: camels, camel husks, happy ghasts, nautiluses, zombie
//! nautiluses, allays, copper golems, breezes and creakings.
//!
//! Each kind lives in its own module; this module dispatches the family hooks by kind and
//! holds what several of them share (the `AgeableMob`/`Animal` data and NBT, the brain
//! tick).

pub mod ai;
pub mod allay;
pub mod breeze;
pub mod camel;
pub mod copper_golem;
pub mod creaking;
pub mod ghast;
pub mod nautilus;
pub mod transport;
pub mod wind;

use vanillite_base::random::LegacyRandom;
use vanillite_base::{BlockPos, Vec3};
use vanillite_nbt::Compound;
use vanillite_protocol::packets::play::clientbound::SoundSource;
use vanillite_world::entity::{Dimensions, EntityId};

use super::{FamilyHooks, Hurt, Interact, SpawnContext, SpawnReason};
use crate::entities::Entities;
use crate::level::Level;
use crate::mob::animal::Interaction;
use crate::mob::missile::{MissileData, MissileKind, MissileTick};
use crate::mob::path::{MalusTable, PathType};
use crate::mob::{Environment, Kind, MobData, Tick};
use crate::player::encode::DataWriter;
use crate::survival::player::Env;

/// The family's hooks.
#[derive(Debug)]
pub struct Hooks;

/// The kind state of the family's mobs (in [`super::Ext::Mount`]).
#[derive(Clone, Debug, Default)]
pub enum State {
    #[default]
    None,
    Camel(camel::Camel),
    HappyGhast(ghast::HappyGhast),
    Nautilus(nautilus::Nautilus),
    Allay(allay::Allay),
    CopperGolem(copper_golem::CopperGolem),
    Breeze(breeze::Breeze),
    Creaking(creaking::Creaking),
}

/// The family's goals (the adult happy ghast is the only one with goals; the others think
/// with their brain).
#[derive(Clone, Copy, Debug)]
pub enum Goal {
    /// Never runs.
    Idle,
    /// A goal of the adult happy ghast.
    Ghast(ghast::GhastGoal),
}

/// The family state of a mob.
pub fn state(m: &MobData) -> &State {
    static NONE: State = State::None;
    match &m.ext {
        super::Ext::Mount(s) => s,
        _ => &NONE,
    }
}

/// The family state of a mob, mutably (created on first use).
pub fn state_mut(m: &mut MobData) -> &mut State {
    if !matches!(m.ext, super::Ext::Mount(_)) {
        m.ext = super::Ext::Mount(State::None);
    }
    match &mut m.ext {
        super::Ext::Mount(s) => s,
        _ => unreachable!("the mount state was just set"),
    }
}

// --- Shared parts -------------------------------------------------------------------------

/// The `AgeableMob` synched data: 16 baby, 17 age locked.
fn encode_ageable(w: &mut DataWriter<'_>, m: &MobData) {
    w.boolean(16, m.baby, false);
    w.boolean(17, m.age_locked, false);
}

/// UUID as the int array of entity NBT.
fn uuid_ints(uuid: u128) -> Vec<i32> {
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    let v = vec![
        (uuid >> 96) as i32,
        (uuid >> 64) as i32,
        (uuid >> 32) as i32,
        uuid as i32,
    ];
    v
}

fn int(tag: &Compound, key: &str) -> Option<i32> {
    crate::mob::entity_nbt::get_int(tag, key)
}

fn long(tag: &Compound, key: &str) -> Option<i64> {
    tag.get(key).and_then(crate::mob::entity_nbt::long_value)
}

fn boolean(tag: &Compound, key: &str) -> bool {
    crate::mob::entity_nbt::get_bool(tag, key).unwrap_or(false)
}

/// `AgeableMob` and `Animal` NBT (`Age`, `ForcedAge`, `AgeLocked` for kinds that can be
/// babies; `InLove`, `LoveCause` for animals).
fn save_ageable(m: &MobData, out: &mut Compound, can_be_baby: bool) {
    if can_be_baby {
        out.insert("Age", m.age);
        out.insert("ForcedAge", m.forced_age);
        out.insert("AgeLocked", m.age_locked);
    }
    out.insert("InLove", m.in_love);
    if let Some(cause) = m.love_cause {
        out.insert("LoveCause", uuid_ints(cause));
    }
}

fn load_ageable(m: &mut MobData, tag: &Compound, can_be_baby: bool) {
    if can_be_baby {
        m.age = int(tag, "Age").unwrap_or(0);
        m.baby = m.age < 0;
        m.forced_age = int(tag, "ForcedAge").unwrap_or(0);
        m.age_locked = crate::mob::entity_nbt::get_bool(tag, "AgeLocked").unwrap_or(false);
    }
    m.in_love = int(tag, "InLove").unwrap_or(0);
    m.love_cause = super::super::nbt::uuid_key_of(tag, "LoveCause");
}

/// `AgeableMob.finalizeSpawn` with `AgeableMobGroupData(babyChance)`: later members of a
/// group spawn as babies by chance.
fn finalize_ageable(m: &mut MobData, ctx: &SpawnContext<'_>, baby_chance: f32) {
    use vanillite_base::random::RandomSource;
    if ctx.group_index > 0 && m.random.next_float() <= baby_chance {
        m.age = -24000;
        m.baby = true;
    }
}

/// The game time of a spawn (0 without a level).
fn spawn_time(ctx: &SpawnContext<'_>) -> i64 {
    ctx.level.map_or(0, Level::game_time)
}

/// `Animal`'s pathfinding malus: burning blocks are avoided.
const ANIMAL_MALUS: &MalusTable = &[(PathType::FireInNeighbor, 16.0), (PathType::Fire, -1.0)];

/// Plays a sound at the ticking mob (`makeSound`: the mob's volume and voice pitch).
fn make_sound(t: &mut Tick<'_, '_>, sound: &str) {
    let id = t.id;
    crate::mob::combat::mob_sound(t.ents, t.env, id, sound);
}

/// Sends an entity event of the ticking mob.
fn entity_event(t: &mut Tick<'_, '_>, event: i8) {
    let id = t.id;
    crate::mob::living::entity_event(t, id, event);
}

impl FamilyHooks for Hooks {
    type Goal = Goal;

    fn is_monster(kind: Kind) -> bool {
        matches!(kind, Kind::Breeze | Kind::Creaking)
    }

    fn ageable(kind: Kind) -> bool {
        matches!(
            kind,
            Kind::Camel
                | Kind::CamelHusk
                | Kind::HappyGhast
                | Kind::Nautilus
                | Kind::ZombieNautilus
        )
    }

    fn ambient_sound_interval(kind: Kind) -> i32 {
        match kind {
            // `AbstractHorse`.
            Kind::Camel | Kind::CamelHusk => 400,
            // `Animal` and `AbstractGolem`.
            Kind::CopperGolem => 120,
            k if Self::ageable(k) => 120,
            // `Mob`.
            _ => 80,
        }
    }

    fn burns_in_daylight(kind: Kind) -> bool {
        kind == Kind::ZombieNautilus
    }

    fn breathes_underwater(kind: Kind) -> bool {
        // `#can_breathe_under_water` (with `#undead`: the camel husk and zombie nautilus).
        matches!(
            kind,
            Kind::CopperGolem | Kind::Nautilus | Kind::ZombieNautilus | Kind::CamelHusk
        )
    }

    fn fall_damage_immune(kind: Kind) -> bool {
        matches!(
            kind,
            Kind::CopperGolem | Kind::Allay | Kind::HappyGhast | Kind::Breeze
        )
    }

    fn allowed_in_peaceful(kind: Kind) -> bool {
        !matches!(kind, Kind::Breeze | Kind::Creaking)
    }

    fn remove_when_far_away(kind: Kind) -> bool {
        matches!(
            kind,
            Kind::CamelHusk | Kind::Nautilus | Kind::ZombieNautilus | Kind::Breeze | Kind::Creaking
        )
    }

    fn requires_custom_persistence(m: &MobData) -> bool {
        matches!(m.kind, Kind::Nautilus | Kind::ZombieNautilus) && m.tame
    }

    fn malus(kind: Kind) -> &'static MalusTable {
        const NAUTILUS: &MalusTable = &[
            (PathType::FireInNeighbor, 16.0),
            (PathType::Fire, -1.0),
            (PathType::Water, 0.0),
        ];
        const BREEZE: &MalusTable = &[(PathType::OnTopOfTrapdoor, -1.0), (PathType::Fire, -1.0)];
        const COPPER_GOLEM: &MalusTable = &[
            (PathType::FireInNeighbor, 16.0),
            (PathType::DamagingInNeighbor, 16.0),
            (PathType::Fire, -1.0),
        ];
        match kind {
            Kind::Camel | Kind::CamelHusk | Kind::HappyGhast => ANIMAL_MALUS,
            Kind::Nautilus | Kind::ZombieNautilus => NAUTILUS,
            Kind::Breeze => BREEZE,
            Kind::CopperGolem => COPPER_GOLEM,
            _ => &[],
        }
    }

    fn sound_source(kind: Kind) -> SoundSource {
        if Self::is_monster(kind) {
            SoundSource::Hostile
        } else {
            SoundSource::Neutral
        }
    }

    fn sound_volume(kind: Kind) -> f32 {
        match kind {
            Kind::Allay => 0.4,
            // The adult's volume; ghastlings use 1.
            Kind::HappyGhast => 4.0,
            _ => 1.0,
        }
    }

    fn ambient_sound(m: &mut MobData) -> Option<&'static str> {
        match m.kind {
            Kind::Camel | Kind::CamelHusk => Some(camel::ambient_sound(m)),
            Kind::HappyGhast => Some(ghast::ambient_sound(m)),
            Kind::Nautilus | Kind::ZombieNautilus => Some(nautilus::sound(m, "ambient")),
            Kind::Allay => Some(allay::ambient_sound(m)),
            Kind::CopperGolem => None,
            // `Breeze.playAmbientSound` is a local sound of the client.
            Kind::Breeze => None,
            Kind::Creaking => creaking::ambient_sound(m),
            _ => None,
        }
    }

    fn hurt_sound(m: &MobData) -> &'static str {
        match m.kind {
            Kind::Camel => "minecraft:entity.camel.hurt",
            Kind::CamelHusk => "minecraft:entity.camel_husk.hurt",
            Kind::HappyGhast => ghast::hurt_sound(m),
            Kind::Nautilus | Kind::ZombieNautilus => nautilus::sound(m, "hurt"),
            Kind::CopperGolem => copper_golem::sound(m, "hurt"),
            Kind::Creaking => creaking::hurt_sound(m),
            _ => m.kind.default_sound(crate::mob::kinds::DefaultSound::Hurt),
        }
    }

    fn death_sound(m: &MobData) -> &'static str {
        match m.kind {
            Kind::Camel => "minecraft:entity.camel.death",
            Kind::CamelHusk => "minecraft:entity.camel_husk.death",
            Kind::HappyGhast => ghast::death_sound(m),
            Kind::Nautilus | Kind::ZombieNautilus => nautilus::sound(m, "death"),
            Kind::CopperGolem => copper_golem::sound(m, "death"),
            _ => m.kind.default_sound(crate::mob::kinds::DefaultSound::Death),
        }
    }

    fn experience_reward(m: &mut MobData) -> i32 {
        use vanillite_base::random::RandomSource;
        match m.kind {
            Kind::Breeze => 10,
            Kind::Creaking | Kind::Allay | Kind::CopperGolem => 0,
            // `Animal.getBaseExperienceReward`.
            _ => 1 + m.random.next_int_bounded(3),
        }
    }

    fn dimensions(m: &MobData) -> Option<Dimensions> {
        match m.kind {
            Kind::Camel | Kind::CamelHusk => Some(camel::dimensions(m)),
            Kind::HappyGhast if m.baby => {
                let d = Dimensions::of(m.kind.type_id());
                Some(Dimensions {
                    width: d.width * ghast::BABY_SCALE,
                    height: d.height * ghast::BABY_SCALE,
                })
            }
            _ => None,
        }
    }

    fn walk_target_value(kind: Kind, level: &Level, env: &Environment, pos: BlockPos) -> f64 {
        match kind {
            Kind::HappyGhast => ghast::walk_target_value(level, pos),
            // `PathfinderMob`'s 0 (nautiluses and creakings override it to 0 as well).
            Kind::Nautilus
            | Kind::ZombieNautilus
            | Kind::Creaking
            | Kind::Allay
            | Kind::CopperGolem => 0.0,
            Kind::Breeze => crate::mob::kinds::base_walk_target_value(true, level, env, pos),
            _ => crate::mob::kinds::base_walk_target_value(false, level, env, pos),
        }
    }

    fn check_spawn_rules(
        kind: Kind,
        level: &Level,
        env: &Environment,
        pos: BlockPos,
        random: &mut LegacyRandom,
        reason: SpawnReason,
    ) -> bool {
        use crate::mob::spawn;
        match kind {
            Kind::Camel => camel::check_spawn_rules(level, pos),
            Kind::CamelHusk => {
                spawn::check_surface_monsters_spawn_rules(kind, level, env, pos, random, reason)
            }
            Kind::HappyGhast => spawn::check_animal_spawn_rules(kind, level, env, pos, reason),
            Kind::Nautilus => nautilus::check_spawn_rules(level, env, pos),
            Kind::Breeze => spawn::check_any_light_monster_spawn_rules(kind, level, pos, reason),
            Kind::Creaking => {
                spawn::check_monster_spawn_rules(kind, level, env, pos, random, reason)
            }
            // Not in `SpawnPlacements`: no rules.
            _ => true,
        }
    }

    fn is_food(kind: Kind, item: u16) -> bool {
        let tag = match kind {
            Kind::Camel => "minecraft:camel_food",
            Kind::CamelHusk => "minecraft:camel_husk_food",
            Kind::HappyGhast => "minecraft:happy_ghast_food",
            Kind::Nautilus | Kind::ZombieNautilus => "minecraft:nautilus_food",
            _ => return false,
        };
        crate::player::play::game_data().is_some_and(|d| d.item_in_tag(item, tag))
    }

    fn init(m: &mut MobData) {
        match m.kind {
            Kind::Camel | Kind::CamelHusk => camel::init(m),
            Kind::HappyGhast => ghast::init(m),
            Kind::Nautilus | Kind::ZombieNautilus => nautilus::init(m),
            Kind::Allay => allay::init(m),
            Kind::CopperGolem => copper_golem::init(m),
            Kind::Breeze => breeze::init(m),
            Kind::Creaking => creaking::init(m),
            _ => {}
        }
    }

    fn register_goals(m: &mut MobData) {
        if m.kind == Kind::HappyGhast {
            ghast::register_goals(m);
        }
    }

    fn finalize_spawn(m: &mut MobData, ctx: &mut SpawnContext<'_>) {
        match m.kind {
            Kind::Camel => {
                camel::finalize(m, ctx);
                finalize_ageable(m, ctx, 0.2);
            }
            Kind::CamelHusk => camel::finalize(m, ctx),
            Kind::HappyGhast => finalize_ageable(m, ctx, 0.05),
            Kind::Nautilus => {
                nautilus::finalize(m, ctx);
                finalize_ageable(m, ctx, 0.05);
            }
            Kind::ZombieNautilus => nautilus::finalize(m, ctx),
            Kind::CopperGolem => copper_golem::finalize(m),
            _ => {}
        }
    }

    fn before_tick(t: &mut Tick<'_, '_>) {
        match t.mob_ref().kind {
            Kind::Breeze => breeze::before_tick(t),
            Kind::Creaking => creaking::before_tick(t),
            _ => {}
        }
    }

    fn after_tick(t: &mut Tick<'_, '_>) {
        match t.mob_ref().kind {
            Kind::Camel | Kind::CamelHusk => camel::after_tick(t),
            Kind::HappyGhast => ghast::after_tick(t),
            Kind::Nautilus | Kind::ZombieNautilus => nautilus::after_tick(t),
            Kind::CopperGolem => copper_golem::after_tick(t),
            Kind::Allay => allay::after_tick(t),
            _ => {}
        }
    }

    fn is_immobile(m: &MobData) -> bool {
        matches!(m.kind, Kind::Camel | Kind::CamelHusk) && camel::is_immobile(m)
    }

    fn ai_step_start(t: &mut Tick<'_, '_>) {
        if matches!(t.mob_ref().kind, Kind::Camel | Kind::CamelHusk) {
            camel::ai_step_start(t);
        }
    }

    fn ai_step(t: &mut Tick<'_, '_>) {
        match t.mob_ref().kind {
            Kind::Camel | Kind::CamelHusk => camel::ai_step(t),
            Kind::HappyGhast => ghast::ai_step(t),
            Kind::Allay => allay::ai_step(t),
            Kind::Creaking => creaking::ai_step(t),
            Kind::Nautilus => nautilus::ai_step(t),
            _ => {}
        }
    }

    fn custom_server_ai_step(t: &mut Tick<'_, '_>) {
        match t.mob_ref().kind {
            Kind::Camel | Kind::CamelHusk => camel::custom_server_ai_step(t),
            Kind::HappyGhast => ghast::custom_server_ai_step(t),
            Kind::Nautilus | Kind::ZombieNautilus => nautilus::custom_server_ai_step(t),
            Kind::Allay => allay::custom_server_ai_step(t),
            Kind::CopperGolem => copper_golem::custom_server_ai_step(t),
            Kind::Breeze => breeze::custom_server_ai_step(t),
            Kind::Creaking => creaking::custom_server_ai_step(t),
            _ => {}
        }
    }

    fn travel(t: &mut Tick<'_, '_>, input: Vec3) -> bool {
        match t.mob_ref().kind {
            Kind::Camel | Kind::CamelHusk => camel::travel(t, input),
            Kind::HappyGhast => ghast::travel(t, input),
            Kind::Allay => allay::travel(t, input),
            Kind::Nautilus | Kind::ZombieNautilus => nautilus::travel(t, input),
            Kind::Breeze => breeze::travel(t, input),
            _ => false,
        }
    }

    fn move_control(t: &mut Tick<'_, '_>) -> bool {
        match t.mob_ref().kind {
            Kind::HappyGhast => ghast::move_control(t),
            Kind::Creaking => !creaking::can_move(t.mob_ref()),
            Kind::Camel | Kind::CamelHusk => camel::move_control(t),
            _ => false,
        }
    }

    fn look_control(t: &mut Tick<'_, '_>) -> bool {
        match t.mob_ref().kind {
            Kind::HappyGhast => ghast::look_control(t),
            Kind::Creaking => !creaking::can_move(t.mob_ref()),
            Kind::Camel | Kind::CamelHusk => camel::look_control(t),
            _ => false,
        }
    }

    fn on_fall_damage(t: &mut Tick<'_, '_>, distance: f64) {
        if t.mob_ref().kind == Kind::Breeze && distance > 3.0 {
            crate::mob::kinds::play_sound(t, "minecraft:entity.breeze.land", 1.0, 1.0);
        }
    }

    fn do_hurt_target(t: &mut Tick<'_, '_>, target: EntityId) -> Option<bool> {
        match t.mob_ref().kind {
            Kind::Creaking => Some(creaking::do_hurt_target(t, target)),
            _ => None,
        }
    }

    fn hurt_filter(h: &mut Hurt<'_, '_>, amount: f32) -> Option<f32> {
        let kind = h.ents.mob(h.target).map(|m| m.kind)?;
        match kind {
            Kind::Allay => allay::hurt_filter(h, amount),
            Kind::Breeze => breeze::hurt_filter(h, amount),
            Kind::Creaking => creaking::hurt_filter(h, amount),
            Kind::Camel | Kind::CamelHusk => {
                camel::stand_up_instantly(h.ents, h.env, h.target);
                Some(amount)
            }
            _ => Some(amount),
        }
    }

    fn after_hurt(h: &mut Hurt<'_, '_>, _amount: f32) {
        let Some(kind) = h.ents.mob(h.target).map(|m| m.kind) else {
            return;
        };
        match kind {
            Kind::Nautilus | Kind::ZombieNautilus => nautilus::after_hurt(h),
            Kind::CopperGolem => copper_golem::after_hurt(h),
            _ => {}
        }
    }

    fn on_die(h: &mut Hurt<'_, '_>) {
        if h.ents.mob(h.target).is_some_and(|m| m.kind == Kind::Allay) {
            allay::on_die(h);
        }
    }

    fn interact(ix: &mut Interact<'_, '_>) -> Interaction {
        let Some(kind) = ix.ents.mob(ix.target).map(|m| m.kind) else {
            return Interaction::Pass;
        };
        match kind {
            Kind::Camel | Kind::CamelHusk => camel::interact(ix),
            Kind::HappyGhast => ghast::interact(ix),
            Kind::Nautilus | Kind::ZombieNautilus => nautilus::interact(ix),
            Kind::Allay => allay::interact(ix),
            Kind::CopperGolem => copper_golem::interact(ix),
            _ => Interaction::Pass,
        }
    }

    fn breed_offspring(
        parent: &MobData,
        _partner: &MobData,
        child: &mut MobData,
        _random: &mut LegacyRandom,
    ) {
        // `Nautilus.getBreedOffspring`: a baby of a tame nautilus is tame with its owner.
        if parent.kind == Kind::Nautilus && parent.tame {
            child.owner = parent.owner;
            child.tame = true;
        }
    }

    fn encode_data(w: &mut DataWriter<'_>, m: &MobData) {
        match m.kind {
            Kind::Camel | Kind::CamelHusk => camel::encode(w, m),
            Kind::HappyGhast => ghast::encode(w, m),
            Kind::Nautilus | Kind::ZombieNautilus => nautilus::encode(w, m),
            Kind::Allay => allay::encode(w, m),
            Kind::CopperGolem => copper_golem::encode(w, m),
            Kind::Creaking => creaking::encode(w, m),
            _ => {}
        }
    }

    fn pick_up_items(t: &mut Tick<'_, '_>) {
        // The others never pick up loot.
        if t.mob_ref().kind == Kind::Allay {
            allay::pick_up_items(t);
        }
    }

    fn save(m: &MobData, out: &mut Compound) {
        match m.kind {
            Kind::Camel | Kind::CamelHusk => camel::save(m, out),
            Kind::HappyGhast => ghast::save(m, out),
            Kind::Nautilus | Kind::ZombieNautilus => nautilus::save(m, out),
            Kind::Allay => allay::save(m, out),
            Kind::CopperGolem => copper_golem::save(m, out),
            Kind::Creaking => creaking::save(m, out),
            _ => {}
        }
    }

    fn load(m: &mut MobData, tag: &Compound) {
        match m.kind {
            Kind::Camel | Kind::CamelHusk => camel::load(m, tag),
            Kind::HappyGhast => ghast::load(m, tag),
            Kind::Nautilus | Kind::ZombieNautilus => nautilus::load(m, tag),
            Kind::Allay => allay::load(m, tag),
            Kind::CopperGolem => copper_golem::load(m, tag),
            Kind::Creaking => creaking::load(m, tag),
            _ => {}
        }
    }

    fn missile_tick(t: &mut MissileTick<'_, '_>) {
        wind::tick(t);
    }

    fn missile_save(d: &MissileData, out: &mut Compound) {
        wind::save(d, out);
    }

    fn missile_load(d: &mut MissileData, tag: &Compound) {
        wind::load(d, tag);
    }

    fn max_passengers(m: &MobData) -> usize {
        match m.kind {
            // `canAddPassenger`: `size() <= 2`, `mobInteract` lets two players on.
            Kind::Camel | Kind::CamelHusk => 3,
            Kind::HappyGhast => 4,
            _ => 1,
        }
    }

    fn rider_controls(ents: &Entities, player: EntityId, vehicle: EntityId) -> bool {
        let Some(m) = ents.mob(vehicle) else {
            return false;
        };
        let _ = player;
        match m.kind {
            Kind::Camel | Kind::CamelHusk => camel::is_saddled(m),
            Kind::HappyGhast => ghast::rider_controls(m),
            Kind::Nautilus | Kind::ZombieNautilus => nautilus::is_saddled(m),
            _ => false,
        }
    }

    fn rider_jump(ents: &mut Entities, env: &mut Env<'_>, vehicle: EntityId, strength: i32) {
        let Some(kind) = ents.mob(vehicle).map(|m| m.kind) else {
            return;
        };
        match kind {
            Kind::Camel | Kind::CamelHusk => camel::handle_start_jump(ents, env, vehicle, strength),
            Kind::Nautilus | Kind::ZombieNautilus => {
                nautilus::handle_start_jump(ents, env, vehicle, strength);
            }
            _ => {}
        }
    }

    fn goal_flags(g: &Goal) -> u8 {
        match g {
            Goal::Ghast(g) => ghast::goal_flags(g),
            Goal::Idle => 0,
        }
    }

    fn goal_every_tick(g: &Goal) -> bool {
        matches!(g, Goal::Ghast(g) if ghast::goal_every_tick(g))
    }

    fn goal_can_use(t: &mut Tick<'_, '_>, g: &mut Goal) -> bool {
        match g {
            Goal::Ghast(g) => ghast::goal_can_use(t, g),
            Goal::Idle => false,
        }
    }

    fn goal_can_continue(t: &mut Tick<'_, '_>, g: &mut Goal) -> bool {
        match g {
            Goal::Ghast(g) => ghast::goal_can_continue(t, g),
            Goal::Idle => false,
        }
    }

    fn goal_start(t: &mut Tick<'_, '_>, g: &mut Goal) {
        if let Goal::Ghast(g) = g {
            ghast::goal_start(t, g);
        }
    }

    fn goal_stop(t: &mut Tick<'_, '_>, g: &mut Goal) {
        if let Goal::Ghast(g) = g {
            ghast::goal_stop(t, g);
        }
    }

    fn goal_tick(t: &mut Tick<'_, '_>, g: &mut Goal) {
        if let Goal::Ghast(g) = g {
            ghast::goal_tick(t, g);
        }
    }
}

/// Whether a missile kind belongs to this family.
pub fn owns_missile(kind: MissileKind) -> bool {
    matches!(
        kind,
        MissileKind::WindCharge | MissileKind::BreezeWindCharge
    )
}

// --- Brain plug-ins (`brain::behavior::FamBehavior`, `brain::sensor::FamSensor`) ---------

use crate::mob::brain::behavior::Shape;
use crate::mob::brain::{MemoryStatus, MemoryType};

/// The family's brain behaviours (implemented in [`ai`]).
#[derive(Clone, Copy, Debug)]
pub enum Behavior {
    /// Never runs.
    Idle,
    /// `CamelAi.RandomSitting(minimalPoseTimeSec)`.
    CamelSitting { min_pose_ticks: i32 },
    /// `GoAndGiveItemsToTarget` of allays.
    GiveItems,
    /// `StayCloseToTarget` of allays.
    StayClose,
    /// `ChargeAttack` of nautiluses.
    Charge {
        speed: f32,
        zombie: bool,
        start: Vec3,
        velocity: Vec3,
    },
    /// The breeze's `Shoot`.
    BreezeShoot,
    /// The breeze's `LongJump`.
    BreezeLongJump,
    /// The breeze's `ShootWhenStuck`.
    BreezeShootWhenStuck,
    /// The breeze's `Slide`.
    BreezeSlide,
    /// The copper golem's `TransportItemsBetweenContainers`.
    GolemTransport(transport::Transport),
}

/// The family's brain sensors.
#[derive(Clone, Copy, Debug)]
pub enum Sensor {
    /// Never ticks.
    Idle,
    /// `BreezeAttackEntitySensor`.
    BreezeAttackEntity,
}

pub fn sensor_scan_rate(s: Sensor) -> i32 {
    ai::sensor_scan_rate(s)
}
pub fn sensor_requires(s: &Sensor) -> &'static [MemoryType] {
    ai::sensor_requires(s)
}
pub fn sensor_tick(t: &mut Tick<'_, '_>, s: Sensor) {
    ai::sensor_tick(t, s);
}
pub fn behavior_duration(b: &Behavior) -> (i32, i32) {
    ai::behavior_duration(b)
}
pub fn behavior_shape(b: &Behavior) -> Shape {
    ai::behavior_shape(b)
}
pub fn behavior_requirements(b: &Behavior) -> &'static [(MemoryType, MemoryStatus)] {
    ai::behavior_requirements(b)
}
pub fn behavior_no_timeout(b: &Behavior) -> bool {
    ai::behavior_no_timeout(b)
}
pub fn behavior_check(t: &mut Tick<'_, '_>, b: &mut Behavior) -> bool {
    ai::behavior_check(t, b)
}
pub fn behavior_can_still_use(t: &mut Tick<'_, '_>, b: &mut Behavior, now: i64) -> bool {
    ai::behavior_can_still_use(t, b, now)
}
pub fn behavior_start(t: &mut Tick<'_, '_>, b: &mut Behavior, now: i64) {
    ai::behavior_start(t, b, now);
}
pub fn behavior_tick(t: &mut Tick<'_, '_>, b: &mut Behavior, now: i64) {
    ai::behavior_tick(t, b, now);
}
pub fn behavior_stop(t: &mut Tick<'_, '_>, b: &mut Behavior, now: i64) {
    ai::behavior_stop(t, b, now);
}
pub fn behavior_trigger(t: &mut Tick<'_, '_>, b: &mut Behavior, now: i64) -> bool {
    ai::behavior_trigger(t, b, now)
}
