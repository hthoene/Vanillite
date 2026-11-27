//! Java entities as Bedrock actors: type identifiers and the entity data (metadata) that has a
//! Bedrock counterpart.

use rustc_hash::FxHashSet;
use vanillite_protocol::codec::Reader;
use vanillite_protocol::item::ItemStack;

/// Java entity names whose Bedrock identifier differs (all others keep their name when the
/// client knows it).
const RENAMED: &[(&str, &str)] = &[
    ("minecraft:experience_orb", "minecraft:xp_orb"),
    ("minecraft:zombified_piglin", "minecraft:zombie_pigman"),
    ("minecraft:end_crystal", "minecraft:ender_crystal"),
    ("minecraft:eye_of_ender", "minecraft:eye_of_ender_signal"),
    ("minecraft:firework_rocket", "minecraft:fireworks_rocket"),
    ("minecraft:experience_bottle", "minecraft:xp_bottle"),
    ("minecraft:fishing_bobber", "minecraft:fishing_hook"),
    ("minecraft:evoker_fangs", "minecraft:evocation_fang"),
    ("minecraft:evoker", "minecraft:evocation_illager"),
    ("minecraft:villager", "minecraft:villager_v2"),
    ("minecraft:zombie_villager", "minecraft:zombie_villager_v2"),
    ("minecraft:trident", "minecraft:thrown_trident"),
    ("minecraft:spectral_arrow", "minecraft:arrow"),
    ("minecraft:potion", "minecraft:splash_potion"),
    ("minecraft:snow_golem", "minecraft:snow_golem"),
    ("minecraft:mooshroom", "minecraft:mooshroom"),
    (
        "minecraft:command_block_minecart",
        "minecraft:command_block_minecart",
    ),
    ("minecraft:furnace_minecart", "minecraft:minecart"),
    ("minecraft:spawner_minecart", "minecraft:minecart"),
    ("minecraft:illusioner", "minecraft:evocation_illager"),
    ("minecraft:giant", "minecraft:zombie"),
    ("minecraft:wind_charge", "minecraft:wind_charge_projectile"),
    (
        "minecraft:breeze_wind_charge",
        "minecraft:breeze_wind_charge_projectile",
    ),
];

/// The Bedrock identifier of a Java entity type, if the client knows one.
pub fn bedrock_identifier(java: &str, known: &FxHashSet<String>) -> Option<String> {
    let name = bedrock_name(java)?;
    known.contains(&name).then_some(name)
}

/// The Bedrock identifier a Java entity type would have.
pub fn bedrock_name(java: &str) -> Option<String> {
    RENAMED
        .iter()
        .find(|(j, _)| *j == java)
        .map(|(_, b)| (*b).to_owned())
        .or_else(|| {
            let base = java.strip_prefix("minecraft:")?;
            if base.ends_with("_chest_boat") {
                Some("minecraft:chest_boat".to_owned())
            } else if base.ends_with("_boat") || base.ends_with("_raft") {
                Some("minecraft:boat".to_owned())
            } else {
                Some(java.to_owned())
            }
        })
}

/// Java's shared entity flags (data index 0).
pub mod shared {
    pub const ON_FIRE: u8 = 0x01;
    pub const CROUCHING: u8 = 0x02;
    pub const SPRINTING: u8 = 0x08;
    pub const SWIMMING: u8 = 0x10;
    pub const INVISIBLE: u8 = 0x20;
    pub const FALL_FLYING: u8 = 0x80;
}

