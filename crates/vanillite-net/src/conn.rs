//! One connection: the state machine of handshake, status, login and configuration, then the
//! play read loop that validates and forwards packets to the world.

use crate::Shared;
use crate::auth::{AuthResult, is_valid_player_name, server_hash};
use crate::limits::{ConnectionGuard, RateLimiter};
use crate::outbound::{Out, Outbound};
use crate::player::{Joined, Latency};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;
use tokio::time::Instant;
use vanillite_protocol::Inbound;
use vanillite_protocol::frame::FrameDecoder;
use vanillite_protocol::packets::common::ClientInfo;
use vanillite_protocol::packets::handshake::Intent;
use vanillite_protocol::packets::{configuration as cfg, login, play, status};
use vanillite_protocol::sets::{
    ServerboundConfiguration, ServerboundHandshake, ServerboundLogin, ServerboundPlay,
    ServerboundStatus,
};
use vanillite_protocol::text::{Component, JsonComponent};
use vanillite_protocol::types::{GameProfile, Identifier, RawBytes, Uuid};
use vanillite_protocol::{Error, State};

/// Why a connection ends.
#[derive(Debug)]
pub(crate) enum End {
    /// Malformed or unexpected input (SEC-05/07).
    Protocol(Error),
    /// The peer closed or the socket failed.
    Closed,
    /// A state deadline or the read timeout passed.
    Timeout,
    /// Disconnect with a message to the client.
    Kick(Box<Component>),
    /// The connection finished normally (status ping answered).
    Done,
}

impl From<Error> for End {
    fn from(e: Error) -> Self {
        Self::Protocol(e)
    }
}

type Result<T> = std::result::Result<T, End>;

enum Source {
    Tcp {
        read: tokio::net::tcp::OwnedReadHalf,
        decoder: Box<FrameDecoder>,
        buf: Box<[u8]>,
    },
    Virtual {
        rx: mpsc::Receiver<Vec<u8>>,
        max: usize,
    },
}

enum Read {
    Bytes,
    Frame(Vec<u8>),
    Eof,
}

impl Source {
    async fn read(&mut self) -> Read {
        match self {
            Self::Tcp { read, decoder, buf } => match read.read(buf).await {
                Ok(0) | Err(_) => Read::Eof,
                Ok(n) => {
                    decoder.push(buf.get(..n).unwrap_or_default());
                    Read::Bytes
                }
            },
            Self::Virtual { rx, .. } => rx.recv().await.map_or(Read::Eof, Read::Frame),
        }
    }

    fn set_state(&mut self, state: State) {
        match self {
            Self::Tcp { decoder, .. } => decoder.set_limits(state.limits()),
            Self::Virtual { max, .. } => *max = state.limits().max_uncompressed,
        }
    }
}

struct KeepAlive {
    enabled: bool,
    next: Instant,
    pending: Option<(i64, Instant)>,
    latency: Latency,
}

pub(crate) struct Conn {
    shared: Arc<Shared>,
    addr: SocketAddr,
    source: Source,
    out: Outbound,
    state: State,
    deadline: Option<Instant>,
    last_read: Instant,
    rate: RateLimiter,
    keepalive: KeepAlive,
    vouched: Option<GameProfile>,
    guard: Option<ConnectionGuard>,
}

impl Conn {
    pub(crate) fn tcp(
        shared: Arc<Shared>,
        addr: SocketAddr,
        read: tokio::net::tcp::OwnedReadHalf,
        out: Outbound,
        guard: ConnectionGuard,
    ) -> Self {
        let source = Source::Tcp {
            read,
            decoder: Box::new(FrameDecoder::new()),
            buf: vec![0; 8192].into_boxed_slice(),
        };
        Self::new(shared, addr, source, out, None, Some(guard))
    }

    pub(crate) fn virtual_(
        shared: Arc<Shared>,
        addr: SocketAddr,
        rx: mpsc::Receiver<Vec<u8>>,
        out: Outbound,
        profile: GameProfile,
    ) -> Self {
        let source = Source::Virtual {
            rx,
            max: State::Handshake.limits().max_uncompressed,
        };
        Self::new(shared, addr, source, out, Some(profile), None)
    }

