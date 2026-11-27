//! Plain values of the plugin API.

use std::fmt;

use vanillite_abi::{self as abi, Status, status};

/// A player or entity, identified by its UUID.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Uuid(pub u128);

impl Uuid {
    pub(crate) fn from_abi(u: abi::Uuid) -> Uuid {
        Uuid(u.to_u128())
    }

    pub(crate) fn to_abi(self) -> abi::Uuid {
        abi::Uuid::from_u128(self.0)
    }
}

impl fmt::Display for Uuid {
    /// The hyphenated form, e.g. `069a79f4-44e9-4726-a5be-fca90e38aaf5`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let v = self.0;
        write!(
            f,
            "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
            v >> 96,
            (v >> 80) & 0xffff,
            (v >> 64) & 0xffff,
            (v >> 48) & 0xffff,
            v & 0xffff_ffff_ffff
        )
    }
}

impl fmt::Debug for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// A world (dimension) of the server, by its index in tick order: 0 is the overworld.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct World(pub u32);

/// A block position.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct BlockPos {
    /// X.
    pub x: i32,
    /// Y.
    pub y: i32,
    /// Z.
    pub z: i32,
}

impl BlockPos {
    /// A position.
    pub const fn new(x: i32, y: i32, z: i32) -> BlockPos {
        BlockPos { x, y, z }
    }

    pub(crate) fn from_abi(p: abi::BlockPos) -> BlockPos {
        BlockPos::new(p.x, p.y, p.z)
    }

    pub(crate) fn to_abi(self) -> abi::BlockPos {
        abi::BlockPos {
            x: self.x,
            y: self.y,
            z: self.z,
        }
    }
}

/// A box of block positions, both corners inclusive. Block arrays over a box
/// ([`Ctx::blocks`](crate::Ctx::blocks), [`Ctx::set_blocks`](crate::Ctx::set_blocks)) are
/// ordered by Y, then Z, then X (X fastest): see [`BlockBox::index`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BlockBox {
    /// The lowest corner.
    pub min: BlockPos,
    /// The highest corner.
    pub max: BlockPos,
}

impl BlockBox {
    /// The box between two corners in any order.
    pub fn new(a: BlockPos, b: BlockPos) -> BlockBox {
        BlockBox {
            min: BlockPos::new(a.x.min(b.x), a.y.min(b.y), a.z.min(b.z)),
            max: BlockPos::new(a.x.max(b.x), a.y.max(b.y), a.z.max(b.z)),
        }
    }

    /// Its size in blocks along X, Y and Z.
    pub fn size(&self) -> [usize; 3] {
        let len = |lo: i32, hi: i32| hi.abs_diff(lo) as usize + 1;
        [
            len(self.min.x, self.max.x),
            len(self.min.y, self.max.y),
            len(self.min.z, self.max.z),
        ]
    }

    /// Its number of blocks.
    pub fn volume(&self) -> usize {
        let [x, y, z] = self.size();
        x * y * z
    }

    /// The array index of a position inside the box.
    pub fn index(&self, pos: BlockPos) -> usize {
        let [sx, _, sz] = self.size();
        let (x, y, z) = (
            pos.x.abs_diff(self.min.x) as usize,
            pos.y.abs_diff(self.min.y) as usize,
            pos.z.abs_diff(self.min.z) as usize,
        );
        (y * sz + z) * sx + x
    }

    pub(crate) fn to_abi(self) -> abi::BlockBox {
        abi::BlockBox {
            min: self.min.to_abi(),
            max: self.max.to_abi(),
        }
    }
}

/// How block changes by numeric id update the world.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SetMode {
    /// Vanilla's `setBlock` with neighbour and shape updates, like [`Ctx::set_block`](crate::Ctx::set_block).
    #[default]
    Update,
    /// Without neighbour and shape updates (no physics); clients still see the change.
    NoPhysics,
}

impl SetMode {
    pub(crate) fn to_abi(self) -> u32 {
        match self {
            SetMode::Update => abi::set_mode::UPDATE,
            SetMode::NoPhysics => abi::set_mode::NO_PHYSICS,
        }
    }
}

