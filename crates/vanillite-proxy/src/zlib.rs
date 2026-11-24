//! Inflating relayed frames of a trusted backend link, which pass the proxy still compressed:
//! only the packet id is inflated to decide whether the proxy acts on the packet, and only those
//! few packets are inflated in full. One inflater per I/O thread keeps per-player memory flat.

use std::cell::RefCell;

use flate2::{Decompress, FlushDecompress, Status};
use vanillite_protocol::codec::Reader;

thread_local! {
    static INFLATE: RefCell<Decompress> = RefCell::new(Decompress::new(true));
}

/// The packet id at the start of a compressed packet.
pub(crate) fn peek_id(zlib: &[u8]) -> Option<i32> {
    INFLATE.with_borrow_mut(|d| {
        d.reset(true);
        let mut head = [0u8; 5];
        d.decompress(zlib, &mut head, FlushDecompress::None).ok()?;
        let n = usize::try_from(d.total_out()).ok()?;
        Reader::new(head.get(..n)?).varint().ok()
    })
}

/// Inflates a whole packet that must be exactly `expected` bytes long (checked against the
/// limits by the frame decoder) into `out`.
pub(crate) fn inflate(zlib: &[u8], expected: usize, out: &mut Vec<u8>) -> bool {
    out.clear();
    out.reserve_exact(expected);
    INFLATE.with_borrow_mut(|d| {
        d.reset(true);
        matches!(
            d.decompress_vec(zlib, out, FlushDecompress::Finish),
            Ok(Status::StreamEnd)
        ) && out.len() == expected
    })
}
