//! RakNet (protocol 11) as Bedrock uses it, own implementation.
//!
//! The listener answers unconnected pings and the open-connection handshake on one UDP socket
//! and hands datagrams of established peers to their connection task. Before a peer gets any
//! state it must echo a cookie (a keyed hash of its address that rotates), so spoofed source
//! addresses cannot allocate connections. [`RakConn`] is the per-peer reliability layer without
//! I/O: acknowledgements, resends, ordering, sequencing and split packets, all bounded.

use std::collections::VecDeque;
use std::hash::BuildHasher;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustc_hash::{FxHashMap, FxHashSet};

use crate::io::{Error, Reader, Result};

/// RakNet protocol version of current Bedrock clients.
pub const PROTOCOL: u8 = 11;
/// The "offline message" magic.
pub const MAGIC: [u8; 16] = [
    0x00, 0xff, 0xff, 0x00, 0xfe, 0xfe, 0xfe, 0xfe, 0xfd, 0xfd, 0xfd, 0xfd, 0x12, 0x34, 0x56, 0x78,
];

pub mod id {
    pub const CONNECTED_PING: u8 = 0x00;
    pub const UNCONNECTED_PING: u8 = 0x01;
    pub const UNCONNECTED_PING_OPEN: u8 = 0x02;
    pub const CONNECTED_PONG: u8 = 0x03;
    pub const OPEN_CONNECTION_REQUEST_1: u8 = 0x05;
    pub const OPEN_CONNECTION_REPLY_1: u8 = 0x06;
    pub const OPEN_CONNECTION_REQUEST_2: u8 = 0x07;
    pub const OPEN_CONNECTION_REPLY_2: u8 = 0x08;
    pub const CONNECTION_REQUEST: u8 = 0x09;
    pub const CONNECTION_REQUEST_ACCEPTED: u8 = 0x10;
    pub const NEW_INCOMING_CONNECTION: u8 = 0x13;
    pub const DISCONNECT: u8 = 0x15;
    pub const INCOMPATIBLE_PROTOCOL: u8 = 0x19;
    pub const UNCONNECTED_PONG: u8 = 0x1c;
    pub const GAME: u8 = 0xfe;
}

const FLAG_VALID: u8 = 0x80;
const FLAG_ACK: u8 = 0x40;
const FLAG_NAK: u8 = 0x20;
const FLAG_NEEDS_B_AND_AS: u8 = 0x04;
const FRAME_SPLIT: u8 = 0x10;

/// UDP + IPv4 header bytes counted in RakNet's MTU.
pub const UDP_OVERHEAD: usize = 28;
/// Smallest and largest MTU accepted.
pub const MIN_MTU: u16 = 400;
pub const MAX_MTU: u16 = 1492;
/// Datagram header (flags + sequence number).
const DATAGRAM_HEADER: usize = 4;
/// Largest frame header (reliable, ordered and split).
const MAX_FRAME_HEADER: usize = 23;
/// 24-bit index space.
const U24: u32 = 1 << 24;
const U24_MASK: u32 = U24 - 1;

/// Limits of one peer (SEC-17: everything a peer can make us store is bounded).
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Largest reassembled message (a game batch before decompression).
    pub max_message: usize,
    /// Parts of one split message.
    pub max_split_parts: u32,
    /// Split messages being reassembled at the same time.
    pub max_concurrent_splits: usize,
    /// Out-of-order frames buffered per peer.
    pub max_out_of_order: usize,
    /// Distance a reliable index may run ahead of the oldest missing one.
    pub reliable_window: u32,
    /// Bytes queued for sending before the peer counts as unable to keep up.
    pub max_send_queue: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_message: 4 << 20,
            max_split_parts: 8192,
            max_concurrent_splits: 8,
            max_out_of_order: 1024,
            reliable_window: 8192,
            max_send_queue: 32 << 20,
        }
    }
}

/// Reliability of a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reliability {
    Unreliable,
    ReliableOrdered,
}

impl Reliability {
    fn bits(self) -> u8 {
        match self {
            Self::Unreliable => 0,
            Self::ReliableOrdered => 3,
        }
    }
}

fn is_reliable(r: u8) -> bool {
    matches!(r, 2 | 3 | 4 | 6 | 7)
}

fn is_sequenced(r: u8) -> bool {
    matches!(r, 1 | 4)
}

fn is_ordered(r: u8) -> bool {
    matches!(r, 3 | 7)
}

/// `a - b` in the 24-bit index space; values of 2^23 and more mean `a` is older.
fn diff24(a: u32, b: u32) -> u32 {
    a.wrapping_sub(b) & U24_MASK
}

