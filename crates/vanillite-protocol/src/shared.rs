//! Packets encoded once for many receivers, and the inbound play packet envelope.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, TryRecvError};
use std::sync::{Arc, OnceLock};

use crate::Packet;
use crate::frame::encode_body;
use crate::sets::ServerboundPlay;

/// A packet encoded once for many receivers (broadcasts, chunk packets, configuration data).
/// Its compressed frame is built by the first writer that needs it and reused by all others
/// with the same compression threshold. Clients of an older version (multi-version layer) get
/// a translated body, likewise built once per version by the first writer that needs it (up to
/// [`TRANSLATIONS`] versions; the slots are allocated by the first translation).
#[derive(Debug)]
pub struct SharedPacket {
    body: Vec<u8>,
    frame: OnceLock<(Option<usize>, Vec<u8>)>,
    translated: OnceLock<Box<[Translation; TRANSLATIONS]>>,
}

/// How many client versions one shared packet caches translations for.
pub const TRANSLATIONS: usize = 12;

type Translation = OnceLock<(i32, Option<Box<[u8]>>)>;

impl SharedPacket {
    /// Wraps an encoded body (`id` + fields).
    pub fn new(body: Vec<u8>) -> Self {
        Self {
            body,
            frame: OnceLock::new(),
            translated: OnceLock::new(),
        }
    }

    /// Encodes a packet.
    pub fn of<P: Packet>(packet: &P) -> Arc<Self> {
        Arc::new(Self::new(encode_body(packet)))
    }

    /// The encoded body.
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// The cached frame for `threshold`, built with `build` by the first caller. Returns
    /// `None` when the cache holds a frame for another threshold.
    pub fn frame(
        &self,
        threshold: Option<usize>,
        build: impl FnOnce(&[u8]) -> Vec<u8>,
    ) -> Option<&[u8]> {
        let (t, frame) = self.frame.get_or_init(|| (threshold, build(&self.body)));
        (*t == threshold).then_some(frame.as_slice())
    }

    /// The cached translation of the body for clients of protocol `version`, built with
    /// `build` by the first caller (`None` = the packet is dropped for them). Returns `None`
    /// when the cache is full with the translations for other versions.
    pub fn translated(
        &self,
        version: i32,
        build: impl FnOnce(&[u8]) -> Option<Vec<u8>>,
    ) -> Option<Option<&[u8]>> {
        let slots = self
            .translated
            .get_or_init(|| Box::new(std::array::from_fn(|_| OnceLock::new())));
        let mut build = Some(build);
        for slot in slots.iter() {
            // Only the caller that fills an empty slot builds, so a slot of another version
            // leaves `build` for the next one.
            let (v, body) = slot.get_or_init(|| {
                let body = build.take().and_then(|b| b(&self.body));
                (version, body.map(Vec::into_boxed_slice))
            });
            if *v == version {
                return Some(body.as_deref());
            }
        }
        None
    }
}

/// A validated serverbound play packet with its sequence number on its connection.
#[derive(Debug, Clone, PartialEq)]
pub struct Inbound {
    pub seq: u64,
    pub packet: ServerboundPlay,
}

/// Creates the queue of one connection's play packets for the world. At most `max` packets
/// wait (SEC-17); the memory follows the packets that wait (blocks of a few dozen), not
/// `max`, so thousands of idle connections stay small.
pub fn inbound_queue(max: usize) -> (InboundSender, InboundReceiver) {
    let (tx, rx) = mpsc::channel();
    let queued = Arc::new(AtomicUsize::new(0));
    (
        InboundSender {
            tx,
            queued: Arc::clone(&queued),
            max,
        },
        InboundReceiver { rx, queued },
    )
}

/// The network side of an [`inbound_queue`].
#[derive(Debug)]
pub struct InboundSender {
    tx: mpsc::Sender<Inbound>,
    queued: Arc<AtomicUsize>,
    max: usize,
}

impl InboundSender {
    /// Queues a packet; gives it back if `max` packets wait or the receiver is gone.
    pub fn try_send(&self, inbound: Inbound) -> Result<(), Inbound> {
        if self.queued.fetch_add(1, Ordering::Relaxed) >= self.max {
            self.queued.fetch_sub(1, Ordering::Relaxed);
            return Err(inbound);
        }
        self.tx.send(inbound).map_err(|e| {
            self.queued.fetch_sub(1, Ordering::Relaxed);
            e.0
        })
    }
}

/// The world side of an [`inbound_queue`].
#[derive(Debug)]
pub struct InboundReceiver {
    rx: mpsc::Receiver<Inbound>,
    queued: Arc<AtomicUsize>,
}

impl InboundReceiver {
    /// The next packet, like [`mpsc::Receiver::try_recv`].
    pub fn try_recv(&self) -> Result<Inbound, TryRecvError> {
        let inbound = self.rx.try_recv()?;
        self.queued.fetch_sub(1, Ordering::Relaxed);
        Ok(inbound)
    }
}
