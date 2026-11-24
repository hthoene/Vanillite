//! Xoroshiro128++ (`XoroshiroRandomSource`) and Vanilla's 128-bit seeding (`RandomSupport`).

use md5::{Digest, Md5};

use super::{DOUBLE_MULTIPLIER, FLOAT_MULTIPLIER, Gaussian, PositionalRandomFactory, RandomSource};
use crate::math::get_seed;

/// `RandomSupport.GOLDEN_RATIO_64`.
pub const GOLDEN_RATIO_64: i64 = 0x9E37_79B9_7F4A_7C15_u64 as i64;
/// `RandomSupport.SILVER_RATIO_64`.
pub const SILVER_RATIO_64: i64 = 0x6A09_E667_F3BC_C909;

/// `RandomSupport.mixStafford13`.
#[inline]
pub const fn mix_stafford13(z: i64) -> i64 {
    let mut z = z as u64;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    (z ^ (z >> 31)) as i64
}

/// `RandomSupport.Seed128bit`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Seed128 {
    /// Low 64 bits.
    pub lo: i64,
    /// High 64 bits.
    pub hi: i64,
}

impl Seed128 {
    /// `upgradeSeedTo128bitUnmixed`.
    pub const fn upgrade_unmixed(seed: i64) -> Self {
        let lo = seed ^ SILVER_RATIO_64;
        Self {
            lo,
            hi: lo.wrapping_add(GOLDEN_RATIO_64),
        }
    }

    /// `upgradeSeedTo128bit`.
    pub const fn upgrade(seed: i64) -> Self {
        Self::upgrade_unmixed(seed).mixed()
    }

    /// `seedFromHashOf(string)`: the MD5 of the UTF-8 bytes, read as two big-endian longs.
    pub fn from_hash_of(name: &str) -> Self {
        let digest: [u8; 16] = Md5::digest(name.as_bytes()).into();
        let mut lo = [0; 8];
        let mut hi = [0; 8];
        lo.copy_from_slice(&digest[..8]);
        hi.copy_from_slice(&digest[8..]);
        Self {
            lo: i64::from_be_bytes(lo),
            hi: i64::from_be_bytes(hi),
        }
    }

    /// `mixed()`.
    pub const fn mixed(self) -> Self {
        Self {
            lo: mix_stafford13(self.lo),
            hi: mix_stafford13(self.hi),
        }
    }

    /// `xor(lo, hi)`.
    pub const fn xor(self, lo: i64, hi: i64) -> Self {
        Self {
            lo: self.lo ^ lo,
            hi: self.hi ^ hi,
        }
    }
}

/// The raw Xoroshiro128++ generator (`Xoroshiro128PlusPlus`).
#[derive(Clone, Copy, Debug)]
pub struct Xoroshiro128PlusPlus {
    lo: u64,
    hi: u64,
}

impl Xoroshiro128PlusPlus {
    /// A generator with the given state; an all-zero state is replaced as in Vanilla.
    pub const fn new(seed: Seed128) -> Self {
        if seed.lo == 0 && seed.hi == 0 {
            Self {
                lo: GOLDEN_RATIO_64 as u64,
                hi: SILVER_RATIO_64 as u64,
            }
        } else {
            Self {
                lo: seed.lo as u64,
                hi: seed.hi as u64,
            }
        }
    }

    /// `nextLong()`.
    #[inline]
    pub fn next_long(&mut self) -> i64 {
        let l = self.lo;
        let mut m = self.hi;
        let n = l.wrapping_add(m).rotate_left(17).wrapping_add(l);
        m ^= l;
        self.lo = l.rotate_left(49) ^ m ^ (m << 21);
        self.hi = m.rotate_left(28);
        n as i64
    }
}

/// Vanilla's `XoroshiroRandomSource`.
#[derive(Clone, Debug)]
pub struct XoroshiroRandom {
    rng: Xoroshiro128PlusPlus,
    gaussian: Gaussian,
}

impl XoroshiroRandom {
    /// A generator from a 64-bit seed, upgraded and mixed to 128 bits.
    pub fn new(seed: i64) -> Self {
        Self::from_seed128(Seed128::upgrade(seed))
    }

    /// A generator from two longs, used as the state unmixed.
    pub fn from_longs(lo: i64, hi: i64) -> Self {
        Self::from_seed128(Seed128 { lo, hi })
    }

    /// A generator from a 128-bit state, used unmixed.
    pub fn from_seed128(seed: Seed128) -> Self {
        Self {
            rng: Xoroshiro128PlusPlus::new(seed),
            gaussian: Gaussian::default(),
        }
    }

    /// `nextBits(bits)`: the top `bits` bits of the next long, `bits` in `1..=64`.
    #[inline]
    pub fn next_bits(&mut self, bits: u32) -> i64 {
        ((self.rng.next_long() as u64) >> (64 - bits)) as i64
    }
}

impl RandomSource for XoroshiroRandom {
    type Positional = XoroshiroPositionalFactory;

    fn set_seed(&mut self, seed: i64) {
        self.rng = Xoroshiro128PlusPlus::new(Seed128::upgrade(seed));
        self.gaussian.reset();
    }

    fn fork(&mut self) -> Self {
        let lo = self.rng.next_long();
        let hi = self.rng.next_long();
        Self::from_longs(lo, hi)
    }