fn next24(a: u32) -> u32 {
    a.wrapping_add(1) & U24_MASK
}

fn read_u24(r: &mut Reader<'_>) -> Result<u32> {
    let [a, b, c] = r.array::<3>()?;
    Ok(u32::from_le_bytes([a, b, c, 0]))
}

fn write_u24(out: &mut Vec<u8>, v: u32) {
    let [a, b, c, _] = v.to_le_bytes();
    out.extend_from_slice(&[a, b, c]);
}

/// A shared slice of a message body (split parts of one message share the buffer).
#[derive(Debug, Clone)]
struct Body {
    buf: Arc<[u8]>,
    start: usize,
    end: usize,
}

impl Body {
    fn bytes(&self) -> &[u8] {
        self.buf.get(self.start..self.end).unwrap_or_default()
    }
}

#[derive(Debug, Clone)]
struct Frame {
    reliability: u8,
    reliable_index: u32,
    sequence_index: u32,
    order_index: u32,
    split: Option<(u32, u16, u32)>,
    body: Body,
}

impl Frame {
    fn wire_len(&self) -> usize {
        MAX_FRAME_HEADER.saturating_add(self.body.end.saturating_sub(self.body.start))
    }

    fn encode(&self, out: &mut Vec<u8>) {
        let r = self.reliability;
        let flags = r.wrapping_shl(5) | if self.split.is_some() { FRAME_SPLIT } else { 0 };
        out.push(flags);
        let len = self.body.end.saturating_sub(self.body.start);
        let bits = u16::try_from(len.saturating_mul(8)).unwrap_or(u16::MAX);
        out.extend_from_slice(&bits.to_be_bytes());
        if is_reliable(r) {
            write_u24(out, self.reliable_index);
        }
        if is_sequenced(r) {
            write_u24(out, self.sequence_index);
        }
        if is_sequenced(r) || is_ordered(r) {
            write_u24(out, self.order_index);
            out.push(0);
        }
        if let Some((count, id, index)) = self.split {
            out.extend_from_slice(&count.to_be_bytes());
            out.extend_from_slice(&id.to_be_bytes());
            out.extend_from_slice(&index.to_be_bytes());
        }
        out.extend_from_slice(self.body.bytes());
    }
}

#[derive(Debug)]
struct Split {
    parts: Vec<Option<Vec<u8>>>,
    received: u32,
    bytes: usize,
    /// Delivery data of the first part (all parts share it).
    reliability: u8,
    sequence_index: u32,
    order_index: u32,
    channel: u8,
}

#[derive(Debug)]
struct InFlight {
    frames: Vec<Frame>,
    sent: Instant,
}

/// Why a peer is dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RakError {
    /// Malformed data.
    Malformed(Error),
    /// A limit was exceeded.
    Limit(&'static str),
}

impl std::fmt::Display for RakError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(e) => write!(f, "malformed RakNet data: {e}"),
            Self::Limit(what) => write!(f, "RakNet limit exceeded: {what}"),
        }
    }
}

impl From<Error> for RakError {
    fn from(e: Error) -> Self {
        Self::Malformed(e)
    }
}

/// What a received datagram produced.
#[derive(Debug, Default)]
pub struct Received {
    /// Game batches (payloads after the `0xfe` id).
    pub game: Vec<Vec<u8>>,
    /// The peer completed the connection handshake.
    pub connected: bool,
    /// The peer disconnected.
    pub disconnected: bool,
}

/// The reliability layer of one peer.
#[derive(Debug)]
pub struct RakConn {
    peer: SocketAddr,
    mtu: usize,
    limits: Limits,
    // Receiving.
    acks: Vec<u32>,
    naks: Vec<u32>,
    highest_datagram: Option<u32>,
    reliable_base: u32,
    reliable_seen: FxHashSet<u32>,
    order_expected: [u32; 32],
    sequence_highest: [u32; 32],
    out_of_order: FxHashMap<(u8, u32), Vec<u8>>,
    splits: FxHashMap<u16, Split>,
    // Sending.
    next_datagram: u32,
    next_reliable: u32,
    next_order: u32,
    next_sequence: u32,
    next_split: u16,
    queue: VecDeque<Frame>,
    queued_bytes: usize,
    in_flight: FxHashMap<u32, InFlight>,
    srtt: Duration,
    /// Last time anything arrived.
    pub last_receive: Instant,
}

