//! Item stacks with their data component patch, and their wire codecs.
//!
//! A stack is an item id, a count and an optional boxed component patch: most stacks carry no
//! patch, so a plain stack is 16 bytes (architecture 4.5). Component values are kept as their
//! validated wire encoding (the component's stream codec): the server reads the few it needs
//! for gameplay and passes the rest through unchanged.
//!
//! Two codecs exist, as in Vanilla:
//! - the trusted one (`ItemStack.OPTIONAL_STREAM_CODEC`), which the server sends: component
//!   values follow each other without delimiters;
//! - the untrusted one (`OPTIONAL_UNTRUSTED_STREAM_CODEC`), which clients send in
//!   `set_creative_mode_slot`: every value is length-prefixed. [`ItemStack::decode_untrusted`]
//!   bounds the patch and validates every value with [`crate::components::validate`].

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use crate::codec::{Encode, Error, Reader, Result, write_len, write_varint};
use crate::components::Registries;

/// Most components one stack may carry (Vanilla has about 100 component types).
pub const MAX_COMPONENTS: usize = 128;
/// Most bytes of one component value from a client.
pub const MAX_COMPONENT_BYTES: usize = 64 * 1024;
/// Most bytes of all component values of one stack from a client.
pub const MAX_PATCH_BYTES: usize = 256 * 1024;

/// A data component patch: added or changed components with their wire value, and removed
/// default components. Both lists are sorted by component type id and hold each type once.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ComponentPatch {
    /// `(type id, value)`.
    pub added: Vec<(u16, Box<[u8]>)>,
    /// Removed type ids.
    pub removed: Vec<u16>,
}

impl ComponentPatch {
    /// Whether the patch changes nothing.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }

    /// The wire value of an added component.
    pub fn get(&self, type_id: u16) -> Option<&[u8]> {
        self.added
            .binary_search_by_key(&type_id, |(t, _)| *t)
            .ok()
            .and_then(|i| self.added.get(i))
            .map(|(_, v)| &**v)
    }

    /// Whether a default component is removed.
    pub fn is_removed(&self, type_id: u16) -> bool {
        self.removed.binary_search(&type_id).is_ok()
    }

    /// Sets a component value (removing a removal of the same type).
    pub fn set(&mut self, type_id: u16, value: Box<[u8]>) {
        if let Ok(i) = self.removed.binary_search(&type_id) {
            self.removed.remove(i);
        }
        match self.added.binary_search_by_key(&type_id, |(t, _)| *t) {
            Ok(i) => {
                if let Some(slot) = self.added.get_mut(i) {
                    slot.1 = value;
                }
            }
            Err(i) => self.added.insert(i, (type_id, value)),
        }
    }

    /// Drops an added component (the default applies again).
    pub fn unset(&mut self, type_id: u16) {
        if let Ok(i) = self.added.binary_search_by_key(&type_id, |(t, _)| *t) {
            self.added.remove(i);
        }
    }

    /// Encodes the patch with `delimited` values (untrusted codec) or without.
    fn encode_with(&self, out: &mut Vec<u8>, delimited: bool) {
        write_len(out, self.added.len());
        write_len(out, self.removed.len());
        for (t, v) in &self.added {
            write_varint(out, i32::from(*t));
            if delimited {
                write_len(out, v.len());
            }
            out.extend_from_slice(v);
        }
        for t in &self.removed {
            write_varint(out, i32::from(*t));
        }
    }
}

/// An item stack. The empty stack has count 0 (`ItemStack.EMPTY`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ItemStack {
    /// Item id (`minecraft:item` registry); 0 (air) for the empty stack.
    pub item: u16,
    /// Count; at most 99 for stacks from clients.
    pub count: i32,
    /// Component changes against the item's defaults.
    pub patch: Option<Box<ComponentPatch>>,
}

impl ItemStack {
    /// The empty stack.
    pub const EMPTY: ItemStack = ItemStack {
        item: 0,
        count: 0,
        patch: None,
    };

