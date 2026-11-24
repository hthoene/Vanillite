//! Primitive wire codecs: the bounded [`Reader`], the [`Encode`]/[`Decode`] traits and their
//! implementations for Java's primitive types, VarInt/VarLong, strings, options and lists.
//!
//! Decoding never panics and never allocates more than the input can back: list lengths are
//! checked against the remaining bytes before reserving (SEC-03), and every variable-size type
//! needs an explicit limit through [`DecodeLimited`] (SEC-02).

use std::fmt;

/// Result alias for decoding.
pub type Result<T> = std::result::Result<T, Error>;

/// A protocol decoding error. All of them end the connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The frame ended inside a value.
    UnexpectedEnd,
    /// A VarInt longer than 5 bytes or a VarLong longer than 10.
    VarIntTooLong,
    /// A length or count outside its limit.
    TooLong {
        what: &'static str,
        len: usize,
        max: usize,
    },
    /// A negative length.
    NegativeLength,
    /// Invalid UTF-8 in a string.
    InvalidUtf8,
    /// A value outside its allowed set or range.
    Invalid(&'static str),
    /// Bytes left over after a packet.
    TrailingBytes { id: i32, len: usize },
    /// A packet id not known in the current state.
    UnknownPacket { id: i32 },
    /// NBT input was rejected.
    Nbt(vanillite_nbt::Error),
    /// A frame does not respect the framing rules (length, compression).
    BadFrame(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedEnd => f.write_str("unexpected end of packet"),
            Self::VarIntTooLong => f.write_str("VarInt too long"),
            Self::TooLong { what, len, max } => write!(f, "{what} too long ({len} > {max})"),
            Self::NegativeLength => f.write_str("negative length"),
            Self::InvalidUtf8 => f.write_str("invalid UTF-8"),
            Self::Invalid(what) => write!(f, "invalid value: {what}"),
            Self::TrailingBytes { id, len } => {
                write!(f, "{len} trailing bytes after packet 0x{id:02x}")
            }
            Self::UnknownPacket { id } => write!(f, "unknown packet 0x{id:02x}"),
            Self::Nbt(e) => write!(f, "NBT: {e}"),
            Self::BadFrame(what) => write!(f, "bad frame: {what}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<vanillite_nbt::Error> for Error {
    fn from(e: vanillite_nbt::Error) -> Self {
        Self::Nbt(e)
    }
}

/// A cursor over one packet body.
#[derive(Debug, Clone)]
pub struct Reader<'a> {
    buf: &'a [u8],
}

macro_rules! read_be {
    ($($name:ident: $t:ty),* $(,)?) => {
        $(
            #[doc = concat!("Reads a big-endian `", stringify!($t), "`.")]
            pub fn $name(&mut self) -> Result<$t> {
                Ok(<$t>::from_be_bytes(self.array()?))
            }
        )*
    };
}

impl<'a> Reader<'a> {
    /// A reader over `buf`.
    pub const fn new(buf: &'a [u8]) -> Self {
        Self { buf }
    }

    /// Bytes left.
    pub const fn remaining(&self) -> usize {
        self.buf.len()
    }

    /// The bytes left, without consuming them.
    pub const fn rest(&self) -> &'a [u8] {
        self.buf
    }

    /// Takes `n` bytes.
    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let (head, tail) = self.buf.split_at_checked(n).ok_or(Error::UnexpectedEnd)?;
        self.buf = tail;
        Ok(head)
    }

    /// Takes all remaining bytes.
    pub fn take_rest(&mut self) -> &'a [u8] {
        std::mem::take(&mut self.buf)
    }

    /// Advances past `n` bytes that a caller has already inspected through [`Reader::rest`].
    pub fn skip(&mut self, n: usize) -> Result<()> {
        self.take(n).map(drop)
    }

    /// Takes a fixed-size array.
    pub fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let (head, tail) = self
            .buf
            .split_first_chunk::<N>()
            .ok_or(Error::UnexpectedEnd)?;
        self.buf = tail;
        Ok(*head)
    }

    read_be!(u8: u8, i8: i8, u16: u16, i16: i16, i32: i32, i64: i64, f32: f32, f64: f64, u128: u128);

    /// Reads a VarInt (at most 5 bytes).
    pub fn varint(&mut self) -> Result<i32> {
        let mut value: u32 = 0;
        for i in 0..5u32 {
            let b = self.u8()?;
            value |= u32::from(b & 0x7F).wrapping_shl(i.wrapping_mul(7));
            if b & 0x80 == 0 {
                return Ok(i32::from_ne_bytes(value.to_ne_bytes()));
            }
        }
        Err(Error::VarIntTooLong)
    }

    /// Reads a VarLong (at most 10 bytes).
    pub fn varlong(&mut self) -> Result<i64> {
        let mut value: u64 = 0;
        for i in 0..10u32 {
            let b = self.u8()?;
            value |= u64::from(b & 0x7F).wrapping_shl(i.wrapping_mul(7));
            if b & 0x80 == 0 {
                return Ok(i64::from_ne_bytes(value.to_ne_bytes()));
            }
        }
        Err(Error::VarIntTooLong)
    }

    /// Reads a VarInt length and checks it against `max` and the remaining input, where each
    /// element needs at least `min_elem_size` bytes. This is the capped preallocation helper:
    /// a returned length is always safe to reserve.
    pub fn len_prefix(
        &mut self,
        what: &'static str,
        max: usize,
        min_elem_size: usize,
    ) -> Result<usize> {
        let len = usize::try_from(self.varint()?).map_err(|_| Error::NegativeLength)?;
        if len > max {
            return Err(Error::TooLong { what, len, max });
        }
        if len.saturating_mul(min_elem_size) > self.remaining() {
            return Err(Error::UnexpectedEnd);
        }
        Ok(len)
    }

    /// Reads a VarInt and maps it to `0..count`, as Java's `ByIdMap` with a strict policy does.
    pub fn enum_id(&mut self, count: u32, what: &'static str) -> Result<u32> {
        u32::try_from(self.varint()?)
            .ok()
            .filter(|v| *v < count)
            .ok_or(Error::Invalid(what))
    }

    /// Reads a string of at most `max_chars` UTF-16 units, like Java's `readUtf(max)`.
    pub fn string(&mut self, max_chars: usize) -> Result<&'a str> {
        let max_bytes = max_chars.saturating_mul(3);
        let len = usize::try_from(self.varint()?).map_err(|_| Error::NegativeLength)?;
        if len > max_bytes {
            return Err(Error::TooLong {
                what: "string bytes",
                len,
                max: max_bytes,
            });
        }
        let s = std::str::from_utf8(self.take(len)?).map_err(|_| Error::InvalidUtf8)?;
        // Only count UTF-16 units when the byte length alone does not prove the limit.
        if len > max_chars {
            let chars = s.encode_utf16().count();
            if chars > max_chars {
                return Err(Error::TooLong {
                    what: "string",
                    len: chars,
                    max: max_chars,
                });
            }
        }
        Ok(s)
    }

    /// Fails if bytes are left (Vanilla rejects packets that are not fully read).
    pub fn finish(&self, id: i32) -> Result<()> {
        if self.buf.is_empty() {
            Ok(())
        } else {
            Err(Error::TrailingBytes {
                id,
                len: self.buf.len(),
            })
        }
    }
}