    fn new(
        shared: Arc<Shared>,
        addr: SocketAddr,
        source: Source,
        out: Outbound,
        vouched: Option<GameProfile>,
        guard: Option<ConnectionGuard>,
    ) -> Self {
        let now = Instant::now();
        let rate = RateLimiter::new(shared.config.rate_limits);
        Self {
            deadline: now.checked_add(shared.config.status_timeout),
            shared,
            addr,
            source,
            out,
            state: State::Handshake,
            last_read: now,
            rate,
            keepalive: KeepAlive {
                enabled: false,
                next: now,
                pending: None,
                latency: Latency::default(),
            },
            vouched,
            guard,
        }
    }

    /// Runs the connection to its end; one log line per abnormal end (SEC-08).
    pub(crate) async fn run(mut self) {
        let end = match self.drive().await {
            Ok(()) => End::Done,
            Err(e) => e,
        };
        match end {
            End::Done | End::Closed => {}
            End::Kick(reason) => {
                let reason = *reason;
                log::debug!(
                    "{}: disconnected in {:?}: {}",
                    self.addr,
                    self.state,
                    reason.plain()
                );
                self.send_disconnect(&reason);
            }
            End::Timeout => {
                log::debug!("{}: timed out in {:?}", self.addr, self.state);
                self.send_disconnect(&Component::translatable("disconnect.timeout", vec![]));
            }
            End::Protocol(e) => {
                log::debug!("{}: protocol error in {:?}: {e}", self.addr, self.state);
                self.send_disconnect(&Component::translatable(
                    "disconnect.genericReason",
                    vec![Component::text("Invalid packet")],
                ));
            }
        }
        self.out.close();
    }

    fn send_disconnect(&self, reason: &Component) {
        match self.state {
            State::Handshake | State::Status => {}
            State::Login => {
                self.out.send(&login::LoginDisconnect {
                    reason: JsonComponent(reason.clone()),
                });
            }
            State::Configuration => {
                self.out.send(&cfg::clientbound::Disconnect {
                    reason: reason.clone(),
                });
            }
            State::Play => {
                self.out.send(&play::clientbound::Disconnect {
                    reason: reason.clone(),
                });
            }
        }
    }

    fn enter(&mut self, state: State, timeout: Option<Duration>) {
        self.state = state;
        self.source.set_state(state);
        self.deadline = timeout.and_then(|t| Instant::now().checked_add(t));
        self.keepalive.enabled = matches!(state, State::Configuration | State::Play);
        // Like Vanilla's new packet listener per state: a keepalive of the previous state is
        // not waited for any more.
        self.keepalive.pending = None;
        self.keepalive.next = Instant::now();
    }

    /// Receives and decodes the next packet, handling timeouts and keepalives while waiting.
    async fn recv<T>(&mut self, decode: fn(&[u8]) -> vanillite_protocol::Result<T>) -> Result<T> {
        loop {
            if let Source::Tcp { decoder, .. } = &mut self.source
                && let Some(frame) = decoder.next_frame()?
            {
                return Ok(decode(frame)?);
            }
            let read_deadline = self.last_read.checked_add(self.shared.config.read_timeout);
            let mut wake = [self.deadline, read_deadline].into_iter().flatten().min();
            if self.keepalive.enabled {
                let ka = match self.keepalive.pending {
                    Some((_, sent)) => sent.checked_add(self.shared.config.keepalive_interval),
                    None => Some(self.keepalive.next),
                };
                wake = [wake, ka].into_iter().flatten().min();
            }
            let wake = wake.unwrap_or_else(Instant::now);
            tokio::select! {
                r = self.source.read() => {
                    self.last_read = Instant::now();
                    match r {
                        Read::Bytes => {}
                        Read::Eof => return Err(End::Closed),
                        Read::Frame(f) => {
                            let max = match &self.source { Source::Virtual { max, .. } => *max, Source::Tcp { .. } => 0 };
                            if f.len() > max {
                                return Err(Error::TooLong { what: "frame", len: f.len(), max }.into());
                            }
                            return Ok(decode(&f)?);
                        }
                    }
                }
                () = tokio::time::sleep_until(wake) => self.tick()?,
            }
        }
    }

