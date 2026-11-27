//! `LockCode`: the `lock` of container block entities (`BaseContainerBlockEntity`) and
//! beacons. The lock is an item predicate stored as its persistent NBT form; a player opens
//! the block only with a matching main-hand item (spectators always).

use vanillite_base::BlockPos;
use vanillite_nbt::{Compound, Tag};
use vanillite_protocol::components::ids;
use vanillite_protocol::item::ItemStack;

use crate::level::{BlockEntity, Level};

/// `LockCode.TAG_LOCK`.
const TAG_LOCK: &str = "lock";

/// `LockCode.fromTag`: a lock that is no item predicate (or the empty one, `NO_LOCK`) reads
/// as no lock.
pub fn load(nbt: &Compound) -> Option<Tag> {
    match nbt.get(TAG_LOCK) {
        Some(Tag::Compound(c)) if !c.is_empty() => Some(Tag::Compound(c.clone())),
        _ => None,
    }
}

/// `LockCode.addToTag`.
pub fn save(lock: &Option<Tag>, out: &mut Compound) {
    if let Some(lock) = lock {
        out.insert(TAG_LOCK, lock.clone());
    }
}

/// The `minecraft:lock` component value of a lock (`collectImplicitComponents`).
pub fn component(lock: &Tag) -> Option<Box<[u8]>> {
    crate::item_nbt::component_from_nbt(ids::LOCK, lock)
}

/// The lock a stack gives the block it places (`applyImplicitComponents`).
pub fn from_stack(stack: &ItemStack) -> Option<Tag> {
    stack
        .component(ids::LOCK)
        .and_then(|wire| crate::item_nbt::component_to_nbt(ids::LOCK, wire))
        .filter(|t| !matches!(t, Tag::Compound(c) if c.is_empty()))
}

/// The lock field of a block entity that has one.
pub fn slot(entity: &BlockEntity) -> Option<&Option<Tag>> {
    match entity {
        BlockEntity::Container(c) => Some(&c.lock),
        BlockEntity::Hopper(h) => Some(&h.lock),
        BlockEntity::Furnace(f) => Some(&f.lock),
        BlockEntity::BrewingStand(b) => Some(&b.lock),
        BlockEntity::Crafter(c) => Some(&c.lock),
        BlockEntity::Beacon(b) => Some(&b.lock),
        _ => None,
    }
}

/// [`slot`], mutable.
pub fn slot_mut(entity: &mut BlockEntity) -> Option<&mut Option<Tag>> {
    match entity {
        BlockEntity::Container(c) => Some(&mut c.lock),
        BlockEntity::Hopper(h) => Some(&mut h.lock),
        BlockEntity::Furnace(f) => Some(&mut f.lock),
        BlockEntity::BrewingStand(b) => Some(&mut b.lock),
        BlockEntity::Crafter(c) => Some(&mut c.lock),
        BlockEntity::Beacon(b) => Some(&mut b.lock),
        _ => None,
    }
}

/// `canOpen` for a non-spectator: the block at `pos` has no lock or `main_hand` unlocks it.
pub fn can_open(level: &Level, pos: BlockPos, main_hand: &ItemStack) -> bool {
    level
        .block_entity(pos)
        .and_then(slot)
        .and_then(Option::as_ref)
        .is_none_or(|lock| unlocks_with(lock, main_hand))
}

/// `LockCode.unlocksWith`: `ItemPredicate.test` of the stored predicate. Exact components
/// are compared with the stack's added components (a value the item only has by default
/// does not match); sub-predicates (`predicates`) are not evaluated and never match.
pub fn unlocks_with(lock: &Tag, stack: &ItemStack) -> bool {
    let Tag::Compound(p) = lock else {
        return false;
    };
    let (item, count) = if stack.is_empty() {
        (0, 0)
    } else {
        (stack.item, stack.count)
    };
    if let Some(items) = p.get("items")
        && !holder_set_contains(items, item)
    {
        return false;
    }
    if let Some(bounds) = p.get("count")
        && !count_matches(bounds, count)
    {
        return false;
    }
    if let Some(Tag::Compound(c)) = p.get("predicates")
        && !c.is_empty()
    {
        return false;
    }
    match p.get("components") {
        None => true,
        Some(Tag::Compound(c)) => c.iter().all(|(k, v)| component_matches(stack, k, v)),
        Some(_) => false,
    }
}

/// A holder set of items: `"id"`, `"#tag"` or a list of ids.
fn holder_set_contains(set: &Tag, item: u16) -> bool {
    let one = |name: &str| match name.strip_prefix('#') {
        Some(tag) => {
            crate::player::play::game_data().is_some_and(|d| d.item_in_tag(item, &qualify(tag)))
        }
        None => vanillite_data::registry::ITEM.id(&qualify(name)) == Some(item),
    };
    match set {
        Tag::String(s) => one(s),
        Tag::List(list) => list.0.iter().any(|t| t.as_str().is_some_and(one)),
        _ => false,
    }
}

/// `MinMaxBounds.Ints`: an exact int or `{min, max}`.
fn count_matches(bounds: &Tag, count: i32) -> bool {
    match bounds {
        Tag::Compound(c) => {
            let get = |k: &str| c.get(k).and_then(Tag::as_i64);
            let count = i64::from(count);
            get("min").is_none_or(|m| count >= m) && get("max").is_none_or(|m| count <= m)
        }
        t => t.as_i64() == Some(i64::from(count)),
    }
}

/// `DataComponentExactPredicate`: the stack's added value of `key` equals `expected`.
fn component_matches(stack: &ItemStack, key: &str, expected: &Tag) -> bool {
    let Some(id) = vanillite_protocol::components_nbt::type_by_name(key) else {
        return false;
    };
    let Some(actual) = stack.component(id) else {
        return false;
    };
    if id == ids::CUSTOM_NAME || id == ids::ITEM_NAME {
        // Text components compare as components (`"Key"` equals `{text: "Key"}`).
        let actual = crate::item_nbt::component_to_nbt(id, actual);
        let text = vanillite_protocol::text::Component::try_from_nbt;
        return match (actual.as_ref().map(text), text(expected)) {
            (Some(Ok(a)), Ok(b)) => a == b,
            _ => false,
        };
    }
    crate::item_nbt::component_from_nbt(id, expected).is_some_and(|wire| *wire == *actual)
}

/// Adds the default namespace.
fn qualify(name: &str) -> String {
    if name.contains(':') {
        name.to_owned()
    } else {
        format!("minecraft:{name}")
    }
}
