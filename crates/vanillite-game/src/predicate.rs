//! Entity and damage source predicates (Vanilla `EntityPredicate`, `DamageSourcePredicate`):
//! parsed once from data pack JSON and tested against a small view of the world. Loot
//! conditions and enchantment requirements share them; unknown parts make a predicate fail
//! (they are reported by the parser).

use serde_json::Value;
use vanillite_data::entity::EntityTypeId;

use crate::survival::damage::{DamageKind, DamageTypes, tag};

/// A set of entity types: a tag (resolved against the loaded tags) or a list of types.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypeSet {
    Tag(Box<str>),
    Types(Box<[u16]>),
}

impl TypeSet {
    /// Parses `"#ns:tag"`, `"ns:type"` or a list of types.
    pub fn parse(v: &Value) -> Option<TypeSet> {
        match v {
            Value::String(s) => Some(match s.strip_prefix('#') {
                Some(t) => TypeSet::Tag(qualify(t).into()),
                None => TypeSet::Types(Box::new([type_id(s)?])),
            }),
            Value::Array(a) => Some(TypeSet::Types(
                a.iter()
                    .map(|x| x.as_str().and_then(type_id))
                    .collect::<Option<Vec<_>>>()?
                    .into(),
            )),
            _ => None,
        }
    }

    /// Whether `kind` is in the set.
    pub fn contains(&self, kind: EntityTypeId, tags: &dyn EntityTags) -> bool {
        match self {
            TypeSet::Tag(t) => tags.entity_in_tag(kind, t),
            TypeSet::Types(ids) => ids.contains(&kind.0),
        }
    }
}

/// Entity type tags as loaded from the data packs.
pub trait EntityTags {
    /// Whether an entity type is in a tag (`ns:path` without `#`).
    fn entity_in_tag(&self, kind: EntityTypeId, tag: &str) -> bool;
}

fn qualify(s: &str) -> String {
    if s.contains(':') {
        s.to_owned()
    } else {
        format!("minecraft:{s}")
    }
}

fn type_id(name: &str) -> Option<u16> {
    vanillite_data::registry::ENTITY_TYPE.id(&qualify(name))
}

/// The entity facts predicates test.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct EntityFacts {
    pub kind: Option<EntityTypeId>,
    pub on_fire: bool,
    pub baby: bool,
    pub sneaking: bool,
    pub sprinting: bool,
    pub swimming: bool,
    pub on_ground: bool,
    /// The main hand item has an enchantment of `#minecraft:smelts_loot` (Fire Aspect).
    pub smelts_loot: bool,
    /// A sheep's colour (`DyeColor` id) and whether it is sheared.
    pub sheep_color: Option<u8>,
    pub sheared: bool,
    /// A villager's or zombie villager's type (`minecraft:villager_type` id).
    pub villager_variant: Option<u16>,
    /// The climate variant of a chicken, cow or pig: 0 cold, 1 temperate, 2 warm.
    pub farm_variant: Option<u8>,
    /// `is_flying`: gliding or flying with the creative abilities.
    pub flying: bool,
    /// `movement.fall_distance`.
    pub fall_distance: f64,
    /// `location.can_see_sky` at the entity's position (known where the level is).
    pub can_see_sky: bool,
    /// A fishing hook and whether it fishes in open water.
    pub fishing_hook: Option<bool>,
}

