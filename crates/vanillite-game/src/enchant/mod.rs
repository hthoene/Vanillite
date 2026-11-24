//! Enchantments as the anvil, grindstone and enchanting table use them: the `enchantment`
//! registry of the data packs (costs, levels, supported items, exclusive sets, tags) and the
//! default `enchantable` and `repairable` item components.

mod items;

use std::collections::HashMap;

use serde_json::Value;
use vanillite_data::registry::ITEM;
use vanillite_protocol::codec::{Reader, write_varint};

use crate::item::ItemStack;

/// `Enchantment.Cost`: `base + per_level_above_first * (level - 1)`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cost {
    /// `base`.
    pub base: i32,
    /// `per_level_above_first`.
    pub per_level: i32,
}

impl Cost {
    /// `calculate(level)`.
    pub fn at(self, level: i32) -> i32 {
        self.base
            .saturating_add(self.per_level.saturating_mul(level.saturating_sub(1)))
    }
}

/// One enchantment of the registry.
#[derive(Clone, Debug, Default)]
pub struct Info {
    /// Its id (e.g. `minecraft:sharpness`).
    pub name: String,
    /// `max_level`.
    pub max_level: i32,
    /// `anvil_cost`.
    pub anvil_cost: i32,
    /// `weight`.
    pub weight: i32,
    /// `min_cost`.
    pub min_cost: Cost,
    /// `max_cost`.
    pub max_cost: Cost,
    /// `supported_items`, sorted.
    pub supported: Vec<u16>,
    /// `primary_items`, sorted (`None`: the supported items).
    pub primary: Option<Vec<u16>>,
    /// `exclusive_set`: enchantment ids.
    pub exclusive: Vec<u32>,
    /// In `#minecraft:curse`.
    pub curse: bool,
    /// In `#minecraft:in_enchanting_table`.
    pub in_table: bool,
}

/// The enchantment data of the loaded data packs.
#[derive(Clone, Debug, Default)]
pub struct Enchanting {
    /// By `minecraft:enchantment` id.
    pub list: Vec<Info>,
    /// `#minecraft:in_enchanting_table` in tag order.
    pub table: Vec<u32>,
    enchantable: HashMap<u16, i32>,
    repairable: HashMap<u16, Vec<u16>>,
    /// Item → `trim_material` registry id (`provides_trim_material`).
    trim_materials: HashMap<u16, u32>,
    /// `trim_pattern` id → registry id.
    trim_patterns: HashMap<String, u32>,
}

/// A holder set of a registry: `"id"`, `"#tag"` or a list, resolved with `tag` and `id`.
fn holder_set<T: Ord>(
    v: Option<&Value>,
    tag: &dyn Fn(&str) -> Vec<T>,
    id: &dyn Fn(&str) -> Option<T>,
) -> Vec<T> {
    let mut out: Vec<T> = match v {
        Some(Value::String(s)) => match s.strip_prefix('#') {
            Some(t) => tag(t),
            None => id(s).into_iter().collect(),
        },
        Some(Value::Array(list)) => list
            .iter()
            .filter_map(Value::as_str)
            .filter_map(id)
            .collect(),
        _ => Vec::new(),
    };
    out.sort();
    out.dedup();
    out
}

fn cost(v: Option<&Value>) -> Cost {
    let int = |k: &str| {
        v.and_then(|v| v.get(k))
            .and_then(Value::as_i64)
            .and_then(|n| i32::try_from(n).ok())
            .unwrap_or(0)
    };
    Cost {
        base: int("base"),
        per_level: int("per_level_above_first"),
    }
}

impl Enchanting {
    /// Loads the registry entries (`(id, json)` in protocol id order), with item tags and
    /// enchantment tags resolved by the callbacks.
    pub fn load(
        entries: &[(String, Value)],
        item_tag: &dyn Fn(&str) -> Vec<u16>,
        enchantment_tag: &dyn Fn(&str) -> Vec<u32>,
    ) -> Enchanting {
        let ids: HashMap<&str, u32> = entries
            .iter()
            .zip(0u32..)
            .map(|((name, _), i)| (name.as_str(), i))
            .collect();
        let item = |s: &str| ITEM.id(s);
        let enchantment = |s: &str| ids.get(s).copied();
        let curses = enchantment_tag("minecraft:curse");
        let table = enchantment_tag("minecraft:in_enchanting_table");
        let list = entries
            .iter()
            .zip(0u32..)
            .map(|((name, json), i)| {
                let int = |k: &str| {
                    json.get(k)
                        .and_then(Value::as_i64)
                        .and_then(|n| i32::try_from(n).ok())
                        .unwrap_or(0)
                };
                Info {
                    name: name.clone(),
                    max_level: int("max_level"),
                    anvil_cost: int("anvil_cost"),
                    weight: int("weight"),
                    min_cost: cost(json.get("min_cost")),
                    max_cost: cost(json.get("max_cost")),
                    supported: holder_set(json.get("supported_items"), item_tag, &item),
                    primary: json
                        .get("primary_items")
                        .map(|p| holder_set(Some(p), item_tag, &item)),
                    exclusive: holder_set(json.get("exclusive_set"), enchantment_tag, &enchantment),
                    curse: curses.contains(&i),
                    in_table: table.contains(&i),
                }
            })
            .collect();
        let table = table.clone();
        let named = |n: &str| ITEM.id(&format!("minecraft:{n}"));
        let enchantable = items::ENCHANTABLE
            .iter()
            .filter_map(|(n, v)| Some((named(n)?, i32::from(*v))))
            .collect();
        let repairable = items::REPAIRABLE
            .iter()
            .filter_map(|(n, v)| {
                let set = holder_set(Some(&Value::String((*v).to_owned())), item_tag, &item);
                Some((named(n)?, set))
            })
            .collect();
        Enchanting {
            list,
            table,
            enchantable,
            repairable,
            trim_materials: HashMap::new(),
            trim_patterns: HashMap::new(),
        }
    }

