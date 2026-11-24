//! SNBT parser (SEC-04): parsing never panics or overflows the stack, and the formatter's output
//! of an accepted value parses again.

#![no_main]

use libfuzzer_sys::fuzz_target;
use vanillite_nbt::snbt;

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };
    let _ = snbt::parse_compound(s);
    if let Ok(tag) = snbt::parse(s) {
        let text = snbt::to_string(&tag);
        snbt::parse(&text).expect("formatted SNBT must parse again");
    }
});
