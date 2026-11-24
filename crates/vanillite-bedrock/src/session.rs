//! One Bedrock player: its RakNet peer, the Bedrock login sequence, the Java connection behind
//! it and the translation in both directions.
//!
//! Everything for one player runs on one task, so no state is shared except the read-only data,
//! the section cache and the skins of other Bedrock players.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustc_hash::{FxHashMap, FxHashSet};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use vanillite_protocol::Packet;
use vanillite_protocol::frame::encode_body;
use vanillite_protocol::packets::common::ClientInfo;
use vanillite_protocol::packets::configuration::serverbound as csb;
use vanillite_protocol::packets::handshake::{Intent, Intention};
use vanillite_protocol::packets::login::{Hello, LoginAcknowledged};
use vanillite_protocol::packets::play::clientbound::{self as pcb, GameMode, relative};
use vanillite_protocol::packets::play::serverbound as psb;
use vanillite_protocol::sets::{ClientboundConfiguration, ClientboundLogin, ClientboundPlay};
use vanillite_protocol::types::{GameProfile, Uuid};

use crate::batch::{self, Codec};
use crate::chunk::{self, BiomeTable, ChunkCache, Layout};
use crate::login::{self, TokenKeys, field};
use crate::num::{angle_deg, f32_of, floor_i32};
use crate::packets::{self as bp, id, input, move_mode, status};
use crate::raknet::RakConn;
use crate::skin::Skin;
use crate::{BedrockConfig, Connector, Status};

/// Eye height of a standing player; Bedrock positions players at their eyes.
const EYE: f64 = 1.62;

/// State shared by all sessions of one frontend.
pub(crate) struct Shared {
    pub config: BedrockConfig,
    pub connector: Connector,
    pub keys: Arc<TokenKeys>,
    pub cache: ChunkCache,
    /// Skins of connected Bedrock players by Java UUID.
    pub skins: Mutex<FxHashMap<u128, Arc<Skin>>>,
    pub plain: Arc<Skin>,
    pub status: Arc<Status>,
    pub guid: i64,
    pub entity_types: FxHashSet<String>,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared").finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    NetworkSettings,
    Login,
    Handshake,
    Packs,
    Joining,
    Playing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JavaState {
    Login,
    Configuration,
    Play,
}

#[derive(Debug, Clone)]
struct Entity {
    pos: [f64; 3],
    yaw: f32,
    pitch: f32,
    head_yaw: f32,
    player: bool,
}

/// The player's world on the Java side, after the Java `login`.
#[derive(Debug)]
struct World {
    entity_id: i32,
    layout: Layout,
    biomes: BiomeTable,
    biome_key: u64,
    game_mode: GameMode,
    hardcore: bool,
    started: bool,
    spawned: bool,
    pos: [f64; 3],
    yaw: f32,
    pitch: f32,
    pending_teleport: Option<(i32, [f64; 3])>,
    entities: FxHashMap<i32, Entity>,
    names: FxHashMap<u128, String>,
    listed: FxHashSet<u128>,
    batch_ack_due: bool,
    sneaking: bool,
    sprinting: bool,
    flying: bool,
    abilities: u8,
    walk_speed: f32,
    fly_speed: f32,
}

/// Bedrock runtime and unique id of a Java entity id.
fn rid(java: i32) -> u64 {
    u64::from(u32::from_ne_bytes(java.to_ne_bytes())).saturating_add(1)
}

fn uid(java: i32) -> i64 {
    i64::try_from(rid(java)).unwrap_or(i64::MAX)
}

fn f3(p: [f64; 3]) -> [f32; 3] {
    p.map(f32_of)
}

fn bedrock_game_mode(m: GameMode) -> i32 {
    match m {
        GameMode::Survival => 0,
        GameMode::Creative => 1,
        GameMode::Adventure => 2,
        GameMode::Spectator => 6,
    }
}

/// English text of Java disconnect keys a Bedrock client cannot translate.
fn reason_text(c: &vanillite_protocol::text::Component) -> String {
    let text = c.plain();
    match text.as_str() {
        "multiplayer.disconnect.not_whitelisted" => {
            "You are not whitelisted on this server!".into()
        }
        "multiplayer.disconnect.server_full" => "The server is full!".into(),
        "multiplayer.disconnect.duplicate_login" => "You logged in from another location".into(),
        "multiplayer.disconnect.kicked" => "Kicked by an operator".into(),
        "multiplayer.disconnect.server_shutdown" => "Server closed".into(),
        "multiplayer.disconnect.banned" => "You are banned from this server".into(),
        _ => text,
    }
}

/// One player.
pub(crate) struct Session {
    shared: Arc<Shared>,
    peer: SocketAddr,
    rak: RakConn,
    codec: Codec,
    phase: Phase,
    batch: Vec<u8>,
    started: Instant,
    closing: Option<Instant>,
    pub done: bool,
    window: (Instant, u32),
    uuid: u128,
    name: String,
    xuid: String,
    language: String,
    skin: Arc<Skin>,
    java_tx: Option<mpsc::Sender<Vec<u8>>>,
    /// A new Java receiver for the task loop to take.
    pub java_rx: Option<mpsc::Receiver<Vec<u8>>>,
    java: JavaState,
    biome_names: Vec<String>,
    dimension_types: Vec<String>,
    world: Option<World>,
    chunk_radius: i32,
    scratch: Vec<Vec<u8>>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("peer", &self.peer)
            .finish_non_exhaustive()
    }
}

