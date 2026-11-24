//! Text components against Vanilla 26.3: captured `system_chat` bodies (join message, command
//! feedback with click and hover events, parse errors, death message) decode and re-encode
//! byte for byte; JSON round trips; nesting limits.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::as_conversions,
    clippy::arithmetic_side_effects
)]

use std::path::Path;

use vanillite_nbt::{Compound, List, Tag};
use vanillite_protocol::codec::{DecodeLimited, Encode, Reader};
use vanillite_protocol::packets::play::clientbound::SystemChat;
use vanillite_protocol::text::{
    Arg, ClickEvent, Color, Component, Content, HoverEvent, MAX_DEPTH, NamedColor,
};

fn vectors() -> Vec<(String, Vec<u8>)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/vectors/text");
    let mut out: Vec<_> = std::fs::read_dir(&dir)
        .expect("vector directory")
        .map(|e| {
            let path = e.unwrap().path();
            let name = path.file_stem().unwrap().to_str().unwrap().to_owned();
            (name, std::fs::read(&path).unwrap())
        })
        .collect();
    out.sort();
    out
}

/// The component of a `system_chat` body (packet id, NBT, overlay).
fn component_of(body: &[u8]) -> Component {
    let mut r = Reader::new(body);
    r.varint().unwrap();
    let c = Component::decode_limited(&mut r, 1 << 21).unwrap();
    r.u8().unwrap();
    c
}

#[test]
fn vanilla_system_chat_re_encodes_byte_exact() {
    let vectors = vectors();
    assert!(vectors.len() >= 12);
    let mut failures = Vec::new();
    for (name, body) in &vectors {
        let mut r = Reader::new(body);
        let id = r.varint().unwrap();
        let content = Component::decode_limited(&mut r, 1 << 21)
            .unwrap_or_else(|e| panic!("{name}: does not decode: {e}"));
        let overlay = r.u8().unwrap() != 0;
        let mut out = Vec::new();
        vanillite_protocol::codec::write_varint(&mut out, id);
        SystemChat { content, overlay }.encode(&mut out);
        if out != *body {
            failures.push(name.clone());
        }
    }
    assert!(failures.is_empty(), "re-encoding differs: {failures:?}");
}

#[test]
fn join_message_decodes_with_events() {
    let body = vectors()
        .into_iter()
        .find(|(n, _)| n == "joined")
        .unwrap()
        .1;
    let c = component_of(&body);
    let Content::Translatable { key, args, .. } = &c.content else {
        panic!("not translatable");
    };
    assert_eq!(key, "multiplayer.player.joined");
    assert_eq!(c.style.color, Some(Color::Named(NamedColor::Yellow)));
    let Some(Arg::Component(name)) = args.first() else {
        panic!("no name argument");
    };
    assert_eq!(
        name.style.click_event,
        Some(ClickEvent::SuggestCommand("/tell Bob ".into()))
    );
    assert_eq!(name.style.insertion.as_deref(), Some("Bob"));
    let Some(HoverEvent::ShowEntity { id, uuid, name: n }) = name.style.hover_event.as_deref()
    else {
        panic!("no show_entity");
    };
    assert_eq!(id, "minecraft:player");
    assert_eq!(*uuid, 0xfaa5dca3_c3d4_354b_ae1b_dde9e5a14b3b);
    assert_eq!(n.as_ref().map(Component::plain).as_deref(), Some("Bob"));
}

#[test]
fn built_join_message_matches_vanilla() {
    let body = vectors()
        .into_iter()
        .find(|(n, _)| n == "joined")
        .unwrap()
        .1;
    // Alice sees Bob joining.
    let uuid = 0xfaa5dca3_c3d4_354b_ae1b_dde9e5a14b3b;
    let name = Component::text("Bob")
        .click(ClickEvent::SuggestCommand("/tell Bob ".into()))
        .hover(HoverEvent::ShowEntity {
            id: "minecraft:player".into(),
            uuid,
            name: Some(Component::text("Bob")),
        })
        .insertion("Bob");
    let built = Component::translatable("multiplayer.player.joined", vec![name])
        .color(Color::Named(NamedColor::Yellow));
    assert_eq!(built, component_of(&body));
    let mut expected = Vec::new();
    built.encode(&mut expected);
    let mut r = Reader::new(&body);
    r.varint().unwrap();
    let start = body.len() - r.remaining();
    assert_eq!(&body[start..body.len() - 1], &expected[..]);
}

