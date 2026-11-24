//! The command tree and suggestions against captures of a Vanilla 26.3 server: the
//! `commands` packet for an operator (level 4) and a player (level 0), and the answers to
//! `command_suggestion` requests.

use vanillite_command::{Args, Source, SuggestionsBuilder};
use vanillite_game::commands::{self, suggest};
use vanillite_protocol::codec::{Decode, Reader};
use vanillite_protocol::commands::{CommandTree, NodeKind};
use vanillite_protocol::packets::play::clientbound::CommandSuggestions;
use vanillite_protocol::text::{Component, Content};

const OUR_COMMANDS: &[&str] = &[
    "execute",
    "function",
    "return",
    "scoreboard",
    "gamemode",
    "gamerule",
    "help",
    "kick",
    "kill",
    "list",
    "msg",
    "tell",
    "w",
    "say",
    "seed",
    "teleport",
    "tp",
    "time",
    "weather",
    "deop",
    "op",
    "save-all",
    "save-off",
    "save-on",
    "stop",
    "whitelist",
    "attribute",
    "clear",
    "data",
    "enchant",
    "experience",
    "xp",
    "give",
    "item",
    "recipe",
    "tag",
];

struct TestSource {
    level: u8,
    entity_type_tags: Vec<String>,
}

impl Source for TestSource {
    fn permission_level(&self) -> u8 {
        self.level
    }

    fn online_player_names(&self) -> Vec<String> {
        vec!["Alice".into(), "Bob".into()]
    }

    fn suggest_custom(&self, provider: &str, _args: &Args, builder: &mut SuggestionsBuilder<'_>) {
        if provider == suggest::OP_CANDIDATES {
            builder.suggest_matching(["Bob"]);
        }
    }

    fn resource_exists(&self, registry: &str, id: &str) -> bool {
        registry != "minecraft:timeline" || id == "minecraft:day"
    }

    fn registry_tags(&self, registry: &str) -> Vec<String> {
        if registry == "minecraft:entity_type" {
            self.entity_type_tags.clone()
        } else {
            Vec::new()
        }
    }
}

fn source(level: u8) -> TestSource {
    TestSource {
        level,
        entity_type_tags: Vec::new(),
    }
}

