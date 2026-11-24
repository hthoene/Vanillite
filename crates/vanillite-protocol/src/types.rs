//! Composite wire types shared by many packets.

use crate::codec::{Decode, DecodeLimited, Encode, Error, Reader, Result, write_len, write_varint};
use std::fmt;
use vanillite_nbt::{Budget, Tag};

/// Vanilla's limit for strings without an explicit one (`readUtf()`).
pub const MAX_STRING: usize = 32767;

/// A namespaced identifier (`minecraft:stone`), validated like Vanilla's `Identifier`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Identifier(String);

impl Identifier {
    /// Parses and validates `namespace:path` (namespace defaults to `minecraft`).
    pub fn new(s: &str) -> Result<Self> {
        let (ns, path) = s.split_once(':').unwrap_or(("minecraft", s));
        let ns_ok = ns
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-'));
        let path_ok = path
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'/'));
        if !ns_ok || !path_ok {
            return Err(Error::Invalid("identifier"));
        }
        Ok(Self(format!("{ns}:{path}")))
    }

    /// An identifier from trusted static text in `namespace:path` form.
    pub fn vanilla(path: &str) -> Self {
        Self(format!("minecraft:{path}"))
    }

    /// The full `namespace:path` text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The namespace.
    pub fn namespace(&self) -> &str {
        self.0.split_once(':').map_or("minecraft", |(n, _)| n)
    }

    /// The path.
    pub fn path(&self) -> &str {
        self.0.split_once(':').map_or(self.0.as_str(), |(_, p)| p)
    }
}

impl fmt::Display for Identifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Encode for Identifier {
    fn encode(&self, out: &mut Vec<u8>) {
        self.0.encode(out);
    }
}

impl Decode for Identifier {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        Self::new(r.string(MAX_STRING)?)
    }
}

impl DecodeLimited for Identifier {
    fn decode_limited(r: &mut Reader<'_>, max: usize) -> Result<Self> {
        Self::new(r.string(max)?)
    }
}

/// A UUID as two big-endian longs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Uuid(pub u128);

impl Uuid {
    /// Parses the hyphenated or the plain 32-digit hex form (the session server uses the latter).
    pub fn parse(s: &str) -> Option<Self> {
        let hex: String = s.chars().filter(|c| *c != '-').collect();
        if hex.len() != 32 {
            return None;
        }
        u128::from_str_radix(&hex, 16).ok().map(Self)
    }

    /// Vanilla's offline-mode UUID: version 3 (MD5) of `OfflinePlayer:<name>`.
    pub fn offline(name: &str) -> Self {
        let digest = md5(format!("OfflinePlayer:{name}").as_bytes());
        let mut v = u128::from_be_bytes(digest);
        v &= !(0xF000u128 << 64);
        v |= 0x3000u128 << 64;
        v &= !(0xC000_0000_0000_0000u128);
        v |= 0x8000_0000_0000_0000u128;
        Self(v)
    }

    /// The 32-digit form without hyphens.
    pub fn simple(&self) -> String {
        format!("{:032x}", self.0)
    }
}

impl fmt::Display for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let h = self.simple();
        let part = |a: usize, b: usize| h.get(a..b).unwrap_or("");
        write!(
            f,
            "{}-{}-{}-{}-{}",
            part(0, 8),
            part(8, 12),
            part(12, 16),
            part(16, 20),
            part(20, 32)
        )
    }
}

impl Encode for Uuid {
    fn encode(&self, out: &mut Vec<u8>) {
        self.0.encode(out);
    }
}

impl Decode for Uuid {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        r.u128().map(Self)
    }
}

/// Raw bytes: as a field with `max_len` a VarInt-prefixed byte array; [`RawBytes::decode_rest`]
/// takes the rest of the packet (custom payloads).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RawBytes(pub Vec<u8>);

impl RawBytes {
    /// Takes the rest of the packet, at most `max` bytes.
    pub fn decode_rest(r: &mut Reader<'_>, max: usize) -> Result<Self> {
        if r.remaining() > max {
            return Err(Error::TooLong {
                what: "payload",
                len: r.remaining(),
                max,
            });
        }
        Ok(Self(r.take_rest().to_vec()))
    }

    /// Writes the bytes without a length prefix.
    pub fn encode_rest(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.0);
    }
}

impl Encode for RawBytes {
    fn encode(&self, out: &mut Vec<u8>) {
        write_len(out, self.0.len());
        out.extend_from_slice(&self.0);
    }
}

impl DecodeLimited for RawBytes {
    fn decode_limited(r: &mut Reader<'_>, max: usize) -> Result<Self> {
        let len = r.len_prefix("byte array", max, 1)?;
        Ok(Self(r.take(len)?.to_vec()))
    }
}

