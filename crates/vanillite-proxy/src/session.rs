//! A player's session after login: relays packet bodies between the client and its backend,
//! handles the proxy's commands, switches servers through Vanilla's reconfiguration and falls
//! back to the next server when a backend is lost.
//!
//! Only the few packets the proxy acts on are decoded; everything else is passed on as the
//! frame decoder returned it, through reused buffers.

use std::collections::VecDeque;
use std::net::IpAddr;
use std::sync::Arc;

use tokio::task::JoinHandle;
use tokio::time::Instant;
use vanillite_protocol::codec::{Decode, Reader, write_varint};
use vanillite_protocol::commands::{NodeKind, Parser, ParserProps, StringKind};
use vanillite_protocol::packets::{configuration as cfg, play};
use vanillite_protocol::text::{Color, Component, NamedColor};
use vanillite_protocol::types::GameProfile;
use vanillite_protocol::{Packet, State};

use crate::Shared;
use crate::backend::{self, Backend};
use crate::client::{End, Registered, kick, reason};
use crate::link::{Rx, Tx};

// Serverbound ids the proxy looks at.
const C_CFG_FINISH: i32 = cfg::serverbound::FinishConfiguration::ID;
const C_CFG_INFO: i32 = cfg::serverbound::ClientInformation::ID;
const C_CFG_PAYLOAD: i32 = cfg::serverbound::CustomPayload::ID;
const C_PLAY_COMMAND: i32 = play::serverbound::ChatCommand::ID;
const C_PLAY_ACK: i32 = play::serverbound::ConfigurationAcknowledged::ID;
const C_PLAY_INFO: i32 = play::serverbound::ClientInformation::ID;
const C_PLAY_PAYLOAD: i32 = play::serverbound::CustomPayload::ID;
// Clientbound ids the proxy looks at.
const B_CFG_FINISH: i32 = cfg::clientbound::FinishConfiguration::ID;
const B_CFG_DISCONNECT: i32 = cfg::clientbound::Disconnect::ID;
const B_PLAY_START_CFG: i32 = play::clientbound::StartConfiguration::ID;
const B_PLAY_DISCONNECT: i32 = play::clientbound::Disconnect::ID;
const B_PLAY_COMMANDS: i32 = play::clientbound::Commands::ID;

/// Where the client is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Configuration with the current backend (first join, after a switch).
    Config,
    /// Play with the current backend.
    Play,
    /// The backend asked the client to reconfigure; waiting for the client's acknowledgement.
    BackendReconfig,
    /// The proxy sent `start_configuration` for a switch; waiting for the acknowledgement.
    AwaitAck,
    /// In play without a backend (the server was lost); a fallback is connecting.
    Limbo,
}

/// Why a backend connection is being made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// While the client is in configuration (join, or loss during configuration).
    Join,
    /// `/server` from play.
    Switch,
    /// After losing the server in play.
    Fallback,
}

struct Pending {
    name: Arc<str>,
    kind: Kind,
    task: JoinHandle<Result<Backend, String>>,
}

struct Session {
    shared: Arc<Shared>,
    player: Arc<backend::Player>,
    client_rx: Rx,
    client_tx: Tx,
    backend: Option<Backend>,
    /// The backend is in its configuration state.
    backend_config: bool,
    phase: Phase,
    next: Option<Backend>,
    pending: Option<Pending>,
    /// Servers still to try for a join or fallback.
    candidates: VecDeque<Arc<str>>,
    /// Last client information body (without id), replayed to new backends.
    client_info: Vec<u8>,
    /// Last brand payload body (without id), replayed to new backends.
    brand: Vec<u8>,
    /// Copy of an intercepted packet.
    scratch: Vec<u8>,
    /// Last reason a server connection failed, for the final kick.
    last_error: String,
    last_client_read: Instant,
    last_backend_read: Instant,
    may_switch: bool,
    may_list: bool,
}

