//! Minimal wire primitives (big-endian, Minecraft VarInt) for chunk encoding and decoding.

use std::fmt;

/// Error while decoding chunk data from untrusted bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// The input ended early.
    UnexpectedEnd,
    /// A VarInt was longer than five bytes.
    VarIntTooLong,
    /// Bits per entry not valid for the container.
    InvalidBits(u8),
    /// Palette length zero or above the capacity of its bit count.
    InvalidPaletteLength(u32),
    /// A palette entry or direct value outside the global palette.
    InvalidValue(u32),
    /// A packed index points past the palette.
    InvalidIndex(u32),
    /// The data array has the wrong number of words.
    InvalidDataLength(usize),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedEnd => f.write_str("unexpected end of chunk data"),
            Self::VarIntTooLong => f.write_str("VarInt longer than 5 bytes"),
            Self::InvalidBits(bits) => write!(f, "invalid bits per entry {bits}"),
            Self::InvalidPaletteLength(len) => write!(f, "invalid palette length {len}"),
            Self::InvalidValue(value) => write!(f, "value {value} outside the global palette"),
            Self::InvalidIndex(index) => write!(f, "palette index {index} out of range"),
            Self::InvalidDataLength(len) => write!(f, "invalid data length {len}"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Bytes a VarInt of `value` takes.
#[inline]
pub const fn varint_len(value: u32) -> usize {
    match value {
        0..0x80 => 1,
        0x80..0x4000 => 2,
        0x4000..0x20_0000 => 3,
        0x20_0000..0x1000_0000 => 4,
        _ => 5,
    }
}

/// Appends a VarInt (the bits of a Java `int`).
#[inline]
pub fn write_varint(out: &mut Vec<u8>, mut value: u32) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

/// Reads a VarInt.
pub fn read_varint(input: &mut &[u8]) -> Result<u32, DecodeError> {
    let mut value = 0u32;
    for i in 0..5 {
        let byte = read_u8(input)?;
        value |= u32::from(byte & 0x7f) << (7 * i);
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(DecodeError::VarIntTooLong)
}

/// Reads one byte.
#[inline]
pub fn read_u8(input: &mut &[u8]) -> Result<u8, DecodeError> {
    let (&first, rest) = input.split_first().ok_or(DecodeError::UnexpectedEnd)?;
    *input = rest;
    Ok(first)
}

/// Reads `N` bytes.
#[inline]
pub fn read_array<const N: usize>(input: &mut &[u8]) -> Result<[u8; N], DecodeError> {
    let (head, rest) = input
        .split_first_chunk::<N>()
        .ok_or(DecodeError::UnexpectedEnd)?;
    *input = rest;
    Ok(*head)
}

/// Appends a Java `BitSet` as written by `FriendlyByteBuf.writeBitSet` (trailing zero words dropped).
pub fn write_bitset(out: &mut Vec<u8>, words: &[u64]) {
    let len = words
        .iter()
        .rposition(|&w| w != 0)
        .map_or(0, |last| last + 1);
    write_varint(out, len as u32);
    for word in &words[..len] {
        out.extend_from_slice(&word.to_be_bytes());
    }
}
