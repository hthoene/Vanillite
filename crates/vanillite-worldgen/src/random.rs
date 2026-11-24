//! Runtime choice between Vanilla's two world generation RNGs (`WorldgenRandom.Algorithm`), and
//! `WorldgenRandom`'s seeding helpers.
//!
//! The overworld uses Xoroshiro, the nether and the end use the legacy LCG
//! (`legacy_random_source` in the noise settings). Both are wrapped in one enum so the generator is
//! not generic over the algorithm; the match is cheap next to the noise work behind every call.

use vanillite_base::random::{
    LegacyPositionalFactory, LegacyRandom, PositionalRandomFactory, RandomSource,
    XoroshiroPositionalFactory, XoroshiroRandom,
};

/// A legacy or Xoroshiro generator.
#[derive(Clone, Debug)]
pub enum AnyRandom {
    Legacy(LegacyRandom),
    Xoroshiro(XoroshiroRandom),
}

/// A legacy or Xoroshiro positional factory.
#[derive(Clone, Copy, Debug)]
pub enum AnyPositional {
    Legacy(LegacyPositionalFactory),
    Xoroshiro(XoroshiroPositionalFactory),
}

impl AnyRandom {
    /// `WorldgenRandom.Algorithm.newInstance(seed)`.
    pub fn new(legacy: bool, seed: i64) -> Self {
        if legacy {
            Self::Legacy(LegacyRandom::new(seed))
        } else {
            Self::Xoroshiro(XoroshiroRandom::new(seed))
        }
    }
}

macro_rules! dispatch {
    ($self:expr, $r:ident => $e:expr) => {
        match $self {
            AnyRandom::Legacy($r) => $e,
            AnyRandom::Xoroshiro($r) => $e,
        }
    };
}

impl RandomSource for AnyRandom {
    type Positional = AnyPositional;

    fn set_seed(&mut self, seed: i64) {
        dispatch!(self, r => r.set_seed(seed));
    }

    fn fork(&mut self) -> Self {
        match self {
            Self::Legacy(r) => Self::Legacy(r.fork()),
            Self::Xoroshiro(r) => Self::Xoroshiro(r.fork()),
        }
    }

    fn fork_positional(&mut self) -> AnyPositional {
        match self {
            Self::Legacy(r) => AnyPositional::Legacy(r.fork_positional()),
            Self::Xoroshiro(r) => AnyPositional::Xoroshiro(r.fork_positional()),
        }
    }

    fn next_int(&mut self) -> i32 {
        dispatch!(self, r => r.next_int())
    }

    fn next_int_bounded(&mut self, bound: i32) -> i32 {
        dispatch!(self, r => r.next_int_bounded(bound))
    }

    fn next_long(&mut self) -> i64 {
        dispatch!(self, r => r.next_long())
    }

    fn next_boolean(&mut self) -> bool {
        dispatch!(self, r => r.next_boolean())
    }

    fn next_float(&mut self) -> f32 {
        dispatch!(self, r => r.next_float())
    }

    fn next_double(&mut self) -> f64 {
        dispatch!(self, r => r.next_double())
    }

    fn next_gaussian(&mut self) -> f64 {
        dispatch!(self, r => r.next_gaussian())
    }

    fn consume_count(&mut self, count: i32) {
        dispatch!(self, r => r.consume_count(count));
    }
}

impl PositionalRandomFactory for AnyPositional {
    type Random = AnyRandom;

    fn at(&self, x: i32, y: i32, z: i32) -> AnyRandom {
        match self {
            Self::Legacy(f) => AnyRandom::Legacy(f.at(x, y, z)),
            Self::Xoroshiro(f) => AnyRandom::Xoroshiro(f.at(x, y, z)),
        }
    }

    fn with_hash_of(&self, name: &str) -> AnyRandom {
        match self {
            Self::Legacy(f) => AnyRandom::Legacy(f.with_hash_of(name)),
            Self::Xoroshiro(f) => AnyRandom::Xoroshiro(f.with_hash_of(name)),
        }
    }

    fn with_seed(&self, seed: i64) -> AnyRandom {
        match self {
            Self::Legacy(f) => AnyRandom::Legacy(f.with_seed(seed)),
            Self::Xoroshiro(f) => AnyRandom::Xoroshiro(f.with_seed(seed)),
        }
    }
}

/// `WorldgenRandom` over a `LegacyRandomSource`, as carvers and features use it.
///
/// With a legacy source every call delegates to the LCG, so this is a [`LegacyRandom`] plus
/// Vanilla's seeding helpers.
#[derive(Clone, Debug)]
pub struct WorldgenRandom {
    pub random: LegacyRandom,
}

impl WorldgenRandom {
    /// `new WorldgenRandom(new LegacyRandomSource(seed))`.
    pub fn new(seed: i64) -> Self {
        Self {
            random: LegacyRandom::new(seed),
        }
    }

    /// `setDecorationSeed(seed, chunkX, chunkZ)`.
    pub fn set_decoration_seed(&mut self, seed: i64, chunk_x: i32, chunk_z: i32) -> i64 {
        self.random.set_seed(seed);
        let x_scale = self.random.next_long() | 1;
        let z_scale = self.random.next_long() | 1;
        let result = (i64::from(chunk_x).wrapping_mul(x_scale))
            .wrapping_add(i64::from(chunk_z).wrapping_mul(z_scale))
            ^ seed;
        self.random.set_seed(result);
        result
    }

    /// `setFeatureSeed(seed, index, step)`.
    pub fn set_feature_seed(&mut self, seed: i64, index: i32, step: i32) {
        let result = seed
            .wrapping_add(i64::from(index))
            .wrapping_add(i64::from(10000_i32.wrapping_mul(step)));
        self.random.set_seed(result);
    }

    /// `setLargeFeatureSeed(seed, chunkX, chunkZ)`.
    pub fn set_large_feature_seed(&mut self, seed: i64, chunk_x: i32, chunk_z: i32) {
        self.random.set_seed(seed);
        let x_scale = self.random.next_long();
        let z_scale = self.random.next_long();
        let result = i64::from(chunk_x).wrapping_mul(x_scale)
            ^ i64::from(chunk_z).wrapping_mul(z_scale)
            ^ seed;
        self.random.set_seed(result);
    }

    /// `setLargeFeatureWithSalt(seed, x, z, salt)`.
    pub fn set_large_feature_with_salt(&mut self, seed: i64, x: i32, z: i32, salt: i32) {
        let result = i64::from(x)
            .wrapping_mul(341_873_128_712)
            .wrapping_add(i64::from(z).wrapping_mul(132_897_987_541))
            .wrapping_add(seed)
            .wrapping_add(i64::from(salt));
        self.random.set_seed(result);
    }
}
