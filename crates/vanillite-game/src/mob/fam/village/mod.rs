//! Villagers and wandering traders (`AbstractVillager`, `Villager`, `WanderingTrader`).
//!
//! The family hooks dispatch by kind: [`villager::Hooks`] for villagers (brain AI, gossip,
//! professions, sleeping, breeding) and [`trader::Hooks`] for wandering traders (goals,
//! despawn timer). What both share as merchants (offers, the trading player, the unhappy
//! counter, the 8 slot inventory) is [`Merchant`]; trading itself is in [`crate::trading`].

pub mod ai;
pub mod merchant;
pub mod trader;
pub mod villager;

use vanillite_base::random::LegacyRandom;
use vanillite_base::{BlockPos, Vec3};
use vanillite_nbt::Compound;
use vanillite_protocol::packets::play::clientbound::SoundSource;
use vanillite_world::entity::{Dimensions, EntityId};

use super::{FamilyHooks, Hurt, Interact, SpawnContext, SpawnReason};
use crate::entities::Entities;
use crate::item::ItemStack;
use crate::level::Level;
use crate::mob::animal::Interaction;
use crate::mob::brain::behavior::Shape;
use crate::mob::brain::{MemoryStatus, MemoryType};
use crate::mob::path::MalusTable;
use crate::mob::{Environment, Kind, MobData, Tick};
use crate::player::encode::DataWriter;
use crate::survival::damage::DamageSource;
use crate::survival::player::Env;
use crate::trading::MerchantOffers;

/// The family's hooks.
#[derive(Debug)]
pub struct Hooks;

/// What villagers and wandering traders share (`AbstractVillager`).
#[derive(Clone, Debug, Default)]
pub struct Merchant {
    /// `offers`: `None` until first asked for (`getOffers` generates them).
    pub offers: Option<MerchantOffers>,
    /// `tradingPlayer`.
    pub trading_player: Option<EntityId>,
    /// `DATA_UNHAPPY_COUNTER`.
    pub unhappy_counter: i32,
    /// The `SimpleContainer(8)` inventory.
    pub inventory: [ItemStack; 8],
}

/// The kind state of the family's mobs (in [`super::Ext::Village`]).
#[derive(Clone, Debug, Default)]
pub struct State {
    pub merchant: Merchant,
    pub villager: villager::Villager,
    pub trader: trader::Trader,
}

/// The family's goals (wandering traders; villagers think with their brain).
#[derive(Clone, Copy, Debug)]
pub enum Goal {
    Trader(trader::Goal),
}

/// The family's brain behaviours (villagers).
pub use ai::Behavior;
/// The family's brain sensors (villagers).
pub use ai::Sensor;

/// The family state of a mob.
pub fn state(m: &MobData) -> &State {
    static NONE: std::sync::OnceLock<State> = std::sync::OnceLock::new();
    match &m.ext {
        super::Ext::Village(s) => s,
        _ => NONE.get_or_init(State::default),
    }
}

/// The family state of a mob, mutably (created on first use).
pub fn state_mut(m: &mut MobData) -> &mut State {
    if !matches!(m.ext, super::Ext::Village(_)) {
        m.ext = super::Ext::Village(Box::default());
    }
    match &mut m.ext {
        super::Ext::Village(s) => s,
        _ => unreachable!("the village state was just set"),
    }
}

/// Calls a hook of the kind's implementation.
macro_rules! by_kind {
    ($kind:expr, $f:ident($($arg:expr),*)) => {{
        let kind: Kind = $kind;
        match kind {
            Kind::WanderingTrader => <trader::Hooks as FamilyHooks>::$f($($arg),*),
            _ => <villager::Hooks as FamilyHooks>::$f($($arg),*),
        }
    }};
}

#[allow(clippy::too_many_arguments)]
impl FamilyHooks for Hooks {
    type Goal = Goal;

