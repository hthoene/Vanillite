//! Saving and loading mobs and experience orbs in Vanilla's entity NBT (`saveWithoutId` /
//! `load` with the keys of 26.3), as stored in the `entities/` region files.

use vanillite_base::Vec3;
use vanillite_base::random::RandomSource;
use vanillite_data::entity::{EntityTypeId, ids};
use vanillite_nbt::{Compound, List, Tag};
use vanillite_world::entity::{EntityFlags, EntityRef, Rotation};

use super::projectile::OrbData;
use super::{Kind, MobData};

fn doubles(v: Vec3) -> List {
    List(vec![Tag::Double(v.x), Tag::Double(v.y), Tag::Double(v.z)])
}

fn uuid_ints(uuid: u128) -> Vec<i32> {
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    let part = |shift: u32| (uuid >> shift) as u32 as i32;
    vec![part(96), part(64), part(32), part(0)]
}

/// The UUID of an entity compound (`UUID` as four ints).
pub fn uuid_of(tag: &Compound) -> Option<u128> {
    match tag.get("UUID") {
        Some(Tag::IntArray(v)) if v.len() == 4 => {
            #[allow(clippy::cast_sign_loss)]
            let part = |i: usize| u128::from(v[i] as u32);
            Some((part(0) << 96) | (part(1) << 64) | (part(2) << 32) | part(3))
        }
        _ => None,
    }
}

/// A UUID stored as four ints under `key`.
fn uuid_key(tag: &Compound, key: &str) -> Option<u128> {
    match tag.get(key) {
        Some(Tag::IntArray(v)) if v.len() == 4 => {
            #[allow(clippy::cast_sign_loss)]
            let part = |i: usize| u128::from(v[i] as u32);
            Some((part(0) << 96) | (part(1) << 64) | (part(2) << 32) | part(3))
        }
        _ => None,
    }
}

/// A text component of a `custom_name` component value (network NBT) as an NBT tag.
pub fn component_tag(encoded: &[u8]) -> Option<Tag> {
    let mut input = encoded;
    let mut budget = vanillite_nbt::read::Budget::new(1 << 16, 64);
    vanillite_nbt::read::read_network(&mut input, &mut budget)
        .ok()
        .flatten()
}

/// The `custom_name` component value (network NBT) of a text component NBT tag.
pub fn tag_component(tag: &Tag) -> Option<Box<[u8]>> {
    let mut out = Vec::new();
    vanillite_nbt::write::write_network(tag, &mut out).ok()?;
    Some(out.into())
}

fn vec3_of(tag: &Compound, key: &str) -> Option<Vec3> {
    match tag.get(key) {
        Some(Tag::List(l)) if l.0.len() == 3 => {
            let d = |i: usize| match l.0[i] {
                Tag::Double(v) if v.is_finite() => Some(v),
                Tag::Float(v) if v.is_finite() => Some(f64::from(v)),
                _ => None,
            };
            Some(Vec3::new(d(0)?, d(1)?, d(2)?))
        }
        _ => None,
    }
}

fn int(tag: &Compound, key: &str) -> Option<i32> {
    tag.get(key)
        .and_then(Tag::as_i64)
        .and_then(|v| i32::try_from(v).ok())
}

fn boolean(tag: &Compound, key: &str) -> bool {
    tag.get(key).and_then(Tag::as_i64).is_some_and(|v| v != 0)
}

fn float(tag: &Compound, key: &str) -> Option<f32> {
    match tag.get(key) {
        Some(Tag::Float(v)) if v.is_finite() => Some(*v),
        Some(Tag::Double(v)) if v.is_finite() => {
            #[allow(clippy::cast_possible_truncation)]
            let f = *v as f32;
            Some(f)
        }
        _ => None,
    }
}

/// The entity type of a compound (`id`).
pub fn type_of(tag: &Compound) -> Option<EntityTypeId> {
    tag.get("id")
        .and_then(Tag::as_str)
        .and_then(EntityTypeId::by_name)
}

