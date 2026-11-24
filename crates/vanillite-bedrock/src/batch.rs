//! Game packet batches: the payload of a RakNet `0xfe` message is a list of VarInt-prefixed
//! packets, compressed (after `network_settings`) and encrypted (after the handshake).
//!
//! Encryption is AES-256 in CTR mode with the IV `key[..12] ++ 00 00 00 02` and an 8-byte
//! SHA-256 checksum over a per-direction counter, the plaintext and the key.

use aes::cipher::{KeyIvInit, StreamCipher};
use sha2::{Digest, Sha256};

use crate::io::{self, Reader};

type Aes256Ctr = ctr::Ctr32BE<aes::Aes256>;

/// Compression id of raw DEFLATE ("zlib" in Bedrock's terms).
pub const COMPRESSION_DEFLATE: u8 = 0x00;
/// No compression.
pub const COMPRESSION_NONE: u8 = 0xff;

/// One direction of the cipher.
pub struct Cipher {
    ctr: Aes256Ctr,
    key: [u8; 32],
    counter: u64,
}

impl std::fmt::Debug for Cipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cipher").finish_non_exhaustive()
    }
}

impl Cipher {
    /// A cipher for the shared key.
    pub fn new(key: [u8; 32]) -> Self {
        let mut iv = [0u8; 16];
        iv.get_mut(..12)
            .unwrap_or_default()
            .copy_from_slice(key.get(..12).unwrap_or_default());
        iv[15] = 2;
        Self {
            ctr: Aes256Ctr::new(&key.into(), &iv.into()),
            key,
            counter: 0,
        }
    }

    fn checksum(&self, plain: &[u8]) -> [u8; 8] {
        let mut h = Sha256::new();
        h.update(self.counter.to_le_bytes());
        h.update(plain);
        h.update(self.key);
        let mut out = [0u8; 8];
        out.copy_from_slice(h.finalize().get(..8).unwrap_or(&[0; 8]));
        out
    }

    /// Appends the checksum and encrypts in place.
    pub fn encrypt(&mut self, data: &mut Vec<u8>) {
        let sum = self.checksum(data);
        data.extend_from_slice(&sum);
        self.ctr.apply_keystream(data);
        self.counter = self.counter.wrapping_add(1);
    }

    /// Decrypts in place and verifies and removes the checksum.
    pub fn decrypt(&mut self, data: &mut Vec<u8>) -> Result<(), BatchError> {
        self.ctr.apply_keystream(data);
        let body = data.len().checked_sub(8).ok_or(BatchError::Checksum)?;
        let expected = self.checksum(data.get(..body).unwrap_or_default());
        if data.get(body..) != Some(&expected[..]) {
            return Err(BatchError::Checksum);
        }
        data.truncate(body);
        self.counter = self.counter.wrapping_add(1);
        Ok(())
    }
}

/// Why a batch was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchError {
    /// Wrong checksum (tampered or out of sync).
    Checksum,
    /// Unknown compression id.
    Compression(u8),
    /// Inflating failed or exceeded the limit.
    Inflate,
    /// Malformed packet list.
    Malformed(io::Error),
    /// More packets than allowed in one batch.
    TooManyPackets,
}

impl std::fmt::Display for BatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Checksum => f.write_str("bad batch checksum"),
            Self::Compression(id) => write!(f, "unsupported compression {id}"),
            Self::Inflate => f.write_str("batch does not inflate within the limit"),
            Self::Malformed(e) => write!(f, "malformed batch: {e}"),
            Self::TooManyPackets => f.write_str("too many packets in one batch"),
        }
    }
}

/// The batch codec of one connection.
pub struct Codec {
    compression: bool,
    threshold: usize,
    encrypt: Option<Cipher>,
    decrypt: Option<Cipher>,
    max_inflated: usize,
    max_packets: usize,
    deflater: libdeflater::Compressor,
    scratch: Vec<u8>,
}

impl std::fmt::Debug for Codec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Codec")
            .field("compression", &self.compression)
            .field("encrypted", &self.encrypt.is_some())
            .finish_non_exhaustive()
    }
}

impl Codec {
    /// A codec before `network_settings`: no compression, no encryption.
    pub fn new(threshold: u16, level: i32, max_inflated: usize, max_packets: usize) -> Self {
        let level = libdeflater::CompressionLvl::new(level).unwrap_or_default();
        Self {
            compression: false,
            threshold: usize::from(threshold),
            encrypt: None,
            decrypt: None,
            max_inflated,
            max_packets,
            deflater: libdeflater::Compressor::new(level),
            scratch: Vec::new(),
        }
    }

    /// Compression on (both directions) after `network_settings`.
    pub fn enable_compression(&mut self) {
        self.compression = true;
    }

    /// Encryption on (both directions) after `server_to_client_handshake`.
    pub fn enable_encryption(&mut self, key: [u8; 32]) {
        self.encrypt = Some(Cipher::new(key));
        self.decrypt = Some(Cipher::new(key));
    }

