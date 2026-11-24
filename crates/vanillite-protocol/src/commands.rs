//! Wire form of the command tree (`ClientboundCommandsPacket`) and of argument parsers
//! (`ArgumentTypeInfos`).

use crate::codec::{Decode, DecodeLimited, Encode, Error, Reader, Result, write_len, write_varint};
use crate::types::Identifier;
use vanillite_data::registry::COMMAND_ARGUMENT_TYPE;

/// Node flags of the commands packet.
mod flag {
    pub const TYPE_MASK: u8 = 3;
    pub const LITERAL: u8 = 1;
    pub const ARGUMENT: u8 = 2;
    pub const EXECUTABLE: u8 = 4;
    pub const REDIRECT: u8 = 8;
    pub const CUSTOM_SUGGESTIONS: u8 = 16;
    pub const RESTRICTED: u8 = 32;
}

/// Brigadier's string argument kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StringKind {
    /// One word without spaces.
    SingleWord,
    /// A word or a quoted phrase.
    QuotablePhrase,
    /// The rest of the input.
    GreedyPhrase,
}

/// The properties an argument parser sends to the client.
#[derive(Debug, Clone, PartialEq)]
pub enum ParserProps {
    /// No properties.
    None,
    /// `brigadier:float` bounds.
    Float { min: Option<f32>, max: Option<f32> },
    /// `brigadier:double` bounds.
    Double { min: Option<f64>, max: Option<f64> },
    /// `brigadier:integer` bounds.
    Integer { min: Option<i32>, max: Option<i32> },
    /// `brigadier:long` bounds.
    Long { min: Option<i64>, max: Option<i64> },
    /// `brigadier:string`.
    String(StringKind),
    /// `minecraft:entity`.
    Entity { single: bool, players_only: bool },
    /// `minecraft:score_holder`.
    ScoreHolder { multiple: bool },
    /// `minecraft:time`: minimum ticks.
    Time { min: i32 },
    /// Registry-bound parsers (`resource`, `resource_key`, `resource_or_tag`, ...).
    Registry(Identifier),
}

/// An argument parser: its name in `minecraft:command_argument_type` and its properties.
#[derive(Debug, Clone, PartialEq)]
pub struct Parser {
    /// Parser name, e.g. `minecraft:entity`.
    pub name: &'static str,
    /// Properties.
    pub props: ParserProps,
}

impl Parser {
    /// A parser without properties.
    pub const fn plain(name: &'static str) -> Self {
        Self {
            name,
            props: ParserProps::None,
        }
    }

    fn id(&self) -> i32 {
        COMMAND_ARGUMENT_TYPE.id(self.name).map_or(0, i32::from)
    }
}

fn bounds_flags(min: bool, max: bool) -> u8 {
    u8::from(min) | (u8::from(max) << 1)
}

impl Encode for Parser {
    fn encode(&self, out: &mut Vec<u8>) {
        write_varint(out, self.id());
        match &self.props {
            ParserProps::None => {}
            ParserProps::Float { min, max } => {
                out.push(bounds_flags(min.is_some(), max.is_some()));
                min.iter().chain(max.iter()).for_each(|v| v.encode(out));
            }
            ParserProps::Double { min, max } => {
                out.push(bounds_flags(min.is_some(), max.is_some()));
                min.iter().chain(max.iter()).for_each(|v| v.encode(out));
            }
            ParserProps::Integer { min, max } => {
                out.push(bounds_flags(min.is_some(), max.is_some()));
                min.iter().chain(max.iter()).for_each(|v| v.encode(out));
            }
            ParserProps::Long { min, max } => {
                out.push(bounds_flags(min.is_some(), max.is_some()));
                min.iter().chain(max.iter()).for_each(|v| v.encode(out));
            }
            ParserProps::String(kind) => write_varint(
                out,
                match kind {
                    StringKind::SingleWord => 0,
                    StringKind::QuotablePhrase => 1,
                    StringKind::GreedyPhrase => 2,
                },
            ),
            ParserProps::Entity {
                single,
                players_only,
            } => out.push(u8::from(*single) | (u8::from(*players_only) << 1)),
            ParserProps::ScoreHolder { multiple } => out.push(u8::from(*multiple)),
            ParserProps::Time { min } => min.encode(out),
            ParserProps::Registry(registry) => registry.encode(out),
        }
    }
}

