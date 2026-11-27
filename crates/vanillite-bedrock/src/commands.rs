//! The Java command tree (Brigadier) as Bedrock's `available_commands`: every path through the
//! tree becomes an overload, literals become enums and argument parsers Bedrock parameter types,
//! so Bedrock clients show the same commands with syntax hints and completions. Each command
//! also accepts free text, so a line Bedrock cannot parse still reaches the server, which has
//! the last word. Arguments whose suggestions the Java client asks the server for
//! (`minecraft:ask_server`: advancements, functions, boss bars, ...) become soft enums; the
//! frontend asks the server once per argument ([`Probe`]) and fills them in.

use rustc_hash::FxHashMap;
use vanillite_protocol::commands::{CommandTree, NodeKind, ParserProps, StringKind};

use crate::io;

const VALID: u32 = 0x10_0000;
const ENUM: u32 = 0x20_0000;
const SOFT_ENUM: u32 = 0x400_0000;

/// Most server-suggested arguments asked for.
const MAX_PROBES: usize = 64;

/// A soft enum to fill with the server's suggestions for a command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    /// The soft enum's name.
    pub name: String,
    /// The command line up to the argument (`/advancement grant @s only `).
    pub line: String,
}

mod arg {
    pub const INT: u32 = 1;
    pub const FLOAT: u32 = 3;
    pub const TARGET: u32 = 8;
    pub const STRING: u32 = 56;
    pub const BLOCK_POSITION: u32 = 64;
    pub const POSITION: u32 = 65;
    pub const MESSAGE: u32 = 67;
    pub const RAW_TEXT: u32 = 70;
    pub const JSON: u32 = 74;
}

/// Longest path through one command.
const MAX_DEPTH: usize = 12;
/// Overloads per command (more fall back to free text).
const MAX_OVERLOADS: usize = 48;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Param {
    name: String,
    kind: u32,
    optional: bool,
}

/// Builds the Bedrock enums and commands.
#[derive(Debug, Default)]
struct Builder {
    values: Vec<String>,
    value_index: FxHashMap<String, u32>,
    enums: Vec<(String, Vec<u32>)>,
    enum_index: FxHashMap<String, u32>,
    /// Soft enums: name and the line their suggestions come from.
    probes: Vec<Probe>,
}

impl Builder {
    fn value(&mut self, v: &str) -> u32 {
        if let Some(&i) = self.value_index.get(v) {
            return i;
        }
        let i = u32::try_from(self.values.len()).unwrap_or(u32::MAX);
        self.values.push(v.to_owned());
        self.value_index.insert(v.to_owned(), i);
        i
    }

