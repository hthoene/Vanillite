//! The parts of `Entity.load` / `saveWithoutId` that every kind has (flags, portal cooldown,
//! custom data), a mob's `DeathLootTable` and `attributes`, and how Vanilla reads numbers.
//!
//! Vanilla's `ValueInput` getters and the `Codec` number codecs take every numeric tag type
//! (`NumericTag`); a floating point value read as an integer is floored.

use vanillite_base::Vec3;
use vanillite_data::entity::ids;
use vanillite_nbt::{Compound, List, Tag};
use vanillite_world::entity::{EntityFlags, EntityId};

use super::{Kind, MobData};
use crate::effect::attribute::{Attribute, Modifier};
use crate::effect::{MobEffect, Operation};
use crate::entities::Entities;

/// `NumericTag.doubleValue()`; `None` for other tags.
pub fn number(tag: &Tag) -> Option<f64> {
    match *tag {
        Tag::Byte(v) => Some(f64::from(v)),
        Tag::Short(v) => Some(f64::from(v)),
        Tag::Int(v) => Some(f64::from(v)),
        #[allow(clippy::cast_precision_loss)]
        Tag::Long(v) => Some(v as f64),
        Tag::Float(v) => Some(f64::from(v)),
        Tag::Double(v) => Some(v),
        _ => None,
    }
}

/// `NumericTag.longValue()`: floating point values are floored.
pub fn long_value(tag: &Tag) -> Option<i64> {
    match tag {
        #[allow(clippy::cast_possible_truncation)]
        Tag::Float(_) | Tag::Double(_) => number(tag).map(|v| v.floor() as i64),
        _ => tag.as_i64(),
    }
}

/// `NumericTag.intValue()`: floating point values are floored (and saturate), a long keeps its
/// low bits.
pub fn int_value(tag: &Tag) -> Option<i32> {
    match tag {
        #[allow(clippy::cast_possible_truncation)]
        Tag::Float(_) | Tag::Double(_) => number(tag).map(|v| v.floor() as i32),
        #[allow(clippy::cast_possible_truncation)]
        _ => tag.as_i64().map(|v| v as i32),
    }
}

/// `NumericTag.byteValue() != 0` (`getBooleanOr`).
pub fn bool_value(tag: &Tag) -> Option<bool> {
    int_value(tag).map(|v| v & 0xFF != 0)
}

/// The integer under `key`.
pub fn get_int(tag: &Compound, key: &str) -> Option<i32> {
    tag.get(key).and_then(int_value)
}

/// The boolean under `key`.
pub fn get_bool(tag: &Compound, key: &str) -> Option<bool> {
    tag.get(key).and_then(bool_value)
}

/// The finite double under `key`.
pub fn get_double(tag: &Compound, key: &str) -> Option<f64> {
    tag.get(key).and_then(number).filter(|v| v.is_finite())
}

/// The finite float under `key`.
pub fn get_float(tag: &Compound, key: &str) -> Option<f32> {
    #[allow(clippy::cast_possible_truncation)]
    get_double(tag, key)
        .map(|v| v as f32)
        .filter(|v| v.is_finite())
}

/// `N` finite numbers: a list or array of any numeric type (`Codec.DOUBLE.listOf()`).
pub fn numbers<const N: usize>(tag: Option<&Tag>) -> Option<[f64; N]> {
    let values: Vec<f64> = match tag? {
        Tag::List(List(l)) => l.iter().map(number).collect::<Option<_>>()?,
        Tag::ByteArray(v) => v.iter().map(|&v| f64::from(v)).collect(),
        Tag::IntArray(v) => v.iter().map(|&v| f64::from(v)).collect(),
        #[allow(clippy::cast_precision_loss)]
        Tag::LongArray(v) => v.iter().map(|&v| v as f64).collect(),
        _ => return None,
    };
    let values: [f64; N] = values.try_into().ok()?;
    values.iter().all(|v| v.is_finite()).then_some(values)
}

/// A position or motion (`Vec3.CODEC`).
pub fn vec3(tag: Option<&Tag>) -> Option<Vec3> {
    numbers::<3>(tag).map(|[x, y, z]| Vec3::new(x, y, z))
}

/// `Rotation` as yaw and pitch (`Vec2.CODEC`).
pub fn rotation(tag: Option<&Tag>) -> Option<(f32, f32)> {
    #[allow(clippy::cast_possible_truncation)]
    numbers::<2>(tag).map(|[y, x]| (y as f32, x as f32))
}

