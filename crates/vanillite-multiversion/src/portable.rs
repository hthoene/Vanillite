//! Item stacks below the newest step. The newest step keeps only the *portable* data
//! components for clients older than its own version: the ones whose codec every version from
//! 1.20.5 on can read and every step can convert (names, lore, damage, enchantments, colours,
//! potions, maps, trims, model data). Each older step reads them in its newer version's codec
//! and writes them in its older one; their registry ids follow the step's tables.
//!
//! Every vanilla item keeps its default components (the client has them built in), so what is
//! lost for older clients is only what a stack changes beyond the portable ones.

use crate::Result;
use crate::common::Registries;
use crate::io;
use crate::step::{V1_21, V1_21_2, V1_21_4, V1_21_5, V26_1};
use crate::table::Tables;
use vanillite_protocol::Error;
use vanillite_protocol::codec::{Reader, write_varint};

/// The components older clients get, sorted.
pub(crate) const PORTABLE: &[&str] = &[
    "minecraft:base_color",
    "minecraft:bundle_contents",
    "minecraft:container",
    "minecraft:custom_model_data",
    "minecraft:custom_name",
    "minecraft:damage",
    "minecraft:dyed_color",
    "minecraft:enchantment_glint_override",
    "minecraft:enchantments",
    "minecraft:item_name",
    "minecraft:lore",
    "minecraft:map_id",
    "minecraft:max_damage",
    "minecraft:max_stack_size",
    "minecraft:potion_contents",
    "minecraft:rarity",
    "minecraft:repair_cost",
    "minecraft:stored_enchantments",
    "minecraft:trim",
    "minecraft:unbreakable",
    "minecraft:writable_book_content",
    "minecraft:written_book_content",
];

/// Most container and bundle levels a stack may be nested in (a shulker box in a bundle in a
/// shulker box is two).
const MAX_NESTING: u8 = 4;

/// Whether older clients get a component.
pub(crate) fn portable(name: &str) -> bool {
    PORTABLE.binary_search(&name).is_ok()
}

/// Translation direction of one step.
#[derive(Clone, Copy)]
pub(crate) struct Items<'a> {
    pub t: &'a Tables,
    pub regs: &'a Registries,
    /// Server to client (newer → older) or the reverse.
    pub clientbound: bool,
    /// Stacks this one is nested in (containers, bundles).
    pub depth: u8,
}

