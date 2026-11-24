//! Java's modified UTF-8 (`DataInput.readUTF`/`DataOutput.writeUTF`): NUL is written as
//! `C0 80` and characters outside the BMP as two 3-byte surrogates.

use crate::{Error, Result};
use std::borrow::Cow;

/// Decodes modified UTF-8. Plain UTF-8 without NUL bytes and without 4-byte sequences is the same
/// in both encodings and takes the fast path. Lone surrogates become U+FFFD.
pub fn decode(bytes: &[u8]) -> Result<String> {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return Ok(s.to_owned());
    }
    let mut units = Vec::with_capacity(bytes.len());
    let mut iter = bytes.iter().copied();
    while let Some(a) = iter.next() {
        let unit = match a {
            0x00..=0x7F => u16::from(a),
            0xC0..=0xDF => {
                let b = cont(iter.next())?;
                (u16::from(a & 0x1F) << 6) | b
            }
            0xE0..=0xEF => {
                let b = cont(iter.next())?;
                let c = cont(iter.next())?;
                (u16::from(a & 0x0F) << 12) | (b << 6) | c
            }
            _ => return Err(Error::InvalidString),
        };
        units.push(unit);
    }
    Ok(String::from_utf16_lossy(&units))
}

fn cont(byte: Option<u8>) -> Result<u16> {
    match byte {
        Some(b) if b & 0xC0 == 0x80 => Ok(u16::from(b & 0x3F)),
        _ => Err(Error::InvalidString),
    }
}

/// Encodes as modified UTF-8. Borrows when the string is already valid as is.
pub fn encode(s: &str) -> Cow<'_, [u8]> {
    if !s.bytes().any(|b| b == 0 || b >= 0xF0) {
        return Cow::Borrowed(s.as_bytes());
    }
    let mut out = Vec::with_capacity(s.len().saturating_add(8));
    for unit in s.encode_utf16() {
        let [hi, lo] = unit.to_be_bytes();
        match unit {
            0x0001..=0x007F => out.push(lo),
            0x0000 | 0x0080..=0x07FF => {
                out.push(0xC0 | ((hi & 0x07) << 2) | (lo >> 6));
                out.push(0x80 | (lo & 0x3F));
            }
            _ => {
                out.push(0xE0 | (hi >> 4));
                out.push(0x80 | ((hi & 0x0F) << 2) | (lo >> 6));
                out.push(0x80 | (lo & 0x3F));
            }
        }
    }
    Cow::Owned(out)
}

/// Length of the modified UTF-8 encoding without encoding.
pub fn encoded_len(s: &str) -> usize {
    s.encode_utf16()
        .map(|u| match u {
            0x0001..=0x007F => 1usize,
            0x0000 | 0x0080..=0x07FF => 2,
            _ => 3,
        })
        .fold(0usize, usize::saturating_add)
}
