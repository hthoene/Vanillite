//! The default components of every item (from the `items.json` Vanilla's data generator
//! writes) in their wire form, and Vanilla's component equality (`Object.equals` of the
//! values: order-insensitive maps and compounds).
//!
//! Defaults that refer to dynamic registries need the installed registry names: once they are
//! installed ([`crate::item::install_registries`]) each item's defaults are converted once and
//! kept; before that they are converted on every lookup (tests and data loading only).

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::OnceLock;

use vanillite_nbt::{Compound, Tag};

use crate::codec::Reader;
use crate::components::ids;
use crate::components_nbt as conv;
use crate::item::InstalledRegistries;

/// Sorted `(type id, wire value)` of one item.
type Defaults = Box<[(u16, Box<[u8]>)]>;

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

/// The `items.json` line of each item by name, indexed on first use.
fn item_lines() -> &'static HashMap<&'static str, &'static str> {
    static LINES: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    LINES.get_or_init(|| {
        vanillite_data::raw::ITEMS_JSON
            .lines()
            .filter_map(|line| {
                let rest = line.strip_prefix("{\"name\":\"")?;
                let (name, _) = rest.split_once('"')?;
                Some((name, line.trim_end_matches(',')))
            })
            .collect()
    })
}

/// Converts the default components of `item`.
fn compute(item: u16) -> Defaults {
    let Some(line) = vanillite_data::registry::ITEM
        .name(item)
        .and_then(|name| item_lines().get(name))
    else {
        return Defaults::default();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(line) else {
        return Defaults::default();
    };
    let Some(components) = json.get("components").and_then(|c| c.as_object()) else {
        return Defaults::default();
    };
    let mut out: Vec<(u16, Box<[u8]>)> = components
        .iter()
        .filter_map(|(name, value)| {
            let t = conv::type_by_name(name)?;
            let wire = conv::from_nbt(t, &json_to_nbt(value), &InstalledRegistries).ok()?;
            Some((t, wire))
        })
        .collect();
    out.sort_by_key(|(t, _)| *t);
    out.into_boxed_slice()
}

/// The defaults of `item`, kept once the registries are installed.
fn defaults(item: u16) -> Cow<'static, [(u16, Box<[u8]>)]> {
    static CACHE: OnceLock<Box<[OnceLock<Defaults>]>> = OnceLock::new();
    if !crate::item::registries_installed() {
        return Cow::Owned(compute(item).into_vec());
    }
    let cache = CACHE.get_or_init(|| {
        (0..vanillite_data::item::ITEMS.len())
            .map(|_| OnceLock::new())
            .collect()
    });
    match cache.get(usize::from(item)) {
        Some(slot) => Cow::Borrowed(&**slot.get_or_init(|| compute(item))),
        None => Cow::Owned(Vec::new()),
    }
}

/// The wire value of an item type's default component, if it has one.
pub fn default_component(item: u16, type_id: u16) -> Option<Cow<'static, [u8]>> {
    match defaults(item) {
        Cow::Borrowed(list) => list
            .binary_search_by_key(&type_id, |(t, _)| *t)
            .ok()
            .and_then(|i| list.get(i))
            .map(|(_, v)| Cow::Borrowed(&**v)),
        Cow::Owned(list) => list
            .into_iter()
            .find(|(t, _)| *t == type_id)
            .map(|(_, v)| Cow::Owned(v.into_vec())),
    }
}

/// Whether the item has a default component of the type.
pub fn has_default(item: u16, type_id: u16) -> bool {
    default_component(item, type_id).is_some()
}

