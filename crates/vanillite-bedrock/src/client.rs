//! A headless Bedrock client for tests and tools: RakNet handshake, offline login, encryption,
//! resource pack handshake, then raw packets. It speaks only what the frontend needs.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;

use crate::batch::{self, Codec};
use crate::io::{self, Reader};
use crate::login;
use crate::packets::{self as bp, id};
use crate::raknet::{self, RakConn, Reliability};

pub use crate::packets::AuthInput;
pub use crate::packets::id as packet_id;
pub use crate::packets::input as input_flag;

/// A connected client.
#[derive(Debug)]
pub struct Client {
    socket: UdpSocket,
    server: SocketAddr,
    rak: RakConn,
    codec: Codec,
    inbox: VecDeque<(u32, Vec<u8>)>,
}

fn err<T>(e: impl std::fmt::Display) -> Result<T, String> {
    Err(e.to_string())
}

impl Client {
    /// Connects and logs in offline as `gamertag` with `xuid`; returns after the resource pack
    /// handshake, before `start_game`.
    pub async fn connect(server: SocketAddr, xuid: &str, gamertag: &str) -> Result<Self, String> {
        let socket = UdpSocket::bind("127.0.0.1:0")
            .await
            .map_err(|e| e.to_string())?;
        let mtu: u16 = 1400;
        let guid: i64 = -12345;
        // Open connection request 1, padded to the MTU.
        let mut req1 = vec![raknet::id::OPEN_CONNECTION_REQUEST_1];
        req1.extend_from_slice(&raknet::MAGIC);
        req1.push(raknet::PROTOCOL);
        req1.resize(usize::from(mtu).saturating_sub(raknet::UDP_OVERHEAD), 0);
        socket
            .send_to(&req1, server)
            .await
            .map_err(|e| e.to_string())?;
        let reply1 = recv_raw(&socket, raknet::id::OPEN_CONNECTION_REPLY_1).await?;
        let mut r = Reader::new(reply1.get(1..).unwrap_or_default());
        r.take(16 + 8).map_err(|e| e.to_string())?;
        let security = r.bool().map_err(|e| e.to_string())?;
        let cookie = if security {
            r.u32().map_err(|e| e.to_string())?.swap_bytes()
        } else {
            0
        };
        let mtu = u16::from_be_bytes(r.array().map_err(|e| e.to_string())?);
        // Request 2 with the cookie.
        let mut req2 = vec![raknet::id::OPEN_CONNECTION_REQUEST_2];
        req2.extend_from_slice(&raknet::MAGIC);
        req2.extend_from_slice(&cookie.to_be_bytes());
        req2.push(0);
        raknet::write_address(&mut req2, server);
        req2.extend_from_slice(&mtu.to_be_bytes());
        req2.extend_from_slice(&guid.to_be_bytes());
        socket
            .send_to(&req2, server)
            .await
            .map_err(|e| e.to_string())?;
        recv_raw(&socket, raknet::id::OPEN_CONNECTION_REPLY_2).await?;

        let now = Instant::now();
        let mut rak = RakConn::new(server, mtu, raknet::Limits::default(), now);
        let mut request = vec![raknet::id::CONNECTION_REQUEST];
        request.extend_from_slice(&guid.to_be_bytes());
        request.extend_from_slice(&0i64.to_be_bytes());
        request.push(0);
        rak.send(request, Reliability::ReliableOrdered);
        let mut incoming = vec![raknet::id::NEW_INCOMING_CONNECTION];
        raknet::write_address(&mut incoming, server);
        for _ in 0..20 {
            raknet::write_address(&mut incoming, SocketAddr::from(([0, 0, 0, 0], 0)));
        }
        incoming.extend_from_slice(&[0; 16]);
        rak.send(incoming, Reliability::ReliableOrdered);

        let mut client = Self {
            socket,
            server,
            rak,
            codec: Codec::new(0, 6, 8 << 20, 4096),
            inbox: VecDeque::new(),
        };
        client
            .send(
                id::REQUEST_NETWORK_SETTINGS,
                &crate::data::PROTOCOL.to_be_bytes(),
            )
            .await?;
        client.recv(id::NETWORK_SETTINGS).await?;
        client.codec.enable_compression();

        let secret = p384::SecretKey::random(&mut rsa::rand_core::OsRng);
        let mut login = crate::data::PROTOCOL.to_be_bytes().to_vec();
        io::write_bytes(&mut login, &login::offline_request(&secret, xuid, gamertag));
        client.send(id::LOGIN, &login).await?;
        let hs = client.recv(id::SERVER_TO_CLIENT_HANDSHAKE).await?;
        let jwt = Reader::new(&hs)
            .string("jwt", 1 << 16)
            .map_err(|e| e.to_string())?
            .to_owned();
        let key = login::client_key_from_handshake(&jwt, &secret).map_err(|e| e.0)?;
        client.codec.enable_encryption(key);
        client.send(id::CLIENT_TO_SERVER_HANDSHAKE, &[]).await?;
        let status = client.recv(id::PLAY_STATUS).await?;
        if status != bp::play_status(bp::status::LOGIN_SUCCESS) {
            return err("login failed");
        }
        client.recv(id::RESOURCE_PACKS_INFO).await?;
        client
            .send(id::RESOURCE_PACK_CLIENT_RESPONSE, &pack_response(2))
            .await?;
        client.recv(id::RESOURCE_PACK_STACK).await?;
        client
            .send(id::RESOURCE_PACK_CLIENT_RESPONSE, &pack_response(3))
            .await?;
        Ok(client)
    }