    fn tick(&mut self) -> Result<()> {
        let now = Instant::now();
        if self.deadline.is_some_and(|d| now >= d)
            || self
                .last_read
                .checked_add(self.shared.config.read_timeout)
                .is_some_and(|d| now >= d)
        {
            return Err(End::Timeout);
        }
        if !self.keepalive.enabled {
            return Ok(());
        }
        match self.keepalive.pending {
            Some((_, sent))
                if now.saturating_duration_since(sent) >= self.shared.config.keepalive_interval =>
            {
                Err(End::Timeout)
            }
            Some(_) => Ok(()),
            None if now >= self.keepalive.next => {
                let id = i64::try_from(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis(),
                )
                .unwrap_or_default();
                self.keepalive.pending = Some((id, now));
                self.keepalive.next = now
                    .checked_add(self.shared.config.keepalive_interval)
                    .unwrap_or(now);
                if self.state == State::Play {
                    self.out.send(&play::clientbound::KeepAlive { id });
                } else {
                    self.out.send(&cfg::clientbound::KeepAlive { id });
                }
                Ok(())
            }
            None => Ok(()),
        }
    }

    fn keepalive_answer(&mut self, id: i64) -> Result<()> {
        match self.keepalive.pending {
            Some((expected, sent)) if expected == id => {
                let rtt = u32::try_from(Instant::now().saturating_duration_since(sent).as_millis())
                    .unwrap_or(u32::MAX);
                let old = self.keepalive.latency.0.load(Ordering::Relaxed);
                let smoothed = (u64::from(old)
                    .saturating_mul(3)
                    .saturating_add(u64::from(rtt)))
                    / 4;
                self.keepalive.latency.0.store(
                    u32::try_from(smoothed).unwrap_or(u32::MAX),
                    Ordering::Relaxed,
                );
                self.keepalive.pending = None;
                Ok(())
            }
            _ => Err(End::Timeout),
        }
    }

    fn rate(&mut self, cost: (vanillite_protocol::Bucket, u16)) -> Result<()> {
        if self.rate.take(cost.0, cost.1) {
            Ok(())
        } else {
            Err(kick(Component::translatable(
                "disconnect.exceeded_packet_rate",
                vec![],
            )))
        }
    }

    async fn drive(&mut self) -> Result<()> {
        let ServerboundHandshake::Intention(hs) = self.recv(ServerboundHandshake::decode).await?;
        match hs.intent {
            Intent::Status => self.status().await,
            Intent::Login | Intent::Transfer => {
                self.enter(State::Login, Some(self.shared.config.login_timeout));
                if hs.protocol_version != vanillite_protocol::PROTOCOL_VERSION {
                    let key = if hs.protocol_version < 754 {
                        "multiplayer.disconnect.outdated_client"
                    } else {
                        "multiplayer.disconnect.incompatible"
                    };
                    return Err(kick(Component::translatable(
                        key,
                        vec![Component::text(vanillite_protocol::VERSION_ID)],
                    )));
                }
                if hs.intent == Intent::Transfer && !self.shared.config.accepts_transfers {
                    return Err(kick(Component::translatable(
                        "multiplayer.disconnect.transfers_disabled",
                        vec![],
                    )));
                }
                let profile = self.login().await?;
                let client_info = self.configure().await?;
                self.play(profile, client_info).await
            }
        }
    }

    async fn status(&mut self) -> Result<()> {
        self.enter(State::Status, Some(self.shared.config.status_timeout));
        let mut answered = false;
        loop {
            match self.recv(ServerboundStatus::decode).await? {
                ServerboundStatus::StatusRequest(_) if !answered => {
                    answered = true;
                    self.out.send(&status::StatusResponse {
                        json: self.shared.status.get().to_string(),
                    });
                }
                ServerboundStatus::PingRequest(p) => {
                    self.out.send(&status::PongResponse { time: p.time });
                    return Err(End::Done);
                }
                ServerboundStatus::StatusRequest(_) => {
                    return Err(Error::Invalid("second status request").into());
                }
            }
        }
    }