impl Session {
    pub(crate) fn new(shared: Arc<Shared>, peer: SocketAddr, mtu: u16, now: Instant) -> Self {
        let c = &shared.config;
        let rak = RakConn::new(peer, mtu, c.raknet, now);
        let codec = Codec::new(
            c.compression_threshold,
            c.compression_level,
            c.max_batch_size,
            c.max_packets_per_batch,
        );
        let chunk_radius = i32::from(c.max_view_distance);
        let plain = Arc::clone(&shared.plain);
        Self {
            shared,
            peer,
            rak,
            codec,
            phase: Phase::NetworkSettings,
            batch: Vec::new(),
            started: now,
            closing: None,
            done: false,
            window: (now, 0),
            uuid: 0,
            name: String::new(),
            xuid: String::new(),
            language: "en_us".into(),
            skin: plain,
            java_tx: None,
            java_rx: None,
            java: JavaState::Login,
            biome_names: Vec::new(),
            dimension_types: Vec::new(),
            world: None,
            chunk_radius,
            scratch: Vec::new(),
        }
    }

    // ---- Output ---------------------------------------------------------------------------

    fn send(&mut self, packet: u32, body: &[u8]) {
        batch::push_packet(&mut self.batch, packet, body);
    }

    /// Encodes the pending packets as one batch.
    fn flush_batch(&mut self) {
        if self.batch.is_empty() {
            return;
        }
        let payload = self.codec.encode(&self.batch);
        self.batch.clear();
        self.rak.send_game(&payload);
    }

    /// Sends everything due on the socket.
    pub(crate) fn flush(&mut self, socket: &UdpSocket, now: Instant) {
        self.flush_batch();
        let peer = self.peer;
        self.rak.flush(now, |d| {
            // A full socket buffer drops the datagram; RakNet resends it.
            let _ = socket.try_send_to(d, peer);
        });
    }

    /// Disconnects with a message (shown by the client), then closes after a short delay.
    pub(crate) fn kick(&mut self, message: &str) {
        if self.closing.is_some() {
            return;
        }
        if !self.name.is_empty() {
            log::info!("Bedrock player {} disconnected: {message}", self.name);
        } else {
            log::debug!("Bedrock connection {} closed: {message}", self.peer);
        }
        self.send(id::DISCONNECT, &bp::disconnect(message));
        self.flush_batch();
        self.rak.disconnect();
        self.java_tx = None;
        self.closing = Some(Instant::now());
    }

    pub(crate) fn close_silently(&mut self) {
        self.java_tx = None;
        self.done = true;
    }

    fn java_send<P: Packet>(&mut self, packet: &P) {
        let Some(tx) = &self.java_tx else {
            return;
        };
        if tx.try_send(encode_body(packet)).is_err() {
            // The server does not keep up with this player's input (SEC-17).
            self.kick("Connection to the server lost");
        }
    }

    // ---- Timers -----------------------------------------------------------------------------

    pub(crate) fn tick(&mut self, now: Instant) {
        if let Some(t) = self.closing {
            if now.saturating_duration_since(t) > Duration::from_millis(500) {
                self.done = true;
            }
            return;
        }
        let c = &self.shared.config;
        if now.saturating_duration_since(self.rak.last_receive) > c.idle_timeout {
            self.close_silently();
            return;
        }
        if self.phase != Phase::Playing
            && now.saturating_duration_since(self.started) > c.login_timeout
        {
            self.kick("Login timed out");
            return;
        }
        if self.rak.overloaded() {
            self.kick("Connection too slow");
            return;
        }
        // Chunk flow control: acknowledge a batch once RakNet has sent most of it.
        let backlog_ok = self.rak.backlog() < self.shared.config.chunk_backlog;
        if let Some(w) = &mut self.world
            && w.batch_ack_due
            && backlog_ok
        {
            w.batch_ack_due = false;
            self.java_send(&psb::ChunkBatchReceived {
                desired_chunks_per_tick: 16.0,
            });
        }
    }

    // ---- Bedrock input ------------------------------------------------------------------

    pub(crate) fn datagram(&mut self, data: &[u8], now: Instant) {
        if self.closing.is_some() || self.done {
            return;
        }
        let received = match self.rak.receive(data, now) {
            Ok(r) => r,
            Err(e) => {
                log::debug!("Dropping Bedrock peer {}: {e}", self.peer);
                self.close_silently();
                return;
            }
        };
        if received.disconnected {
            self.close_silently();
            return;
        }
        for game in received.game {
            let mut packets = std::mem::take(&mut self.scratch);
            packets.clear();
            if let Err(e) = self.codec.decode(game, &mut packets) {
                self.kick(&format!("Bad packet: {e}"));
                return;
            }
            for packet in packets.drain(..) {
                if !self.count_packet(now) {
                    self.kick("Too many packets");
                    return;
                }
                let Ok((pid, body)) = batch::split_packet(&packet) else {
                    self.kick("Bad packet header");
                    return;
                };
                if let Err(e) = self.bedrock_packet(pid, body) {
                    self.kick(&format!("Bad packet {pid}: {e}"));
                    return;
                }
                if self.closing.is_some() {
                    return;
                }
            }
            self.scratch = packets;
        }
    }

    fn count_packet(&mut self, now: Instant) -> bool {
        if now.saturating_duration_since(self.window.0) >= Duration::from_secs(1) {
            self.window = (now, 0);
        }
        self.window.1 = self.window.1.saturating_add(1);
        self.window.1 <= self.shared.config.max_packets_per_second
    }

