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
