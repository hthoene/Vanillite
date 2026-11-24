//! The non-living entities of the bosses: wither skulls, end crystals, dragon fireballs and
//! area effect clouds (the dragon's breath). Their data lives in [`Entities::specials`]; they
//! tick in the world's entity tick like mobs.

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
    AreaEffectCloud(Box<dragon::Cloud>),
}

/// Whether entities of `kind` carry [`SpecialData`].
pub fn is_special_kind(kind: EntityTypeId) -> bool {
    kind == ids::WITHER_SKULL
        || kind == ids::END_CRYSTAL
        || kind == ids::DRAGON_FIREBALL
        || kind == ids::AREA_EFFECT_CLOUD
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
        SpecialData::AreaEffectCloud(_) => dragon::tick_cloud,
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

/// The synched data of a special entity after the entity's own (ids from 8 on).
pub fn entity_data(out: &mut Vec<u8>, data: &SpecialData, all: bool) {
    let _ = (out, data, all);
}

/// The entity NBT of a special entity's kind data; `None` when the entity is not saved.
pub fn save(data: &SpecialData, out: &mut Compound) -> Option<()> {
    let _ = (data, out);
    Some(())
}

/// Reads the kind data of a special entity of `kind`; `None` for other kinds.
pub fn load(kind: EntityTypeId, tag: &Compound) -> Option<SpecialData> {
    let _ = tag;
    if kind == ids::WITHER_SKULL {
        Some(SpecialData::WitherSkull(Box::default()))
    } else if kind == ids::END_CRYSTAL {
        Some(SpecialData::EndCrystal(Box::default()))
    } else if kind == ids::DRAGON_FIREBALL {
        Some(SpecialData::DragonFireball(Box::default()))
    } else if kind == ids::AREA_EFFECT_CLOUD {
        Some(SpecialData::AreaEffectCloud(Box::default()))
    } else {
        None
    }
}
