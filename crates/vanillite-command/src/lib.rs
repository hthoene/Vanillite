//! Brigadier-compatible command tree, argument parsers and suggestions.
//!
//! - [`Dispatcher`] holds the tree; commands are registered with [`literal`] and
//!   [`argument`] builders and identified by a copyable key `C` the game matches on (no
//!   closures, no string building: SEC-10).
//! - [`Dispatcher::parse`] and [`ParseResults::finish`] turn a command line into a
//!   [`Parsed`] command with typed [`Args`] or a Vanilla [`CommandError`].
//! - [`Dispatcher::suggest`] answers tab completion, [`Dispatcher::to_tree`] builds the
//!   commands packet and [`Dispatcher::help_lines`] the `/help` output.
//! - Parsing and suggestions only walk nodes the [`Source`] may use (SEC-11); every parser
//!   is bounded by [`Limits`] (SEC-04).

pub mod argument;
pub mod bounds;
pub mod coords;
pub mod error;
mod game_args;
pub mod game_values;
pub mod limits;
pub mod nbt_path;
mod packet;
pub mod parse;
pub mod reader;
pub mod selector;
mod snbt;
pub mod source;
mod suggest;
pub mod suggestion;
pub mod tree;
mod usage;
pub mod values;

pub use argument::{ArgValue, ArgumentType, GameProfileArg, Message, MessagePart};
pub use error::{Arg, CommandError, ErrorContext};
pub use game_values::*;
pub use limits::Limits;
pub use parse::{Args, ParseResults, Parsed, Stage};
pub use source::Source;
pub use suggestion::{Suggestion, Suggestions, SuggestionsBuilder};
pub use tree::{Builder, Dispatcher, Node, NodeId, NodeKind, Requirement, argument, literal};
pub use vanillite_protocol::commands::StringKind;
