//! Milestone 1 smoke test: two in-memory clients join, see each other (spawn, movement,
//! sneaking), one leaves and rejoins at the position it left.

use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

use vanillite_data::entity::ids;
use vanillite_protocol::chat::LastSeenUpdate;
use vanillite_protocol::client::TestClient;
use vanillite_protocol::codec::write_varint;
use vanillite_protocol::commands::NodeKind;
use vanillite_protocol::item::ItemStack;
use vanillite_protocol::packets::handshake::Intent;
use vanillite_protocol::packets::play::clientbound::GameMode;
use vanillite_protocol::packets::play::serverbound::{
    AcceptTeleportation, Chat, ChatCommand, ChunkBatchReceived, ClientTickEnd, CommandSuggestion,
    DigAction, Hand, Input, MoveFlags, MovePlayerPos, PlayerActionPacket, PlayerInput,
    PlayerLoaded, SetCreativeModeSlot, UseItemOn,
};
use vanillite_protocol::sets::ClientboundPlay;
use vanillite_protocol::types::BlockPos as WirePos;
use vanillite_protocol::types::{GameProfile, Uuid};
use vanillite_server::{ServerBuilder, VanillaPack, VirtualConnection, VirtualOpener};

const TIMEOUT: Duration = Duration::from_secs(20);

