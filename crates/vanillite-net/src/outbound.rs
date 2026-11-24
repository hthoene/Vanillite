//! The outbound side of a connection: an unbounded channel bounded by bytes (SEC-17) and the
//! writer task that frames, compresses, encrypts and writes with one syscall per batch.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use vanillite_multiversion::ClientboundHalf;
use vanillite_protocol::compress::Codec;
use vanillite_protocol::frame::{FrameEncoder, encode_body, encode_frame, for_each_body};
use vanillite_protocol::{Packet, SharedPacket};

/// A message to a connection's writer.
#[derive(Debug)]
pub(crate) enum Out {
    /// An encoded packet body owned by this connection.
    Body(Vec<u8>),
    /// A shared packet.
    Shared(Arc<SharedPacket>),
    /// Frames built with [`encode_frame`] for a compression threshold.
    Batch(Vec<u8>, Option<usize>),
    /// Compression on from here (sent after `login_compression`).
    Compression(i32),
    /// Encryption on from here (after the key exchange).
    Encryption([u8; 16]),
    /// Translate every packet from here for a client of an older version.
    Adapter(Box<ClientboundHalf>),
    /// Flush and close the connection.
    Close,
}

/// Sends packets to one connection. Cheap to clone; usable from any thread.
#[derive(Debug, Clone)]
pub struct Outbound {
    tx: mpsc::UnboundedSender<Out>,
    queued: Arc<AtomicUsize>,
    max_bytes: usize,
    closed: Arc<AtomicBool>,
    /// The last compression threshold queued ([`NO_COMPRESSION`] = off).
    threshold: Arc<AtomicUsize>,
}

/// [`Outbound::threshold`] value for compression off.
const NO_COMPRESSION: usize = usize::MAX;

impl Outbound {
    pub(crate) fn new(max_bytes: usize) -> (Self, mpsc::UnboundedReceiver<Out>, Arc<AtomicUsize>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let queued = Arc::new(AtomicUsize::new(0));
        (
            Self {
                tx,
                queued: Arc::clone(&queued),
                max_bytes,
                closed: Arc::new(AtomicBool::new(false)),
                threshold: Arc::new(AtomicUsize::new(NO_COMPRESSION)),
            },
            rx,
            queued,
        )
    }

    fn push(&self, msg: Out, bytes: usize) -> bool {
        if self.closed.load(Ordering::Relaxed) {
            return false;
        }
        let total = self
            .queued
            .fetch_add(bytes, Ordering::Relaxed)
            .saturating_add(bytes);
        if total > self.max_bytes {
            // The client does not read fast enough: drop it instead of buffering (SEC-17).
            self.close();
            return false;
        }
        self.tx.send(msg).is_ok()
    }

    /// Queues a typed packet. Returns false if the connection is closed or overloaded.
    pub fn send<P: Packet>(&self, packet: &P) -> bool {
        let body = encode_body(packet);
        let n = body.len();
        self.push(Out::Body(body), n)
    }

    /// Queues an encoded body.
    pub fn send_body(&self, body: Vec<u8>) -> bool {
        let n = body.len();
        self.push(Out::Body(body), n)
    }

    /// Queues many packets as one entry: frames built with [`encode_frame`] for `threshold`
    /// (best this connection's, [`Outbound::threshold`]).
    pub fn send_batch(&self, frames: Vec<u8>, threshold: Option<usize>) -> bool {
        let n = frames.len();
        self.push(Out::Batch(frames, threshold), n)
    }

    /// The compression threshold the connection's writer uses once everything queued so far
    /// is written.
    pub fn threshold(&self) -> Option<usize> {
        let t = self.threshold.load(Ordering::Relaxed);
        (t != NO_COMPRESSION).then_some(t)
    }

    /// Queues a shared packet.
    pub fn send_shared(&self, packet: &Arc<SharedPacket>) -> bool {
        self.push(Out::Shared(Arc::clone(packet)), packet.body().len())
    }

    pub(crate) fn control(&self, msg: Out) -> bool {
        if let Out::Compression(t) = msg {
            let t = usize::try_from(t).unwrap_or(NO_COMPRESSION);
            self.threshold.store(t, Ordering::Relaxed);
        }
        self.push(msg, 0)
    }

    /// Flushes what is queued and closes the connection.
    pub fn close(&self) {
        if !self.closed.swap(true, Ordering::Relaxed) {
            let _ = self.tx.send(Out::Close);
        }
    }

    /// Whether the connection was closed.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed) || self.tx.is_closed()
    }
}

