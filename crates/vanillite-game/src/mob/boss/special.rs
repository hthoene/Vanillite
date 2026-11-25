//! The non-living entities of the bosses: wither skulls, end crystals and dragon fireballs
//! (the dragon's breath clouds are [`crate::effect::cloud`]s), and thrown potions
//! ([`crate::effect::thrown`]). Their data lives in
//! [`Entities::specials`]; they tick in the world's entity tick like mobs.

use vanillite_data::entity::{EntityTypeId, ids};
use vanillite_nbt::Compound;
use vanillite_world::entity::EntityId;

use super::{dragon, wither};
use crate::entities::Entities;
use crate::level::Level;
use crate::mob::MobContext;
use crate::survival::player::Env;

/// The kind data of a special entity.
#[derive(Clone, Debug, PartialEq)]
pub enum SpecialData {
    WitherSkull(Box<wither::Skull>),
    EndCrystal(Box<dragon::Crystal>),
    DragonFireball(Box<dragon::Fireball>),
    ThrownPotion(Box<crate::effect::thrown::ThrownPotion>),
}

/// Whether entities of `kind` carry [`SpecialData`].
pub fn is_special_kind(kind: EntityTypeId) -> bool {
    kind == ids::WITHER_SKULL
        || kind == ids::END_CRYSTAL
        || kind == ids::DRAGON_FIREBALL
        || crate::effect::thrown::is_thrown_potion(kind)
}

/// What one special entity tick works on (like [`crate::mob::Tick`]).
pub struct SpecialTick<'a, 'e> {
    pub level: &'a Level,
    pub ents: &'a mut Entities,
    pub env: &'a mut Env<'e>,
    pub cx: &'a mut MobContext,
    pub id: EntityId,
}

impl std::fmt::Debug for SpecialTick<'_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpecialTick")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl SpecialTick<'_, '_> {
    /// The ticking entity's data.
    pub fn data(&mut self) -> Option<&mut SpecialData> {
        self.ents.special_mut(self.id)
    }
}

/// One tick of a special entity; it is despawned by the caller when it queued itself in
/// [`MobContext::discarded`].
pub fn tick(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    id: EntityId,
) {
    let Some(data) = ents.special(id) else {
        return;
    };
    let f: fn(&mut SpecialTick<'_, '_>) = match data {
        SpecialData::WitherSkull(_) => wither::tick_skull,
        SpecialData::EndCrystal(_) => dragon::tick_crystal,
        SpecialData::DragonFireball(_) => dragon::tick_fireball,
        SpecialData::ThrownPotion(_) => crate::effect::thrown::tick,
    };
    let mut t = SpecialTick {
        level,
        ents,
        env,
        cx,
        id,
    };
    f(&mut t);
}

/// The owner a special entity's spawn packet names (`Projectile.getAddEntityPacket`).
pub fn spawn_owner(data: &SpecialData) -> Option<EntityId> {
    match data {
        SpecialData::ThrownPotion(p) => p.owner,
        _ => None,
    }
}

/// The synched data of a special entity after the entity's own (ids from 8 on).
pub fn entity_data(out: &mut Vec<u8>, data: &SpecialData, all: bool) {
    match data {
        SpecialData::EndCrystal(_) | SpecialData::DragonFireball(_) => {
            dragon::crystal::entity_data(out, data, all)
        }
        SpecialData::WitherSkull(s) => wither::skull_entity_data(out, s, all),
        SpecialData::ThrownPotion(p) => crate::effect::thrown::entity_data(out, p, all),
    }
}

/// `hurtServer` of a special entity (end crystals break; the others ignore hits).
pub fn hurt(
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    id: EntityId,
    source: &crate::survival::damage::DamageSource,
) -> bool {
    match ents.special(id) {
        Some(SpecialData::EndCrystal(_)) => {
            dragon::crystal::hurt_crystal(ents, env, cx, id, source)
        }
        _ => false,
    }
}

/// The entity NBT of a special entity's kind data; `None` when the entity is not saved.
pub fn save(data: &SpecialData, out: &mut Compound) -> Option<()> {
    match data {
        SpecialData::EndCrystal(_) | SpecialData::DragonFireball(_) => {
            dragon::crystal::save(data, out)
        }
        SpecialData::WitherSkull(s) => {
            wither::save_skull(s, out);
            Some(())
        }
        SpecialData::ThrownPotion(p) => {
            crate::effect::thrown::save(p, out);
            Some(())
        }
    }
}

/// Reads the kind data of a special entity of `kind`; `None` for other kinds.
pub fn load(kind: EntityTypeId, tag: &Compound) -> Option<SpecialData> {
    if kind == ids::WITHER_SKULL {
        Some(SpecialData::WitherSkull(Box::new(wither::load_skull(tag))))
    } else if crate::effect::thrown::is_thrown_potion(kind) {
        Some(SpecialData::ThrownPotion(Box::new(
            crate::effect::thrown::load(kind, tag),
        )))
    } else {
        dragon::crystal::load(kind, tag)
    }
}
