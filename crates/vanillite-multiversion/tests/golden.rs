//! Golden translations 26.3 ↔ 26.2, one test per packet family. The 26.3 side is what the
//! server encodes (its typed packets where it has them); the expected 26.2 bytes are written
//! out by hand from the 26.2 wire format, with ids from `data/multiversion/26.2/` and
//! `data/vanilla/`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::as_conversions
)]

use serde_json::Value;
use vanillite_multiversion::{Accepted, ClientboundHalf, ServerboundHalf, State};
use vanillite_protocol::codec::{Encode, write_str, write_varint};
use vanillite_protocol::frame::encode_body;
use vanillite_protocol::packets::play::clientbound::{
    EntityPositionSync, MoveEntityPos, MoveEntityRot, PlayerPosition, PositionMoveRotation,
    PositionPath, VecDelta,
};
use vanillite_protocol::types::Vec3;

const OLD: i32 = 776;

fn json(path: &str) -> Value {
    let p = vanillite_multiversion::data_paths::file(path);
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

/// Ids of both versions by name.
struct Ids {
    old: Value,
    new: Value,
    old_packets: Value,
    new_packets: Value,
    old_blocks: Value,
    new_blocks: Value,
}

impl Ids {
    fn load() -> Self {
        Self {
            old: json("data/multiversion/26.2/registries.json"),
            new: json("data/vanilla/registries.json"),
            old_packets: json("data/multiversion/26.2/packets.json"),
            new_packets: json("data/vanilla/packets.json"),
            old_blocks: json("data/multiversion/26.2/blocks.json"),
            new_blocks: json("data/vanilla/blocks.json"),
        }
    }

    fn find(list: &Value, name: &str) -> i32 {
        let name = if name.contains(':') {
            name.to_owned()
        } else {
            format!("minecraft:{name}")
        };
        let pos = list
            .as_array()
            .unwrap()
            .iter()
            .position(|e| e.as_str() == Some(name.as_str()))
            .unwrap_or_else(|| panic!("{name} not found"));
        i32::try_from(pos).unwrap()
    }

    fn old_id(&self, registry: &str, name: &str) -> i32 {
        Self::find(&self.old[registry]["entries"], name)
    }

    fn new_id(&self, registry: &str, name: &str) -> i32 {
        Self::find(&self.new[registry]["entries"], name)
    }

    fn old_packet(&self, state: &str, dir: &str, name: &str) -> i32 {
        Self::find(&self.old_packets[state][dir], name)
    }

    fn new_packet(&self, state: &str, dir: &str, name: &str) -> i32 {
        Self::find(&self.new_packets[state][dir], name)
    }

    fn state(blocks: &Value, block: &str) -> i32 {
        let b = blocks
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["name"] == format!("minecraft:{block}"))
            .unwrap();
        i32::try_from(b["default_state"].as_u64().unwrap()).unwrap()
    }

    fn old_state(&self, block: &str) -> i32 {
        Self::state(&self.old_blocks, block)
    }

    fn new_state(&self, block: &str) -> i32 {
        Self::state(&self.new_blocks, block)
    }
}

fn halves(state: State) -> (ServerboundHalf, ClientboundHalf) {
    Accepted::all()
        .adapter(OLD, state)
        .expect("26.2 is supported")
        .split()
}

fn body(id: i32, fields: &[u8]) -> Vec<u8> {
    let mut b = Vec::new();
    write_varint(&mut b, id);
    b.extend_from_slice(fields);
    b
}

fn cb(half: &mut ClientboundHalf, body: &[u8]) -> Option<Vec<u8>> {
    half.translate(body)
        .expect("translates")
        .map(<[u8]>::to_vec)
}

fn sb(half: &mut ServerboundHalf, body: &[u8]) -> Option<Vec<u8>> {
    half.translate(body)
        .expect("translates")
        .map(<[u8]>::to_vec)
}

fn varint(v: i32) -> Vec<u8> {
    let mut b = Vec::new();
    write_varint(&mut b, v);
    b
}