impl Items<'_> {
    pub(crate) fn from(&self) -> i32 {
        if self.clientbound {
            self.t.next_protocol
        } else {
            self.t.protocol
        }
    }

    pub(crate) fn to(&self) -> i32 {
        if self.clientbound {
            self.t.protocol
        } else {
            self.t.next_protocol
        }
    }

    /// A static registry id in the target version.
    pub(crate) fn id(&self, registry: &str, id: i32) -> Option<i32> {
        if self.clientbound {
            self.t.older(registry, id)
        } else {
            self.t.newer(registry, id)
        }
    }

    /// A synchronised registry id in the target version.
    pub(crate) fn dynamic(&self, registry: &str, id: i32) -> Option<i32> {
        if self.clientbound {
            return self.regs.older(registry, id);
        }
        match self.regs.get(registry) {
            Some(d) => d
                .to_old
                .iter()
                .position(|&o| o == id)
                .and_then(|i| i32::try_from(i).ok()),
            None => Some(id),
        }
    }

    /// An enchantment id: synchronised from 1.21 on, a static registry before.
    fn enchantment(&self, id: i32) -> Option<i32> {
        let (from, to) = (self.from(), self.to());
        if from >= V1_21 && to >= V1_21 {
            return self.dynamic("minecraft:enchantment", id);
        }
        if from < V1_21 && to < V1_21 {
            return Some(id);
        }
        // The step 1.20.5 ↔ 1.21: by name between the static and the synchronised registry.
        let names = self.regs.dropped("minecraft:enchantment")?;
        if self.clientbound {
            let name = names.get(usize::try_from(id).ok()?)?;
            let i = self.t.old_enchantments.iter().position(|n| n == name)?;
            i32::try_from(i).ok()
        } else {
            let name = self.t.old_enchantments.get(usize::try_from(id).ok()?)?;
            let i = names.iter().position(|n| n == name)?;
            i32::try_from(i).ok()
        }
    }

    /// The target id of a component type, if the target version has it.
    pub(crate) fn component_type(&self, id: i32) -> Option<i32> {
        self.id("minecraft:data_component_type", id)
    }

    /// The name of a component type of the source version.
    fn component_name(&self, id: i32) -> Option<&'static str> {
        let new = if self.clientbound {
            id
        } else {
            self.t.newer("minecraft:data_component_type", id)?
        };
        self.t
            .new_data_component_types
            .get(usize::try_from(new).ok()?)
            .copied()
    }

    /// `ItemStack.OPTIONAL_STREAM_CODEC`: count, then item and patch unless empty.
    pub(crate) fn stack(&self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
        let count = r.varint()?;
        if count <= 0 {
            write_varint(out, 0);
            return Ok(());
        }
        let item = r.varint()?;
        match self.id("minecraft:item", item) {
            Some(item) => {
                write_varint(out, count);
                write_varint(out, item);
                self.patch(r, out)
            }
            None => {
                // An item the target version lacks: nothing (clientbound items always map).
                self.patch(r, &mut Vec::new())?;
                write_varint(out, 0);
                Ok(())
            }
        }
    }

    /// `ItemStackTemplate` (26.1): item, count, patch.
    pub(crate) fn template(&self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
        let item = r.varint()?;
        write_varint(out, self.id("minecraft:item", item).unwrap_or(1));
        io::varint(r, out)?;
        self.patch(r, out)
    }

    /// The item particle's item: a template from 26.1 on, a stack before.
    pub(crate) fn particle_item(&self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
        match (self.from() >= V26_1, self.to() >= V26_1) {
            (true, true) => self.template(r, out),
            (false, false) => self.stack(r, out),
            (true, false) => {
                let item = r.varint()?;
                let count = r.varint()?;
                write_varint(out, count.max(1));
                write_varint(out, self.id("minecraft:item", item).unwrap_or(1));
                self.patch(r, out)
            }
            (false, true) => {
                let count = r.varint()?;
                let item = if count > 0 { r.varint()? } else { 1 };
                write_varint(out, self.id("minecraft:item", item).unwrap_or(1));
                write_varint(out, count.max(1));
                if count > 0 {
                    self.patch(r, out)
                } else {
                    write_varint(out, 0);
                    write_varint(out, 0);
                    Ok(())
                }
            }
        }
    }

    /// `DataComponentPatch` (trusted): components of other than the portable types cannot be
    /// read; such a stack fails (the packet is dropped).
    pub(crate) fn patch(&self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
        let added = io::len(r, 256, 1)?;
        let removed = io::len(r, 256, 1)?;
        let (kept, values) = self.typed(r, added)?;
        let mut removals = Vec::new();
        let mut kept_removed = 0usize;
        for _ in 0..removed {
            if let Some(t) = self.component_type(r.varint()?) {
                write_varint(&mut removals, t);
                kept_removed = kept_removed.saturating_add(1);
            }
        }
        io::write_len(out, kept);
        io::write_len(out, kept_removed);
        out.extend_from_slice(&values);
        out.extend_from_slice(&removals);
        Ok(())
    }

    /// A list of typed components (an exact component predicate of a merchant cost).
    pub(crate) fn components(&self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
        let n = io::len(r, 256, 1)?;
        let (kept, values) = self.typed(r, n)?;
        io::write_len(out, kept);
        out.extend_from_slice(&values);
        Ok(())
    }

    /// `n` components (type, value): how many the target keeps and their bytes.
    fn typed(&self, r: &mut Reader<'_>, n: usize) -> Result<(usize, Vec<u8>)> {
        let mut values = Vec::new();
        let mut kept = 0usize;
        for _ in 0..n {
            let t = r.varint()?;
            let name = self
                .component_name(t)
                .filter(|n| portable(n))
                .ok_or(Error::Invalid(
                    "data component an older version cannot read",
                ))?;
            let mut v = Vec::new();
            let keep = self.component(name, r, &mut v)?;
            if keep && let Some(t) = self.component_type(t) {
                write_varint(&mut values, t);
                values.extend_from_slice(&v);
                kept = kept.saturating_add(1);
            }
        }
        Ok((kept, values))
    }

    /// `DataComponentPatch` with length-prefixed values (untrusted, from clients, 1.21.5): the
    /// portable components are converted, the others left out.
    pub(crate) fn delimited_patch(&self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
        let added = io::len(r, 256, 2)?;
        let removed = io::len(r, 256, 1)?;
        let mut values = Vec::new();
        let mut kept = 0usize;
        for _ in 0..added {
            let t = r.varint()?;
            let len = io::len(r, 1 << 20, 1)?;
            let mut value = Reader::new(r.take(len)?);
            let Some(name) = self.component_name(t).filter(|n| portable(n)) else {
                continue;
            };
            let mut v = Vec::new();
            if self.component(name, &mut value, &mut v)?
                && let Some(t) = self.component_type(t)
            {
                write_varint(&mut values, t);
                io::write_len(&mut values, v.len());
                values.extend_from_slice(&v);
                kept = kept.saturating_add(1);
            }
        }
        let mut removals = Vec::new();
        let mut kept_removed = 0usize;
        for _ in 0..removed {
            if let Some(t) = self.component_type(r.varint()?) {
                write_varint(&mut removals, t);
                kept_removed = kept_removed.saturating_add(1);
            }
        }
        io::write_len(out, kept);
        io::write_len(out, kept_removed);
        out.extend_from_slice(&values);
        out.extend_from_slice(&removals);
        Ok(())
    }

    /// A stack a client sends for its creative inventory: trusted codec before 1.21.5,
    /// length-prefixed values from then on.
    pub(crate) fn client_stack(&self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
        let count = r.varint()?;
        if count <= 0 {
            write_varint(out, 0);
            return Ok(());
        }
        let item = self
            .id("minecraft:item", r.varint()?)
            .ok_or(Error::Invalid("item unknown to the newer version"))?;
        write_varint(out, count);
        write_varint(out, item);
        let mut patch = Vec::new();
        if self.from() >= V1_21_5 {
            self.delimited_patch(r, &mut patch)?;
        } else {
            self.patch(r, &mut patch)?;
        }
        if self.to() >= V1_21_5 && self.from() < V1_21_5 {
            // Trusted → delimited: prefix each value with its length.
            let mut pr = Reader::new(&patch);
            let added = io::len(&mut pr, 256, 1)?;
            let removed = io::len(&mut pr, 256, 1)?;
            io::write_len(out, added);
            io::write_len(out, removed);
            for _ in 0..added {
                let t = pr.varint()?;
                let name = self
                    .t
                    .new_data_component_types
                    .get(usize::try_from(t).unwrap_or(usize::MAX))
                    .copied()
                    .ok_or(Error::Invalid("component type"))?;
                let start = pr.rest();
                skip_component(
                    name,
                    &mut pr,
                    self.to(),
                    self.t.new_data_component_types,
                    MAX_NESTING.saturating_add(2),
                )?;
                let len = start.len().saturating_sub(pr.remaining());
                write_varint(out, t);
                io::write_len(out, len);
                out.extend_from_slice(start.get(..len).ok_or(Error::UnexpectedEnd)?);
            }
            for _ in 0..removed {
                io::varint(&mut pr, out)?;
            }
        } else {
            out.extend_from_slice(&patch);
        }
        Ok(())
    }

    /// A stack in a container click before 1.21.5, for a newer version: item and count, the
    /// components left out (from 1.21.5 on the server only compares hashes and sends the slot
    /// again when they differ).
    pub(crate) fn click_stack(&self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
        let count = r.varint()?;
        if count <= 0 {
            if self.to() >= V1_21_5 {
                out.push(0);
            } else {
                write_varint(out, 0);
            }
            return Ok(());
        }
        let item = r.varint()?;
        self.patch(r, &mut Vec::new())?;
        let item = self.id("minecraft:item", item).unwrap_or(1);
        if self.to() >= V1_21_5 {
            // HashedStack: present, item, count, no added or removed component hashes.
            out.push(1);
            write_varint(out, item);
            write_varint(out, count);
            out.extend_from_slice(&[0, 0]);
        } else {
            write_varint(out, count);
            write_varint(out, item);
            out.extend_from_slice(&[0, 0]);
        }
        Ok(())
    }

    /// A `HashedStack` (1.21.5) for a newer version: item and component type ids follow.
    pub(crate) fn hashed_stack(&self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
        if !io::boolean(r, out)? {
            return Ok(());
        }
        let item = r.varint()?;
        write_varint(out, self.id("minecraft:item", item).unwrap_or(1));
        io::varint(r, out)?;
        let added = io::len(r, 256, 5)?;
        let mut body = Vec::new();
        let mut kept = 0usize;
        for _ in 0..added {
            let t = r.varint()?;
            let hash = r.take(4)?;
            if let Some(t) = self.component_type(t) {
                write_varint(&mut body, t);
                body.extend_from_slice(hash);
                kept = kept.saturating_add(1);
            }
        }
        io::write_len(out, kept);
        out.extend_from_slice(&body);
        let removed = io::len(r, 256, 1)?;
        let mut body = Vec::new();
        let mut kept = 0usize;
        for _ in 0..removed {
            if let Some(t) = self.component_type(r.varint()?) {
                write_varint(&mut body, t);
                kept = kept.saturating_add(1);
            }
        }
        io::write_len(out, kept);
        out.extend_from_slice(&body);
        Ok(())
    }

    /// One portable component value from the source into the target version. `false` drops it.
    fn component(&self, name: &str, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<bool> {
        let (from, to) = (self.from(), self.to());
        // `show_in_tooltip` flags until 1.21.4 (1.21.5 moved them to `tooltip_display`).
        let read_show = |r: &mut Reader<'_>| -> Result<bool> {
            if from < V1_21_5 {
                Ok(r.u8()? != 0)
            } else {
                Ok(true)
            }
        };
        let write_show = |out: &mut Vec<u8>, show: bool| {
            if to < V1_21_5 {
                out.push(u8::from(show));
            }
        };
        match name.trim_start_matches("minecraft:") {
            "max_stack_size" | "max_damage" | "damage" | "rarity" | "repair_cost" | "map_id"
            | "base_color" => {
                io::varint(r, out)?;
            }
            "enchantment_glint_override" => io::bytes(r, out, 1)?,
            "custom_name" | "item_name" => io::nbt(r, out)?,
            "lore" => {
                let n = io::len(r, 256, 1)?;
                io::write_len(out, n);
                for _ in 0..n {
                    io::nbt(r, out)?;
                }
            }
            "unbreakable" => {
                let show = read_show(r)?;
                write_show(out, show);
            }
            "dyed_color" => {
                io::bytes(r, out, 4)?;
                let show = read_show(r)?;
                write_show(out, show);
            }
            "enchantments" | "stored_enchantments" => {
                let n = io::len(r, 1024, 2)?;
                let mut list = Vec::new();
                let mut kept = 0usize;
                for _ in 0..n {
                    let id = r.varint()?;
                    let level = r.varint()?;
                    if let Some(id) = self.enchantment(id) {
                        write_varint(&mut list, id);
                        write_varint(&mut list, level);
                        kept = kept.saturating_add(1);
                    }
                }
                io::write_len(out, kept);
                out.extend_from_slice(&list);
                let show = read_show(r)?;
                write_show(out, show);
            }
            "potion_contents" => {
                if io::boolean(r, out)? {
                    let potion = r.varint()?;
                    write_varint(out, self.id("minecraft:potion", potion).unwrap_or(0));
                }
                if io::boolean(r, out)? {
                    io::bytes(r, out, 4)?;
                }
                let n = io::len(r, 256, 2)?;
                let mut list = Vec::new();
                let mut kept = 0usize;
                for _ in 0..n {
                    let effect = r.varint()?;
                    let mut details = Vec::new();
                    effect_details(r, &mut details, 0)?;
                    if let Some(effect) = self.id("minecraft:mob_effect", effect) {
                        write_varint(&mut list, effect);
                        list.extend_from_slice(&details);
                        kept = kept.saturating_add(1);
                    }
                }
                io::write_len(out, kept);
                out.extend_from_slice(&list);
                // Custom name (1.21.2).
                let mut custom = Vec::new();
                if from >= V1_21_2 {
                    if io::boolean(r, &mut custom)? {
                        io::string(r, &mut custom)?;
                    }
                } else {
                    custom.push(0);
                }
                if to >= V1_21_2 {
                    out.extend_from_slice(&custom);
                }
            }
            "custom_model_data" => {
                if from >= V1_21_4 {
                    let mut value = Vec::new();
                    let floats = io::len(r, 1024, 4)?;
                    let mut first = None;
                    io::write_len(&mut value, floats);
                    for _ in 0..floats {
                        let f = r.f32()?;
                        first.get_or_insert(f);
                        value.extend_from_slice(&f.to_be_bytes());
                    }
                    let flags = io::len(r, 1024, 1)?;
                    io::write_len(&mut value, flags);
                    io::bytes(r, &mut value, flags)?;
                    let strings = io::len(r, 1024, 1)?;
                    io::write_len(&mut value, strings);
                    for _ in 0..strings {
                        io::string(r, &mut value)?;
                    }
                    let colors = io::len(r, 1024, 4)?;
                    io::write_len(&mut value, colors);
                    io::bytes(r, &mut value, colors.saturating_mul(4))?;
                    if to >= V1_21_4 {
                        out.extend_from_slice(&value);
                    } else {
                        // Before 1.21.4 one integer: the first float.
                        let Some(f) = first else { return Ok(false) };
                        let v = f.clamp(-2.0e9, 2.0e9).round();
                        #[allow(clippy::as_conversions)] // In range after the clamp.
                        write_varint(out, v as i32);
                    }
                } else {
                    let v = r.varint()?;
                    if to >= V1_21_4 {
                        #[allow(clippy::as_conversions)] // Lossless for model numbers.
                        let f = v as f32;
                        out.push(1);
                        out.extend_from_slice(&f.to_be_bytes());
                        out.extend_from_slice(&[0, 0, 0]);
                    } else {
                        write_varint(out, v);
                    }
                }
            }
            "container" | "bundle_contents" if self.depth >= MAX_NESTING => {
                return Err(Error::Invalid("item nesting"));
            }
            "container" => {
                let nested = Items {
                    depth: self.depth.saturating_add(1),
                    ..*self
                };
                // Optional stacks: templates with a presence flag from 26.1 on.
                let n = io::len(r, 256, 1)?;
                io::write_len(out, n);
                for _ in 0..n {
                    let present = if from >= V26_1 {
                        r.u8()? != 0
                    } else {
                        let mut peek = r.clone();
                        let present = peek.varint()? > 0;
                        if !present {
                            r.varint()?;
                        }
                        present
                    };
                    if !present {
                        if to >= V26_1 {
                            out.push(0);
                        } else {
                            write_varint(out, 0);
                        }
                        continue;
                    }
                    if to >= V26_1 {
                        out.push(1);
                    }
                    nested.particle_item(r, out)?;
                }
            }
            "bundle_contents" => {
                let nested = Items {
                    depth: self.depth.saturating_add(1),
                    ..*self
                };
                let n = io::len(r, 1024, 3)?;
                io::write_len(out, n);
                for _ in 0..n {
                    nested.particle_item(r, out)?;
                }
            }
            "writable_book_content" => {
                let n = io::len(r, 1024, 2)?;
                io::write_len(out, n);
                for _ in 0..n {
                    io::string(r, out)?;
                    if io::boolean(r, out)? {
                        io::string(r, out)?;
                    }
                }
            }
            "written_book_content" => {
                // Title (filterable), author, generation, pages (filterable components),
                // resolved.
                io::string(r, out)?;
                if io::boolean(r, out)? {
                    io::string(r, out)?;
                }
                io::string(r, out)?;
                io::varint(r, out)?;
                let n = io::len(r, 1024, 2)?;
                io::write_len(out, n);
                // Pages may hold click events (1.21.5 changed their shape).
                let old_events = self.clientbound
                    && self.t.protocol < crate::step::V1_21_5
                    && self.t.next_protocol >= crate::step::V1_21_5;
                for _ in 0..n {
                    io::component(r, out, old_events)?;
                    if io::boolean(r, out)? {
                        io::component(r, out, old_events)?;
                    }
                }
                io::bytes(r, out, 1)?;
            }
            "trim" => {
                // Material and pattern holders: registry id + 1, or 0 and an inline value.
                let mut ok = true;
                for registry in ["minecraft:trim_material", "minecraft:trim_pattern"] {
                    let id = r.varint()?;
                    if id == 0 {
                        return Err(Error::Invalid("inline trim"));
                    }
                    match self.dynamic(registry, id.saturating_sub(1)) {
                        Some(old) => write_varint(out, old.saturating_add(1)),
                        None => ok = false,
                    }
                }
                let show = read_show(r)?;
                write_show(out, show);
                return Ok(ok);
            }
            _ => return Err(Error::Invalid("data component")),
        }
        Ok(true)
    }
}

