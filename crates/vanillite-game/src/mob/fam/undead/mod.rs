//! Zombie and skeleton variants, the small monsters (cave spider, silverfish, endermite), the
//! cubes (slime, magma cube, sulfur cube), giants and phantoms.
//!
//! Each group lives in its own module; this one keeps the family's state and goals and
//! dispatches the hooks by kind. The classic zombie and skeleton (in `kinds.rs`) use the
//! zombie finalization and the water and powder snow conversions from here.

pub mod arthropods;
pub mod cubes;
pub mod phantom;
pub mod skeletons;
pub mod trident;
pub mod zombies;

use vanillite_base::random::LegacyRandom;
use vanillite_base::{BlockPos, Vec3};
use vanillite_nbt::Compound;
use vanillite_protocol::packets::play::clientbound::SoundSource;
use vanillite_world::entity::{Dimensions, EntityId, Rotation};

use super::{FamilyHooks, Hurt, Interact, SpawnContext, SpawnReason};
use crate::level::Level;
use crate::mob::animal::Interaction;
use crate::mob::common::CommonGoal;
use crate::mob::goals::{self, TargetState};
use crate::mob::missile::{MissileData, MissileTick};
use crate::mob::path::{MalusTable, PathType};
use crate::mob::{Environment, Kind, MobData, Tick};
use crate::player::encode::DataWriter;
use crate::survival::damage::DamageSource;
use crate::survival::player::Env;

pub use skeletons::skeleton_arrow;

/// The family's hooks.
#[derive(Debug)]
pub struct Hooks;

/// The kind state of the family's mobs (in [`super::Ext::Undead`]).
#[derive(Clone, Debug, Default)]
pub enum State {
    #[default]
    None,
    Zombie(zombies::ZombieState),
    Skeleton(skeletons::SkeletonState),
    Arthropod(arthropods::ArthropodState),
    Cube(cubes::CubeState),
    Phantom(phantom::PhantomState),
}

/// The state of a family mob, created on first use (the classic zombie and skeleton get theirs
/// when they first need it).
pub fn state_mut(m: &mut MobData) -> &mut State {
    if !matches!(m.ext, super::Ext::Undead(_)) {
        m.ext = super::Ext::Undead(State::None);
    }
    match &mut m.ext {
        super::Ext::Undead(s) => s,
        _ => unreachable!("the undead state was just set"),
    }
}

/// The state of a family mob, read only.
pub fn state(m: &MobData) -> &State {
    static NONE: State = State::None;
    match &m.ext {
        super::Ext::Undead(s) => s,
        _ => &NONE,
    }
}

/// The fields of a `MeleeAttackGoal` a family goal carries (a family goal can not hold a
/// [`goals::Goal`] itself).
#[derive(Clone, Copy, Debug)]
pub struct Melee {
    pub kind: goals::MeleeKind,
    pub speed: f64,
    pub follow_unseen: bool,
    pathed: Vec3,
    recalc: i32,
    attack_in: i32,
    last_can_use: i64,
    raise_arm: i32,
}

impl Melee {
    pub fn new(kind: goals::MeleeKind, speed: f64, follow_unseen: bool) -> Melee {
        Melee {
            kind,
            speed,
            follow_unseen,
            pathed: Vec3::ZERO,
            recalc: 0,
            attack_in: 0,
            // `lastCanUseCheck` starts at 0: no attack in a world's first 20 ticks.
            last_can_use: 0,
            raise_arm: 0,
        }
    }

    /// Runs `f` on the equivalent common goal and takes its state back.
    pub fn with<R>(
        &mut self,
        t: &mut Tick<'_, '_>,
        f: impl FnOnce(&mut Tick<'_, '_>, &mut goals::Goal) -> R,
    ) -> R {
        let mut g = goals::Goal::Melee {
            kind: self.kind,
            speed: self.speed,
            follow_unseen: self.follow_unseen,
            pathed: self.pathed,
            recalc: self.recalc,
            attack_in: self.attack_in,
            last_can_use: self.last_can_use,
            raise_arm: self.raise_arm,
        };
        let r = f(t, &mut g);
        if let goals::Goal::Melee {
            pathed,
            recalc,
            attack_in,
            last_can_use,
            raise_arm,
            ..
        } = g
        {
            self.pathed = pathed;
            self.recalc = recalc;
            self.attack_in = attack_in;
            self.last_can_use = last_can_use;
            self.raise_arm = raise_arm;
        }
        r
    }
}

