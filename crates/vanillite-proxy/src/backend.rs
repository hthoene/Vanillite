//! Connecting a player to a backend: handshake, login with the configured player info
//! forwarding (Velocity modern, BungeeCord legacy or none), and the switch to configuration. The result is a link whose next packets are the backend's
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

use crate::config::Forwarding;
use crate::link::{self, Rx, Tx};

/// A backend connection in the configuration state.
#[derive(Debug)]
pub(crate) struct Backend {
    /// Server name from the configuration.
    pub(crate) name: Arc<str>,
    pub(crate) rx: Rx,
    pub(crate) tx: Tx,
    /// Trusted link with the client's threshold: compressed frames are relayed unchanged.
    pub(crate) passthrough: bool,
}

/// What the connection needs to know about the player.
#[derive(Debug, Clone)]
pub(crate) struct Player {
    pub(crate) profile: GameProfile,
    pub(crate) address: IpAddr,
    pub(crate) secret: Option<Secret>,
    pub(crate) forwarding: Forwarding,
    pub(crate) compression_level: i32,
    /// The client's compression threshold, offered to backends for a trusted link; `None` if
    /// the link is off.
    pub(crate) link_threshold: Option<i32>,
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
    let host = match player.forwarding {
        Forwarding::Legacy => legacy_host(host, player),
        Forwarding::Modern | Forwarding::None => host.to_owned(),
    };
    // Only modern forwarding is answered with the player's data and only with a secret.
    let secret = match player.forwarding {
        Forwarding::Modern => player.secret.as_ref(),
        Forwarding::Legacy | Forwarding::None => None,
    };
    tx.send(&Intention {
        protocol_version: vanillite_protocol::PROTOCOL_VERSION,
        host,
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
    let mut linked = false;
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
                let payload = match secret {
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
                    Some(secret) if q.channel.as_str() == velocity::LINK_CHANNEL => {
                        answer.clear();
                        match player.link_threshold {
                            Some(t)
                                if velocity::link_answer(secret, &q.payload.0, t, &mut answer) =>
                            {
                                linked = true;
                                Some(RawBytes(std::mem::take(&mut answer)))
                            }
                            _ => None,
                        }
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
                // Without forwarding an offline backend picks its own (offline) UUID.
                if f.profile.id != player.profile.id && player.forwarding != Forwarding::None {
                    return Err("the server did not accept the forwarded identity".to_owned());
                }
                tx.send(&LoginAcknowledged).map_err(fail)?;
                tx.flush(flush_timeout).await.map_err(io)?;
                // From here on the backend sends configuration and play packets.
                rx.set_state(State::Play);
                // Passing frames on is only valid with the threshold the client uses.
                let client = usize::try_from(player.link_threshold.unwrap_or(-1)).ok();
                let passthrough = linked && tx.encoder.threshold() == client;
                return Ok(Backend {
                    name,
                    rx,
                    tx,
                    passthrough,
                });
            }
        }
        tx.flush(flush_timeout).await.map_err(io)?;
    }
}

/// BungeeCord forwarding: the handshake host carries the player's address, undashed UUID and
/// profile properties (JSON), separated by NUL.
fn legacy_host(host: &str, player: &Player) -> String {
    // Written by hand in BungeeCord's field order (name, value, signature): a
    // `serde_json::Value` map would depend on whether some other crate in the
    // build enables serde_json's `preserve_order` feature.
    let quote = |text: &str| serde_json::Value::from(text).to_string();
    let properties: Vec<String> = player
        .profile
        .properties
        .iter()
        .map(|p| match &p.signature {
            Some(sig) => format!(
                "{{\"name\":{},\"value\":{},\"signature\":{}}}",
                quote(&p.name),
                quote(&p.value),
                quote(sig)
            ),
            None => format!(
                "{{\"name\":{},\"value\":{}}}",
                quote(&p.name),
                quote(&p.value)
            ),
        })
        .collect();
    format!(
        "{host}\0{}\0{:032x}\0[{}]",
        player.address,
        player.profile.id.0,
        properties.join(",")
    )
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]
mod tests {
    use std::io::{Read, Write};

    use vanillite_protocol::frame::{FrameDecoder, FrameEncoder};
    use vanillite_protocol::packets::login::{CustomQuery, LoginCompression, LoginFinished};
    use vanillite_protocol::sets::ServerboundLogin;
    use vanillite_protocol::types::{Identifier, Uuid};

    use super::*;

    const SECRET: &[u8] = b"link-test-secret";

