//! Translations every step needs in the same way, driven by the step's [`Tables`]: the
//! synchronised registries and tags of the configuration, sounds, statistics, the command tree.

use crate::Result;
use crate::chunk::{self, Kind};
use crate::io;
use crate::table::{Remap, Tables, similar};
use vanillite_protocol::Error;
use vanillite_protocol::codec::{Reader, write_str, write_varint};

/// A synchronised registry as the client knows it: newer index → older index.
#[derive(Debug)]
pub(crate) struct Dynamic {
    pub registry: String,
    pub to_old: Vec<i32>,
    pub old_len: usize,
}

/// The synchronised registries one connection was sent (one step's view).
#[derive(Debug, Default)]
pub(crate) struct Registries {
    pub dynamic: Vec<Dynamic>,
    /// Registries whose data the older version does not synchronise, with their entry names.
    pub dropped: Vec<(String, Vec<String>)>,
}

impl Registries {
    pub(crate) fn get(&self, registry: &str) -> Option<&Dynamic> {
        self.dynamic.iter().find(|d| d.registry == registry)
    }

    /// The entry names of a registry the older version does not synchronise.
    pub(crate) fn dropped(&self, registry: &str) -> Option<&[String]> {
        self.dropped
            .iter()
            .find(|d| d.0 == registry)
            .map(|d| d.1.as_slice())
    }

    /// The older index of a newer index of a synchronised registry (the same if the registry
    /// was not sent).
    pub(crate) fn older(&self, registry: &str, id: i32) -> Option<i32> {
        match self.get(registry) {
            Some(d) => d.to_old.get(usize::try_from(id).ok()?).copied(),
            None => Some(id),
        }
    }

    /// Paletted container sizes of biomes.
    pub(crate) fn biome_kind(&self) -> Kind {
        let (new, old) = self
            .get("minecraft:worldgen/biome")
            .map_or((64, 64), |d| (d.to_old.len(), d.old_len));
        Kind {
            entries: 64,
            max_indirect: 3,
            global_new: chunk::global_bits(new),
            global_old: chunk::global_bits(old),
        }
    }