/// `Objects.equals` of two values of component type `type_id`: equal bytes, or values that
/// differ only in the order of an unordered map (enchantment levels) or of compound keys.
pub fn values_equal(type_id: u16, a: &[u8], b: &[u8]) -> bool {
    if a == b {
        return true;
    }
    if type_id == ids::ENCHANTMENTS || type_id == ids::STORED_ENCHANTMENTS {
        return match (enchantment_levels(a), enchantment_levels(b)) {
            (Some(mut x), Some(mut y)) => {
                x.sort_unstable();
                y.sort_unstable();
                x == y
            }
            _ => false,
        };
    }
    if type_id == ids::CUSTOM_DATA || type_id == ids::BUCKET_ENTITY_DATA {
        return match (read_nbt(&mut Reader::new(a)), read_nbt(&mut Reader::new(b))) {
            (Some(x), Some(y)) => tags_equal(&x, &y),
            _ => false,
        };
    }
    if type_id == ids::ENTITY_DATA || type_id == ids::BLOCK_ENTITY_DATA {
        let (mut ra, mut rb) = (Reader::new(a), Reader::new(b));
        return match (ra.varint(), rb.varint()) {
            (Ok(x), Ok(y)) if x == y => match (read_nbt(&mut ra), read_nbt(&mut rb)) {
                (Some(x), Some(y)) => tags_equal(&x, &y),
                _ => false,
            },
            _ => false,
        };
    }
    false
}

/// The `(enchantment id, level)` pairs of an `enchantments` value.
fn enchantment_levels(data: &[u8]) -> Option<Vec<(i32, i32)>> {
    let mut r = Reader::new(data);
    let n = usize::try_from(r.varint().ok()?).ok()?;
    let mut out = Vec::with_capacity(n.min(256));
    for _ in 0..n {
        out.push((r.varint().ok()?, r.varint().ok()?));
    }
    (r.remaining() == 0).then_some(out)
}

/// One network NBT tag (component values come from the server's own stores or were validated).
fn read_nbt(r: &mut Reader<'_>) -> Option<Tag> {
    let mut rest = r.rest();
    let mut budget = vanillite_nbt::read::Budget::new(2 * 1024 * 1024, 512);
    let tag = vanillite_nbt::read::read_network(&mut rest, &mut budget).ok()??;
    let used = r.remaining().checked_sub(rest.len())?;
    r.take(used).ok()?;
    Some(tag)
}

/// `Tag.equals`: compounds compare as maps, everything else in order.
pub fn tags_equal(a: &Tag, b: &Tag) -> bool {
    match (a, b) {
        (Tag::Compound(x), Tag::Compound(y)) => compounds_equal(x, y),
        (Tag::List(x), Tag::List(y)) => {
            x.0.len() == y.0.len() && x.0.iter().zip(&y.0).all(|(a, b)| tags_equal(a, b))
        }
        _ => a == b,
    }
}

/// [`tags_equal`] of two compounds (the last entry of a duplicated name counts).
fn compounds_equal(a: &Compound, b: &Compound) -> bool {
    fn keys(c: &Compound) -> Vec<&str> {
        let mut k: Vec<&str> = c.iter().map(|(k, _)| k).collect();
        k.sort_unstable();
        k.dedup();
        k
    }
    let ka = keys(a);
    ka == keys(b)
        && ka.iter().all(|k| match (a.get(k), b.get(k)) {
            (Some(x), Some(y)) => tags_equal(x, y),
            _ => false,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_of_a_tool_and_equality() {
        let sword = vanillite_data::item::ids::DIAMOND_SWORD.0;
        assert_eq!(
            default_component(sword, ids::DAMAGE).as_deref(),
            Some(&[0u8][..])
        );
        assert!(has_default(sword, ids::MAX_DAMAGE));
        assert!(!has_default(sword, ids::CUSTOM_NAME));
        // Enchantment maps are unordered.
        assert!(values_equal(
            ids::ENCHANTMENTS,
            &[2, 1, 1, 3, 2],
            &[2, 3, 2, 1, 1]
        ));
        assert!(!values_equal(
            ids::ENCHANTMENTS,
            &[2, 1, 1, 3, 2],
            &[2, 3, 1, 1, 1]
        ));
    }

    #[test]
    fn compounds_are_unordered() {
        let mut a = Compound::new();
        a.insert("x", 1);
        a.insert("y", 2);
        let mut b = Compound::new();
        b.insert("y", 2);
        b.insert("x", 1);
        let enc = |c: &Compound| {
            let mut out = Vec::new();
            vanillite_nbt::write::write_network(&Tag::Compound(c.clone()), &mut out).ok();
            out
        };
        assert!(values_equal(ids::CUSTOM_DATA, &enc(&a), &enc(&b)));
        b.insert("z", 3);
        assert!(!values_equal(ids::CUSTOM_DATA, &enc(&a), &enc(&b)));
    }
}
