//! A blocking test client over any `Read + Write` stream: handshake, status, login (with an
//! optional encryption callback, since RSA and authentication live outside this crate) and the
//! configuration phase. Used by tests and tools; it trusts nothing the server sends beyond what
//! the decoders check.

use crate::frame::{FrameDecoder, FrameEncoder};
use crate::packets::configuration::serverbound as csb;
use crate::packets::handshake::{Intent, Intention};
use crate::packets::login::{EncryptionRequest, Hello, Key, LoginAcknowledged};
use crate::packets::status::{PingRequest, StatusRequest};
use crate::sets::{ClientboundConfiguration, ClientboundLogin, ClientboundStatus};
use crate::types::{GameProfile, RawBytes, Uuid};
use crate::{Packet, State};
use std::io::{Read, Write};

/// Test client errors.
#[derive(Debug)]
pub enum ClientError {
    Io(std::io::Error),
    Protocol(crate::Error),
    /// The server disconnected with this reason.
    Disconnected(String),
    /// The server sent something out of order.
    Unexpected(&'static str),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O: {e}"),
            Self::Protocol(e) => write!(f, "protocol: {e}"),
            Self::Disconnected(r) => write!(f, "disconnected: {r}"),
            Self::Unexpected(w) => write!(f, "unexpected: {w}"),
        }
    }
}

impl std::error::Error for ClientError {}

impl From<std::io::Error> for ClientError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<crate::Error> for ClientError {
    fn from(e: crate::Error) -> Self {
        Self::Protocol(e)
    }
}

/// Result of the test client.
pub type ClientResult<T> = std::result::Result<T, ClientError>;

/// What an encryption callback returns: the shared secret and the RSA-encrypted secret and
/// challenge for the `key` packet.
#[derive(Debug, Clone)]
pub struct EncryptionAnswer {
    pub secret: [u8; 16],
    pub encrypted_secret: Vec<u8>,
    pub encrypted_challenge: Vec<u8>,
}

/// Handles the server's encryption request (RSA and, for online mode, the session join).
pub type Encryptor<'a> = &'a mut dyn FnMut(&EncryptionRequest) -> ClientResult<EncryptionAnswer>;

/// The client.
#[derive(Debug)]
pub struct TestClient<S> {
    stream: S,
    decoder: FrameDecoder,
    encoder: FrameEncoder,
    state: State,
    out: Vec<u8>,
    /// Packets of the configuration phase the client did not need to answer, by Vanilla name.
    pub received: Vec<&'static str>,
}

impl<S: Read + Write> TestClient<S> {
    /// A client on a connected stream.
    pub fn new(stream: S) -> Self {
        let mut decoder = FrameDecoder::new();
        // The client side reads clientbound packets; use the largest limits.
        decoder.set_limits(State::Play.limits());
        Self {
            stream,
            decoder,
            encoder: FrameEncoder::new(),
            state: State::Handshake,
            out: Vec::new(),
            received: Vec::new(),
        }
    }

    /// The current state.
    pub fn state(&self) -> State {
        self.state
    }

    /// The underlying stream.
    pub fn stream_mut(&mut self) -> &mut S {
        &mut self.stream
    }

    /// Sends a packet.
    pub fn send<P: Packet>(&mut self, packet: &P) -> ClientResult<()> {
        self.out.clear();
        self.encoder.packet(packet, &mut self.out)?;
        self.stream.write_all(&self.out)?;
        self.stream.flush()?;
        Ok(())
    }

    /// Receives the next packet as `id` + body.
    pub fn recv_raw(&mut self) -> ClientResult<Vec<u8>> {
        loop {
            if let Some(frame) = self.decoder.next_frame()? {
                return Ok(frame.to_vec());
            }
            let spare = self.decoder.spare(8192);
            let n = self.stream.read(spare)?;
            self.decoder.commit(8192, n);
            if n == 0 {
                return Err(ClientError::Io(std::io::ErrorKind::UnexpectedEof.into()));
            }
        }
    }

    /// Sends the handshake.
    pub fn handshake(&mut self, host: &str, port: u16, intent: Intent) -> ClientResult<()> {
        self.send(&Intention {
            protocol_version: crate::PROTOCOL_VERSION,
            host: host.into(),
            port,
            intent,
        })?;
        self.state = match intent {
            Intent::Status => State::Status,
            Intent::Login | Intent::Transfer => State::Login,
        };
        Ok(())
    }