/// Writes to a TCP socket.
pub(crate) async fn tcp_writer(
    mut socket: tokio::net::tcp::OwnedWriteHalf,
    mut rx: mpsc::UnboundedReceiver<Out>,
    queued: Arc<AtomicUsize>,
    compression_level: i32,
) {
    let mut encoder = FrameEncoder::new();
    let mut codec = Codec::new(compression_level);
    let mut buf = Vec::with_capacity(8192);
    let mut open = true;
    let mut adapter: Option<Box<ClientboundHalf>> = None;
    while open {
        let Some(first) = rx.recv().await else { break };
        let mut next = Some(first);
        while let Some(msg) = next.take() {
            let consumed = match msg {
                Out::Body(body) if adapter.is_some() => {
                    if let Some(a) = adapter.as_mut() {
                        match a.translate(&body) {
                            Ok(Some(t)) => {
                                if encoder.body(t, &mut buf).is_err() {
                                    open = false;
                                }
                            }
                            Ok(None) => {}
                            Err(e) => {
                                log::debug!("multiversion: untranslatable packet: {e}");
                            }
                        }
                    }
                    body.len()
                }
                Out::Shared(p) if adapter.is_some() => {
                    if let Some(t) = adapter.as_mut().and_then(|a| a.translate_shared(&p))
                        && encoder.body(t, &mut buf).is_err()
                    {
                        open = false;
                    }
                    p.body().len()
                }
                Out::Body(body) => {
                    if encoder.body(&body, &mut buf).is_err() {
                        open = false;
                    }
                    body.len()
                }
                Out::Batch(frames, threshold) if adapter.is_some() => {
                    let res = for_each_body(&frames, threshold, |body| {
                        if let Some(a) = adapter.as_mut() {
                            match a.translate(body) {
                                Ok(Some(t)) => encoder.body(t, &mut buf)?,
                                Ok(None) => {}
                                Err(e) => log::debug!("multiversion: untranslatable packet: {e}"),
                            }
                        }
                        Ok(true)
                    });
                    if res.is_err() {
                        open = false;
                    }
                    frames.len()
                }
                Out::Batch(frames, threshold) => {
                    if encoder.batch(&frames, threshold, &mut buf).is_err() {
                        open = false;
                    }
                    frames.len()
                }
                Out::Shared(p) => {
                    let threshold = encoder.threshold();
                    let cached = p.frame(threshold, |body| {
                        let mut f = Vec::new();
                        // An unencodable body yields an empty frame that is skipped below.
                        let _ = encode_frame(body, threshold, &mut codec, &mut f);
                        f
                    });
                    match cached {
                        Some(frame) if !frame.is_empty() => encoder.frame(frame, &mut buf),
                        _ => {
                            if encoder.body(p.body(), &mut buf).is_err() {
                                open = false;
                            }
                        }
                    }
                    p.body().len()
                }
                Out::Compression(t) => {
                    encoder.set_compression(t);
                    0
                }
                Out::Encryption(secret) => {
                    encoder.enable_encryption(&secret);
                    0
                }
                Out::Adapter(a) => {
                    adapter = Some(a);
                    0
                }
                Out::Close => {
                    open = false;
                    0
                }
            };
            queued.fetch_sub(consumed, Ordering::Relaxed);
            if open && buf.len() < 256 * 1024 {
                next = rx.try_recv().ok();
            }
        }
        if !buf.is_empty() {
            if socket.write_all(&buf).await.is_err() {
                break;
            }
            buf.clear();
        }
    }
    let _ = socket.shutdown().await;
}

/// Forwards to a virtual connection: plain packet bodies, no framing, compression or cipher.
pub(crate) async fn virtual_writer(
    tx: mpsc::Sender<Vec<u8>>,
    mut rx: mpsc::UnboundedReceiver<Out>,
    queued: Arc<AtomicUsize>,
) {
    let mut adapter: Option<Box<ClientboundHalf>> = None;
    while let Some(msg) = rx.recv().await {
        let (body, queued_len) = match msg {
            Out::Batch(frames, threshold) => {
                queued.fetch_sub(frames.len(), Ordering::Relaxed);
                let mut bodies = Vec::new();
                if for_each_body(&frames, threshold, |body| {
                    match adapter.as_mut() {
                        Some(a) => match a.translate(body) {
                            Ok(Some(t)) => bodies.push(t.to_vec()),
                            Ok(None) => {}
                            Err(e) => log::debug!("multiversion: untranslatable packet: {e}"),
                        },
                        None => bodies.push(body.to_vec()),
                    }
                    Ok(true)
                })
                .is_err()
                {
                    break;
                }
                for body in bodies {
                    if tx.send(body).await.is_err() {
                        return;
                    }
                }
                continue;
            }
            Out::Body(b) => match adapter.as_mut() {
                Some(a) => {
                    let n = b.len();
                    let t = a.translate(&b).unwrap_or_else(|e| {
                        log::debug!("multiversion: untranslatable packet: {e}");
                        None
                    });
                    (t.map(<[u8]>::to_vec), n)
                }
                None => {
                    let n = b.len();
                    (Some(b), n)
                }
            },
            Out::Shared(p) => match adapter.as_mut() {
                Some(a) => (a.translate_shared(&p).map(<[u8]>::to_vec), p.body().len()),
                None => (Some(p.body().to_vec()), p.body().len()),
            },
            Out::Adapter(a) => {
                adapter = Some(a);
                continue;
            }
            Out::Compression(_) | Out::Encryption(_) => continue,
            Out::Close => break,
        };
        queued.fetch_sub(queued_len, Ordering::Relaxed);
        if let Some(body) = body
            && tx.send(body).await.is_err()
        {
            break;
        }
    }
}