    /// Newer biome id → older biome id.
    pub(crate) fn biome_map(&self) -> impl Fn(i32) -> i32 + '_ {
        let d = self.get("minecraft:worldgen/biome");
        move |id| match d {
            Some(d) => usize::try_from(id)
                .ok()
                .and_then(|i| d.to_old.get(i))
                .copied()
                .unwrap_or(0),
            None => id,
        }
    }

    /// `registry_data`: registries the older version does not synchronise are dropped; entries
    /// it does not have built in are left out unless they come with their data. Later ids follow.
    pub(crate) fn registry_data(
        &mut self,
        t: &Tables,
        r: &mut Reader<'_>,
        out: &mut Vec<u8>,
    ) -> Result<bool> {
        let registry = r.string(io::MAX_STRING)?.to_owned();
        let n = io::len(r, 1 << 16, 2)?;
        let known = t.synchronized.iter().find(|(n, _)| *n == registry);
        let mut entries = Vec::new();
        let mut names = Vec::with_capacity(n);
        let mut kept = Vec::with_capacity(n);
        for _ in 0..n {
            let name = r.string(io::MAX_STRING)?.to_owned();
            let start = r.rest();
            let has_data = r.u8()? != 0;
            if has_data {
                let mut skip = Vec::new();
                io::nbt(r, &mut skip)?;
            }
            let len = start.len().saturating_sub(r.remaining());
            let keep =
                known.is_some_and(|(_, k)| has_data || k.binary_search(&name.as_str()).is_ok());
            if keep {
                write_str(&mut entries, &name);
                entries.extend_from_slice(start.get(..len).ok_or(Error::UnexpectedEnd)?);
            }
            kept.push(keep);
            names.push(name);
        }
        if known.is_none() {
            self.dropped.retain(|d| d.0 != registry);
            self.dropped.push((registry, names));
            return Ok(false);
        }
        let kept_names: Vec<&str> = names
            .iter()
            .zip(&kept)
            .filter(|(_, k)| **k)
            .map(|(n, _)| n.as_str())
            .collect();
        let mut to_old = Vec::with_capacity(n);
        let mut next = 0i32;
        for (name, keep) in names.iter().zip(&kept) {
            if *keep {
                to_old.push(next);
                next = next.saturating_add(1);
            } else {
                let fallback = similar(name, kept_names.iter().copied())
                    .and_then(|s| kept_names.iter().position(|k| *k == s))
                    .and_then(|i| i32::try_from(i).ok())
                    .unwrap_or(0);
                to_old.push(fallback);
            }
        }
        write_str(out, &registry);
        io::write_len(out, kept_names.len());
        out.extend_from_slice(&entries);
        let old_len = kept_names.len();
        self.dynamic.retain(|d| d.registry != registry);
        self.dynamic.push(Dynamic {
            registry,
            to_old,
            old_len,
        });
        Ok(true)
    }

    /// `update_tags`: ids follow the older registries; registries it lacks are left out.
    pub(crate) fn tags(&self, t: &Tables, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
        let n = io::len(r, 1024, 2)?;
        let mut body = Vec::new();
        let mut kept = 0usize;
        for _ in 0..n {
            let registry = r.string(io::MAX_STRING)?.to_owned();
            let remap = (t.remap)(&registry);
            let dynamic = self.get(&registry);
            // A client fails on tags of registries it does not know.
            let known = self.dropped(&registry).is_none()
                && (t.static_registries.contains(&registry.as_str())
                    || t.synchronized.iter().any(|(n, _)| *n == registry));
            let tags = io::len(r, 1 << 16, 2)?;
            let mut list = Vec::new();
            for _ in 0..tags {
                io::string(r, &mut list)?;
                let ids = io::len(r, 1 << 20, 1)?;
                let mut mapped = Vec::with_capacity(ids);
                for _ in 0..ids {
                    let id = r.varint()?;
                    let old = match (remap, dynamic) {
                        (Some(m), _) => m.older(id),
                        (None, Some(d)) => usize::try_from(id)
                            .ok()
                            .and_then(|i| d.to_old.get(i).copied()),
                        (None, None) => Some(id),
                    };
                    // Entries the older version lacks leave the tag (fallbacks would add
                    // unrelated entries to it).
                    if let Some(old) = old
                        && !is_fallback(remap, dynamic, id)
                    {
                        mapped.push(old);
                    }
                }
                io::write_len(&mut list, mapped.len());
                for id in mapped {
                    write_varint(&mut list, id);
                }
            }
            if known {
                write_str(&mut body, &registry);
                io::write_len(&mut body, tags);
                body.extend_from_slice(&list);
                kept = kept.saturating_add(1);
            }
        }
        io::write_len(out, kept);
        out.extend_from_slice(&body);
        Ok(())
    }
}

/// Whether `id` (newer) only maps to a fallback in the older version.
pub(crate) fn is_fallback(remap: Option<&Remap>, dynamic: Option<&Dynamic>, id: i32) -> bool {
    match (remap, dynamic) {
        (Some(m), _) => m
            .older(id)
            .and_then(|old| m.newer(old))
            .is_none_or(|back| back != id),
        (None, Some(d)) => {
            // Entries kept by name keep their order; a fallback repeats an earlier index.
            let Ok(i) = usize::try_from(id) else {
                return true;
            };
            let old = d.to_old.get(i).copied().unwrap_or(-1);
            d.to_old.iter().take(i).any(|&o| o >= old)
        }
        (None, None) => false,
    }
}

