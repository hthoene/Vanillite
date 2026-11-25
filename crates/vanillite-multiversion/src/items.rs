//! Item stacks and data component values between two versions whose component codecs agree:
//! registry ids inside values (items, blocks, sounds, entity types, component types of nested
//! patches, …) are rewritten, components the older version does not know are dropped.

use crate::Result;
use crate::io;
use crate::table::Remap;
use vanillite_protocol::Error;
use vanillite_protocol::codec::{Reader, write_varint};
use vanillite_protocol::components::{Ref, registry_refs};

/// The id tables of one step that values refer to.
pub(crate) trait Maps {
    /// The table of a static registry whose ids differ; `None` if they are the same.
    fn registry(&self, name: &str) -> Option<&'static Remap>;

    /// The older id of a (newer) component type; by name unless a step renamed it.
    fn component(&self, t: u16) -> Option<u16> {
        match self.registry("minecraft:data_component_type") {
            Some(remap) => remap
                .older(i32::from(t))
                .and_then(|t| u16::try_from(t).ok()),
            None => Some(t),
        }
    }

    /// Writes the older form of a component value whose codec changed; `None` if it did not.
    fn convert(&self, t: u16, value: &[u8], out: &mut Vec<u8>) -> Option<Result<()>> {
        let _ = (t, value, out);
        None
    }
}

fn to_old(m: &dyn Maps, registry: &str, id: i32) -> Option<i32> {
    match m.registry(registry) {
        Some(remap) => remap.older(id),
        None => Some(id),
    }
}

fn component_to_old(m: &dyn Maps, t: u16) -> Option<u16> {
    m.component(t)
}

/// Copies `data[from..to]` with the references `refs[i..]` that start inside it rewritten to the
/// older version. Returns the index of the first reference after `to`.
fn emit(
    data: &[u8],
    refs: &[Ref],
    mut i: usize,
    from: usize,
    to: usize,
    out: &mut Vec<u8>,
    m: &dyn Maps,
) -> Result<usize> {
    let slice = |a: usize, b: usize| data.get(a..b).ok_or(Error::UnexpectedEnd);
    let mut pos = from;
    while let Some(r) = refs.get(i) {
        let start = ref_start(r);
        if start >= to {
            break;
        }
        out.extend_from_slice(slice(pos, start)?);
        match *r {
            Ref::Id {
                registry,
                end,
                id,
                plus_one,
                ..
            } => {
                let raw = if plus_one { id.saturating_sub(1) } else { id };
                // An id without counterpart becomes the registry's first entry: the value
                // still decodes, it just names something else.
                let new = to_old(m, registry, raw).unwrap_or(0);
                write_varint(out, if plus_one { new.saturating_add(1) } else { new });
                pos = end;
                i = i.saturating_add(1);
            }
            Ref::Patch {
                removed: (_, counts_end),
                end_event,
                ..
            } => {
                // Direct children: components (with their nested references) and removals.
                let mut children = Vec::new();
                let mut j = i.saturating_add(1);
                while j < end_event {
                    match refs.get(j) {
                        Some(Ref::Component { end, .. }) => {
                            let end = *end;
                            children.push(j);
                            j = j.saturating_add(1);
                            while j < end_event && refs.get(j).is_some_and(|r| ref_start(r) < end) {
                                j = j.saturating_add(1);
                            }
                        }
                        Some(Ref::Removed { .. }) => {
                            children.push(j);
                            j = j.saturating_add(1);
                        }
                        _ => j = j.saturating_add(1),
                    }
                }
                let kept = |want_component: bool| {
                    children
                        .iter()
                        .filter(|&&c| match refs.get(c) {
                            Some(Ref::Component { type_id, .. }) => {
                                want_component && component_to_old(m, *type_id).is_some()
                            }
                            Some(Ref::Removed { type_id, .. }) => {
                                !want_component && component_to_old(m, *type_id).is_some()
                            }
                            _ => false,
                        })
                        .count()
                };
                io::write_len(out, kept(true));
                io::write_len(out, kept(false));
                let mut end_pos = counts_end;
                for &c in &children {
                    match refs.get(c) {
                        Some(&Ref::Component {
                            type_id,
                            type_end,
                            end,
                            ..
                        }) => {
                            if let Some(t) = component_to_old(m, type_id) {
                                write_varint(out, i32::from(t));
                                let value = data.get(type_end..end).ok_or(Error::UnexpectedEnd)?;
                                match m.convert(type_id, value, out) {
                                    Some(done) => done?,
                                    None => {
                                        emit(
                                            data,
                                            refs,
                                            c.saturating_add(1),
                                            type_end,
                                            end,
                                            out,
                                            m,
                                        )?;
                                    }
                                }
                            }
                            end_pos = end;
                        }
                        Some(&Ref::Removed { type_id, end, .. }) => {
                            if let Some(t) = component_to_old(m, type_id) {
                                write_varint(out, i32::from(t));
                            }
                            end_pos = end;
                        }
                        _ => {}
                    }
                }
                pos = end_pos;
                i = end_event;
            }
            // Components and removals are handled with their patch.
            Ref::Component { end, .. } | Ref::Removed { end, .. } => {
                pos = end;
                i = i.saturating_add(1);
            }
        }
    }
    out.extend_from_slice(slice(pos, to)?);
    Ok(i)
}

fn ref_start(r: &Ref) -> usize {
    match *r {
        Ref::Id { start, .. } | Ref::Component { start, .. } | Ref::Removed { start, .. } => start,
        Ref::Patch { added, .. } => added.0,
    }
}

