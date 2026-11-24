//! Development and test only: in-process fake players (`debug.test-bots` in `vanillite.toml`).
//!
//! A bot joins over a virtual connection like any other client (handshake, login,
//! configuration, play), so it goes through the same validation as real players. Virtual
//! connections are opened by the server itself and need no authentication; no network client
//! can use this path. A bot walks in a circle around its spawn point and sneaks now and then.

use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

use vanillite_net::{VirtualConnection, VirtualOpener};
use vanillite_protocol::Packet;
use vanillite_protocol::client::TestClient;
use vanillite_protocol::codec::write_varint;
use vanillite_protocol::frame::encode_body;
use vanillite_protocol::packets::handshake::Intent;
use vanillite_protocol::packets::play::serverbound::{
    AcceptTeleportation, ChunkBatchReceived, ClientTickEnd, Input, KeepAlive, MoveFlags,
    MovePlayerPosRot, PlayerInput, PlayerLoaded,
};
use vanillite_protocol::sets::ClientboundPlay;
use vanillite_protocol::types::{GameProfile, Uuid};

/// Radius of the first bot's circle in blocks.
const RADIUS: f64 = 3.0;
/// Walking speed in blocks per tick (Vanilla's walking speed is about 0.216).
const SPEED: f64 = 0.2;
/// Ticks of one sneak cycle and how many of them the bot sneaks.
const SNEAK_CYCLE: u64 = 200;
const SNEAK_TICKS: u64 = 40;

/// Starts `count` bots named `TestBot`, `TestBot2`, ... on their own threads. A bot ends when
/// its connection closes.
pub(crate) fn spawn(opener: VirtualOpener, count: u8) {
    for n in 1..=count {
        let name = if n == 1 {
            "TestBot".to_owned()
        } else {
            format!("TestBot{n}")
        };
        let opener = opener.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("Test bot {name}"))
            .spawn(move || match run(&opener, &name, f64::from(n)) {
                Ok(()) => log::info!("Test bot {name} stopped"),
                Err(e) => log::warn!("Test bot {name} stopped: {e}"),
            });
        if let Err(e) = spawned {
            log::warn!("Cannot start a test bot: {e}");
        }
    }
}

/// A virtual connection as a blocking byte stream of uncompressed frames, for the login and
/// configuration phases driven by the protocol's test client.
struct Stream {
    conn: VirtualConnection,
    input: Vec<u8>,
    pos: usize,
    output: Vec<u8>,
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.input.len() {
            let Some(body) = self.conn.from_server.blocking_recv() else {
                return Ok(0);
            };
            self.input.clear();
            self.pos = 0;
            write_varint(
                &mut self.input,
                i32::try_from(body.len()).map_err(|_| io::ErrorKind::InvalidData)?,
            );
            self.input.extend_from_slice(&body);
        }
        let rest = self.input.get(self.pos..).unwrap_or_default();
        let n = buf.len().min(rest.len());
        buf.get_mut(..n)
            .unwrap_or_default()
            .copy_from_slice(rest.get(..n).unwrap_or_default());
        self.pos = self.pos.saturating_add(n);
        Ok(n)
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.output.extend_from_slice(buf);
        // Split complete frames: a VarInt length, then the body.
        loop {
            let mut reader = vanillite_protocol::codec::Reader::new(&self.output);
            let Ok(len) = reader.varint() else {
                return Ok(buf.len());
            };
            let header = self.output.len().saturating_sub(reader.remaining());
            let len = usize::try_from(len).map_err(|_| io::ErrorKind::InvalidData)?;
            let end = header.saturating_add(len);
            let Some(body) = self.output.get(header..end).map(<[u8]>::to_vec) else {
                return Ok(buf.len());
            };
            self.output.drain(..end);
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

/// The play phase over the raw packet channels.
struct Play {
    conn: VirtualConnection,
}

impl Play {
    fn send<P: Packet>(&self, packet: &P) -> Result<(), String> {
        self.conn
            .to_server
            .try_send(encode_body(packet))
            .map_err(|e| e.to_string())
    }
}

fn run(opener: &VirtualOpener, name: &str, n: f64) -> Result<(), String> {
    let uuid = Uuid::offline(name);
    let conn = opener.open(
        GameProfile {
            id: uuid,
            name: name.to_owned(),
            properties: Vec::new(),
        },
        ([127, 0, 0, 1], 0).into(),
    );
    let mut client = TestClient::new(Stream {
        conn,
        input: Vec::new(),
        pos: 0,
        output: Vec::new(),
    });
    client
        .handshake("localhost", 25565, Intent::Login)
        .and_then(|()| client.login(name, None))
        .and_then(|_| client.configure())
        .map_err(|e| e.to_string())?;
    let Stream { conn, .. } = client.into_stream();
    let mut play = Play { conn };
    walk(&mut play, n)
}

/// Answers the server and walks in a circle at 20 moves per second; bot `n` walks a circle
/// `n` blocks wide around its spawn point.
fn walk(play: &mut Play, n: f64) -> Result<(), String> {
    let radius = RADIUS * n;
    let mut center: Option<(f64, f64, f64)> = None;
    let mut angle = 0.0f64;
    let mut ticks = 0u64;
    let mut sneaking = false;
    let tick = Duration::from_millis(50);
    let mut next = Instant::now();
    loop {
        loop {
            let body = match play.conn.from_server.try_recv() {
                Ok(body) => body,
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => return Ok(()),
            };
            match ClientboundPlay::decode(&body) {
                Ok(ClientboundPlay::KeepAlive(k)) => play.send(&KeepAlive { id: k.id })?,
                Ok(ClientboundPlay::ChunkBatchFinished(_)) => play.send(&ChunkBatchReceived {
                    desired_chunks_per_tick: 64.0,
                })?,
                Ok(ClientboundPlay::PlayerPosition(t)) => {
                    let p = t.change.position;
                    play.send(&AcceptTeleportation {
                        id: t.teleport_id,
                        x: p.x,
                        y: p.y,
                        z: p.z,
                        y_rot: t.change.y_rot,
                        x_rot: t.change.x_rot,
                    })?;
                    if center.is_none() {
                        play.send(&PlayerLoaded)?;
                    }
                    // Walk around the point the server put the bot at.
                    center = Some((p.x - radius, p.y, p.z));
                    angle = 0.0;
                }
                Ok(ClientboundPlay::Disconnect(d)) => {
                    return Err(format!("kicked: {}", d.reason.plain()));
                }
                _ => {}
            }
        }
        if let Some((cx, cy, cz)) = center {
            angle = (angle + SPEED / radius) % std::f64::consts::TAU;
            let (sin, cos) = angle.sin_cos();
            // Facing along the circle: the direction is (-sin, cos) and Minecraft's yaw is
            // atan2(-dx, dz), which is the angle itself.
            let yaw = angle.to_degrees() as f32;
            play.send(&MovePlayerPosRot {
                x: cx + radius * cos,
                y: cy,
                z: cz + radius * sin,
                y_rot: yaw,
                x_rot: 0.0,
                flags: MoveFlags {
                    on_ground: true,
                    horizontal_collision: false,
                },
            })?;
            let sneak = ticks % SNEAK_CYCLE < SNEAK_TICKS;
            if sneak != sneaking {
                sneaking = sneak;
                play.send(&PlayerInput(Input {
                    shift: sneak,
                    ..Input::default()
                }))?;
            }
            play.send(&ClientTickEnd)?;
            ticks += 1;
        }
        next += tick;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now;
        }
    }
}
