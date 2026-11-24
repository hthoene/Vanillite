//! Vanilla's random number generators, bit-exact.
//!
//! - [`LegacyRandom`]: the Java LCG (`java.util.Random` / `LegacyRandomSource`).
//! - [`XoroshiroRandom`]: Xoroshiro128++ (`XoroshiroRandomSource`), used by world generation.
//! - Positional factories ([`PositionalRandomFactory`]) derive independent generators from a
//!   block position, a seed or a string, as Vanilla's `PositionalRandomFactory` does.
//!
//! All methods are statically dispatched; a generator is a few machine words.

mod legacy;
mod xoroshiro;

pub use legacy::{LegacyPositionalFactory, LegacyRandom};
pub use xoroshiro::{Seed128, Xoroshiro128PlusPlus, XoroshiroPositionalFactory, XoroshiroRandom};

/// Vanilla's `nextFloat` multiplier: `5.9604645E-8F` (2⁻²⁴).
pub(crate) const FLOAT_MULTIPLIER: f32 = 5.960_464_5E-8;
/// Vanilla's `nextDouble` multiplier: the float literal `1.110223E-16F` widened to double.
/// It is close to, but not exactly, 2⁻⁵³; Vanilla uses this value, so do we.
pub(crate) const DOUBLE_MULTIPLIER: f64 = 1.110_223E-16_f32 as f64;

/// Vanilla's `RandomSource` interface.
pub trait RandomSource {
    /// The positional factory forked from this generator by [`Self::fork_positional`].
    type Positional: PositionalRandomFactory<Random = Self>;

    /// Re-seeds the generator, as `setSeed(long)`.
    fn set_seed(&mut self, seed: i64);
    /// A new independent generator seeded from this one (`fork()`).
    fn fork(&mut self) -> Self
    where
        Self: Sized;
    /// A positional factory seeded from this generator (`forkPositional()`).
    fn fork_positional(&mut self) -> Self::Positional;

    /// `nextInt()`.
    fn next_int(&mut self) -> i32;
    /// `nextInt(bound)`. Java throws for `bound <= 0`; this panics, callers must check.
    fn next_int_bounded(&mut self, bound: i32) -> i32;
    /// `nextLong()`.
    fn next_long(&mut self) -> i64;
    /// `nextBoolean()`.
    fn next_boolean(&mut self) -> bool;
    /// `nextFloat()`: uniform in `[0, 1)`.
    fn next_float(&mut self) -> f32;
    /// `nextDouble()`: uniform in `[0, 1)`.
    fn next_double(&mut self) -> f64;
    /// `nextGaussian()`: Marsaglia polar method.
    fn next_gaussian(&mut self) -> f64;

    /// `nextInt(origin, bound)`: `origin + nextInt(bound - origin)`.
    fn next_int_between(&mut self, origin: i32, bound: i32) -> i32 {
        origin.wrapping_add(self.next_int_bounded(bound.wrapping_sub(origin)))
    }

    /// `nextIntBetweenInclusive(min, max)`: `nextInt(max - min + 1) + min`.
    fn next_int_between_inclusive(&mut self, min: i32, max: i32) -> i32 {
        self.next_int_bounded(max.wrapping_sub(min).wrapping_add(1))
            .wrapping_add(min)
    }

    /// `triangle(mode, deviation)`: `mode + deviation * (nextDouble() - nextDouble())`.
    fn triangle(&mut self, mode: f64, deviation: f64) -> f64 {
        mode + deviation * (self.next_double() - self.next_double())
    }

    /// `consumeCount(count)`: advances the generator as Vanilla does.
    fn consume_count(&mut self, count: i32);
}

/// Vanilla's `PositionalRandomFactory`.
pub trait PositionalRandomFactory {
    /// The generator type this factory creates.
    type Random: RandomSource;

    /// The generator for a block position (`at(x, y, z)`).
    fn at(&self, x: i32, y: i32, z: i32) -> Self::Random;
    /// The generator for a string, usually a resource location (`fromHashOf`).
    fn with_hash_of(&self, name: &str) -> Self::Random;
    /// The generator for a seed (`fromSeed`).
    fn with_seed(&self, seed: i64) -> Self::Random;
}

/// Shared state of Vanilla's `MarsagliaPolarGaussian`.
#[derive(Clone, Debug, Default)]
pub(crate) struct Gaussian {
    next: f64,
    has_next: bool,
}

impl Gaussian {
    pub(crate) fn reset(&mut self) {
        self.has_next = false;
    }

    pub(crate) fn next(&mut self, mut next_double: impl FnMut() -> f64) -> f64 {
        if self.has_next {
            self.has_next = false;
            return self.next;
        }
        loop {
            let d = 2.0 * next_double() - 1.0;
            let e = 2.0 * next_double() - 1.0;
            let f = crate::math::square(d) + crate::math::square(e);
            if f < 1.0 && f != 0.0 {
                let g = (-2.0 * f.ln() / f).sqrt();
                self.next = e * g;
                self.has_next = true;
                return d * g;
            }
        }
    }
}