    /// The enum `name` with `values` (the first definition of a name wins).
    fn enumeration<'a>(&mut self, name: &str, values: impl IntoIterator<Item = &'a str>) -> u32 {
        if let Some(&i) = self.enum_index.get(name) {
            return i;
        }
        let indices = values.into_iter().map(|v| self.value(v)).collect();
        let i = u32::try_from(self.enums.len()).unwrap_or(u32::MAX);
        self.enums.push((name.to_owned(), indices));
        self.enum_index.insert(name.to_owned(), i);
        i
    }

    fn registry_enum(&mut self, name: &str, registry: &str) -> Option<u32> {
        if let Some(&i) = self.enum_index.get(name) {
            return Some(i);
        }
        let r = vanillite_data::registry::Registry::by_name(registry)?;
        let names: Vec<&'static str> = (0..r.len())
            .filter_map(|i| r.name(u16::try_from(i).ok()?))
            .map(|n| n.strip_prefix("minecraft:").unwrap_or(n))
            .collect();
        Some(self.enumeration(name, names))
    }

    /// A soft enum filled from the server's suggestions for `line`.
    fn soft_enum(&mut self, name: &str, line: &str) -> Option<u32> {
        if let Some(i) = self.probes.iter().position(|p| p.line == line) {
            return u32::try_from(i).ok();
        }
        if self.probes.len() >= MAX_PROBES {
            return None;
        }
        let mut unique = name.to_owned();
        let mut n = 1u32;
        while self.probes.iter().any(|p| p.name == unique) {
            n = n.saturating_add(1);
            unique = format!("{name}{n}");
        }
        self.probes.push(Probe {
            name: unique,
            line: line.to_owned(),
        });
        u32::try_from(self.probes.len().saturating_sub(1)).ok()
    }

    /// The Bedrock parameter of a Java argument.
    fn argument(&mut self, name: &str, parser: &vanillite_protocol::commands::Parser) -> Param {
        let simple = |kind: u32| VALID | kind;
        let kind = match (parser.name, &parser.props) {
            ("brigadier:bool", _) => ENUM | VALID | self.enumeration("Boolean", ["true", "false"]),
            ("brigadier:integer" | "brigadier:long", _) => simple(arg::INT),
            ("brigadier:float" | "brigadier:double", _) => simple(arg::FLOAT),
            ("brigadier:string", ParserProps::String(StringKind::GreedyPhrase)) => {
                simple(arg::RAW_TEXT)
            }
            ("minecraft:entity" | "minecraft:game_profile" | "minecraft:score_holder", _) => {
                simple(arg::TARGET)
            }
            ("minecraft:vec3" | "minecraft:vec2", _) => simple(arg::POSITION),
            ("minecraft:block_pos" | "minecraft:column_pos", _) => simple(arg::BLOCK_POSITION),
            ("minecraft:message", _) => simple(arg::MESSAGE),
            ("minecraft:component" | "minecraft:nbt_compound_tag" | "minecraft:style", _) => {
                simple(arg::JSON)
            }
            ("minecraft:gamemode", _) => {
                ENUM | VALID
                    | self.enumeration(
                        "GameMode",
                        ["survival", "creative", "adventure", "spectator"],
                    )
            }
            ("minecraft:item_stack" | "minecraft:item_predicate", _) => self
                .registry_enum("Item", "minecraft:item")
                .map_or(simple(arg::STRING), |e| ENUM | VALID | e),
            ("minecraft:block_state" | "minecraft:block_predicate", _) => self
                .registry_enum("Block", "minecraft:block")
                .map_or(simple(arg::STRING), |e| ENUM | VALID | e),
            (_, ParserProps::Registry(id)) if id.as_str() == "minecraft:entity_type" => self
                .registry_enum("EntityType", "minecraft:entity_type")
                .map_or(simple(arg::STRING), |e| ENUM | VALID | e),
            (_, ParserProps::Registry(id)) if id.as_str() == "minecraft:mob_effect" => self
                .registry_enum("Effect", "minecraft:mob_effect")
                .map_or(simple(arg::STRING), |e| ENUM | VALID | e),
            _ => simple(arg::STRING),
        };
        Param {
            name: name.to_owned(),
            kind,
            optional: false,
        }
    }
}

/// Collects the overloads below `node` (the path so far in `path`).
/// A text that parses as the argument, to ask for the suggestions of later arguments.
fn sample(parser: &vanillite_protocol::commands::Parser) -> Option<&'static str> {
    Some(match (parser.name, &parser.props) {
        ("brigadier:bool", _) => "true",
        ("brigadier:integer" | "brigadier:long" | "brigadier:float" | "brigadier:double", _) => "0",
        ("brigadier:string", ParserProps::String(StringKind::GreedyPhrase)) => return None,
        ("brigadier:string", _) => "x",
        ("minecraft:entity" | "minecraft:game_profile" | "minecraft:score_holder", _) => "@s",
        ("minecraft:vec3" | "minecraft:block_pos", _) => "~ ~ ~",
        ("minecraft:vec2" | "minecraft:column_pos", _) => "~ ~",
        ("minecraft:gamemode", _) => "survival",
        ("minecraft:resource_location" | "minecraft:identifier", _) => "minecraft:x",
        _ => return None,
    })
}

