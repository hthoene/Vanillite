//! A player's session after login: relays packet bodies between the client and its backend,
//! handles the proxy's commands and the BungeeCord plugin channel, switches servers through
//! Vanilla's reconfiguration and falls back to the next server when a backend is lost.
//!
//! Only the few packets the proxy acts on are decoded; everything else is passed on as the
//! frame decoder returned it, through reused buffers.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
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

use crate::backend::{self, Backend, Refusal};
use crate::client::{End, Registered, reason};
use crate::link::{Rx, Tx};
use crate::zlib;
use crate::{Control, Shared, admin, bungee};

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
const B_PLAY_PAYLOAD: i32 = play::clientbound::CustomPayload::ID;
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
    task: JoinHandle<Result<Backend, Refusal>>,
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
    /// Servers for the join and fallbacks: the forced host's, then the try order.
    try_order: Vec<Arc<str>>,
    /// First entry of `try_order` that may still be tried. Like Velocity it only moves
    /// forward, so a player every server refuses is disconnected instead of passed around.
    try_index: usize,
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
    /// Last reason a server refused the player or was lost, for the final kick.
    last_error: Component,
    last_client_read: Instant,
    last_backend_read: Instant,
    /// Read timeout of both sides.
    timeout: Duration,
    /// Orders from elsewhere on the proxy.
    orders: mpsc::Receiver<Control>,
}

/// Runs the session to its end.
#[allow(clippy::too_many_arguments)] // The parts of a finished login.
pub(crate) async fn run(
    shared: Arc<Shared>,
    client_rx: Rx,
    client_tx: Tx,
    profile: GameProfile,
    address: IpAddr,
    host: String,
    registered: Registered,
    orders: mpsc::Receiver<Control>,
) {
    let config = shared.config();
    let mut try_order: Vec<Arc<str>> = Vec::new();
    let forced = config.forced_hosts.get(&host).into_iter().flatten();
    for name in forced.chain(config.try_order.iter()) {
        if !try_order.iter().any(|c| **c == **name) {
            try_order.push(Arc::from(name.as_str()));
        }
    }
    let name = profile.name.clone();
    let player = Arc::new(backend::Player {
        profile,
        address,
        secret: config.secret.clone(),
        forwarding: config.forwarding,
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
        try_order,
        try_index: 0,
        client_info: Vec::new(),
        brand: Vec::new(),
        channels: Vec::new(),
        packs: Vec::new(),
        old_packs: Vec::new(),
        scratch: Vec::new(),
        last_error: Component::text("no server is configured"),
        last_client_read: now,
        last_backend_read: now,
        timeout: config.limits.read_timeout,
        orders,
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
            let _ = session.client_tx.flush(session.timeout).await;
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
                | B_PLAY_PAYLOAD
        )
    }
}

async fn read_backend(backend: Option<&mut Backend>) -> bool {
    match backend {
        Some(b) => b.rx.fill().await,
        None => std::future::pending().await,
    }
}

async fn join_pending(pending: Option<&mut Pending>) -> Result<Backend, Refusal> {
    match pending {
        Some(p) => (&mut p.task)
            .await
            .unwrap_or_else(|e| Err(format!("connection task failed: {e}").into())),
        None => std::future::pending().await,
    }
}

