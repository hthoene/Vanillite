//! Inbound framing and decompression (SEC-03, SEC-05): arbitrary bytes, delivered in arbitrary
//! pieces, for every connection state, with and without compression.

#![no_main]

use libfuzzer_sys::fuzz_target;
use vanillite_protocol::State;
use vanillite_protocol::frame::FrameDecoder;

const STATES: [State; 5] = [
    State::Handshake,
    State::Status,
    State::Login,
    State::Configuration,
    State::Play,
];

fuzz_target!(|data: &[u8]| {
    let [config, threshold_hi, threshold_lo, chunk, rest @ ..] = data else {
        return;
    };
    let state = STATES[usize::from(config % 5)];
    let limits = state.limits();
    let mut decoder = FrameDecoder::new();
    decoder.set_limits(limits);
    if config & 0x80 != 0 {
        decoder.set_compression(i32::from(u16::from_be_bytes([
            *threshold_hi,
            *threshold_lo,
        ])));
    }
    let chunk = usize::from(*chunk).max(1);
    for piece in rest.chunks(chunk) {
        decoder.push(piece);
        loop {
            match decoder.next_frame() {
                Ok(Some(frame)) => assert!(frame.len() <= limits.max_uncompressed),
                Ok(None) => break,
                Err(_) => return,
            }
        }
    }
});