/// The Java entity data values the frontend translates.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Data {
    pub flags: Option<u8>,
    pub custom_name: Option<Option<String>>,
    pub name_visible: Option<bool>,
    pub item: Option<ItemStack>,
    /// `AgeableMob.DATA_BABY_ID` (index 16, a boolean on mobs).
    pub baby: Option<bool>,
    /// Fuse of primed TNT (index 8, an int on TNT).
    pub fuse: Option<i32>,
    /// `LivingEntity.DATA_EFFECT_PARTICLES` (index 10) as one RGB colour (0: none).
    pub effect_color: Option<i32>,
    /// `Sheep.DATA_WOOL_ID` (index 18; 17 is `AgeableMob.AGE_LOCKED`): colour and the sheared bit.
    pub wool: Option<u8>,
    /// `VillagerData`: type, profession and level ids.
    pub villager: Option<[i32; 3]>,
    /// The item of an item frame (index 9).
    pub frame_item: Option<ItemStack>,
    /// The item rotation of an item frame (index 10, 0-7).
    pub frame_rotation: Option<i32>,
    /// The `painting_variant` id of a painting (index 9).
    pub painting: Option<i32>,
    /// The text of a text display (index 23), plain.
    pub text: Option<String>,
    /// The width and height of an interaction (indices 8 and 9).
    pub width: Option<f32>,
    pub height: Option<f32>,
    /// The client flags of an armor stand (index 15: small, arms, no base plate, marker).
    pub stand_flags: Option<u8>,
    /// `DATA_NO_GRAVITY` (index 5).
    pub no_gravity: Option<bool>,
}

/// Serializer ids (`EntityDataSerializers` registration order of 26.3) read here.
mod serializer {
    pub const BYTE: i32 = 0;
    pub const INT: i32 = 1;
    pub const LONG: i32 = 2;
    pub const FLOAT: i32 = 3;
    pub const STRING: i32 = 4;
    pub const COMPONENT: i32 = 5;
    pub const OPTIONAL_COMPONENT: i32 = 6;
    pub const ITEM_STACK: i32 = 7;
    pub const BOOLEAN: i32 = 8;
    pub const ROTATIONS: i32 = 9;
    pub const BLOCK_POS: i32 = 10;
    pub const OPTIONAL_BLOCK_POS: i32 = 11;
    pub const DIRECTION: i32 = 12;
    pub const OPTIONAL_LIVING_ENTITY_REFERENCE: i32 = 13;
    pub const BLOCK_STATE: i32 = 14;
    pub const OPTIONAL_BLOCK_STATE: i32 = 15;
    pub const PARTICLE: i32 = 16;
    pub const PARTICLES: i32 = 17;
    pub const VILLAGER_DATA: i32 = 18;
    pub const OPTIONAL_UNSIGNED_INT: i32 = 19;
    pub const POSE: i32 = 20;
    /// 21-32: registry holders of the mob variants (cat ... zombie nautilus), a VarInt id.
    pub const FIRST_VARIANT: i32 = 21;
    pub const LAST_VARIANT: i32 = 32;
    pub const OPTIONAL_GLOBAL_POS: i32 = 33;
    pub const PAINTING_VARIANT: i32 = 34;
    /// 35-38: sniffer, armadillo, copper golem and weathering copper states.
    pub const FIRST_STATE: i32 = 35;
    pub const LAST_STATE: i32 = 38;
    pub const VECTOR3: i32 = 39;
    pub const QUATERNION: i32 = 40;
    pub const HUMANOID_ARM: i32 = 42;
    pub const DYE_COLOR: i32 = 43;
}

/// Reads a particle list (`PARTICLES`) and returns the colour of its first
/// `minecraft:entity_effect` (0 when there is none).
fn effect_particles(r: &mut Reader<'_>) -> vanillite_protocol::codec::Result<i32> {
    use vanillite_protocol::codec::Decode;
    let n = r.varint()?;
    if !(0..=256).contains(&n) {
        return Err(vanillite_protocol::codec::Error::Invalid("particle count"));
    }
    let effect = vanillite_data::registry::PARTICLE_TYPE.id("minecraft:entity_effect");
    let mut color = 0;
    for _ in 0..n {
        let p = vanillite_protocol::packets::play::clientbound::Particle::decode(r)?;
        if color == 0
            && effect.is_some_and(|e| i32::from(e) == p.kind)
            && let Some(argb) = p.options.first_chunk::<4>()
        {
            color = i32::from_be_bytes(*argb) & 0x00ff_ffff;
        }
    }
    Ok(color)
}

