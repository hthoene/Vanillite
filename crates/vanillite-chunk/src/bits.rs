//! Packed integer arrays in Vanilla's `SimpleBitStorage` layout.
//!
//! Entries never span two words; a word holds `64 / bits` entries, the first in the lowest bits.
//! Words are stored as big-endian bytes, which is the wire layout, so encoding is a plain copy.

/// One packed 64-bit word, big-endian.
pub type Word = [u8; 8];

#[derive(Clone, Copy)]
struct Divisor {
    per_word: u32,
    magic: u32,
}

/// Exact division by `per_word` for indices below 4096: `i / d == (i * ceil(2^22 / d)) >> 22`,
/// because the rounding error `i * (ceil(2^22 / d) * d - 2^22) < 4096 * 64 < 2^22`.
const MAGIC_SHIFT: u32 = 22;

const DIVISORS: [Divisor; 33] = {
    let mut table = [Divisor {
        per_word: 0,
        magic: 0,
    }; 33];
    let mut bits = 1;
    while bits <= 32 {
        let per_word = 64 / bits;
        table[bits as usize] = Divisor {
            per_word,
            magic: (1u32 << MAGIC_SHIFT).div_ceil(per_word),
        };
        bits += 1;
    }
    table
};

/// Largest index the fast division supports.
pub const MAX_ENTRIES: usize = 4096;

/// Entries per word for `bits` (1..=32).
#[inline]
pub const fn per_word(bits: u8) -> usize {
    64 / bits as usize
}

/// Number of words for `entries` entries of `bits` bits.
#[inline]
pub const fn words_for(entries: usize, bits: u8) -> usize {
    entries.div_ceil(per_word(bits))
}

/// Zeroed storage for `entries` entries of `bits` bits.
pub fn zeroed(entries: usize, bits: u8) -> Box<[Word]> {
    vec![[0u8; 8]; words_for(entries, bits)].into_boxed_slice()
}

#[inline]
fn locate(index: usize, bits: u8) -> (usize, u32) {
    debug_assert!(index < MAX_ENTRIES && (1..=32).contains(&bits));
    let divisor = DIVISORS[usize::from(bits)];
    let index = index as u32;
    let word = ((u64::from(index) * u64::from(divisor.magic)) >> MAGIC_SHIFT) as u32;
    let shift = (index - word * divisor.per_word) * u32::from(bits);
    (word as usize, shift)
}

#[inline]
const fn mask(bits: u8) -> u64 {
    (1u64 << bits) - 1
}

/// Reads entry `index`.
#[inline]
pub fn get(data: &[Word], bits: u8, index: usize) -> u32 {
    let (word, shift) = locate(index, bits);
    ((u64::from_be_bytes(data[word]) >> shift) & mask(bits)) as u32
}

/// Writes entry `index` and returns the previous value.
#[inline]
pub fn get_and_set(data: &mut [Word], bits: u8, index: usize, value: u32) -> u32 {
    let (word, shift) = locate(index, bits);
    let slot = &mut data[word];
    let current = u64::from_be_bytes(*slot);
    let old = (current >> shift) & mask(bits);
    let new = (current & !(mask(bits) << shift)) | ((u64::from(value) & mask(bits)) << shift);
    *slot = new.to_be_bytes();
    old as u32
}

/// Writes entry `index`.
#[inline]
pub fn set(data: &mut [Word], bits: u8, index: usize, value: u32) {
    get_and_set(data, bits, index, value);
}

/// Packs `values` into fresh storage of `bits` bits.
pub fn pack(entries: usize, bits: u8, mut value_at: impl FnMut(usize) -> u32) -> Box<[Word]> {
    let per = per_word(bits);
    let mut data = zeroed(entries, bits);
    for (word_index, word) in data.iter_mut().enumerate() {
        let start = word_index * per;
        let end = (start + per).min(entries);
        let mut acc = 0u64;
        for (slot, index) in (start..end).enumerate() {
            acc |= (u64::from(value_at(index)) & mask(bits)) << (slot * usize::from(bits));
        }
        *word = acc.to_be_bytes();
    }
    data
}

/// Calls `f` with every entry in index order.
#[inline]
pub fn for_each(data: &[Word], bits: u8, entries: usize, mut f: impl FnMut(usize, u32)) {
    let per = per_word(bits);
    let mut index = 0;
    for word in data {
        let mut value = u64::from_be_bytes(*word);
        for _ in 0..per {
            if index == entries {
                return;
            }
            f(index, (value & mask(bits)) as u32);
            value >>= bits;
            index += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_division_is_exact() {
        for bits in 1..=16u8 {
            let per = per_word(bits);
            for index in 0..MAX_ENTRIES {
                let (word, shift) = locate(index, bits);
                assert_eq!(word, index / per, "bits {bits} index {index}");
                assert_eq!(shift as usize, (index % per) * usize::from(bits));
            }
        }
    }

    #[test]
    fn layout_matches_simple_bit_storage() {
        // Three 5-bit values: 12 per word, value 0 in the lowest bits of word 0.
        let mut data = zeroed(4096, 5);
        set(&mut data, 5, 0, 0b10101);
        set(&mut data, 5, 1, 0b00011);
        set(&mut data, 5, 12, 0b11111);
        assert_eq!(u64::from_be_bytes(data[0]), 0b00011_10101);
        assert_eq!(u64::from_be_bytes(data[1]), 0b11111);
        assert_eq!(data.len(), 342);
        let packed = pack(4096, 5, |i| get(&data, 5, i));
        assert_eq!(packed, data);
        let mut seen = 0;
        for_each(&data, 5, 4096, |i, v| {
            assert_eq!(v, get(&data, 5, i));
            seen += 1;
        });
        assert_eq!(seen, 4096);
    }
}