    /// A stack of `count` items without component changes.
    pub fn new(item: u16, count: i32) -> ItemStack {
        ItemStack {
            item,
            count,
            patch: None,
        }
    }

    /// `ItemStack.isEmpty`.
    pub fn is_empty(&self) -> bool {
        self.item == 0 || self.count <= 0
    }

    /// The component patch, if the stack has one.
    pub fn patch(&self) -> Option<&ComponentPatch> {
        self.patch.as_deref().filter(|p| !p.is_empty())
    }

    /// The patch, created on first use.
    pub fn patch_mut(&mut self) -> &mut ComponentPatch {
        self.patch.get_or_insert_with(Box::default)
    }

    /// The wire value of an added component.
    pub fn component(&self, type_id: u16) -> Option<&[u8]> {
        self.patch.as_deref().and_then(|p| p.get(type_id))
    }

    /// `ItemStack.isSameItemSameComponents`.
    pub fn same_item_same_components(&self, other: &ItemStack) -> bool {
        self.item == other.item && self.patch() == other.patch()
    }

    /// A copy with another count (`copyWithCount`).
    pub fn with_count(&self, count: i32) -> ItemStack {
        if count <= 0 {
            return ItemStack::EMPTY;
        }
        ItemStack {
            count,
            ..self.clone()
        }
    }

    /// Takes up to `n` items into a new stack (`split`).
    pub fn split(&mut self, n: i32) -> ItemStack {
        let n = n.min(self.count).max(0);
        let taken = self.with_count(n);
        self.shrink(n);
        taken
    }

    /// `shrink`: removes `n` items; the stack becomes empty at 0.
    pub fn shrink(&mut self, n: i32) {
        self.count = self.count.saturating_sub(n);
        if self.count <= 0 {
            *self = ItemStack::EMPTY;
        }
    }

    /// `grow`.
    pub fn grow(&mut self, n: i32) {
        self.count = self.count.saturating_add(n);
    }

    /// Decodes the untrusted codec (`set_creative_mode_slot`). `max_count` bounds the count;
    /// every component value must pass [`crate::components::validate`] with `registries`.
    pub fn decode_untrusted(r: &mut Reader<'_>, registries: &dyn Registries) -> Result<ItemStack> {
        let count = r.varint()?;
        if count <= 0 {
            return Ok(ItemStack::EMPTY);
        }
        if count > 99 {
            return Err(Error::Invalid("item count"));
        }
        let item = r.varint()?;
        let item = u16::try_from(item)
            .ok()
            .filter(|&i| i != 0 && usize::from(i) < vanillite_data::item::ITEMS.len())
            .ok_or(Error::Invalid("item id"))?;
        let added = r.len_prefix("added components", MAX_COMPONENTS, 2)?;
        let removed = r.len_prefix("removed components", MAX_COMPONENTS, 1)?;
        if added.saturating_add(removed) > MAX_COMPONENTS {
            return Err(Error::TooLong {
                what: "component patch",
                len: added.saturating_add(removed),
                max: MAX_COMPONENTS,
            });
        }
        let mut patch = ComponentPatch::default();
        let mut total = 0usize;
        for _ in 0..added {
            let t = component_type(r)?;
            let len = r.len_prefix("component value", MAX_COMPONENT_BYTES, 1)?;
            total = total.saturating_add(len);
            if total > MAX_PATCH_BYTES {
                return Err(Error::TooLong {
                    what: "component patch bytes",
                    len: total,
                    max: MAX_PATCH_BYTES,
                });
            }
            let value = r.take(len)?;
            crate::components::validate(t, value, registries)?;
            if patch.get(t).is_some() {
                return Err(Error::Invalid("duplicate component"));
            }
            patch.set(t, value.into());
        }
        for _ in 0..removed {
            let t = component_type(r)?;
            if patch.get(t).is_some() || patch.is_removed(t) {
                return Err(Error::Invalid("duplicate component"));
            }
            if let Err(i) = patch.removed.binary_search(&t) {
                patch.removed.insert(i, t);
            }
        }
        Ok(ItemStack {
            item,
            count,
            patch: (!patch.is_empty()).then(|| Box::new(patch)),
        })
    }