    /// Server list ping after a status handshake: returns the status JSON and checks the pong.
    pub fn status(&mut self) -> ClientResult<String> {
        self.send(&StatusRequest)?;
        let ClientboundStatus::StatusResponse(resp) = ClientboundStatus::decode(&self.recv_raw()?)?
        else {
            return Err(ClientError::Unexpected("expected status_response"));
        };
        self.send(&PingRequest { time: 0x1234_5678 })?;
        match ClientboundStatus::decode(&self.recv_raw()?)? {
            ClientboundStatus::PongResponse(p) if p.time == 0x1234_5678 => Ok(resp.json),
            _ => Err(ClientError::Unexpected("expected matching pong_response")),
        }
    }

    /// Logs in after a login handshake and acknowledges, ending in the configuration state.
    pub fn login(
        &mut self,
        name: &str,
        mut encryptor: Option<Encryptor<'_>>,
    ) -> ClientResult<GameProfile> {
        self.send(&Hello {
            name: name.into(),
            profile_id: Uuid::offline(name),
        })?;
        loop {
            match ClientboundLogin::decode(&self.recv_raw()?)? {
                ClientboundLogin::EncryptionRequest(req) => {
                    let Some(enc) = encryptor.as_mut() else {
                        return Err(ClientError::Unexpected("server requested encryption"));
                    };
                    let answer = enc(&req)?;
                    self.send(&Key {
                        shared_secret: RawBytes(answer.encrypted_secret),
                        challenge: RawBytes(answer.encrypted_challenge),
                    })?;
                    self.encoder.enable_encryption(&answer.secret);
                    self.decoder.enable_encryption(&answer.secret);
                }
                ClientboundLogin::LoginCompression(c) => {
                    self.encoder.set_compression(c.threshold);
                    self.decoder.set_compression(c.threshold);
                }
                ClientboundLogin::LoginFinished(f) => {
                    self.send(&LoginAcknowledged)?;
                    self.state = State::Configuration;
                    return Ok(f.profile);
                }
                ClientboundLogin::LoginDisconnect(d) => {
                    return Err(ClientError::Disconnected(d.reason.0.plain()));
                }
                ClientboundLogin::CustomQuery(q) => {
                    self.send(&crate::packets::login::CustomQueryAnswer {
                        transaction_id: q.transaction_id,
                        payload: None,
                    })?;
                }
                ClientboundLogin::CookieRequest(c) => {
                    self.send(&crate::packets::login::CookieResponse {
                        key: c.key,
                        payload: None,
                    })?;
                }
            }
        }
    }

    /// Runs the configuration phase like a Vanilla client: answers known packs, keepalives and
    /// pings, and acknowledges the finish, ending in the play state.
    pub fn configure(&mut self) -> ClientResult<()> {
        self.send(&csb::ClientInformation(Default::default()))?;
        loop {
            let packet = ClientboundConfiguration::decode(&self.recv_raw()?)?;
            match packet {
                ClientboundConfiguration::SelectKnownPacks(p) => {
                    self.send(&csb::SelectKnownPacks { packs: p.packs })?;
                }
                ClientboundConfiguration::KeepAlive(k) => {
                    self.send(&csb::KeepAlive { id: k.id })?
                }
                ClientboundConfiguration::Ping(p) => self.send(&csb::Pong { id: p.id })?,
                ClientboundConfiguration::CodeOfConduct(_) => {
                    self.send(&csb::AcceptCodeOfConduct)?
                }
                ClientboundConfiguration::Disconnect(d) => {
                    return Err(ClientError::Disconnected(d.reason.plain()));
                }
                ClientboundConfiguration::FinishConfiguration(_) => {
                    self.send(&csb::FinishConfiguration)?;
                    self.state = State::Play;
                    return Ok(());
                }
                ClientboundConfiguration::CookieRequest(c) => {
                    self.send(&csb::CookieResponse {
                        key: c.key,
                        payload: None,
                    })?;
                }
                other => self.received.push(other.name()),
            }
        }
    }

    /// Encodes a packet body without sending (for raw frame tests).
    pub fn encode<P: Packet>(packet: &P) -> Vec<u8> {
        crate::codec::Encode::to_bytes(packet)
    }

    /// Sends raw bytes as they are (for malformed-input tests).
    pub fn send_raw(&mut self, bytes: &[u8]) -> ClientResult<()> {
        self.stream.write_all(bytes)?;
        self.stream.flush()?;
        Ok(())
    }
}