#[test]
fn entity_movement() {
    let ids = Ids::load();
    let (_, mut c) = halves(State::Play);
    let id = |n: &str| ids.old_packet("play", "clientbound", n);

    // Several steps become their sum.
    let stepped = encode_body(&MoveEntityPos {
        entity_id: 7,
        delta: VecDelta::Stepped(vec![(1, [100, -20, 3]), (2, [50, 20, -3])]),
        on_ground: true,
    });
    let mut want = varint(id("move_entity_pos"));
    want.extend_from_slice(&[7, 0, 150, 0, 0, 0, 0, 1]);
    assert_eq!(cb(&mut c, &stepped), Some(want));

    // Rotation: on ground moves to the end.
    let rot = encode_body(&MoveEntityRot {
        entity_id: 7,
        on_ground: true,
        y_rot: 10,
        x_rot: -2,
    });
    let mut want = varint(id("move_entity_rot"));
    want.extend_from_slice(&[7, 10, 0xfe, 1]);
    assert_eq!(cb(&mut c, &rot), Some(want));

    // A stepped path ends at its last position; the movement is zero.
    let end = Vec3 {
        x: 1.0,
        y: 2.0,
        z: 3.0,
    };
    let sync = encode_body(&EntityPositionSync {
        entity_id: 7,
        path: PositionPath::Stepped(vec![
            (
                Vec3 {
                    x: 0.5,
                    y: 2.0,
                    z: 3.0,
                },
                1,
            ),
            (end, 2),
        ]),
        y_rot: 90.0,
        x_rot: 0.0,
        on_ground: false,
    });
    let mut want = varint(id("entity_position_sync"));
    want.push(7);
    end.encode(&mut want);
    want.extend_from_slice(&[0; 24]);
    want.extend_from_slice(&90.0f32.to_be_bytes());
    want.extend_from_slice(&0.0f32.to_be_bytes());
    want.push(0);
    assert_eq!(cb(&mut c, &sync), Some(want));
}

/// One section: a 4-bit indirect palette of air and two blocks, single-valued biomes.
fn section(states: &[i32], biome: i32) -> Vec<u8> {
    let mut s = vec![0, 2, 0, 0, 4];
    write_varint(&mut s, i32::try_from(states.len()).unwrap());
    for st in states {
        write_varint(&mut s, *st);
    }
    s.extend_from_slice(&[0x11; 256 * 8]);
    s.push(0);
    write_varint(&mut s, biome);
    s
}

#[test]
fn chunk_and_light() {
    let ids = Ids::load();
    let (_, mut c) = halves(State::Play);
    // A 26.3-only block (shown as the closest 26.2 block, oak planks) and a block whose id shifted.
    let new_states = [
        0,
        ids.new_state("poplar_planks"),
        ids.new_state("bookshelf"),
    ];
    let old_states = [0, ids.old_state("oak_planks"), ids.old_state("bookshelf")];
    assert_ne!(
        new_states[2], old_states[2],
        "the test needs a shifted state"
    );

    let chunk = |states: &[i32], light_masks: &[u8], light_longs: bool| {
        let mut b = Vec::new();
        b.extend_from_slice(&3i32.to_be_bytes());
        b.extend_from_slice(&(-4i32).to_be_bytes());
        b.push(0); // no heightmaps
        let s = section(states, 0);
        write_varint(&mut b, i32::try_from(s.len()).unwrap());
        b.extend_from_slice(&s);
        b.push(0); // no block entities
        for _ in 0..4 {
            if light_longs {
                b.push(1);
                let mut w = [0u8; 8];
                w[..light_masks.len()].copy_from_slice(light_masks);
                b.extend_from_slice(&u64::from_le_bytes(w).to_be_bytes());
            } else {
                b.push(u8::try_from(light_masks.len()).unwrap());
                b.extend_from_slice(light_masks);
            }
        }
        b.extend_from_slice(&[0, 0]); // no light arrays
        b
    };
    let new = body(
        ids.new_packet("play", "clientbound", "level_chunk_with_light"),
        &chunk(&new_states, &[0x05, 0x01], false),
    );
    let want = body(
        ids.old_packet("play", "clientbound", "level_chunk_with_light"),
        &chunk(&old_states, &[0x05, 0x01], true),
    );
    assert_eq!(cb(&mut c, &new), Some(want));
}

