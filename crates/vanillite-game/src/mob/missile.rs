//! The projectiles besides arrows (`Projectile` subclasses): fireballs, shulker bullets,
//! evoker fangs, thrown potions, llama spit, snowballs, tridents and wind charges. They share
//! one entity store; each kind's flight and impact is implemented by the mob family that uses
//! it (`FamilyHooks::missile_tick`), so mobs and later players throw the same entities.

use vanillite_base::Vec3;
use vanillite_data::entity::{EntityTypeId, ids};
use vanillite_nbt::{Compound, Tag};
use vanillite_world::entity::{EntityId, EntityRef, NetworkIds, NewEntity, Rotation};

use super::fam::Family;
use super::{MobContext, Tick};
use crate::entities::Entities;
use crate::item::ItemStack;
use crate::level::Level;
use crate::survival::player::Env;

/// The projectile kinds of the store.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MissileKind {
    /// `SmallFireball` (blazes).
    SmallFireball,
    /// `LargeFireball` (ghasts).
    Fireball,
    ShulkerBullet,
    EvokerFangs,
    /// `ThrownSplashPotion` (witches).
    SplashPotion,
    /// `ThrownLingeringPotion`.
    LingeringPotion,
    LlamaSpit,
    Snowball,
    /// `ThrownTrident` (drowned).
    Trident,
    WindCharge,
    BreezeWindCharge,
}

impl MissileKind {
    pub const ALL: [MissileKind; 11] = [
        MissileKind::SmallFireball,
        MissileKind::Fireball,
        MissileKind::ShulkerBullet,
        MissileKind::EvokerFangs,
        MissileKind::SplashPotion,
        MissileKind::LingeringPotion,
        MissileKind::LlamaSpit,
        MissileKind::Snowball,
        MissileKind::Trident,
        MissileKind::WindCharge,
        MissileKind::BreezeWindCharge,
    ];

    /// The entity type.
    pub fn type_id(self) -> EntityTypeId {
        match self {
            MissileKind::SmallFireball => ids::SMALL_FIREBALL,
            MissileKind::Fireball => ids::FIREBALL,
            MissileKind::ShulkerBullet => ids::SHULKER_BULLET,
            MissileKind::EvokerFangs => ids::EVOKER_FANGS,
            MissileKind::SplashPotion => ids::SPLASH_POTION,
            MissileKind::LingeringPotion => ids::LINGERING_POTION,
            MissileKind::LlamaSpit => ids::LLAMA_SPIT,
            MissileKind::Snowball => ids::SNOWBALL,
            MissileKind::Trident => ids::TRIDENT,
            MissileKind::WindCharge => ids::WIND_CHARGE,
            MissileKind::BreezeWindCharge => ids::BREEZE_WIND_CHARGE,
        }
    }

    /// The kind of an entity type.
    pub fn of(t: EntityTypeId) -> Option<MissileKind> {
        MissileKind::ALL.into_iter().find(|k| k.type_id() == t)
    }

    /// The family that implements the projectile's flight and impact.
    pub fn family(self) -> Family {
        match self {
            MissileKind::SmallFireball
            | MissileKind::Fireball
            | MissileKind::ShulkerBullet
            | MissileKind::EvokerFangs
            | MissileKind::SplashPotion
            | MissileKind::LingeringPotion => Family::Illager,
            MissileKind::LlamaSpit => Family::Companion,
            MissileKind::Snowball => Family::Wildlife,
            MissileKind::Trident => Family::Undead,
            MissileKind::WindCharge | MissileKind::BreezeWindCharge => Family::Mount,
        }
    }
}

/// Whether entities of `t` keep their data in the missile store.
pub fn is_missile_kind(t: EntityTypeId) -> bool {
    MissileKind::of(t).is_some()
}

/// The data of a projectile. The fields beyond the common ones are used as each kind needs
/// them; the owning family documents its use.
#[derive(Clone, Debug)]
pub struct MissileData {
    pub kind: MissileKind,
    /// The entity that shot it (`Projectile.owner`), when loaded.
    pub owner: Option<EntityId>,
    /// The owner's UUID (kept when the owner is not loaded).
    pub owner_uuid: Option<u128>,
    /// `leftOwner`.
    pub left_owner: bool,
    /// Ticks lived (`tickCount` / `life`).
    pub life: i32,
    /// The item it shows or carries (snowball, potion, trident, fireball item).
    pub item: ItemStack,
    /// `AbstractHurtingProjectile.accelerationPower`.
    pub acceleration: f64,
    /// Explosion power of large fireballs (`ExplosionPower`).
    pub explosion_power: i32,
    /// The target of a shulker bullet or the attack target of fangs.
    pub target: Option<EntityId>,
    pub target_uuid: Option<u128>,
    /// Kind specific integers (bullet flight steps and direction, fang warmup, trident
    /// loyalty and return state).
    pub ints: [i32; 4],
    /// A kind specific vector (bullet target deltas).
    pub vec: Vec3,
    /// `inGround` of tridents.
    pub in_ground: bool,
    /// Removed during its tick; the world despawns it afterwards.
    pub removed: bool,
}

impl MissileData {
    /// A new projectile of `kind` with defaults.
    pub fn new(kind: MissileKind) -> MissileData {
        MissileData {
            kind,
            owner: None,
            owner_uuid: None,
            left_owner: false,
            life: 0,
            item: ItemStack::default(),
            acceleration: 0.1,
            explosion_power: 1,
            target: None,
            target_uuid: None,
            ints: [0; 4],
            vec: Vec3::ZERO,
            in_ground: false,
            removed: false,
        }
    }
}