    async fn login(&mut self) -> Result<GameProfile> {
        let ServerboundLogin::Hello(hello) = self.recv(ServerboundLogin::decode).await? else {
            return Err(Error::Invalid("expected login hello").into());
        };
        // Cheap checks before any RSA or network work (SEC-18).
        if !is_valid_player_name(&hello.name) {
            return Err(kick(Component::translatable(
                "multiplayer.disconnect.invalid_player_data",
                vec![],
            )));
        }
        let shared = Arc::clone(&self.shared);
        let profile = if let Some(profile) = self.vouched.clone() {
            profile
        } else if let Some(keys) = shared.keys.as_ref() {
            let challenge: [u8; 4] = rand_bytes();
            self.out.send(&login::EncryptionRequest {
                server_id: String::new(),
                public_key: RawBytes(keys.public_der().to_vec()),
                challenge: RawBytes(challenge.to_vec()),
                should_authenticate: true,
            });
            let ServerboundLogin::Key(key) = self.recv(ServerboundLogin::decode).await? else {
                return Err(Error::Invalid("expected key").into());
            };
            let secret: [u8; 16] = keys
                .decrypt(&key.shared_secret.0)
                .and_then(|s| <[u8; 16]>::try_from(s.as_slice()).ok())
                .ok_or(Error::Invalid("shared secret"))?;
            if keys.decrypt(&key.challenge.0).as_deref() != Some(&challenge[..]) {
                return Err(Error::Invalid("challenge").into());
            }
            if let Source::Tcp { decoder, .. } = &mut self.source {
                decoder.enable_encryption(&secret);
            }
            self.out.control(Out::Encryption(secret));
            let hash = server_hash("", &secret, keys.public_der());
            self.authenticate(hello.name, hash).await?
        } else {
            log::debug!("{}: offline-mode login of {}", self.addr, hello.name);
            GameProfile {
                id: Uuid::offline(&hello.name),
                name: hello.name,
                properties: Vec::new(),
            }
        };

        let threshold = self.shared.config.compression_threshold;
        if threshold >= 0 && self.vouched.is_none() {
            self.out.send(&login::LoginCompression { threshold });
            self.out.control(Out::Compression(threshold));
            if let Source::Tcp { decoder, .. } = &mut self.source {
                decoder.set_compression(threshold);
            }
        }
        self.out.send(&login::LoginFinished {
            profile: profile.clone(),
            session_id: self.shared.session_id,
        });
        // Answers to requests we never send are protocol errors, like in Vanilla.
        let ServerboundLogin::LoginAcknowledged(_) = self.recv(ServerboundLogin::decode).await?
        else {
            return Err(Error::Invalid("unexpected login packet").into());
        };
        log::info!(
            "{} ({}) logged in from {}",
            profile.name,
            profile.id,
            self.addr
        );
        Ok(profile)
    }

    async fn authenticate(&mut self, name: String, hash: String) -> Result<GameProfile> {
        let _permit = self
            .shared
            .auth_permits
            .acquire()
            .await
            .map_err(|_| End::Closed)?;
        let auth = Arc::clone(&self.shared.authenticator);
        let ip = self
            .shared
            .config
            .prevent_proxy_connections
            .then(|| self.addr.ip());
        let task = tokio::task::spawn_blocking(move || {
            let r = auth.authenticate(&name, &hash, ip);
            (name, r)
        });
        let (name, result) = task.await.map_err(|_| End::Closed)?;
        match result {
            AuthResult::Verified(p) if p.name.eq_ignore_ascii_case(&name) => Ok(p),
            AuthResult::Verified(_) | AuthResult::Unverified => Err(kick(Component::translatable(
                "multiplayer.disconnect.unverified_username",
                vec![],
            ))),
            AuthResult::Unavailable => Err(kick(Component::translatable(
                "multiplayer.disconnect.authservers_down",
                vec![],
            ))),
        }
    }

