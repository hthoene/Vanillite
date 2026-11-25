//! The adapter of one connection: chained steps, the connection state of each half and the
//! cache of translated shared packets.

use std::collections::HashMap;

use crate::{Result, State, Version};
use vanillite_protocol::SharedPacket;
use vanillite_protocol::codec::Reader;
use vanillite_protocol::ids;

/// One step's clientbound side: newer packets → older packets.
pub(crate) trait ClientboundStep: Send + std::fmt::Debug {
    /// Rewrites one packet body (`id` + fields) of `state` into `out`. `false` drops it.
    fn translate(
        &mut self,
        state: State,
        body: &[u8],
        out: &mut Vec<u8>,
        ctx: &Ctx<'_>,
    ) -> Result<bool>;

    /// Whether the translation of packet `id` (newer version) depends on nothing but its body
    /// (and the entity types, the same for every client), so one translation serves every
    /// receiver of a shared packet.
    fn stateless(&self, state: State, id: i32) -> bool;

    /// The older id of a newer entity type id.
    fn entity_type_older(&self, new: i32) -> Option<i32>;
}

/// Most entities a connection keeps the type of (more than any client sees at once).
const MAX_ENTITIES: usize = 1 << 15;

/// What a step may ask about the connection.
pub(crate) struct Ctx<'a> {
    /// Entity id → entity type id of the server's version.
    entities: &'a HashMap<i32, i32>,
    /// The steps between the server and this step (oldest first).
    newer: &'a [Box<dyn ClientboundStep>],
}

impl Ctx<'_> {
    /// The type of an entity the client knows, in the numbering of the step's newer version.
    pub(crate) fn entity_type(&self, entity: i32) -> Option<i32> {
        let mut t = *self.entities.get(&entity)?;
        for step in self.newer.iter().rev() {
            t = step.entity_type_older(t)?;
        }
        Some(t)
    }
}

/// One step's serverbound side: older packets → newer packets.
pub(crate) trait ServerboundStep: Send + std::fmt::Debug {
    /// Rewrites one packet body of `state` into `out`. `false` drops it. Packets the newer
    /// version expects in addition (after this one) go to `extra`, in the newer version.
    fn translate(
        &mut self,
        state: State,
        body: &[u8],
        out: &mut Vec<u8>,
        extra: &mut Vec<Vec<u8>>,
    ) -> Result<bool>;
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
        let mut cbs = Vec::new();
        let mut sbs = Vec::new();
        for t in crate::STEPS {
            if t.protocol >= version.protocol {
                let (cb, sb) = crate::step::open(t);
                cbs.push(cb);
                sbs.push(sb);
            }
        }
        // The newest step keeps every item component for its own clients; below it only the
        // ones the older steps translate survive.
        let (cb, sb) = crate::v26_2::open(!cbs.is_empty());
        cbs.push(cb);
        sbs.push(sb);
        Self {
            serverbound: ServerboundHalf {
                version,
                state,
                steps: sbs,
                bufs: [Vec::new(), Vec::new()],
                last: 0,
                held: Vec::new(),
                deferred: Vec::new(),
                extra: Vec::new(),
                pending: std::collections::VecDeque::new(),
            },
            clientbound: ClientboundHalf {
                version,
                state,
                steps: cbs,
                bufs: [Vec::new(), Vec::new()],
                entities: HashMap::new(),
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
    /// Which buffer holds the last translation (`HELD`: `held`).
    last: usize,
    held: Vec<u8>,
    /// Packets steps added: the step to continue at and the body.
    deferred: Vec<(usize, Vec<u8>)>,
    extra: Vec<Vec<u8>>,
    /// Added packets, translated, waiting to be read.
    pending: std::collections::VecDeque<Vec<u8>>,
}

impl ServerboundHalf {
    /// The client's version.
    pub fn version(&self) -> Version {
        self.version
    }

    /// Translates one packet body from the client into the server's version. `None` drops it.
    /// Packets the translation adds come from [`pending`](Self::pending) afterwards.
    pub fn translate<'s>(&'s mut self, body: &[u8]) -> Result<Option<&'s [u8]>> {
        let ok = self.run(0, body)?;
        if self.deferred.is_empty() {
            return Ok(ok.then(|| self.translated()));
        }
        // Rare: a step added packets. They follow the translated one.
        let main = ok.then(|| self.translated().to_vec());
        while let Some((from, extra)) = self.deferred.pop() {
            if self.run(from, &extra)? {
                let t = self.translated().to_vec();
                self.pending.push_back(t);
            }
        }
        match main {
            Some(main) => {
                self.held = main;
                self.last = HELD;
                Ok(Some(self.translated()))
            }
            None => Ok(self.pending()),
        }
    }

