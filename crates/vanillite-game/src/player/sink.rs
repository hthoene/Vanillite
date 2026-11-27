//! The game's side of a player connection: a sink for clientbound packets.

use std::sync::Arc;

use vanillite_protocol::frame::{encode_body, for_each_body};
use vanillite_protocol::packets::play::clientbound::Disconnect;
use vanillite_protocol::tap::BatchMark;
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
    /// Queues many packets as one entry: frames built with
    /// [`vanillite_protocol::frame::encode_frame`] for `threshold`, best the one of
    /// [`PacketSink::frame_threshold`]. The default queues the bodies one by one. Packet
    /// listeners of plugins see a batch's packets per receiver only: whoever encodes them for
    /// many receivers runs the broadcast stage ([`vanillite_protocol::tap::PacketTap::shared`]).
    /// `mark` tells the receiver stage whether it has to look into the batch
    /// ([`vanillite_protocol::tap::PacketTap::batch_mark`]).
    fn send_batch(&self, frames: Vec<u8>, threshold: Option<usize>, mark: BatchMark) -> bool {
        let _ = mark;
        let mut open = true;
        let _ = for_each_body(&frames, threshold, |body| {
            open = self.send_body(body.to_vec());
            Ok(open)
        });
        open
    }

    /// Queues an encoded packet body past the packet listeners of plugins (a packet a plugin
    /// sends itself). The default has no listeners to pass.
    fn send_untapped(&self, body: Vec<u8>) -> bool {
        self.send_body(body)
    }

    /// Feeds a serverbound play packet body into the connection as if the client sent it
    /// (packet listeners, validation and rate limits included; past the listeners with
    /// `skip_listeners`). Returns `false` if the connection is gone, too many fed packets wait,
    /// or the sink has no client to feed.
    fn inject(&self, body: Vec<u8>, skip_listeners: bool) -> bool {
        let _ = (body, skip_listeners);
        false
    }

    /// The compression threshold batches for this sink are best framed with: then they are
    /// written without re-framing.
    fn frame_threshold(&self) -> Option<usize> {
        None
    }

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
