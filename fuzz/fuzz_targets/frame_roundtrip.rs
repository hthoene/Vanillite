//! Outbound framing against inbound framing: every body the encoder frames comes back unchanged
//! through the decoder, with and without compression and encryption.

#![no_main]

use libfuzzer_sys::fuzz_target;
use vanillite_protocol::frame::{FrameDecoder, FrameEncoder, Limits};

fuzz_target!(|data: &[u8]| {
    let [config, threshold_hi, threshold_lo, body @ ..] = data else {
        return;
    };
    if body.is_empty() {
        return;
    }
    let mut encoder = FrameEncoder::new();
    let mut decoder = FrameDecoder::new();
    decoder.set_limits(Limits::PLAY);
    if config & 1 != 0 {
        let threshold = i32::from(u16::from_be_bytes([*threshold_hi, *threshold_lo]));
        encoder.set_compression(threshold);
        decoder.set_compression(threshold);
    }
    if config & 2 != 0 {
        let secret = [*config; 16];
        encoder.enable_encryption(&secret);
        decoder.enable_encryption(&secret);
    }
    let mut wire = Vec::new();
    if encoder.body(body, &mut wire).is_err() {
        return;
    }
    decoder.push(&wire);
    let frame = decoder
        .next_frame()
        .expect("own frame must decode")
        .expect("own frame must be complete");
    assert_eq!(frame, body);
    assert_eq!(decoder.buffered(), 0);
});