/// `Entity.load`'s motion: a component beyond 10 becomes 0.
pub fn motion(tag: Option<&Tag>) -> Option<Vec3> {
    let clamp = |v: f64| if v.abs() > 10.0 { 0.0 } else { v };
    vec3(tag).map(|v| Vec3::new(clamp(v.x), clamp(v.y), clamp(v.z)))
}

/// The boolean `Entity` keys kept as entity flags.
const FLAG_KEYS: [(&str, EntityFlags); 5] = [
    ("Invulnerable", EntityFlags::INVULNERABLE),
    ("NoGravity", EntityFlags::NO_GRAVITY),
    ("Glowing", EntityFlags::GLOWING_TAG),
    ("CustomNameVisible", EntityFlags::NAME_VISIBLE),
    ("HasVisualFire", EntityFlags::VISUAL_FIRE),
];

/// The flag keys of `saveWithoutId` for an entity with `flags`: `Invulnerable` always, the
/// others when set.
pub fn save_flags(flags: EntityFlags, out: &mut Compound) {
    out.insert("Invulnerable", flags.contains(EntityFlags::INVULNERABLE));
    for (key, flag) in &FLAG_KEYS[1..] {
        if flags.contains(*flag) {
            out.insert(*key, true);
        }
    }
}

/// The keys [`load_base`] reads, copied out of an entity compound (loading consumes the
/// compound before the common keys are applied; the rest is not cloned).
pub fn base_keys(tag: &Compound) -> Compound {
    let mut out = Compound::new();
    let keys = FLAG_KEYS.iter().map(|(k, _)| *k).chain([
        "PortalCooldown",
        "data",
        "attributes",
        "DeathLootTable",
        "DeathLootTableSeed",
    ]);
    for key in keys {
        if let Some(v) = tag.get(key) {
            out.insert(key, v.clone());
        }
    }
    out
}

/// Whether the entity keeps its `Entity` state somewhere else: players (in their data) and
/// decorations (in their [`crate::decoration::Decor`]).
fn keeps_own_base(ents: &Entities, id: EntityId) -> bool {
    ents.decor(id).is_some() || ents.store.get(id).is_none_or(|e| e.kind() == ids::PLAYER)
}

/// The common `Entity` keys (and a mob's `DeathLootTable`) of `tag` on a live entity.
/// `complete`: `tag` is the entity's whole data (`/data merge`, `/summon`), so a missing key
/// resets its value like `Entity.load`; otherwise (loading a new entity) what the kind set up
/// when it was created stays.
pub fn load_base(ents: &mut Entities, id: EntityId, tag: &Compound, complete: bool) {
    if keeps_own_base(ents, id) {
        return;
    }
    let glowing_effect = ents
        .mob(id)
        .is_some_and(|m| m.effects.has(MobEffect::GLOWING));
    if let Some(mut e) = ents.store.get_mut(id) {
        // End crystals keep `Invulnerable` in their own data (the dragon fight changes it).
        let kind = e.as_ref().kind();
        let before = e.as_ref().flags();
        for (key, flag) in FLAG_KEYS {
            if flag == EntityFlags::INVULNERABLE && kind == ids::END_CRYSTAL {
                continue;
            }
            let value = get_bool(tag, key);
            if complete || value.is_some() {
                e.set_flags(flag, value.unwrap_or(false));
            }
        }
        let tagged = e.as_ref().flags().contains(EntityFlags::GLOWING_TAG);
        if complete || get_bool(tag, "Glowing").is_some() {
            e.set_flags(EntityFlags::GLOWING, tagged || glowing_effect);
        }
        if e.as_ref().flags() != before {
            e.mark_data_dirty();
        }
    }
    let cooldown = get_int(tag, "PortalCooldown");
    if complete || cooldown.is_some() {
        let cooldown = cooldown.unwrap_or(0);
        if cooldown > 0 {
            ents.portals.entry(id).or_default().cooldown = cooldown;
        } else if let Some(p) = ents.portals.get_mut(&id) {
            p.cooldown = 0;
        }
    }
    let mut extra = ents.extra(id).cloned().unwrap_or_default();
    match tag.get("data") {
        Some(Tag::Compound(c)) => extra.data = (!c.is_empty()).then(|| c.clone()),
        _ if complete => extra.data = None,
        _ => {}
    }
    if let Some(kind) = ents.mob(id).map(|m| m.kind) {
        extra.attributes.read(kind, tag.get("attributes"));
        match tag.get("DeathLootTable").and_then(Tag::as_str) {
            Some(table) => {
                extra.death_loot_table = Some(if table.contains(':') {
                    table.to_owned()
                } else {
                    format!("minecraft:{table}")
                });
            }
            None if complete => extra.death_loot_table = None,
            None => {}
        }
        let seed = tag.get("DeathLootTableSeed").and_then(long_value);
        if complete || seed.is_some() {
            extra.death_loot_seed = seed.unwrap_or(0);
        }
    }
    ents.set_extra(id, extra);
}