/// Reads the values of a Java `set_entity_data` of an entity of type `kind` (Java name) up to
/// the first value of a serializer it does not know (the rest is skipped).
#[allow(clippy::too_many_lines)]
pub fn read_data(raw: &[u8], kind: &str) -> Data {
    use vanillite_protocol::codec::Decode;
    let is_item = kind == "minecraft:item";
    let is_tnt = kind == "minecraft:tnt";
    let is_sheep = kind == "minecraft:sheep";
    let is_frame = kind == "minecraft:item_frame" || kind == "minecraft:glow_item_frame";
    let is_text = kind == "minecraft:text_display";
    let is_interaction = kind == "minecraft:interaction";
    let is_stand = kind == "minecraft:armor_stand";
    let mut d = Data::default();
    let mut r = Reader::new(raw);
    for _ in 0..64 {
        let Ok(index) = r.u8() else {
            break;
        };
        if index == 0xff {
            break;
        }
        let Ok(serializer) = r.varint() else {
            break;
        };
        let ok = match serializer {
            serializer::BYTE => r.u8().map(|v| match index {
                0 => d.flags = Some(v),
                18 if is_sheep => d.wool = Some(v),
                15 if is_stand => d.stand_flags = Some(v),
                _ => {}
            }),
            serializer::INT => r.varint().map(|v| {
                if index == 8 && is_tnt {
                    d.fuse = Some(v);
                }
                if index == 10 && is_frame {
                    d.frame_rotation = Some(v);
                }
            }),
            serializer::LONG => r.varlong().map(|_| ()),
            serializer::FLOAT => r.f32().map(|v| match index {
                8 if is_interaction => d.width = Some(v),
                9 if is_interaction => d.height = Some(v),
                _ => {}
            }),
            serializer::STRING => r.string(32_767).map(|_| ()),
            serializer::COMPONENT => component(&mut r).map(|c| {
                if index == 23 && is_text {
                    d.text = Some(c.plain());
                }
            }),
            serializer::OPTIONAL_COMPONENT => match r.u8() {
                Ok(0) => {
                    if index == 2 {
                        d.custom_name = Some(None);
                    }
                    Ok(())
                }
                Ok(_) => component(&mut r).map(|c| {
                    if index == 2 {
                        d.custom_name = Some(Some(c.plain()));
                    }
                }),
                Err(e) => Err(e),
            },
            serializer::ITEM_STACK => ItemStack::decode_plain(&mut r).map(|s| {
                if index == 8 && is_item {
                    d.item = Some(s);
                } else if index == 9 && is_frame {
                    d.frame_item = Some(s);
                }
            }),
            serializer::BOOLEAN => r.u8().map(|v| match index {
                3 => d.name_visible = Some(v != 0),
                5 => d.no_gravity = Some(v != 0),
                16 if !is_item => d.baby = Some(v != 0),
                _ => {}
            }),
            serializer::ROTATIONS => r.take(12).map(|_| ()),
            serializer::BLOCK_POS => r.i64().map(|_| ()),
            serializer::OPTIONAL_BLOCK_POS => match r.u8() {
                Ok(0) => Ok(()),
                Ok(_) => r.i64().map(|_| ()),
                Err(e) => Err(e),
            },
            serializer::OPTIONAL_LIVING_ENTITY_REFERENCE => match r.u8() {
                Ok(0) => Ok(()),
                Ok(_) => r.take(16).map(|_| ()),
                Err(e) => Err(e),
            },
            serializer::PARTICLE => {
                vanillite_protocol::packets::play::clientbound::Particle::decode(&mut r).map(|_| ())
            }
            serializer::PARTICLES => effect_particles(&mut r).map(|c| {
                if index == 10 {
                    d.effect_color = Some(c);
                }
            }),
            serializer::VILLAGER_DATA => {
                (|| Ok([r.varint()?, r.varint()?, r.varint()?]))().map(|v| d.villager = Some(v))
            }
            serializer::OPTIONAL_GLOBAL_POS => match r.u8() {
                Ok(0) => Ok(()),
                Ok(_) => r.string(32_767).and_then(|_| r.i64()).map(|_| ()),
                Err(e) => Err(e),
            },
            // A holder: registry id + 1, 0 for an inline variant (not sent by the server).
            serializer::PAINTING_VARIANT => match r.varint() {
                Ok(0) => break,
                Ok(v) => {
                    d.painting = Some(v.wrapping_sub(1));
                    Ok(())
                }
                Err(e) => Err(e),
            },
            serializer::VECTOR3 => r.take(12).map(|_| ()),
            serializer::QUATERNION => r.take(16).map(|_| ()),
            serializer::DIRECTION
            | serializer::BLOCK_STATE
            | serializer::OPTIONAL_BLOCK_STATE
            | serializer::OPTIONAL_UNSIGNED_INT
            | serializer::POSE
            | serializer::FIRST_VARIANT..=serializer::LAST_VARIANT
            | serializer::FIRST_STATE..=serializer::LAST_STATE
            | serializer::HUMANOID_ARM
            | serializer::DYE_COLOR => r.varint().map(|_| ()),
            _ => break,
        };
        if ok.is_err() {
            break;
        }
    }
    d
}

