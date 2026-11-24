//! The wither (`WitherBoss`): summoning, the charge-up, its heads and skulls, block breaking
//! and the boss bar.

use vanillite_nbt::Compound;
use vanillite_world::entity::EntityId;

use crate::entities::Entities;
use crate::level::Level;
use crate::mob::{MobContext, MobData, Tick};
use crate::survival::damage::DamageSource;
use crate::survival::player::Env;

/// The kind state.
#[derive(Clone, Debug, Default)]
pub struct State {}

pub fn register_goals(m: &mut MobData) {
    let _ = m;
}

pub fn before_tick(t: &mut Tick<'_, '_>) {
    let _ = t;
}

pub fn after_tick(t: &mut Tick<'_, '_>) {
    let _ = t;
}

pub fn ai_step(t: &mut Tick<'_, '_>) {
    let _ = t;
}

pub fn custom_server_ai_step(t: &mut Tick<'_, '_>) {
    let _ = t;
}

pub fn travel(t: &mut Tick<'_, '_>, input: vanillite_base::Vec3) -> bool {
    let _ = (t, input);
    false
}

pub fn filter_hurt(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    target: EntityId,
    source: &DamageSource,
    amount: f32,
) -> Option<f32> {
    let _ = (level, ents, env, cx, target, source);
    Some(amount)
}

pub fn after_hurt(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    target: EntityId,
    source: &DamageSource,
    took: bool,
) {
    let _ = (level, ents, env, cx, target, source, took);
}

pub fn on_death(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    target: EntityId,
) {
    let _ = (level, ents, env, cx, target);
}

pub fn death_loot(m: &mut MobData, killed_by_player: bool) -> Vec<crate::item::ItemStack> {
    let _ = (m, killed_by_player);
    Vec::new()
}

pub fn entity_data(out: &mut Vec<u8>, s: &State, all: bool) {
    let _ = (out, s, all);
}

pub fn save(s: &State, out: &mut Compound) {
    let _ = (s, out);
}

pub fn load(s: &mut State, tag: &Compound) {
    let _ = (s, tag);
}

/// The kind data of a `wither_skull` (`WitherSkull`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Skull {}

/// `WitherSkull.tick`.
pub fn tick_skull(t: &mut super::special::SpecialTick<'_, '_>) {
    let _ = t;
}
