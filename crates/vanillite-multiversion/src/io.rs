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
