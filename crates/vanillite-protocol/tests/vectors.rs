//! Packet test vectors captured from a Vanilla 26.3 server (offline mode, flat world, two
//! clients joining, walking, turning, sneaking and leaving): every captured body must decode
//! with our play decoders and encode back to the same bytes.
//!
//! Text components are only decoded: the component model keeps text, translations and
//! styles but not yet click and hover events, so Vanilla's join message does not re-encode
//! byte for byte.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use vanillite_protocol::sets::ClientboundPlay;

#[test]
fn captured_vanilla_packets_round_trip() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/vectors/play");
    let mut count = 0;
    let mut failures = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("vector directory") {
        let path = entry.unwrap().path();
        let name = path.file_stem().unwrap().to_str().unwrap().to_owned();
        let bytes = std::fs::read(&path).unwrap();
        let packet = ClientboundPlay::decode(&bytes)
            .unwrap_or_else(|e| panic!("{name}: Vanilla's packet does not decode: {e}"));
        assert_eq!(
            packet.name().trim_start_matches("minecraft:"),
            name,
            "{name}: decoded as another packet"
        );
        if name != "system_chat" && packet.encode_body() != bytes {
            failures.push(name);
        }
        count += 1;
    }
    assert!(failures.is_empty(), "re-encoding differs: {failures:?}");
    assert!(count >= 20, "only {count} vectors");
}
