//! What the server receives when a connection finishes configuration and enters play.

use crate::outbound::Outbound;
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use tokio::sync::Notify;

use vanillite_protocol::packets::common::ClientInfo;
use vanillite_protocol::types::GameProfile;

/// A player connection handed to the server. Dropping `inbound`'s sender (the connection
/// ending) shows up as a disconnected channel.
#[derive(Debug)]
pub struct Joined {
    /// Authenticated (or vouched-for) profile.
    pub profile: GameProfile,
    /// Settings the client sent during configuration.
    pub client_info: ClientInfo,
    /// Remote address.
    pub address: SocketAddr,
    /// Whether this is a virtual connection (in-memory, Bedrock, proxy edge).
    pub is_virtual: bool,
    /// An in-process client that takes all pending chunks at once (Vanilla's
    /// `memoryConnection`). Virtual connections of remote clients (Bedrock) are not.
    pub memory: bool,
    /// Validated play packets, bounded (SEC-17); drained by the world between ticks.
    pub inbound: vanillite_protocol::InboundReceiver,
    /// Packets to the client.
    pub outbound: Outbound,
    /// Smoothed keepalive round trip.
    pub latency: Latency,
    /// Feeds packets into the connection as if the client sent them.
    pub injector: Injector,
}

/// Feeds play packets into a connection as if its client sent them (a plugin's
/// `packet_receive`, ProtocolLib's `receiveClientPacket`). The connection's read loop takes
/// them between the client's packets: they pass the packet listeners (unless skipped), the
/// validating decoder and the rate limits like the client's own, and reach the world through
/// the one inbound queue, so the world still sees the connection end when the queue closes.
/// Cheap to clone; holds the connection only weakly.
#[derive(Debug, Clone, Default)]
pub struct Injector(Weak<Injected>);

/// The packets fed into one connection, waiting for its read loop.
#[derive(Debug)]
pub(crate) struct Injected {
    /// Bodies (`id` + fields, the server's protocol version) and whether they skip the
    /// packet listeners.
    queue: Mutex<VecDeque<(Vec<u8>, bool)>>,
    /// Wakes the read loop.
    pub(crate) ready: Notify,
    /// At most this many wait (SEC-17).
    max: usize,
}

impl Injected {
    pub(crate) fn new(max: usize) -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(VecDeque::new()),
            ready: Notify::new(),
            max: max.max(1),
        })
    }

    pub(crate) fn injector(self: &Arc<Self>) -> Injector {
        Injector(Arc::downgrade(self))
    }

    /// The next packet fed in, if any.
    pub(crate) fn take(&self) -> Option<(Vec<u8>, bool)> {
        self.queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
    }
}

impl Injector {
    /// Queues a play packet body as if the client sent it; `skip_listeners` lets it pass the
    /// packet listeners. Returns false when the connection is gone or too many packets wait.
    pub fn inject(&self, body: Vec<u8>, skip_listeners: bool) -> bool {
        let Some(injected) = self.0.upgrade() else {
            return false;
        };
        {
            let mut queue = injected
                .queue
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if queue.len() >= injected.max {
                return false;
            }
            queue.push_back((body, skip_listeners));
        }
        injected.ready.notify_one();
        true
    }
}

/// Keepalive latency in milliseconds, updated by the network thread.
#[derive(Debug, Clone, Default)]
pub struct Latency(pub(crate) Arc<AtomicU32>);

impl Latency {
    /// Current value in milliseconds.
    pub fn millis(&self) -> u32 {
        self.0.load(Ordering::Relaxed)
    }
}