impl Session {
    async fn drive(&mut self) -> Result<(), End> {
        if !self.connect_next(Kind::Join, None) {
            return Err(self.refused());
        }
        self.client_frames()?;
        loop {
            let timeout = self.timeout;
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
                        self.backend_lost("the server closed the connection".into())?;
                    }
                }
                result = join_pending(self.pending.as_mut()) => {
                    if let Some(p) = self.pending.take() {
                        self.connected(p.name, p.kind, result)?;
                    }
                }
                Some(order) = self.orders.recv() => self.order(order)?,
                () = tokio::time::sleep_until(wake) => {
                    let now = Instant::now();
                    if client_deadline.is_some_and(|d| now >= d) {
                        return Err(End::Timeout);
                    }
                    self.backend_lost("timed out".into())?;
                }
            }
            self.flush().await?;
        }
    }

    async fn flush(&mut self) -> Result<(), End> {
        let timeout = self.timeout;
        if let Some(b) = self.backend.as_mut()
            && b.tx.flush(timeout).await.is_err()
        {
            self.backend_lost("connection lost".into())?;
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
            // Answers on this channel come from the proxy only (backends trust them).
            Ok(c) if c.as_str() == bungee::CHANNEL => return Ok(()),
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
                Err(e) => return self.backend_lost(format!("protocol error: {e}").into()),
            };
            match frame {
                RawFrame::Packet(body) => {
                    let Ok(id) = packet_id(body) else {
                        return self.backend_lost("protocol error".into());
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
                        return self.backend_lost("protocol error".into());
                    };
                    if !backend_intercepts(self.backend_config, id) {
                        self.client_tx.raw(frame)?;
                        continue;
                    }
                    if !zlib::inflate(data, data_len, &mut self.scratch) {
                        return self.backend_lost("bad compressed data".into());
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
                let reason = cfg::clientbound::Disconnect::decode(&mut r).map_or_else(
                    |_| "disconnected".into(),
                    |d| Refusal::Kicked(Box::new(d.reason)),
                );
                self.backend_lost(reason)?;
            }
            (false, B_PLAY_START_CFG) => {
                self.client_tx.body(&self.scratch)?;
                self.backend_config = true;
                self.phase = Phase::BackendReconfig;
            }
            (false, B_PLAY_DISCONNECT) => {
                let reason = play::clientbound::Disconnect::decode(&mut r).map_or_else(
                    |_| "disconnected".into(),
                    |d| Refusal::Kicked(Box::new(d.reason)),
                );
                self.backend_lost(reason)?;
            }
            (false, B_PLAY_PAYLOAD) => {
                let mut pr = Reader::new(after_id(&self.scratch));
                match Identifier::decode(&mut pr) {
                    Ok(c)
                        if c.as_str() == bungee::CHANNEL && self.shared.config().bungee_channel =>
                    {
                        let data = pr.take_rest().to_vec();
                        self.bungee(&data)?;
                    }
                    _ => self.client_tx.body(&self.scratch)?,
                }
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
        let config = self.shared.config();
        let name = &self.player.profile.name;
        let uuid = self.player.profile.id.to_string();
        let tree = &mut commands.0;
        let Ok(root) = usize::try_from(tree.root) else {
            return;
        };
        let add = |tree: &mut vanillite_protocol::commands::CommandTree,
                   literal: &str,
                   argument: Option<(&str, StringKind)>| {
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
            if let Some((name, kind)) = argument {
                tree.nodes.push(vanillite_protocol::commands::Node {
                    kind: NodeKind::Argument {
                        name: name.to_owned(),
                        parser: Parser {
                            name: "brigadier:string",
                            props: ParserProps::String(kind),
                        },
                        suggestions: (kind == StringKind::SingleWord)
                            .then(|| Identifier::new("minecraft:ask_server").ok())
                            .flatten(),
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
        let commands = [
            ("server", Some(("server", StringKind::SingleWord))),
            ("glist", Some(("server", StringKind::SingleWord))),
            ("send", Some(("arguments", StringKind::GreedyPhrase))),
            ("alert", Some(("message", StringKind::GreedyPhrase))),
            ("proxy", Some(("action", StringKind::SingleWord))),
        ];
        for (command, argument) in commands {
            if config.permitted(command, name, &uuid) {
                add(tree, command, argument);
            }
        }
    }

    // ---- commands and orders ----

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

    /// Whether the player may use `command`.
    fn may(&self, command: &str) -> bool {
        let profile = &self.player.profile;
        self.shared
            .config()
            .permitted(command, &profile.name, &profile.id.to_string())
    }

    /// Runs a proxy command; `false` passes it on to the backend.
    fn command(&mut self, line: &str) -> Result<bool, End> {
        let mut words = line.split_whitespace();
        match words.next() {
            Some("server") if self.may("server") => {
                match words.next() {
                    None => {
                        let list = self.server_list();
                        self.chat(list)?;
                    }
                    Some(target) => self.switch(target)?,
                }
                Ok(true)
            }
            Some(_) => {
                let player = Arc::clone(&self.player);
                let uuid = player.profile.id.to_string();
                let server = self.backend.as_ref().map(|b| Arc::clone(&b.name));
                let sender = admin::Sender::Player {
                    name: &player.profile.name,
                    uuid: &uuid,
                    server: server.as_deref(),
                };
                match admin::run(&self.shared, &sender, line) {
                    Some(out) => {
                        self.chat(out)?;
                        Ok(true)
                    }
                    None => Ok(false),
                }
            }
            None => Ok(false),
        }
    }

    /// An order from another session, a command or a plugin message.
    fn order(&mut self, order: Control) -> Result<(), End> {
        match order {
            Control::Send(server) => {
                let here = self.backend.as_ref().is_some_and(|b| b.name == server);
                if !here && self.pending.is_none() && self.phase == Phase::Play {
                    self.switch(&server)?;
                }
                Ok(())
            }
            Control::Chat(message) => self.chat(message),
            Control::Kick(reason) => Err(End::Kick(Box::new(reason))),
            Control::Payload(data) => {
                if self.phase == Phase::Play
                    && let Some(b) = self.backend.as_mut()
                {
                    b.tx.send(&play::serverbound::CustomPayload {
                        channel: Identifier::new(bungee::CHANNEL)?,
                        data: RawBytes(data),
                    })?;
                }
                Ok(())
            }
        }
    }

    /// A request of the backend on the BungeeCord channel.
    fn bungee(&mut self, data: &[u8]) -> Result<(), End> {
        let server = self.backend.as_ref().map(|b| Arc::clone(&b.name));
        let me = bungee::Me {
            id: self.player.profile.id,
            server: server.as_deref(),
        };
        match bungee::handle(&self.shared, &me, data) {
            bungee::Reply::Nothing => {}
            bungee::Reply::Answer(answer) => self.order(Control::Payload(answer))?,
            bungee::Reply::Connect(target) => {
                if self.pending.is_none() && self.phase == Phase::Play {
                    self.switch(&target)?;
                }
            }
        }
        Ok(())
    }

    /// `/server` without a name: the current server and every server to click on, with its
    /// player count, like Velocity.
    fn server_list(&self) -> Component {
        let config = self.shared.config();
        let current = self.backend.as_ref().map(|b| Arc::clone(&b.name));
        let mut counts = vec![0usize; config.servers.len()];
        if let Ok(players) = self.shared.players.lock() {
            for p in players.values() {
                let Some(server) = p.server.as_deref() else {
                    continue;
                };
                let index = config.servers.keys().position(|s| s == server);
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
        for (i, (name, count)) in config.servers.keys().zip(counts).enumerate() {
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

    /// Tab completion of the proxy's commands; `false` passes the request on to the backend.
    fn suggest(&mut self, request: &play::serverbound::CommandSuggestion) -> Result<bool, End> {
        let line = request
            .command
            .strip_prefix('/')
            .unwrap_or(&request.command);
        let Some((command, partial)) = line.split_once(' ') else {
            return Ok(false);
        };
        let config = self.shared.config();
        let candidates: Vec<String> = match command {
            "server" | "glist" if !partial.contains(' ') && self.may(command) => {
                config.servers.keys().cloned().collect()
            }
            "proxy" if !partial.contains(' ') && self.may(command) => {
                vec!["info".to_owned(), "reload".to_owned()]
            }
            _ => return Ok(false),
        };
        let lower = partial.to_ascii_lowercase();
        let suggestions = candidates
            .into_iter()
            .filter(|n| n.to_ascii_lowercase().starts_with(&lower))
            .map(|text| play::clientbound::Suggestion {
                text,
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

    fn switch(&mut self, target: &str) -> Result<(), End> {
        let config = self.shared.config();
        let Some((name, _)) = config.servers.get_key_value(target) else {
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
        if !self.start(Arc::clone(&name), Kind::Switch) {
            return self.message(format!("There is no server named {name}."), NamedColor::Red);
        }
        Ok(())
    }

    // ---- backend connections ----

    /// Starts connecting to `name`; `false` if it is no longer configured.
    fn start(&mut self, name: Arc<str>, kind: Kind) -> bool {
        let config = self.shared.config();
        let Some(address) = config.servers.get(&*name).cloned() else {
            return false;
        };
        let task = tokio::spawn(backend::connect(
            Arc::clone(&name),
            address,
            Arc::clone(&self.player),
            config.limits.connect_timeout,
        ));
        self.pending = Some(Pending { name, kind, task });
        true
    }

    /// Starts connecting to the next server of the try order other than `refused` and the
    /// current one (Velocity's `getNextServerToTry`); `false` if none is left.
    fn connect_next(&mut self, kind: Kind, refused: Option<&str>) -> bool {
        while let Some(name) = self.try_order.get(self.try_index).cloned() {
            let current = self.backend.as_ref().is_some_and(|b| b.name == name);
            if !current && refused != Some(&*name) && self.start(name, kind) {
                return true;
            }
            self.try_index = self.try_index.saturating_add(1);
        }
        false
    }

    /// The final kick after the servers refused the player: the last reason.
    fn refused(&self) -> End {
        End::Kick(Box::new(self.last_error.clone()))
    }

    /// Records why `name` refused or lost the player: a kick reason is shown as the server
    /// sent it, a failure says which server failed. Returns the line for the chat.
    fn note_refusal(&mut self, prefix: &str, name: &str, why: Refusal) -> Component {
        let line = Component::text(format!("{prefix} {name}: "))
            .append(why.component())
            .color(Color::Named(NamedColor::Red));
        self.last_error = match why {
            Refusal::Kicked(reason) => *reason,
            Refusal::Failed(_) => line.clone(),
        };
        line
    }

    fn connected(
        &mut self,
        name: Arc<str>,
        kind: Kind,
        result: Result<Backend, Refusal>,
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
            Err(why) => {
                log::info!(
                    "{} could not connect to {name}: {}",
                    self.player.profile.name,
                    why.plain()
                );
                let line = self.note_refusal("Could not connect to", &name, why);
                match kind {
                    Kind::Switch if self.phase != Phase::Limbo => self.chat(line),
                    Kind::Switch | Kind::Join | Kind::Fallback => {
                        if self.connect_next(kind, Some(&name)) {
                            Ok(())
                        } else {
                            Err(self.refused())
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
    fn backend_lost(&mut self, why: Refusal) -> Result<(), End> {
        let Some(lost) = self.backend.take() else {
            return Ok(());
        };
        self.shared.set_server(self.player.profile.id, None);
        log::info!(
            "{} lost the connection to {}: {}",
            self.player.profile.name,
            lost.name,
            why.plain()
        );
        let line = self.note_refusal("Disconnected from", &lost.name, why);
        if self.pending.is_some() {
            // A switch is already connecting; it takes the player.
            if self.phase == Phase::Play {
                self.phase = Phase::Limbo;
                return self.chat(line);
            }
            return Err(self.refused());
        }
        match self.phase {
            Phase::Config => {
                if self.connect_next(Kind::Join, Some(&lost.name)) {
                    Ok(())
                } else {
                    Err(self.refused())
                }
            }
            Phase::Play => {
                if !self.connect_next(Kind::Fallback, Some(&lost.name)) {
                    return Err(self.refused());
                }
                self.phase = Phase::Limbo;
                self.chat(line)
            }
            Phase::BackendReconfig | Phase::AwaitAck | Phase::Limbo => Err(self.refused()),
        }
    }
}
