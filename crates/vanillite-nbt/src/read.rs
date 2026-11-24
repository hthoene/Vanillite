//! One-pass binary NBT reader over a byte slice with Vanilla's accounting.
//!
//! The [`Budget`] charges the same estimated in-memory sizes as Vanilla's `NbtAccounter` *before*
//! anything is allocated, so a length prefix can never reserve more than the budget (SEC-03), and
//! compounds and lists count towards the depth limit (SEC-04). The reader never panics on input.

use crate::{Compound, Error, List, Result, Tag, TagType, mutf8};

/// Vanilla's quota for NBT from the network: 2 MiB.
pub const NETWORK_QUOTA: u64 = 2_097_152;
/// Vanilla's quota for uncompressed files: 100 MiB.
pub const FILE_QUOTA: u64 = 104_857_600;
/// Vanilla's maximum nesting depth.
pub const MAX_DEPTH: u32 = 512;

/// Memory and depth budget of one read.
#[derive(Debug, Clone)]
pub struct Budget {
    quota: u64,
    usage: u64,
    max_depth: u32,
    depth: u32,
}

impl Budget {
    /// A budget with a memory quota in (estimated) bytes and a maximum depth.
    pub const fn new(quota: u64, max_depth: u32) -> Self {
        Self {
            quota,
            usage: 0,
            max_depth,
            depth: 0,
        }
    }

    /// The network default: 2 MiB, depth 512.
    pub const fn network() -> Self {
        Self::new(NETWORK_QUOTA, MAX_DEPTH)
    }

    /// The file default: 100 MiB, depth 512.
    pub const fn file() -> Self {
        Self::new(FILE_QUOTA, MAX_DEPTH)
    }

    /// Estimated bytes charged so far.
    pub const fn usage(&self) -> u64 {
        self.usage
    }

    /// Charges `size` bytes.
    pub fn charge(&mut self, size: u64) -> Result<()> {
        match self.usage.checked_add(size) {
            Some(total) if total <= self.quota => {
                self.usage = total;
                Ok(())
            }
            _ => Err(Error::BudgetExceeded),
        }
    }

    /// Charges `per_entry * count` bytes.
    pub fn charge_n(&mut self, per_entry: u64, count: u64) -> Result<()> {
        self.charge(per_entry.checked_mul(count).ok_or(Error::BudgetExceeded)?)
    }

    /// Estimated bytes still available.
    fn remaining(&self) -> u64 {
        self.quota.saturating_sub(self.usage)
    }

    fn push(&mut self) -> Result<()> {
        if self.depth >= self.max_depth {
            return Err(Error::DepthExceeded);
        }
        self.depth = self.depth.saturating_add(1);
        Ok(())
    }

    fn pop(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }
}

/// Reads a network NBT value: a type byte and the payload, no root name (since 1.20.2).
/// Returns `None` for a `TAG_End` root, which the protocol uses for "no value".
pub fn read_network(input: &mut &[u8], budget: &mut Budget) -> Result<Option<Tag>> {
    let mut r = Reader { input, budget };
    let ty = r.u8()?;
    if ty == 0 {
        r.budget.charge(8)?;
        return Ok(None);
    }
    r.payload(tag_type(ty)?).map(Some)
}

/// Reads a file NBT root: a type byte, a root name (ignored) and the payload. The root must be a
/// compound.
pub fn read_named(input: &mut &[u8], budget: &mut Budget) -> Result<Compound> {
    let mut r = Reader { input, budget };
    let ty = r.u8()?;
    if ty != TagType::Compound.id() {
        return Err(Error::InvalidRoot);
    }
    let len = usize::from(r.u16()?);
    r.take(len)?;
    match r.payload(TagType::Compound)? {
        Tag::Compound(c) => Ok(c),
        _ => Err(Error::InvalidRoot),
    }
}