    /// A fake backend that offers the trusted link if `offer` and then compresses with
    /// `threshold`; returns the threshold the proxy's link answer verified to.
    fn fake_backend(offer: bool, threshold: i32) -> (String, std::thread::JoinHandle<Option<i32>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let thread = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let secret = Secret::new(SECRET).unwrap();
            let mut decoder = FrameDecoder::new();
            decoder.set_limits(State::Login.limits());
            let mut encoder = FrameEncoder::new();
            let mut buf = vec![0u8; 4096];
            let mut next = |socket: &mut std::net::TcpStream, decoder: &mut FrameDecoder| loop {
                if let Some(f) = decoder.next_frame().unwrap() {
                    return f.to_vec();
                }
                let n = socket.read(&mut buf).unwrap();
                assert!(n > 0, "proxy closed");
                decoder.push(&buf[..n]);
            };
            let _intention = next(&mut socket, &mut decoder);
            let ServerboundLogin::Hello(hello) =
                ServerboundLogin::decode(&next(&mut socket, &mut decoder)).unwrap()
            else {
                panic!("expected hello");
            };
            let request = velocity::link_request([9; 16]);
            let mut out = Vec::new();
            encoder
                .packet(
                    &CustomQuery {
                        transaction_id: 1,
                        channel: Identifier::new(velocity::CHANNEL).unwrap(),
                        payload: RawBytes(vec![velocity::MAX_VERSION]),
                    },
                    &mut out,
                )
                .unwrap();
            if offer {
                encoder
                    .packet(
                        &CustomQuery {
                            transaction_id: 2,
                            channel: Identifier::new(velocity::LINK_CHANNEL).unwrap(),
                            payload: RawBytes(request.clone()),
                        },
                        &mut out,
                    )
                    .unwrap();
            }
            socket.write_all(&out).unwrap();
            let mut link = None;
            for _ in 0..1 + usize::from(offer) {
                let ServerboundLogin::CustomQueryAnswer(answer) =
                    ServerboundLogin::decode(&next(&mut socket, &mut decoder)).unwrap()
                else {
                    panic!("expected an answer");
                };
                let payload = answer.payload.map(|p| p.0);
                if answer.transaction_id == 1 {
                    let f = velocity::decode(&secret, &payload.unwrap()).unwrap();
                    assert_eq!(f.profile.name, hello.name);
                } else {
                    link = payload.and_then(|p| velocity::link_verify(&secret, &request, &p));
                }
            }
            out.clear();
            encoder
                .packet(&LoginCompression { threshold }, &mut out)
                .unwrap();
            encoder.set_compression(threshold);
            decoder.set_compression(threshold);
            encoder
                .packet(
                    &LoginFinished {
                        profile: GameProfile {
                            id: hello.profile_id,
                            name: hello.name,
                            properties: Vec::new(),
                        },
                        session_id: Uuid(0),
                    },
                    &mut out,
                )
                .unwrap();
            socket.write_all(&out).unwrap();
            assert!(matches!(
                ServerboundLogin::decode(&next(&mut socket, &mut decoder)).unwrap(),
                ServerboundLogin::LoginAcknowledged(_)
            ));
            link
        });
        (address, thread)
    }

    /// Logs in to a fake backend; whether the link was negotiated, and the verified threshold.
    fn negotiate(offer: bool, threshold: i32, link_threshold: Option<i32>) -> (bool, Option<i32>) {
        let (address, thread) = fake_backend(offer, threshold);
        let player = Arc::new(Player {
            profile: GameProfile {
                id: Uuid::offline("Alice"),
                name: "Alice".to_owned(),
                properties: Vec::new(),
            },
            address: [127, 0, 0, 1].into(),
            secret: Secret::new(SECRET),
            forwarding: Forwarding::Modern,
            compression_level: 4,
            link_threshold,
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let backend = runtime
            .block_on(connect(
                Arc::from("test"),
                address,
                player,
                Duration::from_secs(10),
            ))
            .unwrap();
        (backend.passthrough, thread.join().unwrap())
    }

    #[test]
    fn legacy_forwarding_host() {
        let player = Player {
            profile: GameProfile {
                id: Uuid(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef),
                name: "Alice".to_owned(),
                properties: vec![vanillite_protocol::types::Property {
                    name: "textures".to_owned(),
                    value: "v".to_owned(),
                    signature: Some("s".to_owned()),
                }],
            },
            address: [10, 0, 0, 7].into(),
            secret: None,
            forwarding: Forwarding::Legacy,
            compression_level: 4,
            link_threshold: None,
        };
        assert_eq!(
            legacy_host("lobby", &player),
            "lobby\u{0}10.0.0.7\u{0}0123456789abcdef0123456789abcdef\u{0}\
             [{\"name\":\"textures\",\"value\":\"v\",\"signature\":\"s\"}]"
        );
    }

    #[test]
    fn trusted_link_handshake() {
        // A Vanillite backend accepts the signed threshold: frames pass through.
        assert_eq!(negotiate(true, 256, Some(256)), (true, Some(256)));
        // No compression towards the client: the backend sends none either.
        assert_eq!(negotiate(true, -1, Some(-1)), (true, Some(-1)));
        // A backend that does not offer the link (Paper) keeps the re-encoding relay.
        assert_eq!(negotiate(false, 256, Some(256)), (false, None));
        // A backend that compresses differently is re-encoded although the link was answered.
        assert_eq!(negotiate(true, 512, Some(256)), (false, Some(256)));
        // With the link turned off the proxy does not answer the offer.
        assert_eq!(negotiate(true, 256, None), (false, None));
    }
}