    fn bedrock_packet(&mut self, pid: u32, body: &[u8]) -> crate::io::Result<()> {
        match (self.phase, pid) {
            (Phase::NetworkSettings, id::REQUEST_NETWORK_SETTINGS) => {
                let protocol = crate::io::Reader::new(body).be_i32()?;
                if protocol != crate::data::PROTOCOL {
                    let s = if protocol < crate::data::PROTOCOL {
                        status::FAILED_CLIENT
                    } else {
                        status::FAILED_SERVER
                    };
                    self.send(id::PLAY_STATUS, &bp::play_status(s));
                    self.kick(&format!(
                        "This server runs Bedrock {}",
                        crate::data::VERSION
                    ));
                    return Ok(());
                }
                let threshold = self.shared.config.compression_threshold;
                self.send(id::NETWORK_SETTINGS, &bp::network_settings(threshold));
                self.flush_batch();
                self.codec.enable_compression();
                self.phase = Phase::Login;
            }
            (Phase::Login, id::LOGIN) => {
                let mut r = crate::io::Reader::new(body);
                let _protocol = r.be_i32()?;
                let request = r.bytes("login request", self.shared.config.max_batch_size)?;
                self.login(request);
            }
            (Phase::Handshake, id::CLIENT_TO_SERVER_HANDSHAKE) => {
                self.send(id::PLAY_STATUS, &bp::play_status(status::LOGIN_SUCCESS));
                self.send(id::RESOURCE_PACKS_INFO, &bp::resource_packs_info());
                self.phase = Phase::Packs;
            }
            (Phase::Packs, id::RESOURCE_PACK_CLIENT_RESPONSE) => {
                match bp::read_pack_response(body)? {
                    0 => self.kick("Resource packs refused"),
                    1 | 2 => self.send(id::RESOURCE_PACK_STACK, &bp::resource_pack_stack()),
                    3 => self.connect_java(),
                    _ => {}
                }
            }
            (Phase::Joining | Phase::Playing, id::REQUEST_CHUNK_RADIUS) => {
                let requested = bp::read_chunk_radius(body)?;
                let radius = requested.clamp(2, i32::from(self.shared.config.max_view_distance));
                self.chunk_radius = radius;
                self.send(id::CHUNK_RADIUS_UPDATED, &bp::chunk_radius_updated(radius));
                if self.java == JavaState::Play {
                    let info = self.client_info();
                    self.java_send(&psb::ClientInformation(info));
                }
                self.publish_chunks();
            }
            (Phase::Playing, id::SET_LOCAL_PLAYER_AS_INITIALIZED) => {
                self.java_send(&psb::PlayerLoaded);
            }
            (Phase::Playing, id::PLAYER_AUTH_INPUT) => self.auth_input(&bp::read_auth_input(body)?),
            (Phase::Playing, id::TEXT) => {
                let message = bp::read_text_message(body)?;
                let message: String = message
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(256)
                    .collect();
                if !message.is_empty() {
                    self.java_send(&psb::Chat {
                        message,
                        timestamp: i64::try_from(login::now_secs())
                            .unwrap_or(0)
                            .saturating_mul(1000),
                        salt: 0,
                        signature: None,
                        last_seen: Default::default(),
                    });
                }
            }
            (Phase::Playing, id::COMMAND_REQUEST) => {
                let line = bp::read_command(body)?;
                let command: String = line
                    .strip_prefix('/')
                    .unwrap_or(&line)
                    .chars()
                    .filter(|c| !c.is_control())
                    .collect();
                if !command.is_empty() {
                    self.java_send(&psb::ChatCommand { command });
                }
            }
            (Phase::NetworkSettings | Phase::Login | Phase::Handshake, _) => {
                self.kick("Unexpected packet during login");
            }
            // Everything else (inventory, interaction, emotes, ...) is not translated yet.
            _ => {}
        }
        Ok(())
    }

    fn login(&mut self, request: &[u8]) {
        let online = self.shared.config.online_mode;
        let verified = match login::verify(request, &self.shared.keys, online, login::now_secs()) {
            Ok(v) => v,
            Err(e) => {
                log::info!("Bedrock login from {} refused: {e}", self.peer);
                self.kick(&format!("Login failed: {e}"));
                return;
            }
        };
        let (uuid, name) = login::java_profile(&verified.identity, &self.shared.config.name_prefix);
        self.uuid = uuid;
        self.name = name;
        self.xuid = verified.identity.xuid.clone();
        if let Some(lang) = field(&verified.client_data, "LanguageCode").as_str() {
            self.language = lang.to_ascii_lowercase().chars().take(16).collect();
        }
        if let Some(skin) = Skin::from_client_data(&verified.client_data) {
            self.skin = Arc::new(skin);
        }
        let handshake = match login::handshake(&verified.identity.client_key) {
            Ok(h) => h,
            Err(e) => {
                self.kick(&e.0);
                return;
            }
        };
        self.send(
            id::SERVER_TO_CLIENT_HANDSHAKE,
            &bp::server_to_client_handshake(&handshake.jwt),
        );
        self.flush_batch();
        self.codec.enable_encryption(handshake.key);
        self.phase = Phase::Handshake;
        log::info!(
            "Bedrock player {} (XUID {}) logs in as {} ({})",
            verified.identity.gamertag,
            if self.xuid.is_empty() {
                "none"
            } else {
                &self.xuid
            },
            self.name,
            Uuid(self.uuid)
        );
    }

    fn client_info(&self) -> ClientInfo {
        ClientInfo {
            language: self.language.clone(),
            view_distance: i8::try_from(self.chunk_radius).unwrap_or(i8::MAX),
            ..ClientInfo::default()
        }
    }

    fn connect_java(&mut self) {
        if self.java_tx.is_some() {
            return;
        }
        let profile = GameProfile {
            id: Uuid(self.uuid),
            name: self.name.clone(),
            properties: Vec::new(),
        };
        let conn = (self.shared.connector)(profile, self.peer);
        self.java_tx = Some(conn.to_server);
        self.java_rx = Some(conn.from_server);
        self.java = JavaState::Login;
        self.phase = Phase::Joining;
        if let Ok(mut skins) = self.shared.skins.lock() {
            skins.insert(self.uuid, Arc::clone(&self.skin));
        }
        self.java_send(&Intention {
            protocol_version: vanillite_protocol::PROTOCOL_VERSION,
            host: "bedrock".into(),
            port: self.shared.config.bind.port(),
            intent: Intent::Login,
        });
        self.java_send(&Hello {
            name: self.name.clone(),
            profile_id: Uuid(self.uuid),
        });
    }

