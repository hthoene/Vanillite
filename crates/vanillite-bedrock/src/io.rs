//! Bedrock wire primitives: little-endian integers, LEB128 VarInts (zig-zag for signed ones) and
//! length-prefixed strings. The reader is bounded and never panics on untrusted input.

use std::fmt;

/// Error while reading untrusted Bedrock data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The input ended early.
    Eof,
    /// A VarInt was too long.
    VarInt,
    /// A length above its limit.
    TooLong(&'static str),
    /// A value that is not allowed here.
    Invalid(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Eof => f.write_str("unexpected end of packet"),
            Self::VarInt => f.write_str("VarInt too long"),
            Self::TooLong(what) => write!(f, "{what} too long"),
            Self::Invalid(what) => write!(f, "invalid {what}"),
        }
    }
}

impl std::error::Error for Error {}

/// Result of reading.
pub type Result<T> = std::result::Result<T, Error>;

/// A bounded reader over a byte slice.
#[derive(Debug, Clone)]
pub struct Reader<'a> {
    data: &'a [u8],
}

macro_rules! read_le {
    ($($name:ident: $t:ty),*) => {
        $(
            /// Reads a little-endian value.
            #[allow(dead_code)]
            pub fn $name(&mut self) -> Result<$t> {
                Ok(<$t>::from_le_bytes(self.array()?))
            }
        )*
    };
}

impl<'a> Reader<'a> {
    /// A reader over `data`.
    pub fn new(data: &'a [u8]) -> Self {
        Self { data }
    }

    /// Bytes left.
    pub fn remaining(&self) -> usize {
        self.data.len()
    }