#[test]
fn direct_palette_is_repacked() {
    let ids = Ids::load();
    let (_, mut c) = halves(State::Play);
    // 26.3: 16 bits per state, 4 per long; 26.2: 15 bits, 4 per long.
    let (new, old) = (ids.new_state("bookshelf"), ids.old_state("bookshelf"));
    let mut section_new = vec![0, 1, 0, 0, 16];
    let mut section_old = vec![0, 1, 0, 0, 15];
    for _ in 0..1024 {
        let w = (0..4).fold(0u64, |w, i| w | u64::try_from(new).unwrap() << (16 * i));
        section_new.extend_from_slice(&w.to_be_bytes());
        let w = (0..4).fold(0u64, |w, i| w | u64::try_from(old).unwrap() << (15 * i));
        section_old.extend_from_slice(&w.to_be_bytes());
    }
    for s in [&mut section_new, &mut section_old] {
        s.extend_from_slice(&[0, 0]);
    }
    let wrap = |s: &[u8], id: i32, light: &[u8]| {
        let mut b = Vec::new();
        b.extend_from_slice(&[0; 8]);
        b.push(0);
        write_varint(&mut b, i32::try_from(s.len()).unwrap());
        b.extend_from_slice(s);
        b.push(0);
        b.extend_from_slice(light);
        body(id, &b)
    };
    let new = wrap(
        &section_new,
        ids.new_packet("play", "clientbound", "level_chunk_with_light"),
        &[0, 0, 0, 0, 0, 0],
    );
    let want = wrap(
        &section_old,
        ids.old_packet("play", "clientbound", "level_chunk_with_light"),
        &[0, 0, 0, 0, 0, 0],
    );
    assert_eq!(cb(&mut c, &new), Some(want));
}

#[test]
fn items_and_components() {
    let ids = Ids::load();
    let (_, mut c) = halves(State::Play);
    let comp_new = |n: &str| ids.new_id("minecraft:data_component_type", n);
    let comp_old = |n: &str| ids.old_id("minecraft:data_component_type", n);
    let item_new = |n: &str| ids.new_id("minecraft:item", n);
    let item_old = |n: &str| ids.old_id("minecraft:item", n);

    // A diamond sword with damage 5, the renamed attack animation, a 26.3-only component
    // (villager_food) and a removed 26.3-only default (waxed).
    let stack = |old: bool| {
        let mut s = varint(1);
        if old {
            s.extend(varint(item_old("diamond_sword")));
            s.extend(varint(2));
            s.extend(varint(0));
            s.extend(varint(comp_old("damage")));
            s.extend(varint(5));
            s.extend(varint(comp_old("swing_animation")));
            s.extend([1, 6]);
        } else {
            s.extend(varint(item_new("diamond_sword")));
            s.extend(varint(3));
            s.extend(varint(1));
            s.extend(varint(comp_new("damage")));
            s.extend(varint(5));
            s.extend(varint(comp_new("attack_animation")));
            s.extend([1, 6]);
            s.extend(varint(comp_new("villager_food")));
            s.extend(varint(4));
            s.extend(varint(comp_new("waxed")));
        }
        s
    };
    let slot = |id: i32, old: bool| {
        let mut b = vec![0, 3, 0, 36];
        b.extend(stack(old));
        body(id, &b)
    };
    let new = slot(
        ids.new_packet("play", "clientbound", "container_set_slot"),
        false,
    );
    let want = slot(
        ids.old_packet("play", "clientbound", "container_set_slot"),
        true,
    );
    assert_eq!(cb(&mut c, &new), Some(want));

    // A shulker box whose contents hold a 26.3-only item: nested stacks are rewritten too.
    let shulker = |old: bool| {
        let mut s = varint(1);
        let (item, comp, inner) = if old {
            (
                item_old("shulker_box"),
                comp_old("container"),
                item_old("oak_planks"),
            )
        } else {
            (
                item_new("shulker_box"),
                comp_new("container"),
                item_new("poplar_planks"),
            )
        };
        s.extend(varint(item));
        s.extend([1, 0]);
        s.extend(varint(comp));
        s.extend(varint(1)); // one slot: present, item, count, empty patch
        s.push(1);
        s.extend(varint(inner));
        s.extend(varint(64));
        s.extend([0, 0]);
        s
    };
    let cursor = |id: i32, old: bool| body(id, &shulker(old));
    let new = cursor(
        ids.new_packet("play", "clientbound", "set_cursor_item"),
        false,
    );
    let want = cursor(
        ids.old_packet("play", "clientbound", "set_cursor_item"),
        true,
    );
    assert_eq!(cb(&mut c, &new), Some(want));
}

