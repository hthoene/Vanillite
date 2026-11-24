//! Serverbound status packets (SEC-02, SEC-05, SEC-06): every implemented decoder of the state,
//! selected by the packet id at the start of the input.

#![no_main]

use libfuzzer_sys::fuzz_target;
use vanillite_protocol::sets::ServerboundStatus;

fuzz_target!(|data: &[u8]| {
    if let Ok(packet) = ServerboundStatus::decode(data) {
        let _ = (packet.cost(), packet.name());
    }
});