impl RakConn {
    /// A peer with the negotiated MTU.
    pub fn new(peer: SocketAddr, mtu: u16, limits: Limits, now: Instant) -> Self {
        Self {
            peer,
            mtu: usize::from(mtu.clamp(MIN_MTU, MAX_MTU)),
            limits,
            acks: Vec::new(),
            naks: Vec::new(),
            highest_datagram: None,
            reliable_base: 0,
            reliable_seen: FxHashSet::default(),
            order_expected: [0; 32],
            sequence_highest: [0; 32],
            out_of_order: FxHashMap::default(),
            splits: FxHashMap::default(),
            next_datagram: 0,
            next_reliable: 0,
            next_order: 0,
            next_sequence: 0,
            next_split: 0,
            queue: VecDeque::new(),
            queued_bytes: 0,
            in_flight: FxHashMap::default(),
            srtt: Duration::from_millis(100),
            last_receive: now,
        }
    }

    /// The negotiated MTU.
    pub fn mtu(&self) -> usize {
        self.mtu
    }

    /// Bytes waiting to be sent or acknowledged.
    pub fn backlog(&self) -> usize {
        self.queued_bytes
    }

    /// Whether the peer stopped acknowledging so much data that it must be dropped.
    pub fn overloaded(&self) -> bool {
        self.queued_bytes > self.limits.max_send_queue
    }

    /// Handles one datagram from the peer.
    pub fn receive(
        &mut self,
        data: &[u8],
        now: Instant,
    ) -> std::result::Result<Received, RakError> {
        let mut out = Received::default();
        let Some(&flags) = data.first() else {
            return Ok(out);
        };
        if flags & FLAG_VALID == 0 {
            return Ok(out);
        }
        self.last_receive = now;
        let mut r = Reader::new(data.get(1..).unwrap_or_default());
        if flags & FLAG_ACK != 0 {
            let mut acked = Vec::new();
            read_ranges(&mut r, &mut acked)?;
            for seq in acked {
                if let Some(f) = self.in_flight.remove(&seq) {
                    self.release(&f.frames);
                    let sample = now.saturating_duration_since(f.sent);
                    self.srtt = (self.srtt.saturating_mul(7).saturating_add(sample))
                        .checked_div(8)
                        .unwrap_or(self.srtt);
                }
            }
            return Ok(out);
        }
        if flags & FLAG_NAK != 0 {
            let mut lost = Vec::new();
            read_ranges(&mut r, &mut lost)?;
            for seq in lost {
                if let Some(f) = self.in_flight.remove(&seq) {
                    for frame in f.frames.into_iter().rev() {
                        self.queue.push_front(frame);
                    }
                }
            }
            return Ok(out);
        }
        let seq = read_u24(&mut r)?;
        self.track_datagram(seq);
        while r.remaining() > 0 {
            self.read_frame(&mut r, &mut out, now)?;
        }
        Ok(out)
    }

    fn track_datagram(&mut self, seq: u32) {
        if self.acks.len() < 8192 {
            self.acks.push(seq);
        }
        match self.highest_datagram {
            None => self.highest_datagram = Some(seq),
            Some(high) => {
                let ahead = diff24(seq, high);
                if ahead == 0 || ahead >= U24 / 2 {
                    // Duplicate or late: the gap it fills was already reported.
                    return;
                }
                let mut missing = next24(high);
                for _ in 1..ahead.min(64) {
                    if self.naks.len() < 1024 {
                        self.naks.push(missing);
                    }
                    missing = next24(missing);
                }
                self.highest_datagram = Some(seq);
            }
        }
    }

    fn read_frame(
        &mut self,
        r: &mut Reader<'_>,
        out: &mut Received,
        now: Instant,
    ) -> std::result::Result<(), RakError> {
        let flags = r.u8()?;
        let reliability = flags.wrapping_shr(5);
        let bits = u16::from_be_bytes(r.array()?);
        let len = usize::from(bits).div_ceil(8);
        let mut fresh = true;
        if is_reliable(reliability) {
            fresh = self.accept_reliable(read_u24(r)?)?;
        }
        let sequence_index = if is_sequenced(reliability) {
            read_u24(r)?
        } else {
            0
        };
        let (order_index, channel) = if is_sequenced(reliability) || is_ordered(reliability) {
            let index = read_u24(r)?;
            let channel = r.u8()?;
            if channel >= 32 {
                return Err(RakError::Malformed(Error::Invalid("order channel")));
            }
            (index, channel)
        } else {
            (0, 0)
        };
        let split = if flags & FRAME_SPLIT != 0 {
            Some((
                u32::from_be_bytes(r.array()?),
                u16::from_be_bytes(r.array()?),
                u32::from_be_bytes(r.array()?),
            ))
        } else {
            None
        };
        let body = r.take(len)?;
        if !fresh {
            return Ok(());
        }
        let (body, reliability, sequence_index, order_index, channel) = match split {
            None => (
                body.to_vec(),
                reliability,
                sequence_index,
                order_index,
                channel,
            ),
            Some((count, id, index)) => {
                let Some(done) = self.add_split(
                    count,
                    id,
                    index,
                    body,
                    (reliability, sequence_index, order_index, channel),
                )?
                else {
                    return Ok(());
                };
                done
            }
        };
        self.deliver(
            body,
            reliability,
            sequence_index,
            order_index,
            channel,
            out,
            now,
        )
    }