/// `select_known_packs`: `minecraft:core` of a release of protocol `from` becomes the core
/// pack of every release of protocol `to` (a client answers with the one it is; releases of one
/// protocol share their data).
pub(crate) fn known_packs(r: &mut Reader<'_>, out: &mut Vec<u8>, from: i32, to: i32) -> Result<()> {
    let n = io::len(r, 64, 3)?;
    let mut body = Vec::new();
    let mut count = 0usize;
    let mut core_done = false;
    for _ in 0..n {
        let namespace = r.string(io::MAX_STRING)?;
        let id = r.string(io::MAX_STRING)?;
        let version = r.string(io::MAX_STRING)?;
        let core =
            namespace == "minecraft" && id == "core" && crate::names(from).contains(&version);
        if !core {
            write_str(&mut body, namespace);
            write_str(&mut body, id);
            write_str(&mut body, version);
            count = count.saturating_add(1);
            continue;
        }
        if core_done {
            continue;
        }
        core_done = true;
        for name in crate::names(to) {
            write_str(&mut body, namespace);
            write_str(&mut body, id);
            write_str(&mut body, name);
            count = count.saturating_add(1);
        }
    }
    io::write_len(out, count);
    out.extend_from_slice(&body);
    Ok(())
}

/// `SoundEvent` holder: newer registry ids become older ids, or the event by name.
pub(crate) fn sound(t: &Tables, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
    let id = r.varint()?;
    if id == 0 {
        write_varint(out, 0);
        io::string(r, out)?;
        if io::boolean(r, out)? {
            io::bytes(r, out, 4)?;
        }
        return Ok(());
    }
    let new = id.saturating_sub(1);
    match t.older("minecraft:sound_event", new) {
        Some(old) => write_varint(out, old.saturating_add(1)),
        None => {
            let name = t
                .sound_added
                .iter()
                .find(|(i, _)| i32::from(*i) == new)
                .map_or("minecraft:intentionally_empty", |(_, n)| *n);
            write_varint(out, 0);
            write_str(out, name);
            out.push(0);
        }
    }
    Ok(())
}

/// `award_stats`: (stat type, value id, amount); values of changed registries are rewritten,
/// stats the older version lacks are left out.
pub(crate) fn award_stats(t: &Tables, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
    let n = io::len(r, 1 << 16, 3)?;
    let mut body = Vec::new();
    let mut kept = 0usize;
    for _ in 0..n {
        let kind = r.varint()?;
        let value = r.varint()?;
        let amount = r.varint()?;
        let registry = match t
            .new_stat_types
            .get(usize::try_from(kind).unwrap_or(usize::MAX))
        {
            Some(&"minecraft:custom") => "minecraft:custom_stat",
            Some(&"minecraft:mined") => "minecraft:block",
            Some(&"minecraft:killed" | &"minecraft:killed_by") => "minecraft:entity_type",
            Some(_) => "minecraft:item",
            None => return Err(Error::Invalid("stat type")),
        };
        let old = match (t.remap)(registry) {
            Some(m) if !is_fallback(Some(m), None, value) => m.older(value),
            Some(_) => None,
            None => Some(value),
        };
        if let Some(old) = old {
            write_varint(&mut body, kind);
            write_varint(&mut body, old);
            write_varint(&mut body, amount);
            kept = kept.saturating_add(1);
        }
    }
    io::write_len(out, kept);
    out.extend_from_slice(&body);
    Ok(())
}

/// `commands`: argument parser ids follow the older registry; a parser the older version lacks
/// becomes a greedy string (the client still completes the literal parts of the tree). Flags
/// the older version does not know are cleared (`restricted`, 1.21.6).
pub(crate) fn commands(t: &Tables, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
    const TYPE_MASK: u8 = 3;
    const ARGUMENT: u8 = 2;
    const LITERAL: u8 = 1;
    const REDIRECT: u8 = 8;
    const CUSTOM_SUGGESTIONS: u8 = 0x10;
    const RESTRICTED: u8 = 0x20;
    let n = io::len(r, 1 << 20, 2)?;
    io::write_len(out, n);
    for _ in 0..n {
        let flags = r.u8()?;
        out.push(if t.protocol < crate::step::V1_21_6 {
            flags & !RESTRICTED
        } else {
            flags
        });
        let children = io::len(r, 1 << 20, 1)?;
        io::write_len(out, children);
        for _ in 0..children {
            io::varint(r, out)?;
        }
        if flags & REDIRECT != 0 {
            io::varint(r, out)?;
        }
        match flags & TYPE_MASK {
            LITERAL => io::string(r, out)?,
            ARGUMENT => {
                io::string(r, out)?;
                let id = r.varint()?;
                let name = usize::try_from(id)
                    .ok()
                    .and_then(|i| t.new_command_argument_types.get(i))
                    .copied()
                    .ok_or(Error::Invalid("argument type"))?;
                let mut props = Vec::new();
                parser_properties(name, r, &mut props)?;
                match t.older("minecraft:command_argument_type", id) {
                    Some(old) => {
                        write_varint(out, old);
                        out.extend_from_slice(&props);
                    }
                    None => {
                        write_varint(out, t.old_string_parser);
                        write_varint(out, 2);
                    }
                }
                if flags & CUSTOM_SUGGESTIONS != 0 {
                    io::string(r, out)?;
                }
            }
            _ => {}
        }
    }
    io::varint(r, out)?;
    Ok(())
}