    fn auth_input(&mut self, i: &bp::AuthInput) {
        let Some(w) = &mut self.world else {
            return;
        };
        let eye = i.position.map(f64::from);
        let feet = [eye[0], eye[1] - EYE, eye[2]];
        if let Some((tid, target)) = w.pending_teleport {
            let near = feet.iter().zip(target).all(|(a, b)| (a - b).abs() < 1.0);
            if !(i.has(input::HANDLED_TELEPORT) || near) {
                self.java_send(&psb::ClientTickEnd);
                return;
            }
            w.pending_teleport = None;
            let (yaw, pitch) = (w.yaw, w.pitch);
            self.java_send(&psb::AcceptTeleportation {
                id: tid,
                x: target[0],
                y: target[1],
                z: target[2],
                y_rot: yaw,
                x_rot: pitch,
            });
            let Some(w) = &mut self.world else {
                return;
            };
            w.pos = target;
        }
        let Some(w) = &mut self.world else {
            return;
        };
        w.pos = feet;
        w.yaw = i.yaw;
        w.pitch = i.pitch;
        let on_ground = i.has(input::VERTICAL_COLLISION) && i.delta[1] <= 0.0;
        let mut commands: Vec<psb::PlayerAction> = Vec::new();
        let mut input_changed = false;
        if i.has(input::START_SNEAKING) && !w.sneaking {
            w.sneaking = true;
            input_changed = true;
        }
        if i.has(input::STOP_SNEAKING) && w.sneaking {
            w.sneaking = false;
            input_changed = true;
        }
        if i.has(input::START_SPRINTING) && !w.sprinting {
            w.sprinting = true;
            commands.push(psb::PlayerAction::StartSprinting);
        }
        if i.has(input::STOP_SPRINTING) && w.sprinting {
            w.sprinting = false;
            commands.push(psb::PlayerAction::StopSprinting);
        }
        if i.has(input::START_FLYING) {
            w.flying = true;
        }
        if i.has(input::STOP_FLYING) {
            w.flying = false;
        }
        let jumping = i.has(input::JUMP_DOWN) || i.has(input::START_JUMPING);
        let sneaking = w.sneaking;
        let entity_id = w.entity_id;
        let move_packet = psb::MovePlayerPosRot {
            x: feet[0],
            y: feet[1],
            z: feet[2],
            y_rot: i.yaw,
            x_rot: i.pitch,
            flags: psb::MoveFlags {
                on_ground,
                horizontal_collision: i.has(input::HORIZONTAL_COLLISION),
            },
        };
        if input_changed || jumping {
            self.java_send(&psb::PlayerInput(psb::Input {
                jump: jumping,
                shift: sneaking,
                ..psb::Input::default()
            }));
        }
        for action in commands {
            self.java_send(&psb::PlayerCommand {
                entity_id,
                action,
                data: 0,
            });
        }
        self.java_send(&move_packet);
        self.java_send(&psb::ClientTickEnd);
    }

    // ---- Java input ---------------------------------------------------------------------

    pub(crate) fn java_closed(&mut self) {
        self.java_rx = None;
        if self.java_tx.is_some() {
            self.kick("Disconnected from the server");
        }
    }

    pub(crate) fn java_packet(&mut self, body: &[u8]) {
        if self.closing.is_some() {
            return;
        }
        match self.java {
            JavaState::Login => self.java_login(body),
            JavaState::Configuration => self.java_configuration(body),
            JavaState::Play => self.java_play(body),
        }
    }

    fn java_login(&mut self, body: &[u8]) {
        match ClientboundLogin::decode(body) {
            Ok(ClientboundLogin::LoginFinished(_)) => {
                self.java_send(&LoginAcknowledged);
                self.java = JavaState::Configuration;
                let info = self.client_info();
                self.java_send(&csb::ClientInformation(info));
            }
            Ok(ClientboundLogin::LoginDisconnect(d)) => self.kick(&reason_text(&d.reason.0)),
            Ok(ClientboundLogin::CustomQuery(q)) => {
                self.java_send(&vanillite_protocol::packets::login::CustomQueryAnswer {
                    transaction_id: q.transaction_id,
                    payload: None,
                });
            }
            Ok(ClientboundLogin::CookieRequest(c)) => {
                self.java_send(&vanillite_protocol::packets::login::CookieResponse {
                    key: c.key,
                    payload: None,
                });
            }
            Ok(_) => {}
            Err(e) => self.kick(&format!("Bad server packet: {e}")),
        }
    }

    fn java_configuration(&mut self, body: &[u8]) {
        match ClientboundConfiguration::decode(body) {
            Ok(ClientboundConfiguration::SelectKnownPacks(p)) => {
                self.java_send(&csb::SelectKnownPacks { packs: p.packs });
            }
            Ok(ClientboundConfiguration::KeepAlive(k)) => {
                self.java_send(&csb::KeepAlive { id: k.id })
            }
            Ok(ClientboundConfiguration::Ping(p)) => self.java_send(&csb::Pong { id: p.id }),
            Ok(ClientboundConfiguration::CodeOfConduct(_)) => {
                self.java_send(&csb::AcceptCodeOfConduct)
            }
            Ok(ClientboundConfiguration::CookieRequest(c)) => {
                self.java_send(&csb::CookieResponse {
                    key: c.key,
                    payload: None,
                });
            }
            Ok(ClientboundConfiguration::RegistryData(r)) => {
                let names = r.entries.iter().map(|e| e.id.as_str().to_owned()).collect();
                match r.registry.as_str() {
                    "minecraft:worldgen/biome" => self.biome_names = names,
                    "minecraft:dimension_type" => self.dimension_types = names,
                    _ => {}
                }
            }
            Ok(ClientboundConfiguration::FinishConfiguration(_)) => {
                self.java_send(&csb::FinishConfiguration);
                self.java = JavaState::Play;
            }
            Ok(ClientboundConfiguration::Disconnect(d)) => self.kick(&reason_text(&d.reason)),
            Ok(_) => {}
            Err(e) => self.kick(&format!("Bad server packet: {e}")),
        }
    }