    /// Records a reliable index; `false` for a duplicate.
    fn accept_reliable(&mut self, index: u32) -> std::result::Result<bool, RakError> {
        let ahead = diff24(index, self.reliable_base);
        if ahead >= U24 / 2 {
            return Ok(false);
        }
        if ahead >= self.limits.reliable_window {
            return Err(RakError::Limit("reliable window"));
        }
        if ahead == 0 {
            self.reliable_base = next24(self.reliable_base);
            while self.reliable_seen.remove(&self.reliable_base) {
                self.reliable_base = next24(self.reliable_base);
            }
            return Ok(true);
        }
        Ok(self.reliable_seen.insert(index))
    }

    #[allow(clippy::type_complexity)]
    fn add_split(
        &mut self,
        count: u32,
        id: u16,
        index: u32,
        body: &[u8],
        delivery: (u8, u32, u32, u8),
    ) -> std::result::Result<Option<(Vec<u8>, u8, u32, u32, u8)>, RakError> {
        if count == 0 || count > self.limits.max_split_parts || index >= count {
            return Err(RakError::Limit("split parts"));
        }
        if !self.splits.contains_key(&id) && self.splits.len() >= self.limits.max_concurrent_splits
        {
            return Err(RakError::Limit("concurrent split packets"));
        }
        let slots = usize::try_from(count).map_err(|_| RakError::Limit("split parts"))?;
        let split = self.splits.entry(id).or_insert_with(|| Split {
            parts: vec![None; slots],
            received: 0,
            bytes: 0,
            reliability: delivery.0,
            sequence_index: delivery.1,
            order_index: delivery.2,
            channel: delivery.3,
        });
        if split.parts.len() != slots {
            return Err(RakError::Malformed(Error::Invalid("split count")));
        }
        let slot = split
            .parts
            .get_mut(usize::try_from(index).unwrap_or(usize::MAX))
            .ok_or(RakError::Limit("split index"))?;
        if slot.is_some() {
            return Ok(None);
        }
        split.bytes = split.bytes.saturating_add(body.len());
        if split.bytes > self.limits.max_message {
            return Err(RakError::Limit("split message size"));
        }
        *slot = Some(body.to_vec());
        split.received = split.received.saturating_add(1);
        if split.received < count {
            return Ok(None);
        }
        let Some(split) = self.splits.remove(&id) else {
            return Ok(None);
        };
        let mut whole = Vec::with_capacity(split.bytes);
        for part in split.parts.into_iter().flatten() {
            whole.extend_from_slice(&part);
        }
        Ok(Some((
            whole,
            split.reliability,
            split.sequence_index,
            split.order_index,
            split.channel,
        )))
    }

    #[allow(clippy::too_many_arguments)]
    fn deliver(
        &mut self,
        body: Vec<u8>,
        reliability: u8,
        sequence_index: u32,
        order_index: u32,
        channel: u8,
        out: &mut Received,
        now: Instant,
    ) -> std::result::Result<(), RakError> {
        let ch = usize::from(channel);
        if is_sequenced(reliability) {
            let expected = self.order_expected.get(ch).copied().unwrap_or(0);
            let highest = self.sequence_highest.get(ch).copied().unwrap_or(0);
            if diff24(order_index, expected) >= U24 / 2
                || diff24(sequence_index, highest) >= U24 / 2
            {
                return Ok(());
            }
            if let Some(h) = self.sequence_highest.get_mut(ch) {
                *h = next24(sequence_index);
            }
            return self.handle_message(body, out, now);
        }
        if !is_ordered(reliability) {
            return self.handle_message(body, out, now);
        }
        let expected = self.order_expected.get(ch).copied().unwrap_or(0);
        let ahead = diff24(order_index, expected);
        if ahead >= U24 / 2 {
            return Ok(());
        }
        if ahead > 0 {
            if self.out_of_order.len() >= self.limits.max_out_of_order {
                return Err(RakError::Limit("out-of-order frames"));
            }
            self.out_of_order.insert((channel, order_index), body);
            return Ok(());
        }
        self.handle_message(body, out, now)?;
        let mut next = next24(expected);
        while let Some(body) = self.out_of_order.remove(&(channel, next)) {
            self.handle_message(body, out, now)?;
            next = next24(next);
        }
        if let Some(e) = self.order_expected.get_mut(ch) {
            *e = next;
        }
        Ok(())
    }

