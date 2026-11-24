//! The game's side of a player connection: a sink for clientbound packets.

use std::sync::Arc;

use vanillite_protocol::frame::encode_body;
use vanillite_protocol::packets::play::clientbound::Disconnect;
use vanillite_protocol::text::Component;
use vanillite_protocol::{Packet, SharedPacket};

/// Where a player's clientbound packets go. The network layer implements it for its connections
/// (the game never depends on the network crate); tests implement it in memory. Sends never
/// block; a connection that cannot keep up is closed by the implementation (SEC-17).
pub trait PacketSink: Send + Sync {
    /// Queues an encoded packet body (`id` + fields). Returns `false` if the connection is gone.
    fn send_body(&self, body: Vec<u8>) -> bool;
    /// Queues a packet encoded once for many receivers.
    fn send_shared(&self, packet: &Arc<SharedPacket>) -> bool;
    /// Flushes what is queued and closes the connection.
    fn close(&self);
    /// Whether the connection is closed.
    fn is_closed(&self) -> bool;
    /// The smoothed keepalive round trip in milliseconds.
    fn latency_ms(&self) -> u32;
}

impl dyn PacketSink + '_ {
    /// Encodes and queues a packet.
    pub fn send<P: Packet>(&self, packet: &P) -> bool {
        self.send_body(encode_body(packet))
    }

    /// Sends the play `disconnect` packet and closes the connection.
    pub fn kick(&self, reason: Component) {
        self.send(&Disconnect { reason });
        self.close();
    }
}
