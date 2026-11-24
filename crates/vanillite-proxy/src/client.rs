//! The client side before play: handshake, status, and login with session-server
//! authentication, encryption and compression. Everything goes through the shared decoders and
//! the per-state frame limits (SEC-01, SEC-05).

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::TcpStream;
use tokio::time::Instant;
use vanillite_net::{AuthResult, ConnectionGuard, is_valid_player_name, server_hash};
use vanillite_protocol::packets::handshake::Intent;
use vanillite_protocol::packets::{login, status};
use vanillite_protocol::sets::{ServerboundHandshake, ServerboundLogin, ServerboundStatus};
use vanillite_protocol::text::{Component, JsonComponent};
use vanillite_protocol::types::{GameProfile, RawBytes, Uuid};
use vanillite_protocol::{Error, State};

use crate::link::{self, Rx, Tx};
use crate::{Online, Shared};

/// Why a connection ends.
#[derive(Debug)]
pub(crate) enum End {
    /// Malformed or unexpected input.
    Protocol(Error),
    /// The peer closed or a write failed.
    Closed,
    /// A deadline passed.
    Timeout,
    /// Disconnect with a message.
    Kick(Box<Component>),
}

impl From<Error> for End {
    fn from(e: Error) -> Self {
        Self::Protocol(e)
    }
}

pub(crate) fn kick(text: impl Into<String>) -> End {
    End::Kick(Box::new(Component::text(text.into())))
}

fn kick_key(key: &str) -> End {
    End::Kick(Box::new(Component::translatable(key, vec![])))
}

/// The reason shown to the client for an end.
pub(crate) fn reason(end: End) -> Option<Component> {
    match end {
        End::Closed => None,
        End::Kick(c) => Some(*c),
        End::Timeout => Some(Component::translatable("disconnect.timeout", vec![])),
        End::Protocol(_) => Some(Component::translatable(
            "disconnect.genericReason",
            vec![Component::text("Invalid packet")],
        )),
    }
}

/// Receives one packet before `deadline`.
async fn recv<T>(
    rx: &mut Rx,
    deadline: Instant,
    decode: fn(&[u8]) -> vanillite_protocol::Result<T>,
) -> Result<T, End> {
    loop {
        if let Some(frame) = rx.decoder.next_frame()? {
            return Ok(decode(frame)?);
        }
        match tokio::time::timeout_at(deadline, rx.fill()).await {
            Ok(true) => {}
            Ok(false) => return Err(End::Closed),
            Err(_) => return Err(End::Timeout),
        }
    }
}

/// Removes the player from the table when the connection ends.
pub(crate) struct Registered {
    shared: Arc<Shared>,
    id: Uuid,
}

impl Drop for Registered {
    fn drop(&mut self) {
        if let Ok(mut players) = self.shared.players.lock() {
            players.remove(&self.id);
            self.shared.update_status(&players);
        }
    }
}

/// Runs one client connection to its end.
pub(crate) async fn handle(
    shared: Arc<Shared>,
    socket: TcpStream,
    addr: SocketAddr,
    mut guard: ConnectionGuard,
) {
    let (mut rx, mut tx) = link::split(socket, shared.config.compression_level);
    let timeout = shared.config.limits.read_timeout;
    let mut state = State::Handshake;
    let r = pre_play(&shared, &mut rx, &mut tx, addr, &mut state).await;
    let (profile, host, registered) = match r {
        Ok(Some(v)) => v,
        Ok(None) => return,
        Err(end) => {
            if let End::Protocol(e) = &end {
                log::debug!("{addr}: protocol error in {state:?}: {e}");
            }
            if state == State::Login
                && let Some(reason) = reason(end)
                && tx
                    .send(&login::LoginDisconnect {
                        reason: JsonComponent(reason),
                    })
                    .is_ok()
            {
                let _ = tx.flush(timeout).await;
            }
            return;
        }
    };
    guard.joined();
    log::info!("{} ({}) connected from {addr}", profile.name, profile.id);
    crate::session::run(shared, rx, tx, profile, addr.ip(), host, registered).await;
    drop(guard);
}

type Joined = (GameProfile, String, Registered);