    /// Handles a complete message: connection control here, game batches for the session.
    fn handle_message(
        &mut self,
        body: Vec<u8>,
        out: &mut Received,
        now: Instant,
    ) -> std::result::Result<(), RakError> {
        let Some(&kind) = body.first() else {
            return Ok(());
        };
        let mut r = Reader::new(body.get(1..).unwrap_or_default());
        match kind {
            id::GAME => {
                if body.len() > self.limits.max_message {
                    return Err(RakError::Limit("message size"));
                }
                let mut body = body;
                body.remove(0);
                out.game.push(body);
            }
            id::CONNECTION_REQUEST => {
                let _client_guid = r.i64()?;
                let time = i64::from_be_bytes(r.array()?);
                let mut reply = vec![id::CONNECTION_REQUEST_ACCEPTED];
                write_address(&mut reply, self.peer);
                reply.extend_from_slice(&0u16.to_be_bytes());
                let local = SocketAddr::from(([0, 0, 0, 0], 0));
                for _ in 0..20 {
                    write_address(&mut reply, local);
                }
                reply.extend_from_slice(&time.to_be_bytes());
                reply.extend_from_slice(&millis(now).to_be_bytes());
                self.send(reply, Reliability::ReliableOrdered);
            }
            id::NEW_INCOMING_CONNECTION => out.connected = true,
            id::CONNECTED_PING => {
                let time = i64::from_be_bytes(r.array()?);
                let mut reply = vec![id::CONNECTED_PONG];
                reply.extend_from_slice(&time.to_be_bytes());
                reply.extend_from_slice(&millis(now).to_be_bytes());
                self.send(reply, Reliability::Unreliable);
            }
            id::DISCONNECT => out.disconnected = true,
            _ => {}
        }
        Ok(())
    }

    /// Queues a game batch (`0xfe` + payload), reliable and ordered on channel 0.
    pub fn send_game(&mut self, payload: &[u8]) {
        let mut body = Vec::with_capacity(payload.len().saturating_add(1));
        body.push(id::GAME);
        body.extend_from_slice(payload);
        self.send(body, Reliability::ReliableOrdered);
    }

    /// Queues a message, split into parts that fit the MTU.
    pub fn send(&mut self, body: Vec<u8>, reliability: Reliability) {
        let max = self
            .mtu
            .saturating_sub(UDP_OVERHEAD + DATAGRAM_HEADER + MAX_FRAME_HEADER)
            .max(1);
        let r = reliability.bits();
        let len = body.len();
        let buf: Arc<[u8]> = body.into();
        let order_index = if is_ordered(r) || is_sequenced(r) {
            let i = self.next_order;
            if is_ordered(r) {
                self.next_order = next24(i);
            }
            i
        } else {
            0
        };
        let sequence_index = if is_sequenced(r) {
            let i = self.next_sequence;
            self.next_sequence = next24(i);
            i
        } else {
            0
        };
        let parts = len.div_ceil(max).max(1);
        let split_id = if parts > 1 {
            let id = self.next_split;
            self.next_split = id.wrapping_add(1);
            Some(id)
        } else {
            None
        };
        for part in 0..parts {
            let start = part.saturating_mul(max);
            let end = start.saturating_add(max).min(len);
            let reliable_index = if is_reliable(r) {
                let i = self.next_reliable;
                self.next_reliable = next24(i);
                i
            } else {
                0
            };
            let split = split_id.map(|id| {
                (
                    u32::try_from(parts).unwrap_or(u32::MAX),
                    id,
                    u32::try_from(part).unwrap_or(u32::MAX),
                )
            });
            self.queued_bytes = self.queued_bytes.saturating_add(end.saturating_sub(start));
            self.queue.push_back(Frame {
                reliability: r,
                reliable_index,
                sequence_index,
                order_index,
                split,
                body: Body {
                    buf: Arc::clone(&buf),
                    start,
                    end,
                },
            });
        }
    }

    fn release(&mut self, frames: &[Frame]) {
        for f in frames {
            self.queued_bytes = self
                .queued_bytes
                .saturating_sub(f.body.end.saturating_sub(f.body.start));
        }
    }