    /// Sends one packet.
    pub async fn send(&mut self, packet: u32, body: &[u8]) -> Result<(), String> {
        let mut plain = Vec::new();
        batch::push_packet(&mut plain, packet, body);
        let payload = self.codec.encode(&plain);
        self.rak.send_game(&payload);
        self.flush();
        Ok(())
    }

    fn flush(&mut self) {
        let (socket, server) = (&self.socket, self.server);
        self.rak.flush(Instant::now(), |d| {
            let _ = socket.try_send_to(d, server);
        });
    }

    /// The next packet of any id (with a timeout).
    pub async fn next(&mut self, timeout: Duration) -> Result<(u32, Vec<u8>), String> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .unwrap_or_else(Instant::now);
        let mut buf = vec![0u8; 2048];
        loop {
            if let Some(p) = self.inbox.pop_front() {
                return Ok(p);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return err("timed out");
            }
            let wait = left.min(Duration::from_millis(10));
            match tokio::time::timeout(wait, self.socket.recv_from(&mut buf)).await {
                Ok(Ok((n, _))) => {
                    let received = self
                        .rak
                        .receive(buf.get(..n).unwrap_or_default(), Instant::now())
                        .map_err(|e| e.to_string())?;
                    if received.disconnected {
                        return err("disconnected");
                    }
                    for game in received.game {
                        let mut packets = Vec::new();
                        self.codec
                            .decode(game, &mut packets)
                            .map_err(|e| e.to_string())?;
                        for p in packets {
                            let (pid, body) = batch::split_packet(&p).map_err(|e| e.to_string())?;
                            if pid == id::DISCONNECT {
                                let mut r = Reader::new(body);
                                let _ = r.vari32();
                                let _ = r.varu32();
                                let msg = r.string("message", 4096).unwrap_or_default();
                                return Err(format!("disconnected: {msg}"));
                            }
                            self.inbox.push_back((pid, body.to_vec()));
                        }
                    }
                }
                Ok(Err(e)) => return err(e),
                Err(_) => {}
            }
            self.flush();
        }
    }

    /// Receives until a packet with id `packet` arrives; earlier packets are dropped.
    pub async fn recv(&mut self, packet: u32) -> Result<Vec<u8>, String> {
        loop {
            let (pid, body) = self.next(Duration::from_secs(20)).await?;
            if pid == packet {
                return Ok(body);
            }
        }
    }
}

fn pack_response(code: u32) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_varu32(&mut out, code);
    io::write_str(&mut out, "");
    out
}

async fn recv_raw(socket: &UdpSocket, kind: u8) -> Result<Vec<u8>, String> {
    let mut buf = vec![0u8; 2048];
    loop {
        let (n, _) = tokio::time::timeout(Duration::from_secs(5), socket.recv_from(&mut buf))
            .await
            .map_err(|_| "RakNet handshake timed out".to_owned())?
            .map_err(|e| e.to_string())?;
        let data = buf.get(..n).unwrap_or_default();
        if data.first() == Some(&kind) {
            return Ok(data.to_vec());
        }
    }
}

/// Reads `start_game`'s leading fields: unique id and eye position.
pub fn read_start_game(body: &[u8]) -> Result<(i64, [f32; 3]), String> {
    let mut r = Reader::new(body);
    let unique = r.vari64().map_err(|e| e.to_string())?;
    r.varu64().map_err(|e| e.to_string())?;
    r.vari32().map_err(|e| e.to_string())?;
    Ok((unique, r.vec3().map_err(|e| e.to_string())?))
}

/// Encodes `player_auth_input`.
pub fn auth_input(i: &AuthInput) -> Vec<u8> {
    bp::write_auth_input(i)
}

/// Encodes a chat `text`.
pub fn chat(source: &str, message: &str) -> Vec<u8> {
    let mut out = vec![0, 1, 1];
    io::write_str(&mut out, source);
    io::write_str(&mut out, message);
    io::write_str(&mut out, "");
    io::write_str(&mut out, "");
    out.push(0);
    out
}

/// Reads the message of a `text` packet of type raw.
pub fn read_text(body: &[u8]) -> Option<String> {
    let mut r = Reader::new(body);
    r.take(3).ok()?;
    Some(r.string("message", 1 << 16).ok()?.to_owned())
}

/// Reads `add_player`: UUID-less name and eye position.
pub fn read_add_player(body: &[u8]) -> Option<(String, [f32; 3])> {
    let mut r = Reader::new(body);
    r.take(16).ok()?;
    let name = r.string("name", 64).ok()?.to_owned();
    r.varu64().ok()?;
    r.string("chat id", 64).ok()?;
    Some((name, r.vec3().ok()?))
}

/// Reads `move_player`: runtime id and eye position.
pub fn read_move_player(body: &[u8]) -> Option<(u64, [f32; 3])> {
    let mut r = Reader::new(body);
    let rid = r.varu64().ok()?;
    Some((rid, r.vec3().ok()?))
}