/// Adds a projectile (`addFreshEntity`).
pub fn spawn(
    ents: &mut Entities,
    network_ids: &NetworkIds,
    uuid: u128,
    pos: Vec3,
    velocity: Vec3,
    rot: Rotation,
    data: MissileData,
) -> EntityId {
    let mut new = NewEntity::new(data.kind.type_id(), network_ids.next(), uuid, pos);
    new.velocity = velocity;
    new.rot = rot;
    ents.spawn_missile(new, data)
}

/// Spawns a projectile from a mob's tick with a random UUID.
pub fn spawn_from(
    t: &mut Tick<'_, '_>,
    pos: Vec3,
    velocity: Vec3,
    rot: Rotation,
    data: MissileData,
) -> EntityId {
    let uuid = crate::mob::new_entity_uuid();
    spawn_owned(t, uuid, pos, velocity, rot, data)
}

/// `ProjectileUtil.computeMargin`: how much the boxes of the entities a projectile may hit
/// grow, from 0 in its first two ticks to 0.3 after eight.
pub fn margin(life: i32) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let m = ((life - 2) as f32 / 20.0).clamp(0.0, 0.3);
    f64::from(m)
}

/// The own random of a projectile a mob is about to shoot (`Entity.random`, seeded like every
/// new entity): `Projectile.shoot` draws its spread from it. Spawn the projectile with
/// [`spawn_shot`] then.
pub fn new_projectile_random() -> vanillite_base::random::LegacyRandom {
    vanillite_base::random::LegacyRandom::new(crate::mob::new_entity_seed())
}

/// [`spawn_from`] for a projectile that took its seed in [`new_projectile_random`].
pub fn spawn_shot(
    t: &mut Tick<'_, '_>,
    pos: Vec3,
    velocity: Vec3,
    rot: Rotation,
    data: MissileData,
) -> EntityId {
    spawn_owned(t, crate::mob::new_uuid(), pos, velocity, rot, data)
}

fn spawn_owned(
    t: &mut Tick<'_, '_>,
    uuid: u128,
    pos: Vec3,
    velocity: Vec3,
    rot: Rotation,
    mut data: MissileData,
) -> EntityId {
    if data.owner.is_none() {
        data.owner = Some(t.id);
    }
    if data.owner_uuid.is_none() {
        data.owner_uuid = t.ents.store.get(t.id).map(|e| e.uuid());
    }
    spawn(
        t.ents,
        t.level.network_ids(),
        uuid,
        pos,
        velocity,
        rot,
        data,
    )
}

/// What one projectile tick works on.
pub struct MissileTick<'a, 'e> {
    pub level: &'a Level,
    pub ents: &'a mut Entities,
    pub env: &'a mut Env<'e>,
    pub cx: &'a mut MobContext,
    pub id: EntityId,
}

impl std::fmt::Debug for MissileTick<'_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MissileTick")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl MissileTick<'_, '_> {
    /// The ticking projectile's data.
    pub fn data(&mut self) -> Option<&mut MissileData> {
        self.ents.missile_mut(self.id)
    }

    /// The ticking projectile's core columns.
    pub fn core(&self) -> Option<EntityRef<'_>> {
        self.ents.store.get(self.id)
    }
}

/// One projectile tick (`tickNonPassenger`); removed projectiles are despawned.
pub fn tick(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    id: EntityId,
) {
    let Some(kind) = ents.missile(id).map(|d| d.kind) else {
        return;
    };
    {
        let mut t = MissileTick {
            level,
            ents,
            env,
            cx,
            id,
        };
        if let Some(d) = t.data() {
            d.life += 1;
        }
        super::fam::missile_tick(&mut t, kind);
    }
    if ents.missile(id).is_some_and(|d| d.removed) {
        cx.discarded.push(id);
    }
}

/// The owner as a UUID array.
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

/// A projectile as entity NBT: the entity, the owner, the item, then the kind's own data.
pub fn save(core: &EntityRef<'_>, d: &MissileData, owner: Option<u128>) -> Compound {
    let mut out = super::nbt::save_entity_base(core);
    if let Some(o) = owner.or(d.owner_uuid) {
        out.insert("Owner", uuid_ints(o));
    }
    out.insert("LeftOwner", d.left_owner);
    out.insert("HasBeenShot", true);
    if !d.item.is_empty() {
        out.insert("Item", crate::inventory::stack_to_nbt(&d.item));
    }
    super::fam::missile_save(d, &mut out);
    out
}

/// A projectile from entity NBT; `None` for other types.
pub fn load(tag: &Compound) -> Option<(super::nbt::LoadedEntity, MissileData)> {
    let kind = MissileKind::of(super::nbt::type_of(tag)?)?;
    let e = super::nbt::load_entity_base(tag)?;
    let mut d = MissileData::new(kind);
    d.owner_uuid = super::nbt::uuid_key_of(tag, "Owner");
    d.left_owner = matches!(tag.get("LeftOwner"), Some(Tag::Byte(b)) if *b != 0);
    if let Some(Tag::Compound(c)) = tag.get("Item") {
        d.item = crate::inventory::stack_from_nbt(c);
    }
    super::fam::missile_load(&mut d, tag);
    Some((e, d))
}