/// The fields of a `HurtByTargetGoal`.
#[derive(Clone, Copy, Debug)]
pub struct HurtBy {
    state: TargetState,
    timestamp: i32,
    alert: bool,
}

impl HurtBy {
    pub fn new(alert: bool) -> HurtBy {
        HurtBy {
            state: TargetState::new(true),
            timestamp: 0,
            alert,
        }
    }

    /// Runs `f` on the equivalent common goal and takes its state back.
    pub fn with<R>(
        &mut self,
        t: &mut Tick<'_, '_>,
        f: impl FnOnce(&mut Tick<'_, '_>, &mut goals::Goal) -> R,
    ) -> R {
        let mut g = goals::Goal::HurtBy {
            state: self.state,
            timestamp: self.timestamp,
            alert: self.alert,
        };
        let r = f(t, &mut g);
        if let goals::Goal::HurtBy {
            state, timestamp, ..
        } = g
        {
            self.state = state;
            self.timestamp = timestamp;
        }
        r
    }
}

/// The family's goals.
#[derive(Clone, Copy, Debug)]
pub enum Goal {
    /// Never runs.
    Idle,
    /// `Drowned.DrownedAttackGoal`: the zombie attack while the target is fine to attack.
    DrownedMelee(Melee),
    /// `Drowned.DrownedTridentAttackGoal`.
    DrownedTrident(CommonGoal),
    /// `Drowned.DrownedGoToWaterGoal`.
    DrownedGoToWater { speed: f64, wanted: Vec3 },
    /// `Drowned.DrownedGoToBeachGoal`.
    DrownedGoToBeach(crate::mob::common::MoveToBlock),
    /// `Drowned.DrownedSwimUpGoal`.
    DrownedSwimUp { speed: f64, stuck: bool },
    /// `HurtByTargetGoal(this, Drowned.class)`: damage by drowned does not count.
    HurtByIgnoringDrowned(HurtBy),
    /// `SpearUseGoal(mob, 1.0, 1.0, 10.0F, 2.0F)` of the zombies.
    SpearUse(zombies::SpearUse),
    /// The melee goal of a skeleton without a bow (`AbstractSkeleton.meleeGoal`).
    SkeletonMelee(Melee),
    /// `Spider.SpiderTargetGoal` for other targets than players.
    SpiderTarget(CommonGoal),
    /// `Silverfish.SilverfishWakeUpFriendsGoal`.
    SilverfishWakeUpFriends,
    /// `Silverfish.SilverfishMergeWithStoneGoal`.
    SilverfishMergeWithStone {
        wanted: Vec3,
        direction: u8,
        do_merge: bool,
    },
    /// `CubeMobFloatGoal`.
    CubeFloat,
    /// `CubeMobAttackGoal`.
    CubeAttack { grow_tired: i32 },
    /// `CubeMobRandomDirectionGoal`.
    CubeRandomDirection { chosen: f32, next: i32 },
    /// `CubeMobKeepOnJumpingGoal`.
    CubeKeepOnJumping,
    /// `SulfurCube.SulfurCubeTemptGoal`.
    SulfurTempt {
        player: Option<EntityId>,
        calm_down: i32,
    },
    /// `SulfurCube.SulfurCubeSearchForItemsGoal`.
    SulfurSearchItems { item: Option<EntityId> },
    /// `Phantom.PhantomAttackStrategyGoal`.
    PhantomAttackStrategy { next_sweep: i32 },
    /// `Phantom.PhantomSweepAttackGoal`.
    PhantomSweep {
        scared_of_cat: bool,
        cat_search: i32,
    },
    /// `Phantom.PhantomCircleAroundAnchorGoal`.
    PhantomCircle {
        angle: f32,
        distance: f32,
        height: f32,
        clockwise: f32,
    },
    /// `Phantom.PhantomAttackPlayerTargetGoal`.
    PhantomTargetPlayer { next_scan: i32 },
}

/// The goal as a common goal of the selector.
pub fn goal(g: Goal) -> goals::Goal {
    goals::Goal::Fam(super::FamGoal::Undead(g))
}