/// Adds what [`load_base`] keeps to the saved data of a live entity.
pub fn save_base(ents: &Entities, id: EntityId, out: &mut Compound) {
    if keeps_own_base(ents, id) {
        return;
    }
    let Some(e) = ents.store.get(id) else {
        return;
    };
    let flags = e.flags();
    for (key, flag) in FLAG_KEYS {
        if flags.contains(flag) {
            out.insert(key, true);
        } else if matches!(e.kind(), ids::ITEM | ids::FALLING_BLOCK | ids::TNT) {
            // Their saved data starts from the loaded one: a cleared flag goes.
            if flag == EntityFlags::INVULNERABLE {
                out.insert(key, false);
            } else {
                out.remove(key);
            }
        }
    }
    let cooldown = ents.portals.get(&id).map_or(0, |p| p.cooldown);
    out.insert("PortalCooldown", cooldown);
    let extra = ents.extra(id);
    match extra.and_then(|x| x.data.clone()) {
        Some(data) => out.insert("data", data),
        None => {
            out.remove("data");
        }
    }
    if ents.mob(id).is_some()
        && let Some(x) = extra
    {
        x.attributes.save(out);
        if let Some(table) = &x.death_loot_table {
            out.insert("DeathLootTable", table.clone());
        }
        if x.death_loot_seed != 0 {
            out.insert("DeathLootTableSeed", x.death_loot_seed);
        }
    }
}

/// `isInvulnerableToBase`'s `Invulnerable` part: damage that neither bypasses it nor comes
/// from a player in creative mode.
pub fn invulnerable_to(
    ents: &Entities,
    types: &crate::survival::damage::DamageTypes,
    id: EntityId,
    source: &crate::survival::damage::DamageSource,
) -> bool {
    is_invulnerable(ents, id)
        && !types.is(
            source.kind,
            crate::survival::damage::tag::BYPASSES_INVULNERABILITY,
        )
        && !source
            .cause
            .and_then(|c| ents.player(c))
            .is_some_and(|p| p.game_mode == vanillite_config::GameMode::Creative)
}

/// `Entity.isInvulnerable` by the `Invulnerable` tag.
pub fn is_invulnerable(ents: &Entities, id: EntityId) -> bool {
    ents.store
        .get(id)
        .is_some_and(|e| e.flags().contains(EntityFlags::INVULNERABLE))
}

/// The `attributes` a mob was given (by NBT or a base value change), which its save keeps.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NbtAttributes(pub Vec<NbtAttribute>);

/// One `AttributeInstance.Packed`: the base value and the permanent modifiers.
#[derive(Clone, Debug, PartialEq)]
pub struct NbtAttribute {
    pub attribute: Attribute,
    pub base: f64,
    pub modifiers: Vec<Modifier>,
}

impl NbtAttributes {
    /// The entry of `attribute`, added with `base` if there is none.
    pub fn entry(&mut self, attribute: Attribute, base: f64) -> &mut NbtAttribute {
        let i = match self.0.iter().position(|a| a.attribute == attribute) {
            Some(i) => i,
            None => {
                self.0.push(NbtAttribute {
                    attribute,
                    base,
                    modifiers: Vec::new(),
                });
                self.0.len() - 1
            }
        };
        &mut self.0[i]
    }
}

const OPERATIONS: [(&str, Operation); 3] = [
    ("add_value", Operation::AddValue),
    ("add_multiplied_base", Operation::AddMultipliedBase),
    ("add_multiplied_total", Operation::AddMultipliedTotal),
];

fn namespaced(id: &str) -> String {
    if id.contains(':') {
        id.to_owned()
    } else {
        format!("minecraft:{id}")
    }
}

