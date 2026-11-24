//! What the server receives when a connection finishes configuration and enters play.

use crate::outbound::Outbound;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use vanillite_protocol::Inbound;
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
    /// Validated play packets, bounded (SEC-17); drained by the world between ticks.
    pub inbound: std::sync::mpsc::Receiver<Inbound>,
    /// Packets to the client.
    pub outbound: Outbound,
    /// Smoothed keepalive round trip.
    pub latency: Latency,
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
