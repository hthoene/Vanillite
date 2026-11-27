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
