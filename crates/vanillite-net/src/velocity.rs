//! Velocity "modern" player info forwarding (architecture 8.3, SEC-14).
//!
//! The backend sends a login plugin request on [`CHANNEL`] whose payload is the highest
//! forwarding version it understands (one byte). The proxy answers with an HMAC-SHA256
//! signature (32 bytes) followed by the signed data: the forwarding version (VarInt), the
//! player's address (string) and the game profile (UUID, name, properties). Proxy and backend
//! share the secret; the backend compares signatures in constant time.

use std::net::IpAddr;
use std::sync::Arc;

use sha2::{Digest, Sha256};
use vanillite_protocol::codec::{Decode, DecodeLimited, Encode, Reader, write_varint};
use vanillite_protocol::types::GameProfile;

/// The login plugin channel.
pub const CHANNEL: &str = "velocity:player_info";
/// Forwarding version without a chat signing key.
pub const MODERN_DEFAULT: u8 = 1;
/// Forwarding version whose chat session arrives later in play (1.19.3 and newer clients).
pub const MODERN_LAZY_SESSION: u8 = 4;
/// Highest version this implementation requests and answers.
pub const MAX_VERSION: u8 = MODERN_LAZY_SESSION;

/// Length of the signature in front of the data.
const SIGNATURE_LEN: usize = 32;
/// Longest address string accepted (an IPv6 address with zone fits easily).
const MAX_ADDRESS: usize = 128;

/// The shared forwarding secret. Its `Debug` output hides the value.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(Arc<[u8]>);

impl Secret {
    /// A secret from its bytes; `None` for an empty one.
    pub fn new(bytes: &[u8]) -> Option<Self> {
        (!bytes.is_empty()).then(|| Self(bytes.into()))
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(..)")
    }
}

/// Player data a proxy forwarded and signed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Forwarded {
    /// Forwarding version the proxy used.
    pub version: u8,
    /// The player's address as the proxy saw it.
    pub address: IpAddr,
    /// The authenticated profile.
    pub profile: GameProfile,
}

/// Why forwarded data was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForwardError {
    /// The signature does not match the secret.
    Signature,
    /// The data is malformed or of an unsupported version.
    Malformed,
}

/// HMAC-SHA256 (RFC 2104).
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut block = [0u8; 64];
    if key.len() > block.len() {
        let digest: [u8; 32] = Sha256::digest(key).into();
        block.iter_mut().zip(digest).for_each(|(b, k)| *b = k);
    } else {
        block.iter_mut().zip(key).for_each(|(b, k)| *b = *k);
    }
    let mut inner = Sha256::new();
    inner.update(block.map(|b| b ^ 0x36));
    inner.update(data);
    let inner: [u8; 32] = inner.finalize().into();
    let mut outer = Sha256::new();
    outer.update(block.map(|b| b ^ 0x5c));
    outer.update(inner);
    outer.finalize().into()
}

/// Constant-time comparison of two equally long byte strings.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len()
        && std::hint::black_box(a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y))) == 0
}

/// The version a proxy answers a request payload with: the lazy-session version if the backend
/// understands it, the default version otherwise (neither carries a signing key).
pub fn answer_version(request: &[u8]) -> u8 {
    match request.first() {
        Some(&v) if v >= MODERN_LAZY_SESSION => MODERN_LAZY_SESSION,
        _ => MODERN_DEFAULT,
    }
}

/// Writes the signed answer (signature, then data) for `profile` connecting from `address`.
pub fn encode(
    secret: &Secret,
    version: u8,
    address: IpAddr,
    profile: &GameProfile,
    out: &mut Vec<u8>,
) {
    let start = out.len();
    out.extend_from_slice(&[0; SIGNATURE_LEN]);
    write_varint(out, i32::from(version));
    address.to_string().encode(out);
    profile.encode(out);
    let Some((signature, data)) = out
        .get_mut(start..)
        .and_then(|s| s.split_at_mut_checked(32))
    else {
        return;
    };
    signature.copy_from_slice(&hmac_sha256(&secret.0, data));
}

/// Verifies and decodes a forwarding answer.
pub fn decode(secret: &Secret, payload: &[u8]) -> Result<Forwarded, ForwardError> {
    let (signature, data) = payload
        .split_at_checked(SIGNATURE_LEN)
        .ok_or(ForwardError::Signature)?;
    if !constant_time_eq(signature, &hmac_sha256(&secret.0, data)) {
        return Err(ForwardError::Signature);
    }
    let mut r = Reader::new(data);
    let version = r
        .varint()
        .ok()
        .and_then(|v| u8::try_from(v).ok())
        .ok_or(ForwardError::Malformed)?;
    // Versions 2 and 3 carry a chat signing key that clients of this protocol never send.
    if version != MODERN_DEFAULT && version != MODERN_LAZY_SESSION {
        return Err(ForwardError::Malformed);
    }
    let address = String::decode_limited(&mut r, MAX_ADDRESS)
        .ok()
        .and_then(|a| a.parse().ok())
        .ok_or(ForwardError::Malformed)?;
    let profile = GameProfile::decode(&mut r).map_err(|_| ForwardError::Malformed)?;
    if !r.take_rest().is_empty() {
        return Err(ForwardError::Malformed);
    }
    Ok(Forwarded {
        version,
        address,
        profile,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_rfc_4231_and_round_trips() {
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        let hex: String = mac.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );

        let secret = Secret::new(b"secret").unwrap();
        let profile = GameProfile {
            name: "Alice".into(),
            ..GameProfile::default()
        };
        let mut out = Vec::new();
        encode(&secret, 4, [1, 2, 3, 4].into(), &profile, &mut out);
        let f = decode(&secret, &out).unwrap();
        assert_eq!((f.version, f.profile), (4, profile));
        assert_eq!(
            decode(&Secret::new(b"other").unwrap(), &out),
            Err(ForwardError::Signature)
        );
    }
}
