//! The decor family: armor stands and mannequins, living entities that are no `Mob` (no AI,
//! no despawning, no loot pickup). Their logic is in [`crate::decoration::stand`]; the mob
//! tick skips the `Mob` parts for this family (see `Family::Decor` in `living.rs`).

use vanillite_nbt::Compound;
use vanillite_protocol::packets::play::clientbound::SoundSource;
use vanillite_world::entity::Dimensions;

use super::{FamilyHooks, Hurt};
use crate::decoration::stand;
use crate::item::ItemStack;
use crate::mob::kinds::Kind;
use crate::mob::{MobData, Tick};
use crate::player::encode::DataWriter;

/// No goals.
#[derive(Clone, Copy, Debug)]
pub enum Goal {}

#[derive(Debug)]
pub struct Hooks;

impl FamilyHooks for Hooks {
    type Goal = Goal;

    fn is_monster(_kind: Kind) -> bool {
        false
    }
    fn allowed_in_peaceful(_kind: Kind) -> bool {
        true
    }
    fn remove_when_far_away(_kind: Kind) -> bool {
        false
    }
    fn requires_custom_persistence(_m: &MobData) -> bool {
        true
    }
    fn sound_source(_kind: Kind) -> SoundSource {
        SoundSource::Neutral
    }
    fn ambient_sound(_m: &MobData) -> Option<&'static str> {
        None
    }
    fn hurt_sound(m: &MobData) -> &'static str {
        if m.kind == Kind::ArmorStand {
            "minecraft:entity.armor_stand.hit"
        } else {
            "minecraft:entity.generic.hurt"
        }
    }
    fn death_sound(m: &MobData) -> &'static str {
        if m.kind == Kind::ArmorStand {
            "minecraft:entity.armor_stand.break"
        } else {
            "minecraft:entity.generic.death"
        }
    }
    fn experience_reward(_m: &mut MobData) -> i32 {
        0
    }
    fn dimensions(m: &MobData) -> Option<Dimensions> {
        stand::dimensions(m)
    }
    fn init(m: &mut MobData) {
        stand::state_mut(m);
    }
    fn thunder_hit(h: &mut Hurt<'_, '_>) -> bool {
        // `ArmorStand.thunderHit` does nothing; mannequins burn like other living entities.
        h.ents
            .mob(h.target)
            .is_some_and(|m| m.kind == Kind::ArmorStand)
    }
    fn is_immobile(m: &MobData) -> bool {
        m.kind == Kind::Mannequin && stand::state(m).is_some_and(|s| s.immovable)
    }
    fn encode_data(w: &mut DataWriter<'_>, m: &MobData) {
        stand::encode_data(w, m);
    }
    fn pick_up_items(_t: &mut Tick<'_, '_>) {}
    fn wants_to_pick_up(_t: &Tick<'_, '_>, _stack: &ItemStack) -> bool {
        false
    }
    fn living_flags(_m: &MobData) -> u8 {
        0
    }
    fn save(m: &MobData, out: &mut Compound) {
        stand::save(m, out);
    }
    fn load(m: &mut MobData, tag: &Compound) {
        stand::load(m, tag);
    }
}