    /// Writes due acknowledgements, resends and queued frames as datagrams.
    pub fn flush(&mut self, now: Instant, mut send: impl FnMut(&[u8])) {
        let max = self.mtu.saturating_sub(UDP_OVERHEAD);
        let mut buf = Vec::with_capacity(max);
        for (flag, list) in [(FLAG_ACK, &mut self.acks), (FLAG_NAK, &mut self.naks)] {
            if list.is_empty() {
                continue;
            }
            list.sort_unstable();
            list.dedup();
            write_ranges(flag, list, max, &mut buf, &mut send);
            list.clear();
        }
        // Resend what was not acknowledged in time.
        let rto = self
            .srtt
            .saturating_mul(2)
            .saturating_add(Duration::from_millis(50))
            .clamp(Duration::from_millis(100), Duration::from_secs(2));
        let mut expired: Vec<u32> = self
            .in_flight
            .iter()
            .filter(|(_, f)| now.saturating_duration_since(f.sent) >= rto)
            .map(|(&seq, _)| seq)
            .collect();
        expired.sort_unstable_by_key(|&s| std::cmp::Reverse(diff24(s, self.next_datagram)));
        for seq in expired {
            if let Some(f) = self.in_flight.remove(&seq) {
                for frame in f.frames.into_iter().rev() {
                    self.queue.push_front(frame);
                }
            }
        }
        // At most this many datagrams wait for acknowledgement (a fixed congestion window).
        while !self.queue.is_empty() && self.in_flight.len() < 1024 {
            buf.clear();
            buf.push(FLAG_VALID | FLAG_NEEDS_B_AND_AS);
            let seq = self.next_datagram;
            write_u24(&mut buf, seq);
            let mut reliable = Vec::new();
            while let Some(frame) = self.queue.front() {
                if buf.len() > DATAGRAM_HEADER && buf.len().saturating_add(frame.wire_len()) > max {
                    break;
                }
                let Some(frame) = self.queue.pop_front() else {
                    break;
                };
                frame.encode(&mut buf);
                if is_reliable(frame.reliability) {
                    reliable.push(frame);
                } else {
                    self.release(std::slice::from_ref(&frame));
                }
            }
            self.next_datagram = next24(seq);
            if !reliable.is_empty() {
                self.in_flight.insert(
                    seq,
                    InFlight {
                        frames: reliable,
                        sent: now,
                    },
                );
            }
            send(&buf);
        }
    }

    /// Queues a disconnect notification.
    pub fn disconnect(&mut self) {
        self.send(vec![id::DISCONNECT], Reliability::ReliableOrdered);
    }
}

fn read_ranges(r: &mut Reader<'_>, out: &mut Vec<u32>) -> std::result::Result<(), RakError> {
    let count = u16::from_be_bytes(r.array()?);
    for _ in 0..count {
        let single = r.bool()?;
        let start = read_u24(r)?;
        let end = if single { start } else { read_u24(r)? };
        let span = diff24(end, start);
        if span >= 8192 || out.len() > 65536 {
            return Err(RakError::Limit("acknowledgement ranges"));
        }
        let mut seq = start;
        for _ in 0..=span {
            out.push(seq);
            seq = next24(seq);
        }
    }
    Ok(())
}

fn write_ranges(
    flag: u8,
    sorted: &[u32],
    max: usize,
    buf: &mut Vec<u8>,
    send: &mut impl FnMut(&[u8]),
) {
    let mut records: Vec<(u32, u32)> = Vec::new();
    for &seq in sorted {
        match records.last_mut() {
            Some((_, end)) if next24(*end) == seq => *end = seq,
            _ => records.push((seq, seq)),
        }
    }
    for chunk in records.chunks(max.saturating_sub(3) / 7) {
        buf.clear();
        buf.push(FLAG_VALID | flag);
        buf.extend_from_slice(&u16::try_from(chunk.len()).unwrap_or(0).to_be_bytes());
        for &(start, end) in chunk {
            if start == end {
                buf.push(1);
                write_u24(buf, start);
            } else {
                buf.push(0);
                write_u24(buf, start);
                write_u24(buf, end);
            }
        }
        send(buf);
    }
}

fn millis(now: Instant) -> i64 {
    // Only differences matter to the peer; a process-local clock is enough.
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let start = *START.get_or_init(|| now);
    i64::try_from(now.saturating_duration_since(start).as_millis()).unwrap_or(i64::MAX)
}