/// Runs the session to its end.
pub(crate) async fn run(
    shared: Arc<Shared>,
    client_rx: Rx,
    client_tx: Tx,
    profile: GameProfile,
    address: IpAddr,
    host: String,
    registered: Registered,
) {
    let uuid = profile.id.to_string();
    let config = &shared.config;
    let may_switch = config.permitted("server", &profile.name, &uuid);
    let may_list = config.permitted("glist", &profile.name, &uuid);
    let mut candidates: VecDeque<Arc<str>> = VecDeque::new();
    let forced = config.forced_hosts.get(&host).into_iter().flatten();
    for name in forced.chain(config.try_order.iter()) {
        if !candidates.iter().any(|c| **c == **name) {
            candidates.push_back(Arc::from(name.as_str()));
        }
    }
    let name = profile.name.clone();
    let player = Arc::new(backend::Player {
        profile,
        address,
        secret: config.secret.clone(),
        compression_level: config.compression_level,
    });
    let now = Instant::now();
    let mut session = Session {
        shared: Arc::clone(&shared),
        player,
        client_rx,
        client_tx,
        backend: None,
        backend_config: false,
        phase: Phase::Config,
        next: None,
        pending: None,
        candidates,
        client_info: Vec::new(),
        brand: Vec::new(),
        scratch: Vec::new(),
        last_error: "no server is configured".to_owned(),
        last_client_read: now,
        last_backend_read: now,
        may_switch,
        may_list,
    };
    let end = session.drive().await.err().unwrap_or(End::Closed);
    if let Some(p) = session.pending.take() {
        p.task.abort();
    }
    if let Some(reason) = reason(end) {
        log::info!("{name} disconnected: {}", reason.plain());
        let sent = match session.phase {
            Phase::Config => session
                .client_tx
                .send(&cfg::clientbound::Disconnect { reason }),
            _ => session
                .client_tx
                .send(&play::clientbound::Disconnect { reason }),
        };
        if sent.is_ok() {
            let _ = session.client_tx.flush(config.limits.read_timeout).await;
        }
    } else {
        log::info!("{name} disconnected");
    }
    drop(registered);
}

/// The packet id at the start of a body.
fn packet_id(body: &[u8]) -> vanillite_protocol::Result<i32> {
    Reader::new(body).varint()
}

/// The body after the packet id.
fn after_id(body: &[u8]) -> &[u8] {
    let mut r = Reader::new(body);
    match r.varint() {
        Ok(_) => r.take_rest(),
        Err(_) => &[],
    }
}

async fn read_backend(backend: Option<&mut Backend>) -> bool {
    match backend {
        Some(b) => b.rx.fill().await,
        None => std::future::pending().await,
    }
}

async fn join_pending(pending: Option<&mut Pending>) -> Result<Backend, String> {
    match pending {
        Some(p) => (&mut p.task)
            .await
            .unwrap_or_else(|e| Err(format!("connection task failed: {e}"))),
        None => std::future::pending().await,
    }
}

impl Session {
    async fn drive(&mut self) -> Result<(), End> {
        if !self.connect_next(Kind::Join) {
            return Err(kick(self.last_error.clone()));
        }
        self.client_frames()?;
        loop {
            let timeout = self.shared.config.limits.read_timeout;
            let client_deadline = self.last_client_read.checked_add(timeout);
            let backend_deadline = self
                .backend
                .as_ref()
                .and_then(|_| self.last_backend_read.checked_add(timeout));
            let wake = [client_deadline, backend_deadline]
                .into_iter()
                .flatten()
                .min()
                .unwrap_or_else(Instant::now);
            tokio::select! {
                ok = self.client_rx.fill() => {
                    if !ok {
                        return Err(End::Closed);
                    }
                    self.last_client_read = Instant::now();
                    self.client_frames()?;
                }
                ok = read_backend(self.backend.as_mut()) => {
                    self.last_backend_read = Instant::now();
                    if ok {
                        self.backend_frames()?;
                    } else {
                        self.backend_lost(None)?;
                    }
                }
                result = join_pending(self.pending.as_mut()) => {
                    if let Some(p) = self.pending.take() {
                        self.connected(p.name, p.kind, result)?;
                    }
                }
                () = tokio::time::sleep_until(wake) => {
                    let now = Instant::now();
                    if client_deadline.is_some_and(|d| now >= d) {
                        return Err(End::Timeout);
                    }
                    self.backend_lost(Some("timed out".to_owned()))?;
                }
            }
            self.flush().await?;
        }
    }