/// Bedrock `VARIANT` (profession) and `MARK_VARIANT` (biome type) of Java villager data.
/// The Bedrock motif of a Java painting variant: the motifs before 1.21 in CamelCase, the
/// newer ones by their Java name.
pub fn painting_title(java: &str) -> String {
    const OLD: [&str; 30] = [
        "Kebab",
        "Aztec",
        "Alban",
        "Aztec2",
        "Bomb",
        "Plant",
        "Wasteland",
        "Wanderer",
        "Graham",
        "Pool",
        "Courbet",
        "Sunset",
        "Sea",
        "Creebet",
        "Match",
        "Bust",
        "Stage",
        "Void",
        "SkullAndRoses",
        "Wither",
        "Fighters",
        "Skeleton",
        "DonkeyKong",
        "Pointer",
        "Pigscene",
        "BurningSkull",
        "Earth",
        "Wind",
        "Water",
        "Fire",
    ];
    let path = java.strip_prefix("minecraft:").unwrap_or(java);
    let flat: String = path.chars().filter(|c| *c != '_').collect();
    OLD.iter()
        .find(|o| o.eq_ignore_ascii_case(&flat))
        .map_or_else(|| path.to_owned(), |o| (*o).to_owned())
}

pub fn villager_variants(data: [i32; 3]) -> (i32, i32) {
    use vanillite_data::registry::Registry;
    let name = |registry: &str, id: i32| {
        u16::try_from(id)
            .ok()
            .and_then(|i| Registry::by_name(registry)?.name(i))
            .map(|n| n.strip_prefix("minecraft:").unwrap_or(n))
    };
    const PROFESSIONS: &[&str] = &[
        "none",
        "farmer",
        "fisherman",
        "shepherd",
        "fletcher",
        "librarian",
        "cartographer",
        "cleric",
        "armorer",
        "weaponsmith",
        "toolsmith",
        "butcher",
        "leatherworker",
        "mason",
        "nitwit",
    ];
    const TYPES: &[&str] = &[
        "plains", "desert", "jungle", "savanna", "snow", "swamp", "taiga",
    ];
    let index = |list: &[&str], n: Option<&str>| {
        n.and_then(|n| list.iter().position(|p| *p == n))
            .and_then(|i| i32::try_from(i).ok())
            .unwrap_or(0)
    };
    (
        index(PROFESSIONS, name("minecraft:villager_profession", data[1])),
        index(TYPES, name("minecraft:villager_type", data[0])),
    )
}

/// Reads a network text component (NBT).
pub fn component(
    r: &mut Reader<'_>,
) -> vanillite_protocol::codec::Result<vanillite_protocol::text::Component> {
    <vanillite_protocol::text::Component as vanillite_protocol::codec::DecodeLimited>::decode_limited(
        r,
        1 << 18,
    )
}