fn walk(
    b: &mut Builder,
    tree: &CommandTree,
    node: usize,
    path: &mut Vec<Param>,
    line: Option<String>,
    out: &mut Vec<Vec<Param>>,
) -> bool {
    if out.len() > MAX_OVERLOADS || path.len() > MAX_DEPTH {
        return false;
    }
    let Some(n) = tree.nodes.get(node) else {
        return true;
    };
    if n.redirect.is_some() {
        // `execute run ...` and aliases: the rest is free text.
        let mut p = path.clone();
        p.push(Param {
            name: "args".into(),
            kind: VALID | arg::RAW_TEXT,
            optional: true,
        });
        out.push(p);
        return true;
    }
    if n.executable && !path.is_empty() {
        out.push(path.clone());
    }
    for &c in &n.children {
        let Some(child) = usize::try_from(c).ok().filter(|&c| c < tree.nodes.len()) else {
            continue;
        };
        let (param, next) = match tree.nodes.get(child).map(|n| &n.kind) {
            Some(NodeKind::Literal(l)) => (
                Param {
                    name: l.clone(),
                    kind: ENUM | VALID | b.enumeration(l, [l.as_str()]),
                    optional: false,
                },
                line.as_ref().map(|s| format!("{s}{l} ")),
            ),
            Some(NodeKind::Argument {
                name,
                parser,
                suggestions,
            }) => {
                let asks = suggestions
                    .as_ref()
                    .is_some_and(|s| s.as_str() == "minecraft:ask_server");
                let soft = line
                    .as_deref()
                    .filter(|_| asks)
                    .and_then(|l| b.soft_enum(name, l));
                let param = match soft {
                    Some(i) => Param {
                        name: name.clone(),
                        kind: SOFT_ENUM | VALID | i,
                        optional: false,
                    },
                    None => b.argument(name, parser),
                };
                let next = line
                    .as_ref()
                    .zip(sample(parser))
                    .map(|(s, v)| format!("{s}{v} "));
                (param, next)
            }
            _ => continue,
        };
        path.push(param);
        let ok = walk(b, tree, child, path, next, out);
        path.pop();
        if !ok {
            return false;
        }
    }
    true
}