/// Validates a network NBT value without building it and returns its encoded length. Used to pass
/// NBT through as bytes after checking it against the same budget a full read would use.
pub fn skip_network(input: &[u8], budget: &mut Budget) -> Result<usize> {
    let mut rest = input;
    let mut r = Reader {
        input: &mut rest,
        budget,
    };
    let ty = r.u8()?;
    if ty != 0 {
        r.skip(tag_type(ty)?)?;
    }
    let rest_len = rest.len();
    input
        .len()
        .checked_sub(rest_len)
        .ok_or(Error::UnexpectedEnd)
}

fn tag_type(id: u8) -> Result<TagType> {
    TagType::from_id(id).ok_or(Error::InvalidTagType(id))
}

struct Reader<'a, 'b> {
    input: &'a mut &'b [u8],
    budget: &'a mut Budget,
}

macro_rules! read_be {
    ($name:ident, $t:ty) => {
        fn $name(&mut self) -> Result<$t> {
            let bytes = self.take(size_of::<$t>())?;
            Ok(<$t>::from_be_bytes(
                bytes.try_into().map_err(|_| Error::UnexpectedEnd)?,
            ))
        }
    };
}

impl<'b> Reader<'_, 'b> {
    fn take(&mut self, n: usize) -> Result<&'b [u8]> {
        let input: &'b [u8] = self.input;
        let (head, tail) = input.split_at_checked(n).ok_or(Error::UnexpectedEnd)?;
        *self.input = tail;
        Ok(head)
    }

    read_be!(u8, u8);
    read_be!(i8, i8);
    read_be!(u16, u16);
    read_be!(i16, i16);
    read_be!(i32, i32);
    read_be!(i64, i64);
    read_be!(f32, f32);
    read_be!(f64, f64);

    fn len(&mut self) -> Result<usize> {
        usize::try_from(self.i32()?).map_err(|_| Error::InvalidLength)
    }

    /// Reads a string and charges `base + 2 * chars` like Vanilla.
    fn string(&mut self, base: u64) -> Result<String> {
        let len = usize::from(self.u16()?);
        let bytes = self.take(len)?;
        let s = mutf8::decode(bytes)?;
        self.budget.charge(base)?;
        self.budget.charge_n(2, utf16_len(&s))?;
        Ok(s)
    }

    fn payload(&mut self, ty: TagType) -> Result<Tag> {
        Ok(match ty {
            TagType::End => return Err(Error::InvalidTagType(0)),
            TagType::Byte => {
                self.budget.charge(9)?;
                Tag::Byte(self.i8()?)
            }
            TagType::Short => {
                self.budget.charge(10)?;
                Tag::Short(self.i16()?)
            }
            TagType::Int => {
                self.budget.charge(12)?;
                Tag::Int(self.i32()?)
            }
            TagType::Long => {
                self.budget.charge(16)?;
                Tag::Long(self.i64()?)
            }
            TagType::Float => {
                self.budget.charge(12)?;
                Tag::Float(self.f32()?)
            }
            TagType::Double => {
                self.budget.charge(16)?;
                Tag::Double(self.f64()?)
            }
            TagType::ByteArray => {
                self.budget.charge(24)?;
                let n = self.len()?;
                self.budget.charge_n(1, as_u64(n))?;
                Tag::ByteArray(
                    self.take(n)?
                        .iter()
                        .map(|b| i8::from_be_bytes([*b]))
                        .collect(),
                )
            }
            TagType::IntArray => {
                self.budget.charge(24)?;
                let n = self.len()?;
                self.budget.charge_n(4, as_u64(n))?;
                let bytes = self.take(n.checked_mul(4).ok_or(Error::InvalidLength)?)?;
                Tag::IntArray(
                    bytes
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|c| i32::from_be_bytes(*c))
                        .collect(),
                )
            }
            TagType::LongArray => {
                self.budget.charge(24)?;
                let n = self.len()?;
                self.budget.charge_n(8, as_u64(n))?;
                let bytes = self.take(n.checked_mul(8).ok_or(Error::InvalidLength)?)?;
                Tag::LongArray(
                    bytes
                        .as_chunks::<8>()
                        .0
                        .iter()
                        .map(|c| i64::from_be_bytes(*c))
                        .collect(),
                )
            }
            TagType::String => Tag::String(self.string(36)?),
            TagType::List => {
                self.budget.push()?;
                let list = self.list();
                self.budget.pop();
                Tag::List(list?)
            }
            TagType::Compound => {
                self.budget.push()?;
                let compound = self.compound();
                self.budget.pop();
                Tag::Compound(compound?)
            }
        })
    }

    fn list(&mut self) -> Result<List> {
        self.budget.charge(36)?;
        let ty = self.u8()?;
        let n = self.len()?;
        if ty == 0 && n > 0 {
            return Err(Error::InvalidTagType(0));
        }
        self.budget.charge_n(4, as_u64(n))?;
        let ty = tag_type(ty)?;
        // Reserve only what the remaining input can back and the remaining budget covers at the
        // real element size: Vanilla charges 4 bytes per entry, a `Tag` takes more (SEC-03).
        let affordable = self
            .budget
            .remaining()
            .checked_div(as_u64(size_of::<Tag>()))
            .unwrap_or(0);
        let cap = n
            .min(self.input.len())
            .min(usize::try_from(affordable).unwrap_or(usize::MAX));
        let mut items = Vec::with_capacity(cap);
        for _ in 0..n {
            items.push(match self.payload(ty)? {
                Tag::Compound(c) => match c.as_list_wrapper() {
                    Some(inner) => inner.clone(),
                    None => Tag::Compound(c),
                },
                other => other,
            });
        }
        Ok(List(items))
    }

    fn compound(&mut self) -> Result<Compound> {
        self.budget.charge(48)?;
        let mut c = Compound::new();
        loop {
            let ty = self.u8()?;
            if ty == 0 {
                return Ok(c);
            }
            let ty = tag_type(ty)?;
            let name = self.string(28)?;
            let value = self.payload(ty)?;
            self.budget.charge(36)?;
            c.push_unchecked(name, value);
        }
    }

    fn skip(&mut self, ty: TagType) -> Result<()> {
        match ty {
            TagType::End => return Err(Error::InvalidTagType(0)),
            TagType::Byte => self.take(1).map(drop)?,
            TagType::Short => self.take(2).map(drop)?,
            TagType::Int | TagType::Float => self.take(4).map(drop)?,
            TagType::Long | TagType::Double => self.take(8).map(drop)?,
            TagType::ByteArray | TagType::IntArray | TagType::LongArray => {
                let width: usize = match ty {
                    TagType::ByteArray => 1,
                    TagType::IntArray => 4,
                    _ => 8,
                };
                let n = self.len()?;
                self.budget.charge(24)?;
                self.budget.charge_n(as_u64(width), as_u64(n))?;
                self.take(n.checked_mul(width).ok_or(Error::InvalidLength)?)?;
            }
            TagType::String => {
                self.string(36)?;
            }
            TagType::List => {
                self.budget.push()?;
                let r = self.skip_list();
                self.budget.pop();
                r?;
            }
            TagType::Compound => {
                self.budget.push()?;
                let r = self.skip_compound();
                self.budget.pop();
                r?;
            }
        }
        Ok(())
    }

    fn skip_list(&mut self) -> Result<()> {
        self.budget.charge(36)?;
        let ty = self.u8()?;
        let n = self.len()?;
        if ty == 0 && n > 0 {
            return Err(Error::InvalidTagType(0));
        }
        self.budget.charge_n(4, as_u64(n))?;
        let ty = tag_type(ty)?;
        for _ in 0..n {
            self.skip(ty)?;
        }
        Ok(())
    }

    fn skip_compound(&mut self) -> Result<()> {
        self.budget.charge(48)?;
        loop {
            let ty = self.u8()?;
            if ty == 0 {
                return Ok(());
            }
            let ty = tag_type(ty)?;
            self.string(28)?;
            self.skip(ty)?;
            self.budget.charge(36)?;
        }
    }
}

fn as_u64(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

fn utf16_len(s: &str) -> u64 {
    as_u64(s.encode_utf16().count())
}