/// The position of an entity compound.
pub fn pos_of(tag: &Compound) -> Option<Vec3> {
    vec3_of(tag, "Pos")
}

/// `Entity.saveWithoutId` plus the id: the common entity keys.
/// The `Entity` part of entity NBT in a new compound.
pub fn save_entity_base(core: &EntityRef<'_>) -> Compound {
    let mut out = Compound::new();
    save_entity(core, &mut out);
    out
}

/// The `Entity` part of entity NBT.
pub fn load_entity_base(tag: &Compound) -> Option<LoadedEntity> {
    load_entity(tag)
}

/// A UUID int array under `key`.
pub fn uuid_key_of(tag: &Compound, key: &str) -> Option<u128> {
    uuid_key(tag, key)
}

fn save_entity(core: &EntityRef<'_>, out: &mut Compound) {
    let name = core.kind().info().name;
    out.insert(
        "id",
        format!(
            "minecraft:{}",
            name.strip_prefix("minecraft:").unwrap_or(name)
        ),
    );
    out.insert("Pos", doubles(core.pos()));
    out.insert("Motion", doubles(core.velocity()));
    let rot = core.rot();
    out.insert(
        "Rotation",
        List(vec![Tag::Float(rot.y_rot), Tag::Float(rot.x_rot)]),
    );
    out.insert("OnGround", core.flags().contains(EntityFlags::ON_GROUND));
    out.insert("Invulnerable", false);
    out.insert("PortalCooldown", 0);
    out.insert("UUID", uuid_ints(core.uuid()));
    if core.flags().contains(EntityFlags::NO_GRAVITY) {
        out.insert("NoGravity", true);
    }
}

/// A mob as entity NBT.
pub fn save_mob(core: &EntityRef<'_>, m: &MobData) -> Compound {
    let mut out = Compound::new();
    save_entity(core, &mut out);
    out.insert("fall_distance", m.fall_distance);
    #[allow(clippy::cast_possible_truncation)]
    {
        out.insert("Fire", m.fire_ticks.clamp(-32768, 32767) as i16);
        out.insert("Air", m.air.clamp(-32768, 32767) as i16);
        out.insert("HurtTime", m.hurt_time as i16);
        out.insert("DeathTime", m.death_time as i16);
    }
    if m.silent {
        out.insert("Silent", true);
    }
    out.insert("Health", m.health);
    out.insert("AbsorptionAmount", 0.0_f32);
    out.insert("FallFlying", false);
    out.insert("CanPickUpLoot", m.can_pick_up_loot);
    out.insert("PersistenceRequired", m.persistent);
    out.insert("LeftHanded", m.left_handed);
    if m.has_restriction() {
        out.insert("home_radius", m.restrict_radius);
        let c = m.restrict_center;
        out.insert("home_pos", vec![c.x, c.y, c.z]);
    }
    super::effects::save(m, &mut out);
    if m.no_ai {
        out.insert("NoAI", true);
    }
    if let Some(name) = m.custom_name.as_deref()
        && let Some(tag) = component_tag(name)
    {
        out.insert("CustomName", tag);
    }
    {
        let mut equipment = m.equipment.clone();
        if m.saddled {
            equipment.set(
                crate::item::EquipmentSlot::Saddle,
                super::equipment::stack("minecraft:saddle"),
            );
        }
        super::equipment::save_equipment(&equipment, &mut out);
    }
    if let Some(owner) = m.owner {
        out.insert("Owner", uuid_ints(owner));
        out.insert("Sitting", m.order_to_sit);
    }
    if let Some(l) = m.pending_leash {
        match l {
            super::LeashRef::Entity(u) => {
                let mut c = Compound::new();
                c.insert("UUID", uuid_ints(u));
                out.insert("leash", c);
            }
            super::LeashRef::Knot(p) => out.insert("leash", vec![p.x, p.y, p.z]),
        }
    }
    match m.kind {
        Kind::Zombie => {
            out.insert("IsBaby", m.baby);
            out.insert("CanBreakDoors", m.can_break_doors);
            out.insert("InWaterTime", -1);
            out.insert("DrownedConversionTime", -1);
        }
        Kind::Creeper => {
            #[allow(clippy::cast_possible_truncation)]
            {
                out.insert("Fuse", m.max_swell as i16);
                out.insert("ExplosionRadius", m.explosion_radius as i8);
            }
            if m.powered {
                out.insert("powered", true);
            }
            out.insert("ignited", m.ignited);
        }
        Kind::Cow | Kind::Pig | Kind::Sheep | Kind::Chicken => {
            out.insert("Age", m.age);
            out.insert("ForcedAge", m.forced_age);
            out.insert("AgeLocked", false);
            out.insert("InLove", m.in_love);
            if let Some(cause) = m.love_cause {
                out.insert("LoveCause", uuid_ints(cause));
            }
            match m.kind {
                Kind::Sheep => {
                    #[allow(clippy::cast_possible_wrap)]
                    out.insert("Color", m.sheep_color as i8);
                    out.insert("Sheared", m.sheared);
                }
                _ => {
                    out.insert("variant", variant_name(m.variant));
                    if m.kind == Kind::Chicken {
                        out.insert("EggLayTime", m.egg_time);
                    }
                }
            }
        }
        Kind::Skeleton | Kind::Spider | Kind::Enderman => {}
        _ => super::fam::save(m, &mut out),
    }
    out
}