/// The body of `available_commands` for a Java command tree, and the soft enums to fill.
pub fn available_commands(tree: &CommandTree) -> (Vec<u8>, Vec<Probe>) {
    let mut b = Builder::default();
    let root = usize::try_from(tree.root)
        .ok()
        .and_then(|r| tree.nodes.get(r));
    let mut commands: Vec<(String, Vec<Vec<Param>>)> = Vec::new();
    for &c in root.map(|r| r.children.as_slice()).unwrap_or_default() {
        let Some(index) = usize::try_from(c).ok() else {
            continue;
        };
        let Some(NodeKind::Literal(name)) = tree.nodes.get(index).map(|n| &n.kind) else {
            continue;
        };
        let mut overloads = Vec::new();
        let mut path = Vec::new();
        let line = Some(format!("/{name} "));
        if !walk(&mut b, tree, index, &mut path, line, &mut overloads) {
            overloads.clear();
        }
        if tree.nodes.get(index).is_some_and(|n| n.executable) {
            overloads.insert(0, Vec::new());
        }
        // Free text last, so anything Bedrock cannot parse still reaches the server.
        overloads.push(vec![Param {
            name: "args".into(),
            kind: VALID | arg::RAW_TEXT,
            optional: true,
        }]);
        commands.push((name.clone(), overloads));
    }
    let mut out = Vec::with_capacity(4096);
    io::write_len(&mut out, b.values.len());
    for v in &b.values {
        io::write_str(&mut out, v);
    }
    io::write_varu32(&mut out, 0); // chained subcommand values
    io::write_varu32(&mut out, 0); // suffixes
    io::write_len(&mut out, b.enums.len());
    for (name, values) in &b.enums {
        io::write_str(&mut out, name);
        io::write_len(&mut out, values.len());
        for v in values {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    io::write_varu32(&mut out, 0); // chained subcommands
    io::write_len(&mut out, commands.len());
    for (name, overloads) in &commands {
        io::write_str(&mut out, name);
        io::write_str(&mut out, "");
        out.extend_from_slice(&0u16.to_le_bytes());
        io::write_str(&mut out, "any");
        out.extend_from_slice(&u32::MAX.to_le_bytes()); // no aliases
        io::write_varu32(&mut out, 0); // chained subcommand offsets
        io::write_len(&mut out, overloads.len());
        for params in overloads {
            out.push(0); // not chaining
            io::write_len(&mut out, params.len());
            for p in params {
                io::write_str(&mut out, &p.name);
                out.extend_from_slice(&p.kind.to_le_bytes());
                out.push(u8::from(p.optional));
                out.push(0);
            }
        }
    }
    // Dynamic (soft) enums, empty until the server's suggestions arrive.
    io::write_len(&mut out, b.probes.len());
    for p in &b.probes {
        io::write_str(&mut out, &p.name);
        io::write_varu32(&mut out, 0);
    }
    io::write_varu32(&mut out, 0); // constraints
    (out, b.probes)
}

/// `update_soft_enum`: sets the values of a soft enum.
pub fn update_soft_enum(name: &str, values: &[String]) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_str(&mut out, name);
    io::write_len(&mut out, values.len());
    for v in values {
        io::write_str(&mut out, v);
    }
    out.push(2); // set
    out
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]
mod tests {
    use super::*;
    use vanillite_protocol::commands::{Node, Parser};

    fn node(kind: NodeKind, executable: bool, children: Vec<i32>) -> Node {
        Node {
            kind,
            executable,
            restricted: false,
            children,
            redirect: None,
        }
    }

    #[test]
    fn literals_become_enums_and_arguments_types() {
        let tree = CommandTree {
            root: 0,
            nodes: vec![
                node(NodeKind::Root, false, vec![1]),
                node(NodeKind::Literal("gamemode".into()), false, vec![2]),
                node(
                    NodeKind::Argument {
                        name: "gamemode".into(),
                        parser: Parser {
                            name: "minecraft:gamemode",
                            props: ParserProps::None,
                        },
                        suggestions: None,
                    },
                    true,
                    vec![3],
                ),
                node(
                    NodeKind::Argument {
                        name: "target".into(),
                        parser: Parser {
                            name: "minecraft:entity",
                            props: ParserProps::Entity {
                                single: false,
                                players_only: true,
                            },
                        },
                        suggestions: None,
                    },
                    true,
                    vec![],
                ),
            ],
        };
        let (body, probes) = available_commands(&tree);
        assert!(probes.is_empty());
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("gamemode"));
        assert!(text.contains("spectator"));
        assert!(text.contains("GameMode"));
        assert!(text.contains("target"));
    }

    #[test]
    fn server_suggestions_become_soft_enums() {
        let tree = CommandTree {
            root: 0,
            nodes: vec![
                node(NodeKind::Root, false, vec![1]),
                node(NodeKind::Literal("bossbar".into()), false, vec![2]),
                node(NodeKind::Literal("get".into()), false, vec![3]),
                node(
                    NodeKind::Argument {
                        name: "id".into(),
                        parser: Parser {
                            name: "minecraft:resource_location",
                            props: ParserProps::None,
                        },
                        suggestions: Some(
                            vanillite_protocol::types::Identifier::new("minecraft:ask_server")
                                .unwrap(),
                        ),
                    },
                    true,
                    vec![],
                ),
            ],
        };
        let (body, probes) = available_commands(&tree);
        assert_eq!(
            probes,
            vec![Probe {
                name: "id".into(),
                line: "/bossbar get ".into()
            }]
        );
        // The soft enum is declared (empty) after the commands.
        assert!(body.ends_with(&[2, b'i', b'd', 0, 0]));
    }
}
