//! Item stacks and component values in saved data, in Vanilla's persistent form
//! (`ItemStack.CODEC`): `{id, count, components: {"minecraft:<type>": …, "!minecraft:<type>": {}}}`.
//!
//! Component values convert between their wire form (how the server keeps them) and their NBT
//! form with [`vanillite_protocol::components_nbt`] against the installed registries. A value
//! without a persistent form (Vanilla could not save it either, e.g. an inline painting
//! variant) is kept under [`LEGACY_KEY`] in the wire form so nothing is lost; that key is also
//! what earlier versions wrote for the whole patch, and is still read.

use vanillite_nbt::{Compound, Tag};
use vanillite_protocol::codec::{Reader, write_varint};
use vanillite_protocol::components_nbt as conv;
use vanillite_protocol::item::InstalledRegistries;

use crate::item::{ComponentPatch, ItemStack};

/// Key of component values kept in their wire form: the patch as written by the untrusted
/// (delimited) codec without count and item. Vanilla readers ignore it.
pub const LEGACY_KEY: &str = "vanillite:patch";

/// The persistent NBT form of one component value in its wire form; `None` for transient
/// types and values without a persistent form.
pub fn component_to_nbt(type_id: u16, wire: &[u8]) -> Option<Tag> {
    conv::to_nbt(type_id, wire, &InstalledRegistries).ok()
}

/// The registry name of a component type (`minecraft:item_name`).
pub fn component_type_name(type_id: u16) -> Option<&'static str> {
    conv::type_name(type_id)
}

/// The wire form of one component value from its persistent NBT form; `None` if the tag does
/// not fit the component's codec.
pub fn component_from_nbt(type_id: u16, tag: &Tag) -> Option<Box<[u8]>> {
    conv::from_nbt(type_id, tag, &InstalledRegistries).ok()
}

/// JSON as NBT for the component codecs (numbers as int, long or double; `null` as an empty
/// compound).
pub fn json_to_nbt(v: &serde_json::Value) -> Tag {
    use serde_json::Value;
    match v {
        Value::Null => Tag::Compound(Compound::new()),
        Value::Bool(b) => Tag::Byte(i8::from(*b)),
        Value::Number(n) => match n.as_i64() {
            Some(i) => i32::try_from(i).map_or(Tag::Long(i), Tag::Int),
            None => Tag::Double(n.as_f64().unwrap_or(0.0)),
        },
        Value::String(s) => Tag::String(s.clone()),
        Value::Array(a) => Tag::List(vanillite_nbt::List(a.iter().map(json_to_nbt).collect())),
        Value::Object(o) => {
            let mut c = Compound::new();
            for (k, v) in o {
                c.insert(k.clone(), json_to_nbt(v));
            }
            Tag::Compound(c)
        }
    }
}

/// The `items.json` line (the default components Vanilla's data generator writes, one item
/// per line) of each item by name, read on first use.
fn item_lines() -> &'static std::collections::HashMap<&'static str, &'static str> {
    static LINES: std::sync::OnceLock<std::collections::HashMap<&'static str, &'static str>> =
        std::sync::OnceLock::new();
    LINES.get_or_init(|| {
        vanillite_data::raw::ITEMS_JSON
            .lines()
            .filter_map(|line| {
                let rest = line.strip_prefix("{\"name\":\"")?;
                let name = &rest[..rest.find('"')?];
                Some((name, line.trim_end_matches(',')))
            })
            .collect()
    })
}

/// The wire value of an item type's default component, if it has one.
pub fn default_component(item: u16, type_id: u16) -> Option<Box<[u8]>> {
    let name = vanillite_data::registry::ITEM.name(item)?;
    let component = conv::type_name(type_id)?;
    let line = item_lines().get(name)?;
    let json: serde_json::Value = serde_json::from_str(line).ok()?;
    let value = json.get("components")?.get(component)?;
    component_from_nbt(type_id, &json_to_nbt(value))
}

/// `ItemStack.get(type)`: the wire value of the stack's patch, else the item's default
/// (`None` where the patch removes it or the item has none).
pub fn effective_component(stack: &ItemStack, type_id: u16) -> Option<std::borrow::Cow<'_, [u8]>> {
    if stack.is_empty() {
        return None;
    }
    if let Some(p) = stack.patch() {
        if let Some(v) = p.get(type_id) {
            return Some(std::borrow::Cow::Borrowed(v));
        }
        if p.is_removed(type_id) {
            return None;
        }
    }
    default_component(stack.item, type_id).map(|v| std::borrow::Cow::Owned(v.into_vec()))
}