    async fn flush(&mut self) -> Result<(), End> {
        let timeout = self.shared.config.limits.read_timeout;
        if let Some(b) = self.backend.as_mut()
            && b.tx.flush(timeout).await.is_err()
        {
            self.backend_lost(Some("connection lost".to_owned()))?;
        }
        self.client_tx.flush(timeout).await.map_err(|_| End::Closed)
    }

    // ---- client to backend ----

    fn client_frames(&mut self) -> Result<(), End> {
        loop {
            let Some(frame) = self.client_rx.decoder.next_frame()? else {
                return Ok(());
            };
            let id = packet_id(frame)?;
            let intercept = match self.phase {
                Phase::Config => matches!(id, C_CFG_FINISH | C_CFG_INFO | C_CFG_PAYLOAD),
                _ => matches!(
                    id,
                    C_PLAY_COMMAND | C_PLAY_ACK | C_PLAY_INFO | C_PLAY_PAYLOAD
                ),
            };
            if !intercept {
                let relay = matches!(
                    self.phase,
                    Phase::Config | Phase::Play | Phase::BackendReconfig
                );
                if relay && let Some(b) = self.backend.as_mut() {
                    b.tx.body(frame)?;
                }
                continue;
            }
            self.scratch.clear();
            self.scratch.extend_from_slice(frame);
            self.client_packet(id)?;
        }
    }

    fn forward(&mut self) -> Result<(), End> {
        if let Some(b) = self.backend.as_mut() {
            b.tx.body(&self.scratch)?;
        }
        Ok(())
    }

    fn client_packet(&mut self, id: i32) -> Result<(), End> {
        match (self.phase, id) {
            (Phase::Config, C_CFG_FINISH) => {
                self.forward()?;
                self.phase = Phase::Play;
                self.client_rx.set_state(State::Play);
            }
            (Phase::Config, C_CFG_INFO) => self.store_info()?,
            (Phase::Config, _) => self.store_brand()?,
            (_, C_PLAY_INFO) => self.store_info()?,
            (_, C_PLAY_PAYLOAD) => self.store_brand()?,
            (Phase::AwaitAck, C_PLAY_ACK) => {
                self.phase = Phase::Config;
                self.client_rx.set_state(State::Configuration);
                if let Some(b) = self.next.take() {
                    self.attach(b)?;
                }
            }
            (Phase::BackendReconfig, C_PLAY_ACK) => {
                self.forward()?;
                self.phase = Phase::Config;
                self.client_rx.set_state(State::Configuration);
            }
            (Phase::Play, C_PLAY_COMMAND) => {
                let mut r = Reader::new(&self.scratch);
                let _ = r.varint()?;
                let command = play::serverbound::ChatCommand::decode(&mut r)?;
                if !self.command(&command.command)? {
                    self.forward()?;
                }
            }
            _ => self.relay_if_connected()?,
        }
        Ok(())
    }

    fn store_info(&mut self) -> Result<(), End> {
        self.client_info.clear();
        self.client_info.extend_from_slice(after_id(&self.scratch));
        self.relay_if_connected()
    }

    /// A custom payload: remembers the client's brand for later backends.
    fn store_brand(&mut self) -> Result<(), End> {
        let mut r = Reader::new(after_id(&self.scratch));
        if vanillite_protocol::types::Identifier::decode(&mut r)
            .is_ok_and(|c| c.as_str() == "minecraft:brand")
        {
            self.brand.clear();
            self.brand.extend_from_slice(after_id(&self.scratch));
        }
        self.relay_if_connected()
    }

