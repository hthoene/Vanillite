//! Binary NBT reader (SEC-03, SEC-04): network and file roots, the allocation-free validator, and
//! idempotent re-encoding of everything the reader accepts.

#![no_main]

use libfuzzer_sys::fuzz_target;
use vanillite_nbt::{Budget, read, write};

fuzz_target!(|data: &[u8]| {
    let mut input = data;
    let tag = read::read_network(&mut input, &mut Budget::network());
    let _ = read::skip_network(data, &mut Budget::network());
    let _ = read::read_named(&mut &data[..], &mut Budget::network());

    // Whatever the reader accepts, the writer encodes, and that encoding is a fixed point.
    if let Ok(Some(tag)) = tag {
        let mut first = Vec::new();
        write::write_network(&tag, &mut first).expect("accepted NBT must be encodable");
        let again = read::read_network(&mut &first[..], &mut Budget::file())
            .expect("re-encoded NBT must be readable")
            .expect("re-encoded NBT is not empty");
        let mut second = Vec::new();
        write::write_network(&again, &mut second).expect("re-read NBT must be encodable");
        assert_eq!(first, second, "NBT encoding is not stable");
    }
});