    /// Sets the trim registries (`trim_material` and `trim_pattern` ids in protocol order).
    pub fn set_trims(&mut self, materials: &[String], patterns: &[String]) {
        let named = |n: &str| ITEM.id(&format!("minecraft:{n}"));
        self.trim_materials = items::TRIM_MATERIALS
            .iter()
            .filter_map(|(item, material)| {
                let id = materials.iter().position(|m| m == material)?;
                Some((named(item)?, u32::try_from(id).ok()?))
            })
            .collect();
        self.trim_patterns = patterns
            .iter()
            .zip(0u32..)
            .map(|(p, i)| (p.clone(), i))
            .collect();
    }

    /// `provides_trim_material` of an item: the `trim_material` id.
    pub fn trim_material(&self, stack: &ItemStack) -> Option<u32> {
        self.trim_materials.get(&stack.item).copied()
    }

    /// The `trim_pattern` id of a pattern name.
    pub fn trim_pattern(&self, name: &str) -> Option<u32> {
        self.trim_patterns.get(name).copied()
    }

    /// The enchantment with id `id`.
    pub fn get(&self, id: u32) -> Option<&Info> {
        self.list.get(usize::try_from(id).ok()?)
    }

    /// `Enchantment.canEnchant(stack)`: the item is supported.
    pub fn can_enchant(&self, id: u32, stack: &ItemStack) -> bool {
        self.get(id)
            .is_some_and(|e| e.supported.binary_search(&stack.item).is_ok())
    }

    /// `Enchantment.isPrimaryItem`.
    pub fn is_primary(&self, id: u32, stack: &ItemStack) -> bool {
        self.get(id).is_some_and(|e| {
            e.primary
                .as_ref()
                .unwrap_or(&e.supported)
                .binary_search(&stack.item)
                .is_ok()
        })
    }

    /// `Enchantment.areCompatible(a, b)`.
    pub fn compatible(&self, a: u32, b: u32) -> bool {
        a != b
            && !self.get(a).is_some_and(|e| e.exclusive.contains(&b))
            && !self.get(b).is_some_and(|e| e.exclusive.contains(&a))
    }

    /// Whether the enchantment is a curse.
    pub fn is_curse(&self, id: u32) -> bool {
        self.get(id).is_some_and(|e| e.curse)
    }

    /// The `enchantable` value of an item (0: not enchantable at a table).
    pub fn enchantable(&self, stack: &ItemStack) -> i32 {
        if stack.is_empty() {
            return 0;
        }
        self.enchantable.get(&stack.item).copied().unwrap_or(0)
    }

    /// `ItemStack.isValidRepairItem(addition)`: the `repairable` component.
    pub fn is_valid_repair_item(&self, stack: &ItemStack, addition: &ItemStack) -> bool {
        !addition.is_empty()
            && self
                .repairable
                .get(&stack.item)
                .is_some_and(|items| items.binary_search(&addition.item).is_ok())
    }
}

/// An `ItemEnchantments` value: `(enchantment id, level)` in insertion order.
pub fn read(data: Option<&[u8]>) -> Vec<(u32, i32)> {
    let Some(data) = data else {
        return Vec::new();
    };
    let mut r = Reader::new(data);
    let Ok(n) = r.varint() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for _ in 0..n.clamp(0, 256) {
        let (Ok(id), Ok(level)) = (r.varint(), r.varint()) else {
            break;
        };
        if let Ok(id) = u32::try_from(id) {
            out.push((id, level));
        }
    }
    out
}

/// Encodes an `ItemEnchantments` value.
pub fn write(list: &[(u32, i32)]) -> Box<[u8]> {
    let mut out = Vec::new();
    write_varint(&mut out, i32::try_from(list.len()).unwrap_or(0));
    for &(id, level) in list {
        write_varint(&mut out, i32::try_from(id).unwrap_or(0));
        write_varint(&mut out, level);
    }
    out.into_boxed_slice()
}

/// An integer component (`repair_cost`).
pub fn read_int(data: Option<&[u8]>) -> i32 {
    data.and_then(|d| Reader::new(d).varint().ok()).unwrap_or(0)
}

/// Encodes an integer component.
pub fn write_int(value: i32) -> Box<[u8]> {
    let mut out = Vec::new();
    write_varint(&mut out, value);
    out.into_boxed_slice()
}