/// Appends a RakNet system address.
pub fn write_address(out: &mut Vec<u8>, addr: SocketAddr) {
    match addr {
        SocketAddr::V4(a) => {
            out.push(4);
            out.extend(a.ip().octets().map(|b| !b));
            out.extend_from_slice(&a.port().to_be_bytes());
        }
        SocketAddr::V6(a) => {
            out.push(6);
            out.extend_from_slice(&23u16.to_le_bytes());
            out.extend_from_slice(&a.port().to_be_bytes());
            out.extend_from_slice(&a.flowinfo().to_be_bytes());
            out.extend_from_slice(&a.ip().octets());
            out.extend_from_slice(&a.scope_id().to_be_bytes());
        }
    }
}

/// Skips a RakNet system address.
pub fn skip_address(r: &mut Reader<'_>) -> Result<()> {
    match r.u8()? {
        6 => r.take(28).map(|_| ()),
        _ => r.take(6).map(|_| ()),
    }
}

/// Stateless handshake cookies: a keyed hash of the peer address, rotated by the caller.
#[derive(Debug)]
pub struct Cookies {
    keys: [std::collections::hash_map::RandomState; 2],
    rotated: Instant,
}

impl Cookies {
    /// Fresh random keys.
    pub fn new(now: Instant) -> Self {
        Self {
            keys: [Default::default(), Default::default()],
            rotated: now,
        }
    }

    /// Rotates the key every few seconds; the previous key stays valid for one period.
    pub fn rotate(&mut self, now: Instant) {
        if now.saturating_duration_since(self.rotated) >= Duration::from_secs(5) {
            self.keys.swap(0, 1);
            self.keys[0] = Default::default();
            self.rotated = now;
        }
    }

    /// The cookie for `addr` under the current key.
    pub fn issue(&self, addr: SocketAddr) -> u32 {
        cookie(&self.keys[0], addr)
    }

    /// Whether `value` is a cookie for `addr` under the current or previous key.
    pub fn check(&self, addr: SocketAddr, value: u32) -> bool {
        self.keys.iter().any(|k| cookie(k, addr) == value)
    }
}

fn cookie(key: &std::collections::hash_map::RandomState, addr: SocketAddr) -> u32 {
    u32::try_from(key.hash_one(addr) & u64::from(u32::MAX)).unwrap_or(0)
}

/// Parsed `open_connection_request_1`: the MTU the peer could send.
pub fn parse_request_1(packet: &[u8]) -> Option<(u8, u16)> {
    let mut r = Reader::new(packet.get(1..)?);
    if r.array::<16>().ok()? != MAGIC {
        return None;
    }
    let protocol = r.u8().ok()?;
    let mtu = u16::try_from(packet.len().saturating_add(UDP_OVERHEAD)).unwrap_or(u16::MAX);
    Some((protocol, mtu))
}

/// Parsed `open_connection_request_2`: cookie, MTU and client GUID.
pub fn parse_request_2(packet: &[u8]) -> Option<(u32, u16, i64)> {
    let mut r = Reader::new(packet.get(1..)?);
    if r.array::<16>().ok()? != MAGIC {
        return None;
    }
    let cookie = u32::from_be_bytes(r.array().ok()?);
    let _client_wrote_challenge = r.u8().ok()?;
    skip_address(&mut r).ok()?;
    let mtu = u16::from_be_bytes(r.array().ok()?);
    let guid = i64::from_be_bytes(r.array().ok()?);
    Some((cookie, mtu, guid))
}

/// `open_connection_reply_1` with a cookie.
pub fn reply_1(guid: i64, cookie: u32, mtu: u16) -> Vec<u8> {
    let mut out = vec![id::OPEN_CONNECTION_REPLY_1];
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&guid.to_be_bytes());
    out.push(1);
    out.extend_from_slice(&cookie.to_be_bytes());
    out.extend_from_slice(&mtu.to_be_bytes());
    out
}

/// `open_connection_reply_2`.
pub fn reply_2(guid: i64, peer: SocketAddr, mtu: u16) -> Vec<u8> {
    let mut out = vec![id::OPEN_CONNECTION_REPLY_2];
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&guid.to_be_bytes());
    write_address(&mut out, peer);
    out.extend_from_slice(&mtu.to_be_bytes());
    out.push(0);
    out
}

/// `incompatible_protocol_version`.
pub fn incompatible(guid: i64) -> Vec<u8> {
    let mut out = vec![id::INCOMPATIBLE_PROTOCOL, PROTOCOL];
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&guid.to_be_bytes());
    out
}