/// A network NBT value (nameless root); `None` is the `TAG_End` root. The limit given to
/// [`DecodeLimited`] is the memory quota in bytes; the depth limit is Vanilla's 512.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Nbt(pub Option<Tag>);

impl Encode for Nbt {
    fn encode(&self, out: &mut Vec<u8>) {
        match &self.0 {
            // Values in packets come from the server; an unencodable string is a server bug and
            // degrades to an empty compound instead of a corrupt stream.
            Some(tag) => {
                let start = out.len();
                if vanillite_nbt::write::write_network(tag, out).is_err() {
                    out.truncate(start);
                    out.extend_from_slice(&[10, 0]);
                }
            }
            None => out.push(0),
        }
    }
}

impl DecodeLimited for Nbt {
    fn decode_limited(r: &mut Reader<'_>, max: usize) -> Result<Self> {
        let mut budget = Budget::new(
            u64::try_from(max).unwrap_or(u64::MAX),
            vanillite_nbt::read::MAX_DEPTH,
        );
        let mut input = r.rest();
        let before = input.len();
        let tag = vanillite_nbt::read::read_network(&mut input, &mut budget)?;
        r.skip(before.saturating_sub(input.len()))?;
        Ok(Self(tag))
    }
}

/// Network NBT kept as validated bytes, for values the server passes through or pre-encodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawNbt(pub Vec<u8>);

impl Default for RawNbt {
    fn default() -> Self {
        Self(vec![0])
    }
}

impl Encode for RawNbt {
    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.0);
    }
}

impl DecodeLimited for RawNbt {
    fn decode_limited(r: &mut Reader<'_>, max: usize) -> Result<Self> {
        let mut budget = Budget::new(
            u64::try_from(max).unwrap_or(u64::MAX),
            vanillite_nbt::read::MAX_DEPTH,
        );
        let len = vanillite_nbt::read::skip_network(r.rest(), &mut budget)?;
        Ok(Self(r.take(len)?.to_vec()))
    }
}

/// Java's `BitSet` as a VarInt-prefixed long array. The limit is in longs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BitSet(pub Vec<i64>);

impl Encode for BitSet {
    fn encode(&self, out: &mut Vec<u8>) {
        self.0.encode(out);
    }
}

impl DecodeLimited for BitSet {
    fn decode_limited(r: &mut Reader<'_>, max: usize) -> Result<Self> {
        let len = r.len_prefix("bit set", max, 8)?;
        let mut v = Vec::with_capacity(len);
        for _ in 0..len {
            v.push(r.i64()?);
        }
        Ok(Self(v))
    }
}

/// A block position packed into a long: x 26 bits, z 26 bits, y 12 bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct BlockPos {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl BlockPos {
    /// Packs into Vanilla's long layout.
    pub fn pack(self) -> i64 {
        let x = i64::from(self.x) & 0x3FF_FFFF;
        let y = i64::from(self.y) & 0xFFF;
        let z = i64::from(self.z) & 0x3FF_FFFF;
        x.wrapping_shl(38) | z.wrapping_shl(12) | y
    }

    /// Unpacks Vanilla's long layout.
    pub fn unpack(v: i64) -> Self {
        let x = v.wrapping_shr(38);
        let y = v.wrapping_shl(52).wrapping_shr(52);
        let z = v.wrapping_shl(26).wrapping_shr(38);
        Self {
            x: i32::try_from(x).unwrap_or(0),
            y: i32::try_from(y).unwrap_or(0),
            z: i32::try_from(z).unwrap_or(0),
        }
    }
}

impl Encode for BlockPos {
    fn encode(&self, out: &mut Vec<u8>) {
        self.pack().encode(out);
    }
}

impl Decode for BlockPos {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        r.i64().map(Self::unpack)
    }
}

/// A profile property (`textures` with its Mojang signature).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Property {
    pub name: String,
    pub value: String,
    pub signature: Option<String>,
}

impl Encode for Property {
    fn encode(&self, out: &mut Vec<u8>) {
        self.name.encode(out);
        self.value.encode(out);
        self.signature.encode(out);
    }
}

impl Decode for Property {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        Ok(Self {
            name: String::decode_limited(r, 64)?,
            value: String::decode_limited(r, MAX_STRING)?,
            signature: Option::<String>::decode_limited(r, 1024)?,
        })
    }
}

/// A game profile: UUID, name (max 16) and at most 16 properties.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GameProfile {
    pub id: Uuid,
    pub name: String,
    pub properties: Vec<Property>,
}