/// Appends a VarInt.
pub fn write_varint(out: &mut Vec<u8>, value: i32) {
    let mut v = u32::from_ne_bytes(value.to_ne_bytes());
    loop {
        let byte = u8::try_from(v & 0x7F).unwrap_or(0);
        v = v.wrapping_shr(7);
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Appends a VarLong.
pub fn write_varlong(out: &mut Vec<u8>, value: i64) {
    let mut v = u64::from_ne_bytes(value.to_ne_bytes());
    loop {
        let byte = u8::try_from(v & 0x7F).unwrap_or(0);
        v = v.wrapping_shr(7);
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Encoded size of a VarInt.
pub const fn varint_len(value: i32) -> usize {
    let v = u32::from_ne_bytes(value.to_ne_bytes());
    match v {
        0..=0x7F => 1,
        0x80..=0x3FFF => 2,
        0x4000..=0x1F_FFFF => 3,
        0x20_0000..=0xFFF_FFFF => 4,
        _ => 5,
    }
}

/// Appends a length prefix. Lengths beyond `i32::MAX` cannot come from server data in practice;
/// they are clamped, which the client then rejects.
pub fn write_len(out: &mut Vec<u8>, len: usize) {
    write_varint(out, i32::try_from(len).unwrap_or(i32::MAX));
}

/// Appends a string with its length prefix.
pub fn write_str(out: &mut Vec<u8>, s: &str) {
    write_len(out, s.len());
    out.extend_from_slice(s.as_bytes());
}

/// Values that can be written to the wire.
pub trait Encode {
    /// Appends the encoding of `self`.
    fn encode(&self, out: &mut Vec<u8>);

    /// Encodes into a new buffer.
    fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }
}

/// Fixed-size values that can be read from the wire.
pub trait Decode: Sized {
    /// Reads one value.
    fn decode(r: &mut Reader<'_>) -> Result<Self>;
}

/// Variable-size values that need a caller-given limit (strings, lists, byte arrays, NBT).
/// For strings the limit is in characters, for lists in elements, for byte arrays in bytes.
pub trait DecodeLimited: Sized {
    /// Reads one value of at most `max` units.
    fn decode_limited(r: &mut Reader<'_>, max: usize) -> Result<Self>;
}

macro_rules! num {
    ($($t:ident),*) => {
        $(
            impl Encode for $t {
                fn encode(&self, out: &mut Vec<u8>) {
                    out.extend_from_slice(&self.to_be_bytes());
                }
            }
            impl Decode for $t {
                fn decode(r: &mut Reader<'_>) -> Result<Self> {
                    r.$t()
                }
            }
        )*
    };
}

num!(u8, i8, u16, i16, i32, i64, f32, f64, u128);

impl Encode for bool {
    fn encode(&self, out: &mut Vec<u8>) {
        out.push(u8::from(*self));
    }
}

impl Decode for bool {
    /// Java's `readBoolean` accepts any non-zero byte as true.
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        Ok(r.u8()? != 0)
    }
}

impl Encode for String {
    fn encode(&self, out: &mut Vec<u8>) {
        write_str(out, self);
    }
}

impl Encode for str {
    fn encode(&self, out: &mut Vec<u8>) {
        write_str(out, self);
    }
}

impl DecodeLimited for String {
    fn decode_limited(r: &mut Reader<'_>, max: usize) -> Result<Self> {
        r.string(max).map(str::to_owned)
    }
}

impl<T: Encode> Encode for Option<T> {
    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Some(v) => {
                out.push(1);
                v.encode(out);
            }
            None => out.push(0),
        }
    }
}

impl<T: Decode> Decode for Option<T> {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        if bool::decode(r)? {
            T::decode(r).map(Some)
        } else {
            Ok(None)
        }
    }
}

impl<T: DecodeLimited> DecodeLimited for Option<T> {
    fn decode_limited(r: &mut Reader<'_>, max: usize) -> Result<Self> {
        if bool::decode(r)? {
            T::decode_limited(r, max).map(Some)
        } else {
            Ok(None)
        }
    }
}

impl<T: Encode> Encode for Vec<T> {
    fn encode(&self, out: &mut Vec<u8>) {
        write_len(out, self.len());
        for item in self {
            item.encode(out);
        }
    }
}

impl<T: Encode> Encode for [T] {
    fn encode(&self, out: &mut Vec<u8>) {
        write_len(out, self.len());
        for item in self {
            item.encode(out);
        }
    }
}

/// A list of fixed-size elements: `max` is the element count limit.
impl<T: Decode> DecodeLimited for Vec<T> {
    fn decode_limited(r: &mut Reader<'_>, max: usize) -> Result<Self> {
        let len = r.len_prefix("list", max, 1)?;
        let mut items = Vec::with_capacity(len);
        for _ in 0..len {
            items.push(T::decode(r)?);
        }
        Ok(items)
    }
}

/// Decodes a list whose elements need their own limit.
pub fn decode_list_limited<T: DecodeLimited>(
    r: &mut Reader<'_>,
    max: usize,
    elem_max: usize,
) -> Result<Vec<T>> {
    let len = r.len_prefix("list", max, 1)?;
    let mut items = Vec::with_capacity(len);
    for _ in 0..len {
        items.push(T::decode_limited(r, elem_max)?);
    }
    Ok(items)
}

/// Implements `Encode`/`Decode` for a fieldless enum written as a VarInt id `0..n` in declaration
/// order (Java's `ByIdMap` with a strict out-of-bounds policy).
#[macro_export]
macro_rules! varint_enum {
    ($(#[$m:meta])* $vis:vis enum $name:ident { $($variant:ident),* $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        $vis enum $name { $($variant),* }

        impl $name {
            const ALL: &'static [$name] = &[$($name::$variant),*];

            /// The wire id.
            pub fn id(self) -> i32 {
                Self::ALL.iter().position(|v| *v == self).and_then(|p| i32::try_from(p).ok()).unwrap_or(0)
            }
        }

        impl $crate::codec::Encode for $name {
            fn encode(&self, out: &mut Vec<u8>) {
                $crate::codec::write_varint(out, self.id());
            }
        }

        impl $crate::codec::Decode for $name {
            fn decode(r: &mut $crate::codec::Reader<'_>) -> $crate::codec::Result<Self> {
                let id = r.varint()?;
                usize::try_from(id)
                    .ok()
                    .and_then(|i| Self::ALL.get(i).copied())
                    .ok_or($crate::codec::Error::Invalid(stringify!($name)))
            }
        }
    };
}

/// A last field that takes the rest of the packet without a length prefix.
pub trait Rest: Sized {
    /// Reads the rest of the packet, at most `max` bytes.
    fn decode_rest(r: &mut Reader<'_>, max: usize) -> Result<Self>;
    /// Writes the value without a length prefix.
    fn encode_rest(&self, out: &mut Vec<u8>);
}
