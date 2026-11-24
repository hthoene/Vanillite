//! One side of a relayed connection: a read half with its frame decoder and a write half that
//! collects framed output and flushes it with one write. Buffers are reused, so relaying a
//! packet allocates nothing.
//!
//! A client side can also be a virtual connection (architecture 14.1, the Bedrock frontend):
//! packet bodies over channels. Its bodies pass the same frame decoder and per-state limits as
//! a socket's (SEC-05); compression and encryption are never enabled on it.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::mpsc;
use vanillite_protocol::codec::{Reader, write_varint};
use vanillite_protocol::frame::{FrameDecoder, FrameEncoder};
use vanillite_protocol::{Packet, State};

/// Bytes read from the socket at once.
const READ_CHUNK: usize = 16 * 1024;

/// Bodies a virtual connection takes from its channel per read.
const VIRTUAL_BATCH: usize = 256;

/// Where a side reads from.
#[derive(Debug)]
enum Source {
    Socket(OwnedReadHalf),
    /// Packet bodies of a virtual connection.
    Virtual(mpsc::Receiver<Vec<u8>>),
}

/// Where a side writes to.
#[derive(Debug)]
enum Sink {
    Socket(OwnedWriteHalf),
    /// Packet bodies of a virtual connection.
    Virtual(mpsc::Sender<Vec<u8>>),
}

/// Reading side.
#[derive(Debug)]
pub(crate) struct Rx {
    read: Source,
    pub(crate) decoder: FrameDecoder,
    buf: Box<[u8]>,
}

impl Rx {
    /// Reads what the socket has (cancel-safe); `false` at the end of the stream.
    pub(crate) async fn fill(&mut self) -> bool {
        match &mut self.read {
            Source::Socket(read) => match read.read(&mut self.buf).await {
                Ok(0) | Err(_) => false,
                Ok(n) => {
                    self.decoder.push(self.buf.get(..n).unwrap_or_default());
                    true
                }
            },
            Source::Virtual(rx) => {
                let Some(body) = rx.recv().await else {
                    return false;
                };
                push_body(&mut self.decoder, &body);
                for _ in 0..VIRTUAL_BATCH {
                    match rx.try_recv() {
                        Ok(body) => push_body(&mut self.decoder, &body),
                        Err(_) => break,
                    }
                }
                true
            }
        }
    }

    /// Sets the frame limits of a state (SEC-05).
    pub(crate) fn set_state(&mut self, state: State) {
        self.decoder.set_limits(state.limits());
    }
}

/// Frames a body (uncompressed) into the decoder, so it passes the frame limits.
fn push_body(decoder: &mut FrameDecoder, body: &[u8]) {
    let mut len = Vec::with_capacity(5);
    write_varint(&mut len, i32::try_from(body.len()).unwrap_or(i32::MAX));
    decoder.push(&len);
    decoder.push(body);
}

/// Writing side.
#[derive(Debug)]
pub(crate) struct Tx {
    write: Sink,
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
        let r = match &mut self.write {
            Sink::Socket(write) => tokio::time::timeout(timeout, write.write_all(&self.out)).await,
            Sink::Virtual(tx) => tokio::time::timeout(timeout, send_frames(tx, &self.out)).await,
        };
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

/// Sends the frames (uncompressed, unencrypted) of `out` as bodies.
async fn send_frames(tx: &mpsc::Sender<Vec<u8>>, out: &[u8]) -> std::io::Result<()> {
    let mut r = Reader::new(out);
    while r.remaining() > 0 {
        let len = r
            .varint()
            .ok()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or(std::io::ErrorKind::InvalidData)?;
        let body = r.take(len).map_err(|_| std::io::ErrorKind::InvalidData)?;
        tx.send(body.to_vec())
            .await
            .map_err(|_| std::io::ErrorKind::BrokenPipe)?;
    }
    Ok(())
}

/// Both sides of a virtual connection: `from_client` carries its bodies to the proxy,
/// `to_client` the proxy's bodies to it.
pub(crate) fn virtual_link(
    from_client: mpsc::Receiver<Vec<u8>>,
    to_client: mpsc::Sender<Vec<u8>>,
) -> (Rx, Tx) {
    (
        Rx {
            read: Source::Virtual(from_client),
            decoder: FrameDecoder::new(),
            buf: Box::default(),
        },
        Tx {
            write: Sink::Virtual(to_client),
            encoder: FrameEncoder::new(),
            out: Vec::with_capacity(4096),
        },
    )
}

/// Splits a connected socket into both sides, starting in the handshake state.
pub(crate) fn split(stream: TcpStream, level: i32) -> (Rx, Tx) {
    let _ = stream.set_nodelay(true);
    let (read, write) = stream.into_split();
    let mut encoder = FrameEncoder::new();
    encoder.set_compression_level(level);
    (
        Rx {
            read: Source::Socket(read),
            decoder: FrameDecoder::new(),
            buf: vec![0; READ_CHUNK].into_boxed_slice(),
        },
        Tx {
            write: Sink::Socket(write),
            encoder,
            out: Vec::with_capacity(READ_CHUNK),
        },
    )
}