/// Which group implements a kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Group {
    Zombie,
    Skeleton,
    Arthropod,
    Cube,
    Phantom,
}

fn group(kind: Kind) -> Group {
    match kind {
        Kind::Husk | Kind::Drowned | Kind::ZombieVillager | Kind::ZombifiedPiglin | Kind::Giant => {
            Group::Zombie
        }
        Kind::Stray | Kind::Bogged | Kind::Parched | Kind::WitherSkeleton => Group::Skeleton,
        Kind::CaveSpider | Kind::Silverfish | Kind::Endermite => Group::Arthropod,
        Kind::Slime | Kind::MagmaCube | Kind::SulfurCube => Group::Cube,
        _ => Group::Phantom,
    }
}

fn goal_group(g: &Goal) -> Group {
    match g {
        Goal::DrownedMelee(_)
        | Goal::DrownedTrident(_)
        | Goal::DrownedGoToWater { .. }
        | Goal::DrownedGoToBeach(_)
        | Goal::DrownedSwimUp { .. }
        | Goal::HurtByIgnoringDrowned(_)
        | Goal::SpearUse(_) => Group::Zombie,
        Goal::SkeletonMelee(_) => Group::Skeleton,
        Goal::SpiderTarget(_)
        | Goal::SilverfishWakeUpFriends
        | Goal::SilverfishMergeWithStone { .. } => Group::Arthropod,
        Goal::CubeFloat
        | Goal::CubeAttack { .. }
        | Goal::CubeRandomDirection { .. }
        | Goal::CubeKeepOnJumping
        | Goal::SulfurTempt { .. }
        | Goal::SulfurSearchItems { .. } => Group::Cube,
        Goal::Idle
        | Goal::PhantomAttackStrategy { .. }
        | Goal::PhantomSweep { .. }
        | Goal::PhantomCircle { .. }
        | Goal::PhantomTargetPlayer { .. } => Group::Phantom,
    }
}

/// `undead`: the zombies, skeletons and phantoms.
pub fn is_undead(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::Husk
            | Kind::Drowned
            | Kind::ZombieVillager
            | Kind::ZombifiedPiglin
            | Kind::Stray
            | Kind::Bogged
            | Kind::Parched
            | Kind::WitherSkeleton
            | Kind::Phantom
    )
}

/// Plays a sound of the ticking mob at its position (`playSound`).
pub fn sound(t: &mut Tick<'_, '_>, sound: &str, volume: f32, pitch: f32) {
    crate::mob::kinds::play_sound(t, sound, volume, pitch);
}

/// Queues a level event at a block (`levelEvent(event, pos, data)`).
pub fn level_event(cx: &mut crate::mob::MobContext, event: i32, pos: BlockPos, data: i32) {
    cx.actions
        .push(crate::mob::WorldAction::LevelEvent(event, pos, data));
}