/// A virtual connection as a byte stream with uncompressed length-prefixed frames, so the
/// protocol's test client can drive it.
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
        // Split complete frames: a VarInt length, then the body.
        loop {
            let mut len = 0usize;
            let mut shift = 0;
            let mut header = 0;
            let mut complete = false;
            for &b in self.output.iter().take(5) {
                len |= usize::from(b & 0x7F) << shift;
                shift += 7;
                header += 1;
                if b & 0x80 == 0 {
                    complete = true;
                    break;
                }
            }
            if !complete || self.output.len() < header + len {
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

struct Player {
    client: TestClient<Mem>,
    uuid: u128,
    /// Root literals of the command tree the player got on joining.
    commands: Vec<String>,
}

impl Player {
    fn recv(&mut self) -> ClientboundPlay {
        let raw = self.client.recv_raw().expect("receive");
        ClientboundPlay::decode(&raw).expect("decode")
    }

    /// Receives until `f` matches, answering what a client must answer on the way.
    fn until<T>(&mut self, what: &str, mut f: impl FnMut(&ClientboundPlay) -> Option<T>) -> T {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            let packet = self.recv();
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

    fn send<P: vanillite_protocol::Packet>(&mut self, packet: &P) {
        self.client.send(packet).expect("send");
    }
}

fn connect(opener: &VirtualOpener, name: &str) -> Player {
    let uuid = Uuid::offline(name);
    let conn = opener.open(
        GameProfile {
            id: uuid,
            name: name.to_owned(),
            properties: Vec::new(),
        },
        ([127, 0, 0, 1], 0).into(),
    );
    let mut client = TestClient::new(Mem {
        conn,
        input: Vec::new(),
        pos: 0,
        output: Vec::new(),
    });
    client.handshake("localhost", 25565, Intent::Login).unwrap();
    client.login(name, None).unwrap();
    Player {
        client,
        uuid: uuid.0,
        commands: Vec::new(),
    }
}

/// Configuration and the join sequence; returns the teleport position.
fn join(player: &mut Player) -> (f64, f64, f64) {
    player.client.configure().unwrap();
    player.until("login", |p| {
        matches!(p, ClientboundPlay::Login(_)).then_some(())
    });
    let mut roots = Vec::new();
    let teleport = player.until("player_position", |p| match p {
        ClientboundPlay::Commands(tree) => {
            let nodes = &tree.0.nodes;
            let root = usize::try_from(tree.0.root).ok().and_then(|i| nodes.get(i));
            roots = root
                .map(|r| {
                    r.children
                        .iter()
                        .filter_map(|&c| match nodes.get(usize::try_from(c).ok()?)?.kind {
                            NodeKind::Literal(ref name) => Some(name.clone()),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default();
            None
        }
        ClientboundPlay::PlayerPosition(t) => Some(t.clone()),
        _ => None,
    });
    player.commands = roots;
    let pos = teleport.change.position;
    player.send(&AcceptTeleportation {
        id: teleport.teleport_id,
        x: pos.x,
        y: pos.y,
        z: pos.z,
        y_rot: teleport.change.y_rot,
        x_rot: teleport.change.x_rot,
    });
    player.until("chunks", |p| {
        matches!(p, ClientboundPlay::LevelChunkWithLight(_)).then_some(())
    });
    player.send(&PlayerLoaded);
    (pos.x, pos.y, pos.z)
}

/// `Some(())` for a system message with the translation key `key`.
fn system_key(packet: &ClientboundPlay, key: &str) -> Option<()> {
    let ClientboundPlay::SystemChat(chat) = packet else {
        return None;
    };
    let mut stack = vec![&chat.content];
    while let Some(c) = stack.pop() {
        if let vanillite_protocol::text::Content::Translatable { key: k, .. } = &c.content
            && k == key
        {
            return Some(());
        }
        stack.extend(c.extra.iter());
    }
    None
}

/// Waits until `viewer` sees an entity with `uuid` spawn and returns its network id.
fn sees_spawn(viewer: &mut Player, uuid: u128) -> i32 {
    viewer.until("add_entity", |p| match p {
        ClientboundPlay::AddEntity(a) if a.uuid.0 == uuid => Some(a.entity_id),
        _ => None,
    })
}

#[test]
fn two_players_join_see_each_other_and_rejoin() {
    let dir = std::env::temp_dir().join(format!("vanillite-join-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("server.properties"), "view-distance=2\n").unwrap();
    // Alice is an operator.
    std::fs::write(
        dir.join("ops.json"),
        format!(
            r#"[{{"uuid":"{}","name":"Alice","level":4,"bypassesPlayerLimit":false}}]"#,
            Uuid::offline("Alice")
        ),
    )
    .unwrap();
    std::fs::write(dir.join("eula.txt"), "eula=true\n").unwrap();
    let start = |dir: &std::path::Path| {
        let mut builder = ServerBuilder::new(dir);
        builder.vanilla_pack(VanillaPack::None).network(false);
        builder.start().expect("server starts")
    };

    let server = start(&dir);
    let opener = server.virtual_opener().expect("network");
    let handle = server.handle();
    let thread = std::thread::spawn(move || server.run());

    let mut a = connect(&opener, "Alice");
    let (x, y, z) = join(&mut a);
    // The F3 debug ping is echoed with the same payload.
    let time = 0x0123_4567_89ab_cdef;
    a.send(&vanillite_protocol::packets::play::serverbound::PingRequest { time });
    a.until("pong_response", |p| match p {
        ClientboundPlay::PongResponse(r) => {
            assert_eq!(r.time, time);
            Some(())
        }
        _ => None,
    });
    let mut b = connect(&opener, "Bob");
    join(&mut b);

    // Both see each other with a skin entry in the tab list.
    let alice_id = sees_spawn(&mut b, a.uuid);
    a.until("Bob in the tab list", |p| match p {
        ClientboundPlay::PlayerInfoUpdate(u) => u
            .entries
            .iter()
            .any(|e| e.id.0 == b.uuid && e.profile.is_some())
            .then_some(()),
        _ => None,
    });
    sees_spawn(&mut a, b.uuid);

    // Alice walks; Bob sees the movement.
    let target = (x + 1.5, y, z - 0.5);
    a.send(&MovePlayerPos {
        x: target.0,
        y: target.1,
        z: target.2,
        flags: MoveFlags {
            on_ground: true,
            horizontal_collision: false,
        },
    });
    a.send(&ClientTickEnd);
    b.until("Alice moving", |p| match p {
        ClientboundPlay::MoveEntityPos(m) if m.entity_id == alice_id => Some(()),
        ClientboundPlay::MoveEntityPosRot(m) if m.entity_id == alice_id => Some(()),
        ClientboundPlay::EntityPositionSync(m) if m.entity_id == alice_id => Some(()),
        _ => None,
    });

    // Alice sneaks; Bob gets her entity data (crouching flag 0x02, pose 5).
    a.send(&PlayerInput(Input {
        shift: true,
        ..Input::default()
    }));
    b.until("Alice sneaking", |p| match p {
        ClientboundPlay::SetEntityData(d) if d.entity_id == alice_id => {
            (d.items.0.windows(3).any(|w| w == [0, 0, 0x02])).then_some(())
        }
        _ => None,
    });

    // Alice chats; both get her (unsigned) message as player chat.
    a.send(&Chat {
        message: "hello world".to_owned(),
        timestamp: 0,
        salt: 0,
        signature: None,
        last_seen: LastSeenUpdate::default(),
    });
    let alice = a.uuid;
    for viewer in [&mut b, &mut a] {
        viewer.until("Alice's chat message", |p| match p {
            ClientboundPlay::PlayerChat(c) if c.sender.0 == alice => {
                (c.content == "hello world" && c.signature.is_none()).then_some(())
            }
            _ => None,
        });
    }

    // The command tree follows the permission level.
    assert!(a.commands.iter().any(|c| c == "gamemode"));
    assert!(a.commands.iter().any(|c| c == "list"));
    assert!(!b.commands.iter().any(|c| c == "gamemode"));
    assert!(b.commands.iter().any(|c| c == "msg"));

    // Alice (operator) changes her game mode: the game event to her, the tab list to Bob.
    a.send(&ChatCommand {
        command: "gamemode creative".to_owned(),
    });
    a.until("creative game event", |p| match p {
        ClientboundPlay::GameEvent(e) if e.event == 3 && e.param == 1.0 => Some(()),
        _ => None,
    });
    a.until("game mode feedback", |p| {
        system_key(p, "commands.gamemode.success.self")
    });
    b.until("Alice creative in the tab list", |p| match p {
        ClientboundPlay::PlayerInfoUpdate(u) => u
            .entries
            .iter()
            .any(|e| e.id.0 == alice && e.game_mode == GameMode::Creative)
            .then_some(()),
        _ => None,
    });

    // Alice takes stone from the creative inventory and places it; Bob sees the block.
    a.send(&SetCreativeModeSlot {
        slot: 36,
        item: ItemStack::new(1, 64),
    });
    let below = WirePos {
        x: (target.0.floor() as i32) + 2,
        y: (target.1.floor() as i32) - 1,
        z: target.2.floor() as i32,
    };
    a.send(&UseItemOn {
        hand: Hand::MainHand,
        pos: below,
        face: 1,
        cursor_x: 0.5,
        cursor_y: 1.0,
        cursor_z: 0.5,
        inside: false,
        world_border_hit: false,
        sequence: 1,
    });
    a.until("place acknowledged", |p| match p {
        ClientboundPlay::BlockChangedAck(ack) if ack.sequence == 1 => Some(()),
        _ => None,
    });
    let placed = b.until("Alice's stone", |p| match p {
        ClientboundPlay::BlockUpdate(u) if u.state == 1 => Some(u.pos),
        _ => None,
    });
    assert!(
        placed == below
            || placed
                == WirePos {
                    y: below.y + 1,
                    ..below
                }
    );
    // She breaks it again (instantly in creative); Bob sees the particles and the air.
    a.send(&PlayerActionPacket {
        action: DigAction::StartDestroyBlock,
        pos: placed,
        direction: 1,
        sequence: 2,
    });
    a.until("break acknowledged", |p| match p {
        ClientboundPlay::BlockChangedAck(ack) if ack.sequence == 2 => Some(()),
        _ => None,
    });
    let local = i64::from(((placed.x & 15) << 8) | ((placed.z & 15) << 4) | (placed.y & 15));
    let (mut particles, mut air) = (false, false);
    b.until("break particles and the broken stone", |p| {
        match p {
            ClientboundPlay::LevelEvent(e) if e.event == 2001 && e.pos == placed => {
                particles = e.data == 1;
            }
            ClientboundPlay::BlockUpdate(u) if u.pos == placed => air = u.state == 0,
            ClientboundPlay::SectionBlocksUpdate(u) => {
                air |= u.changes.contains(&local);
            }
            _ => {}
        }
        (particles && air).then_some(())
    });
    // She drops one stone (Q); Bob sees the item entity.
    a.send(&PlayerActionPacket {
        action: DigAction::DropItem,
        pos: WirePos { x: 0, y: 0, z: 0 },
        direction: 0,
        sequence: 0,
    });
    b.until("dropped item", |p| match p {
        ClientboundPlay::AddEntity(e) if e.entity_type == i32::from(ids::ITEM.0) => Some(()),
        _ => None,
    });

    // Bob may not: the command is unknown to him (permission before parse, SEC-11).
    b.send(&ChatCommand {
        command: "gamemode creative".to_owned(),
    });
    b.until("unknown command error", |p| {
        system_key(p, "command.unknown.command")
    });
    // Bob lists the players.
    b.send(&ChatCommand {
        command: "list".to_owned(),
    });
    b.until("player list", |p| system_key(p, "commands.list.players"));
    // Alice says something; unsigned commands come as system messages with the say type.
    a.send(&ChatCommand {
        command: "say hi there".to_owned(),
    });
    b.until("say message", |p| match p {
        ClientboundPlay::DisguisedChat(c) => (c.message.plain() == "hi there").then_some(()),
        _ => None,
    });
    // Bob completes `/msg `: both names.
    b.send(&CommandSuggestion {
        id: 7,
        command: "/msg ".to_owned(),
    });
    b.until("suggestions", |p| match p {
        ClientboundPlay::CommandSuggestions(s) if s.id == 7 => {
            let names: Vec<&str> = s.suggestions.iter().map(|s| s.text.as_str()).collect();
            assert!(
                names.contains(&"Alice") && names.contains(&"Bob"),
                "{names:?}"
            );
            assert_eq!(s.start, 5);
            Some(())
        }
        _ => None,
    });

    // Alice leaves; Bob sees her go.
    let alice_uuid = a.uuid;
    drop(a);
    b.until("Alice removed", |p| match p {
        ClientboundPlay::RemoveEntities(r) if r.ids.0.contains(&alice_id) => Some(()),
        _ => None,
    });
    b.until("Alice off the tab list", |p| match p {
        ClientboundPlay::PlayerInfoRemove(r) => {
            r.ids.iter().any(|u| u.0 == alice_uuid).then_some(())
        }
        _ => None,
    });

    // She rejoins where she left.
    let mut alice_again = connect(&opener, "Alice");
    let rejoined = join(&mut alice_again);
    assert_eq!(rejoined, target);
    sees_spawn(&mut b, alice_again.uuid);

    handle.stop();
    thread.join().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