#[test]
fn entity_data_and_spawn() {
    let ids = Ids::load();
    let (_, mut c) = halves(State::Play);
    // add_entity of a falling block: type and block state rewritten.
    let add = |old: bool| {
        let (packet, kind, state) = if old {
            (
                ids.old_packet("play", "clientbound", "add_entity"),
                ids.old_id("minecraft:entity_type", "falling_block"),
                ids.old_state("bookshelf"),
            )
        } else {
            (
                ids.new_packet("play", "clientbound", "add_entity"),
                ids.new_id("minecraft:entity_type", "falling_block"),
                ids.new_state("bookshelf"),
            )
        };
        let mut b = varint(9);
        b.extend_from_slice(&[0xab; 16]);
        b.extend(varint(kind));
        b.extend_from_slice(&[0; 24]);
        b.push(0); // no movement
        b.extend_from_slice(&[1, 2, 3]);
        b.extend(varint(state));
        body(packet, &b)
    };
    assert_eq!(cb(&mut c, &add(false)), Some(add(true)));

    // A cushion (26.3 only) is not shown.
    let mut cushion = varint(10);
    cushion.extend_from_slice(&[0xcd; 16]);
    cushion.extend(varint(ids.new_id("minecraft:entity_type", "cushion")));
    cushion.extend_from_slice(&[0; 24]);
    cushion.extend_from_slice(&[0, 0, 0, 0]);
    cushion.push(0);
    let cushion = body(
        ids.new_packet("play", "clientbound", "add_entity"),
        &cushion,
    );
    assert_eq!(cb(&mut c, &cushion), None);

    // Entity data: a block state and an item, then a dye color (26.3 only) that is left out.
    let data = |old: bool| {
        let mut b = varint(9);
        b.push(8);
        b.extend(varint(14));
        b.extend(varint(if old {
            ids.old_state("bookshelf")
        } else {
            ids.new_state("bookshelf")
        }));
        b.push(9);
        b.extend(varint(7));
        b.extend(varint(1));
        b.extend(varint(if old {
            ids.old_id("minecraft:item", "oak_planks")
        } else {
            ids.new_id("minecraft:item", "poplar_planks")
        }));
        b.extend([0, 0]);
        if !old {
            b.push(10);
            b.extend(varint(43));
            b.extend(varint(3));
        }
        b.push(0xff);
        let packet = if old {
            ids.old_packet("play", "clientbound", "set_entity_data")
        } else {
            ids.new_packet("play", "clientbound", "set_entity_data")
        };
        body(packet, &b)
    };
    assert_eq!(cb(&mut c, &data(false)), Some(data(true)));
}

#[test]
fn configuration() {
    let ids = Ids::load();
    let (mut s, mut c) = halves(State::Configuration);
    let cfg_old = |n: &str| ids.old_packet("configuration", "clientbound", n);
    let cfg_new = |n: &str| ids.new_packet("configuration", "clientbound", n);

    // Known packs carry the client's version both ways.
    let packs = |id: i32, version: &str| {
        let mut b = varint(1);
        for v in ["minecraft", "core", version] {
            write_str(&mut b, v);
        }
        body(id, &b)
    };
    assert_eq!(
        cb(&mut c, &packs(cfg_new("select_known_packs"), "26.3")),
        Some(packs(cfg_old("select_known_packs"), "26.2"))
    );
    let sb_id = ids.old_packet("configuration", "serverbound", "select_known_packs");
    let sb_new = ids.new_packet("configuration", "serverbound", "select_known_packs");
    assert_eq!(
        sb(&mut s, &packs(sb_id, "26.2")),
        Some(packs(sb_new, "26.3"))
    );

    // Biomes: the 26.3-only biome is left out, later ones move up.
    let registry = |id: i32, name: &str, entries: &[&str]| {
        let mut b = Vec::new();
        write_str(&mut b, name);
        b.extend(varint(i32::try_from(entries.len()).unwrap()));
        for e in entries {
            write_str(&mut b, e);
            b.push(0);
        }
        body(id, &b)
    };
    let biomes_new = [
        "minecraft:badlands",
        "minecraft:dappled_forest",
        "minecraft:forest",
    ];
    let biomes_old = ["minecraft:badlands", "minecraft:forest"];
    assert_eq!(
        cb(
            &mut c,
            &registry(
                cfg_new("registry_data"),
                "minecraft:worldgen/biome",
                &biomes_new
            )
        ),
        Some(registry(
            cfg_old("registry_data"),
            "minecraft:worldgen/biome",
            &biomes_old
        ))
    );
    // A registry 26.2 does not synchronise is not sent, nor are its tags.
    assert_eq!(
        cb(
            &mut c,
            &registry(
                cfg_new("registry_data"),
                "minecraft:block_transformer",
                &["minecraft:axe"]
            )
        ),
        None
    );
    let tags = |id: i32, entries: &[(&str, &str, &[i32])]| {
        let mut b = varint(i32::try_from(entries.len()).unwrap());
        for (registry, tag, values) in entries {
            write_str(&mut b, registry);
            b.extend(varint(1));
            write_str(&mut b, tag);
            b.extend(varint(i32::try_from(values.len()).unwrap()));
            for v in *values {
                b.extend(varint(*v));
            }
        }
        body(id, &b)
    };
    let planks_new = [
        ids.new_id("minecraft:block", "oak_planks"),
        ids.new_id("minecraft:block", "poplar_planks"),
    ];
    let planks_old = [ids.old_id("minecraft:block", "oak_planks")];
    let new = tags(
        cfg_new("update_tags"),
        &[
            ("minecraft:block", "minecraft:planks", &planks_new),
            ("minecraft:worldgen/biome", "minecraft:is_forest", &[1, 2]),
            ("minecraft:block_transformer", "minecraft:all", &[0]),
        ],
    );
    let want = tags(
        cfg_old("update_tags"),
        &[
            ("minecraft:block", "minecraft:planks", &planks_old),
            ("minecraft:worldgen/biome", "minecraft:is_forest", &[1]),
        ],
    );
    assert_eq!(cb(&mut c, &new), Some(want));

    // Post effects do not exist in 26.2.
    assert_eq!(cb(&mut c, &body(cfg_new("post_effects"), &[0])), None);
}