/// `Mob.convertTo(type, ConversionParams.single(mob, keepEquipment, preserveCanPickUpLoot))`:
/// replaces the ticking mob by a new mob of `kind` at the same place with its equipment,
/// effects, age, name and flags. The old mob is removed; returns the new one. `reason` is the
/// plugins' `vanillite_abi::transform_reason`; `None` when a plugin cancelled the conversion
/// (the old mob stays).
pub fn convert_to(
    t: &mut Tick<'_, '_>,
    kind: Kind,
    keep_equipment: bool,
    preserve_loot: bool,
    reason: u32,
    after: impl FnOnce(&mut MobData),
) -> Option<EntityId> {
    if t.mob_ref().removed {
        return None;
    }
    let seed = crate::mob::new_entity_seed();
    let mut new = MobData::new(kind, seed);
    let old = t.mob_ref();
    if keep_equipment {
        new.equipment = old.equipment.clone();
    }
    new.fall_distance = old.fall_distance;
    new.hurt_time = old.hurt_time;
    new.y_body_rot = old.y_body_rot;
    new.last_hurt_by_player_time = old.last_hurt_by_player_time;
    for e in old.effects.iter() {
        crate::effect::living::add_to_mob(&mut new, e.clone());
    }
    if old.baby {
        set_baby(&mut new, true);
    }
    if old.kind.ageable() && kind.ageable() {
        new.age = old.age;
        new.forced_age = old.forced_age;
        new.forced_age_timer = old.forced_age_timer;
    }
    if preserve_loot {
        new.can_pick_up_loot = old.can_pick_up_loot;
    }
    new.left_handed = old.left_handed;
    new.no_ai = old.no_ai;
    new.persistent = old.persistent;
    new.silent = old.silent;
    new.custom_name.clone_from(&old.custom_name);
    new.fire_ticks = old.fire_ticks;
    let silent = new.silent;
    after(&mut new);
    let (pos, velocity, rot, flags) = {
        let e = t.core();
        (e.pos(), e.velocity(), e.rot(), e.flags())
    };
    let uuid = crate::mob::new_uuid();
    if !t.ents.transforming(t.id, kind.type_id(), uuid, reason) {
        return None;
    }
    let vehicle = t.ents.vehicle_of(t.id);
    let id = crate::mob::spawn(t.ents, t.level.network_ids(), uuid, pos, rot, new);
    let old_uuid = t.core().uuid();
    if !t
        .ents
        .spawned(id, vanillite_abi::spawn_reason::CONVERSION, old_uuid)
    {
        return None;
    }
    if let Some(mut e) = t.ents.store.get_mut(id) {
        e.set_velocity(velocity);
        e.set_flags(flags, true);
    }
    if let Some(v) = vehicle {
        t.ents.riding.remove(t.id);
        t.ents.riding.add(id, v, false);
    }
    let _ = silent;
    let m = t.mob();
    m.removed = true;
    Some(id)
}

/// Adds the mobs spawned with `id` (`addFreshEntityWithPassengers` from a tick).
pub fn spawn_extras(
    ents: &mut crate::entities::Entities,
    level: &Level,
    id: EntityId,
    pos: Vec3,
    extra: Vec<super::ExtraMob>,
    reason: super::SpawnReason,
) {
    use super::Relation;
    if extra.is_empty() {
        return;
    }
    let parent = ents.store.get(id).map_or(0, |e| e.uuid());
    let mut spawned = Vec::with_capacity(extra.len());
    for e in extra {
        if let Relation::Existing(vehicle) = e.relation {
            ents.riding.add(id, vehicle, false);
            spawned.push(vehicle);
            continue;
        }
        let mob = e.mob;
        let uuid = crate::mob::new_uuid();
        let rot = rotation(mob.y_body_rot);
        let other = crate::mob::spawn(ents, level.network_ids(), uuid, pos + e.offset, rot, mob);
        spawned.push(other);
        match e.relation {
            Relation::RidesIt => ents.riding.add(other, id, false),
            Relation::CarriesIt => ents.riding.add(id, other, false),
            Relation::RidesExtra(i) => {
                if let Some(&v) = spawned.get(i) {
                    ents.riding.add(other, v, false);
                }
            }
            Relation::Beside | Relation::Existing(_) => {}
        }
        let reason = if matches!(e.relation, Relation::Beside) {
            reason
        } else {
            super::SpawnReason::Jockey
        };
        ents.spawned(other, reason.abi(), parent);
    }
}

/// `spawnAtLocation`: an item entity at `pos` with the default random motion.
pub fn drop_item(env: &mut Env<'_>, pos: Vec3, stack: crate::item::ItemStack) {
    if stack.is_empty() {
        return;
    }
    let (uuid, y_rot, velocity) = crate::mob::new_item_entity();
    env.out.push(crate::survival::Effect::DropItem {
        uuid,
        pos,
        velocity,
        y_rot,
        pickup_delay: 10,
        stack,
    });
}

/// The items of a loot table without entity or block conditions (shearing tables).
pub fn simple_table_loot(
    level: &crate::level::Level,
    table: &str,
    random: &mut LegacyRandom,
) -> Vec<crate::item::ItemStack> {
    let mut out = Vec::new();
    let Some(data) = crate::player::play::game_data() else {
        return out;
    };
    if !data.loot.has_table(table) {
        return out;
    }
    let none = |_: &str| 0;
    let in_tag = |item: u16, tag: &str| data.item_in_tag(item, tag);
    let empty = crate::item::ItemStack::default();
    let ctx = crate::loot::BlockLootContext {
        state: vanillite_data::block::BlockState::AIR,
        tool: &empty,
        enchantment_level: &none,
        item_in_tag: &in_tag,
        explosion_radius: None,
        this_present: false,
        entity: None,
        block_entity: None,
    };
    data.loot.roll_table(level, table, &ctx, random, &mut out);
    out
}