    async fn configure(&mut self) -> Result<ClientInfo> {
        self.enter(
            State::Configuration,
            Some(self.shared.config.configuration_timeout),
        );
        let data = Arc::clone(&self.shared.configuration);
        let mut brand = Vec::new();
        vanillite_protocol::codec::write_str(&mut brand, &self.shared.config.brand);
        self.out.send(&cfg::clientbound::CustomPayload {
            channel: Identifier::vanilla("brand"),
            data: RawBytes(brand),
        });
        self.out.send_shared(&data.features);
        self.out.send_shared(&data.select_known_packs);
        let mut info = ClientInfo::default();
        let mut finish_sent = false;
        loop {
            let packet = self.recv(ServerboundConfiguration::decode).await?;
            self.rate(packet.cost())?;
            match packet {
                ServerboundConfiguration::ClientInformation(c) => info = c.0,
                ServerboundConfiguration::SelectKnownPacks(p) if !finish_sent => {
                    let registries = if p.packs == data.offered_packs {
                        &data.registries_known
                    } else {
                        &data.registries_full
                    };
                    for r in registries {
                        self.out.send_shared(r);
                    }
                    self.out.send_shared(&data.tags);
                    self.out.send(&cfg::clientbound::FinishConfiguration);
                    finish_sent = true;
                    // The client is in play after `finish_configuration`: a configuration
                    // keepalive sent now would reach it as an unknown play packet.
                    self.keepalive.enabled = false;
                }
                ServerboundConfiguration::FinishConfiguration(_) if finish_sent => return Ok(info),
                ServerboundConfiguration::KeepAlive(k) => self.keepalive_answer(k.id)?,
                ServerboundConfiguration::CustomPayload(_)
                | ServerboundConfiguration::Pong(_)
                | ServerboundConfiguration::ResourcePack(_)
                | ServerboundConfiguration::CookieResponse(_)
                | ServerboundConfiguration::CustomClickAction(_)
                | ServerboundConfiguration::AcceptCodeOfConduct(_) => {}
                ServerboundConfiguration::SelectKnownPacks(_)
                | ServerboundConfiguration::FinishConfiguration(_) => {
                    return Err(Error::Invalid("unexpected configuration packet").into());
                }
            }
        }
    }

    async fn play(&mut self, profile: GameProfile, client_info: ClientInfo) -> Result<()> {
        self.enter(State::Play, None);
        let (tx, rx) = std::sync::mpsc::sync_channel(self.shared.config.max_inbound_packets);
        let joined = Joined {
            profile,
            client_info,
            address: self.addr,
            is_virtual: self.vouched.is_some(),
            inbound: rx,
            outbound: self.out.clone(),
            latency: self.keepalive.latency.clone(),
        };
        if self.shared.joins.try_send(joined).is_err() {
            return Err(kick(Component::translatable(
                "multiplayer.disconnect.server_full",
                vec![],
            )));
        }
        if let Some(g) = self.guard.as_mut() {
            g.joined();
        }
        let mut seq: u64 = 0;
        loop {
            let packet = self.recv(ServerboundPlay::decode).await?;
            if self.out.is_closed() {
                return Err(End::Closed);
            }
            self.rate(packet.cost())?;
            match packet {
                ServerboundPlay::KeepAlive(k) => self.keepalive_answer(k.id)?,
                // `handlePingRequest` answers on the network thread, like Vanilla.
                ServerboundPlay::PingRequest(p) => {
                    self.out
                        .send(&play::clientbound::PongResponse { time: p.time });
                }
                ServerboundPlay::Unhandled { .. } => {}
                packet => {
                    seq = seq.wrapping_add(1);
                    if tx.try_send(Inbound { seq, packet }).is_err() {
                        // The world is not draining this connection fast enough (SEC-17).
                        return Err(kick(Component::translatable(
                            "disconnect.exceeded_packet_rate",
                            vec![],
                        )));
                    }
                }
            }
        }
    }
}

fn kick(reason: Component) -> End {
    End::Kick(Box::new(reason))
}

/// Random bytes from the OS.
pub(crate) fn rand_bytes<const N: usize>() -> [u8; N] {
    use rsa::rand_core::RngCore;
    let mut b = [0u8; N];
    rsa::rand_core::OsRng.fill_bytes(&mut b);
    b
}