/// `EntityPredicate`: the parts the game evaluates (`type`, `flags`); other parts are
/// unsupported and make it fail.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EntityPredicate {
    pub types: Option<TypeSet>,
    /// `flags`: `is_on_fire`, `is_baby`, `is_sneaking`, `is_sprinting`, `is_swimming`,
    /// `is_on_ground` (`None` = any).
    pub on_fire: Option<bool>,
    pub baby: Option<bool>,
    pub sneaking: Option<bool>,
    pub sprinting: Option<bool>,
    pub swimming: Option<bool>,
    pub on_ground: Option<bool>,
    /// `equipment.mainhand` with an enchantment of `#minecraft:smelts_loot`.
    pub smelts_loot: bool,
    /// `components` `minecraft:sheep/color`.
    pub sheep_color: Option<u8>,
    /// `type_specific/sheep` `sheared`.
    pub sheared: Option<bool>,
    /// `components` `minecraft:chicken/variant`, `minecraft:cow/variant` or
    /// `minecraft:pig/variant`: 0 cold, 1 temperate, 2 warm.
    pub farm_variant: Option<u8>,
    /// `predicates` `minecraft:villager/variant`: the villager types (`villager_type` ids).
    pub villager_variants: Option<Vec<u16>>,
    /// `flags.is_flying`.
    pub flying: Option<bool>,
    /// `movement.fall_distance.min`.
    pub min_fall_distance: Option<f64>,
    /// `location.can_see_sky`.
    pub can_see_sky: Option<bool>,
    /// `type_specific/fishing_hook`: a hook, with `in_open_water` if given.
    pub fishing_hook: Option<Option<bool>>,
    /// Has parts that are not evaluated.
    pub unsupported: bool,
}

impl EntityPredicate {
    /// Parses an entity predicate object. Both `type` and `minecraft:entity_type` (the
    /// component form of 26.3 enchantment requirements) name the types.
    pub fn parse(v: &Value) -> EntityPredicate {
        let mut p = EntityPredicate::default();
        let Some(o) = v.as_object() else {
            p.unsupported = !v.is_null();
            return p;
        };
        for (k, v) in o {
            match k.trim_start_matches("minecraft:") {
                "type" | "entity_type" => {
                    p.types = TypeSet::parse(v);
                    p.unsupported |= p.types.is_none();
                }
                "flags" => {
                    let flag = |name: &str| v.get(name).and_then(Value::as_bool);
                    p.on_fire = flag("is_on_fire");
                    p.baby = flag("is_baby");
                    p.sneaking = flag("is_sneaking");
                    p.sprinting = flag("is_sprinting");
                    p.swimming = flag("is_swimming");
                    p.on_ground = flag("is_on_ground");
                    p.flying = flag("is_flying");
                    let known = [
                        "is_on_fire",
                        "is_baby",
                        "is_sneaking",
                        "is_sprinting",
                        "is_swimming",
                        "is_on_ground",
                        "is_flying",
                    ];
                    p.unsupported |= v
                        .as_object()
                        .is_some_and(|f| f.keys().any(|k| !known.contains(&k.as_str())));
                }
                "components" => {
                    const FARM: [&str; 3] = [
                        "minecraft:chicken/variant",
                        "minecraft:cow/variant",
                        "minecraft:pig/variant",
                    ];
                    let known = v.as_object().is_some_and(|c| {
                        c.keys()
                            .all(|k| k == "minecraft:sheep/color" || FARM.contains(&k.as_str()))
                    });
                    p.sheep_color = v
                        .get("minecraft:sheep/color")
                        .and_then(Value::as_str)
                        .and_then(dye_id);
                    let farm = FARM.iter().find_map(|k| v.get(*k));
                    p.farm_variant = farm.and_then(Value::as_str).and_then(|name| {
                        match name.strip_prefix("minecraft:").unwrap_or(name) {
                            "cold" => Some(0),
                            "temperate" => Some(1),
                            "warm" => Some(2),
                            _ => None,
                        }
                    });
                    p.unsupported |= !known
                        || (v.get("minecraft:sheep/color").is_some() && p.sheep_color.is_none())
                        || (farm.is_some() && p.farm_variant.is_none());
                }
                "movement" => {
                    // Only a lower bound of `fall_distance` (Wind Burst).
                    let fall = v.get("fall_distance");
                    let only = v.as_object().is_some_and(|m| m.len() == 1)
                        && fall.is_some_and(|f| {
                            f.as_object().is_some_and(|f| f.keys().all(|k| k == "min"))
                        });
                    p.min_fall_distance = fall.and_then(|f| f.get("min")).and_then(Value::as_f64);
                    p.unsupported |= !only || p.min_fall_distance.is_none();
                }
                "location" => {
                    // Only `can_see_sky` (Channeling).
                    let only = v
                        .as_object()
                        .is_some_and(|l| l.keys().all(|k| k == "can_see_sky"));
                    p.can_see_sky = v.get("can_see_sky").and_then(Value::as_bool);
                    p.unsupported |= !only || p.can_see_sky.is_none();
                }
                "type_specific/fishing_hook" => {
                    p.fishing_hook = Some(v.get("in_open_water").and_then(Value::as_bool));
                }
                "type_specific/sheep" => {
                    p.sheared = v.get("sheared").and_then(Value::as_bool);
                }
                "predicates" => {
                    // Only the villager type of the villager trades.
                    let only = v
                        .as_object()
                        .is_some_and(|c| c.keys().all(|k| k == "minecraft:villager/variant"));
                    let variants = v.get("minecraft:villager/variant").and_then(villager_types);
                    p.unsupported |= !only || variants.is_none();
                    p.villager_variants = variants;
                }
                "equipment" => {
                    // Only the smelting check of the mob loot tables.
                    let tag = v
                        .pointer("/mainhand/predicates/minecraft:enchantments/0/enchantments")
                        .and_then(Value::as_str);
                    let only = v.as_object().is_some_and(|e| e.len() == 1);
                    if only && tag == Some("#minecraft:smelts_loot") {
                        p.smelts_loot = true;
                    } else {
                        p.unsupported = true;
                    }
                }
                _ => p.unsupported = true,
            }
        }
        p
    }