/// `setBaby` with the size and the baby speed bonus of the zombies.
pub fn set_baby(m: &mut MobData, baby: bool) {
    if m.baby == baby {
        return;
    }
    m.baby = baby;
    if zombies::is_zombie(m.kind) {
        zombies::baby_speed(m, baby);
    }
}

/// The hooks of a group, for the dispatch below.
macro_rules! by_group {
    ($kind:expr, $f:ident($($arg:expr),*), $default:expr) => {
        match group($kind) {
            Group::Zombie => zombies::$f($($arg),*),
            Group::Skeleton => skeletons::$f($($arg),*),
            Group::Arthropod => arthropods::$f($($arg),*),
            Group::Cube => cubes::$f($($arg),*),
            Group::Phantom => phantom::$f($($arg),*),
        }
    };
}

impl FamilyHooks for Hooks {
    type Goal = Goal;

    fn is_monster(kind: Kind) -> bool {
        kind != Kind::SulfurCube
    }

    fn ageable(kind: Kind) -> bool {
        // `AbstractCubeMob` is an `AgeableMob`; only sulfur cubes can be babies.
        kind == Kind::SulfurCube
    }

    fn ambient_sound_interval(_kind: Kind) -> i32 {
        80
    }

    fn burns_in_daylight(kind: Kind) -> bool {
        matches!(
            kind,
            Kind::Stray
                | Kind::WitherSkeleton
                | Kind::Bogged
                | Kind::ZombieVillager
                | Kind::Drowned
                | Kind::Phantom
        )
    }

    fn breathes_underwater(kind: Kind) -> bool {
        is_undead(kind)
    }

    fn fall_damage_immune(kind: Kind) -> bool {
        matches!(kind, Kind::Phantom | Kind::MagmaCube)
    }

    fn allowed_in_peaceful(kind: Kind) -> bool {
        kind == Kind::SulfurCube
    }

    fn remove_when_far_away(kind: Kind) -> bool {
        // `Mob.removeWhenFarAway`; the zombie villager's depends on its state.
        let _ = kind;
        true
    }

    fn requires_custom_persistence(m: &MobData) -> bool {
        match m.kind {
            Kind::ZombieVillager => !zombies::zombie_villager_removes_when_far(m),
            Kind::SulfurCube => cubes::sulfur_persistent(m),
            _ => false,
        }
    }

