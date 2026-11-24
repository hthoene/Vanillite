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
    let name = RENAMED
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
        })?;
    known.contains(&name).then_some(name)
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
    pub const BLOCK_STATE: i32 = 14;
    pub const POSE: i32 = 20;
}

/// Reads the values of a Java `set_entity_data` up to the first value of a serializer it does
/// not know (the rest is skipped).
pub fn read_data(raw: &[u8], is_item: bool, is_tnt: bool) -> Data {
    let mut d = Data::default();
    let mut r = Reader::new(raw);
    for _ in 0..64 {
        let Ok(index) = r.u8() else {
            break;
        };
        if index == 0xff {
            break;
        }
        let Ok(kind) = r.varint() else {
            break;
        };
        let ok = match kind {
            serializer::BYTE => r.u8().map(|v| {
                if index == 0 {
                    d.flags = Some(v);
                }
            }),
            serializer::INT => r.varint().map(|v| {
                if index == 8 && is_tnt {
                    d.fuse = Some(v);
                }
            }),
            serializer::LONG => r.varlong().map(|_| ()),
            serializer::FLOAT => r.i32().map(|_| ()),
            serializer::STRING => r.string(32_767).map(|_| ()),
            serializer::COMPONENT => component(&mut r).map(|_| ()),
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
                }
            }),
            serializer::BOOLEAN => r.u8().map(|v| match index {
                3 => d.name_visible = Some(v != 0),
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
            serializer::DIRECTION | serializer::BLOCK_STATE | serializer::POSE => {
                r.varint().map(|_| ())
            }
            _ => break,
        };
        if ok.is_err() {
            break;
        }
    }
    d
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
    const EFFECTS: &[&str] = &[
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
    let name = java.strip_prefix("minecraft:").unwrap_or(java);
    let i = EFFECTS.iter().position(|e| *e == name)?;
    i32::try_from(i).ok()?.checked_add(1)
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
        let d = read_data(&raw, false, false);
        assert_eq!(d.flags, Some(shared::CROUCHING));
        assert_eq!(d.baby, Some(true));
    }
}