/// A block state id that is no block: [`Ctx::blocks`](crate::Ctx::blocks) reads it outside
/// loaded chunks, [`Ctx::set_blocks`](crate::Ctx::set_blocks) leaves such entries unchanged.
pub const NO_BLOCK: u32 = abi::limits::NO_BLOCK;

/// A position.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Vec3 {
    /// X.
    pub x: f64,
    /// Y.
    pub y: f64,
    /// Z.
    pub z: f64,
}

impl Vec3 {
    pub(crate) fn from_abi(v: abi::Vec3) -> Vec3 {
        Vec3 {
            x: v.x,
            y: v.y,
            z: v.z,
        }
    }
}

/// A message: plain text or a JSON text component.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Text {
    /// Plain text.
    Plain(String),
    /// A JSON text component, e.g. `{"text":"Hi","color":"gold"}`.
    Json(String),
}

impl Text {
    /// Plain text in one colour (`red`, `gold`, `#ff8800`, ...).
    pub fn colored(text: &str, color: &str) -> Text {
        Text::Json(format!(
            "{{\"text\":\"{}\",\"color\":\"{}\"}}",
            escape_json(text),
            escape_json(color)
        ))
    }

    /// The text as a JSON text component.
    pub(crate) fn to_json(&self) -> String {
        match self {
            Text::Plain(s) => format!("\"{}\"", escape_json(s)),
            Text::Json(s) => s.clone(),
        }
    }

    pub(crate) fn parts(&self) -> (&str, u32) {
        match self {
            Text::Plain(s) => (s, abi::text_format::PLAIN),
            Text::Json(s) => (s, abi::text_format::JSON),
        }
    }
}

impl From<&str> for Text {
    fn from(s: &str) -> Text {
        Text::Plain(s.to_owned())
    }
}

impl From<String> for Text {
    fn from(s: String) -> Text {
        Text::Plain(s)
    }
}

/// A game mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GameMode {
    /// Survival.
    Survival,
    /// Creative.
    Creative,
    /// Adventure.
    Adventure,
    /// Spectator.
    Spectator,
}

impl GameMode {
    pub(crate) fn from_abi(v: u32) -> GameMode {
        match v {
            abi::game_mode::CREATIVE => GameMode::Creative,
            abi::game_mode::ADVENTURE => GameMode::Adventure,
            abi::game_mode::SPECTATOR => GameMode::Spectator,
            _ => GameMode::Survival,
        }
    }
}

/// A hand.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Hand {
    /// The main hand.
    Main,
    /// The off hand.
    Off,
}

/// A persistent data value ([`Ctx::entity_data`](crate::Ctx::entity_data)), stored as the NBT
/// tag of the same name in the entity's `BukkitValues`.
#[derive(Clone, Debug, PartialEq)]
pub enum DataValue {
    /// A byte.
    Byte(i8),
    /// A short.
    Short(i16),
    /// An int.
    Int(i32),
    /// A long.
    Long(i64),
    /// A float.
    Float(f32),
    /// A double.
    Double(f64),
    /// Bytes.
    Bytes(Vec<u8>),
    /// A string.
    String(String),
    /// Ints.
    Ints(Vec<i32>),
    /// Longs.
    Longs(Vec<i64>),
}

impl DataValue {
    /// The ABI type and bytes.
    pub(crate) fn encode(&self) -> (u32, Vec<u8>) {
        use abi::data_type as t;
        match self {
            DataValue::Byte(v) => (t::BYTE, v.to_le_bytes().to_vec()),
            DataValue::Short(v) => (t::SHORT, v.to_le_bytes().to_vec()),
            DataValue::Int(v) => (t::INT, v.to_le_bytes().to_vec()),
            DataValue::Long(v) => (t::LONG, v.to_le_bytes().to_vec()),
            DataValue::Float(v) => (t::FLOAT, v.to_le_bytes().to_vec()),
            DataValue::Double(v) => (t::DOUBLE, v.to_le_bytes().to_vec()),
            DataValue::Bytes(v) => (t::BYTE_ARRAY, v.clone()),
            DataValue::String(v) => (t::STRING, v.as_bytes().to_vec()),
            DataValue::Ints(v) => (
                t::INT_ARRAY,
                v.iter().flat_map(|x| x.to_le_bytes()).collect(),
            ),
            DataValue::Longs(v) => (
                t::LONG_ARRAY,
                v.iter().flat_map(|x| x.to_le_bytes()).collect(),
            ),
        }
    }