    fn fork_positional(&mut self) -> XoroshiroPositionalFactory {
        let lo = self.rng.next_long();
        let hi = self.rng.next_long();
        XoroshiroPositionalFactory::new(lo, hi)
    }

    #[inline]
    fn next_int(&mut self) -> i32 {
        self.rng.next_long() as i32
    }

    fn next_int_bounded(&mut self, bound: i32) -> i32 {
        assert!(bound > 0, "bound must be positive");
        let bound = u64::from(bound.unsigned_abs());
        let mut m = u64::from(self.next_int() as u32) * bound;
        let mut n = m & 0xFFFF_FFFF;
        if n < bound {
            // Integer.remainderUnsigned(~bound + 1, bound)
            let threshold = u64::from((bound as u32).wrapping_neg() % bound as u32);
            while n < threshold {
                m = u64::from(self.next_int() as u32) * bound;
                n = m & 0xFFFF_FFFF;
            }
        }
        (m >> 32) as i32
    }

    #[inline]
    fn next_long(&mut self) -> i64 {
        self.rng.next_long()
    }

    #[inline]
    fn next_boolean(&mut self) -> bool {
        self.rng.next_long() & 1 != 0
    }

    #[inline]
    fn next_float(&mut self) -> f32 {
        self.next_bits(24) as f32 * FLOAT_MULTIPLIER
    }

    #[inline]
    fn next_double(&mut self) -> f64 {
        self.next_bits(53) as f64 * DOUBLE_MULTIPLIER
    }

    fn next_gaussian(&mut self) -> f64 {
        let mut gaussian = std::mem::take(&mut self.gaussian);
        let value = gaussian.next(|| self.next_double());
        self.gaussian = gaussian;
        value
    }

    fn consume_count(&mut self, count: i32) {
        for _ in 0..count {
            self.rng.next_long();
        }
    }
}

/// `XoroshiroRandomSource.XoroshiroPositionalRandomFactory`.
#[derive(Clone, Copy, Debug)]
pub struct XoroshiroPositionalFactory {
    seed_lo: i64,
    seed_hi: i64,
}

impl XoroshiroPositionalFactory {
    /// A factory with the given 128-bit seed.
    pub const fn new(seed_lo: i64, seed_hi: i64) -> Self {
        Self { seed_lo, seed_hi }
    }
}

impl PositionalRandomFactory for XoroshiroPositionalFactory {
    type Random = XoroshiroRandom;

    fn at(&self, x: i32, y: i32, z: i32) -> XoroshiroRandom {
        XoroshiroRandom::from_longs(get_seed(x, y, z) ^ self.seed_lo, self.seed_hi)
    }

    fn with_hash_of(&self, name: &str) -> XoroshiroRandom {
        XoroshiroRandom::from_seed128(Seed128::from_hash_of(name).xor(self.seed_lo, self.seed_hi))
    }

    fn with_seed(&self, seed: i64) -> XoroshiroRandom {
        XoroshiroRandom::from_longs(seed ^ self.seed_lo, seed ^ self.seed_hi)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xoroshiro128_plus_plus_reference_step() {
        // Reference algorithm with state (1, 2): rotl(1 + 2, 17) + 1.
        let mut rng = Xoroshiro128PlusPlus::new(Seed128 { lo: 1, hi: 2 });
        assert_eq!(rng.next_long(), (3_i64 << 17) + 1);
    }

    #[test]
    fn zero_state_is_replaced() {
        let mut zero = Xoroshiro128PlusPlus::new(Seed128 { lo: 0, hi: 0 });
        let mut golden = Xoroshiro128PlusPlus::new(Seed128 {
            lo: GOLDEN_RATIO_64,
            hi: SILVER_RATIO_64,
        });
        assert_eq!(zero.next_long(), golden.next_long());
    }

    #[test]
    fn hash_seed_is_md5_big_endian() {
        // MD5("") = d41d8cd98f00b204 e9800998ecf8427e
        let seed = Seed128::from_hash_of("");
        assert_eq!(seed.lo, 0xd41d_8cd9_8f00_b204_u64 as i64);
        assert_eq!(seed.hi, 0xe980_0998_ecf8_427e_u64 as i64);
    }

    #[test]
    fn bounded_and_unit_ranges() {
        let mut random = XoroshiroRandom::new(42);
        for bound in [1, 2, 3, 7, 16, 100, i32::MAX] {
            for _ in 0..100 {
                assert!((0..bound).contains(&random.next_int_bounded(bound)));
            }
        }
        for _ in 0..1000 {
            let d = random.next_double();
            assert!((0.0..1.0).contains(&d));
            let f = random.next_float();
            assert!((0.0..1.0).contains(&f));
        }
    }

    #[test]
    fn positional_is_deterministic() {
        let factory = XoroshiroRandom::new(1).fork_positional();
        assert_eq!(
            factory.at(1, 2, 3).next_long(),
            factory.at(1, 2, 3).next_long()
        );
        assert_ne!(
            factory.at(1, 2, 3).next_long(),
            factory.at(1, 2, 4).next_long()
        );
        assert_eq!(
            factory.with_hash_of("minecraft:ore").next_long(),
            factory.with_hash_of("minecraft:ore").next_long()
        );
    }
}
