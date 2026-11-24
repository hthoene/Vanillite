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
use vanillite_protocol::frame::RawFrame;
use vanillite_protocol::ids;
use vanillite_protocol::packets::{configuration as cfg, play};
use vanillite_protocol::text::{ClickEvent, Color, Component, HoverEvent, NamedColor};
use vanillite_protocol::types::{GameProfile, Identifier, RawBytes};
use vanillite_protocol::{Packet, State};

use crate::Shared;
use crate::backend::{self, Backend};
use crate::client::{End, Registered, kick, reason};
use crate::link::{Rx, Tx};
use crate::zlib;

// Serverbound ids the proxy looks at.
const C_CFG_FINISH: i32 = cfg::serverbound::FinishConfiguration::ID;
const C_CFG_INFO: i32 = cfg::serverbound::ClientInformation::ID;
const C_CFG_PAYLOAD: i32 = cfg::serverbound::CustomPayload::ID;
const C_PLAY_COMMAND: i32 = play::serverbound::ChatCommand::ID;
const C_PLAY_ACK: i32 = play::serverbound::ConfigurationAcknowledged::ID;
const C_PLAY_INFO: i32 = play::serverbound::ClientInformation::ID;
const C_PLAY_PAYLOAD: i32 = play::serverbound::CustomPayload::ID;
const C_PLAY_SUGGEST: i32 = play::serverbound::CommandSuggestion::ID;
// Clientbound ids the proxy looks at.
const B_CFG_FINISH: i32 = cfg::clientbound::FinishConfiguration::ID;
const B_CFG_DISCONNECT: i32 = cfg::clientbound::Disconnect::ID;
const B_PLAY_START_CFG: i32 = play::clientbound::StartConfiguration::ID;
const B_PLAY_DISCONNECT: i32 = play::clientbound::Disconnect::ID;
const B_PLAY_COMMANDS: i32 = play::clientbound::Commands::ID;
const B_CFG_PACK_PUSH: i32 = ids::configuration::clientbound::RESOURCE_PACK_PUSH;
const B_CFG_PACK_POP: i32 = ids::configuration::clientbound::RESOURCE_PACK_POP;
const B_PLAY_PACK_PUSH: i32 = ids::play::clientbound::RESOURCE_PACK_PUSH;
const B_PLAY_PACK_POP: i32 = ids::play::clientbound::RESOURCE_PACK_POP;

/// Resource packs of one server the proxy keeps track of.
const MAX_PACKS: usize = 64;
/// Bytes of channel registrations kept for later servers.
const MAX_CHANNELS: usize = 16 * 1024;

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
    /// Plugin channels the client registered (`minecraft:register` data, NUL-separated),
    /// replayed to new backends.
    channels: Vec<u8>,
    /// Resource packs the current server pushed.
    packs: Vec<u128>,
    /// Packs of earlier servers, removed when the new server ends its configuration unless it
    /// pushed them again.
    old_packs: Vec<u128>,
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
        // The threshold the client link really uses: virtual connections never compress.
        link_threshold: config.trusted_link.then(|| {
            client_tx
                .encoder
                .threshold()
                .and_then(|t| i32::try_from(t).ok())
                .unwrap_or(-1)
        }),
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
        channels: Vec::new(),
        packs: Vec::new(),
        old_packs: Vec::new(),
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