/// `NbtUtils.compareNbt(expected, actual, true)`: every key of an expected compound matches,
/// every element of an expected list finds a match (an empty list matches only an empty
/// one), other tags are equal.
pub fn compare_nbt(expected: &Tag, actual: &Tag) -> bool {
    match (expected, actual) {
        (Tag::Compound(e), Tag::Compound(a)) => compare_compounds(e, a),
        (Tag::List(e), Tag::List(a)) => {
            if e.0.is_empty() {
                return a.0.is_empty();
            }
            e.0.iter().all(|x| a.0.iter().any(|y| compare_nbt(x, y)))
        }
        (e, a) => e == a,
    }
}

/// [`compare_nbt`] of two compounds.
pub fn compare_compounds(expected: &Compound, actual: &Compound) -> bool {
    expected
        .iter()
        .all(|(k, v)| actual.get(k).is_some_and(|a| compare_nbt(v, a)))
}

/// A component patch in its persistent form (`DataComponentPatch.CODEC`), and the part that
/// has no persistent form (transient types are dropped as in Vanilla).
pub fn patch_to_nbt(patch: &ComponentPatch) -> (Compound, ComponentPatch) {
    let mut components = Compound::new();
    let mut rest = ComponentPatch::default();
    for (type_id, value) in &patch.added {
        if conv::is_transient(*type_id) {
            continue;
        }
        let Some(name) = conv::type_name(*type_id) else {
            continue;
        };
        match component_to_nbt(*type_id, value) {
            Some(tag) => components.insert(name, tag),
            None => rest.set(*type_id, value.clone()),
        }
    }
    for type_id in &patch.removed {
        if conv::is_transient(*type_id) {
            continue;
        }
        if let Some(name) = conv::type_name(*type_id) {
            components.insert(format!("!{name}"), Compound::new());
        }
    }
    (components, rest)
}

/// Reads a persistent component patch; entries that do not convert are skipped.
pub fn patch_from_nbt(components: &Compound) -> ComponentPatch {
    let mut patch = ComponentPatch::default();
    for (key, value) in components.iter() {
        if let Some(name) = key.strip_prefix('!') {
            if let Some(type_id) = conv::type_by_name(name)
                && let Err(i) = patch.removed.binary_search(&type_id)
            {
                patch.unset(type_id);
                patch.removed.insert(i, type_id);
            }
        } else if let Some(type_id) = conv::type_by_name(key)
            && let Some(wire) = component_from_nbt(type_id, value)
        {
            patch.set(type_id, wire);
        }
    }
    patch
}

/// `ItemStack.CODEC` of a non-empty stack.
pub fn stack_to_nbt(stack: &ItemStack) -> Compound {
    let mut c = Compound::new();
    c.insert(
        "id",
        vanillite_data::registry::ITEM
            .name(stack.item)
            .unwrap_or("minecraft:air"),
    );
    c.insert("count", stack.count);
    if let Some(patch) = stack.patch() {
        let (components, rest) = patch_to_nbt(patch);
        if !components.is_empty() {
            c.insert("components", components);
        }
        if !rest.is_empty() {
            let mut bytes = Vec::new();
            ItemStack {
                item: stack.item,
                count: stack.count,
                patch: Some(Box::new(rest)),
            }
            .encode_untrusted(&mut bytes);
            // Drop the count and item id: only the patch is stored.
            let mut r = Reader::new(&bytes);
            if r.varint().is_ok() && r.varint().is_ok() {
                let patch: Vec<i8> = r
                    .take_rest()
                    .iter()
                    .map(|b| i8::from_ne_bytes([*b]))
                    .collect();
                c.insert(LEGACY_KEY, patch);
            }
        }
    }
    c
}