    /// Encodes the untrusted (delimited) codec, as clients send it.
    pub fn encode_untrusted(&self, out: &mut Vec<u8>) {
        self.encode_with(out, true);
    }

    fn encode_with(&self, out: &mut Vec<u8>, delimited: bool) {
        if self.is_empty() {
            write_varint(out, 0);
            return;
        }
        write_varint(out, self.count);
        write_varint(out, i32::from(self.item));
        match self.patch.as_deref() {
            Some(p) => p.encode_with(out, delimited),
            None => out.extend_from_slice(&[0, 0]),
        }
    }

    /// Decodes the trusted codec (test clients, test vectors): component values are not
    /// delimited, so each is measured with its stream codec. Values keep their wire order.
    pub fn decode_plain(r: &mut Reader<'_>) -> Result<ItemStack> {
        let count = r.varint()?;
        if count <= 0 {
            return Ok(ItemStack::EMPTY);
        }
        let item = u16::try_from(r.varint()?).map_err(|_| Error::Invalid("item id"))?;
        let added = r.len_prefix("added components", MAX_COMPONENTS, 2)?;
        let removed = r.len_prefix("removed components", MAX_COMPONENTS, 1)?;
        let mut patch = ComponentPatch::default();
        for _ in 0..added {
            let t = component_type(r)?;
            let len = crate::components::value_len(t, r.rest())?;
            patch.added.push((t, r.take(len)?.into()));
        }
        for _ in 0..removed {
            let t = component_type(r)?;
            if let Err(i) = patch.removed.binary_search(&t) {
                patch.removed.insert(i, t);
            }
        }
        Ok(ItemStack {
            item,
            count,
            patch: (!patch.is_empty()).then(|| Box::new(patch)),
        })
    }
}

impl ItemStack {
    /// Encodes `Optional<ItemStackTemplate>` (`ByteBufCodecs.optional(ItemStackTemplate
    /// .STREAM_CODEC)`): presence, item, count, trusted patch. Used by component values that
    /// hold stacks (`container`, `pot_decorations`, `bundle_contents`, …).
    pub fn encode_template_optional(&self, out: &mut Vec<u8>) {
        if self.is_empty() {
            out.push(0);
            return;
        }
        out.push(1);
        self.encode_template(out);
    }

    /// Encodes a non-empty stack as `ItemStackTemplate` (item, count, trusted patch).
    pub fn encode_template(&self, out: &mut Vec<u8>) {
        write_varint(out, i32::from(self.item));
        write_varint(out, self.count);
        match self.patch.as_deref() {
            Some(p) => p.encode_with(out, false),
            None => out.extend_from_slice(&[0, 0]),
        }
    }

    /// Decodes what [`Self::encode_template_optional`] writes (values from the server's own
    /// stores or already validated client values).
    pub fn decode_template_optional(r: &mut Reader<'_>) -> Result<ItemStack> {
        if r.u8()? == 0 {
            return Ok(ItemStack::EMPTY);
        }
        ItemStack::decode_template(r)
    }

    /// Decodes what [`Self::encode_template`] writes.
    pub fn decode_template(r: &mut Reader<'_>) -> Result<ItemStack> {
        let item = u16::try_from(r.varint()?).map_err(|_| Error::Invalid("item id"))?;
        let count = r.varint()?;
        if !(1..=99).contains(&count) {
            return Err(Error::Invalid("item stack template count"));
        }
        let mut header = Vec::with_capacity(8);
        write_varint(&mut header, count);
        write_varint(&mut header, i32::from(item));
        let rest = r.rest();
        let mut joined = Vec::with_capacity(header.len().saturating_add(rest.len()));
        joined.extend_from_slice(&header);
        joined.extend_from_slice(rest);
        let mut inner = Reader::new(&joined);
        let stack = ItemStack::decode_plain(&mut inner)?;
        let used = joined
            .len()
            .saturating_sub(inner.remaining())
            .saturating_sub(header.len());
        r.take(used)?;
        Ok(stack)
    }
}