fn variant_name(v: i32) -> &'static str {
    match v {
        0 => "minecraft:cold",
        2 => "minecraft:warm",
        _ => "minecraft:temperate",
    }
}

/// A mob loaded from entity NBT: what its entity starts with and its kind data.
#[derive(Debug)]
pub struct LoadedMob {
    pub pos: Vec3,
    pub velocity: Vec3,
    pub rot: Rotation,
    pub uuid: u128,
    pub on_ground: bool,
    pub data: MobData,
}

/// `Entity.load` of a mob of a kind with behaviour; `None` for other types or broken data.
pub fn load_mob(tag: &Compound, seed: i64) -> Option<LoadedMob> {
    let kind = Kind::of(type_of(tag)?)?;
    let pos = vec3_of(tag, "Pos")?;
    let velocity = vec3_of(tag, "Motion").unwrap_or(Vec3::ZERO);
    let (y_rot, x_rot) = match tag.get("Rotation") {
        Some(Tag::List(l)) if l.0.len() == 2 => match (&l.0[0], &l.0[1]) {
            (Tag::Float(y), Tag::Float(x)) if y.is_finite() && x.is_finite() => (*y, *x),
            _ => (0.0, 0.0),
        },
        _ => (0.0, 0.0),
    };
    let uuid = uuid_of(tag).unwrap_or_else(|| {
        let mut r = vanillite_base::random::LegacyRandom::new(seed);
        super::random_uuid(&mut r)
    });
    let mut m = MobData::new(kind, seed);
    if let Some(h) = float(tag, "Health") {
        m.health = h.clamp(0.0, m.max_health());
    }
    m.fire_ticks = int(tag, "Fire").unwrap_or(-1);
    m.air = int(tag, "Air").unwrap_or(300);
    m.fall_distance = match tag.get("fall_distance") {
        Some(Tag::Double(d)) if d.is_finite() => *d,
        _ => f64::from(float(tag, "FallDistance").unwrap_or(0.0)),
    };
    m.hurt_time = int(tag, "HurtTime").unwrap_or(0);
    m.death_time = int(tag, "DeathTime").unwrap_or(0);
    m.persistent = boolean(tag, "PersistenceRequired");
    m.no_ai = boolean(tag, "NoAI");
    m.silent = boolean(tag, "Silent");
    m.y_body_rot = y_rot;
    m.custom_name = tag.get("CustomName").and_then(tag_component);
    super::equipment::load(&mut m, tag);
    {
        use crate::item::EquipmentSlot;
        let saddle = m.equipment.get(EquipmentSlot::Saddle);
        // The classic pig keeps its saddle in `saddled`; family mobs in the slot.
        m.saddled = m.kind == Kind::Pig
            && !saddle.is_empty()
            && crate::item::item_id(saddle).name() == "minecraft:saddle";
        if m.saddled {
            m.equipment
                .set(EquipmentSlot::Saddle, crate::item::ItemStack::default());
        }
    }
    m.can_pick_up_loot = boolean(tag, "CanPickUpLoot");
    m.left_handed = boolean(tag, "LeftHanded");
    if let Some(r) = int(tag, "home_radius")
        && r >= 0
        && let Some(Tag::IntArray(v)) = tag.get("home_pos")
        && v.len() == 3
    {
        m.restrict_radius = r;
        m.restrict_center = vanillite_base::BlockPos::new(v[0], v[1], v[2]);
    }
    super::effects::load(&mut m, tag);
    m.owner = uuid_key(tag, "Owner");
    m.tame = m.owner.is_some();
    m.order_to_sit = boolean(tag, "Sitting");
    m.in_sitting_pose = m.order_to_sit;
    m.pending_leash = match tag.get("leash") {
        Some(Tag::Compound(c)) => uuid_of(c).map(super::LeashRef::Entity),
        Some(Tag::IntArray(v)) if v.len() == 3 => Some(super::LeashRef::Knot(
            vanillite_base::BlockPos::new(v[0], v[1], v[2]),
        )),
        _ => None,
    };
    match kind {
        Kind::Zombie => {
            m.baby = boolean(tag, "IsBaby");
            super::movement::set_can_break_doors(&mut m, boolean(tag, "CanBreakDoors"));
        }
        Kind::Creeper => {
            m.max_swell = int(tag, "Fuse").unwrap_or(30);
            m.explosion_radius = int(tag, "ExplosionRadius").unwrap_or(3);
            m.powered = boolean(tag, "powered");
            m.ignited = boolean(tag, "ignited");
        }
        Kind::Cow | Kind::Pig | Kind::Sheep | Kind::Chicken => {
            m.age = int(tag, "Age").unwrap_or(0);
            m.baby = m.age < 0;
            m.in_love = int(tag, "InLove").unwrap_or(0);
            m.love_cause = uuid_key(tag, "LoveCause");
            m.forced_age = int(tag, "ForcedAge").unwrap_or(0);
            if kind == Kind::Sheep {
                m.sheep_color = u8::try_from(int(tag, "Color").unwrap_or(0) & 15).unwrap_or(0);
                m.sheared = boolean(tag, "Sheared");
            } else {
                m.variant = match tag.get("variant").and_then(Tag::as_str) {
                    Some("minecraft:cold" | "cold") => 0,
                    Some("minecraft:warm" | "warm") => 2,
                    _ => 1,
                };
                if kind == Kind::Chicken
                    && let Some(t) = int(tag, "EggLayTime")
                {
                    m.egg_time = t;
                }
            }
        }
        Kind::Skeleton | Kind::Spider | Kind::Enderman => {}
        _ => super::fam::load(&mut m, tag),
    }
    // A fresh random per loaded mob, like `Entity.random`.
    let _ = m.random.next_int();
    Some(LoadedMob {
        pos,
        velocity,
        rot: Rotation {
            y_rot,
            x_rot,
            y_head_rot: y_rot,
        },
        uuid,
        on_ground: boolean(tag, "OnGround"),
        data: m,
    })
}