/// `unconnected_pong` with the server list text.
pub fn pong(ping: &[u8], guid: i64, text: &str) -> Option<Vec<u8>> {
    let time: [u8; 8] = ping.get(1..9)?.try_into().ok()?;
    let mut out = vec![id::UNCONNECTED_PONG];
    out.extend_from_slice(&time);
    out.extend_from_slice(&guid.to_be_bytes());
    out.extend_from_slice(&MAGIC);
    let text = text.as_bytes();
    let len = u16::try_from(text.len()).ok()?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(text);
    Some(out)
}

/// Connection counts per IP for the admission limit (SEC-18).
#[derive(Debug, Default)]
pub struct PerIp(FxHashMap<IpAddr, u32>);

impl PerIp {
    /// Adds a connection if `ip` has fewer than `max`.
    pub fn admit(&mut self, ip: IpAddr, max: u32) -> bool {
        let n = self.0.entry(ip).or_insert(0);
        if *n >= max {
            return false;
        }
        *n = n.saturating_add(1);
        true
    }

    /// Removes a connection of `ip`.
    pub fn release(&mut self, ip: IpAddr) {
        if let Some(n) = self.0.get_mut(&ip) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                self.0.remove(&ip);
            }
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]
mod tests {
    use super::*;

    fn pair() -> (RakConn, RakConn, Instant) {
        let now = Instant::now();
        let a = SocketAddr::from(([127, 0, 0, 1], 1));
        let b = SocketAddr::from(([127, 0, 0, 1], 2));
        (
            RakConn::new(b, 576, Limits::default(), now),
            RakConn::new(a, 576, Limits::default(), now),
            now,
        )
    }

    fn pump(
        from: &mut RakConn,
        to: &mut RakConn,
        now: Instant,
        drop: impl Fn(usize) -> bool,
    ) -> Vec<Vec<u8>> {
        let mut datagrams = Vec::new();
        from.flush(now, |d| datagrams.push(d.to_vec()));
        let mut game = Vec::new();
        for (i, d) in datagrams.iter().enumerate() {
            if drop(i) {
                continue;
            }
            game.extend(to.receive(d, now).unwrap().game);
        }
        // Acknowledgements back.
        let mut acks = Vec::new();
        to.flush(now, |d| acks.push(d.to_vec()));
        for d in acks {
            from.receive(&d, now).unwrap();
        }
        game
    }

    #[test]
    fn splits_and_reorders_large_messages() {
        let (mut a, mut b, now) = pair();
        let big: Vec<u8> = (0..20_000u32)
            .map(|i| u8::try_from(i % 251).unwrap())
            .collect();
        a.send_game(&big);
        a.send_game(b"second");
        // Lose every third datagram; the NAKs and resends deliver them.
        let mut got = pump(&mut a, &mut b, now, |i| i % 3 == 1);
        for _ in 0..10 {
            got.extend(pump(&mut a, &mut b, now, |_| false));
        }
        assert_eq!(got, vec![big, b"second".to_vec()]);
        assert_eq!(a.backlog(), 0);
    }

    #[test]
    fn duplicates_are_dropped() {
        let (mut a, mut b, now) = pair();
        a.send_game(b"once");
        let mut datagrams = Vec::new();
        a.flush(now, |d| datagrams.push(d.to_vec()));
        let first = b.receive(&datagrams[0], now).unwrap();
        let second = b.receive(&datagrams[0], now).unwrap();
        assert_eq!(first.game, vec![b"once".to_vec()]);
        assert!(second.game.is_empty());
    }

    #[test]
    fn rejects_oversized_splits() {
        let (_, mut b, now) = pair();
        let mut d = vec![FLAG_VALID, 0, 0, 0];
        d.push(0x60 | FRAME_SPLIT);
        d.extend_from_slice(&8u16.to_be_bytes());
        write_u24(&mut d, 0);
        write_u24(&mut d, 0);
        d.push(0);
        d.extend_from_slice(&u32::MAX.to_be_bytes());
        d.extend_from_slice(&0u16.to_be_bytes());
        d.extend_from_slice(&0u32.to_be_bytes());
        d.push(0xfe);
        assert!(b.receive(&d, now).is_err());
    }

    #[test]
    fn cookies_rotate() {
        let now = Instant::now();
        let mut c = Cookies::new(now);
        let addr = SocketAddr::from(([1, 2, 3, 4], 5));
        let v = c.issue(addr);
        assert!(c.check(addr, v));
        c.rotate(now + Duration::from_secs(6));
        assert!(c.check(addr, v));
        c.rotate(now + Duration::from_secs(12));
        assert!(!c.check(addr, v) || c.issue(addr) == v);
    }
}
