//! A fixed-length bitset over `u64` words.

/// A bitset with a fixed number of bits, stored in `u64` words (bit `i` is bit `i % 64` of word
/// `i / 64`). Word access allows shift-based bulk operations such as light wavefronts.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BitSet {
    words: Box<[u64]>,
    len: usize,
}

impl BitSet {
    /// A bitset of `len` bits, all clear.
    pub fn new(len: usize) -> Self {
        Self {
            words: vec![0; len.div_ceil(64)].into_boxed_slice(),
            len,
        }
    }

    /// Number of bits.
    #[inline]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether the bitset has no bits.
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The bit at `index`; `false` when out of range.
    #[inline]
    pub fn get(&self, index: usize) -> bool {
        index < self.len && self.words[index / 64] & (1 << (index % 64)) != 0
    }

    /// Sets the bit at `index`. Panics when out of range.
    #[inline]
    pub fn set(&mut self, index: usize) {
        assert!(index < self.len, "bit index out of range");
        self.words[index / 64] |= 1 << (index % 64);
    }

    /// Clears the bit at `index`. Panics when out of range.
    #[inline]
    pub fn clear(&mut self, index: usize) {
        assert!(index < self.len, "bit index out of range");
        self.words[index / 64] &= !(1 << (index % 64));
    }

    /// Clears all bits.
    #[inline]
    pub fn clear_all(&mut self) {
        self.words.fill(0);
    }

    /// Number of set bits.
    #[inline]
    pub fn count_ones(&self) -> usize {
        self.words.iter().map(|w| w.count_ones() as usize).sum()
    }

    /// Whether no bit is set.
    #[inline]
    pub fn none(&self) -> bool {
        self.words.iter().all(|&w| w == 0)
    }

    /// Indices of set bits in ascending order.
    pub fn iter_ones(&self) -> impl Iterator<Item = usize> + '_ {
        self.words.iter().enumerate().flat_map(|(i, &word)| {
            let mut w = word;
            std::iter::from_fn(move || {
                if w == 0 {
                    return None;
                }
                let bit = w.trailing_zeros() as usize;
                w &= w - 1;
                Some(i * 64 + bit)
            })
        })
    }

    /// `self |= other`. Panics if the lengths differ.
    pub fn union_with(&mut self, other: &Self) {
        assert_eq!(self.len, other.len, "bitset lengths differ");
        for (a, b) in self.words.iter_mut().zip(other.words.iter()) {
            *a |= b;
        }
    }

    /// `self &= other`. Panics if the lengths differ.
    pub fn intersect_with(&mut self, other: &Self) {
        assert_eq!(self.len, other.len, "bitset lengths differ");
        for (a, b) in self.words.iter_mut().zip(other.words.iter()) {
            *a &= b;
        }
    }

    /// The words. Bits past `len` in the last word are always clear.
    #[inline]
    pub fn words(&self) -> &[u64] {
        &self.words
    }

    /// The words, mutably. Callers must keep bits past `len` clear.
    #[inline]
    pub fn words_mut(&mut self) -> &mut [u64] {
        &mut self.words
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_iterate() {
        let mut bits = BitSet::new(4096);
        for i in [0, 63, 64, 1000, 4095] {
            bits.set(i);
        }
        assert!(bits.get(64));
        assert!(!bits.get(65));
        assert!(!bits.get(5000));
        assert_eq!(bits.count_ones(), 5);
        assert_eq!(
            bits.iter_ones().collect::<Vec<_>>(),
            [0, 63, 64, 1000, 4095]
        );
        bits.clear(63);
        assert!(!bits.get(63));
    }
}