    fn java_play(&mut self, body: &[u8]) {
        let packet = match ClientboundPlay::decode(body) {
            Ok(p) => p,
            Err(e) => {
                log::debug!("Undecodable Java packet for {}: {e}", self.name);
                return;
            }
        };
        match packet {
            ClientboundPlay::Login(l) => self.java_join(&l),
            ClientboundPlay::PlayerPosition(p) => self.teleport(&p),
            ClientboundPlay::KeepAlive(k) => self.java_send(&psb::KeepAlive { id: k.id }),
            ClientboundPlay::LevelChunkWithLight(c) => self.chunk(c.x, c.z, &c.data.0),
            ClientboundPlay::ChunkBatchFinished(_) => {
                if let Some(w) = &mut self.world {
                    w.batch_ack_due = true;
                }
                self.spawn_if_ready();
            }
            ClientboundPlay::SetChunkCacheCenter(_) => self.publish_chunks(),
            ClientboundPlay::Disconnect(d) => self.kick(&reason_text(&d.reason)),
            ClientboundPlay::SystemChat(c) => {
                let text = c.content.plain();
                if c.overlay {
                    self.send(id::TEXT, &bp::text_tip(&text));
                } else if !text.is_empty() {
                    self.send(id::TEXT, &bp::text_raw(&text));
                }
            }
            ClientboundPlay::PlayerChat(c) => {
                let content = c
                    .unsigned_content
                    .as_ref()
                    .map_or_else(|| c.content.clone(), |u| u.plain());
                let line = format!("<{}> {content}", c.chat_type.name.plain());
                self.send(id::TEXT, &bp::text_raw(&line));
            }
            ClientboundPlay::DisguisedChat(c) => {
                let line = format!("<{}> {}", c.chat_type.name.plain(), c.message.plain());
                self.send(id::TEXT, &bp::text_raw(&line));
            }
            ClientboundPlay::Commands(t) => {
                let tree = &t.0;
                let root = usize::try_from(tree.root)
                    .ok()
                    .and_then(|r| tree.nodes.get(r));
                let names: Vec<String> = root
                    .map(|r| r.children.as_slice())
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|&c| tree.nodes.get(usize::try_from(c).ok()?))
                    .filter_map(|n| match &n.kind {
                        vanillite_protocol::commands::NodeKind::Literal(l) => Some(l.clone()),
                        _ => None,
                    })
                    .collect();
                self.send(id::AVAILABLE_COMMANDS, &bp::available_commands(&names));
            }
            ClientboundPlay::GameEvent(e) if e.event == pcb::GameEvent::CHANGE_GAME_MODE => {
                let mode = match crate::num::f32_to_u8(e.param) {
                    1 => GameMode::Creative,
                    2 => GameMode::Adventure,
                    3 => GameMode::Spectator,
                    _ => GameMode::Survival,
                };
                if let Some(w) = &mut self.world {
                    w.game_mode = mode;
                }
                self.send(
                    id::SET_PLAYER_GAME_TYPE,
                    &bp::set_player_game_type(bedrock_game_mode(mode)),
                );
                self.send_abilities();
            }
            ClientboundPlay::PlayerAbilities(a) => {
                if let Some(w) = &mut self.world {
                    w.abilities = a.flags;
                    w.fly_speed = a.flying_speed;
                    w.walk_speed = a.walking_speed;
                }
                self.send_abilities();
            }
            ClientboundPlay::PlayerInfoUpdate(u) => self.player_info(&u),
            ClientboundPlay::PlayerInfoRemove(r) => {
                let ids: Vec<u128> = r.ids.iter().map(|u| u.0).collect();
                if let Some(w) = &mut self.world {
                    for id in &ids {
                        w.names.remove(id);
                        w.listed.remove(id);
                    }
                }
                self.send(id::PLAYER_LIST, &bp::player_list_remove(&ids));
            }
            ClientboundPlay::AddEntity(e) => self.add_entity(&e),
            ClientboundPlay::RemoveEntities(r) => {
                for &e in &r.ids.0 {
                    let known = self
                        .world
                        .as_mut()
                        .and_then(|w| w.entities.remove(&e))
                        .is_some();
                    if known {
                        self.send(id::REMOVE_ACTOR, &bp::remove_actor(uid(e)));
                    }
                }
            }
            ClientboundPlay::MoveEntityPos(m) => {
                self.move_entity(m.entity_id, Some(&m.delta), None, m.on_ground);
            }
            ClientboundPlay::MoveEntityPosRot(m) => {
                self.move_entity(
                    m.entity_id,
                    Some(&m.delta),
                    Some((angle_deg(m.y_rot), angle_deg(m.x_rot))),
                    m.on_ground,
                );
            }
            ClientboundPlay::MoveEntityRot(m) => {
                self.move_entity(
                    m.entity_id,
                    None,
                    Some((angle_deg(m.y_rot), angle_deg(m.x_rot))),
                    m.on_ground,
                );
            }
            ClientboundPlay::EntityPositionSync(s) => {
                let target = match &s.path {
                    pcb::PositionPath::Linear(p) => *p,
                    pcb::PositionPath::Stepped(steps) => {
                        steps.last().map(|(p, _)| *p).unwrap_or_default()
                    }
                };
                if let Some(e) = self
                    .world
                    .as_mut()
                    .and_then(|w| w.entities.get_mut(&s.entity_id))
                {
                    e.pos = [target.x, target.y, target.z];
                    e.yaw = s.y_rot;
                    e.pitch = s.x_rot;
                }
                self.send_entity_move(s.entity_id, s.on_ground, true);
            }
            ClientboundPlay::RotateHead(h) => {
                if let Some(e) = self
                    .world
                    .as_mut()
                    .and_then(|w| w.entities.get_mut(&h.entity_id))
                {
                    e.head_yaw = angle_deg(h.head_y_rot);
                }
                self.send_entity_move(h.entity_id, false, false);
            }
            ClientboundPlay::Unhandled { id } => self.java_unhandled(id, body),
            _ => {}
        }
    }

    fn java_unhandled(&mut self, packet: i32, body: &[u8]) {
        use vanillite_protocol::ids::play::clientbound as ids;
        let mut r = vanillite_protocol::codec::Reader::new(body);
        if r.varint().is_err() {
            return;
        }
        match packet {
            ids::BLOCK_UPDATE => {
                let (Ok(pos), Ok(state)) = (r.i64(), r.varint()) else {
                    return;
                };
                let p = vanillite_protocol::types::BlockPos::unpack(pos);
                self.block_update([p.x, p.y, p.z], u32::try_from(state).unwrap_or(0));
            }
            ids::SECTION_BLOCKS_UPDATE => {
                let Ok(section) = r.i64() else {
                    return;
                };
                let sx = i32::try_from(section.wrapping_shr(42)).unwrap_or(0);
                let sy = i32::try_from(section.wrapping_shl(44).wrapping_shr(44)).unwrap_or(0);
                let sz = i32::try_from(section.wrapping_shl(22).wrapping_shr(42)).unwrap_or(0);
                let Ok(n) = r.varint() else {
                    return;
                };
                for _ in 0..n.clamp(0, 4096) {
                    let Ok(v) = r.varlong() else {
                        return;
                    };
                    let state = u32::try_from(v.wrapping_shr(12)).unwrap_or(0);
                    let local = i32::try_from(v & 0xfff).unwrap_or(0);
                    let pos = [
                        sx.wrapping_mul(16).wrapping_add(local.wrapping_shr(8) & 15),
                        sy.wrapping_mul(16).wrapping_add(local & 15),
                        sz.wrapping_mul(16).wrapping_add(local.wrapping_shr(4) & 15),
                    ];
                    self.block_update(pos, state);
                }
            }
            ids::START_CONFIGURATION => {
                self.java_send(&psb::ConfigurationAcknowledged);
                self.java = JavaState::Configuration;
            }
            _ => {}
        }
    }

    fn block_update(&mut self, pos: [i32; 3], java_state: u32) {
        let blocks = crate::data::Blocks::get();
        let e = blocks.entry(java_state);
        let water = if e & crate::data::WATERLOGGED != 0 {
            blocks.water
        } else {
            blocks.air
        };
        self.send(
            id::UPDATE_BLOCK,
            &bp::update_block(pos, u32::from(e & !crate::data::WATERLOGGED), 0),
        );
        self.send(id::UPDATE_BLOCK, &bp::update_block(pos, water, 1));
    }

    fn java_join(&mut self, l: &pcb::Login) {
        let dim_type = usize::try_from(l.spawn.dimension_type)
            .ok()
            .and_then(|i| self.dimension_types.get(i))
            .cloned()
            .unwrap_or_else(|| l.spawn.dimension.as_str().to_owned());
        let (min_y, height) = match dim_type.as_str() {
            "minecraft:the_nether" | "minecraft:the_end" => (0, 256),
            _ => (-64, 384),
        };
        let layout = Layout::for_dimension(l.spawn.dimension.as_str(), min_y, height);
        let biomes = BiomeTable::new(&self.biome_names);
        let biome_key = {
            use std::hash::{Hash, Hasher};
            let mut h = rustc_hash::FxHasher::default();
            biomes.hash(&mut h);
            h.finish()
        };
        if self.world.as_ref().is_some_and(|w| w.started) {
            // A second login (server switch through a proxy) is not translated yet.
            self.kick("Server switch is not supported for Bedrock players yet");
            return;
        }
        self.world = Some(World {
            entity_id: l.entity_id,
            layout,
            biomes,
            biome_key,
            game_mode: l.spawn.game_mode,
            hardcore: l.hardcore,
            started: false,
            spawned: false,
            pos: [0.0; 3],
            yaw: 0.0,
            pitch: 0.0,
            pending_teleport: None,
            entities: FxHashMap::default(),
            names: FxHashMap::default(),
            listed: FxHashSet::default(),
            batch_ack_due: false,
            sneaking: false,
            sprinting: false,
            flying: false,
            abilities: 0,
            walk_speed: 0.1,
            fly_speed: 0.05,
        });
    }

    fn teleport(&mut self, p: &pcb::PlayerPosition) {
        let Some(w) = &mut self.world else {
            return;
        };
        let c = &p.change;
        let rel = |bit: i32| p.relatives & bit != 0;
        let target = [
            if rel(relative::X) {
                w.pos[0] + c.position.x
            } else {
                c.position.x
            },
            if rel(relative::Y) {
                w.pos[1] + c.position.y
            } else {
                c.position.y
            },
            if rel(relative::Z) {
                w.pos[2] + c.position.z
            } else {
                c.position.z
            },
        ];
        w.yaw = if rel(relative::Y_ROT) {
            w.yaw + c.y_rot
        } else {
            c.y_rot
        };
        w.pitch = if rel(relative::X_ROT) {
            w.pitch + c.x_rot
        } else {
            c.x_rot
        };
        if !w.started {
            w.pos = target;
            w.started = true;
            self.start_game();
            let (yaw, pitch) = self.world.as_ref().map_or((0.0, 0.0), |w| (w.yaw, w.pitch));
            self.java_send(&psb::AcceptTeleportation {
                id: p.teleport_id,
                x: target[0],
                y: target[1],
                z: target[2],
                y_rot: yaw,
                x_rot: pitch,
            });
            return;
        }
        w.pending_teleport = Some((p.teleport_id, target));
        let (yaw, pitch, rid) = (w.yaw, w.pitch, rid(w.entity_id));
        let eye = [target[0], target[1] + EYE, target[2]];
        self.send(
            id::MOVE_PLAYER,
            &bp::move_player(rid, f3(eye), pitch, yaw, yaw, move_mode::TELEPORT, false, 0),
        );
    }

    fn start_game(&mut self) {
        let Some(w) = &self.world else {
            return;
        };
        let eye = [w.pos[0], w.pos[1] + EYE, w.pos[2]];
        let spawn = [
            floor_i32(w.pos[0]),
            floor_i32(w.pos[1]),
            floor_i32(w.pos[2]),
        ];
        let body = bp::start_game(&bp::StartGame {
            entity_id: uid(w.entity_id),
            game_mode: bedrock_game_mode(w.game_mode),
            position: f3(eye),
            yaw: w.yaw,
            pitch: w.pitch,
            dimension: w.layout.dimension,
            difficulty: 2,
            spawn,
            hardcore: w.hardcore,
            world_name: &self.shared.config.world_name,
            permission: 1,
        });
        let runtime = rid(w.entity_id);
        let own = uid(w.entity_id);
        self.send(id::START_GAME, &body);
        self.send(id::ITEM_REGISTRY, crate::data::ITEM_REGISTRY);
        self.send(id::BIOME_DEFINITION_LIST, crate::data::BIOME_DEFINITIONS);
        self.send(
            id::AVAILABLE_ACTOR_IDENTIFIERS,
            crate::data::ENTITY_IDENTIFIERS,
        );
        self.send(id::CREATIVE_CONTENT, &bp::creative_content());
        self.send(id::SET_COMMANDS_ENABLED, &bp::set_commands_enabled(true));
        self.send(
            id::UPDATE_ATTRIBUTES,
            &bp::update_movement_speed(runtime, 0.1, 0),
        );
        let radius = self.chunk_radius;
        self.send(id::CHUNK_RADIUS_UPDATED, &bp::chunk_radius_updated(radius));
        self.send_abilities();
        self.publish_chunks();
        // The player's own entry, so its skin shows in third person and the player list.
        let skin = Arc::clone(&self.skin);
        let entry = bp::ListEntry {
            uuid: self.uuid,
            entity_id: own,
            name: &self.name,
            xuid: &self.xuid,
            skin: &skin,
        };
        let body = bp::player_list_add(&[entry]);
        self.send(id::PLAYER_LIST, &body);
        if let Some(w) = &mut self.world {
            w.listed.insert(self.uuid);
        }
        self.phase = Phase::Playing;
    }

    fn spawn_if_ready(&mut self) {
        let Some(w) = &mut self.world else {
            return;
        };
        if w.started && !w.spawned {
            w.spawned = true;
            self.send(id::PLAY_STATUS, &bp::play_status(status::PLAYER_SPAWN));
        }
    }

    fn publish_chunks(&mut self) {
        let Some(w) = &self.world else {
            return;
        };
        if !w.started {
            return;
        }
        let pos = [
            floor_i32(w.pos[0]),
            floor_i32(w.pos[1]),
            floor_i32(w.pos[2]),
        ];
        let radius = u32::try_from(self.chunk_radius.saturating_mul(16)).unwrap_or(0);
        self.send(
            id::NETWORK_CHUNK_PUBLISHER_UPDATE,
            &bp::chunk_publisher_update(pos, radius),
        );
    }

    fn send_abilities(&mut self) {
        let Some(w) = &self.world else {
            return;
        };
        use bp::ability::*;
        let mut values = BUILD | MINE | DOORS | CONTAINERS | ATTACK_PLAYERS | ATTACK_MOBS;
        let flags = w.abilities;
        if flags & 1 != 0 {
            values |= INVULNERABLE;
        }
        if flags & 2 != 0 || w.flying {
            values |= FLYING;
        }
        if flags & 4 != 0 {
            values |= MAY_FLY;
        }
        if flags & 8 != 0 {
            values |= INSTABUILD;
        }
        if w.game_mode == GameMode::Spectator {
            values = (values | NO_CLIP | FLYING | MAY_FLY)
                & !(BUILD | MINE | ATTACK_MOBS | ATTACK_PLAYERS);
        }
        let body = bp::update_abilities(uid(w.entity_id), false, values, w.fly_speed, w.walk_speed);
        self.send(id::UPDATE_ABILITIES, &body);
    }

    fn chunk(&mut self, x: i32, z: i32, data: &[u8]) {
        let Some(w) = &self.world else {
            return;
        };
        match chunk::translate(
            x,
            z,
            data,
            w.layout,
            &w.biomes,
            w.biome_key,
            &self.shared.cache,
        ) {
            Ok(body) => self.send(id::LEVEL_CHUNK, &body),
            Err(e) => log::debug!("Cannot translate chunk {x} {z} for {}: {e}", self.name),
        }
    }

    fn player_info(&mut self, u: &pcb::PlayerInfoUpdate) {
        use pcb::info_action::ADD_PLAYER;
        if u.actions & ADD_PLAYER == 0 {
            return;
        }
        let skins = self
            .shared
            .skins
            .lock()
            .map(|s| s.clone())
            .unwrap_or_default();
        let mut owned: Vec<(u128, String, Arc<Skin>)> = Vec::new();
        for e in &u.entries {
            let Some(profile) = &e.profile else {
                continue;
            };
            let uuid = e.id.0;
            if let Some(w) = &mut self.world {
                w.names.insert(uuid, profile.name.clone());
                if uuid == self.uuid || !w.listed.insert(uuid) {
                    continue;
                }
            }
            let skin = skins
                .get(&uuid)
                .cloned()
                .unwrap_or_else(|| Arc::clone(&self.shared.plain));
            owned.push((uuid, profile.name.clone(), skin));
        }
        if owned.is_empty() {
            return;
        }
        let entries: Vec<bp::ListEntry<'_>> = owned
            .iter()
            .map(|(uuid, name, skin)| bp::ListEntry {
                uuid: *uuid,
                entity_id: 0,
                name,
                xuid: "",
                skin,
            })
            .collect();
        let body = bp::player_list_add(&entries);
        self.send(id::PLAYER_LIST, &body);
    }

    fn add_entity(&mut self, e: &pcb::AddEntity) {
        let Some(w) = &mut self.world else {
            return;
        };
        let pos = [e.position.x, e.position.y, e.position.z];
        let entity = Entity {
            pos,
            yaw: angle_deg(e.y_rot),
            pitch: angle_deg(e.x_rot),
            head_yaw: angle_deg(e.y_head_rot),
            player: false,
        };
        let kind = u16::try_from(e.entity_type)
            .ok()
            .map(vanillite_data::entity::EntityTypeId);
        if kind == Some(vanillite_data::entity::ids::PLAYER) {
            let name = w.names.get(&e.uuid.0).cloned().unwrap_or_default();
            let mode = bedrock_game_mode(w.game_mode);
            w.entities.insert(
                e.entity_id,
                Entity {
                    player: true,
                    ..entity.clone()
                },
            );
            let eye = [pos[0], pos[1] + EYE, pos[2]];
            let body = bp::add_player(&bp::AddPlayer {
                uuid: e.uuid.0,
                name: &name,
                entity_id: uid(e.entity_id),
                pos: f3(eye),
                pitch: entity.pitch,
                yaw: entity.yaw,
                head_yaw: entity.head_yaw,
                game_mode: if mode == 6 { 0 } else { mode },
                sneaking: false,
            });
            self.send(id::ADD_PLAYER, &body);
            return;
        }
        let Some(name) = kind.map(|k| k.name()) else {
            return;
        };
        if !self.shared.entity_types.contains(name) || name == "minecraft:item" {
            return;
        }
        w.entities.insert(e.entity_id, entity.clone());
        let v = e.movement.0;
        let body = bp::add_actor(
            uid(e.entity_id),
            name,
            f3(pos),
            f3([v.x, v.y, v.z]),
            entity.pitch,
            entity.yaw,
            entity.head_yaw,
        );
        self.send(id::ADD_ACTOR, &body);
    }

    fn move_entity(
        &mut self,
        eid: i32,
        delta: Option<&pcb::VecDelta>,
        rot: Option<(f32, f32)>,
        on_ground: bool,
    ) {
        let Some(e) = self.world.as_mut().and_then(|w| w.entities.get_mut(&eid)) else {
            return;
        };
        if let Some(d) = delta {
            let steps: Vec<[i16; 3]> = match d {
                pcb::VecDelta::Linear(d) => vec![*d],
                pcb::VecDelta::Stepped(s) => s.iter().map(|(_, d)| *d).collect(),
            };
            for d in steps {
                for (p, v) in e.pos.iter_mut().zip(d) {
                    *p += f64::from(v) / 4096.0;
                }
            }
        }
        if let Some((yaw, pitch)) = rot {
            e.yaw = yaw;
            e.pitch = pitch;
        }
        self.send_entity_move(eid, on_ground, false);
    }

    fn send_entity_move(&mut self, eid: i32, on_ground: bool, teleport: bool) {
        let Some(e) = self
            .world
            .as_ref()
            .and_then(|w| w.entities.get(&eid))
            .cloned()
        else {
            return;
        };
        if e.player {
            let eye = [e.pos[0], e.pos[1] + EYE, e.pos[2]];
            let mode = if teleport {
                move_mode::TELEPORT
            } else {
                move_mode::NORMAL
            };
            let body = bp::move_player(
                rid(eid),
                f3(eye),
                e.pitch,
                e.yaw,
                e.head_yaw,
                mode,
                on_ground,
                0,
            );
            self.send(id::MOVE_PLAYER, &body);
        } else {
            let body = bp::move_actor_absolute(
                rid(eid),
                on_ground,
                teleport,
                f3(e.pos),
                [e.pitch, e.yaw, e.head_yaw],
            );
            self.send(id::MOVE_ACTOR_ABSOLUTE, &body);
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.uuid != 0
            && let Ok(mut skins) = self.shared.skins.lock()
        {
            skins.remove(&self.uuid);
        }
    }
}

