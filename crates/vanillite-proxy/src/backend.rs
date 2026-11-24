//! Connecting a player to a backend: handshake, login with Velocity modern forwarding, and the
//! switch to configuration. The result is a link whose next packets are the backend's
//! configuration phase.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpStream;
use vanillite_net::velocity::{self, Secret};
use vanillite_protocol::State;
use vanillite_protocol::packets::handshake::{Intent, Intention};
use vanillite_protocol::packets::login::{
    CookieResponse, CustomQueryAnswer, Hello, LoginAcknowledged,
};
use vanillite_protocol::sets::ClientboundLogin;
use vanillite_protocol::types::{GameProfile, RawBytes};

use crate::link::{self, Rx, Tx};

/// A backend connection in the configuration state.
#[derive(Debug)]
pub(crate) struct Backend {
    /// Server name from the configuration.
    pub(crate) name: Arc<str>,
    pub(crate) rx: Rx,
    pub(crate) tx: Tx,
}

/// What the connection needs to know about the player.
#[derive(Debug, Clone)]
pub(crate) struct Player {
    pub(crate) profile: GameProfile,
    pub(crate) address: IpAddr,
    pub(crate) secret: Option<Secret>,
    pub(crate) compression_level: i32,
}

/// Connects and logs in; the error is a reason for the player.
pub(crate) async fn connect(
    name: Arc<str>,
    address: String,
    player: Arc<Player>,
    timeout: Duration,
) -> Result<Backend, String> {
    match tokio::time::timeout(timeout, login(name, &address, &player)).await {
        Ok(r) => r,
        Err(_) => Err("the server did not answer in time".to_owned()),
    }
}

async fn login(name: Arc<str>, address: &str, player: &Player) -> Result<Backend, String> {
    let stream = TcpStream::connect(address)
        .await
        .map_err(|e| format!("cannot connect: {e}"))?;
    let (mut rx, mut tx) = link::split(stream, player.compression_level);
    rx.set_state(State::Login);
    let (host, port) = address
        .rsplit_once(':')
        .map(|(h, p)| (h, p.parse().unwrap_or(25565)))
        .unwrap_or((address, 25565));
    let fail = |e: vanillite_protocol::Error| format!("protocol error: {e}");
    tx.send(&Intention {
        protocol_version: vanillite_protocol::PROTOCOL_VERSION,
        host: host.to_owned(),
        port,
        intent: Intent::Login,
    })
    .map_err(fail)?;
    tx.send(&Hello {
        name: player.profile.name.clone(),
        profile_id: player.profile.id,
    })
    .map_err(fail)?;
    let io = |e: std::io::Error| format!("connection lost: {e}");
    let flush_timeout = Duration::from_secs(10);
    tx.flush(flush_timeout).await.map_err(io)?;
    let mut answer = Vec::new();
    loop {
        let packet = loop {
            if let Some(frame) = rx.decoder.next_frame().map_err(fail)? {
                break ClientboundLogin::decode(frame).map_err(fail)?;
            }
            if !rx.fill().await {
                return Err("the server closed the connection".to_owned());
            }
        };
        match packet {
            ClientboundLogin::CustomQuery(q) => {
                let payload = match &player.secret {
                    Some(secret) if q.channel.as_str() == velocity::CHANNEL => {
                        answer.clear();
                        let version = velocity::answer_version(&q.payload.0);
                        velocity::encode(
                            secret,
                            version,
                            player.address,
                            &player.profile,
                            &mut answer,
                        );
                        Some(RawBytes(std::mem::take(&mut answer)))
                    }
                    _ => None,
                };
                tx.send(&CustomQueryAnswer {
                    transaction_id: q.transaction_id,
                    payload,
                })
                .map_err(fail)?;
            }
            ClientboundLogin::CookieRequest(c) => tx
                .send(&CookieResponse {
                    key: c.key,
                    payload: None,
                })
                .map_err(fail)?,
            ClientboundLogin::LoginCompression(c) => {
                rx.decoder.set_compression(c.threshold);
                tx.encoder.set_compression(c.threshold);
            }
            ClientboundLogin::EncryptionRequest(_) => {
                return Err(
                    "the server is in online mode without proxy forwarding (configure modern \
                     forwarding with the proxy's secret)"
                        .to_owned(),
                );
            }
            ClientboundLogin::LoginDisconnect(d) => return Err(d.reason.0.plain()),
            ClientboundLogin::LoginFinished(f) => {
                if f.profile.id != player.profile.id {
                    return Err("the server did not accept the forwarded identity".to_owned());
                }
                tx.send(&LoginAcknowledged).map_err(fail)?;
                tx.flush(flush_timeout).await.map_err(io)?;
                // From here on the backend sends configuration and play packets.
                rx.set_state(State::Play);
                return Ok(Backend { name, rx, tx });
            }
        }
        tx.flush(flush_timeout).await.map_err(io)?;
    }
}
