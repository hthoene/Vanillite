//! One bot: login, configuration and the play loop (random walk, straight flight, spread or
//! standing still, chat, latency probes).
//!
//! Incoming packets are framed and counted; only the few a client must answer are decoded.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::sync::{OwnedSemaphorePermit, watch};
use vanillite_protocol::codec::{Encode, Reader, write_str, write_varint};
use vanillite_protocol::frame::{FrameDecoder, FrameEncoder};
use vanillite_protocol::ids;
use vanillite_protocol::packets::common::ClientInfo;
use vanillite_protocol::packets::configuration::serverbound as csb;
use vanillite_protocol::packets::handshake::{Intent, Intention};
use vanillite_protocol::packets::login::{
    CookieResponse, CustomQueryAnswer, Hello, LoginAcknowledged,
};
use vanillite_protocol::packets::play::clientbound::PlayerPosition;
use vanillite_protocol::packets::play::serverbound::{
    AcceptTeleportation, ChunkBatchReceived, ClientTickEnd, MoveFlags, MovePlayerPosRot,
    PlayerLoaded,
};
use vanillite_protocol::sets::{ClientboundConfiguration, ClientboundLogin};
use vanillite_protocol::types::Uuid;
use vanillite_protocol::{Decode, Packet, State};

use crate::{Config, Metrics, Movement};

/// Walking speed in blocks per tick (Vanilla's 4.317 blocks per second).
const WALK_SPEED: f64 = 4.317 / 20.0;
/// Cruise altitude of straight flight: above the highest overworld terrain, so no move
/// collides with blocks.
const FLIGHT_Y: f64 = 300.0;
/// Climb or descent towards the cruise altitude in blocks per tick.
const CLIMB_SPEED: f64 = 5.0;
/// Seconds between latency probes (`ping_request`).
const PROBE_INTERVAL: Duration = Duration::from_secs(5);
/// Chunks around the bot that count as "near" for the holes metric: a square of
/// (2 * NEAR + 1)² chunks.
const NEAR: i32 = 2;
/// Speed of the flight to the spread point in blocks per second (boat on blue ice).
const SPREAD_SPEED: f64 = 72.7;
/// Bytes read per `read` call.
const READ_CHUNK: usize = 16 * 1024;

type BotResult<T> = Result<T, String>;

/// Runs one bot until it is disconnected; records the outcome in `metrics`.
pub async fn run(
    index: u64,
    config: Arc<Config>,
    metrics: Arc<Metrics>,
    movement: watch::Receiver<Movement>,
    permit: OwnedSemaphorePermit,
) {
    let mut bot = Bot {
        movement,
        name: format!("{}{index:05}", config.prefix),
        rng: Rng::new(config.seed ^ index.wrapping_mul(0x9E37_79B9_7F4A_7C15)),
        config,
        metrics,
        conn: None,
    };
    let mut joined = false;
    let mut permit = Some(permit);
    let result = bot.play(&mut joined, &mut permit).await;
    let reason = match result {
        Ok(()) => "closed".to_owned(),
        Err(e) => e,
    };
    if joined {
        bot.metrics.online.fetch_sub(1, Ordering::Relaxed);
        bot.metrics.disconnected.fetch_add(1, Ordering::Relaxed);
    } else {
        bot.metrics.failed.fetch_add(1, Ordering::Relaxed);
    }
    bot.metrics.error(reason);
}

struct Bot {
    movement: watch::Receiver<Movement>,
    name: String,
    rng: Rng,
    config: Arc<Config>,
    metrics: Arc<Metrics>,
    conn: Option<Conn>,
}

/// What the play loop does with an incoming packet.
enum Action {
    KeepAlive(i64),
    Ping(i32),
    Teleport(PlayerPosition),
    BatchFinished,
    Chunk(i32, i32),
    Forget(i32, i32),
    Pong(i64),
    Disconnect(String),
    StartConfiguration,
}