/// The `container` component value of `items` (`ItemContainerContents.fromItems`): the slots
/// up to the last non-empty one; `None` when all are empty (the item's default).
pub fn encode_container_contents(items: &[ItemStack]) -> Option<Box<[u8]>> {
    let last = items.iter().rposition(|s| !s.is_empty())?;
    let mut out = Vec::new();
    write_len(&mut out, last.saturating_add(1));
    for stack in items.get(..=last)? {
        stack.encode_template_optional(&mut out);
    }
    Some(out.into_boxed_slice())
}

/// The slots of a `container` component value (at most 256).
pub fn decode_container_contents(value: &[u8]) -> Result<Vec<ItemStack>> {
    let mut r = Reader::new(value);
    let n = r.len_prefix("container slots", 256, 1)?;
    let mut items = Vec::with_capacity(n);
    for _ in 0..n {
        items.push(ItemStack::decode_template_optional(&mut r)?);
    }
    Ok(items)
}

/// The trusted codec (`OPTIONAL_STREAM_CODEC`), which the server sends.
impl Encode for ItemStack {
    fn encode(&self, out: &mut Vec<u8>) {
        self.encode_with(out, false);
    }
}

/// A data component type id, checked against the registry.
fn component_type(r: &mut Reader<'_>) -> Result<u16> {
    let t = r.varint()?;
    u16::try_from(t)
        .ok()
        .filter(|&t| usize::from(t) < crate::components::TYPE_COUNT)
        .ok_or(Error::Invalid("component type"))
}

/// Entry counts of the dynamic registries and the tags of all registries, installed once at
/// server start (after data packs are loaded and frozen) for decoding client stacks on the
/// network threads.
#[derive(Debug, Default)]
pub struct RegistryInfo {
    /// Dynamic registry → entry count (`minecraft:enchantment` → 42, …).
    pub sizes: HashMap<String, usize>,
    /// Registry → its tag ids (without `#`).
    pub tags: HashMap<String, HashSet<String>>,
    /// Dynamic registry → its entry names in id order (for saved item components).
    pub names: HashMap<String, Vec<String>>,
}

/// The installed information with the entry ids by name.
#[derive(Debug)]
struct Installed {
    info: RegistryInfo,
    ids: HashMap<String, HashMap<String, usize>>,
}

static INSTALLED: OnceLock<Installed> = OnceLock::new();

/// Installs the registry information. Later calls are ignored: registries are frozen for the
/// lifetime of the process.
pub fn install_registries(info: RegistryInfo) {
    let ids = info
        .names
        .iter()
        .map(|(registry, names)| {
            let by_name = names
                .iter()
                .enumerate()
                .map(|(id, name)| (name.clone(), id))
                .collect();
            (registry.clone(), by_name)
        })
        .collect();
    let _ = INSTALLED.set(Installed { info, ids });
}

/// Static registries plus the installed dynamic ones and tags.
#[derive(Debug, Clone, Copy, Default)]
pub struct InstalledRegistries;

impl Registries for InstalledRegistries {
    fn size(&self, registry: &str) -> Option<usize> {
        vanillite_data::registry::Registry::by_name(registry)
            .map(|r| r.len())
            .or_else(|| INSTALLED.get()?.info.sizes.get(registry).copied())
    }

    fn has_tag(&self, registry: &str, tag: &str) -> bool {
        INSTALLED
            .get()
            .and_then(|i| i.info.tags.get(registry))
            .is_some_and(|t| t.contains(tag))
    }
}

impl crate::components_nbt::RegistryNames for InstalledRegistries {
    fn entry_name(&self, registry: &str, id: usize) -> Option<&str> {
        INSTALLED
            .get()?
            .info
            .names
            .get(registry)?
            .get(id)
            .map(String::as_str)
    }

    fn entry_id(&self, registry: &str, name: &str) -> Option<usize> {
        INSTALLED.get()?.ids.get(registry)?.get(name).copied()
    }
}