/// Bedrock effect id of a Java mob effect name.
pub fn effect_id(java: &str) -> Option<i32> {
    let name = java.strip_prefix("minecraft:").unwrap_or(java);
    let i = EFFECT_NAMES.iter().position(|e| *e == name)?;
    i32::try_from(i).ok()?.checked_add(1)
}

/// Bedrock effect ids - 1 by Java effect name.
const EFFECT_NAMES: &[&str] = &[
    "speed",
    "slowness",
    "haste",
    "mining_fatigue",
    "strength",
    "instant_health",
    "instant_damage",
    "jump_boost",
    "nausea",
    "regeneration",
    "resistance",
    "fire_resistance",
    "water_breathing",
    "invisibility",
    "blindness",
    "night_vision",
    "hunger",
    "weakness",
    "poison",
    "wither",
    "health_boost",
    "absorption",
    "saturation",
    "levitation",
    "fatal_poison",
    "conduit_power",
    "slow_falling",
    "bad_omen",
    "hero_of_the_village",
    "darkness",
    "trial_omen",
    "wind_charged",
    "weaving",
    "oozing",
    "infested",
    "raid_omen",
];

/// The Java `mob_effect` registry id of a Bedrock effect id (0: none).
pub fn java_effect(bedrock: i32) -> Option<i32> {
    let i = usize::try_from(bedrock.checked_sub(1)?).ok()?;
    let java = EFFECT_NAMES.get(i)?;
    let id = vanillite_data::registry::Registry::by_name("minecraft:mob_effect")?
        .id(&format!("minecraft:{java}"))?;
    Some(i32::from(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapped_entities_exist_in_the_bedrock_data() {
        let known = crate::entity_types();
        for (java, bedrock) in RENAMED {
            assert!(
                known.contains(*bedrock) || *java == "minecraft:snow_golem",
                "{java} -> {bedrock} unknown"
            );
        }
        let r = vanillite_data::registry::Registry::by_name("minecraft:entity_type");
        let names: Vec<&str> = r
            .map(|r| {
                (0..r.len())
                    .filter_map(|i| r.name(u16::try_from(i).ok()?))
                    .collect()
            })
            .unwrap_or_default();
        let mapped = names
            .iter()
            .filter(|n| bedrock_identifier(n, &known).is_some())
            .count();
        // Nearly all Java entities have a Bedrock actor (displays, markers and the like do not).
        assert!(
            mapped * 10 >= names.len() * 8,
            "{mapped} of {}",
            names.len()
        );
        for n in [
            "minecraft:zombie",
            "minecraft:cow",
            "minecraft:tnt",
            "minecraft:falling_block",
            "minecraft:item",
            "minecraft:oak_boat",
        ] {
            assert!(bedrock_identifier(n, &known).is_some(), "{n}");
        }
        assert_eq!(effect_id("minecraft:speed"), Some(1));
        assert_eq!(effect_id("minecraft:raid_omen"), Some(36));
    }

    #[test]
    fn reads_flags_names_and_items() {
        let mut raw = vec![0u8, 0, shared::CROUCHING];
        raw.extend_from_slice(&[16, 8, 1]);
        raw.push(0xff);
        let d = read_data(&raw, "minecraft:zombie");
        assert_eq!(d.flags, Some(shared::CROUCHING));
        assert_eq!(d.baby, Some(true));
    }

    #[test]
    fn sheep_wool_is_index_18() {
        // Index 17 (AGE_LOCKED) must not be read as wool; index 18 carries colour and sheared bit.
        let raw = [17u8, 0, 1, 18, 0, 0x13, 0xff];
        let d = read_data(&raw, "minecraft:sheep");
        assert_eq!(d.wool, Some(0x13));
        let raw = [17u8, 0, 1, 0xff];
        assert_eq!(read_data(&raw, "minecraft:sheep").wool, None);
    }
}
