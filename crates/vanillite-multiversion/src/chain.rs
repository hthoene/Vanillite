//! The adapter of one connection: chained steps, the connection state of each half and the
//! cache of translated shared packets.

use crate::{Result, State, Version};
use vanillite_protocol::SharedPacket;
use vanillite_protocol::codec::Reader;
use vanillite_protocol::ids;

/// One step's clientbound side: newer packets → older packets.
pub(crate) trait ClientboundStep: Send + std::fmt::Debug {
    /// Rewrites one packet body (`id` + fields) of `state` into `out`. `false` drops it.
    fn translate(&mut self, state: State, body: &[u8], out: &mut Vec<u8>) -> Result<bool>;

    /// Whether the translation of packet `id` (newer version) depends on nothing but its body,
    /// so one translation serves every receiver of a shared packet.
    fn stateless(&self, state: State, id: i32) -> bool;
}

/// One step's serverbound side: older packets → newer packets.
pub(crate) trait ServerboundStep: Send + std::fmt::Debug {
    /// Rewrites one packet body of `state` into `out`. `false` drops it.
    fn translate(&mut self, state: State, body: &[u8], out: &mut Vec<u8>) -> Result<bool>;
}

/// Both halves of a new connection's adapter.
#[derive(Debug)]
pub struct Adapter {
    pub serverbound: ServerboundHalf,
    pub clientbound: ClientboundHalf,
}

impl Adapter {
    /// Steps from `version` up to the current version, starting in `state`.
    pub(crate) fn new(version: Version, state: State) -> Self {
        // One step per supported version so far; later versions chain more steps here.
        let (cb, sb) = crate::v26_2::open();
        Self {
            serverbound: ServerboundHalf {
                version,
                state,
                steps: vec![sb],
                bufs: [Vec::new(), Vec::new()],
                last: 0,
            },
            clientbound: ClientboundHalf {
                version,
                state,
                steps: vec![cb],
                bufs: [Vec::new(), Vec::new()],
            },
        }
    }

    /// Splits into the halves for the reader and the writer.
    pub fn split(self) -> (ServerboundHalf, ClientboundHalf) {
        (self.serverbound, self.clientbound)
    }
}

fn packet_id(body: &[u8]) -> Result<i32> {
    Reader::new(body).varint()
}

/// Translates what the client sends.
#[derive(Debug)]
pub struct ServerboundHalf {
    version: Version,
    state: State,
    /// Oldest step first.
    steps: Vec<Box<dyn ServerboundStep>>,
    bufs: [Vec<u8>; 2],
    /// Which buffer holds the last translation.
    last: usize,
}

impl ServerboundHalf {
    /// The client's version.
    pub fn version(&self) -> Version {
        self.version
    }

    /// Translates one packet body from the client into the server's version. `None` drops it.
    pub fn translate<'s>(&'s mut self, body: &[u8]) -> Result<Option<&'s [u8]>> {
        let [a, b] = &mut self.bufs;
        let (mut input, mut output) = (a, b);
        input.clear();
        input.extend_from_slice(body);
        for step in &mut self.steps {
            output.clear();
            if !step.translate(self.state, input, output)? {
                return Ok(None);
            }
            std::mem::swap(&mut input, &mut output);
        }
        let id = packet_id(input)?;
        let last = usize::from(self.steps.len() % 2 == 1);
        self.state = match (self.state, id) {
            (State::Login, ids::login::serverbound::LOGIN_ACKNOWLEDGED)
            | (State::Play, ids::play::serverbound::CONFIGURATION_ACKNOWLEDGED) => {
                State::Configuration
            }
            (State::Configuration, ids::configuration::serverbound::FINISH_CONFIGURATION) => {
                State::Play
            }
            (s, _) => s,
        };
        self.last = last;
        Ok(Some(self.translated()))
    }

    /// The result of the last successful [`translate`](Self::translate).
    pub fn translated(&self) -> &[u8] {
        self.bufs.get(self.last).map_or(&[], Vec::as_slice)
    }
}

/// Translates what the server sends.
#[derive(Debug)]
pub struct ClientboundHalf {
    version: Version,
    state: State,
    /// Oldest step first; clientbound packets run through them newest first.
    steps: Vec<Box<dyn ClientboundStep>>,
    bufs: [Vec<u8>; 2],
}

impl ClientboundHalf {
    /// The client's version.
    pub fn version(&self) -> Version {
        self.version
    }

    /// Translates one packet body from the server into the client's version. `None` drops it.
    pub fn translate<'s>(&'s mut self, body: &[u8]) -> Result<Option<&'s [u8]>> {
        let state = self.state;
        if let Ok(id) = packet_id(body) {
            self.state = match (state, id) {
                (State::Login, ids::login::clientbound::LOGIN_FINISHED)
                | (State::Play, ids::play::clientbound::START_CONFIGURATION) => {
                    State::Configuration
                }
                (State::Configuration, ids::configuration::clientbound::FINISH_CONFIGURATION) => {
                    State::Play
                }
                (s, _) => s,
            };
        }
        let [a, b] = &mut self.bufs;
        let (mut input, mut output) = (a, b);
        input.clear();
        input.extend_from_slice(body);
        for step in self.steps.iter_mut().rev() {
            output.clear();
            if !step.translate(state, input, output)? {
                return Ok(None);
            }
            std::mem::swap(&mut input, &mut output);
        }
        Ok(Some(input.as_slice()))
    }

    /// Translates a shared packet: once for all receivers of this version when the translation
    /// is stateless, else for this connection. Packets that fail to translate are dropped.
    pub fn translate_shared<'s>(&'s mut self, packet: &'s SharedPacket) -> Option<&'s [u8]> {
        let body = packet.body();
        let cacheable = self.state == State::Play
            && packet_id(body)
                .is_ok_and(|id| self.steps.iter().all(|s| s.stateless(State::Play, id)));
        if cacheable {
            let protocol = self.version.protocol;
            if let Some(cached) = packet.translated(protocol, |body| {
                self.translate(body).ok().flatten().map(<[u8]>::to_vec)
            }) {
                return cached;
            }
        }
        match self.translate(body) {
            Ok(out) => out,
            Err(e) => {
                log::debug!("multiversion: dropped an untranslatable packet: {e}");
                None
            }
        }
    }
}