fn body(file: &str) -> Vec<u8> {
    let path = format!("{}/tests/vectors/{file}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn tree(file: &str) -> CommandTree {
    let raw = body(file);
    let mut r = Reader::new(&raw);
    r.varint().expect("packet id");
    CommandTree::decode(&mut r).expect("commands packet")
}

fn label(t: &CommandTree, i: i32) -> String {
    match t
        .nodes
        .get(usize::try_from(i).unwrap_or(usize::MAX))
        .map(|n| &n.kind)
    {
        Some(NodeKind::Literal(l)) => l.clone(),
        Some(NodeKind::Argument { name, .. }) => format!("<{name}>"),
        _ => String::new(),
    }
}

/// The path of literal and argument names from the root to every node.
fn paths(t: &CommandTree) -> Vec<String> {
    let mut paths = vec![String::new(); t.nodes.len()];
    let mut queue = std::collections::VecDeque::from([t.root]);
    let mut seen = vec![false; t.nodes.len()];
    while let Some(i) = queue.pop_front() {
        let iu = usize::try_from(i).expect("index");
        if std::mem::replace(&mut seen[iu], true) {
            continue;
        }
        for &c in &t.nodes[iu].children {
            let cu = usize::try_from(c).expect("index");
            if !seen[cu] {
                paths[cu] = format!("{} {}", paths[iu], label(t, c)).trim().to_owned();
                queue.push_back(c);
            }
        }
    }
    paths
}

/// A canonical text of a subtree: kinds, parsers, flags, child order and redirects by path.
fn render(t: &CommandTree, i: i32, paths: &[String], depth: usize, out: &mut String) {
    let n = &t.nodes[usize::try_from(i).expect("index")];
    let kind = match &n.kind {
        NodeKind::Root => "root".to_owned(),
        NodeKind::Literal(l) => format!("literal {l}"),
        NodeKind::Argument {
            name,
            parser,
            suggestions,
        } => format!(
            "argument {name} {} {:?} suggestions={:?}",
            parser.name,
            parser.props,
            suggestions.as_ref().map(|s| s.as_str().to_owned())
        ),
    };
    let redirect = n
        .redirect
        .map(|r| format!(" -> {}", paths[usize::try_from(r).expect("index")]))
        .unwrap_or_default();
    out.push_str(&format!(
        "{}{kind} executable={} restricted={}{redirect}\n",
        "  ".repeat(depth),
        n.executable,
        n.restricted
    ));
    for &c in &n.children {
        render(t, c, paths, depth + 1, out);
    }
}

fn root_children(t: &CommandTree) -> Vec<String> {
    t.nodes[usize::try_from(t.root).expect("root")]
        .children
        .iter()
        .map(|&c| label(t, c))
        .collect()
}

fn subtree(t: &CommandTree, command: &str) -> String {
    let paths = paths(t);
    let root = &t.nodes[usize::try_from(t.root).expect("root")];
    let i = root
        .children
        .iter()
        .copied()
        .find(|&c| label(t, c) == command)
        .unwrap_or_else(|| panic!("{command} missing"));
    let mut out = String::new();
    render(t, i, &paths, 0, &mut out);
    out
}

#[test]
fn operator_tree_matches_vanilla() {
    let vanilla = tree("commands_op4.bin");
    let ours = commands::dispatcher().to_tree(&source(4));
    let vanilla_order: Vec<String> = root_children(&vanilla)
        .into_iter()
        .filter(|c| OUR_COMMANDS.contains(&c.as_str()))
        .collect();
    assert_eq!(root_children(&ours), vanilla_order);
    for command in OUR_COMMANDS {
        assert_eq!(
            subtree(&ours, command),
            subtree(&vanilla, command),
            "/{command}"
        );
    }
}

#[test]
fn player_tree_matches_vanilla() {
    let vanilla = tree("commands_level0.bin");
    let ours = commands::dispatcher().to_tree(&source(0));
    let vanilla_ours: Vec<String> = root_children(&vanilla)
        .into_iter()
        .filter(|c| OUR_COMMANDS.contains(&c.as_str()))
        .collect();
    assert_eq!(root_children(&ours), vanilla_ours);
    for command in &vanilla_ours {
        assert_eq!(
            subtree(&ours, command),
            subtree(&vanilla, command),
            "/{command}"
        );
    }
}

fn tooltip_key(c: &Option<Component>) -> Option<String> {
    match c.as_ref().map(|c| &c.content) {
        Some(Content::Translatable { key, .. }) => Some(key.clone()),
        Some(Content::Text(t)) => Some(t.clone()),
        Some(_) | None => None,
    }
}

fn vanilla_suggestions(file: &str) -> CommandSuggestions {
    let raw = body(file);
    let mut r = Reader::new(&raw);
    r.varint().expect("packet id");
    CommandSuggestions::decode(&mut r).expect("command_suggestions")
}

type Answer = (i32, i32, Vec<(String, Option<String>)>);

fn answer(p: &CommandSuggestions, only: Option<&[&str]>) -> Answer {
    let list = p
        .suggestions
        .iter()
        .filter(|s| only.is_none_or(|o| o.contains(&s.text.as_str())))
        .map(|s| (s.text.clone(), tooltip_key(&s.tooltip)))
        .collect();
    (p.start, p.length, list)
}

fn ours(input: &str, source: &TestSource) -> CommandSuggestions {
    let d = commands::dispatcher();
    d.suggest(input, source).to_packet(1, input, 1000)
}

#[test]
fn suggestions_match_vanilla() {
    let op = source(4);
    for (file, input) in [
        ("suggest_s00.bin", "/gamemode "),
        ("suggest_s01.bin", "/tp "),
        ("suggest_s02.bin", "/msg "),
        ("suggest_s03.bin", "/time q"),
        ("suggest_s04.bin", "/whitelist "),
        ("suggest_s05.bin", "/op "),
        ("suggest_s06.bin", "/tp @e["),
        ("suggest_s08.bin", "/help "),
        ("suggest_s10.bin", "/gamemode creative "),
    ] {
        assert_eq!(
            answer(&ours(input, &op), None),
            answer(&vanilla_suggestions(file), None),
            "{input}"
        );
    }
    assert_eq!(
        answer(&ours("/msg ", &source(0)), None),
        answer(&vanilla_suggestions("suggest_sb00.bin"), None),
        "/msg as a player"
    );
}

#[test]
fn root_and_type_suggestions_match_vanilla() {
    let op = source(4);
    let ours_root = answer(&ours("/", &op), None);
    let vanilla_root = answer(&vanilla_suggestions("suggest_s09.bin"), Some(OUR_COMMANDS));
    assert_eq!(ours_root, vanilla_root);

    let vanilla = vanilla_suggestions("suggest_s07.bin");
    let tags = vanilla
        .suggestions
        .iter()
        .filter_map(|s| s.text.strip_prefix('#').map(str::to_owned))
        .collect();
    let with_tags = TestSource {
        level: 4,
        entity_type_tags: tags,
    };
    assert_eq!(
        answer(&ours("/tp @e[type=", &with_tags), None),
        answer(&vanilla, None)
    );
}

#[test]
fn players_get_nothing_of_operator_commands() {
    let player = source(0);
    let d = commands::dispatcher();
    for input in [
        "/stop",
        "/op ",
        "/gamemode ",
        "/tp @e[nbt={",
        "/whitelist add ",
    ] {
        assert!(d.suggest(input, &player).list.is_empty(), "{input}");
    }
    let root = d.suggest("/", &player);
    assert_eq!(root.texts(), ["help", "list", "msg", "tell", "w"]);
}