impl Bot {
    async fn play(
        &mut self,
        joined: &mut bool,
        permit: &mut Option<OwnedSemaphorePermit>,
    ) -> BotResult<()> {
        let started = Instant::now();
        let stream = TcpStream::connect((self.config.host.as_str(), self.config.port))
            .await
            .map_err(|e| format!("connect: {e}"))?;
        stream
            .set_nodelay(true)
            .map_err(|e| format!("nodelay: {e}"))?;
        let conn = self
            .conn
            .insert(Conn::new(stream, Arc::clone(&self.metrics)));
        conn.login(&self.name, &self.config).await?;

        // Play: answer what must be answered, walk once the first chunk is there.
        let mut pos: Option<(f64, f64, f64)> = None;
        let mut home = (0.0, 0.0);
        let mut yaw: f64 = self.rng.next_f64() * 360.0;
        let mut movement = *self.movement.borrow_and_update();
        let mut next_turn = turn_deadline(movement);
        // Spread: the point this bot flies to (picked when the mode starts).
        let mut goal: Option<(f64, f64)> = None;
        let mut loaded: HashSet<(i32, i32)> = HashSet::new();
        // Full view: every chunk within the view distance (a circle) has arrived.
        let mut full_view = false;
        let view = i32::from(self.config.view_distance.max(1));
        let mut tick = tokio::time::interval_at(
            tokio::time::Instant::now() + Duration::from_millis(self.rng.next_u64() % 50),
            Duration::from_millis(50),
        );
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut probe = tokio::time::interval(PROBE_INTERVAL);
        let chat_every = (self.config.chat_interval > 0.0)
            .then(|| Duration::from_secs_f64(self.config.chat_interval));
        let mut next_chat = tokio::time::Instant::now()
            + chat_every.map_or(Duration::ZERO, |d| d.mul_f64(self.rng.next_f64()));
        let mut actions = Vec::new();
        loop {
            tokio::select! {
                ready = conn.stream.readable() => {
                    ready.map_err(|e| format!("read: {e}"))?;
                    conn.fill()?;
                    conn.drain_play(&mut actions)?;
                    for action in actions.drain(..) {
                        match action {
                            Action::KeepAlive(id) => {
                                let mut b = body(ids::play::serverbound::KEEP_ALIVE);
                                id.encode(&mut b);
                                conn.queue_body(&b)?;
                            }
                            Action::Ping(id) => {
                                let mut b = body(ids::play::serverbound::PONG);
                                id.encode(&mut b);
                                conn.queue_body(&b)?;
                            }
                            Action::Teleport(t) => {
                                let p = t.change.position;
                                conn.queue(&AcceptTeleportation {
                                    id: t.teleport_id,
                                    x: p.x,
                                    y: p.y,
                                    z: p.z,
                                    y_rot: t.change.y_rot,
                                    x_rot: t.change.x_rot,
                                })?;
                                if pos.is_none() {
                                    home = (p.x, p.z);
                                } else if *joined {
                                    self.metrics.teleports.fetch_add(1, Ordering::Relaxed);
                                }
                                pos = Some((p.x, p.y, p.z));
                            }
                            Action::BatchFinished => conn.queue(&ChunkBatchReceived {
                                desired_chunks_per_tick: 64.0,
                            })?,
                            Action::Forget(cx, cz) => {
                                loaded.remove(&(cx, cz));
                                self.metrics.chunks_forgotten.fetch_add(1, Ordering::Relaxed);
                            }
                            Action::Chunk(cx, cz) => {
                                loaded.insert((cx, cz));
                                self.metrics.chunks.fetch_add(1, Ordering::Relaxed);
                            }
                            Action::Pong(sent) => {
                                let now = micros_since(started);
                                let ms = now.saturating_sub(u64::try_from(sent).unwrap_or(0)) as f64 / 1e3;
                                if let Ok(mut v) = self.metrics.rtt_ms.lock() {
                                    v.push(ms);
                                }
                            }
                            Action::Disconnect(reason) => return Err(format!("kicked: {reason}")),
                            Action::StartConfiguration => {
                                return Err("server restarted configuration".into());
                            }
                        }
                    }
                    // In the world once the position and a first chunk are there, in either
                    // order (Paper may send chunks before the position).
                    if !*joined && pos.is_some() && !loaded.is_empty() {
                        *joined = true;
                        permit.take();
                        let ms = started.elapsed().as_secs_f64() * 1e3;
                        if let Ok(mut v) = self.metrics.join_ms.lock() {
                            v.push(ms);
                        }
                        self.metrics.joined.fetch_add(1, Ordering::Relaxed);
                        self.metrics.online.fetch_add(1, Ordering::Relaxed);
                        conn.queue(&PlayerLoaded)?;
                    }
                    if *joined
                        && !full_view
                        && let Some((x, _, z)) = pos
                    {
                        let (cx, cz) = ((x / 16.0).floor() as i32, (z / 16.0).floor() as i32);
                        full_view = (-view..=view).all(|dz| {
                            (-view..=view).all(|dx| {
                                dx * dx + dz * dz > view * view
                                    || loaded.contains(&(cx + dx, cz + dz))
                            })
                        });
                        if full_view && let Ok(mut v) = self.metrics.full_view_ms.lock() {
                            v.push(started.elapsed().as_secs_f64() * 1e3);
                        }
                    }
                    conn.flush().await?;
                }
                _ = tick.tick(), if *joined => {
                    let Some((x, y, z)) = pos.as_mut() else { continue };
                    if self.movement.has_changed().unwrap_or(false) {
                        movement = *self.movement.borrow_and_update();
                        next_turn = turn_deadline(movement);
                        home = (*x, *z);
                        yaw = self.rng.next_f64() * 360.0;
                        goal = None;
                    }
                    let speed = match movement {
                        Movement::Still => 0.0,
                        Movement::Spread { radius } => {
                            let (gx, gz) = *goal.get_or_insert_with(|| {
                                // Uniform in the disk around the point where the mode started.
                                let r = radius * self.rng.next_f64().sqrt();
                                let a = self.rng.next_f64() * std::f64::consts::TAU;
                                (*x + r * a.cos(), *z + r * a.sin())
                            });
                            let (dx, dz) = (gx - *x, gz - *z);
                            let left = (dx * dx + dz * dz).sqrt();
                            yaw = (-dx).atan2(dz).to_degrees();
                            climb(y, (SPREAD_SPEED / 20.0).min(left))
                        }
                        Movement::Random => {
                            // Drift the heading, turn back home outside the radius.
                            let (dx, dz) = (*x - home.0, *z - home.1);
                            if dx * dx + dz * dz > self.config.radius * self.config.radius {
                                yaw = (-dx).atan2(dz).to_degrees()
                                    + 180.0
                                    + self.rng.range(-30.0, 30.0);
                            } else {
                                yaw += self.rng.range(-15.0, 15.0);
                            }
                            WALK_SPEED
                        }
                        Movement::Straight { speed, turn } => {
                            if let Some(at) = next_turn
                                && tokio::time::Instant::now() >= at
                            {
                                next_turn = Some(at + Duration::from_secs_f64(turn));
                                yaw = self.rng.next_f64() * 360.0;
                            }
                            climb(y, speed / 20.0)
                        }
                    };
                    if movement == Movement::Still {
                        conn.queue(&ClientTickEnd)?;
                        conn.flush().await?;
                        continue;
                    }
                    let rad = yaw.to_radians();
                    *x -= rad.sin() * speed;
                    *z += rad.cos() * speed;
                    conn.queue(&MovePlayerPosRot {
                        x: *x,
                        y: *y,
                        z: *z,
                        y_rot: (yaw.rem_euclid(360.0)) as f32,
                        x_rot: 0.0,
                        flags: MoveFlags {
                            on_ground: movement == Movement::Random,
                            horizontal_collision: false,
                        },
                    })?;
                    conn.queue(&ClientTickEnd)?;
                    if let Some(every) = chat_every
                        && tokio::time::Instant::now() >= next_chat
                    {
                        next_chat += every;
                        let n = self.rng.next_u64() % 1000;
                        conn.queue_body(&chat_body(&format!("hello from {} #{n}", self.name)))?;
                    }
                    conn.flush().await?;
                }
                _ = probe.tick(), if *joined => {
                    if let Some((x, _, z)) = pos {
                        let (cx, cz) = ((x / 16.0).floor() as i32, (z / 16.0).floor() as i32);
                        let mut missing = 0;
                        for dz in -NEAR..=NEAR {
                            for dx in -NEAR..=NEAR {
                                missing += u64::from(!loaded.contains(&(cx + dx, cz + dz)));
                            }
                        }
                        self.metrics.near_missing.fetch_add(missing, Ordering::Relaxed);
                        self.metrics.near_checked.fetch_add(
                            ((2 * NEAR + 1) * (2 * NEAR + 1)) as u64,
                            Ordering::Relaxed,
                        );
                    }
                    let mut b = body(ids::play::serverbound::PING_REQUEST);
                    i64::try_from(micros_since(started)).unwrap_or(i64::MAX).encode(&mut b);
                    conn.queue_body(&b)?;
                    conn.flush().await?;
                }
            }
        }
    }
}