    fn is_monster(kind: Kind) -> bool {
        by_kind!(kind, is_monster(kind))
    }
    fn ageable(kind: Kind) -> bool {
        by_kind!(kind, ageable(kind))
    }
    fn ambient_sound_interval(kind: Kind) -> i32 {
        by_kind!(kind, ambient_sound_interval(kind))
    }
    fn burns_in_daylight(kind: Kind) -> bool {
        by_kind!(kind, burns_in_daylight(kind))
    }
    fn breathes_underwater(kind: Kind) -> bool {
        by_kind!(kind, breathes_underwater(kind))
    }
    fn sensitive_to_water(kind: Kind) -> bool {
        by_kind!(kind, sensitive_to_water(kind))
    }
    fn fall_damage_immune(kind: Kind) -> bool {
        by_kind!(kind, fall_damage_immune(kind))
    }
    fn allowed_in_peaceful(kind: Kind) -> bool {
        by_kind!(kind, allowed_in_peaceful(kind))
    }
    fn remove_when_far_away(kind: Kind) -> bool {
        by_kind!(kind, remove_when_far_away(kind))
    }
    fn remove_when_far_away_mob(m: &MobData, dist_sqr: f64) -> bool {
        by_kind!(m.kind, remove_when_far_away_mob(m, dist_sqr))
    }
    fn requires_custom_persistence(m: &MobData) -> bool {
        by_kind!(m.kind, requires_custom_persistence(m))
    }
    fn malus(kind: Kind) -> &'static MalusTable {
        by_kind!(kind, malus(kind))
    }
    fn sound_source(kind: Kind) -> SoundSource {
        by_kind!(kind, sound_source(kind))
    }
    fn sound_volume(kind: Kind) -> f32 {
        by_kind!(kind, sound_volume(kind))
    }
    fn mob_sound_volume(m: &MobData) -> f32 {
        by_kind!(m.kind, mob_sound_volume(m))
    }
    fn jump_power_bonus(m: &MobData) -> f32 {
        by_kind!(m.kind, jump_power_bonus(m))
    }
    fn jump_in_liquid(t: &mut Tick<'_, '_>) -> bool {
        by_kind!(t.mob_ref().kind, jump_in_liquid(t))
    }
    fn ambient_sound(m: &MobData) -> Option<&'static str> {
        by_kind!(m.kind, ambient_sound(m))
    }
    fn hurt_sound(m: &MobData) -> &'static str {
        by_kind!(m.kind, hurt_sound(m))
    }
    fn death_sound(m: &MobData) -> &'static str {
        by_kind!(m.kind, death_sound(m))
    }
    fn experience_reward(m: &mut MobData) -> i32 {
        by_kind!(m.kind, experience_reward(m))
    }
    fn dimensions(m: &MobData) -> Option<Dimensions> {
        by_kind!(m.kind, dimensions(m))
    }
    fn walk_target_value(kind: Kind, level: &Level, env: &Environment, pos: BlockPos) -> f64 {
        by_kind!(kind, walk_target_value(kind, level, env, pos))
    }
    fn check_spawn_rules(
        kind: Kind,
        level: &Level,
        env: &Environment,
        pos: BlockPos,
        random: &mut LegacyRandom,
        reason: SpawnReason,
    ) -> bool {
        by_kind!(
            kind,
            check_spawn_rules(kind, level, env, pos, random, reason)
        )
    }
    fn anger_range(kind: Kind) -> (i32, i32) {
        by_kind!(kind, anger_range(kind))
    }
    fn is_max_group_size_reached(m: &MobData, group_size: i32) -> bool {
        by_kind!(m.kind, is_max_group_size_reached(m, group_size))
    }
    fn max_passengers(m: &MobData) -> usize {
        by_kind!(m.kind, max_passengers(m))
    }
    fn rider_controls(ents: &Entities, player: EntityId, vehicle: EntityId) -> bool {
        by_kind!(
            ents.mob(vehicle).map_or(Kind::Villager, |m| m.kind),
            rider_controls(ents, player, vehicle)
        )
    }
    fn rider_jump(ents: &mut Entities, env: &mut Env<'_>, vehicle: EntityId, strength: i32) {
        by_kind!(
            ents.mob(vehicle).map_or(Kind::Villager, |m| m.kind),
            rider_jump(ents, env, vehicle, strength)
        )
    }
    fn is_food(kind: Kind, item: u16) -> bool {
        by_kind!(kind, is_food(kind, item))
    }
    fn init(m: &mut MobData) {
        by_kind!(m.kind, init(m))
    }
    fn register_goals(m: &mut MobData) {
        by_kind!(m.kind, register_goals(m))
    }
    fn finalize_spawn(m: &mut MobData, ctx: &mut SpawnContext<'_>) {
        by_kind!(m.kind, finalize_spawn(m, ctx))
    }
    fn before_tick(t: &mut Tick<'_, '_>) {
        by_kind!(t.mob_ref().kind, before_tick(t))
    }
    fn after_tick(t: &mut Tick<'_, '_>) {
        by_kind!(t.mob_ref().kind, after_tick(t))
    }
    fn ai_step_start(t: &mut Tick<'_, '_>) {
        by_kind!(t.mob_ref().kind, ai_step_start(t))
    }
    fn ai_step(t: &mut Tick<'_, '_>) {
        by_kind!(t.mob_ref().kind, ai_step(t))
    }
    fn custom_server_ai_step(t: &mut Tick<'_, '_>) {
        by_kind!(t.mob_ref().kind, custom_server_ai_step(t))
    }
    fn travel(t: &mut Tick<'_, '_>, input: Vec3) -> bool {
        by_kind!(t.mob_ref().kind, travel(t, input))
    }
    fn pushed_by_fluid(m: &MobData) -> bool {
        by_kind!(m.kind, pushed_by_fluid(m))
    }
    fn water_slow_down(kind: Kind) -> f32 {
        by_kind!(kind, water_slow_down(kind))
    }
    fn move_control(t: &mut Tick<'_, '_>) -> bool {
        by_kind!(t.mob_ref().kind, move_control(t))
    }
    fn look_control(t: &mut Tick<'_, '_>) -> bool {
        by_kind!(t.mob_ref().kind, look_control(t))
    }
    fn after_base_tick(t: &mut Tick<'_, '_>) {
        by_kind!(t.mob_ref().kind, after_base_tick(t))
    }
    fn thunder_hit(h: &mut Hurt<'_, '_>) -> bool {
        by_kind!(
            h.ents.mob(h.target).map_or(Kind::Villager, |m| m.kind),
            thunder_hit(h)
        )
    }
    fn is_immobile(m: &MobData) -> bool {
        by_kind!(m.kind, is_immobile(m))
    }
    fn on_fall_damage(t: &mut Tick<'_, '_>, distance: f64) {
        by_kind!(t.mob_ref().kind, on_fall_damage(t, distance))
    }
    fn do_hurt_target(t: &mut Tick<'_, '_>, target: EntityId) -> Option<bool> {
        by_kind!(t.mob_ref().kind, do_hurt_target(t, target))
    }
    fn after_attack(t: &mut Tick<'_, '_>, target: EntityId) {
        by_kind!(t.mob_ref().kind, after_attack(t, target))
    }
    fn perform_ranged_attack(t: &mut Tick<'_, '_>, target: EntityId, power: f32) {
        by_kind!(t.mob_ref().kind, perform_ranged_attack(t, target, power))
    }
    fn hurt_filter(h: &mut Hurt<'_, '_>, amount: f32) -> Option<f32> {
        by_kind!(
            h.ents.mob(h.target).map_or(Kind::Villager, |m| m.kind),
            hurt_filter(h, amount)
        )
    }
    fn absorb(m: &MobData, source: &DamageSource, env: &Env<'_>, damage: f32) -> f32 {
        by_kind!(m.kind, absorb(m, source, env, damage))
    }
    fn after_hurt(h: &mut Hurt<'_, '_>, amount: f32) {
        by_kind!(
            h.ents.mob(h.target).map_or(Kind::Villager, |m| m.kind),
            after_hurt(h, amount)
        )
    }
    fn on_die(h: &mut Hurt<'_, '_>) {
        by_kind!(
            h.ents.mob(h.target).map_or(Kind::Villager, |m| m.kind),
            on_die(h)
        )
    }
    fn interact(ix: &mut Interact<'_, '_>) -> Interaction {
        by_kind!(
            ix.ents.mob(ix.target).map_or(Kind::Villager, |m| m.kind),
            interact(ix)
        )
    }
    fn can_mate(me: &MobData, partner: &MobData) -> bool {
        by_kind!(me.kind, can_mate(me, partner))
    }
    fn offspring_kind(parent: &MobData, partner: &MobData) -> Kind {
        by_kind!(parent.kind, offspring_kind(parent, partner))
    }
    fn rider_controls_mob(m: &MobData) -> bool {
        by_kind!(m.kind, rider_controls_mob(m))
    }
    fn breed_offspring(
        parent: &MobData,
        partner: &MobData,
        child: &mut MobData,
        random: &mut LegacyRandom,
    ) {
        by_kind!(parent.kind, breed_offspring(parent, partner, child, random))
    }
    fn encode_data(w: &mut DataWriter<'_>, m: &MobData) {
        by_kind!(m.kind, encode_data(w, m))
    }
    fn pick_up_items(t: &mut Tick<'_, '_>) {
        by_kind!(t.mob_ref().kind, pick_up_items(t))
    }
    fn wants_to_pick_up(t: &Tick<'_, '_>, stack: &ItemStack) -> bool {
        by_kind!(t.mob_ref().kind, wants_to_pick_up(t, stack))
    }
    fn living_flags(m: &MobData) -> u8 {
        by_kind!(m.kind, living_flags(m))
    }
    fn save(m: &MobData, out: &mut Compound) {
        by_kind!(m.kind, save(m, out))
    }
    fn load(m: &mut MobData, tag: &Compound) {
        by_kind!(m.kind, load(m, tag))
    }

    fn goal_flags(g: &Goal) -> u8 {
        match g {
            Goal::Trader(g) => <trader::Hooks as FamilyHooks>::goal_flags(g),
        }
    }
    fn goal_every_tick(g: &Goal) -> bool {
        match g {
            Goal::Trader(g) => <trader::Hooks as FamilyHooks>::goal_every_tick(g),
        }
    }
    fn goal_can_use(t: &mut Tick<'_, '_>, g: &mut Goal) -> bool {
        match g {
            Goal::Trader(g) => <trader::Hooks as FamilyHooks>::goal_can_use(t, g),
        }
    }
    fn goal_can_continue(t: &mut Tick<'_, '_>, g: &mut Goal) -> bool {
        match g {
            Goal::Trader(g) => <trader::Hooks as FamilyHooks>::goal_can_continue(t, g),
        }
    }
    fn goal_start(t: &mut Tick<'_, '_>, g: &mut Goal) {
        match g {
            Goal::Trader(g) => <trader::Hooks as FamilyHooks>::goal_start(t, g),
        }
    }
    fn goal_stop(t: &mut Tick<'_, '_>, g: &mut Goal) {
        match g {
            Goal::Trader(g) => <trader::Hooks as FamilyHooks>::goal_stop(t, g),
        }
    }
    fn goal_tick(t: &mut Tick<'_, '_>, g: &mut Goal) {
        match g {
            Goal::Trader(g) => <trader::Hooks as FamilyHooks>::goal_tick(t, g),
        }
    }
}

// --- Brain dispatch (villagers) ------------------------------------------------------------

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