/// `MobEffectInstance.Details`: amplifier, duration, ambient, particles, icon, and optionally
/// the hidden effect underneath (recursive; the same in every version).
fn effect_details(r: &mut Reader<'_>, out: &mut Vec<u8>, depth: u8) -> Result<()> {
    if depth > 16 {
        return Err(Error::Invalid("effect details too deep"));
    }
    io::varint(r, out)?;
    io::varint(r, out)?;
    io::bytes(r, out, 3)?;
    if io::boolean(r, out)? {
        effect_details(r, out, depth.saturating_add(1))?;
    }
    Ok(())
}

/// Skips a stack (`optional`: may be empty) in the codec of protocol `proto`, with portable
/// components named by `names`.
fn skip_stack(
    r: &mut Reader<'_>,
    proto: i32,
    names: &[&str],
    optional: bool,
    depth: u8,
) -> Result<()> {
    if proto >= V26_1 {
        if optional && r.u8()? == 0 {
            return Ok(());
        }
        r.varint()?;
        r.varint()?;
    } else {
        if r.varint()? <= 0 {
            return Ok(());
        }
        r.varint()?;
    }
    let added = io::len(r, 256, 1)?;
    let removed = io::len(r, 256, 1)?;
    for _ in 0..added {
        let t = r.varint()?;
        let name = names
            .get(usize::try_from(t).unwrap_or(usize::MAX))
            .ok_or(Error::Invalid("component type"))?;
        skip_component(name, r, proto, names, depth)?;
    }
    for _ in 0..removed {
        r.varint()?;
    }
    Ok(())
}