    fn malus(kind: Kind) -> &'static MalusTable {
        const LAVA: &MalusTable = &[(PathType::Lava, 8.0)];
        const WATER: &MalusTable = &[(PathType::Water, 0.0)];
        match kind {
            Kind::ZombifiedPiglin | Kind::WitherSkeleton => LAVA,
            Kind::Drowned => WATER,
            _ => &[],
        }
    }

    fn sound_source(kind: Kind) -> SoundSource {
        if kind == Kind::SulfurCube {
            SoundSource::Neutral
        } else {
            SoundSource::Hostile
        }
    }

    fn mob_sound_volume(m: &MobData) -> f32 {
        match m.kind {
            Kind::Slime | Kind::MagmaCube | Kind::SulfurCube => cubes::sound_volume(m),
            _ => 1.0,
        }
    }

    fn ambient_sound(m: &mut MobData) -> Option<&'static str> {
        by_group!(m.kind, ambient_sound(m), None)
    }

    fn hurt_sound(m: &MobData) -> &'static str {
        by_group!(m.kind, hurt_sound(m), "")
    }

    fn death_sound(m: &MobData) -> &'static str {
        by_group!(m.kind, death_sound(m), "")
    }

    fn experience_reward(m: &mut MobData) -> i32 {
        let kind = m.kind;
        by_group!(kind, experience_reward(m), 0)
    }

    fn dimensions(m: &MobData) -> Option<Dimensions> {
        by_group!(m.kind, dimensions(m), None)
    }

    fn size_scale(m: &MobData) -> Option<f32> {
        matches!(m.kind, Kind::Slime | Kind::MagmaCube | Kind::SulfurCube).then(|| {
            #[allow(clippy::cast_precision_loss)]
            let s = cubes::size(m) as f32 * if m.baby { 0.5 } else { 1.0 };
            s
        })
    }

    fn walk_target_value(kind: Kind, level: &Level, env: &Environment, pos: BlockPos) -> f64 {
        match kind {
            Kind::Slime | Kind::MagmaCube | Kind::SulfurCube | Kind::Phantom => 0.0,
            Kind::Giant => {
                // `getPathfindingCostFromLightLevels`.
                f64::from(crate::mob::light_magic_value(level, env, pos) - 0.5)
            }
            Kind::Silverfish => arthropods::silverfish_walk_value(level, env, pos),
            _ => crate::mob::kinds::base_walk_target_value(true, level, env, pos),
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
        by_group!(
            kind,
            check_spawn_rules(kind, level, env, pos, random, reason),
            false
        )
    }

    fn is_food(kind: Kind, item: u16) -> bool {
        kind == Kind::SulfurCube && cubes::sulfur_is_food(item)
    }

    fn init(m: &mut MobData) {
        let kind = m.kind;
        by_group!(kind, init(m), ());
    }

    fn register_goals(m: &mut MobData) {
        let kind = m.kind;
        by_group!(kind, register_goals(m), ());
    }

    fn finalize_spawn(m: &mut MobData, ctx: &mut SpawnContext<'_>) {
        let kind = m.kind;
        by_group!(kind, finalize_spawn(m, ctx), ());
    }

    fn before_tick(t: &mut Tick<'_, '_>) {
        by_group!(t.mob_ref().kind, before_tick(t), ());
    }

    fn after_tick(t: &mut Tick<'_, '_>) {
        by_group!(t.mob_ref().kind, after_tick(t), ());
    }

    fn ai_step(t: &mut Tick<'_, '_>) {
        by_group!(t.mob_ref().kind, ai_step(t), ());
    }

    fn custom_server_ai_step(t: &mut Tick<'_, '_>) {
        by_group!(t.mob_ref().kind, custom_server_ai_step(t), ());
    }

    fn move_control(t: &mut Tick<'_, '_>) -> bool {
        by_group!(t.mob_ref().kind, move_control(t), false)
    }

    fn look_control(t: &mut Tick<'_, '_>) -> bool {
        t.mob_ref().kind == Kind::Phantom
    }

    fn travel(t: &mut Tick<'_, '_>, input: Vec3) -> bool {
        by_group!(t.mob_ref().kind, travel(t, input), false)
    }

    fn jump_power_bonus(m: &MobData) -> f32 {
        if m.kind == Kind::MagmaCube {
            #[allow(clippy::cast_precision_loss)]
            let bonus = cubes::size(m) as f32 * 0.1;
            bonus
        } else {
            0.0
        }
    }

    fn jump_in_liquid(t: &mut Tick<'_, '_>) -> bool {
        cubes::jump_in_liquid(t)
    }

    fn do_hurt_target(t: &mut Tick<'_, '_>, target: EntityId) -> Option<bool> {
        by_group!(t.mob_ref().kind, do_hurt_target(t, target), None)
    }

    fn perform_ranged_attack(t: &mut Tick<'_, '_>, target: EntityId, power: f32) {
        if t.mob_ref().kind == Kind::Drowned {
            zombies::drowned_throw_trident(t, target, power);
        } else if skeletons::is_skeleton(t.mob_ref().kind) {
            crate::mob::projectile::skeleton_shoot(t, target, power);
        }
    }

    fn hurt_filter(h: &mut Hurt<'_, '_>, amount: f32) -> Option<f32> {
        let Some(kind) = h.ents.mob(h.target).map(|m| m.kind) else {
            return Some(amount);
        };
        by_group!(kind, hurt_filter(h, amount), Some(amount))
    }

    fn after_hurt(h: &mut Hurt<'_, '_>, amount: f32) {
        let Some(kind) = h.ents.mob(h.target).map(|m| m.kind) else {
            return;
        };
        by_group!(kind, after_hurt(h, amount), ());
    }

    fn on_die(h: &mut Hurt<'_, '_>) {
        let Some(kind) = h.ents.mob(h.target).map(|m| m.kind) else {
            return;
        };
        by_group!(kind, on_die(h), ());
    }

    fn interact(ix: &mut Interact<'_, '_>) -> Interaction {
        let Some(kind) = ix.ents.mob(ix.target).map(|m| m.kind) else {
            return Interaction::Pass;
        };
        by_group!(kind, interact(ix), Interaction::Pass)
    }

    fn encode_data(w: &mut DataWriter<'_>, m: &MobData) {
        by_group!(m.kind, encode_data(w, m), ());
    }

    fn living_flags(m: &MobData) -> u8 {
        u8::from(m.using_item >= 0)
    }

    fn save(m: &MobData, out: &mut Compound) {
        by_group!(m.kind, save(m, out), ());
    }

    fn load(m: &mut MobData, tag: &Compound) {
        let kind = m.kind;
        by_group!(kind, load(m, tag), ());
    }

    fn pick_up_items(t: &mut Tick<'_, '_>) {
        by_group!(t.mob_ref().kind, pick_up_items(t), ());
    }

    fn missile_tick(t: &mut MissileTick<'_, '_>) {
        trident::tick(t);
    }

    fn missile_save(d: &MissileData, out: &mut Compound) {
        trident::save(d, out);
    }

    fn missile_load(d: &mut MissileData, tag: &Compound) {
        trident::load(d, tag);
    }

    fn missile_encode(w: &mut DataWriter<'_>, d: &MissileData) {
        trident::encode(w, d);
    }

    fn goal_flags(g: &Goal) -> u8 {
        by_group!(goal_kind(g), goal_flags(g), 0)
    }

    fn goal_every_tick(g: &Goal) -> bool {
        by_group!(goal_kind(g), goal_every_tick(g), false)
    }

    fn goal_can_use(t: &mut Tick<'_, '_>, g: &mut Goal) -> bool {
        let k = goal_kind(g);
        by_group!(k, goal_can_use(t, g), false)
    }

    fn goal_can_continue(t: &mut Tick<'_, '_>, g: &mut Goal) -> bool {
        let k = goal_kind(g);
        by_group!(k, goal_can_continue(t, g), false)
    }

    fn goal_start(t: &mut Tick<'_, '_>, g: &mut Goal) {
        let k = goal_kind(g);
        by_group!(k, goal_start(t, g), ());
    }

    fn goal_stop(t: &mut Tick<'_, '_>, g: &mut Goal) {
        let k = goal_kind(g);
        by_group!(k, goal_stop(t, g), ());
    }

    fn goal_tick(t: &mut Tick<'_, '_>, g: &mut Goal) {
        let k = goal_kind(g);
        by_group!(k, goal_tick(t, g), ());
    }
}

