//! A client speaking the previous version's protocol (26.2) joins a server of the current
//! version through the multi-version layer: configuration, chunks, walking, and seeing a
//! current-version player (and being seen by it).
//!
//! The old client is written against the 26.2 wire format by hand (packet ids from
//! `data/multiversion/26.2/`), not with the translation code it tests.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::as_conversions
)]

use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

use serde_json::Value;
use vanillite_protocol::client::TestClient;
use vanillite_protocol::codec::{Reader, write_str, write_varint};
use vanillite_protocol::packets::handshake::Intent;
use vanillite_protocol::packets::play::serverbound::{
    AcceptTeleportation, ChunkBatchReceived, ClientTickEnd, MoveFlags, MovePlayerPos, PlayerLoaded,
};
use vanillite_protocol::sets::ClientboundPlay;
use vanillite_protocol::types::{GameProfile, Uuid};
use vanillite_server::{ServerBuilder, VanillaPack, VirtualConnection, VirtualOpener};

const TIMEOUT: Duration = Duration::from_secs(30);
const OLD_PROTOCOL: i32 = 776;

/// A virtual connection as a byte stream with uncompressed length-prefixed frames.
struct Mem {
    conn: VirtualConnection,
    input: Vec<u8>,
    pos: usize,
    output: Vec<u8>,
}

