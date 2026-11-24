use std::fmt;

/// Result alias of this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Every way NBT input can be rejected. Errors are values, never panics (SEC-07).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The input ended inside a value.
    UnexpectedEnd,
    /// An unknown tag type id.
    InvalidTagType(u8),
    /// A negative or impossible length.
    InvalidLength,
    /// The memory budget of the reader is exhausted.
    BudgetExceeded,
    /// Nesting is deeper than the budget allows.
    DepthExceeded,
    /// A string is not valid (modified) UTF-8.
    InvalidString,
    /// A string does not fit in the 65535 bytes of its length prefix.
    StringTooLong,
    /// The root of the input is not what the caller asked for.
    InvalidRoot,
    /// Bytes remain after the root value.
    TrailingBytes,
    /// SNBT syntax error at a character offset.
    Syntax {
        offset: usize,
        message: &'static str,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedEnd => f.write_str("unexpected end of NBT input"),
            Self::InvalidTagType(t) => write!(f, "invalid NBT tag type {t}"),
            Self::InvalidLength => f.write_str("invalid NBT length"),
            Self::BudgetExceeded => f.write_str("NBT memory budget exceeded"),
            Self::DepthExceeded => f.write_str("NBT nesting too deep"),
            Self::InvalidString => f.write_str("invalid NBT string"),
            Self::StringTooLong => f.write_str("NBT string longer than 65535 bytes"),
            Self::InvalidRoot => f.write_str("invalid NBT root"),
            Self::TrailingBytes => f.write_str("trailing bytes after NBT root"),
            Self::Syntax { offset, message } => {
                write!(f, "SNBT syntax error at {offset}: {message}")
            }
        }
    }
}

impl std::error::Error for Error {}
