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

/// Streaming reader over binary NBT that builds no tree: the caller walks compounds and lists
/// and reads, borrows or skips each value. It charges the same [`Budget`] as a full read (names
/// and borrowed strings at their byte length), so it accepts the same input.
#[derive(Debug)]
pub struct Cursor<'a, 'b> {
    rest: &'b [u8],
    budget: &'a mut Budget,
}

impl<'a, 'b> Cursor<'a, 'b> {
    /// A cursor at the start of `input`.
    pub fn new(input: &'b [u8], budget: &'a mut Budget) -> Self {
        Self {
            rest: input,
            budget,
        }
    }

    /// The input not read yet. Comparing it before and after a value gives the value's bytes.
    pub fn rest(&self) -> &'b [u8] {
        self.rest
    }

    fn reader(&mut self) -> Reader<'_, 'b> {
        Reader {
            input: &mut self.rest,
            budget: self.budget,
        }
    }

    /// Reads a file root header (a compound type byte and the ignored root name) and enters the
    /// root compound.
    pub fn named_root(&mut self) -> Result<()> {
        let mut r = self.reader();
        if r.u8()? != TagType::Compound.id() {
            return Err(Error::InvalidRoot);
        }
        let len = usize::from(r.u16()?);
        r.take(len)?;
        self.enter_compound()
    }

    /// Enters a compound value; its entries follow through [`Cursor::entry`].
    pub fn enter_compound(&mut self) -> Result<()> {
        self.budget.push()?;
        self.budget.charge(48)
    }

    /// The next entry of the current compound: its type and its raw name (modified UTF-8, equal
    /// to UTF-8 for ASCII names). `None` at the end of the compound, which leaves it. The caller
    /// must then read or skip the value.
    pub fn entry(&mut self) -> Result<Option<(TagType, &'b [u8])>> {
        let mut r = self.reader();
        let id = r.u8()?;
        if id == 0 {
            self.budget.pop();
            return Ok(None);
        }
        let ty = tag_type(id)?;
        let len = usize::from(r.u16()?);
        let name = r.take(len)?;
        r.budget.charge(28 + 36)?;
        r.budget.charge_n(2, as_u64(len))?;
        Ok(Some((ty, name)))
    }

    /// Enters a list value: its element type and count. Read or skip every element, then call
    /// [`Cursor::leave_list`].
    pub fn enter_list(&mut self) -> Result<(TagType, usize)> {
        self.budget.push()?;
        self.reader().list_header()
    }

    /// Leaves a list entered with [`Cursor::enter_list`].
    pub fn leave_list(&mut self) {
        self.budget.pop();
    }

    /// Validates and skips a value of type `ty`; returns its payload bytes.
    pub fn skip(&mut self, ty: TagType) -> Result<&'b [u8]> {
        let before = self.rest;
        self.reader().skip(ty)?;
        let used = before
            .len()
            .checked_sub(self.rest.len())
            .ok_or(Error::UnexpectedEnd)?;
        before.get(..used).ok_or(Error::UnexpectedEnd)
    }

    /// Reads a value of type `ty` as a tag.
    pub fn read(&mut self, ty: TagType) -> Result<Tag> {
        self.reader().payload(ty)
    }

    /// A numeric value widened to `i64` like [`Tag::as_i64`] (floating values truncated as Java
    /// casts them); a value of another type is skipped and gives `None`.
    #[allow(clippy::as_conversions)]
    pub fn int(&mut self, ty: TagType) -> Result<Option<i64>> {
        let mut r = self.reader();
        Ok(Some(match ty {
            TagType::Byte => i64::from(r.i8()?),
            TagType::Short => i64::from(r.i16()?),
            TagType::Int => i64::from(r.i32()?),
            TagType::Long => r.i64()?,
            TagType::Float => r.f32()? as i64,
            TagType::Double => r.f64()? as i64,
            _ => {
                self.skip(ty)?;
                return Ok(None);
            }
        }))
    }

    /// A string value, borrowed when its bytes are plain UTF-8 (then they mean the same in
    /// modified UTF-8); a value of another type is skipped and gives `None`.
    pub fn str(&mut self, ty: TagType) -> Result<Option<std::borrow::Cow<'b, str>>> {
        if ty != TagType::String {
            self.skip(ty)?;
            return Ok(None);
        }
        let mut r = self.reader();
        let len = usize::from(r.u16()?);
        let bytes = r.take(len)?;
        r.budget.charge(36)?;
        r.budget.charge_n(2, as_u64(len))?;
        Ok(Some(match std::str::from_utf8(bytes) {
            Ok(s) => std::borrow::Cow::Borrowed(s),
            Err(_) => std::borrow::Cow::Owned(mutf8::decode(bytes)?),
        }))
    }

    /// The element bytes of a byte array (`ty` is `ByteArray`) or a long array (`LongArray`,
    /// 8 big-endian bytes per long), without the length prefix; a value of another type is
    /// skipped and gives `None`.
    pub fn array(&mut self, ty: TagType) -> Result<Option<&'b [u8]>> {
        let width: usize = match ty {
            TagType::ByteArray => 1,
            TagType::LongArray => 8,
            _ => {
                self.skip(ty)?;
                return Ok(None);
            }
        };
        let mut r = self.reader();
        let n = r.len()?;
        r.budget.charge(24)?;
        r.budget.charge_n(as_u64(width), as_u64(n))?;
        r.take(n.checked_mul(width).ok_or(Error::InvalidLength)?)
            .map(Some)
    }
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

    /// Validates and skips a string, charged like [`Reader::string`]. ASCII (the same in
    /// modified UTF-8, one UTF-16 unit per byte) is checked without decoding.
    fn skip_string(&mut self, base: u64) -> Result<()> {
        let len = usize::from(self.u16()?);
        let bytes = self.take(len)?;
        let units = if bytes.is_ascii() {
            as_u64(len)
        } else {
            utf16_len(&mutf8::decode(bytes)?)
        };
        self.budget.charge(base)?;
        self.budget.charge_n(2, units)
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

    /// Reads one value of type `ty`. Nesting is walked with an explicit stack on the heap, so the
    /// call stack stays flat at any depth the budget allows (SEC-04).
    fn payload(&mut self, ty: TagType) -> Result<Tag> {
        let depth = self.budget.depth;
        let value = self.read_value(ty);
        self.budget.depth = depth;
        value
    }

    fn read_value(&mut self, mut ty: TagType) -> Result<Tag> {
        let mut stack: Vec<Open> = Vec::new();
        loop {
            let mut done = match ty {
                TagType::List => {
                    self.budget.push()?;
                    let (element, n) = self.list_header()?;
                    // Reserve only what the remaining input can back and the remaining budget
                    // covers at the real element size: Vanilla charges 4 bytes per entry, a `Tag`
                    // takes more (SEC-03).
                    let affordable = self
                        .budget
                        .remaining()
                        .checked_div(as_u64(size_of::<Tag>()))
                        .unwrap_or(0);
                    let cap = n
                        .min(self.input.len())
                        .min(usize::try_from(affordable).unwrap_or(usize::MAX));
                    stack.push(Open::List {
                        element,
                        remaining: n,
                        items: Vec::with_capacity(cap),
                    });
                    None
                }
                TagType::Compound => {
                    self.budget.push()?;
                    self.budget.charge(48)?;
                    stack.push(Open::Compound {
                        compound: Compound::new(),
                        name: String::new(),
                    });
                    None
                }
                _ => Some(self.leaf(ty)?),
            };
            // Hand finished values to their parents until one asks for its next child.
            loop {
                let top = match (done.take(), stack.last_mut()) {
                    (Some(value), None) => return Ok(value),
                    // An opened container is always on the stack.
                    (None, None) => return Err(Error::UnexpectedEnd),
                    (value, Some(top)) => {
                        if let Some(value) = value {
                            top.attach(value, self.budget)?;
                        }
                        top
                    }
                };
                match top {
                    Open::List {
                        element, remaining, ..
                    } => {
                        if let Some(rest) = remaining.checked_sub(1) {
                            *remaining = rest;
                            ty = *element;
                            break;
                        }
                    }
                    Open::Compound { name, .. } => {
                        let id = self.u8()?;
                        if id != 0 {
                            ty = tag_type(id)?;
                            *name = self.string(28)?;
                            break;
                        }
                    }
                }
                done = stack.pop().map(Open::finish);
                self.budget.pop();
            }
        }
    }

    /// Reads a value that holds no other values.
    fn leaf(&mut self, ty: TagType) -> Result<Tag> {
        Ok(match ty {
            TagType::End | TagType::List | TagType::Compound => {
                return Err(Error::InvalidTagType(ty.id()));
            }
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
        })
    }

    /// Reads a list header and charges it: the element type and the element count.
    fn list_header(&mut self) -> Result<(TagType, usize)> {
        self.budget.charge(36)?;
        let ty = self.u8()?;
        let n = self.len()?;
        if ty == 0 && n > 0 {
            return Err(Error::InvalidTagType(0));
        }
        self.budget.charge_n(4, as_u64(n))?;
        Ok((tag_type(ty)?, n))
    }

    /// Validates one value of type `ty` without building it, with the same explicit stack as
    /// [`Reader::payload`].
    fn skip(&mut self, ty: TagType) -> Result<()> {
        let depth = self.budget.depth;
        let result = self.skip_value(ty);
        self.budget.depth = depth;
        result
    }

    fn skip_value(&mut self, mut ty: TagType) -> Result<()> {
        // The open containers: the element type and remaining count of a list, `None` for a
        // compound.
        let mut stack: Vec<Option<(TagType, usize)>> = Vec::new();
        loop {
            let mut finished = match ty {
                TagType::List => {
                    self.budget.push()?;
                    stack.push(Some(self.list_header()?));
                    false
                }
                TagType::Compound => {
                    self.budget.push()?;
                    self.budget.charge(48)?;
                    stack.push(None);
                    false
                }
                _ => {
                    self.skip_leaf(ty)?;
                    true
                }
            };
            loop {
                let Some(top) = stack.last_mut() else {
                    return Ok(());
                };
                match top {
                    Some((element, remaining)) => {
                        if let Some(rest) = remaining.checked_sub(1) {
                            *remaining = rest;
                            ty = *element;
                            break;
                        }
                    }
                    None => {
                        if finished {
                            self.budget.charge(36)?;
                        }
                        let id = self.u8()?;
                        if id != 0 {
                            ty = tag_type(id)?;
                            self.skip_string(28)?;
                            break;
                        }
                    }
                }
                stack.pop();
                self.budget.pop();
                finished = true;
            }
        }
    }

    fn skip_leaf(&mut self, ty: TagType) -> Result<()> {
        match ty {
            TagType::End | TagType::List | TagType::Compound => {
                return Err(Error::InvalidTagType(ty.id()));
            }
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
            TagType::String => self.skip_string(36)?,
        }
        Ok(())
    }
}

/// A container being read.
enum Open {
    List {
        element: TagType,
        remaining: usize,
        items: Vec<Tag>,
    },
    Compound {
        compound: Compound,
        /// Name of the entry whose value is being read.
        name: String,
    },
}

impl Open {
    /// Adds a finished child value.
    fn attach(&mut self, value: Tag, budget: &mut Budget) -> Result<()> {
        match self {
            // Elements of a mixed list arrive wrapped as {"": value}.
            Self::List { items, .. } => items.push(match value {
                Tag::Compound(c) => c.into_list_wrapper().unwrap_or_else(Tag::Compound),
                other => other,
            }),
            Self::Compound { compound, name } => {
                budget.charge(36)?;
                compound.push_unchecked(std::mem::take(name), value);
            }
        }
        Ok(())
    }

    fn finish(self) -> Tag {
        match self {
            Self::List { items, .. } => Tag::List(List(items)),
            Self::Compound { compound, .. } => Tag::Compound(compound),
        }
    }
}

fn as_u64(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

fn utf16_len(s: &str) -> u64 {
    as_u64(s.encode_utf16().count())
}