/// Moves `y` towards the cruise altitude; returns the horizontal speed: none while climbing,
/// so bots rise straight up out of the terrain instead of flying through hills (which servers
/// that check movement reject).
fn climb(y: &mut f64, speed: f64) -> f64 {
    let step = (FLIGHT_Y - *y).clamp(-CLIMB_SPEED, CLIMB_SPEED);
    *y += step;
    if step.abs() < CLIMB_SPEED { speed } else { 0.0 }
}

/// When straight flight turns next (`None`: never).
fn turn_deadline(movement: Movement) -> Option<tokio::time::Instant> {
    match movement {
        Movement::Straight { turn, .. } if turn > 0.0 => {
            Some(tokio::time::Instant::now() + Duration::from_secs_f64(turn))
        }
        _ => None,
    }
}

/// A packet body starting with its id.
fn body(id: i32) -> Vec<u8> {
    let mut b = Vec::with_capacity(16);
    write_varint(&mut b, id);
    b
}

/// An unsigned `chat` message: message, timestamp, salt, no signature, no acknowledgements.
fn chat_body(message: &str) -> Vec<u8> {
    let mut b = body(ids::play::serverbound::CHAT);
    write_str(&mut b, message);
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0));
    millis.encode(&mut b);
    0i64.encode(&mut b);
    false.encode(&mut b);
    write_varint(&mut b, 0);
    b.extend_from_slice(&[0, 0, 0]);
    b.push(0);
    b
}

