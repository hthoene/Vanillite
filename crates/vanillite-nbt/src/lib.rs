//! NBT and SNBT: budgeted streaming reader and writer.
//!
//! - [`Tag`], [`Compound`] and [`List`] are the owned value model.
//! - [`read`] decodes binary NBT from a byte slice in one pass under a [`Budget`] (memory estimate
//!   and nesting depth, SEC-03/04/26); [`read::skip_network`] validates and measures without
//!   allocating, and [`read::Cursor`] walks the input value by value without building a tree.
//! - [`write`] encodes binary NBT (network form with a nameless root, file form with a named root).
//! - [`snbt`] parses and formats the string form under the same depth limit.
//!
//! Strings use Java's modified UTF-8 on the wire ([`mutf8`]).

mod error;
pub mod mutf8;
pub mod read;
pub mod snbt;
mod tag;
pub mod write;

pub use error::{Error, Result};
pub use read::Budget;
pub use tag::{Compound, List, Tag, TagType};