    /// From the ABI type and bytes; `None` for a malformed value.
    pub(crate) fn decode(kind: u32, bytes: &[u8]) -> Option<DataValue> {
        fn array<const N: usize, T>(bytes: &[u8], f: fn([u8; N]) -> T) -> Option<Vec<T>> {
            let (chunks, rest) = bytes.as_chunks::<N>();
            rest.is_empty()
                .then(|| chunks.iter().map(|c| f(*c)).collect())
        }
        use abi::data_type as t;
        Some(match kind {
            t::BYTE => DataValue::Byte(i8::from_le_bytes(bytes.try_into().ok()?)),
            t::SHORT => DataValue::Short(i16::from_le_bytes(bytes.try_into().ok()?)),
            t::INT => DataValue::Int(i32::from_le_bytes(bytes.try_into().ok()?)),
            t::LONG => DataValue::Long(i64::from_le_bytes(bytes.try_into().ok()?)),
            t::FLOAT => DataValue::Float(f32::from_le_bytes(bytes.try_into().ok()?)),
            t::DOUBLE => DataValue::Double(f64::from_le_bytes(bytes.try_into().ok()?)),
            t::BYTE_ARRAY => DataValue::Bytes(bytes.to_vec()),
            t::STRING => DataValue::String(String::from_utf8(bytes.to_vec()).ok()?),
            t::INT_ARRAY => DataValue::Ints(array(bytes, i32::from_le_bytes)?),
            t::LONG_ARRAY => DataValue::Longs(array(bytes, i64::from_le_bytes)?),
            _ => return None,
        })
    }
}

/// Escapes a string for a JSON string literal.
fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if u32::from(c) < 0x20 => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out
}

/// An error of a host call or of the plugin itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The thing does not exist (offline player, unknown key, unknown world).
    NotFound,
    /// The context does not own this world or entity; schedule a task on its owner.
    WrongOwner,
    /// An argument is invalid (unknown block state, bad value).
    InvalidArgument,
    /// Not supported by this server.
    Unsupported,
    /// Not allowed now (e.g. registering after `load`).
    WrongPhase,
    /// The plugin is disabled.
    Disabled,
    /// A failure with a message (plugin errors) or another status code.
    Other(String),
}

impl Error {
    /// The error of a status code, `Ok` for [`status::OK`].
    pub(crate) fn check(code: Status) -> Result<(), Error> {
        match code {
            status::OK => Ok(()),
            status::NOT_FOUND => Err(Error::NotFound),
            status::WRONG_OWNER => Err(Error::WrongOwner),
            status::INVALID_ARGUMENT => Err(Error::InvalidArgument),
            status::UNSUPPORTED => Err(Error::Unsupported),
            status::WRONG_PHASE => Err(Error::WrongPhase),
            status::DISABLED => Err(Error::Disabled),
            other => Err(Error::Other(format!("status {other}"))),
        }
    }

    /// The status code reported to the host.
    pub(crate) fn status(&self) -> Status {
        match self {
            Error::NotFound => status::NOT_FOUND,
            Error::WrongOwner => status::WRONG_OWNER,
            Error::InvalidArgument => status::INVALID_ARGUMENT,
            Error::Unsupported => status::UNSUPPORTED,
            Error::WrongPhase => status::WRONG_PHASE,
            Error::Disabled => status::DISABLED,
            Error::Other(_) => status::ERROR,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotFound => f.write_str("not found"),
            Error::WrongOwner => f.write_str("the context does not own this data"),
            Error::InvalidArgument => f.write_str("invalid argument"),
            Error::Unsupported => f.write_str("not supported"),
            Error::WrongPhase => f.write_str("not allowed in this phase"),
            Error::Disabled => f.write_str("the plugin is disabled"),
            Error::Other(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for Error {}

impl From<String> for Error {
    fn from(message: String) -> Error {
        Error::Other(message)
    }
}

impl From<&str> for Error {
    fn from(message: &str) -> Error {
        Error::Other(message.to_owned())
    }
}