    /// Whether `entity` matches; a missing entity never does.
    pub fn test(&self, entity: Option<&EntityFacts>, tags: &dyn EntityTags) -> bool {
        let Some(e) = entity else {
            return false;
        };
        if self.unsupported {
            return false;
        }
        if let Some(t) = &self.types
            && !e.kind.is_some_and(|k| t.contains(k, tags))
        {
            return false;
        }
        if self.smelts_loot && !e.smelts_loot {
            return false;
        }
        if self.sheep_color.is_some() && self.sheep_color != e.sheep_color {
            return false;
        }
        if self.farm_variant.is_some() && self.farm_variant != e.farm_variant {
            return false;
        }
        if let Some(types) = &self.villager_variants
            && !e.villager_variant.is_some_and(|v| types.contains(&v))
        {
            return false;
        }
        if self
            .sheared
            .is_some_and(|s| e.sheep_color.is_none() || s != e.sheared)
        {
            return false;
        }
        let flag = |want: Option<bool>, have: bool| want.is_none_or(|w| w == have);
        flag(self.on_fire, e.on_fire)
            && flag(self.baby, e.baby)
            && flag(self.sneaking, e.sneaking)
            && flag(self.sprinting, e.sprinting)
            && flag(self.swimming, e.swimming)
            && flag(self.on_ground, e.on_ground)
            && flag(self.flying, e.flying)
            && flag(self.can_see_sky, e.can_see_sky)
            && self.min_fall_distance.is_none_or(|m| e.fall_distance >= m)
            && self.fishing_hook.is_none_or(|want| {
                e.fishing_hook
                    .is_some_and(|open| want.is_none_or(|w| w == open))
            })
    }
}

/// A `villager_type` holder set of ids (a single id or a list; tags are not supported).
fn villager_types(v: &Value) -> Option<Vec<u16>> {
    let one = |s: &str| vanillite_data::registry::VILLAGER_TYPE.id(&qualify(s));
    match v {
        Value::String(s) if !s.starts_with('#') => Some(vec![one(s)?]),
        Value::Array(list) => list.iter().map(|e| e.as_str().and_then(one)).collect(),
        _ => None,
    }
}

/// The damage source facts predicates test.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SourceFacts {
    pub kind: DamageKind,
    /// `isDirect`: the direct entity is the causing entity.
    pub is_direct: bool,
    pub direct: Option<EntityFacts>,
    pub cause: Option<EntityFacts>,
}