#[test]
fn json_round_trips() {
    for (name, body) in vectors() {
        let c = component_of(&body);
        let json = c.to_json_string();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        let back = Component::try_from_json(&value)
            .unwrap_or_else(|e| panic!("{name}: JSON does not decode: {e}"));
        // JSON numbers have no width (`4736L` comes back as an int), so compare the JSON.
        assert_eq!(back.to_json_string(), json, "{name}");
    }
}

#[test]
fn json_uses_codec_field_order() {
    let c = Component::text("x")
        .color(Color::Rgb(0x12AB34))
        .bold(true)
        .click(ClickEvent::RunCommand("/say hi".into()))
        .append(Component::text("y"));
    assert_eq!(
        c.to_json_string(),
        r##"{"text":"x","extra":["y"],"color":"#12AB34","bold":true,"click_event":{"command":"/say hi","action":"run_command"}}"##
    );
    assert_eq!(Component::text("plain").to_json_string(), r#""plain""#);
}

#[test]
fn primitive_arguments_keep_their_type() {
    let c = Component::translatable_args(
        "k",
        vec![
            Arg::Int(2),
            Arg::Long(4750),
            Arg::Bool(true),
            Arg::from("s"),
        ],
    );
    let back = Component::try_from_nbt(&c.to_nbt()).unwrap();
    let Content::Translatable { args, .. } = back.content else {
        panic!()
    };
    // Booleans travel as bytes, like Vanilla's `NbtOps.createBoolean`.
    assert_eq!(
        args,
        vec![Arg::Int(2), Arg::Long(4750), Arg::Byte(1), Arg::from("s")]
    );
}

#[test]
fn invalid_components_are_errors() {
    let bad = [
        Tag::Int(1),
        Tag::List(List(vec![])),
        Tag::Compound(Compound::new().with("color", "red")),
        Tag::Compound(Compound::new().with("text", "a").with("color", "nocolor")),
        Tag::Compound(
            Compound::new().with("text", "a").with(
                "click_event",
                Compound::new()
                    .with("action", "run_command")
                    .with("command", "say \u{a7}c"),
            ),
        ),
        Tag::Compound(
            Compound::new().with("text", "a").with(
                "click_event",
                Compound::new()
                    .with("action", "open_url")
                    .with("url", "file:///etc/passwd"),
            ),
        ),
        Tag::Compound(
            Compound::new().with("text", "a").with(
                "click_event",
                Compound::new()
                    .with("action", "open_file")
                    .with("path", "/etc/passwd"),
            ),
        ),
        Tag::Compound(
            Compound::new()
                .with("text", "a")
                .with("extra", List(vec![])),
        ),
    ];
    for tag in bad {
        assert!(Component::try_from_nbt(&tag).is_err(), "{tag:?}");
    }
}

type Wrap = fn(Tag) -> Tag;

/// The three ways components nest: siblings, translation arguments and hover texts.
const WRAPS: [Wrap; 3] = [
    |inner| {
        Tag::Compound(
            Compound::new()
                .with("text", "")
                .with("extra", List(vec![inner])),
        )
    },
    |inner| {
        Tag::Compound(
            Compound::new()
                .with("translate", "k")
                .with("with", List(vec![inner])),
        )
    },
    |inner| {
        Tag::Compound(
            Compound::new().with("text", "").with(
                "hover_event",
                Compound::new()
                    .with("action", "show_text")
                    .with("value", inner),
            ),
        )
    },
];

/// Decodes on a thread with the stack of a network worker (2 MiB).
fn decodes_on_small_stack(tag: Tag) -> bool {
    std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(move || {
            let ok = Component::try_from_nbt(&tag).is_ok();
            // Dropping a deep value recursively is not under test here.
            std::mem::forget(tag);
            ok
        })
        .unwrap()
        .join()
        .unwrap()
}

fn nested(wrap: Wrap, depth: usize) -> Tag {
    let mut tag = Tag::String("x".into());
    for _ in 0..depth {
        tag = wrap(tag);
    }
    tag
}

/// The deepest nesting Vanilla's NBT limit allows decodes; deeper nesting fails cleanly
/// instead of overflowing the stack.
#[test]
fn nesting_is_limited() {
    for wrap in WRAPS {
        assert!(decodes_on_small_stack(nested(wrap, MAX_DEPTH)));
        assert!(!decodes_on_small_stack(nested(wrap, 2000)));
    }
}

#[test]
fn nested_limit_allows_moderate_depth() {
    let mut c = Component::text("x");
    for _ in 0..100 {
        c = Component::text("").append(c);
    }
    assert_eq!(Component::try_from_nbt(&c.to_nbt()).unwrap(), c);
}