    fn relay_if_connected(&mut self) -> Result<(), End> {
        if matches!(
            self.phase,
            Phase::Config | Phase::Play | Phase::BackendReconfig
        ) {
            self.forward()?;
        }
        Ok(())
    }

    // ---- backend to client ----

    fn backend_frames(&mut self) -> Result<(), End> {
        loop {
            let Some(b) = self.backend.as_mut() else {
                return Ok(());
            };
            let frame = match b.rx.decoder.next_frame() {
                Ok(Some(f)) => f,
                Ok(None) => return Ok(()),
                Err(e) => return self.backend_lost(Some(format!("protocol error: {e}"))),
            };
            let Ok(id) = packet_id(frame) else {
                return self.backend_lost(Some("protocol error".to_owned()));
            };
            let intercept = if self.backend_config {
                matches!(id, B_CFG_FINISH | B_CFG_DISCONNECT)
            } else {
                matches!(id, B_PLAY_START_CFG | B_PLAY_DISCONNECT | B_PLAY_COMMANDS)
            };
            if !intercept {
                self.client_tx.body(frame)?;
                continue;
            }
            self.scratch.clear();
            self.scratch.extend_from_slice(frame);
            self.backend_packet(id)?;
        }
    }

    fn backend_packet(&mut self, id: i32) -> Result<(), End> {
        let mut r = Reader::new(after_id(&self.scratch));
        match (self.backend_config, id) {
            (true, B_CFG_FINISH) => {
                self.client_tx.body(&self.scratch)?;
                self.backend_config = false;
            }
            (true, _) => {
                let reason = cfg::clientbound::Disconnect::decode(&mut r)
                    .map_or_else(|_| "disconnected".to_owned(), |d| d.reason.plain());
                self.backend_lost(Some(reason))?;
            }
            (false, B_PLAY_START_CFG) => {
                self.client_tx.body(&self.scratch)?;
                self.backend_config = true;
                self.phase = Phase::BackendReconfig;
            }
            (false, B_PLAY_DISCONNECT) => {
                let reason = play::clientbound::Disconnect::decode(&mut r)
                    .map_or_else(|_| "disconnected".to_owned(), |d| d.reason.plain());
                self.backend_lost(Some(reason))?;
            }
            (false, _) => match play::clientbound::Commands::decode(&mut r) {
                Ok(mut commands) => {
                    self.inject_commands(&mut commands);
                    self.client_tx.send(&commands)?;
                }
                Err(_) => self.client_tx.body(&self.scratch)?,
            },
        }
        Ok(())
    }

    /// Adds the proxy's commands the player may use to the backend's command tree, so the
    /// client completes them instead of marking them as unknown.
    fn inject_commands(&self, commands: &mut play::clientbound::Commands) {
        let tree = &mut commands.0;
        let Ok(root) = usize::try_from(tree.root) else {
            return;
        };
        let add = |tree: &mut vanillite_protocol::commands::CommandTree,
                   literal: &str,
                   argument: Option<&str>| {
            let Some(root_node) = tree.nodes.get(root) else {
                return;
            };
            let exists = root_node.children.iter().any(|&c| {
                usize::try_from(c)
                    .ok()
                    .and_then(|c| tree.nodes.get(c))
                    .is_some_and(|n| matches!(&n.kind, NodeKind::Literal(l) if l == literal))
            });
            let Ok(index) = i32::try_from(tree.nodes.len()) else {
                return;
            };
            if exists {
                return;
            }
            let mut children = Vec::new();
            if argument.is_some() {
                children.push(index.saturating_add(1));
            }
            tree.nodes.push(vanillite_protocol::commands::Node {
                kind: NodeKind::Literal(literal.to_owned()),
                executable: true,
                restricted: false,
                children,
                redirect: None,
            });
            if let Some(name) = argument {
                tree.nodes.push(vanillite_protocol::commands::Node {
                    kind: NodeKind::Argument {
                        name: name.to_owned(),
                        parser: Parser {
                            name: "brigadier:string",
                            props: ParserProps::String(StringKind::SingleWord),
                        },
                        suggestions: None,
                    },
                    executable: true,
                    restricted: false,
                    children: Vec::new(),
                    redirect: None,
                });
            }
            if let Some(r) = tree.nodes.get_mut(root) {
                r.children.push(index);
            }
        };
        if self.may_switch {
            add(tree, "server", Some("server"));
        }
        if self.may_list {
            add(tree, "glist", None);
        }
    }