/// An experience orb as entity NBT.
pub fn save_orb(core: &EntityRef<'_>, o: &OrbData) -> Compound {
    let mut out = Compound::new();
    save_entity(core, &mut out);
    out.insert("fall_distance", 0.0_f64);
    out.insert("Fire", -1_i16);
    out.insert("Air", 300_i16);
    out.insert("Health", 5_i16);
    #[allow(clippy::cast_possible_truncation)]
    out.insert("Age", o.age.min(32767) as i16);
    #[allow(clippy::cast_possible_truncation)]
    out.insert("Value", o.value.clamp(0, 32767) as i16);
    out.insert("Count", o.count);
    out
}

/// An experience orb from entity NBT.
pub fn load_orb(tag: &Compound) -> Option<(Vec3, Vec3, u128, OrbData)> {
    if type_of(tag)? != ids::EXPERIENCE_ORB {
        return None;
    }
    let pos = vec3_of(tag, "Pos")?;
    let velocity = vec3_of(tag, "Motion").unwrap_or(Vec3::ZERO);
    let uuid = uuid_of(tag)?;
    Some((
        pos,
        velocity,
        uuid,
        OrbData {
            value: int(tag, "Value").unwrap_or(1),
            count: int(tag, "Count").unwrap_or(1).max(1),
            age: int(tag, "Age").unwrap_or(0),
            tick_count: 0,
            following: None,
            random_seed: i64::from(int(tag, "Value").unwrap_or(1)) ^ (uuid as i64),
        },
    ))
}