/// Whether the proxy acts on a clientbound packet (all others pass unchanged).
fn backend_intercepts(config: bool, id: i32) -> bool {
    if config {
        matches!(
            id,
            B_CFG_FINISH | B_CFG_DISCONNECT | B_CFG_PACK_PUSH | B_CFG_PACK_POP
        )
    } else {
        matches!(
            id,
            B_PLAY_START_CFG
                | B_PLAY_DISCONNECT
                | B_PLAY_COMMANDS
                | B_PLAY_PACK_PUSH
                | B_PLAY_PACK_POP
        )
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
            let Some(frame) = self.client_rx.next_frame()? else {
                return Ok(());
            };
            let id = packet_id(frame)?;
            let intercept = match self.phase {
                Phase::Config => matches!(id, C_CFG_FINISH | C_CFG_INFO | C_CFG_PAYLOAD),
                _ => matches!(
                    id,
                    C_PLAY_COMMAND | C_PLAY_ACK | C_PLAY_INFO | C_PLAY_PAYLOAD | C_PLAY_SUGGEST
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
            (Phase::Play, C_PLAY_SUGGEST) => {
                let mut r = Reader::new(&self.scratch);
                let _ = r.varint()?;
                let request = play::serverbound::CommandSuggestion::decode(&mut r)?;
                if !self.suggest(&request)? {
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

    /// A custom payload: remembers the client's brand and channel registrations for later
    /// backends.
    fn store_brand(&mut self) -> Result<(), End> {
        let mut r = Reader::new(after_id(&self.scratch));
        match Identifier::decode(&mut r) {
            Ok(c) if c.as_str() == "minecraft:brand" => {
                self.brand.clear();
                self.brand.extend_from_slice(after_id(&self.scratch));
            }
            Ok(c) if c.as_str() == "minecraft:register" => {
                let data = r.take_rest();
                if self.channels.len().saturating_add(data.len()) < MAX_CHANNELS {
                    if !self.channels.is_empty() {
                        self.channels.push(0);
                    }
                    self.channels.extend_from_slice(data);
                }
            }
            _ => {}
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
            let next = if b.passthrough {
                b.rx.decoder.next_raw()
            } else {
                b.rx.decoder.next_frame().map(|f| f.map(RawFrame::Packet))
            };
            let frame = match next {
                Ok(Some(f)) => f,
                Ok(None) => return Ok(()),
                Err(e) => return self.backend_lost(Some(format!("protocol error: {e}"))),
            };
            match frame {
                RawFrame::Packet(body) => {
                    let Ok(id) = packet_id(body) else {
                        return self.backend_lost(Some("protocol error".to_owned()));
                    };
                    if !backend_intercepts(self.backend_config, id) {
                        self.client_tx.body(body)?;
                        continue;
                    }
                    self.scratch.clear();
                    self.scratch.extend_from_slice(body);
                    self.backend_packet(id)?;
                }
                RawFrame::Compressed {
                    frame,
                    zlib: data,
                    data_len,
                } => {
                    // Trusted link: compressed for the client already; only the id is looked at.
                    let Some(id) = zlib::peek_id(data) else {
                        return self.backend_lost(Some("protocol error".to_owned()));
                    };
                    if !backend_intercepts(self.backend_config, id) {
                        self.client_tx.raw(frame)?;
                        continue;
                    }
                    if !zlib::inflate(data, data_len, &mut self.scratch) {
                        return self.backend_lost(Some("bad compressed data".to_owned()));
                    }
                    self.backend_packet(id)?;
                }
            }
        }
    }

    fn backend_packet(&mut self, id: i32) -> Result<(), End> {
        let mut r = Reader::new(after_id(&self.scratch));
        match (self.backend_config, id) {
            (true, B_CFG_FINISH) => {
                self.drop_old_packs()?;
                self.client_tx.body(&self.scratch)?;
                self.backend_config = false;
            }
            (true, B_CFG_PACK_PUSH) | (false, B_PLAY_PACK_PUSH) => {
                if let Ok(id) = r.u128()
                    && !self.packs.contains(&id)
                    && self.packs.len() < MAX_PACKS
                {
                    self.packs.push(id);
                }
                self.client_tx.body(&self.scratch)?;
            }
            (true, B_CFG_PACK_POP) | (false, B_PLAY_PACK_POP) => {
                match r.u8() {
                    Ok(0) => self.packs.clear(),
                    _ => {
                        if let Ok(id) = r.u128() {
                            self.packs.retain(|p| *p != id);
                        }
                    }
                }
                self.client_tx.body(&self.scratch)?;
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
            (false, B_PLAY_COMMANDS) => match play::clientbound::Commands::decode(&mut r) {
                Ok(mut commands) => {
                    self.inject_commands(&mut commands);
                    self.client_tx.send(&commands)?;
                }
                Err(_) => self.client_tx.body(&self.scratch)?,
            },
            (false, _) => self.client_tx.body(&self.scratch)?,
        }
        Ok(())
    }

    /// Removes the packs of earlier servers that the new server did not push again; sent while
    /// the client is still in configuration.
    fn drop_old_packs(&mut self) -> Result<(), End> {
        let mut out = Vec::new();
        for id in std::mem::take(&mut self.old_packs) {
            if self.packs.contains(&id) {
                continue;
            }
            out.clear();
            write_varint(&mut out, B_CFG_PACK_POP);
            out.push(1);
            out.extend_from_slice(&id.to_be_bytes());
            self.client_tx.body(&out)?;
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
                        suggestions: Identifier::new("minecraft:ask_server").ok(),
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
        self.chat(Component::text(text).color(Color::Named(color)))
    }

    fn chat(&mut self, content: Component) -> Result<(), End> {
        if matches!(self.phase, Phase::Play | Phase::Limbo) {
            self.client_tx.send(&play::clientbound::SystemChat {
                content,
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
                        let list = self.server_list();
                        self.chat(list)?;
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

    /// `/server` without a name: the current server and every server to click on, with its
    /// player count, like Velocity.
    fn server_list(&self) -> Component {
        let current = self.backend.as_ref().map(|b| Arc::clone(&b.name));
        let mut counts = vec![0usize; self.shared.config.servers.len()];
        if let Ok(players) = self.shared.players.lock() {
            for p in players.values() {
                let Some(server) = p.server.as_deref() else {
                    continue;
                };
                let index = self.shared.config.servers.keys().position(|s| s == server);
                if let Some(c) = index.and_then(|i| counts.get_mut(i)) {
                    *c = c.saturating_add(1);
                }
            }
        }
        let yellow = Color::Named(NamedColor::Yellow);
        let mut out = Component::text(match &current {
            Some(name) => format!("You are currently connected to {name}.\n"),
            None => "You are not connected to a server.\n".to_owned(),
        })
        .color(yellow)
        .append(Component::text("Available servers: ").color(yellow));
        for (i, (name, count)) in self.shared.config.servers.keys().zip(counts).enumerate() {
            if i > 0 {
                out = out.append(Component::text(", ").color(Color::Named(NamedColor::Gray)));
            }
            let here = current.as_deref() == Some(name.as_str());
            let players = if count == 1 {
                "1 player online".to_owned()
            } else {
                format!("{count} players online")
            };
            let (color, hover) = if here {
                (NamedColor::Green, format!("Currently connected\n{players}"))
            } else {
                (NamedColor::Gray, format!("Click to connect\n{players}"))
            };
            out = out.append(
                Component::text(name.clone())
                    .color(Color::Named(color))
                    .click(ClickEvent::RunCommand(format!("/server {name}")))
                    .hover(HoverEvent::ShowText(Component::text(hover))),
            );
        }
        out
    }

    /// Tab completion of `/server <name>`; `false` passes the request on to the backend.
    fn suggest(&mut self, request: &play::serverbound::CommandSuggestion) -> Result<bool, End> {
        let line = request
            .command
            .strip_prefix('/')
            .unwrap_or(&request.command);
        let Some(partial) = line.strip_prefix("server ") else {
            return Ok(false);
        };
        if !self.may_switch || partial.contains(' ') {
            return Ok(false);
        }
        let lower = partial.to_ascii_lowercase();
        let suggestions = self
            .shared
            .config
            .servers
            .keys()
            .filter(|n| n.to_ascii_lowercase().starts_with(&lower))
            .map(|n| play::clientbound::Suggestion {
                text: n.clone(),
                tooltip: None,
            })
            .collect();
        let start = request.command.len().saturating_sub(partial.len());
        let prefix = request.command.get(..start).unwrap_or_default();
        let java_len = |s: &str| i32::try_from(s.encode_utf16().count()).unwrap_or(i32::MAX);
        self.client_tx
            .send(&play::clientbound::CommandSuggestions {
                id: request.id,
                start: java_len(prefix),
                length: java_len(partial),
                suggestions,
            })?;
        Ok(true)
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
        if !self.channels.is_empty() {
            backend.tx.send(&cfg::serverbound::CustomPayload {
                channel: Identifier::new("minecraft:register")?,
                data: RawBytes(self.channels.clone()),
            })?;
        }
        // The new server's configuration decides which packs stay.
        let earlier = std::mem::take(&mut self.packs);
        for id in earlier {
            if !self.old_packs.contains(&id) && self.old_packs.len() < MAX_PACKS {
                self.old_packs.push(id);
            }
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
