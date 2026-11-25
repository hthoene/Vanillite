//! The Java LCG (`java.util.Random`), as Vanilla's `LegacyRandomSource` uses it.

use super::{DOUBLE_MULTIPLIER, FLOAT_MULTIPLIER, Gaussian, PositionalRandomFactory, RandomSource};
use crate::math::{get_seed, java_string_hash};

const MULTIPLIER: i64 = 0x5_DEEC_E66D;
const INCREMENT: i64 = 0xB;
const MASK: i64 = (1 << 48) - 1;

/// Java's 48-bit linear congruential generator (`LegacyRandomSource`).
#[derive(Clone, Debug)]
pub struct LegacyRandom {
    seed: i64,
    gaussian: Gaussian,
}

impl LegacyRandom {
    /// A generator with the given seed, as `new Random(seed)`.
    pub fn new(seed: i64) -> Self {
        Self {
            seed: (seed ^ MULTIPLIER) & MASK,
            gaussian: Gaussian::default(),
        }
    }

    /// A random with the 48-bit `state` of [`Self::state`] (no scrambling).
    pub fn from_state(state: i64) -> Self {
        LegacyRandom {
            seed: state & MASK,
            gaussian: Gaussian::default(),
        }
    }

    /// The 48-bit state (`LegacyRandomSource.seed`): equal states give equal sequences. The
    /// parity traces compare it to find the first diverging random call.

    pub fn state(&self) -> i64 {
        self.seed
    }

    /// `next(bits)`: the top `bits` bits of the next state, `bits` in `1..=32`.
    #[inline]
    pub fn next_bits(&mut self, bits: u32) -> i32 {
        self.seed = self.seed.wrapping_mul(MULTIPLIER).wrapping_add(INCREMENT) & MASK;
        (self.seed >> (48 - bits)) as i32
    }
}

impl RandomSource for LegacyRandom {
    type Positional = LegacyPositionalFactory;

    fn set_seed(&mut self, seed: i64) {
        self.seed = (seed ^ MULTIPLIER) & MASK;
        self.gaussian.reset();
    }

    fn fork(&mut self) -> Self {
        Self::new(self.next_long())
    }

    fn fork_positional(&mut self) -> LegacyPositionalFactory {
        LegacyPositionalFactory::new(self.next_long())
    }

    #[inline]
    fn next_int(&mut self) -> i32 {
        self.next_bits(32)
    }

    fn next_int_bounded(&mut self, bound: i32) -> i32 {
        assert!(bound > 0, "bound must be positive");
        if bound & (bound - 1) == 0 {
            return ((i64::from(bound) * i64::from(self.next_bits(31))) >> 31) as i32;
        }
        loop {
            let bits = self.next_bits(31);
            let value = bits % bound;
            if bits.wrapping_sub(value).wrapping_add(bound - 1) >= 0 {
                return value;
            }
        }
    }

    #[inline]
    fn next_long(&mut self) -> i64 {
        let hi = i64::from(self.next_bits(32));
        let lo = i64::from(self.next_bits(32));
        (hi << 32).wrapping_add(lo)
    }

    #[inline]
    fn next_boolean(&mut self) -> bool {
        self.next_bits(1) != 0
    }

    #[inline]
    fn next_float(&mut self) -> f32 {
        self.next_bits(24) as f32 * FLOAT_MULTIPLIER
    }

    #[inline]
    fn next_double(&mut self) -> f64 {
        let hi = i64::from(self.next_bits(26));
        let lo = i64::from(self.next_bits(27));
        ((hi << 27) + lo) as f64 * DOUBLE_MULTIPLIER
    }

    fn next_gaussian(&mut self) -> f64 {
        let mut gaussian = std::mem::take(&mut self.gaussian);
        let value = gaussian.next(|| self.next_double());
        self.gaussian = gaussian;
        value
    }

    fn consume_count(&mut self, count: i32) {
        for _ in 0..count {
            self.next_bits(32);
        }
    }
}

/// `LegacyRandomSource.LegacyPositionalRandomFactory`.
#[derive(Clone, Copy, Debug)]
pub struct LegacyPositionalFactory {
    seed: i64,
}

impl LegacyPositionalFactory {
    /// A factory with the given seed.
    pub const fn new(seed: i64) -> Self {
        Self { seed }
    }
}

impl PositionalRandomFactory for LegacyPositionalFactory {
    type Random = LegacyRandom;

    fn at(&self, x: i32, y: i32, z: i32) -> LegacyRandom {
        LegacyRandom::new(get_seed(x, y, z) ^ self.seed)
    }

    fn with_hash_of(&self, name: &str) -> LegacyRandom {
        LegacyRandom::new(i64::from(java_string_hash(name)) ^ self.seed)
    }

    fn with_seed(&self, seed: i64) -> LegacyRandom {
        LegacyRandom::new(seed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_java_util_random() {
        // Values of `new java.util.Random(seed)`.
        assert_eq!(LegacyRandom::new(0).next_int(), -1_155_484_576);
        assert_eq!(LegacyRandom::new(42).next_int(), -1_170_105_035);
        assert_eq!(LegacyRandom::new(0).next_long(), -4_962_768_465_676_381_896);
        assert_eq!(LegacyRandom::new(0).next_float(), 0.730_967_76);
        assert!(LegacyRandom::new(0).next_boolean());
    }

    #[test]
    fn bounded_int_in_range() {
        let mut random = LegacyRandom::new(1234);
        for bound in [1, 2, 3, 7, 16, 100, i32::MAX] {
            for _ in 0..100 {
                let value = random.next_int_bounded(bound);
                assert!((0..bound).contains(&value));
            }
        }
    }

    #[test]
    fn set_seed_restarts_sequence() {
        let mut random = LegacyRandom::new(99);
        let first = random.next_long();
        random.set_seed(99);
        assert_eq!(random.next_long(), first);
    }
}