impl Encode for GameProfile {
    fn encode(&self, out: &mut Vec<u8>) {
        self.id.encode(out);
        self.name.encode(out);
        self.properties.encode(out);
    }
}

impl Decode for GameProfile {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        Ok(Self {
            id: Uuid::decode(r)?,
            name: String::decode_limited(r, 16)?,
            properties: Vec::decode_limited(r, 16)?,
        })
    }
}

/// A data pack the client may already know (`minecraft:core` of the same version).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KnownPack {
    pub namespace: String,
    pub id: String,
    pub version: String,
}

impl Encode for KnownPack {
    fn encode(&self, out: &mut Vec<u8>) {
        self.namespace.encode(out);
        self.id.encode(out);
        self.version.encode(out);
    }
}

impl Decode for KnownPack {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        Ok(Self {
            namespace: String::decode_limited(r, MAX_STRING)?,
            id: String::decode_limited(r, MAX_STRING)?,
            version: String::decode_limited(r, MAX_STRING)?,
        })
    }
}

/// Writes a VarInt-prefixed list of VarInts.
pub fn write_varint_list(out: &mut Vec<u8>, values: &[i32]) {
    write_len(out, values.len());
    for v in values {
        write_varint(out, *v);
    }
}

/// MD5 for offline UUIDs (RFC 1321). Small enough to keep a dependency out.
fn md5(input: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: Vec<u32> = (0..64u32)
        .map(|i| {
            let v = (f64::from(i) + 1.0).sin().abs() * 4_294_967_296.0;
            // Truncation to u32 is the definition of the MD5 constants.
            #[allow(clippy::as_conversions)]
            let k = v as u64 as u32;
            k
        })
        .collect();
    let mut msg = input.to_vec();
    let bit_len = u64::try_from(input.len()).unwrap_or(0).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());
    let (mut a0, mut b0, mut c0, mut d0) = (
        0x6745_2301u32,
        0xefcd_ab89u32,
        0x98ba_dcfeu32,
        0x1032_5476u32,
    );
    for chunk in msg.as_chunks::<64>().0 {
        let m: Vec<u32> = chunk
            .as_chunks::<4>()
            .0
            .iter()
            .map(|w| u32::from_le_bytes(*w))
            .collect();
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64usize {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => (
                    (d & b) | (!d & c),
                    (5usize.wrapping_mul(i).wrapping_add(1)) % 16,
                ),
                2 => (b ^ c ^ d, (3usize.wrapping_mul(i).wrapping_add(5)) % 16),
                _ => (c ^ (b | !d), (7usize.wrapping_mul(i)) % 16),
            };
            let f = f
                .wrapping_add(a)
                .wrapping_add(k.get(i).copied().unwrap_or(0))
                .wrapping_add(m.get(g).copied().unwrap_or(0));
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S.get(i).copied().unwrap_or(0)));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    let mut out = [0u8; 16];
    for (dst, word) in out.as_chunks_mut::<4>().0.iter_mut().zip([a0, b0, c0, d0]) {
        *dst = word.to_le_bytes();
    }
    out
}

impl crate::codec::Rest for RawBytes {
    fn decode_rest(r: &mut Reader<'_>, max: usize) -> Result<Self> {
        Self::decode_rest(r, max)
    }

    fn encode_rest(&self, out: &mut Vec<u8>) {
        Self::encode_rest(self, out);
    }
}

/// A nullable rest-of-packet payload (login plugin answers): a presence flag, then the bytes.
impl crate::codec::Rest for Option<RawBytes> {
    fn decode_rest(r: &mut Reader<'_>, max: usize) -> Result<Self> {
        if bool::decode(r)? {
            RawBytes::decode_rest(r, max).map(Some)
        } else {
            Ok(None)
        }
    }

    fn encode_rest(&self, out: &mut Vec<u8>) {
        match self {
            Some(b) => {
                out.push(1);
                b.encode_rest(out);
            }
            None => out.push(0),
        }
    }
}

/// A VarInt-prefixed list of VarInts; the limit is the element count.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VarIntList(pub Vec<i32>);

impl Encode for VarIntList {
    fn encode(&self, out: &mut Vec<u8>) {
        write_varint_list(out, &self.0);
    }
}

impl DecodeLimited for VarIntList {
    fn decode_limited(r: &mut Reader<'_>, max: usize) -> Result<Self> {
        let len = r.len_prefix("VarInt list", max, 1)?;
        let mut v = Vec::with_capacity(len);
        for _ in 0..len {
            v.push(r.varint()?);
        }
        Ok(Self(v))
    }
}