/// The common part of an entity loaded from NBT.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoadedEntity {
    pub pos: Vec3,
    pub velocity: Vec3,
    pub rot: Rotation,
    pub uuid: u128,
    pub on_ground: bool,
}

fn load_entity(tag: &Compound) -> Option<LoadedEntity> {
    let pos = vec3_of(tag, "Pos")?;
    let (y_rot, x_rot) = match tag.get("Rotation") {
        Some(Tag::List(l)) if l.0.len() == 2 => match (&l.0[0], &l.0[1]) {
            (Tag::Float(y), Tag::Float(x)) if y.is_finite() && x.is_finite() => (*y, *x),
            _ => (0.0, 0.0),
        },
        _ => (0.0, 0.0),
    };
    Some(LoadedEntity {
        pos,
        velocity: vec3_of(tag, "Motion").unwrap_or(Vec3::ZERO),
        rot: Rotation {
            y_rot,
            x_rot,
            y_head_rot: y_rot,
        },
        uuid: uuid_of(tag)?,
        on_ground: boolean(tag, "OnGround"),
    })
}

/// An item entity as entity NBT (`ItemEntity.addAdditionalSaveData`).
pub fn save_item(core: &EntityRef<'_>, d: &crate::item_entity::ItemData) -> Compound {
    let mut out = Compound::new();
    save_entity(core, &mut out);
    out.insert("fall_distance", 0.0_f64);
    out.insert("Fire", -1_i16);
    out.insert("Air", 300_i16);
    out.insert("Health", d.health);
    out.insert("Age", d.age);
    out.insert("PickupDelay", d.pickup_delay);
    if let Some(t) = d.thrower {
        out.insert("Thrower", uuid_ints(t));
    }
    if let Some(t) = d.target {
        out.insert("Owner", uuid_ints(t));
    }
    out.insert("Item", crate::inventory::stack_to_nbt(&d.stack));
    out
}

/// An item entity from entity NBT; `None` without a valid item.
pub fn load_item(tag: &Compound) -> Option<(LoadedEntity, crate::item_entity::ItemData)> {
    if type_of(tag)? != ids::ITEM {
        return None;
    }
    let e = load_entity(tag)?;
    let stack = match tag.get("Item") {
        Some(Tag::Compound(c)) => crate::inventory::stack_from_nbt(c),
        _ => return None,
    };
    if stack.is_empty() {
        return None;
    }
    let short = |k: &str| int(tag, k).map_or(0, |v| i16::try_from(v).unwrap_or(0));
    let mut d =
        crate::item_entity::ItemData::new(stack, short("PickupDelay"), uuid_key(tag, "Thrower"));
    d.age = short("Age");
    d.health = int(tag, "Health").map_or(5, |v| i16::try_from(v).unwrap_or(5));
    d.target = uuid_key(tag, "Owner");
    Some((e, d))
}