/// Runs one peer until it disconnects. `datagrams` carries its UDP payloads.
pub(crate) async fn run(
    shared: Arc<Shared>,
    socket: Arc<UdpSocket>,
    peer: SocketAddr,
    mtu: u16,
    mut datagrams: mpsc::Receiver<Vec<u8>>,
) {
    let mut s = Session::new(shared, peer, mtu, Instant::now());
    let mut java: Option<mpsc::Receiver<Vec<u8>>> = None;
    let mut tick = tokio::time::interval(Duration::from_millis(10));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            d = datagrams.recv() => {
                let Some(d) = d else { break };
                let now = Instant::now();
                s.datagram(&d, now);
                for _ in 0..64 {
                    match datagrams.try_recv() {
                        Ok(d) => s.datagram(&d, now),
                        Err(_) => break,
                    }
                }
            }
            j = async {
                match &mut java {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                match j {
                    Some(body) => {
                        s.java_packet(&body);
                        if let Some(rx) = &mut java {
                            for _ in 0..256 {
                                match rx.try_recv() {
                                    Ok(body) => s.java_packet(&body),
                                    Err(_) => break,
                                }
                            }
                        }
                    }
                    None => {
                        java = None;
                        s.java_closed();
                    }
                }
            }
            _ = tick.tick() => s.tick(Instant::now()),
        }
        if let Some(rx) = s.java_rx.take() {
            java = Some(rx);
        }
        s.flush(&socket, Instant::now());
        if s.done {
            break;
        }
    }
}