/// A representative kind of a goal's group (for the group dispatch).
fn goal_kind(g: &Goal) -> Kind {
    match goal_group(g) {
        Group::Zombie => Kind::Husk,
        Group::Skeleton => Kind::Stray,
        Group::Arthropod => Kind::CaveSpider,
        Group::Cube => Kind::Slime,
        Group::Phantom => Kind::Phantom,
    }
}

/// `DamageSource.getEntity()` is a living entity.
pub fn living_cause(h: &Hurt<'_, '_>, source: &DamageSource) -> Option<EntityId> {
    source
        .cause
        .filter(|&c| h.ents.mob(c).is_some() || h.ents.player(c).is_some())
}

/// Adds an effect to a living target (mob or player).
pub fn add_effect(
    ents: &mut crate::entities::Entities,
    target: EntityId,
    effect: crate::effect::EffectInstance,
) {
    crate::effect::living::add_detached(ents, target, effect);
}

/// The rotation of a freshly spawned mob looking along `y_rot`.
pub fn rotation(y_rot: f32) -> Rotation {
    Rotation {
        y_rot,
        x_rot: 0.0,
        y_head_rot: y_rot,
    }
}

/// `noCollision` and no liquid in the box of a type at `pos` (reinforcement and split
/// checks).
pub fn box_free(level: &Level, kind: Kind, pos: Vec3, liquids_ok: bool) -> bool {
    let aabb = Dimensions::of(kind.type_id()).bounding_box(pos);
    let mut scratch = Vec::new();
    crate::mob::physics::no_collision(&level.view(), &aabb, &mut scratch)
        && (liquids_ok || !crate::mob::physics::contains_any_liquid(level, &aabb))
}