fn micros_since(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX)
}

/// The connection with framing, compression and byte counters.
struct Conn {
    stream: TcpStream,
    decoder: FrameDecoder,
    encoder: FrameEncoder,
    out: Vec<u8>,
    metrics: Arc<Metrics>,
}

impl Conn {
    fn new(stream: TcpStream, metrics: Arc<Metrics>) -> Self {
        let mut decoder = FrameDecoder::new();
        decoder.set_limits(State::Play.limits());
        Self {
            stream,
            decoder,
            encoder: FrameEncoder::new(),
            out: Vec::new(),
            metrics,
        }
    }

    fn queue<P: Packet>(&mut self, packet: &P) -> BotResult<()> {
        self.encoder
            .packet(packet, &mut self.out)
            .map_err(|e| format!("encode {}: {e}", P::NAME))?;
        self.metrics.packets_out.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn queue_body(&mut self, body: &[u8]) -> BotResult<()> {
        self.encoder
            .body(body, &mut self.out)
            .map_err(|e| format!("encode: {e}"))?;
        self.metrics.packets_out.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    async fn flush(&mut self) -> BotResult<()> {
        if self.out.is_empty() {
            return Ok(());
        }
        self.stream
            .write_all(&self.out)
            .await
            .map_err(|e| format!("write: {e}"))?;
        self.metrics
            .bytes_out
            .fetch_add(self.out.len() as u64, Ordering::Relaxed);
        self.out.clear();
        Ok(())
    }

    /// Reads what the socket has without waiting.
    fn fill(&mut self) -> BotResult<()> {
        loop {
            let spare = self.decoder.spare(READ_CHUNK);
            match self.stream.try_read(spare) {
                Ok(0) => {
                    self.decoder.commit(READ_CHUNK, 0);
                    return Err("connection closed".into());
                }
                Ok(n) => {
                    self.decoder.commit(READ_CHUNK, n);
                    self.metrics.bytes_in.fetch_add(n as u64, Ordering::Relaxed);
                    if n < READ_CHUNK {
                        return Ok(());
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    self.decoder.commit(READ_CHUNK, 0);
                    return Ok(());
                }
                Err(e) => {
                    self.decoder.commit(READ_CHUNK, 0);
                    return Err(format!("read: {e}"));
                }
            }
        }
    }

    /// The next frame, waiting for the socket if needed (login and configuration only).
    async fn next_frame(&mut self) -> BotResult<Vec<u8>> {
        loop {
            if let Some(frame) = self
                .decoder
                .next_frame()
                .map_err(|e| format!("frame: {e}"))?
            {
                self.metrics.packets_in.fetch_add(1, Ordering::Relaxed);
                return Ok(frame.to_vec());
            }
            self.stream
                .readable()
                .await
                .map_err(|e| format!("read: {e}"))?;
            self.fill()?;
        }
    }

    /// Handshake, login (offline mode) and configuration; ends in the play state.
    async fn login(&mut self, name: &str, config: &Config) -> BotResult<()> {
        self.queue(&Intention {
            protocol_version: vanillite_protocol::PROTOCOL_VERSION,
            host: config.host.clone(),
            port: config.port,
            intent: Intent::Login,
        })?;
        self.queue(&Hello {
            name: name.into(),
            profile_id: Uuid::offline(name),
        })?;
        self.flush().await?;
        loop {
            let frame = self.next_frame().await?;
            match ClientboundLogin::decode(&frame).map_err(|e| format!("login: {e}"))? {
                ClientboundLogin::LoginCompression(c) => {
                    self.encoder.set_compression(c.threshold);
                    self.decoder.set_compression(c.threshold);
                }
                ClientboundLogin::LoginFinished(_) => {
                    self.queue(&LoginAcknowledged)?;
                    break;
                }
                ClientboundLogin::LoginDisconnect(d) => {
                    return Err(format!("login refused: {}", d.reason.0.plain()));
                }
                ClientboundLogin::EncryptionRequest(_) => {
                    return Err("server is in online mode".into());
                }
                ClientboundLogin::CustomQuery(q) => self.queue(&CustomQueryAnswer {
                    transaction_id: q.transaction_id,
                    payload: None,
                })?,
                ClientboundLogin::CookieRequest(c) => self.queue(&CookieResponse {
                    key: c.key,
                    payload: None,
                })?,
            }
            self.flush().await?;
        }

        self.queue(&csb::ClientInformation(ClientInfo {
            view_distance: config.view_distance,
            ..ClientInfo::default()
        }))?;
        self.flush().await?;
        use ids::configuration::clientbound as id;
        loop {
            let frame = self.next_frame().await?;
            let packet_id = Reader::new(&frame)
                .varint()
                .map_err(|e| format!("id: {e}"))?;
            // Registries, tags and the rest are only counted.
            if !matches!(
                packet_id,
                id::SELECT_KNOWN_PACKS
                    | id::KEEP_ALIVE
                    | id::PING
                    | id::CODE_OF_CONDUCT
                    | id::DISCONNECT
                    | id::FINISH_CONFIGURATION
                    | id::COOKIE_REQUEST
            ) {
                continue;
            }
            match ClientboundConfiguration::decode(&frame)
                .map_err(|e| format!("configuration: {e}"))?
            {
                ClientboundConfiguration::SelectKnownPacks(p) => {
                    self.queue(&csb::SelectKnownPacks { packs: p.packs })?;
                }
                ClientboundConfiguration::KeepAlive(k) => {
                    self.queue(&csb::KeepAlive { id: k.id })?
                }
                ClientboundConfiguration::Ping(p) => self.queue(&csb::Pong { id: p.id })?,
                ClientboundConfiguration::CodeOfConduct(_) => {
                    self.queue(&csb::AcceptCodeOfConduct)?
                }
                ClientboundConfiguration::CookieRequest(c) => self.queue(&csb::CookieResponse {
                    key: c.key,
                    payload: None,
                })?,
                ClientboundConfiguration::Disconnect(d) => {
                    return Err(format!("configuration refused: {}", d.reason.plain()));
                }
                ClientboundConfiguration::FinishConfiguration(_) => {
                    self.queue(&csb::FinishConfiguration)?;
                    self.flush().await?;
                    return Ok(());
                }
                _ => {}
            }
            self.flush().await?;
        }
    }

    /// Decodes the buffered play packets into actions; the rest is only counted.
    fn drain_play(&mut self, actions: &mut Vec<Action>) -> BotResult<()> {
        use ids::play::clientbound as id;
        let err = |e: vanillite_protocol::Error| format!("play: {e}");
        while let Some(frame) = self.decoder.next_frame().map_err(err)? {
            self.metrics.packets_in.fetch_add(1, Ordering::Relaxed);
            let mut r = Reader::new(frame);
            let action = match r.varint().map_err(err)? {
                id::KEEP_ALIVE => Action::KeepAlive(r.i64().map_err(err)?),
                id::PING => Action::Ping(r.i32().map_err(err)?),
                id::PONG_RESPONSE => Action::Pong(r.i64().map_err(err)?),
                id::PLAYER_POSITION => {
                    Action::Teleport(PlayerPosition::decode(&mut r).map_err(err)?)
                }
                id::CHUNK_BATCH_FINISHED => Action::BatchFinished,
                id::LEVEL_CHUNK_WITH_LIGHT => {
                    Action::Chunk(r.i32().map_err(err)?, r.i32().map_err(err)?)
                }
                id::FORGET_LEVEL_CHUNK => {
                    // `ChunkPos` as one long: x in the low, z in the high 32 bits.
                    let packed = r.i64().map_err(err)?;
                    Action::Forget(packed as i32, (packed >> 32) as i32)
                }
                id::START_CONFIGURATION => Action::StartConfiguration,
                id::DISCONNECT => Action::Disconnect(
                    vanillite_protocol::packets::play::clientbound::Disconnect::decode(&mut r)
                        .map_or_else(|_| "?".to_owned(), |d| d.reason.plain()),
                ),
                _ => continue,
            };
            actions.push(action);
        }
        Ok(())
    }
}

/// SplitMix64: deterministic per bot, so runs are comparable.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn range(&mut self, min: f64, max: f64) -> f64 {
        min + self.next_f64() * (max - min)
    }
}