impl Read for Mem {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.input.len() {
            let deadline = Instant::now() + TIMEOUT;
            let body = loop {
                match self.conn.from_server.try_recv() {
                    Ok(body) => break body,
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => return Ok(0),
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                        if Instant::now() > deadline {
                            return Err(io::ErrorKind::TimedOut.into());
                        }
                        std::thread::sleep(Duration::from_millis(1));
                    }
                }
            };
            self.input.clear();
            self.pos = 0;
            write_varint(&mut self.input, i32::try_from(body.len()).unwrap());
            self.input.extend_from_slice(&body);
        }
        let n = buf.len().min(self.input.len() - self.pos);
        buf[..n].copy_from_slice(&self.input[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

impl Write for Mem {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.output.extend_from_slice(buf);
        loop {
            let mut r = Reader::new(&self.output);
            let Ok(len) = r.varint() else {
                return Ok(buf.len());
            };
            let header = self.output.len() - r.remaining();
            let len = usize::try_from(len).unwrap();
            if r.remaining() < len {
                return Ok(buf.len());
            }
            let body = self.output[header..header + len].to_vec();
            self.output.drain(..header + len);
            self.conn
                .to_server
                .blocking_send(body)
                .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?;
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn mem(opener: &VirtualOpener, name: &str) -> TestClient<Mem> {
    let conn = opener.open_remote(
        GameProfile {
            id: Uuid::offline(name),
            name: name.to_owned(),
            properties: Vec::new(),
        },
        ([127, 0, 0, 1], 0).into(),
    );
    TestClient::new(Mem {
        conn,
        input: Vec::new(),
        pos: 0,
        output: Vec::new(),
    })
}

/// The 26.2 facts the old client needs, from the kept data.
struct Old262 {
    packets: Value,
    registries: Value,
    synchronized: Value,
    block_states: usize,
}

impl Old262 {
    fn load() -> Self {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/multiversion/26.2/");
        let json = |f: &str| -> Value {
            serde_json::from_str(&std::fs::read_to_string(format!("{dir}{f}")).unwrap()).unwrap()
        };
        let blocks = json("blocks.json");
        let block_states = blocks
            .as_array()
            .unwrap()
            .iter()
            .map(|b| {
                let n: u64 = b["properties"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|p| p["values"].as_array().unwrap().len() as u64)
                    .product();
                b["first_state"].as_u64().unwrap() + n
            })
            .max()
            .unwrap();
        Self {
            packets: json("packets.json"),
            registries: json("registries.json"),
            synchronized: json("synchronized.json"),
            block_states: usize::try_from(block_states).unwrap(),
        }
    }

    fn id(&self, state: &str, dir: &str, name: &str) -> i32 {
        let list = self.packets[state][dir].as_array().unwrap();
        let name = format!("minecraft:{name}");
        i32::try_from(
            list.iter()
                .position(|p| p == &Value::from(name.as_str()))
                .unwrap(),
        )
        .unwrap()
    }

    fn registry_id(&self, registry: &str, name: &str) -> i32 {
        let list = self.registries[registry]["entries"].as_array().unwrap();
        i32::try_from(list.iter().position(|p| p == name).unwrap()).unwrap()
    }
}

/// A client of protocol 776 driven packet by packet.
struct OldClient {
    client: TestClient<Mem>,
    v: Old262,
}

impl OldClient {
    fn send(&mut self, state: &str, name: &str, fields: &[u8]) {
        let mut body = Vec::new();
        write_varint(&mut body, self.v.id(state, "serverbound", name));
        body.extend_from_slice(fields);
        let mut frame = Vec::new();
        write_varint(&mut frame, i32::try_from(body.len()).unwrap());
        frame.extend_from_slice(&body);
        self.client.send_raw(&frame).unwrap();
    }

    /// The next packet: its 26.2 name and fields.
    fn recv(&mut self, state: &str) -> (String, Vec<u8>) {
        let raw = self.client.recv_raw().expect("receive");
        let mut r = Reader::new(&raw);
        let id = usize::try_from(r.varint().unwrap()).unwrap();
        let name = self.v.packets[state]["clientbound"][id]
            .as_str()
            .unwrap_or_else(|| panic!("{state} packet id {id} unknown to 26.2"))
            .trim_start_matches("minecraft:")
            .to_owned();
        (name, r.rest().to_vec())
    }

    /// Play packets until `f` matches, answering keepalives and chunk batches like a client.
    fn until<T>(&mut self, what: &str, mut f: impl FnMut(&str, &[u8]) -> Option<T>) -> T {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            assert!(
                Instant::now() < deadline,
                "old client timed out waiting for {what}"
            );
            let (name, data) = self.recv("play");
            match name.as_str() {
                "keep_alive" => self.send("play", "keep_alive", &data),
                "chunk_batch_finished" => {
                    self.send("play", "chunk_batch_received", &64.0f32.to_be_bytes());
                }
                "disconnect" => panic!("old client kicked"),
                "level_chunk_with_light" => check_chunk(&data, self.v.block_states),
                _ => {}
            }
            if let Some(v) = f(&name, &data) {
                return v;
            }
        }
    }
}

/// Every block state of every section is one the 26.2 client knows; direct palettes use its
/// global bit count.
fn check_chunk(data: &[u8], old_states: usize) {
    let mut r = Reader::new(data);
    r.take(8).unwrap();
    let heightmaps = r.varint().unwrap();
    for _ in 0..heightmaps {
        r.varint().unwrap();
        let n = usize::try_from(r.varint().unwrap()).unwrap();
        r.take(n * 8).unwrap();
    }
    let len = usize::try_from(r.varint().unwrap()).unwrap();
    let mut s = Reader::new(r.take(len).unwrap());
    let global = usize::BITS - (old_states - 1).leading_zeros();
    while s.remaining() > 0 {
        s.take(4).unwrap();
        for (entries, max_indirect) in [(4096usize, 8u8), (64, 3)] {
            let bits = s.u8().unwrap();
            if bits == 0 {
                let v = usize::try_from(s.varint().unwrap()).unwrap();
                if entries == 4096 {
                    assert!(v < old_states, "single block state {v} unknown to 26.2");
                }
                continue;
            }
            if bits <= max_indirect {
                let n = s.varint().unwrap();
                for _ in 0..n {
                    let v = usize::try_from(s.varint().unwrap()).unwrap();
                    if entries == 4096 {
                        assert!(v < old_states, "palette block state {v} unknown to 26.2");
                    }
                }
            } else if entries == 4096 {
                assert_eq!(u32::from(bits), global, "direct palette bits");
            }
            let per = 64 / usize::from(bits);
            s.take(entries.div_ceil(per) * 8).unwrap();
        }
    }
}

/// A current-version player.
struct Player {
    client: TestClient<Mem>,
}

impl Player {
    fn until<T>(&mut self, what: &str, mut f: impl FnMut(&ClientboundPlay) -> Option<T>) -> T {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            let raw = self.client.recv_raw().expect("receive");
            let packet = ClientboundPlay::decode(&raw).expect("decode");
            match &packet {
                ClientboundPlay::ChunkBatchFinished(_) => self
                    .client
                    .send(&ChunkBatchReceived {
                        desired_chunks_per_tick: 64.0,
                    })
                    .unwrap(),
                ClientboundPlay::KeepAlive(k) => self
                    .client
                    .send(&vanillite_protocol::packets::play::serverbound::KeepAlive { id: k.id })
                    .unwrap(),
                ClientboundPlay::Disconnect(d) => panic!("kicked: {}", d.reason.plain()),
                _ => {}
            }
            if let Some(v) = f(&packet) {
                return v;
            }
        }
    }
}

fn f64s(data: &[u8], at: usize) -> [f64; 3] {
    let mut r = Reader::new(&data[at..]);
    [r.f64().unwrap(), r.f64().unwrap(), r.f64().unwrap()]
}

#[test]
fn previous_version_client_joins_walks_and_sees_players() {
    let dir = std::env::temp_dir().join(format!("vanillite-multiversion-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("server.properties"), "view-distance=2\n").unwrap();
    std::fs::write(dir.join("eula.txt"), "eula=true\n").unwrap();
    let mut builder = ServerBuilder::new(&dir);
    builder.vanilla_pack(VanillaPack::None).network(false);
    let server = builder.start().expect("server starts");
    let opener = server.virtual_opener().expect("network");
    let handle = server.handle();
    let thread = std::thread::spawn(move || server.run());

    // --- The 26.2 client logs in and configures.
    let mut old = OldClient {
        client: mem(&opener, "Oldie"),
        v: Old262::load(),
    };
    let mut hs = Vec::new();
    write_varint(&mut hs, OLD_PROTOCOL);
    write_str(&mut hs, "localhost");
    hs.extend_from_slice(&25565u16.to_be_bytes());
    write_varint(&mut hs, 2);
    old.send("handshake", "intention", &hs);
    let mut hello = Vec::new();
    write_str(&mut hello, "Oldie");
    hello.extend_from_slice(&Uuid::offline("Oldie").0.to_be_bytes());
    old.send("login", "hello", &hello);
    loop {
        let (name, _) = old.recv("login");
        if name == "login_finished" {
            break;
        }
        assert_ne!(name, "login_disconnect", "old client refused");
    }
    old.send("login", "login_acknowledged", &[]);

    let mut registries = Vec::new();
    loop {
        let (name, data) = old.recv("configuration");
        let mut r = Reader::new(&data);
        match name.as_str() {
            "select_known_packs" => {
                let n = r.varint().unwrap();
                let mut core = None;
                for _ in 0..n {
                    let (ns, id, version) = (
                        r.string(256).unwrap(),
                        r.string(256).unwrap(),
                        r.string(256).unwrap(),
                    );
                    if ns == "minecraft" && id == "core" {
                        core = Some(version.to_owned());
                    }
                }
                // A test server without data packs offers no pack.
                if n > 0 {
                    assert_eq!(
                        core.as_deref(),
                        Some("26.2"),
                        "core pack of the client's version"
                    );
                }
                let mut answer = Vec::new();
                write_varint(&mut answer, 1);
                for s in ["minecraft", "core", "26.2"] {
                    write_str(&mut answer, s);
                }
                old.send("configuration", "select_known_packs", &answer);
            }
            "registry_data" => registries.push(r.string(256).unwrap().to_owned()),
            "keep_alive" => old.send("configuration", "keep_alive", &data),
            "finish_configuration" => {
                old.send("configuration", "finish_configuration", &[]);
                break;
            }
            "disconnect" => panic!("old client kicked in configuration"),
            _ => {}
        }
    }
    // (The test server has no data packs, so this list may be empty; the golden tests cover
    // the registry filter.)
    let synchronized = old.v.synchronized.as_object().unwrap();
    for r in &registries {
        assert!(
            synchronized.contains_key(r),
            "registry {r} is not synchronised by 26.2"
        );
    }

    // --- Play: join, confirm the teleport the 26.2 way (only the id), receive chunks.
    old.until("login", |n, _| (n == "login").then_some(()));
    let (teleport, pos) = old.until("player_position", |n, d| {
        (n == "player_position").then(|| {
            let mut r = Reader::new(d);
            let id = r.varint().unwrap();
            (id, f64s(d, d.len() - r.remaining()))
        })
    });
    let mut confirm = Vec::new();
    write_varint(&mut confirm, teleport);
    old.send("play", "accept_teleportation", &confirm);
    old.until("chunk", |n, _| {
        (n == "level_chunk_with_light").then_some(())
    });
    old.send("play", "player_loaded", &[]);

    // --- A current-version player joins; both see each other.
    let mut new = Player {
        client: mem(&opener, "Newbie"),
    };
    new.client
        .handshake("localhost", 25565, Intent::Login)
        .unwrap();
    new.client.login("Newbie", None).unwrap();
    new.client.configure().unwrap();
    let new_pos = new.until("player_position", |p| match p {
        ClientboundPlay::PlayerPosition(t) => Some(t.clone()),
        _ => None,
    });
    new.client
        .send(&AcceptTeleportation {
            id: new_pos.teleport_id,
            x: new_pos.change.position.x,
            y: new_pos.change.position.y,
            z: new_pos.change.position.z,
            y_rot: new_pos.change.y_rot,
            x_rot: new_pos.change.x_rot,
        })
        .unwrap();
    new.client.send(&PlayerLoaded).unwrap();

    let player_type = old
        .v
        .registry_id("minecraft:entity_type", "minecraft:player");
    let newbie = Uuid::offline("Newbie").0;
    let newbie_id = old.until("add_entity of the new player", |n, d| {
        if n != "add_entity" {
            return None;
        }
        let mut r = Reader::new(d);
        let id = r.varint().unwrap();
        let uuid = r.u128().unwrap();
        let kind = r.varint().unwrap();
        (uuid == newbie).then(|| {
            assert_eq!(kind, player_type, "entity type in 26.2 numbering");
            id
        })
    });
    let oldie = Uuid::offline("Oldie").0;
    let oldie_id = new.until("add_entity of the old player", |p| match p {
        ClientboundPlay::AddEntity(a) if a.uuid.0 == oldie => Some(a.entity_id),
        _ => None,
    });

    // --- The new player walks; the old client sees it in the 26.2 movement format.
    let p = new_pos.change.position;
    new.client
        .send(&MovePlayerPos {
            x: p.x + 1.5,
            y: p.y,
            z: p.z - 0.5,
            flags: MoveFlags {
                on_ground: true,
                horizontal_collision: false,
            },
        })
        .unwrap();
    new.client.send(&ClientTickEnd).unwrap();
    old.until("the new player moving", |n, d| {
        let mut r = Reader::new(d);
        match n {
            // 26.2: id, three shorts, (yaw, pitch,) on ground.
            "move_entity_pos" | "move_entity_pos_rot" => {
                if r.varint().unwrap() != newbie_id {
                    return None;
                }
                let dx = r.i16().unwrap();
                r.take(4).unwrap();
                if n == "move_entity_pos_rot" {
                    r.take(2).unwrap();
                }
                r.u8().unwrap();
                assert_eq!(r.remaining(), 0, "26.2 {n} layout");
                assert_eq!(dx, 6144, "x delta in 1/4096 blocks");
                Some(())
            }
            // 26.2: id, position, movement, yaw, pitch, on ground.
            "entity_position_sync" => {
                if r.varint().unwrap() != newbie_id {
                    return None;
                }
                r.take(24 + 24 + 8 + 1).unwrap();
                assert_eq!(r.remaining(), 0, "26.2 entity_position_sync layout");
                Some(())
            }
            _ => None,
        }
    });

    // --- The old client walks; the new player sees it.
    let mut walk = Vec::new();
    for v in [pos[0] - 1.0, pos[1], pos[2] + 0.5] {
        walk.extend_from_slice(&v.to_be_bytes());
    }
    walk.push(1);
    old.send("play", "move_player_pos", &walk);
    old.send("play", "client_tick_end", &[]);
    new.until("the old player moving", |p| match p {
        ClientboundPlay::MoveEntityPos(m) if m.entity_id == oldie_id => Some(()),
        ClientboundPlay::MoveEntityPosRot(m) if m.entity_id == oldie_id => Some(()),
        ClientboundPlay::EntityPositionSync(m) if m.entity_id == oldie_id => Some(()),
        _ => None,
    });

    handle.stop();
    thread.join().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
