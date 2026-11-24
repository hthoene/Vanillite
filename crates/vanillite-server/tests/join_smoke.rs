//! Milestone 1 smoke test: two in-memory clients join, see each other (spawn, movement,
//! sneaking), one leaves and rejoins at the position it left.

use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

use vanillite_protocol::client::TestClient;
use vanillite_protocol::codec::write_varint;
use vanillite_protocol::packets::handshake::Intent;
use vanillite_protocol::packets::play::serverbound::{
    AcceptTeleportation, ChunkBatchReceived, ClientTickEnd, Input, MoveFlags, MovePlayerPos,
    PlayerInput, PlayerLoaded,
};
use vanillite_protocol::sets::ClientboundPlay;
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
    }
}

/// Configuration and the join sequence; returns the teleport position.
fn join(player: &mut Player) -> (f64, f64, f64) {
    player.client.configure().unwrap();
    player.until("login", |p| {
        matches!(p, ClientboundPlay::Login(_)).then_some(())
    });
    let teleport = player.until("player_position", |p| match p {
        ClientboundPlay::PlayerPosition(t) => Some(t.clone()),
        _ => None,
    });
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