impl Decode for Parser {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        let id = r.varint()?;
        let name = u16::try_from(id)
            .ok()
            .and_then(|i| COMMAND_ARGUMENT_TYPE.name(i))
            .ok_or(Error::Invalid("unknown argument type"))?;
        let props = match name {
            "brigadier:float" => {
                let f = r.u8()?;
                ParserProps::Float {
                    min: if f & 1 != 0 { Some(r.f32()?) } else { None },
                    max: if f & 2 != 0 { Some(r.f32()?) } else { None },
                }
            }
            "brigadier:double" => {
                let f = r.u8()?;
                ParserProps::Double {
                    min: if f & 1 != 0 { Some(r.f64()?) } else { None },
                    max: if f & 2 != 0 { Some(r.f64()?) } else { None },
                }
            }
            "brigadier:integer" => {
                let f = r.u8()?;
                ParserProps::Integer {
                    min: if f & 1 != 0 { Some(r.i32()?) } else { None },
                    max: if f & 2 != 0 { Some(r.i32()?) } else { None },
                }
            }
            "brigadier:long" => {
                let f = r.u8()?;
                ParserProps::Long {
                    min: if f & 1 != 0 { Some(r.i64()?) } else { None },
                    max: if f & 2 != 0 { Some(r.i64()?) } else { None },
                }
            }
            "brigadier:string" => ParserProps::String(match r.varint()? {
                0 => StringKind::SingleWord,
                1 => StringKind::QuotablePhrase,
                2 => StringKind::GreedyPhrase,
                _ => return Err(Error::Invalid("string argument kind")),
            }),
            "minecraft:entity" => {
                let f = r.u8()?;
                ParserProps::Entity {
                    single: f & 1 != 0,
                    players_only: f & 2 != 0,
                }
            }
            "minecraft:score_holder" => ParserProps::ScoreHolder {
                multiple: r.u8()? & 1 != 0,
            },
            "minecraft:time" => ParserProps::Time { min: r.i32()? },
            "minecraft:resource_or_tag"
            | "minecraft:resource_or_tag_key"
            | "minecraft:resource"
            | "minecraft:resource_key"
            | "minecraft:resource_selector" => ParserProps::Registry(Identifier::decode(r)?),
            _ => ParserProps::None,
        };
        Ok(Self { name, props })
    }
}

/// What a node is.
#[derive(Debug, Clone, PartialEq)]
pub enum NodeKind {
    /// The root.
    Root,
    /// A literal word.
    Literal(String),
    /// An argument.
    Argument {
        /// Argument name.
        name: String,
        /// Its parser.
        parser: Parser,
        /// Suggestion provider the client asks the server for (`minecraft:ask_server`) or
        /// resolves itself.
        suggestions: Option<Identifier>,
    },
}

/// One node of the commands packet.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    /// Kind and payload.
    pub kind: NodeKind,
    /// The command can end at this node.
    pub executable: bool,
    /// The node needs a permission (`Commands.COMMAND_NODE_INSPECTOR.isRestricted`).
    pub restricted: bool,
    /// Child node indices.
    pub children: Vec<i32>,
    /// Redirect target index.
    pub redirect: Option<i32>,
}

impl Encode for Node {
    fn encode(&self, out: &mut Vec<u8>) {
        let mut flags = match &self.kind {
            NodeKind::Root => 0,
            NodeKind::Literal(_) => flag::LITERAL,
            NodeKind::Argument { suggestions, .. } => {
                flag::ARGUMENT
                    | if suggestions.is_some() {
                        flag::CUSTOM_SUGGESTIONS
                    } else {
                        0
                    }
            }
        };
        if self.executable {
            flags |= flag::EXECUTABLE;
        }
        if self.redirect.is_some() {
            flags |= flag::REDIRECT;
        }
        if self.restricted {
            flags |= flag::RESTRICTED;
        }
        out.push(flags);
        write_len(out, self.children.len());
        for &child in &self.children {
            write_varint(out, child);
        }
        if let Some(redirect) = self.redirect {
            write_varint(out, redirect);
        }
        match &self.kind {
            NodeKind::Root => {}
            NodeKind::Literal(literal) => literal.encode(out),
            NodeKind::Argument {
                name,
                parser,
                suggestions,
            } => {
                name.encode(out);
                parser.encode(out);
                if let Some(s) = suggestions {
                    s.encode(out);
                }
            }
        }
    }
}

impl Decode for Node {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        let flags = r.u8()?;
        let n = r.len_prefix("node children", 1 << 16, 1)?;
        let mut children = Vec::with_capacity(n);
        for _ in 0..n {
            children.push(r.varint()?);
        }
        let redirect = if flags & flag::REDIRECT != 0 {
            Some(r.varint()?)
        } else {
            None
        };
        let kind = match flags & flag::TYPE_MASK {
            0 => NodeKind::Root,
            flag::LITERAL => NodeKind::Literal(String::decode_limited(r, 32767)?),
            flag::ARGUMENT => NodeKind::Argument {
                name: String::decode_limited(r, 32767)?,
                parser: Parser::decode(r)?,
                suggestions: if flags & flag::CUSTOM_SUGGESTIONS != 0 {
                    Some(Identifier::decode(r)?)
                } else {
                    None
                },
            },
            _ => return Err(Error::Invalid("command node type")),
        };
        Ok(Self {
            kind,
            executable: flags & flag::EXECUTABLE != 0,
            restricted: flags & flag::RESTRICTED != 0,
            children,
            redirect,
        })
    }
}

/// `ClientboundCommandsPacket.Entry` list plus the root index.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CommandTree {
    /// Nodes in breadth-first order.
    pub nodes: Vec<Node>,
    /// Index of the root node.
    pub root: i32,
}

impl Encode for CommandTree {
    fn encode(&self, out: &mut Vec<u8>) {
        write_len(out, self.nodes.len());
        for node in &self.nodes {
            node.encode(out);
        }
        write_varint(out, self.root);
    }
}

impl Decode for CommandTree {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        let n = r.len_prefix("command nodes", 1 << 16, 2)?;
        let mut nodes = Vec::with_capacity(n);
        for _ in 0..n {
            nodes.push(Node::decode(r)?);
        }
        Ok(Self {
            nodes,
            root: r.varint()?,
        })
    }
}
