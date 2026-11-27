//! Helpers for rewriting packet fields: copy what does not change, rewrite what does.

use crate::Result;
use crate::table::Remap;
use vanillite_protocol::Error;
use vanillite_protocol::codec::{Reader, write_varint};

/// Longest string a rewritten packet carries (Vanilla's default `readUtf` limit).
pub(crate) const MAX_STRING: usize = 32767;

/// Copies one VarInt.
pub(crate) fn varint(r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<i32> {
    let v = r.varint()?;
    write_varint(out, v);
    Ok(v)
}

/// Copies `n` bytes.
pub(crate) fn bytes(r: &mut Reader<'_>, out: &mut Vec<u8>, n: usize) -> Result<()> {
    out.extend_from_slice(r.take(n)?);
    Ok(())
}

/// Copies a string.
pub(crate) fn string(r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
    let s = r.string(MAX_STRING)?;
    vanillite_protocol::codec::write_str(out, s);
    Ok(())
}

/// Copies a bool and returns it.
pub(crate) fn boolean(r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<bool> {
    let b = r.u8()?;
    out.push(b);
    match b {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(Error::Invalid("boolean")),
    }
}

/// Copies one network NBT tag.
pub(crate) fn nbt(r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
    let mut budget = vanillite_nbt::Budget::new(
        vanillite_nbt::read::NETWORK_QUOTA,
        vanillite_nbt::read::MAX_DEPTH,
    );
    let len = vanillite_nbt::read::skip_network(r.rest(), &mut budget)?;
    bytes(r, out, len)
}

/// Rewrites a registry id VarInt from the older to the newer version.
pub(crate) fn id_to_new(r: &mut Reader<'_>, remap: &Remap) -> Result<i32> {
    let id = r.varint()?;
    remap
        .newer(id)
        .ok_or(Error::Invalid("registry id unknown to the newer version"))
}

/// A list length, bounded by the bytes left (every element takes at least `min` bytes).
pub(crate) fn len(r: &mut Reader<'_>, max: usize, min: usize) -> Result<usize> {
    r.len_prefix("list", max, min)
}

/// Writes a length.
pub(crate) fn write_len(out: &mut Vec<u8>, n: usize) {
    vanillite_protocol::codec::write_len(out, n);
}

/// Copies a text component (network NBT). With `old_events` (a client before 1.21.5) its click
/// and hover events are rewritten to the older shape, which the client reads instead of
/// dropping the whole event.
pub(crate) fn component(r: &mut Reader<'_>, out: &mut Vec<u8>, old_events: bool) -> Result<()> {
    if !old_events {
        return nbt(r, out);
    }
    let mut budget = vanillite_nbt::Budget::new(
        vanillite_nbt::read::NETWORK_QUOTA,
        vanillite_nbt::read::MAX_DEPTH,
    );
    let mut input = r.rest();
    let before = input.len();
    let tag = vanillite_nbt::read::read_network(&mut input, &mut budget)?;
    r.skip(before.saturating_sub(input.len()))?;
    match tag {
        Some(mut tag) => {
            text_events_to_old(&mut tag);
            vanillite_nbt::write::write_network(&tag, out)?;
        }
        None => out.push(0),
    }
    Ok(())
}

/// 1.21.5 renamed `clickEvent`/`hoverEvent` to `click_event`/`hover_event` and gave each action
/// its own fields; this writes them back as `{action, value}` and `{action, contents}`, in the
/// component and its children. The depth is bounded by the NBT reader's.
pub(crate) fn text_events_to_old(tag: &mut vanillite_nbt::Tag) {
    use vanillite_nbt::{Compound, Tag};
    match tag {
        Tag::List(list) => list.0.iter_mut().for_each(text_events_to_old),
        Tag::Compound(c) => {
            for key in ["extra", "with", "separator"] {
                if let Some(child) = c.get_mut(key) {
                    text_events_to_old(child);
                }
            }
            if let Some(Tag::Compound(click)) = c.remove("click_event") {
                let action = click
                    .get("action")
                    .and_then(Tag::as_str)
                    .unwrap_or_default();
                let value = match action {
                    "open_url" => click.get("url").and_then(Tag::as_str).map(str::to_owned),
                    "open_file" => click.get("path").and_then(Tag::as_str).map(str::to_owned),
                    "run_command" | "suggest_command" => click
                        .get("command")
                        .and_then(Tag::as_str)
                        .map(str::to_owned),
                    "change_page" => click
                        .get("page")
                        .and_then(Tag::as_i64)
                        .map(|p| p.to_string()),
                    "copy_to_clipboard" => {
                        click.get("value").and_then(Tag::as_str).map(str::to_owned)
                    }
                    _ => None,
                };
                if let Some(value) = value {
                    c.insert(
                        "clickEvent",
                        Compound::new()
                            .with("action", action.to_owned())
                            .with("value", value),
                    );
                }
            }
            if let Some(Tag::Compound(mut hover)) = c.remove("hover_event") {
                let action = hover
                    .get("action")
                    .and_then(Tag::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let contents = match action.as_str() {
                    "show_text" => hover.remove("value").map(|mut v| {
                        text_events_to_old(&mut v);
                        v
                    }),
                    "show_item" => {
                        hover.remove("action");
                        Some(Tag::Compound(hover))
                    }
                    "show_entity" => {
                        let mut e = Compound::new();
                        if let Some(t) = hover.remove("id") {
                            e.insert("type", t);
                        }
                        if let Some(u) = hover.remove("uuid") {
                            e.insert("id", u);
                        }
                        if let Some(mut n) = hover.remove("name") {
                            text_events_to_old(&mut n);
                            e.insert("name", n);
                        }
                        Some(Tag::Compound(e))
                    }
                    _ => None,
                };
                if let Some(contents) = contents {
                    c.insert(
                        "hoverEvent",
                        Compound::new()
                            .with("action", action)
                            .with("contents", contents),
                    );
                }
            }
        }
        _ => {}
    }
}
