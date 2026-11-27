//! The last packets written to a connection, kept so that a connection the client drops can
//! be explained: a Java client sends no reason when it gives up on a packet (#135). A fixed
//! ring of packed atomics: one store per packet, no allocation, written by the connection's
//! writer and read when the connection ends.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;
use vanillite_protocol::State;
use vanillite_protocol::codec::Reader;
use vanillite_protocol::ids;

/// Entries kept (a power of two).
const SLOTS: usize = 128;
const SLOT_MASK: usize = 127;
/// Set in every written entry.
const VALID: u64 = 1 << 63;
/// Milliseconds since the connection started, wrapping (23 bits).
const MS_MASK: u64 = 0x7F_FFFF;
const SIZE_MASK: u64 = 0xFF_FFFF;
/// Packet id that is unknown (compressed inside a batch) or above 254.
const UNKNOWN_ID: u8 = 0xFF;

/// Ring of the last packets written: time, connection state, packet id and size.
#[derive(Debug)]
pub(crate) struct SentLog {
    start: Instant,
    state: AtomicU8,
    next: AtomicUsize,
    slots: [AtomicU64; SLOTS],
}

impl SentLog {
    pub(crate) fn new() -> Self {
        Self {
            start: Instant::now(),
            state: AtomicU8::new(state_tag(State::Handshake)),
            next: AtomicUsize::new(0),
            slots: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    /// The state that names the ids of the packets recorded from now on.
    pub(crate) fn set_state(&self, state: State) {
        self.state.store(state_tag(state), Ordering::Relaxed);
    }

    /// Milliseconds since the connection started, for [`SentLog::body`] and
    /// [`SentLog::frames`].
    pub(crate) fn now_ms(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX) & MS_MASK
    }

    fn record(&self, ms: u64, id: Option<i32>, size: usize) {
        let index = self.next.fetch_add(1, Ordering::Relaxed);
        self.store(index, ms, id, size);
    }

    /// Writes the entry with the sequence number `index`.
    fn store(&self, index: usize, ms: u64, id: Option<i32>, size: usize) {
        let id = id
            .and_then(|i| u8::try_from(i).ok())
            .filter(|&i| i != UNKNOWN_ID)
            .unwrap_or(UNKNOWN_ID);
        let size = u64::try_from(size).unwrap_or(SIZE_MASK).min(SIZE_MASK);
        let state = u64::from(self.state.load(Ordering::Relaxed));
        let packed = VALID
            | (ms & MS_MASK).wrapping_shl(40)
            | state.wrapping_shl(32)
            | u64::from(id).wrapping_shl(24)
            | size;
        if let Some(slot) = self.slots.get(index & SLOT_MASK) {
            slot.store(packed, Ordering::Relaxed);
        }
    }

    /// Records a packet body (`id` + fields).
    pub(crate) fn body(&self, ms: u64, body: &[u8]) {
        self.record(ms, Reader::new(body).varint().ok(), body.len());
    }

    /// Records the frames of a batch built for `compressed` or uncompressed framing. The id
    /// of a compressed frame is not read (that would inflate it). Only the last [`SLOTS`]
    /// frames stay in the ring, so only those are written.
    pub(crate) fn frames(&self, ms: u64, frames: &[u8], compressed: bool) {
        let mut last = [(None, 0usize); SLOTS];
        let mut count = 0usize;
        let mut r = Reader::new(frames);
        while r.remaining() > 0 {
            let Some(frame) = r
                .varint()
                .ok()
                .and_then(|len| usize::try_from(len).ok())
                .and_then(|len| r.take(len).ok())
            else {
                return;
            };
            let mut f = Reader::new(frame);
            let id = if compressed {
                match f.varint() {
                    Ok(0) => f.varint().ok(),
                    _ => None,
                }
            } else {
                f.varint().ok()
            };
            if let Some(entry) = last.get_mut(count & SLOT_MASK) {
                *entry = (id, frame.len());
            }
            count = count.wrapping_add(1);
        }
        let kept = count.min(SLOTS);
        let first = self.next.fetch_add(kept, Ordering::Relaxed);
        for (k, index) in (count.wrapping_sub(kept)..count).zip(first..) {
            if let Some(&(id, size)) = last.get(k & SLOT_MASK) {
                self.store(index, ms, id, size);
            }
        }
    }

    /// The recorded packets, oldest first: age, state, name and size.
    pub(crate) fn dump(&self) -> String {
        let now = self.now_ms();
        let next = self.next.load(Ordering::Relaxed);
        let mut out = String::new();
        for k in 0..SLOTS {
            let index = next.wrapping_add(k) & SLOT_MASK;
            let Some(packed) = self.slots.get(index).map(|s| s.load(Ordering::Relaxed)) else {
                continue;
            };
            if packed & VALID == 0 {
                continue;
            }
            let ms = packed.wrapping_shr(40) & MS_MASK;
            let state = u8::try_from(packed.wrapping_shr(32) & 0xFF).unwrap_or(0);
            let id = u8::try_from(packed.wrapping_shr(24) & 0xFF).unwrap_or(UNKNOWN_ID);
            let size = packed & SIZE_MASK;
            let age = now.wrapping_sub(ms) & MS_MASK;
            if !out.is_empty() {
                out.push_str(", ");
            }
            let _ = write!(out, "-{age}ms {} {size}B", packet_name(state, id));
        }
        if out.is_empty() {
            out.push_str("none");
        }
        out
    }
}

fn state_tag(state: State) -> u8 {
    match state {
        State::Handshake => 0,
        State::Status => 1,
        State::Login => 2,
        State::Configuration => 3,
        State::Play => 4,
    }
}

fn packet_name(state: u8, id: u8) -> &'static str {
    if id == UNKNOWN_ID {
        return "compressed";
    }
    let names: &[&str] = match state {
        1 => &ids::status::clientbound::NAMES,
        2 => &ids::login::clientbound::NAMES,
        3 => &ids::configuration::clientbound::NAMES,
        4 => &ids::play::clientbound::NAMES,
        _ => &[],
    };
    names.get(usize::from(id)).copied().unwrap_or("unknown")
}

#[cfg(test)]
mod tests {
    use super::*;
    use vanillite_protocol::codec::write_varint;

    #[test]
    fn keeps_the_last_packets_in_order() {
        let log = SentLog::new();
        log.set_state(State::Play);
        for i in (0..2).flat_map(|_| 0..100u8) {
            log.body(0, &[i, 0, 0]);
        }
        let dump = log.dump();
        assert_eq!(dump.matches(", ").count(), SLOTS - 1);
        assert!(dump.starts_with("-0ms "), "{dump}");
        assert!(dump.ends_with(&format!("{} 3B", ids::play::clientbound::NAMES[99])));
    }

    #[test]
    fn reads_ids_of_uncompressed_frames() {
        let log = SentLog::new();
        log.set_state(State::Play);
        let mut frames = Vec::new();
        // An uncompressed frame below the threshold, then a compressed one.
        for (data_len, id) in [(0, 1u8), (300, 2)] {
            let mut frame = Vec::new();
            write_varint(&mut frame, data_len);
            frame.extend_from_slice(&[id, 9, 9]);
            write_varint(&mut frames, i32::try_from(frame.len()).unwrap_or_default());
            frames.extend_from_slice(&frame);
        }
        log.frames(0, &frames, true);
        let dump = log.dump();
        assert!(dump.contains(ids::play::clientbound::NAMES[1]), "{dump}");
        assert!(dump.contains("compressed 5B"), "{dump}");
    }
}