    // ---- commands ----

    fn message(&mut self, text: String, color: NamedColor) -> Result<(), End> {
        if matches!(self.phase, Phase::Play | Phase::Limbo) {
            self.client_tx.send(&play::clientbound::SystemChat {
                content: Component::text(text).color(Color::Named(color)),
                overlay: false,
            })?;
        }
        Ok(())
    }

    /// Runs a proxy command; `false` passes it on to the backend.
    fn command(&mut self, line: &str) -> Result<bool, End> {
        let mut words = line.split_whitespace();
        match words.next() {
            Some("server") if self.may_switch => {
                match words.next() {
                    None => {
                        let current = self
                            .backend
                            .as_ref()
                            .map_or_else(|| "none".to_owned(), |b| b.name.to_string());
                        let list: Vec<&str> = self
                            .shared
                            .config
                            .servers
                            .keys()
                            .map(String::as_str)
                            .collect();
                        self.message(
                            format!(
                                "You are on {current}. Servers: {}. Switch with /server <name>.",
                                list.join(", ")
                            ),
                            NamedColor::Yellow,
                        )?;
                    }
                    Some(target) => self.switch(target)?,
                }
                Ok(true)
            }
            Some("glist") if self.may_list => {
                let lines = self.player_list();
                self.message(lines, NamedColor::Yellow)?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn player_list(&self) -> String {
        let Ok(players) = self.shared.players.lock() else {
            return String::new();
        };
        let mut out = String::new();
        for server in self.shared.config.servers.keys() {
            let mut names: Vec<&str> = players
                .values()
                .filter(|p| p.server.as_deref() == Some(server.as_str()))
                .map(|p| p.name.as_str())
                .collect();
            names.sort_unstable();
            out.push_str(&format!(
                "[{server}] ({}): {}\n",
                names.len(),
                names.join(", ")
            ));
        }
        out.push_str(&format!("{} players are online.", players.len()));
        out
    }

    fn switch(&mut self, target: &str) -> Result<(), End> {
        let Some((name, _)) = self.shared.config.servers.get_key_value(target) else {
            return self.message(
                format!("There is no server named {target}."),
                NamedColor::Red,
            );
        };
        if self.backend.as_ref().is_some_and(|b| *b.name == **name) {
            return self.message(
                format!("You are already connected to {name}."),
                NamedColor::Red,
            );
        }
        if self.pending.is_some() || self.phase != Phase::Play {
            return self.message(
                "You are already connecting to a server.".to_owned(),
                NamedColor::Red,
            );
        }
        let name: Arc<str> = Arc::from(name.as_str());
        self.message(format!("Connecting to {name}..."), NamedColor::Gray)?;
        self.start(name, Kind::Switch);
        Ok(())
    }

    // ---- backend connections ----

    fn start(&mut self, name: Arc<str>, kind: Kind) {
        let Some(address) = self.shared.config.servers.get(&*name).cloned() else {
            return;
        };
        let task = tokio::spawn(backend::connect(
            Arc::clone(&name),
            address,
            Arc::clone(&self.player),
            self.shared.config.limits.connect_timeout,
        ));
        self.pending = Some(Pending { name, kind, task });
    }

    /// Starts connecting to the next candidate; `false` if none is left.
    fn connect_next(&mut self, kind: Kind) -> bool {
        match self.candidates.pop_front() {
            Some(name) => {
                self.start(name, kind);
                true
            }
            None => false,
        }
    }

    fn connected(
        &mut self,
        name: Arc<str>,
        kind: Kind,
        result: Result<Backend, String>,
    ) -> Result<(), End> {
        match result {
            Ok(backend) => {
                log::info!("{} connected to {name}", self.player.profile.name);
                match (kind, self.phase) {
                    (Kind::Join, Phase::Config) => self.attach(backend)?,
                    (Kind::Switch | Kind::Fallback, Phase::Play | Phase::Limbo) => {
                        // The old server sees the player leave now; its packets stop here, so
                        // nothing of it reaches the client in configuration.
                        self.backend = None;
                        self.shared.set_server(self.player.profile.id, None);
                        self.client_tx
                            .send(&play::clientbound::StartConfiguration)?;
                        self.next = Some(backend);
                        self.phase = Phase::AwaitAck;
                    }
                    _ => {}
                }
                Ok(())
            }
            Err(e) => {
                log::info!(
                    "{} could not connect to {name}: {e}",
                    self.player.profile.name
                );
                self.last_error = format!("Could not connect to {name}: {e}");
                match kind {
                    Kind::Switch if self.phase != Phase::Limbo => {
                        self.message(self.last_error.clone(), NamedColor::Red)
                    }
                    Kind::Switch | Kind::Join | Kind::Fallback => {
                        if self.connect_next(kind) {
                            Ok(())
                        } else {
                            Err(kick(self.last_error.clone()))
                        }
                    }
                }
            }
        }
    }

    /// Makes `backend` the current server while the client is in configuration, replaying
    /// what the client told earlier servers.
    fn attach(&mut self, mut backend: Backend) -> Result<(), End> {
        if !self.client_info.is_empty() {
            self.scratch.clear();
            write_varint(&mut self.scratch, C_CFG_INFO);
            self.scratch.extend_from_slice(&self.client_info);
            backend.tx.body(&self.scratch)?;
        }
        if !self.brand.is_empty() {
            self.scratch.clear();
            write_varint(&mut self.scratch, C_CFG_PAYLOAD);
            self.scratch.extend_from_slice(&self.brand);
            backend.tx.body(&self.scratch)?;
        }
        self.shared
            .set_server(self.player.profile.id, Some(Arc::clone(&backend.name)));
        self.backend = Some(backend);
        self.backend_config = true;
        self.last_backend_read = Instant::now();
        // Packets that arrived together with the end of the login.
        self.backend_frames()
    }

    /// The current backend is gone: fall back to another server or disconnect.
    fn backend_lost(&mut self, why: Option<String>) -> Result<(), End> {
        let Some(lost) = self.backend.take() else {
            return Ok(());
        };
        self.shared.set_server(self.player.profile.id, None);
        let why = why.unwrap_or_else(|| "the server closed the connection".to_owned());
        log::info!(
            "{} lost the connection to {}: {why}",
            self.player.profile.name,
            lost.name
        );
        self.last_error = format!("Disconnected from {}: {why}", lost.name);
        self.candidates = self
            .shared
            .config
            .try_order
            .iter()
            .filter(|n| n.as_str() != &*lost.name)
            .map(|n| Arc::from(n.as_str()))
            .collect();
        if self.pending.is_some() {
            // A switch is already connecting; it takes the player.
            if self.phase == Phase::Play {
                self.phase = Phase::Limbo;
                return self.message(self.last_error.clone(), NamedColor::Red);
            }
            return Err(kick(self.last_error.clone()));
        }
        match self.phase {
            Phase::Config => {
                if self.connect_next(Kind::Join) {
                    Ok(())
                } else {
                    Err(kick(self.last_error.clone()))
                }
            }
            Phase::Play => {
                if self.candidates.is_empty() {
                    return Err(kick(self.last_error.clone()));
                }
                self.phase = Phase::Limbo;
                self.message(self.last_error.clone(), NamedColor::Red)?;
                self.connect_next(Kind::Fallback);
                Ok(())
            }
            Phase::BackendReconfig | Phase::AwaitAck | Phase::Limbo => {
                Err(kick(self.last_error.clone()))
            }
        }
    }
}
