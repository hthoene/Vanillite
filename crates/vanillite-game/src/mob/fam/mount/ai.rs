//! The brains of the family (`CamelAi`, `HappyGhastAi`, `NautilusAi`, `ZombieNautilusAi`,
//! `AllayAi`, `CopperGolemAi`, `BreezeAi`, `CreakingAi`).

use vanillite_nbt::Compound;
use vanillite_world::entity::EntityId;

use crate::mob::{MobData, Tick};

pub fn camel_brain(_m: &mut MobData) {}
pub fn happy_ghast_brain(_m: &mut MobData) {}
pub fn nautilus_brain(_m: &mut MobData) {}
pub fn allay_brain(_m: &mut MobData) {}
pub fn copper_golem_brain(_m: &mut MobData) {}
pub fn breeze_brain(_m: &mut MobData) {}
pub fn creaking_brain(_m: &mut MobData) {}

/// `brain.tick` and the kind's `updateActivity`.
pub fn tick(_t: &mut Tick<'_, '_>) {}

pub fn nautilus_init_memories(_m: &mut MobData) {}
pub fn nautilus_set_anger(_m: &mut MobData, _uuid: u128, _now: i64) {}
pub fn save_brain(_m: &MobData, _out: &mut Compound) {}
pub fn load_brain(_m: &mut MobData, _tag: &Compound) {}
pub fn nearest_players(_t: &Tick<'_, '_>) -> Vec<EntityId> {
    Vec::new()
}
pub fn set_attack_target(_m: &mut MobData, _target: Option<EntityId>) {}
