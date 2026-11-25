//! Packets encoded once for many receivers, and the inbound play packet envelope.

use std::sync::{Arc, OnceLock};

use crate::Packet;
use crate::frame::encode_body;
use crate::sets::ServerboundPlay;

/// A packet encoded once for many receivers (broadcasts, chunk packets, configuration data).
/// Its compressed frame is built by the first writer that needs it and reused by all others
/// with the same compression threshold. Clients of an older version (multi-version layer) get
/// a translated body, likewise built once per version by the first writer that needs it.
#[derive(Debug)]
pub struct SharedPacket {
    body: Vec<u8>,
    frame: OnceLock<(Option<usize>, Vec<u8>)>,
    translated: OnceLock<(i32, Option<Box<[u8]>>)>,
}

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
    /// when the cache holds the translation for another version.
    pub fn translated(
        &self,
        version: i32,
        build: impl FnOnce(&[u8]) -> Option<Vec<u8>>,
    ) -> Option<Option<&[u8]>> {
        let (v, body) = self
            .translated
            .get_or_init(|| (version, build(&self.body).map(Vec::into_boxed_slice)));
        (*v == version).then_some(body.as_deref())
    }
}

/// A validated serverbound play packet with its sequence number on its connection.
#[derive(Debug, Clone, PartialEq)]
pub struct Inbound {
    pub seq: u64,
    pub packet: ServerboundPlay,
}