    /// The next packet a translation added, if any. Call before reading the next packet.
    pub fn pending(&mut self) -> Option<&[u8]> {
        self.held = self.pending.pop_front()?;
        self.last = HELD;
        Some(self.translated())
    }

    /// Runs `body` through the steps from `from`; the result is [`translated`](Self::translated).
    fn run(&mut self, from: usize, body: &[u8]) -> Result<bool> {
        let [a, b] = &mut self.bufs;
        let (mut input, mut output) = (a, b);
        input.clear();
        input.extend_from_slice(body);
        let mut in_a = true;
        for (k, step) in self.steps.iter_mut().enumerate().skip(from) {
            output.clear();
            let keep = step.translate(self.state, input, output, &mut self.extra)?;
            for e in self.extra.drain(..).rev() {
                self.deferred.push((k.saturating_add(1), e));
            }
            if !keep {
                return Ok(false);
            }
            std::mem::swap(&mut input, &mut output);
            in_a = !in_a;
        }
        let id = packet_id(input)?;
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
        self.last = usize::from(!in_a);
        Ok(true)
    }

    /// The result of the last successful [`translate`](Self::translate).
    pub fn translated(&self) -> &[u8] {
        match self.last {
            HELD => &self.held,
            i => self.bufs.get(i).map_or(&[], Vec::as_slice),
        }
    }
}

/// [`ServerboundHalf::last`]: the translation is in `held`.
const HELD: usize = 2;

/// Translates what the server sends.
#[derive(Debug)]
pub struct ClientboundHalf {
    version: Version,
    state: State,
    /// Oldest step first; clientbound packets run through them newest first.
    steps: Vec<Box<dyn ClientboundStep>>,
    bufs: [Vec<u8>; 2],
    /// Entity id → type (server's version) of the entities the client was sent.
    entities: HashMap<i32, i32>,
}

impl ClientboundHalf {
    /// The client's version.
    pub fn version(&self) -> Version {
        self.version
    }

    /// Translates one packet body from the server into the client's version. `None` drops it.
    pub fn translate<'s>(&'s mut self, body: &[u8]) -> Result<Option<&'s [u8]>> {
        self.track(body);
        self.run(body)
    }

    /// Follows the entity types the client knows.
    fn track(&mut self, body: &[u8]) {
        use ids::play::clientbound as cb;
        if self.state != State::Play {
            return;
        }
        let mut r = Reader::new(body);
        let Ok(id) = r.varint() else { return };
        let _ = (|| -> Result<()> {
            match id {
                cb::ADD_ENTITY => {
                    let entity = r.varint()?;
                    r.skip(16)?;
                    let kind = r.varint()?;
                    if self.entities.len() >= MAX_ENTITIES {
                        self.entities.clear();
                    }
                    self.entities.insert(entity, kind);
                }
                cb::REMOVE_ENTITIES => {
                    let n = r.len_prefix("entities", 1 << 16, 1)?;
                    for _ in 0..n {
                        self.entities.remove(&r.varint()?);
                    }
                }
                cb::LOGIN => {
                    let entity = r.i32()?;
                    self.entities.clear();
                    self.entities.insert(entity, crate::player_type());
                }
                _ => {}
            }
            Ok(())
        })();
    }

    fn run<'s>(&'s mut self, body: &[u8]) -> Result<Option<&'s [u8]>> {
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
        for k in (0..self.steps.len()).rev() {
            let (lower, newer) = self.steps.split_at_mut(k.saturating_add(1));
            let Some(step) = lower.last_mut() else { break };
            let ctx = Ctx {
                entities: &self.entities,
                newer,
            };
            output.clear();
            if !step.translate(state, input, output, &ctx)? {
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
        self.track(body);
        if cacheable {
            let protocol = self.version.protocol;
            if let Some(cached) = packet.translated(protocol, |body| {
                self.run(body).ok().flatten().map(<[u8]>::to_vec)
            }) {
                return cached;
            }
        }
        match self.run(body) {
            Ok(out) => out,
            Err(e) => {
                log::debug!("multiversion: dropped an untranslatable packet: {e}");
                None
            }
        }
    }
}