#[test]
fn login_spawn_info() {
    let ids = Ids::load();
    let (_, mut c) = halves(State::Play);
    let respawn = |id: i32, old: bool| {
        let mut b = varint(0);
        write_str(&mut b, "minecraft:overworld");
        b.extend_from_slice(&[0; 8]);
        b.push(1); // creative
        if old {
            b.push(0xff); // no previous game mode
        } else {
            b.push(0);
        }
        b.extend_from_slice(&[0, 0, 0, 0, 63, 3]);
        body(id, &b)
    };
    assert_eq!(
        cb(
            &mut c,
            &respawn(ids.new_packet("play", "clientbound", "respawn"), false)
        ),
        Some(respawn(
            ids.old_packet("play", "clientbound", "respawn"),
            true
        ))
    );
}

#[test]
fn commands_tree() {
    let ids = Ids::load();
    let (_, mut c) = halves(State::Play);
    let arg = |parser: i32, props: &[u8]| {
        let mut b = vec![2];
        b.push(0);
        write_str(&mut b, "x");
        b.extend(varint(parser));
        b.extend_from_slice(props);
        b
    };
    let tree = |id: i32, nodes: &[Vec<u8>]| {
        let mut b = varint(i32::try_from(nodes.len()).unwrap());
        for n in nodes {
            b.extend_from_slice(n);
        }
        b.extend(varint(0));
        body(id, &b)
    };
    let t = |n: &str, old: bool| {
        if old {
            ids.old_id("minecraft:command_argument_type", n)
        } else {
            ids.new_id("minecraft:command_argument_type", n)
        }
    };
    // uuid moved; feature is new and becomes a greedy string; integer bounds are kept.
    let new = tree(
        ids.new_packet("play", "clientbound", "commands"),
        &[
            arg(t("uuid", false), &[]),
            arg(t("feature", false), &[]),
            arg(t("brigadier:integer", false), &[1, 0, 0, 0, 5]),
        ],
    );
    let want = tree(
        ids.old_packet("play", "clientbound", "commands"),
        &[
            arg(t("uuid", true), &[]),
            arg(t("brigadier:string", true), &[2]),
            arg(t("brigadier:integer", true), &[1, 0, 0, 0, 5]),
        ],
    );
    assert_eq!(cb(&mut c, &new), Some(want));
}