/// Copies the properties of an argument parser (`ArgumentTypeInfos`, the same since 1.20.5).
fn parser_properties(name: &str, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
    let bounds = |r: &mut Reader<'_>, out: &mut Vec<u8>, size: usize| -> Result<()> {
        let f = r.u8()?;
        out.push(f);
        let n = usize::from(f & 1).saturating_add(usize::from((f >> 1) & 1));
        io::bytes(r, out, size.saturating_mul(n))
    };
    match name {
        "brigadier:float" | "brigadier:integer" => bounds(r, out, 4)?,
        "brigadier:double" | "brigadier:long" => bounds(r, out, 8)?,
        "brigadier:string" => {
            io::varint(r, out)?;
        }
        "minecraft:entity" | "minecraft:score_holder" => io::bytes(r, out, 1)?,
        "minecraft:time" => io::bytes(r, out, 4)?,
        "minecraft:resource_or_tag"
        | "minecraft:resource_or_tag_key"
        | "minecraft:resource"
        | "minecraft:resource_key"
        | "minecraft:resource_selector" => io::string(r, out)?,
        _ => {}
    }
    Ok(())
}

/// Copies an `LpVec3`: 1 byte for zero, else 6 bytes and a VarInt scale extension when bit 2
/// is set.
pub(crate) fn lp_vec3(r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
    let lowest = r.u8()?;
    out.push(lowest);
    if lowest == 0 {
        return Ok(());
    }
    io::bytes(r, out, 5)?;
    if lowest & 4 != 0 {
        io::varint(r, out)?;
    }
    Ok(())
}

/// Reads an `LpVec3` (1.21.9): three 15-bit components scaled by a common factor.
pub(crate) fn read_lp_vec3(r: &mut Reader<'_>) -> Result<[f64; 3]> {
    let b0 = r.u8()?;
    if b0 == 0 {
        return Ok([0.0; 3]);
    }
    let b1 = r.u8()?;
    let rest = u32::from_ne_bytes(r.i32()?.to_ne_bytes());
    let packed = (u64::from(rest) << 16) | (u64::from(b1) << 8) | u64::from(b0);
    let mut scale = u64::from(b0 & 3);
    if b0 & 4 != 0 {
        let ext = u64::from(u32::from_ne_bytes(r.varint()?.to_ne_bytes()));
        scale |= ext << 2;
    }
    let unpack = |v: u64| -> f64 {
        let q = f64::from(u16::try_from(v & 0x7fff).unwrap_or(0)).min(32766.0);
        q * 2.0 / 32766.0 - 1.0
    };
    let s = f64::from(u32::try_from(scale).unwrap_or(u32::MAX));
    Ok([
        unpack(packed >> 3) * s,
        unpack(packed >> 18) * s,
        unpack(packed >> 33) * s,
    ])
}

/// Writes a velocity as the three shorts of 1/8000 blocks per tick used before 1.21.9.
pub(crate) fn write_velocity_shorts(out: &mut Vec<u8>, v: [f64; 3]) {
    for c in v {
        let s = (c.clamp(-3.9, 3.9) * 8000.0).round();
        // In range after the clamp.
        let s = s.clamp(f64::from(i16::MIN), f64::from(i16::MAX));
        #[allow(clippy::as_conversions)]
        out.extend_from_slice(&(s as i16).to_be_bytes());
    }
}
