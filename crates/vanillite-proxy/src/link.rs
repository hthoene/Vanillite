//! One side of a relayed connection: a read half with its frame decoder and a write half that
//! collects framed output and flushes it with one write. Buffers are reused, so relaying a
//! packet allocates nothing.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use vanillite_protocol::frame::{FrameDecoder, FrameEncoder};
use vanillite_protocol::{Packet, State};

/// Bytes read from the socket at once.
const READ_CHUNK: usize = 16 * 1024;

/// Reading side.
#[derive(Debug)]
pub(crate) struct Rx {
    read: OwnedReadHalf,
    pub(crate) decoder: FrameDecoder,
    buf: Box<[u8]>,
}

impl Rx {
    /// Reads what the socket has (cancel-safe); `false` at the end of the stream.
    pub(crate) async fn fill(&mut self) -> bool {
        match self.read.read(&mut self.buf).await {
            Ok(0) | Err(_) => false,
            Ok(n) => {
                self.decoder.push(self.buf.get(..n).unwrap_or_default());
                true
            }
        }
    }

    /// Sets the frame limits of a state (SEC-05).
    pub(crate) fn set_state(&mut self, state: State) {
        self.decoder.set_limits(state.limits());
    }
}

/// Writing side.
#[derive(Debug)]
pub(crate) struct Tx {
    write: OwnedWriteHalf,
    pub(crate) encoder: FrameEncoder,
    out: Vec<u8>,
}

impl Tx {
    /// Queues a typed packet.
    pub(crate) fn send<P: Packet>(&mut self, packet: &P) -> vanillite_protocol::Result<()> {
        self.encoder.packet(packet, &mut self.out)
    }

    /// Queues an encoded packet (`id` + body).
    pub(crate) fn body(&mut self, body: &[u8]) -> vanillite_protocol::Result<()> {
        self.encoder.body(body, &mut self.out)
    }

    /// Writes everything queued; fails if the peer does not read within `timeout` (SEC-17).
    pub(crate) async fn flush(&mut self, timeout: Duration) -> std::io::Result<()> {
        if self.out.is_empty() {
            return Ok(());
        }
        let r = tokio::time::timeout(timeout, self.write.write_all(&self.out)).await;
        self.out.clear();
        if self.out.capacity() > 1 << 20 {
            self.out.shrink_to(64 * 1024);
        }
        match r {
            Ok(r) => r,
            Err(_) => Err(std::io::ErrorKind::TimedOut.into()),
        }
    }
}

/// Splits a connected socket into both sides, starting in the handshake state.
pub(crate) fn split(stream: TcpStream, level: i32) -> (Rx, Tx) {
    let _ = stream.set_nodelay(true);
    let (read, write) = stream.into_split();
    let mut encoder = FrameEncoder::new();
    encoder.set_compression_level(level);
    (
        Rx {
            read,
            decoder: FrameDecoder::new(),
            buf: vec![0; READ_CHUNK].into_boxed_slice(),
        },
        Tx {
            write,
            encoder,
            out: Vec::with_capacity(READ_CHUNK),
        },
    )
}
