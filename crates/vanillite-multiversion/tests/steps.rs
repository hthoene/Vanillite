//! Golden translations of the steps below 26.2: for each step, packets whose format changed in
//! it, from what the server (26.3) sends down to a client of the step's older version. The
//! 26.3 side is written from `data/vanilla/`, the expected bytes by hand from the older wire
//! format with ids from `data/multiversion/<version>/`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::as_conversions
)]

use serde_json::Value;
use vanillite_multiversion::{Accepted, ClientboundHalf, ServerboundHalf, State};
use vanillite_protocol::codec::{write_str, write_varint};

fn json(path: &str) -> Value {
    let p = format!("{}/../../{path}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

/// Ids of an older version and of the server's version by name.
struct Ids {
    protocol: i32,
    old: Value,
    new: Value,
    old_packets: Value,
    new_packets: Value,
}

impl Ids {
    fn load(dir: &str) -> Self {
        let version = std::fs::read_to_string(format!(
            "{}/../../data/multiversion/{dir}/version.toml",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        Self {
            protocol: version
                .lines()
                .find_map(|l| l.strip_prefix("protocol = "))
                .unwrap()
                .parse()
                .unwrap(),
            old: json(&format!("data/multiversion/{dir}/registries.json")),
            new: json("data/vanilla/registries.json"),
            old_packets: json(&format!("data/multiversion/{dir}/packets.json")),
            new_packets: json("data/vanilla/packets.json"),
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

    /// A clientbound play packet of the server's version.
    fn new_cb(&self, name: &str, fields: &[u8]) -> Vec<u8> {
        body(
            Self::find(&self.new_packets["play"]["clientbound"], name),
            fields,
        )
    }

    /// A clientbound play packet of the older version.
    fn old_cb(&self, name: &str, fields: &[u8]) -> Vec<u8> {
        body(
            Self::find(&self.old_packets["play"]["clientbound"], name),
            fields,
        )
    }

    fn new_sb(&self, name: &str, fields: &[u8]) -> Vec<u8> {
        body(
            Self::find(&self.new_packets["play"]["serverbound"], name),
            fields,
        )
    }

    fn old_sb(&self, name: &str, fields: &[u8]) -> Vec<u8> {
        body(
            Self::find(&self.old_packets["play"]["serverbound"], name),
            fields,
        )
    }

    fn halves(&self) -> (ServerboundHalf, ClientboundHalf) {
        Accepted::all()
            .adapter(self.protocol, State::Play)
            .expect("supported")
            .split()
    }
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

fn f64s(v: [f64; 3]) -> Vec<u8> {
    v.iter().flat_map(|c| c.to_be_bytes()).collect()
}

/// The server's `login`: entity 5, one dimension, spawn info with sea level, online mode.
fn new_login() -> Vec<u8> {
    let mut b = 5i32.to_be_bytes().to_vec();
    b.push(0);
    b.push(1);
    write_str(&mut b, "minecraft:overworld");
    b.extend([20, 2, 2, 0, 1, 0]);
    // Spawn info (26.3): dimension type, name, seed, game mode, previous (none), debug, flat,
    // no death location, portal cooldown, sea level.
    b.push(0);
    write_str(&mut b, "minecraft:overworld");
    b.extend([0; 8]);
    b.extend([1, 0, 0, 0, 0, 0]);
    b.extend(varint(63));
    b.push(1); // online mode
    b.push(0); // secure chat
    b
}

/// The same `login` for a client of `protocol`.
fn old_login(protocol: i32) -> Vec<u8> {
    let mut b = 5i32.to_be_bytes().to_vec();
    b.push(0);
    b.push(1);
    write_str(&mut b, "minecraft:overworld");
    b.extend([20, 2, 2, 0, 1, 0]);
    b.push(0);
    write_str(&mut b, "minecraft:overworld");
    b.extend([0; 8]);
    // Game mode and previous game mode as bytes (-1: none) before 26.3.
    b.extend([1, 0xff, 0, 0, 0, 0]);
    if protocol >= 768 {
        b.extend(varint(63));
    }
    if protocol >= 776 {
        b.push(1);
    }
    b.push(0);
    b
}

#[test]
fn login_down_every_step() {
    for dir in [
        "26.1.2", "1.21.11", "1.21.10", "1.21.8", "1.21.6", "1.21.5", "1.21.4", "1.21.3", "1.21.1",
        "1.20.6",
    ] {
        let ids = Ids::load(dir);
        let (_, mut c) = ids.halves();
        assert_eq!(
            cb(&mut c, &ids.new_cb("login", &new_login())),
            Some(ids.old_cb("login", &old_login(ids.protocol))),
            "login for {dir}"
        );
    }
}

/// Step 26.1 ↔ 26.2: team parameters in the old order with a colour enum (`RESET` for none).
#[test]
fn step_26_1_team() {
    let ids = Ids::load("26.1.2");
    let (_, mut c) = ids.halves();
    let text = |s: &str| {
        let mut b = vec![8];
        b.extend((s.len() as u16).to_be_bytes());
        b.extend(s.as_bytes());
        b
    };
    let mut new = Vec::new();
    write_str(&mut new, "red");
    new.push(0);
    new.extend(text("Red"));
    new.extend(text("["));
    new.extend(text("]"));
    new.extend([1, 2]); // never, push other teams
    new.extend([1, 12]); // colour red
    new.push(3); // options
    new.push(0); // no players
    let mut old = Vec::new();
    write_str(&mut old, "red");
    old.push(0);
    old.extend(text("Red"));
    old.push(3);
    old.extend([1, 2, 12]);
    old.extend(text("["));
    old.extend(text("]"));
    old.push(0);
    assert_eq!(
        cb(&mut c, &ids.new_cb("set_player_team", &new)),
        Some(ids.old_cb("set_player_team", &old))
    );
}

/// Step 1.21.11 ↔ 26.1: interactions split into `attack` and `interact` with a position.
#[test]
fn step_1_21_11_interact() {
    let ids = Ids::load("1.21.11");
    let (mut s, _) = ids.halves();
    // Attack (action 1): entity 9.
    assert_eq!(
        sb(&mut s, &ids.old_sb("interact", &[9, 1, 0])),
        Some(ids.new_sb("attack", &[9]))
    );
    // Interact at (2) a position, then the plain interact the old client sends after it.
    let mut at = vec![9, 2];
    for v in [0.5f32, 1.0, 0.0] {
        at.extend(v.to_be_bytes());
    }
    at.extend([0, 1]);
    let sent = sb(&mut s, &ids.old_sb("interact", &at)).unwrap();
    assert_eq!(&sent[..3], &ids.new_sb("interact", &[9, 0])[..]);
    assert_eq!(*sent.last().unwrap(), 1, "sneaking");
    assert_eq!(sb(&mut s, &ids.old_sb("interact", &[9, 0, 0, 1])), None);
    // A plain interact on its own.
    assert_eq!(
        sb(&mut s, &ids.old_sb("interact", &[9, 0, 1, 0])),
        Some(ids.new_sb("interact", &[9, 1, 0, 0]))
    );
}

/// Step 1.21.8 ↔ 1.21.9: velocities as three shorts at the end of `add_entity`.
#[test]
fn step_1_21_8_velocity() {
    let ids = Ids::load("1.21.8");
    let (_, mut c) = ids.halves();
    let mut new = varint(3);
    new.extend([0x11; 16]);
    new.extend(varint(ids.new_id("minecraft:entity_type", "minecraft:pig")));
    new.extend(f64s([1.0, 2.0, 3.0]));
    new.push(0); // no movement
    new.extend([4, 5, 6]);
    new.push(0);
    let mut old = varint(3);
    old.extend([0x11; 16]);
    old.extend(varint(ids.old_id("minecraft:entity_type", "minecraft:pig")));
    old.extend(f64s([1.0, 2.0, 3.0]));
    old.extend([4, 5, 6]);
    old.push(0);
    old.extend([0; 6]);
    assert_eq!(
        cb(&mut c, &ids.new_cb("add_entity", &new)),
        Some(ids.old_cb("add_entity", &old))
    );
    // Respawn data: dimension, position, yaw, pitch → position, angle.
    let mut spawn = Vec::new();
    write_str(&mut spawn, "minecraft:overworld");
    spawn.extend(7i64.to_be_bytes());
    spawn.extend(90.0f32.to_be_bytes());
    spawn.extend(0.0f32.to_be_bytes());
    let mut want = 7i64.to_be_bytes().to_vec();
    want.extend(90.0f32.to_be_bytes());
    assert_eq!(
        cb(&mut c, &ids.new_cb("set_default_spawn_position", &spawn)),
        Some(ids.old_cb("set_default_spawn_position", &want))
    );
}

/// Steps 1.21.5 ↔ 1.21.6 and down: shift comes as player commands, difficulty as a byte.
#[test]
fn step_1_21_5_commands() {
    let ids = Ids::load("1.21.5");
    let (mut s, mut c) = ids.halves();
    // Press shift (0) is gone from 1.21.6 on; stop sleeping (2) becomes 0.
    assert_eq!(sb(&mut s, &ids.old_sb("player_command", &[1, 0, 0])), None);
    assert_eq!(
        sb(&mut s, &ids.old_sb("player_command", &[1, 2, 0])),
        Some(ids.new_sb("player_command", &[1, 0, 0]))
    );
    assert_eq!(
        cb(&mut c, &ids.new_cb("change_difficulty", &[2, 1])),
        Some(ids.old_cb("change_difficulty", &[2, 1]))
    );
}

/// Step 1.21.4 ↔ 1.21.5: heightmaps as NBT, palette data with its length, player chat without
/// the global index, container clicks with full stacks.
#[test]
fn step_1_21_4_chunk_and_click() {
    let ids = Ids::load("1.21.4");
    let (mut s, mut c) = ids.halves();
    let chunk = |old: bool| {
        let mut b = Vec::new();
        b.extend(1i32.to_be_bytes());
        b.extend(2i32.to_be_bytes());
        if old {
            // Compound { MOTION_BLOCKING: long[1] }.
            b.push(10);
            b.push(12);
            b.extend((15u16).to_be_bytes());
            b.extend(b"MOTION_BLOCKING");
            b.extend(1i32.to_be_bytes());
            b.extend(9i64.to_be_bytes());
            b.push(0);
        } else {
            b.extend([1, 4, 1]);
            b.extend(9i64.to_be_bytes());
        }
        // One section of air with plains-like biome 0; 26.3 also counts fluids.
        let mut sec = vec![0, 0];
        if !old {
            sec.extend([0, 0]);
        }
        sec.extend([0, 0]);
        if old {
            sec.push(0);
        }
        sec.extend([0, 0]);
        if old {
            sec.push(0);
        }
        b.extend(varint(sec.len() as i32));
        b.extend(sec);
        b.push(0);
        // Light: four empty masks (long arrays before 26.3), no arrays.
        b.extend([0, 0, 0, 0, 0, 0]);
        b
    };
    assert_eq!(
        cb(&mut c, &ids.new_cb("level_chunk_with_light", &chunk(false))),
        Some(ids.old_cb("level_chunk_with_light", &chunk(true)))
    );

    // Container click: one changed slot with a full stack → a hashed stack without
    // component hashes; the carried item empty.
    let stone_old = ids.old_id("minecraft:item", "minecraft:stone");
    let stone_new = ids.new_id("minecraft:item", "minecraft:stone");
    let mut click = vec![1, 0, 0, 5, 0, 0, 1];
    click.extend(5i16.to_be_bytes());
    click.extend(varint(3));
    click.extend(varint(stone_old));
    click.extend([0, 0, 0]);
    let mut want = vec![1, 0, 0, 5, 0, 0, 1];
    want.extend(5i16.to_be_bytes());
    want.push(1);
    want.extend(varint(stone_new));
    want.extend(varint(3));
    want.extend([0, 0, 0]);
    let got = sb(&mut s, &ids.old_sb("container_click", &click));
    assert_eq!(got, Some(ids.new_sb("container_click", &want)));
}

/// Step 1.21.3 ↔ 1.21.4: the held slot as a byte; `player_loaded` follows the teleport
/// confirmation.
#[test]
fn step_1_21_3_held_slot_and_loaded() {
    let ids = Ids::load("1.21.3");
    let (mut s, mut c) = ids.halves();
    assert_eq!(
        cb(&mut c, &ids.new_cb("set_held_slot", &[4])),
        Some(ids.old_cb("set_held_slot", &[4]))
    );
    assert_eq!(
        sb(&mut s, &ids.old_sb("accept_teleportation", &[7])),
        Some(ids.new_sb("accept_teleportation", &{
            let mut b = vec![7];
            b.extend([0; 24 + 8]);
            b
        }))
    );
    assert_eq!(
        s.pending().map(<[u8]>::to_vec),
        Some(ids.new_sb("player_loaded", &[]))
    );
}

/// Step 1.21.1 ↔ 1.21.2: player positions in the old layout, container ids as bytes, the
/// cursor item as a slot of container -1, list order left out of the tab list, the end of
/// each tick after movements.
#[test]
fn step_1_21_1_positions_and_containers() {
    let ids = Ids::load("1.21.1");
    let (mut s, mut c) = ids.halves();
    let mut pos = varint(3);
    pos.extend(f64s([1.0, 64.0, -2.0]));
    pos.extend([0; 24]);
    pos.extend(90.0f32.to_be_bytes());
    pos.extend(5.0f32.to_be_bytes());
    pos.extend(0x18i32.to_be_bytes());
    let mut want = f64s([1.0, 64.0, -2.0]);
    want.extend(90.0f32.to_be_bytes());
    want.extend(5.0f32.to_be_bytes());
    want.push(0x18);
    want.push(3);
    assert_eq!(
        cb(&mut c, &ids.new_cb("player_position", &pos)),
        Some(ids.old_cb("player_position", &want))
    );

    // Cursor: container -1, state 0, slot -1, empty.
    let mut want = vec![0xff, 0];
    want.extend((-1i16).to_be_bytes());
    want.push(0);
    assert_eq!(
        cb(&mut c, &ids.new_cb("set_cursor_item", &[0])),
        Some(ids.old_cb("container_set_slot", &want))
    );

    // Tab list: add player + list order (bit 6) → add player only.
    let mut info = vec![0x41, 1];
    info.extend([0xaa; 16]);
    write_str(&mut info, "Bob");
    info.push(0);
    info.push(9);
    let mut want = vec![0x01, 1];
    want.extend([0xaa; 16]);
    write_str(&mut want, "Bob");
    want.push(0);
    assert_eq!(
        cb(&mut c, &ids.new_cb("player_info_update", &info)),
        Some(ids.old_cb("player_info_update", &want))
    );

    // Movement, then the end of the client tick the server expects.
    let mut walk = f64s([1.0, 64.0, -2.0]);
    walk.push(1);
    assert_eq!(
        sb(&mut s, &ids.old_sb("move_player_pos", &walk)),
        Some(ids.new_sb("move_player_pos", &walk))
    );
    assert_eq!(
        s.pending().map(<[u8]>::to_vec),
        Some(ids.new_sb("client_tick_end", &[]))
    );
    assert_eq!(s.pending(), None);

    // Pressing shift: the command is gone from 1.21.6 on, the player input carries it.
    assert_eq!(
        sb(&mut s, &ids.old_sb("player_command", &[1, 0, 0])),
        Some(ids.new_sb("player_input", &[32]))
    );
}

/// Step 1.20.5 ↔ 1.21: attribute modifiers by UUID (sprinting keeps the client's own), items
/// used with the player's rotation.
#[test]
fn step_1_20_5_attributes_and_use_item() {
    let ids = Ids::load("1.20.6");
    let (mut s, mut c) = ids.halves();
    let speed_new = ids.new_id("minecraft:attribute", "minecraft:movement_speed");
    let speed_old = ids.old_id("minecraft:attribute", "minecraft:generic.movement_speed");
    let mut attrs = varint(5);
    attrs.push(1);
    attrs.extend(varint(speed_new));
    attrs.extend(0.1f64.to_be_bytes());
    attrs.push(1);
    write_str(&mut attrs, "minecraft:sprinting");
    attrs.extend(0.3f64.to_be_bytes());
    attrs.push(2);
    let mut want = varint(5);
    want.push(1);
    want.extend(varint(speed_old));
    want.extend(0.1f64.to_be_bytes());
    want.push(1);
    want.extend(0x662a_6b8d_da3e_4c1c_8813_96ea_6097_278d_u128.to_be_bytes());
    want.extend(0.3f64.to_be_bytes());
    want.push(2);
    assert_eq!(
        cb(&mut c, &ids.new_cb("update_attributes", &attrs)),
        Some(ids.old_cb("update_attributes", &want))
    );

    // The rotation from the last movement goes with `use_item`.
    let mut rot = 45.0f32.to_be_bytes().to_vec();
    rot.extend(10.0f32.to_be_bytes());
    rot.push(1);
    sb(&mut s, &ids.old_sb("move_player_rot", &rot)).unwrap();
    let mut want = vec![0, 3];
    want.extend(45.0f32.to_be_bytes());
    want.extend(10.0f32.to_be_bytes());
    assert_eq!(
        sb(&mut s, &ids.old_sb("use_item", &[0, 3])),
        Some(ids.new_sb("use_item", &want))
    );
}

/// Entity data of players follows the layouts: main hand and skin parts moved with 1.21.9,
/// the main hand became an enum with 1.21.11.
#[test]
fn player_entity_data() {
    for (dir, hand, skin) in [("26.1.2", 15, 16), ("1.21.10", 15, 16), ("1.20.6", 18, 17)] {
        let ids = Ids::load(dir);
        let (_, mut c) = ids.halves();
        // The player's own entity, known from `login`.
        cb(&mut c, &ids.new_cb("login", &new_login())).unwrap();
        // 26.3: main hand (15, humanoid arm 42) right, skin parts (16, byte 0) 0x7f.
        let data = ids.new_cb("set_entity_data", &[5, 15, 42, 1, 16, 0, 0x7f, 0xff]);
        let got = cb(&mut c, &data).unwrap();
        let hand_bytes = if dir == "26.1.2" {
            // Still a humanoid arm: its serializer id in 26.1.
            let serializers = json(&format!("data/multiversion/{dir}/entity_data.json"));
            let id = serializers["serializers"]
                .as_array()
                .unwrap()
                .iter()
                .position(|s| s == "humanoid_arm")
                .unwrap();
            vec![hand, id as u8, 1]
        } else {
            vec![hand, 0, 1]
        };
        let mut want = hand_bytes;
        want.extend([skin, 0, 0x7f, 0xff]);
        let want = ids.old_cb("set_entity_data", &[&[5][..], &want[..]].concat());
        assert_eq!(got, want, "player entity data for {dir}");
    }
}