    /// Turns a received batch into packets (each `header VarInt + body`).
    pub fn decode(&mut self, mut data: Vec<u8>, out: &mut Vec<Vec<u8>>) -> Result<(), BatchError> {
        if let Some(c) = &mut self.decrypt {
            c.decrypt(&mut data)?;
        }
        let plain: &[u8] = if self.compression {
            let (&id, rest) = data.split_first().ok_or(BatchError::Inflate)?;
            match id {
                COMPRESSION_NONE => rest,
                COMPRESSION_DEFLATE => {
                    use std::io::Read;
                    self.scratch.clear();
                    let limit = u64::try_from(self.max_inflated).unwrap_or(u64::MAX);
                    flate2::read::DeflateDecoder::new(rest)
                        .take(limit.saturating_add(1))
                        .read_to_end(&mut self.scratch)
                        .map_err(|_| BatchError::Inflate)?;
                    if self.scratch.len() > self.max_inflated {
                        return Err(BatchError::Inflate);
                    }
                    &self.scratch
                }
                other => return Err(BatchError::Compression(other)),
            }
        } else {
            &data
        };
        let mut r = Reader::new(plain);
        while r.remaining() > 0 {
            if out.len() >= self.max_packets {
                return Err(BatchError::TooManyPackets);
            }
            let packet = r
                .bytes("packet", self.max_inflated)
                .map_err(BatchError::Malformed)?;
            out.push(packet.to_vec());
        }
        if self.scratch.capacity() > 1 << 18 {
            // A large login batch must not pin its buffer for the whole session.
            self.scratch = Vec::new();
        }
        Ok(())
    }

    /// Encodes packets (already `header + body`, VarInt-prefixed and concatenated in `plain`)
    /// into a batch payload.
    pub fn encode(&mut self, plain: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(plain.len().saturating_add(16));
        if self.compression {
            if plain.len() < self.threshold {
                out.push(COMPRESSION_NONE);
                out.extend_from_slice(plain);
            } else {
                out.push(COMPRESSION_DEFLATE);
                let bound = self.deflater.deflate_compress_bound(plain.len());
                out.resize(bound.saturating_add(1), 0);
                let n = self
                    .deflater
                    .deflate_compress(plain, out.get_mut(1..).unwrap_or_default())
                    .unwrap_or(0);
                out.truncate(n.saturating_add(1));
                if n == 0 {
                    out.clear();
                    out.push(COMPRESSION_NONE);
                    out.extend_from_slice(plain);
                }
            }
        } else {
            out.extend_from_slice(plain);
        }
        if let Some(c) = &mut self.encrypt {
            c.encrypt(&mut out);
        }
        out
    }
}

/// Appends one packet (`header + body`) to a batch being built.
pub fn push_packet(batch: &mut Vec<u8>, id: u32, body: &[u8]) {
    let mut header = Vec::with_capacity(3);
    io::write_varu32(&mut header, id & 0x3ff);
    io::write_len(batch, header.len().saturating_add(body.len()));
    batch.extend_from_slice(&header);
    batch.extend_from_slice(body);
}

/// Splits a packet into its id and body.
pub fn split_packet(packet: &[u8]) -> Result<(u32, &[u8]), io::Error> {
    let mut r = Reader::new(packet);
    let header = r.varu32()?;
    Ok((header & 0x3ff, r.rest()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_compressed_and_encrypted() {
        let mut server = Codec::new(16, 6, 1 << 20, 100);
        let mut client = Codec::new(16, 6, 1 << 20, 100);
        for c in [&mut server, &mut client] {
            c.enable_compression();
            c.enable_encryption([7; 32]);
        }
        for round in 0..3u8 {
            let mut plain = Vec::new();
            push_packet(&mut plain, 9, &[round; 300]);
            push_packet(&mut plain, 1, b"x");
            let wire = server.encode(&plain);
            let mut packets = Vec::new();
            client.decode(wire, &mut packets).unwrap();
            assert_eq!(packets.len(), 2);
            assert_eq!(split_packet(&packets[0]).unwrap(), (9, &[round; 300][..]));
        }
    }

    #[test]
    fn rejects_tampering_and_bombs() {
        let mut server = Codec::new(0, 6, 1 << 20, 100);
        let mut client = Codec::new(0, 6, 1 << 10, 100);
        for c in [&mut server, &mut client] {
            c.enable_compression();
        }
        let mut plain = Vec::new();
        push_packet(&mut plain, 9, &[0; 4096]);
        let wire = server.encode(&plain);
        assert_eq!(
            client.decode(wire, &mut Vec::new()),
            Err(BatchError::Inflate)
        );

        let mut a = Codec::new(0, 6, 1 << 20, 100);
        let mut b = Codec::new(0, 6, 1 << 20, 100);
        a.enable_encryption([1; 32]);
        b.enable_encryption([1; 32]);
        let mut wire = a.encode(b"\x01\x01");
        wire[0] ^= 1;
        assert_eq!(b.decode(wire, &mut Vec::new()), Err(BatchError::Checksum));
    }
}