fn attribute_of(kind: Kind, name: &str) -> Option<Attribute> {
    let id = vanillite_data::registry::ATTRIBUTE.id(&namespaced(name))?;
    // `AttributeMap.apply`: only the attributes the type has.
    kind.type_id()
        .info()
        .attributes
        .iter()
        .any(|&(a, _)| a == id)
        .then(|| u8::try_from(id).ok().map(Attribute))
        .flatten()
}

fn read_modifier(attribute: Attribute, tag: &Tag) -> Option<Modifier> {
    let Tag::Compound(c) = tag else {
        return None;
    };
    let operation = c.get("operation").and_then(Tag::as_str)?;
    Some(Modifier {
        attribute,
        id: namespaced(c.get("id").and_then(Tag::as_str)?).into(),
        amount: get_double(c, "amount")?,
        operation: OPERATIONS.iter().find(|(n, _)| *n == operation)?.1,
    })
}

/// The entries of the `attributes` list for the attributes a mob of `kind` has.
fn read_attributes(kind: Kind, tag: Option<&Tag>) -> Vec<NbtAttribute> {
    let Some(Tag::List(List(list))) = tag else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|t| {
            let Tag::Compound(c) = t else {
                return None;
            };
            let attribute = attribute_of(kind, c.get("id").and_then(Tag::as_str)?)?;
            let modifiers = match c.get("modifiers") {
                Some(Tag::List(List(l))) => l
                    .iter()
                    .filter_map(|t| read_modifier(attribute, t))
                    .collect(),
                _ => Vec::new(),
            };
            Some(NbtAttribute {
                attribute,
                base: get_double(c, "base").unwrap_or(0.0),
                modifiers,
            })
        })
        .collect()
}

/// `AttributeMap.apply` of the `attributes` list: base values replace, modifiers are added.
/// Attributes the game keeps no value of change nothing (they are only kept for the save).
pub fn load_attributes(m: &mut MobData, tag: Option<&Tag>) {
    for a in read_attributes(m.kind, tag) {
        let base = a.attribute.sanitize(a.base);
        crate::effect::living::set_mob_attribute_base(m, a.attribute, base);
        for x in &a.modifiers {
            m.effects
                .modifiers
                .set(a.attribute, &x.id, x.amount, x.operation);
        }
    }
    let dirty = m.effects.modifiers.take_dirty();
    if !dirty.is_empty() {
        crate::effect::living::apply_mob_modifiers(m, &dirty);
        // The next effect sync tells the trackers.
        for a in dirty {
            m.effects.modifiers.touch(a);
        }
    }
}

impl NbtAttributes {
    /// Keeps what the `attributes` list of a mob of `kind` says, for its save.
    pub fn read(&mut self, kind: Kind, tag: Option<&Tag>) {
        for a in read_attributes(kind, tag) {
            let entry = self.entry(a.attribute, a.base);
            entry.base = a.base;
            for x in a.modifiers {
                match entry.modifiers.iter_mut().find(|y| y.id == x.id) {
                    Some(y) => *y = x,
                    None => entry.modifiers.push(x),
                }
            }
        }
    }

    /// Adds the kept attributes to the `attributes` list of `out` (a kind may have written
    /// some of its base values already: those are the current ones).
    pub fn save(&self, out: &mut Compound) {
        if self.0.is_empty() {
            return;
        }
        let mut list = match out.remove("attributes") {
            Some(Tag::List(List(l))) => l,
            _ => Vec::new(),
        };
        for a in &self.0 {
            let name = a.attribute.name();
            let i = list.iter().position(
                |t| matches!(t, Tag::Compound(c) if c.get("id").and_then(Tag::as_str) == Some(name)),
            );
            let i = i.unwrap_or_else(|| {
                let mut c = Compound::new();
                c.insert("id", name);
                c.insert("base", a.base);
                list.push(Tag::Compound(c));
                list.len() - 1
            });
            if a.modifiers.is_empty() {
                continue;
            }
            let modifiers: Vec<Tag> = a.modifiers.iter().map(modifier_tag).collect();
            if let Some(Tag::Compound(c)) = list.get_mut(i) {
                c.insert("modifiers", List(modifiers));
            }
        }
        out.insert("attributes", List(list));
    }
}

fn modifier_tag(x: &Modifier) -> Tag {
    let mut c = Compound::new();
    c.insert("id", x.id.to_string());
    c.insert("amount", x.amount);
    if let Some((op, _)) = OPERATIONS.iter().find(|(_, o)| *o == x.operation) {
        c.insert("operation", *op);
    }
    Tag::Compound(c)
}