async fn pre_play(
    shared: &Arc<Shared>,
    rx: &mut Rx,
    tx: &mut Tx,
    addr: SocketAddr,
    state: &mut State,
) -> Result<Option<Joined>, End> {
    let limits = &shared.config.limits;
    let deadline = Instant::now()
        .checked_add(limits.login_timeout)
        .unwrap_or_else(Instant::now);
    let flush = limits.read_timeout;
    let ServerboundHandshake::Intention(hs) =
        recv(rx, deadline, ServerboundHandshake::decode).await?;
    if hs.intent == Intent::Status {
        *state = State::Status;
        rx.set_state(State::Status);
        let ServerboundStatus::StatusRequest(_) =
            recv(rx, deadline, ServerboundStatus::decode).await?
        else {
            return Err(Error::Invalid("expected status request").into());
        };
        tx.send(&status::StatusResponse {
            json: shared.status.get().to_string(),
        })?;
        tx.flush(flush).await.map_err(|_| End::Closed)?;
        if let ServerboundStatus::PingRequest(p) =
            recv(rx, deadline, ServerboundStatus::decode).await?
        {
            tx.send(&status::PongResponse { time: p.time })?;
            tx.flush(flush).await.map_err(|_| End::Closed)?;
        }
        return Ok(None);
    }
    *state = State::Login;
    rx.set_state(State::Login);
    if hs.protocol_version != vanillite_protocol::PROTOCOL_VERSION {
        let key = if hs.protocol_version < vanillite_protocol::PROTOCOL_VERSION {
            "multiplayer.disconnect.outdated_client"
        } else {
            "multiplayer.disconnect.incompatible"
        };
        return Err(End::Kick(Box::new(Component::translatable(
            key,
            vec![Component::text(vanillite_protocol::VERSION_ID)],
        ))));
    }
    if hs.intent == Intent::Transfer {
        return Err(kick_key("multiplayer.disconnect.transfers_disabled"));
    }
    if !shared.login_allowed(addr.ip()) {
        return Err(kick(
            "Connection throttled! Please wait before reconnecting.",
        ));
    }
    // Forge and similar clients append data after a NUL; a trailing dot is a FQDN.
    let host = hs
        .host
        .split('\0')
        .next()
        .unwrap_or_default()
        .trim_end_matches('.')
        .to_ascii_lowercase();

    let ServerboundLogin::Hello(hello) = recv(rx, deadline, ServerboundLogin::decode).await? else {
        return Err(Error::Invalid("expected login hello").into());
    };
    if !is_valid_player_name(&hello.name) {
        return Err(kick_key("multiplayer.disconnect.invalid_player_data"));
    }
    let profile = if let Some(keys) = shared.keys.as_ref() {
        let challenge: [u8; 4] = vanillite_net::rand_bytes();
        tx.send(&login::EncryptionRequest {
            server_id: String::new(),
            public_key: RawBytes(keys.public_der().to_vec()),
            challenge: RawBytes(challenge.to_vec()),
            should_authenticate: true,
        })?;
        tx.flush(flush).await.map_err(|_| End::Closed)?;
        let ServerboundLogin::Key(key) = recv(rx, deadline, ServerboundLogin::decode).await? else {
            return Err(Error::Invalid("expected key").into());
        };
        let secret: [u8; 16] = keys
            .decrypt(&key.shared_secret.0)
            .and_then(|s| <[u8; 16]>::try_from(s.as_slice()).ok())
            .ok_or(Error::Invalid("shared secret"))?;
        if keys.decrypt(&key.challenge.0).as_deref() != Some(&challenge[..]) {
            return Err(Error::Invalid("challenge").into());
        }
        rx.decoder.enable_encryption(&secret);
        tx.encoder.enable_encryption(&secret);
        let hash = server_hash("", &secret, keys.public_der());
        authenticate(shared, hello.name, hash).await?
    } else {
        GameProfile {
            id: Uuid::offline(&hello.name),
            name: hello.name,
            properties: Vec::new(),
        }
    };

    let registered = {
        let Ok(mut players) = shared.players.lock() else {
            return Err(End::Closed);
        };
        if players.contains_key(&profile.id) {
            return Err(kick("You are already connected to this proxy."));
        }
        players.insert(
            profile.id,
            Online {
                name: profile.name.clone(),
                server: None,
            },
        );
        shared.update_status(&players);
        Registered {
            shared: Arc::clone(shared),
            id: profile.id,
        }
    };

    let threshold = shared.config.compression_threshold;
    if threshold >= 0 {
        tx.send(&login::LoginCompression { threshold })?;
        tx.encoder.set_compression(threshold);
        rx.decoder.set_compression(threshold);
    }
    tx.send(&login::LoginFinished {
        profile: profile.clone(),
        session_id: shared.session_id,
    })?;
    tx.flush(flush).await.map_err(|_| End::Closed)?;
    let ServerboundLogin::LoginAcknowledged(_) =
        recv(rx, deadline, ServerboundLogin::decode).await?
    else {
        return Err(Error::Invalid("unexpected login packet").into());
    };
    *state = State::Configuration;
    rx.set_state(State::Configuration);
    Ok(Some((profile, host, registered)))
}

async fn authenticate(
    shared: &Arc<Shared>,
    name: String,
    hash: String,
) -> Result<GameProfile, End> {
    let _permit = shared
        .auth_permits
        .acquire()
        .await
        .map_err(|_| End::Closed)?;
    let auth = Arc::clone(&shared.authenticator);
    let task = tokio::task::spawn_blocking(move || {
        let r = auth.authenticate(&name, &hash, None);
        (name, r)
    });
    let (name, result) = task.await.map_err(|_| End::Closed)?;
    match result {
        AuthResult::Verified(p) if p.name.eq_ignore_ascii_case(&name) => Ok(p),
        AuthResult::Verified(_) | AuthResult::Unverified => {
            Err(kick_key("multiplayer.disconnect.unverified_username"))
        }
        AuthResult::Unavailable => Err(kick_key("multiplayer.disconnect.authservers_down")),
    }
}