/// Skips one portable component value in the codec of protocol `proto` (nested component
/// types named by `names`).
fn skip_component(
    name: &str,
    r: &mut Reader<'_>,
    proto: i32,
    names: &[&str],
    depth: u8,
) -> Result<()> {
    let depth = depth.checked_sub(1).ok_or(Error::Invalid("item nesting"))?;
    let mut sink = Vec::new();
    let show = usize::from(proto < V1_21_5);
    match name.trim_start_matches("minecraft:") {
        "max_stack_size" | "max_damage" | "damage" | "rarity" | "repair_cost" | "map_id"
        | "base_color" => {
            r.varint()?;
        }
        "enchantment_glint_override" => r.skip(1)?,
        "custom_name" | "item_name" => io::nbt(r, &mut sink)?,
        "lore" => {
            for _ in 0..io::len(r, 256, 1)? {
                io::nbt(r, &mut sink)?;
            }
        }
        "unbreakable" => r.skip(show)?,
        "dyed_color" => r.skip(4usize.saturating_add(show))?,
        "enchantments" | "stored_enchantments" => {
            for _ in 0..io::len(r, 1024, 2)? {
                r.varint()?;
                r.varint()?;
            }
            r.skip(show)?;
        }
        "potion_contents" => {
            if r.u8()? != 0 {
                r.varint()?;
            }
            if r.u8()? != 0 {
                r.skip(4)?;
            }
            for _ in 0..io::len(r, 256, 2)? {
                r.varint()?;
                effect_details(r, &mut sink, 0)?;
            }
            if proto >= V1_21_2 && r.u8()? != 0 {
                r.string(io::MAX_STRING)?;
            }
        }
        "custom_model_data" => {
            if proto >= V1_21_4 {
                let n = io::len(r, 1024, 4)?;
                r.skip(n.saturating_mul(4))?;
                let n = io::len(r, 1024, 1)?;
                r.skip(n)?;
                for _ in 0..io::len(r, 1024, 1)? {
                    r.string(io::MAX_STRING)?;
                }
                let n = io::len(r, 1024, 4)?;
                r.skip(n.saturating_mul(4))?;
            } else {
                r.varint()?;
            }
        }
        "trim" => {
            r.varint()?;
            r.varint()?;
            r.skip(show)?;
        }
        "container" => {
            for _ in 0..io::len(r, 256, 1)? {
                skip_stack(r, proto, names, true, depth)?;
            }
        }
        "bundle_contents" => {
            for _ in 0..io::len(r, 1024, 3)? {
                skip_stack(r, proto, names, false, depth)?;
            }
        }
        "writable_book_content" => {
            for _ in 0..io::len(r, 1024, 2)? {
                r.string(io::MAX_STRING)?;
                if r.u8()? != 0 {
                    r.string(io::MAX_STRING)?;
                }
            }
        }
        "written_book_content" => {
            r.string(io::MAX_STRING)?;
            if r.u8()? != 0 {
                r.string(io::MAX_STRING)?;
            }
            r.string(io::MAX_STRING)?;
            r.varint()?;
            for _ in 0..io::len(r, 1024, 2)? {
                io::nbt(r, &mut sink)?;
                if r.u8()? != 0 {
                    io::nbt(r, &mut sink)?;
                }
            }
            r.skip(1)?;
        }
        _ => return Err(Error::Invalid("data component")),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn portable_is_sorted() {
        assert!(super::PORTABLE.windows(2).all(|w| w.first() < w.last()));
    }
}