/// An arrow as entity NBT (`AbstractArrow.addAdditionalSaveData`); `owner` is the owner's
/// UUID.
pub fn save_arrow(
    core: &EntityRef<'_>,
    a: &super::projectile::ArrowData,
    owner: Option<u128>,
) -> Compound {
    let mut out = Compound::new();
    save_entity(core, &mut out);
    out.insert("fall_distance", 0.0_f64);
    out.insert("Fire", if a.fire { 2000_i16 } else { -1_i16 });
    out.insert("Air", 300_i16);
    #[allow(clippy::cast_possible_truncation)]
    {
        out.insert("life", a.life.clamp(-32768, 32767) as i16);
        out.insert("shake", a.shake.clamp(0, 127) as i8);
    }
    out.insert("inGround", a.in_ground);
    #[allow(clippy::cast_possible_wrap)]
    out.insert("pickup", a.pickup as i8);
    out.insert("damage", a.base_damage);
    out.insert("crit", a.crit);
    out.insert("PierceLevel", 0_i8);
    out.insert("SoundEvent", "minecraft:entity.arrow.hit");
    if let Some(o) = owner {
        out.insert("Owner", uuid_ints(o));
    }
    out.insert("HasBeenShot", true);
    out.insert("LeftOwner", a.left_owner);
    let mut item = Compound::new();
    item.insert("id", "minecraft:arrow");
    item.insert("count", 1);
    out.insert("item", item);
    out
}

/// An arrow from entity NBT (its owner is not resolved).
pub fn load_arrow(tag: &Compound) -> Option<(LoadedEntity, super::projectile::ArrowData)> {
    if type_of(tag)? != ids::ARROW {
        return None;
    }
    let e = load_entity(tag)?;
    let damage = match tag.get("damage") {
        Some(Tag::Double(d)) if d.is_finite() => *d,
        _ => 2.0,
    };
    Some((
        e,
        super::projectile::ArrowData {
            owner: None,
            base_damage: damage,
            in_ground: boolean(tag, "inGround"),
            life: int(tag, "life").unwrap_or(0),
            shake: int(tag, "shake").unwrap_or(0),
            crit: boolean(tag, "crit"),
            left_owner: boolean(tag, "LeftOwner"),
            last_state: None,
            tick_count: 0,
            pickup: u8::try_from(int(tag, "pickup").unwrap_or(0).clamp(0, 2)).unwrap_or(0),
            random_seed: e.uuid as i64,
            knockback: 0.0,
            fire: int(tag, "Fire").is_some_and(|f| f > 0),
        },
    ))
}

/// A boat as entity NBT.
pub fn save_boat(
    core: &EntityRef<'_>,
    b: &crate::vehicle::BoatData,
    leash: Option<super::LeashRef>,
) -> Compound {
    let mut out = Compound::new();
    save_entity(core, &mut out);
    out.insert("fall_distance", b.fall_distance);
    out.insert("Fire", -1_i16);
    out.insert("Air", 300_i16);
    if let Some(name) = b.custom_name.as_deref()
        && let Some(tag) = component_tag(name)
    {
        out.insert("CustomName", tag);
    }
    match leash {
        Some(super::LeashRef::Entity(u)) => {
            let mut c = Compound::new();
            c.insert("UUID", uuid_ints(u));
            out.insert("leash", c);
        }
        Some(super::LeashRef::Knot(p)) => out.insert("leash", vec![p.x, p.y, p.z]),
        None => {}
    }
    out
}

/// A boat from entity NBT.
pub fn load_boat(tag: &Compound) -> Option<(LoadedEntity, crate::vehicle::BoatData)> {
    let kind = crate::vehicle::BoatKind::of(type_of(tag)?)?;
    let e = load_entity(tag)?;
    let mut b = crate::vehicle::BoatData::new(kind);
    b.custom_name = tag.get("CustomName").and_then(tag_component);
    Some((e, b))
}
