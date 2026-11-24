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

/// A special entity (see [`super::boss::special`]) as entity NBT; `None` when it is not
/// saved (`shouldBeSaved`).
pub fn save_special(
    core: &EntityRef<'_>,
    data: &super::boss::special::SpecialData,
) -> Option<Compound> {
    let mut out = Compound::new();
    save_entity(core, &mut out);
    super::boss::special::save(data, &mut out)?;
    Some(out)
}

/// A special entity loaded from entity NBT: type, position, motion, rotation, UUID and data.
pub fn load_special(
    tag: &Compound,
    seed: i64,
) -> Option<(
    EntityTypeId,
    Vec3,
    Vec3,
    Rotation,
    u128,
    super::boss::special::SpecialData,
)> {
    let kind = type_of(tag)?;
    let data = super::boss::special::load(kind, tag)?;
    let pos = vec3_of(tag, "Pos")?;
    let velocity = vec3_of(tag, "Motion").unwrap_or(Vec3::ZERO);
    let rot = match tag.get("Rotation") {
        Some(Tag::List(l)) if l.0.len() == 2 => match (&l.0[0], &l.0[1]) {
            (Tag::Float(y), Tag::Float(x)) if y.is_finite() && x.is_finite() => Rotation {
                y_rot: *y,
                x_rot: *x,
                y_head_rot: *y,
            },
            _ => Rotation::default(),
        },
        _ => Rotation::default(),
    };
    // Generated entities (the pillars' crystals) have no UUID yet.
    let uuid = uuid_of(tag).unwrap_or_else(|| {
        let mut r = vanillite_base::random::LegacyRandom::new(seed);
        super::random_uuid(&mut r)
    });
    Some((kind, pos, velocity, rot, uuid, data))
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
    if let Some(effects) = m.effects.save() {
        out.insert("active_effects", effects);
    }
    out.insert("AbsorptionAmount", 0.0_f32);
    out.insert("FallFlying", false);
    out.insert("CanPickUpLoot", false);
    out.insert("PersistenceRequired", m.persistent);
    out.insert("LeftHanded", false);
    if m.no_ai {
        out.insert("NoAI", true);
    }
    match m.kind {
        Kind::Zombie => {
            out.insert("IsBaby", m.baby);
            out.insert("CanBreakDoors", false);
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
        _ if super::boss::is_boss_kind(m.kind) => super::boss::save(m, &mut out),
        Kind::Cow | Kind::Pig | Kind::Sheep | Kind::Chicken => {
            out.insert("Age", m.age);
            out.insert("ForcedAge", 0);
            out.insert("AgeLocked", false);
            out.insert("InLove", m.in_love);
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
        _ => {}
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
    // Elder guardians are persistent from birth (`setPersistenceRequired`), also when
    // generated monuments store them without the key.
    m.persistent = boolean(tag, "PersistenceRequired")
        || kind == Kind::ElderGuardian && tag.get("PersistenceRequired").is_none();
    m.no_ai = boolean(tag, "NoAI");
    m.silent = boolean(tag, "Silent");
    m.y_body_rot = y_rot;
    match kind {
        Kind::Zombie => m.baby = boolean(tag, "IsBaby"),
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
        _ => super::boss::load(&mut m, tag),
    }
    m.effects = crate::effect::MobEffects::load(tag.get("active_effects"));
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