#[test]
fn serverbound_play() {
    let ids = Ids::load();
    let (mut s, mut c) = halves(State::Play);
    let old = |n: &str| ids.old_packet("play", "serverbound", n);
    let new = |n: &str| ids.new_packet("play", "serverbound", n);

    // A teleport to an absolute position is confirmed with that position.
    let target = Vec3 {
        x: 10.5,
        y: 64.0,
        z: -3.5,
    };
    let teleport = encode_body(&PlayerPosition {
        teleport_id: 4,
        change: PositionMoveRotation {
            position: target,
            delta: Vec3::default(),
            y_rot: 45.0,
            x_rot: 10.0,
        },
        relatives: 0,
    });
    assert!(cb(&mut c, &teleport).is_some());
    let mut want = varint(new("accept_teleportation"));
    want.push(4);
    target.encode(&mut want);
    want.extend_from_slice(&45.0f32.to_be_bytes());
    want.extend_from_slice(&10.0f32.to_be_bytes());
    assert_eq!(
        sb(&mut s, &body(old("accept_teleportation"), &[4])),
        Some(want)
    );

    // Player actions after the inserted CHANGE_DESTROY_DIRECTION move up by one.
    let action = |id: i32, a: u8| body(id, &[a, 0, 0, 0, 0, 0, 0, 0, 0, 1, 7]);
    assert_eq!(
        sb(&mut s, &action(old("player_action"), 2)),
        Some(action(new("player_action"), 3))
    );
    assert_eq!(
        sb(&mut s, &action(old("player_action"), 0)),
        Some(action(new("player_action"), 0))
    );

    // A main-hand swing at nothing is a punch; one after using an item is not sent.
    assert_eq!(
        sb(&mut s, &body(old("swing"), &[0])),
        Some(varint(new("punch")))
    );
    assert!(
        sb(
            &mut s,
            &body(old("use_item"), &[0, 1, 0, 0, 0, 0, 0, 0, 0, 0])
        )
        .is_some()
    );
    assert_eq!(sb(&mut s, &body(old("swing"), &[0])), None);
    assert!(sb(&mut s, &body(old("client_tick_end"), &[])).is_some());
    assert_eq!(sb(&mut s, &body(old("swing"), &[1])), None);

    // Sign lines come before the side in 26.3.
    let mut sign_old = vec![0; 8];
    sign_old.push(1);
    let mut lines = Vec::new();
    for l in ["a", "b", "c", "d"] {
        write_str(&mut lines, l);
    }
    sign_old.extend_from_slice(&lines);
    let mut sign_new = vec![0; 8];
    sign_new.extend_from_slice(&lines);
    sign_new.push(1);
    assert_eq!(
        sb(&mut s, &body(old("sign_update"), &sign_old)),
        Some(body(new("sign_update"), &sign_new))
    );

    // Creative items: item and component ids in 26.3 numbering, 26.2-only components left out.
    let creative = |old: bool| {
        let mut b = vec![0, 36];
        b.extend(varint(1));
        let (item, damage, map_color) = if old {
            (
                ids.old_id("minecraft:item", "bookshelf"),
                ids.old_id("minecraft:data_component_type", "damage"),
                Some(ids.old_id("minecraft:data_component_type", "map_color")),
            )
        } else {
            (
                ids.new_id("minecraft:item", "bookshelf"),
                ids.new_id("minecraft:data_component_type", "damage"),
                None,
            )
        };
        b.extend(varint(item));
        b.extend(varint(if map_color.is_some() { 2 } else { 1 }));
        b.extend(varint(0));
        b.extend(varint(damage));
        b.extend([1, 3]);
        if let Some(m) = map_color {
            b.extend(varint(m));
            b.extend([4, 0, 0, 0, 1]);
        }
        let packet = if old {
            ids.old_packet("play", "serverbound", "set_creative_mode_slot")
        } else {
            ids.new_packet("play", "serverbound", "set_creative_mode_slot")
        };
        body(packet, &b)
    };
    assert_eq!(sb(&mut s, &creative(true)), Some(creative(false)));
}

#[test]
fn status_and_versions() {
    assert_eq!(Accepted::parse("none").unwrap(), Accepted::default());
    assert!(Accepted::parse("26.2").unwrap().version(OLD).is_some());
    assert!(Accepted::parse("1.8").is_err());
    let json = format!(
        r#"{{"version":{{"name":"26.3","protocol":{}}}}}"#,
        vanillite_protocol::PROTOCOL_VERSION
    );
    assert_eq!(
        vanillite_multiversion::status_json(&json, OLD),
        r#"{"version":{"name":"26.3","protocol":776}}"#
    );
}