/// `DamageSourcePredicate`: `tags`, `is_direct`, `direct_entity`, `source_entity`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DamageSourcePredicate {
    /// Tag bits with their expected value.
    pub tags: Vec<(u32, bool)>,
    pub is_direct: Option<bool>,
    pub direct_entity: Option<EntityPredicate>,
    pub source_entity: Option<EntityPredicate>,
    pub unsupported: bool,
}

impl DamageSourcePredicate {
    /// Parses a damage source predicate object.
    pub fn parse(v: &Value) -> DamageSourcePredicate {
        let mut p = DamageSourcePredicate::default();
        let Some(o) = v.as_object() else {
            p.unsupported = true;
            return p;
        };
        for (k, v) in o {
            match k.as_str() {
                "tags" => {
                    for t in v.as_array().into_iter().flatten() {
                        let id = t
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .trim_start_matches('#');
                        let id = qualify(id);
                        let expected = t.get("expected").and_then(Value::as_bool).unwrap_or(true);
                        match tag::NAMES.iter().find(|(n, _)| *n == id) {
                            Some(&(_, bit)) => p.tags.push((bit, expected)),
                            None => p.unsupported = true,
                        }
                    }
                }
                "is_direct" => p.is_direct = v.as_bool(),
                "direct_entity" => p.direct_entity = Some(EntityPredicate::parse(v)),
                "source_entity" => p.source_entity = Some(EntityPredicate::parse(v)),
                _ => p.unsupported = true,
            }
        }
        p
    }

    /// Whether `source` matches.
    pub fn test(&self, source: &SourceFacts, types: &DamageTypes, tags: &dyn EntityTags) -> bool {
        if self.unsupported {
            return false;
        }
        self.tags
            .iter()
            .all(|&(bit, expected)| types.is(source.kind, bit) == expected)
            && self.is_direct.is_none_or(|d| d == source.is_direct)
            && self
                .direct_entity
                .as_ref()
                .is_none_or(|p| p.test(source.direct.as_ref(), tags))
            && self
                .source_entity
                .as_ref()
                .is_none_or(|p| p.test(source.cause.as_ref(), tags))
    }
}

/// The id of a dye colour name (`DyeColor`).
pub fn dye_id(name: &str) -> Option<u8> {
    const NAMES: [&str; 16] = [
        "white",
        "orange",
        "magenta",
        "light_blue",
        "yellow",
        "lime",
        "pink",
        "gray",
        "light_gray",
        "cyan",
        "purple",
        "blue",
        "brown",
        "green",
        "red",
        "black",
    ];
    NAMES
        .iter()
        .position(|n| *n == name)
        .and_then(|i| u8::try_from(i).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoTags;
    impl EntityTags for NoTags {
        fn entity_in_tag(&self, _: EntityTypeId, _: &str) -> bool {
            false
        }
    }

    #[test]
    fn sheep_loot_predicates() {
        let p = EntityPredicate::parse(&serde_json::json!({
            "minecraft:components": {"minecraft:sheep/color": "lime"},
            "minecraft:type_specific/sheep": {"sheared": false}
        }));
        assert!(!p.unsupported);
        let mut sheep = EntityFacts {
            sheep_color: Some(5),
            ..EntityFacts::default()
        };
        assert!(p.test(Some(&sheep), &NoTags));
        sheep.sheared = true;
        assert!(!p.test(Some(&sheep), &NoTags));
        sheep.sheared = false;
        sheep.sheep_color = Some(0);
        assert!(!p.test(Some(&sheep), &NoTags));
    }

    #[test]
    fn fire_aspect_smelts() {
        let p = EntityPredicate::parse(&serde_json::json!({"minecraft:equipment": {"mainhand": {
            "predicates": {"minecraft:enchantments": [{"enchantments": "#minecraft:smelts_loot"}]}}}}));
        assert!(!p.unsupported);
        let mut killer = EntityFacts::default();
        assert!(!p.test(Some(&killer), &NoTags));
        killer.smelts_loot = true;
        assert!(p.test(Some(&killer), &NoTags));
    }
}