/// Reads what [`stack_to_nbt`] or Vanilla wrote (and the earlier wire-only form).
pub fn stack_from_nbt(c: &Compound) -> ItemStack {
    let Some(id) = c
        .get("id")
        .and_then(Tag::as_str)
        .and_then(|n| vanillite_data::registry::ITEM.id(n))
    else {
        return ItemStack::EMPTY;
    };
    let count = c
        .get("count")
        .and_then(Tag::as_i64)
        .and_then(|v| i32::try_from(v).ok())
        .unwrap_or(1)
        .clamp(1, 99);
    let mut stack = ItemStack::new(id, count);
    if let Some(Tag::Compound(components)) = c.get("components") {
        let patch = patch_from_nbt(components);
        if !patch.is_empty() {
            stack.patch = Some(Box::new(patch));
        }
    }
    if let Some(Tag::ByteArray(legacy)) = c.get(LEGACY_KEY) {
        let mut bytes = Vec::with_capacity(legacy.len().saturating_add(8));
        write_varint(&mut bytes, count);
        write_varint(&mut bytes, i32::from(id));
        bytes.extend(legacy.iter().map(|b| b.to_ne_bytes()[0]));
        let mut r = Reader::new(&bytes);
        if let Ok(old) = ItemStack::decode_untrusted(&mut r, &InstalledRegistries)
            && let Some(old) = old.patch
        {
            let patch = stack.patch_mut();
            for (type_id, value) in old.added {
                patch.set(type_id, value);
            }
            for type_id in old.removed {
                if let Err(i) = patch.removed.binary_search(&type_id) {
                    patch.unset(type_id);
                    patch.removed.insert(i, type_id);
                }
            }
        }
    }
    stack
}

#[cfg(test)]
mod tests {
    use super::*;
    use vanillite_protocol::components::ids;

    fn text(s: &str) -> Box<[u8]> {
        let mut out = Vec::new();
        vanillite_nbt::write::write_network(&Tag::String(s.to_owned()), &mut out).unwrap();
        out.into_boxed_slice()
    }

    #[test]
    fn stack_round_trips_in_vanilla_form() {
        let mut stack = ItemStack::new(1, 5);
        stack.patch_mut().set(ids::CUSTOM_NAME, text("Hello"));
        stack
            .patch_mut()
            .set(ids::DAMAGE, vec![3].into_boxed_slice());
        stack.patch_mut().removed.push(ids::MAX_STACK_SIZE);
        let nbt = stack_to_nbt(&stack);
        let components = nbt.get("components").and_then(Tag::as_compound).unwrap();
        assert_eq!(
            components.get("minecraft:custom_name"),
            Some(&Tag::String("Hello".into()))
        );
        assert_eq!(components.get("minecraft:damage"), Some(&Tag::Int(3)));
        assert!(components.get("!minecraft:max_stack_size").is_some());
        assert!(nbt.get(LEGACY_KEY).is_none());
        assert_eq!(stack_from_nbt(&nbt), stack);
    }

    #[test]
    fn unconvertible_values_stay_in_wire_form() {
        // An inline painting variant has no persistent form.
        let mut value = Vec::new();
        value.push(0); // inline
        value.extend([1, 1]); // width, height
        vanillite_protocol::codec::write_str(&mut value, "minecraft:kebab");
        value.extend([0, 0]); // no title, no author
        let t = conv::type_by_name("minecraft:painting/variant").unwrap();
        let mut stack = ItemStack::new(1, 1);
        stack.patch_mut().set(t, value.into_boxed_slice());
        stack
            .patch_mut()
            .set(ids::DAMAGE, vec![7].into_boxed_slice());
        let nbt = stack_to_nbt(&stack);
        assert!(nbt.get(LEGACY_KEY).is_some());
        assert_eq!(stack_from_nbt(&nbt), stack);
    }

    #[test]
    fn reads_the_earlier_wire_only_form() {
        let mut stack = ItemStack::new(1, 2);
        stack.patch_mut().set(ids::CUSTOM_NAME, text("Old"));
        let mut bytes = Vec::new();
        stack.encode_untrusted(&mut bytes);
        let mut r = Reader::new(&bytes);
        r.varint().unwrap();
        r.varint().unwrap();
        let patch: Vec<i8> = r.take_rest().iter().map(|b| *b as i8).collect();
        let mut c = Compound::new();
        c.insert("id", "minecraft:stone");
        c.insert("count", 2);
        c.insert(LEGACY_KEY, patch);
        assert_eq!(stack_from_nbt(&c), stack);
    }
}