/// Rewrites one component value of (newer) type `t` at the start of `r` into `out`.
fn value_to_old(
    t: u16,
    r: &mut Reader<'_>,
    out: &mut Vec<u8>,
    m: &dyn Maps,
    refs: &mut Vec<Ref>,
) -> Result<()> {
    refs.clear();
    let data = r.rest();
    let len = registry_refs(t, data, refs)?;
    match m.convert(t, data.get(..len).ok_or(Error::UnexpectedEnd)?, out) {
        Some(done) => done?,
        None => {
            emit(data, refs, 0, 0, len, out, m)?;
        }
    }
    r.skip(len)
}

/// `DataComponentExactPredicate` (merchant costs): a list of components with their values.
pub(crate) fn exact_to_old(
    r: &mut Reader<'_>,
    out: &mut Vec<u8>,
    m: &dyn Maps,
    refs: &mut Vec<Ref>,
) -> Result<()> {
    let n = io::len(r, 256, 1)?;
    let mut values = Vec::new();
    let mut kept = 0usize;
    for _ in 0..n {
        let t = component_type(r)?;
        let mut v = Vec::new();
        value_to_old(t, r, &mut v, m, refs)?;
        if let Some(old) = component_to_old(m, t) {
            write_varint(&mut values, i32::from(old));
            values.extend_from_slice(&v);
            kept = kept.saturating_add(1);
        }
    }
    io::write_len(out, kept);
    out.extend_from_slice(&values);
    Ok(())
}

/// Rewrites a component patch (trusted codec) to the older version.
pub(crate) fn patch_to_old(
    r: &mut Reader<'_>,
    out: &mut Vec<u8>,
    m: &dyn Maps,
    refs: &mut Vec<Ref>,
) -> Result<()> {
    let added = io::len(r, 256, 1)?;
    let removed = io::len(r, 256, 1)?;
    let mut values = Vec::new();
    let mut kept = 0usize;
    for _ in 0..added {
        let t = component_type(r)?;
        let mut v = Vec::new();
        let old = component_to_old(m, t);
        value_to_old(t, r, &mut v, m, refs)?;
        if let Some(old) = old {
            write_varint(&mut values, i32::from(old));
            values.extend_from_slice(&v);
            kept = kept.saturating_add(1);
        }
    }
    let mut removals = Vec::new();
    let mut kept_removed = 0usize;
    for _ in 0..removed {
        if let Some(old) = component_to_old(m, component_type(r)?) {
            write_varint(&mut removals, i32::from(old));
            kept_removed = kept_removed.saturating_add(1);
        }
    }
    io::write_len(out, kept);
    io::write_len(out, kept_removed);
    out.extend_from_slice(&values);
    out.extend_from_slice(&removals);
    Ok(())
}

fn component_type(r: &mut Reader<'_>) -> Result<u16> {
    u16::try_from(r.varint()?).map_err(|_| Error::Invalid("component type"))
}

/// `ItemStack.OPTIONAL_STREAM_CODEC` (trusted): count, then item and patch unless empty.
pub(crate) fn stack_to_old(
    r: &mut Reader<'_>,
    out: &mut Vec<u8>,
    m: &dyn Maps,
    refs: &mut Vec<Ref>,
) -> Result<()> {
    let count = io::varint(r, out)?;
    if count <= 0 {
        return Ok(());
    }
    template_rest_to_old(r, out, m, refs)
}

/// `ItemStackTemplate.STREAM_CODEC` (item, count, patch).
pub(crate) fn template_to_old(
    r: &mut Reader<'_>,
    out: &mut Vec<u8>,
    m: &dyn Maps,
    refs: &mut Vec<Ref>,
) -> Result<()> {
    let item = r.varint()?;
    write_varint(out, to_old(m, "minecraft:item", item).unwrap_or(1));
    io::varint(r, out)?;
    patch_to_old(r, out, m, refs)
}

fn template_rest_to_old(
    r: &mut Reader<'_>,
    out: &mut Vec<u8>,
    m: &dyn Maps,
    refs: &mut Vec<Ref>,
) -> Result<()> {
    let item = r.varint()?;
    write_varint(out, to_old(m, "minecraft:item", item).unwrap_or(1));
    patch_to_old(r, out, m, refs)
}

/// `ItemStack.OPTIONAL_UNTRUSTED_STREAM_CODEC` from an older client: count, item and a patch
/// with length-prefixed values. Item and component type ids are rewritten, components the
/// newer version does not know are dropped. Values are copied (their codecs agree).
pub(crate) fn untrusted_stack_to_new(
    r: &mut Reader<'_>,
    out: &mut Vec<u8>,
    items: &Remap,
    components: &dyn Fn(i32) -> Option<i32>,
) -> Result<()> {
    let count = io::varint(r, out)?;
    if count <= 0 {
        return Ok(());
    }
    write_varint(out, io::id_to_new(r, items)?);
    let added = io::len(r, 256, 2)?;
    let removed = io::len(r, 256, 1)?;
    let mut values = Vec::new();
    let mut kept = 0usize;
    for _ in 0..added {
        let t = r.varint()?;
        let len = io::len(r, 1 << 20, 1)?;
        let v = r.take(len)?;
        if let Some(t) = components(t) {
            write_varint(&mut values, t);
            io::write_len(&mut values, len);
            values.extend_from_slice(v);
            kept = kept.saturating_add(1);
        }
    }
    let mut removals = Vec::new();
    let mut kept_removed = 0usize;
    for _ in 0..removed {
        if let Some(t) = components(r.varint()?) {
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
