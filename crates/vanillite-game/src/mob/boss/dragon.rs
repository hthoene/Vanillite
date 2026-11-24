//! The ender dragon (`EnderDragon`) with its phases and the End fight (`EndDragonFight`).

use vanillite_nbt::Compound;
use vanillite_world::entity::EntityId;

use crate::entities::Entities;
use crate::level::Level;
use crate::mob::{MobContext, Tick};
use crate::survival::damage::DamageSource;
use crate::survival::player::Env;

/// The kind state.
#[derive(Clone, Debug, Default)]
pub struct State {}

pub fn before_tick(t: &mut Tick<'_, '_>) {
    let _ = t;
}

pub fn after_tick(t: &mut Tick<'_, '_>) {
    let _ = t;
}

pub fn ai_step(t: &mut Tick<'_, '_>) {
    let _ = t;
}

pub fn tick_death(t: &mut Tick<'_, '_>) -> bool {
    let _ = t;
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

pub fn on_death(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    target: EntityId,
) {
    let _ = (level, ents, env, cx, target);
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

/// The kind data of an `end_crystal` (`EndCrystal`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Crystal {}

/// The kind data of a `dragon_fireball` (`DragonFireball`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Fireball {}

/// The kind data of an `area_effect_cloud` (`AreaEffectCloud`, the dragon's breath).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cloud {}

/// `EndCrystal.tick`.
pub fn tick_crystal(t: &mut super::special::SpecialTick<'_, '_>) {
    let _ = t;
}

/// `DragonFireball.tick`.
pub fn tick_fireball(t: &mut super::special::SpecialTick<'_, '_>) {
    let _ = t;
}

/// `AreaEffectCloud.tick`.
pub fn tick_cloud(t: &mut super::special::SpecialTick<'_, '_>) {
    let _ = t;
}
