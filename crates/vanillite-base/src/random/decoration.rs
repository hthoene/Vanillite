//! `WorldgenRandom` over a `XoroshiroRandomSource`, as feature and structure placement use it.

use super::{
    DOUBLE_MULTIPLIER, FLOAT_MULTIPLIER, Gaussian, PositionalRandomFactory, RandomSource,
    XoroshiroPositionalFactory, XoroshiroRandom,
};

/// Vanilla's `WorldgenRandom` wrapping a Xoroshiro source.
///
/// `WorldgenRandom` extends `LegacyRandomSource` and only replaces `next(bits)` (the top `bits`
/// bits of the source's next long), so every derived method runs the LCG's algorithms on top of
/// Xoroshiro output. `setSeed` re-seeds the source but never resets the Gaussian cache.
///
/// `fork()` and `forkPositional()` hand out plain Xoroshiro generators; those are values of this
/// type in plain mode, which delegate every call to the source unchanged.
#[derive(Clone, Debug)]
pub struct DecorationRandom {
    source: XoroshiroRandom,
    gaussian: Gaussian,
    plain: bool,
}

impl DecorationRandom {
    /// `new WorldgenRandom(new XoroshiroRandomSource(seed))`.
    pub fn new(seed: i64) -> Self {
        Self {
            source: XoroshiroRandom::new(seed),
            gaussian: Gaussian::default(),
            plain: false,
        }
    }

    fn plain(source: XoroshiroRandom) -> Self {
        Self {
            source,
            gaussian: Gaussian::default(),
            plain: true,
        }
    }

    /// `next(bits)`, `bits` in `1..=32`.
    #[inline]
    pub fn next_bits(&mut self, bits: u32) -> i32 {
        if self.plain {
            return self.source.next_bits(bits) as i32;
        }
        ((self.source.next_long() as u64) >> (64 - bits)) as i32
    }

    /// `setDecorationSeed(seed, x, z)`: returns the decoration seed.
    pub fn set_decoration_seed(&mut self, seed: i64, x: i32, z: i32) -> i64 {
        self.set_seed(seed);
        let x_scale = self.next_long() | 1;
        let z_scale = self.next_long() | 1;
        let result = (i64::from(x).wrapping_mul(x_scale))
            .wrapping_add(i64::from(z).wrapping_mul(z_scale))
            ^ seed;
        self.set_seed(result);
        result
    }

    /// `setFeatureSeed(seed, index, step)`.
    pub fn set_feature_seed(&mut self, seed: i64, index: i32, step: i32) {
        self.set_seed(
            seed.wrapping_add(i64::from(index))
                .wrapping_add(i64::from(10000_i32.wrapping_mul(step))),
        );
    }

    /// `setLargeFeatureSeed(seed, chunkX, chunkZ)`.
    pub fn set_large_feature_seed(&mut self, seed: i64, chunk_x: i32, chunk_z: i32) {
        self.set_seed(seed);
        let x_scale = self.next_long();
        let z_scale = self.next_long();
        self.set_seed(
            i64::from(chunk_x).wrapping_mul(x_scale)
                ^ i64::from(chunk_z).wrapping_mul(z_scale)
                ^ seed,
        );
    }

    /// `setLargeFeatureWithSalt(seed, x, z, salt)`.
    pub fn set_large_feature_with_salt(&mut self, seed: i64, x: i32, z: i32, salt: i32) {
        self.set_seed(
            i64::from(x)
                .wrapping_mul(341_873_128_712)
                .wrapping_add(i64::from(z).wrapping_mul(132_897_987_541))
                .wrapping_add(seed)
                .wrapping_add(i64::from(salt)),
        );
    }
}

impl RandomSource for DecorationRandom {
    type Positional = DecorationPositional;

    fn set_seed(&mut self, seed: i64) {
        self.source.set_seed(seed);
    }

    fn fork(&mut self) -> Self {
        Self::plain(self.source.fork())
    }

    fn fork_positional(&mut self) -> DecorationPositional {
        DecorationPositional(self.source.fork_positional())
    }

    #[inline]
    fn next_int(&mut self) -> i32 {
        if self.plain {
            return self.source.next_int();
        }
        self.next_bits(32)
    }

    fn next_int_bounded(&mut self, bound: i32) -> i32 {
        if self.plain {
            return self.source.next_int_bounded(bound);
        }
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
        if self.plain {
            return self.source.next_long();
        }
        let hi = i64::from(self.next_bits(32));
        let lo = i64::from(self.next_bits(32));
        (hi << 32).wrapping_add(lo)
    }

    #[inline]
    fn next_boolean(&mut self) -> bool {
        if self.plain {
            return self.source.next_boolean();
        }
        self.next_bits(1) != 0
    }

    #[inline]
    fn next_float(&mut self) -> f32 {
        if self.plain {
            return self.source.next_float();
        }
        self.next_bits(24) as f32 * FLOAT_MULTIPLIER
    }

    #[inline]
    fn next_double(&mut self) -> f64 {
        if self.plain {
            return self.source.next_double();
        }
        let hi = i64::from(self.next_bits(26));
        let lo = i64::from(self.next_bits(27));
        ((hi << 27) + lo) as f64 * DOUBLE_MULTIPLIER
    }

    fn next_gaussian(&mut self) -> f64 {
        if self.plain {
            return self.source.next_gaussian();
        }
        let mut gaussian = std::mem::take(&mut self.gaussian);
        let value = gaussian.next(|| self.next_double());
        self.gaussian = gaussian;
        value
    }

    fn consume_count(&mut self, count: i32) {
        if self.plain {
            return self.source.consume_count(count);
        }
        for _ in 0..count {
            self.next_bits(32);
        }
    }
}

/// The positional factory of a [`DecorationRandom`]: plain Xoroshiro generators.
#[derive(Clone, Copy, Debug)]
pub struct DecorationPositional(pub XoroshiroPositionalFactory);

impl PositionalRandomFactory for DecorationPositional {
    type Random = DecorationRandom;

    fn at(&self, x: i32, y: i32, z: i32) -> DecorationRandom {
        DecorationRandom::plain(self.0.at(x, y, z))
    }

    fn with_hash_of(&self, name: &str) -> DecorationRandom {
        DecorationRandom::plain(self.0.with_hash_of(name))
    }

    fn with_seed(&self, seed: i64) -> DecorationRandom {
        DecorationRandom::plain(self.0.with_seed(seed))
    }
}