    /// The unread rest; the reader ends empty.
    pub fn rest(&mut self) -> &'a [u8] {
        std::mem::take(&mut self.data)
    }

    /// The next `n` bytes.
    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if n > self.data.len() {
            return Err(Error::Eof);
        }
        let (head, tail) = self.data.split_at(n);
        self.data = tail;
        Ok(head)
    }

    /// The next `N` bytes as an array.
    pub fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    /// One byte.
    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.array::<1>()?[0])
    }

    /// A boolean byte.
    pub fn bool(&mut self) -> Result<bool> {
        Ok(self.u8()? != 0)
    }

    read_le!(u16: u16, i16: i16, u32: u32, i32: i32, u64: u64, i64: i64, f32: f32);

    /// A big-endian `i32`.
    pub fn be_i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.array()?))
    }

    /// An unsigned LEB128 VarInt of at most 32 bits.
    pub fn varu32(&mut self) -> Result<u32> {
        let mut value = 0u32;
        for shift in (0..35).step_by(7) {
            let b = self.u8()?;
            value |= u32::from(b & 0x7f).checked_shl(shift).unwrap_or(0);
            if b & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(Error::VarInt)
    }

    /// An unsigned LEB128 VarInt of at most 64 bits.
    pub fn varu64(&mut self) -> Result<u64> {
        let mut value = 0u64;
        for shift in (0..70).step_by(7) {
            let b = self.u8()?;
            value |= u64::from(b & 0x7f).checked_shl(shift).unwrap_or(0);
            if b & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(Error::VarInt)
    }

    /// A zig-zag VarInt of at most 32 bits.
    pub fn vari32(&mut self) -> Result<i32> {
        let v = self.varu32()?;
        let half = i32::from_ne_bytes(v.wrapping_shr(1).to_ne_bytes());
        Ok(half ^ 0i32.wrapping_sub(i32::from_ne_bytes((v & 1).to_ne_bytes())))
    }

    /// A zig-zag VarInt of at most 64 bits.
    pub fn vari64(&mut self) -> Result<i64> {
        let v = self.varu64()?;
        let half = i64::from_ne_bytes(v.wrapping_shr(1).to_ne_bytes());
        Ok(half ^ 0i64.wrapping_sub(i64::from_ne_bytes((v & 1).to_ne_bytes())))
    }

    /// A VarInt length, checked against `max`.
    pub fn len(&mut self, what: &'static str, max: usize) -> Result<usize> {
        let n = usize::try_from(self.varu32()?).map_err(|_| Error::TooLong(what))?;
        if n > max {
            return Err(Error::TooLong(what));
        }
        Ok(n)
    }

    /// A VarInt-prefixed byte string of at most `max` bytes.
    pub fn bytes(&mut self, what: &'static str, max: usize) -> Result<&'a [u8]> {
        let n = self.len(what, max)?;
        self.take(n)
    }

    /// A VarInt-prefixed UTF-8 string of at most `max` bytes.
    pub fn string(&mut self, what: &'static str, max: usize) -> Result<&'a str> {
        std::str::from_utf8(self.bytes(what, max)?).map_err(|_| Error::Invalid(what))
    }

    /// Three little-endian floats.
    pub fn vec3(&mut self) -> Result<[f32; 3]> {
        Ok([self.f32()?, self.f32()?, self.f32()?])
    }
}

/// Appends an unsigned LEB128 VarInt.
pub fn write_varu32(out: &mut Vec<u8>, mut v: u32) {
    while v >= 0x80 {
        out.push(low7(u64::from(v)));
        v = v.wrapping_shr(7);
    }
    out.push(low7(u64::from(v)) & 0x7f);
}

/// Appends an unsigned LEB128 VarInt of 64 bits.
pub fn write_varu64(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(low7(v));
        v = v.wrapping_shr(7);
    }
    out.push(low7(v) & 0x7f);
}

/// The low 7 bits of `v` with the continuation bit set.
fn low7(v: u64) -> u8 {
    u8::try_from(v & 0x7f).unwrap_or(0) | 0x80
}

/// Appends a zig-zag VarInt.
pub fn write_vari32(out: &mut Vec<u8>, v: i32) {
    write_varu32(
        out,
        u32::from_ne_bytes((v.wrapping_shl(1) ^ v.wrapping_shr(31)).to_ne_bytes()),
    );
}

/// Appends a zig-zag VarInt of 64 bits.
pub fn write_vari64(out: &mut Vec<u8>, v: i64) {
    write_varu64(
        out,
        u64::from_ne_bytes((v.wrapping_shl(1) ^ v.wrapping_shr(63)).to_ne_bytes()),
    );
}

/// Appends a VarInt length (saturating; lengths here never exceed `u32`).
pub fn write_len(out: &mut Vec<u8>, n: usize) {
    write_varu32(out, u32::try_from(n).unwrap_or(u32::MAX));
}

/// Appends a VarInt-prefixed string.
pub fn write_str(out: &mut Vec<u8>, s: &str) {
    write_bytes(out, s.as_bytes());
}

/// Appends VarInt-prefixed bytes.
pub fn write_bytes(out: &mut Vec<u8>, b: &[u8]) {
    write_len(out, b.len());
    out.extend_from_slice(b);
}

/// Appends three little-endian floats.
pub fn write_vec3(out: &mut Vec<u8>, v: [f32; 3]) {
    for c in v {
        out.extend_from_slice(&c.to_le_bytes());
    }
}

/// Appends a UUID in Bedrock's order (two little-endian halves, most significant first).
pub fn write_uuid(out: &mut Vec<u8>, id: u128) {
    let [hi, lo] =
        [id.wrapping_shr(64), id & u128::from(u64::MAX)].map(|h| u64::try_from(h).unwrap_or(0));
    out.extend_from_slice(&hi.to_le_bytes());
    out.extend_from_slice(&lo.to_le_bytes());
}

/// Appends a block position (three zig-zag VarInts).
pub fn write_block_pos(out: &mut Vec<u8>, [x, y, z]: [i32; 3]) {
    write_vari32(out, x);
    write_vari32(out, y);
    write_vari32(out, z);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints_round_trip() {
        for v in [0i32, 1, -1, 63, -64, 64, i32::MAX, i32::MIN] {
            let mut out = Vec::new();
            write_vari32(&mut out, v);
            assert_eq!(Reader::new(&out).vari32(), Ok(v));
        }
        for v in [0i64, -1, i64::MAX, i64::MIN] {
            let mut out = Vec::new();
            write_vari64(&mut out, v);
            assert_eq!(Reader::new(&out).vari64(), Ok(v));
        }
        let mut out = Vec::new();
        write_varu32(&mut out, 300);
        assert_eq!(out, [0xac, 0x02]);
        assert_eq!(Reader::new(&[0xff; 6]).varu32(), Err(Error::VarInt));
    }
}
