//! AES-128-CFB8, the stream cipher of the Java protocol (key = IV = the shared secret).
//!
//! Encryption is serial by nature (each byte feeds the next block). Decryption knows every
//! ciphertext byte up front, so the keystream blocks of a whole buffer are computed in batches
//! that the AES hardware pipelines.

use aes::Aes128;
use aes::cipher::{BlockEncrypt, KeyInit, generic_array::GenericArray};

/// One direction of a CFB8 stream.
#[derive(Clone)]
pub struct Cfb8 {
    aes: Aes128,
    register: [u8; 16],
}

impl std::fmt::Debug for Cfb8 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cfb8(..)")
    }
}

/// Blocks per decryption batch.
const BATCH: usize = 64;

impl Cfb8 {
    /// A stream with the Minecraft convention: key and IV are both the 16-byte shared secret.
    pub fn new(secret: &[u8; 16]) -> Self {
        Self {
            aes: Aes128::new(GenericArray::from_slice(secret)),
            register: *secret,
        }
    }

    fn keystream_byte(&self) -> u8 {
        let mut block = GenericArray::clone_from_slice(&self.register);
        self.aes.encrypt_block(&mut block);
        block.first().copied().unwrap_or(0)
    }

    fn shift_in(&mut self, c: u8) {
        self.register.copy_within(1.., 0);
        if let Some(last) = self.register.last_mut() {
            *last = c;
        }
    }

    /// Encrypts `data` in place.
    pub fn encrypt(&mut self, data: &mut [u8]) {
        for b in data {
            let c = *b ^ self.keystream_byte();
            *b = c;
            self.shift_in(c);
        }
    }

    /// Decrypts `data` in place.
    pub fn decrypt(&mut self, data: &mut [u8]) {
        let mut blocks = [GenericArray::<u8, aes::cipher::consts::U16>::default(); BATCH];
        // The register before byte i is the 16 ciphertext bytes before it; build the
        // registers of a batch from the previous register and the ciphertext, then encrypt
        // them all at once.
        let mut window = [0u8; 16 + BATCH];
        for chunk in data.chunks_mut(BATCH) {
            let n = chunk.len();
            if let Some(w) = window.get_mut(..16) {
                w.copy_from_slice(&self.register);
            }
            if let Some(w) = window.get_mut(16..16usize.saturating_add(n)) {
                w.copy_from_slice(chunk);
            }
            for (i, block) in blocks.iter_mut().take(n).enumerate() {
                if let Some(src) = window.get(i..i.saturating_add(16)) {
                    block.copy_from_slice(src);
                }
            }
            if let Some(batch) = blocks.get_mut(..n) {
                self.aes.encrypt_blocks(batch);
            }
            for (b, k) in chunk.iter_mut().zip(blocks.iter()) {
                *b ^= k.first().copied().unwrap_or(0);
            }
            if let Some(src) = window.get(n..n.saturating_add(16)) {
                self.register.copy_from_slice(src);
            }
        }
    }
}
