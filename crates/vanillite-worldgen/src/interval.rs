//! `net.minecraft.util.Interval`: value ranges of density functions and noises.
//!
//! Ranges decide compile-time simplifications (for example `min`/`max` of non-overlapping inputs),
//! so they follow Vanilla's `float` arithmetic exactly. Vanilla throws on invalid bounds; here such
//! bounds become [`Interval::NAI`] instead, which only disables simplifications.

use crate::mth::{max_f, min_f, signum_f, square_f};

/// A closed `float` interval, or "not an interval" ([`Interval::NAI`]).
#[derive(Clone, Copy, Debug)]
pub struct Interval {
    min: f32,
    max: f32,
}

impl PartialEq for Interval {
    fn eq(&self, other: &Self) -> bool {
        (self.is_nai() && other.is_nai()) || (self.min == other.min && self.max == other.max)
    }
}

impl Interval {
    /// `Interval.NaI`.
    pub const NAI: Self = Self {
        min: f32::NAN,
        max: f32::NAN,
    };
    /// `Interval.INFINITE`.
    pub const INFINITE: Self = Self {
        min: f32::NEG_INFINITY,
        max: f32::INFINITY,
    };

    /// `Interval.of(min, max)`.
    pub fn of(min: f32, max: f32) -> Self {
        if max < min || min.is_nan() || max.is_nan() {
            return Self::NAI;
        }
        Self { min, max }
    }

    /// `Interval.ofSymmetric(range)`.
    pub fn of_symmetric(range: f32) -> Self {
        Self::of(-range, range)
    }

    /// `Interval.ofExact(value)`.
    pub fn of_exact(value: f32) -> Self {
        Self::of(value, value)
    }

    /// Lower bound.
    pub fn min(self) -> f32 {
        self.min
    }

    /// Upper bound.
    pub fn max(self) -> f32 {
        self.max
    }

    /// `isNaI()`.
    pub fn is_nai(self) -> bool {
        self.min.is_nan()
    }

    /// `contains(value)`.
    pub fn contains(self, value: f32) -> bool {
        value >= self.min && value <= self.max
    }

    /// `intersects(other)`.
    pub fn intersects(self, other: Self) -> bool {
        self.min <= other.max && self.max >= other.min
    }

    /// `encapsulating(List<Interval>)`.
    pub fn encapsulating(intervals: &[Self]) -> Self {
        let mut min = f32::INFINITY;
        let mut max = f32::NEG_INFINITY;
        for interval in intervals {
            if !interval.is_nai() {
                min = min_f(interval.min, min);
                max = max_f(interval.max, max);
            }
        }
        if max < min {
            Self::NAI
        } else {
            Self::of(min, max)
        }
    }

    /// `encapsulating(float, float)`.
    pub fn encapsulating_values(first: f32, second: f32) -> Self {
        if first.is_nan() && second.is_nan() {
            Self::NAI
        } else if first.is_nan() {
            Self::of_exact(second)
        } else if second.is_nan() {
            Self::of_exact(first)
        } else {
            Self::of(min_f(first, second), max_f(first, second))
        }
    }

    fn encapsulating_with(first: Self, second: f32) -> Self {
        if second.is_nan() {
            first
        } else if first.is_nai() {
            Self::of_exact(second)
        } else {
            Self::of(min_f(first.min, second), max_f(first.max, second))
        }
    }

    fn mul_bound(left: f32, right: f32) -> f32 {
        if left != 0.0 && right != 0.0 {
            left * right
        } else {
            0.0
        }
    }

    /// `reciprocal(input)`.
    pub fn reciprocal(input: Self) -> Self {
        if input.is_nai() || (input.min == 0.0 && input.max == 0.0) {
            Self::NAI
        } else if !input.contains(0.0) {
            Self::of(1.0 / input.max, 1.0 / input.min)
        } else if input.max == 0.0 {
            Self::of(f32::NEG_INFINITY, 1.0 / input.min)
        } else if input.min == 0.0 {
            Self::of(1.0 / input.max, f32::INFINITY)
        } else {
            Self::INFINITE
        }
    }

    /// `min(left, right)`.
    pub fn min_of(left: Self, right: Self) -> Self {
        if left.is_nai() || right.is_nai() {
            Self::NAI
        } else {
            Self::of(min_f(left.min, right.min), min_f(left.max, right.max))
        }
    }

    /// `max(left, right)`.
    pub fn max_of(left: Self, right: Self) -> Self {
        if left.is_nai() || right.is_nai() {
            Self::NAI
        } else {
            Self::of(max_f(left.min, right.min), max_f(left.max, right.max))
        }
    }

    /// `clamp(input, min, max)`.
    pub fn clamp(input: Self, min: f32, max: f32) -> Self {
        if min > max || input.is_nai() {
            Self::NAI
        } else if input.min >= max {
            Self::of(max, max)
        } else if input.max <= min {
            Self::of(min, min)
        } else {
            Self::of(max_f(input.min, min), min_f(input.max, max))
        }
    }

    /// `abs(input)`.
    pub fn abs(input: Self) -> Self {
        if input.is_nai() {
            return Self::NAI;
        }
        let max = max_f(input.min.abs(), input.max.abs());
        if input.contains(0.0) {
            Self::of(0.0, max)
        } else {
            Self::of(min_f(input.min.abs(), input.max.abs()), max)
        }
    }

    /// `square(input)`.
    pub fn square(input: Self) -> Self {
        if input.is_nai() {
            return Self::NAI;
        }
        let max = max_f(square_f(input.min), square_f(input.max));
        if input.contains(0.0) {
            Self::of(0.0, max)
        } else {
            Self::of(min_f(square_f(input.min), square_f(input.max)), max)
        }
    }

    /// `pow(base, exponent)`.
    pub fn pow(base: Self, exponent: Self) -> Self {
        if base.is_nai() || exponent.is_nai() {
            return Self::NAI;
        }
        if base.min == base.max {
            return Self::pow_value(base.min, exponent);
        }
        let mut result = Self::encapsulating(&[
            Self::pow_value(base.min, exponent),
            Self::pow_value(base.max, exponent),
        ]);
        if base.contains(0.0) {
            if base.max > 0.0 {
                result = Self::encapsulating(&[result, Self::pow_value(0.0, exponent)]);
            }
            if base.min < 0.0 {
                result = Self::encapsulating(&[result, Self::pow_value(-0.0, exponent)]);
            }
        }
        result
    }

    fn jpow(base: f32, exponent: f32) -> f32 {
        f64::from(base).powf(f64::from(exponent)) as f32
    }

    fn pow_value(base: f32, exponent: Self) -> Self {
        if base.is_nan() || exponent.is_nai() {
            Self::NAI
        } else if exponent.min == exponent.max {
            let value = Self::jpow(base, exponent.min);
            if value.is_nan() {
                Self::NAI
            } else {
                Self::of_exact(value)
            }
        } else if base == 0.0 {
            Self::pow_zero_base(exponent) * Self::of_exact(1.0_f32.copysign(base))
        } else if base == 1.0 {
            Self::of_exact(1.0)
        } else if base > 0.0 {
            Self::pow_positive_base(base, exponent)
        } else {
            Self::pow_negative_base(base, exponent)
        }
    }

    fn pow_positive_base(base: f32, exponent: Self) -> Self {
        if exponent.min.is_finite() && exponent.max.is_finite() {
            Self::encapsulating_values(
                Self::jpow(base, exponent.min),
                Self::jpow(base, exponent.max),
            )
        } else if exponent.min.is_infinite() && exponent.max.is_infinite() {
            Self::of(0.0, f32::INFINITY)
        } else if exponent.min.is_infinite() {
            if base < 1.0 {
                Self::of(Self::jpow(base, exponent.max), f32::INFINITY)
            } else {
                Self::of(0.0, Self::jpow(base, exponent.max))
            }
        } else if base < 1.0 {
            Self::of(0.0, Self::jpow(base, exponent.min))
        } else {
            Self::of(Self::jpow(base, exponent.min), f32::INFINITY)
        }
    }

    fn pow_zero_base(exponent: Self) -> Self {
        if exponent.contains(0.0) {
            if exponent.max == 0.0 {
                Self::of(1.0, f32::INFINITY)
            } else if exponent.min == 0.0 {
                Self::of(0.0, 1.0)
            } else {
                Self::of(0.0, f32::INFINITY)
            }
        } else if exponent.max < 0.0 {
            Self::of_exact(f32::INFINITY)
        } else {
            Self::of_exact(0.0)
        }
    }

    fn pow_negative_base(base: f32, exponent: Self) -> Self {
        let min_int = f64::from(exponent.min).ceil() as f32;
        let max_int = f64::from(exponent.max).floor() as f32;
        if max_int < min_int {
            return Self::NAI;
        }
        let to_min = Self::jpow(base, min_int);
        let to_max = Self::jpow(base, max_int);
        let mut result = Self::encapsulating_values(to_min, to_max);
        if min_int.is_infinite() {
            result = Self::encapsulating_with(result, -to_min);
        } else if min_int + 1.0 < max_int {
            result = Self::encapsulating_with(result, Self::jpow(base, min_int + 1.0));
        }
        if max_int.is_infinite() {
            result = Self::encapsulating_with(result, -to_max);
        } else if max_int - 1.0 > min_int {
            result = Self::encapsulating_with(result, Self::jpow(base, max_int - 1.0));
        }
        result
    }

    /// `log(input)`.
    pub fn log(input: Self) -> Self {
        if input.max < 0.0 {
            return Self::NAI;
        }
        let clipped = Self::max_of(input, Self::of_exact(0.0));
        Self::map_monotonic(clipped, |x| f64::from(x).ln() as f32)
    }

    /// `mapMonotonic(input, op)`; NaN results give [`Interval::NAI`] (Vanilla throws).
    pub fn map_monotonic(input: Self, op: impl Fn(f32) -> f32) -> Self {
        if input.is_nai() {
            return Self::NAI;
        }
        let a = op(input.min);
        let b = op(input.max);
        if a.is_nan() || b.is_nan() {
            Self::NAI
        } else {
            Self::of(min_f(a, b), max_f(a, b))
        }
    }

    /// `lerp(alpha, first, second)`.
    pub fn lerp(alpha: Self, first: Self, second: Self) -> Self {
        if alpha.is_nai() || first.is_nai() || second.is_nai() {
            return Self::NAI;
        }
        Self::encapsulating(&[
            Self::lerp_values(alpha, first.min, second.min),
            Self::lerp_values(alpha, first.max, second.min),
            Self::lerp_values(alpha, first.min, second.max),
            Self::lerp_values(alpha, first.max, second.max),
        ])
    }

    /// `lerp(alpha, float first, float second)`.
    pub fn lerp_values(alpha: Self, first: f32, second: f32) -> Self {
        if alpha.is_nai() || first.is_nan() || second.is_nan() {
            Self::NAI
        } else if first.is_finite() && second.is_finite() {
            Self::encapsulating_values(
                first + Self::mul_bound(alpha.min, second - first),
                first + Self::mul_bound(alpha.max, second - first),
            )
        } else if first == second {
            Self::of_exact(first)
        } else {
            let a = Self::lerp_infinite_bound(alpha.min, first, second);
            let b = Self::lerp_infinite_bound(alpha.max, first, second);
            if a.is_nan() || b.is_nan() {
                Self::NAI
            } else {
                Self::encapsulating_values(a, b)
            }
        }
    }

    fn lerp_infinite_bound(alpha: f32, first: f32, second: f32) -> f32 {
        let first_part = Self::mul_bound(1.0 - alpha, first);
        let second_part = Self::mul_bound(alpha, second);
        if !first_part.is_infinite() || !second_part.is_infinite() {
            first_part + second_part
        } else if alpha <= 0.0 {
            if second > first {
                f32::NEG_INFINITY
            } else {
                f32::INFINITY
            }
        } else if alpha >= 1.0 {
            if second > first {
                f32::INFINITY
            } else {
                f32::NEG_INFINITY
            }
        } else {
            f32::NAN
        }
    }

    /// `sign(input)`.
    pub fn sign(input: Self) -> Self {
        if input.is_nai() {
            Self::NAI
        } else if input.min == input.max {
            Self::of_exact(signum_f(input.min))
        } else if input.contains(0.0) {
            if input.min == 0.0 {
                Self::of(0.0, 1.0)
            } else if input.max == 0.0 {
                Self::of(-1.0, 0.0)
            } else {
                Self::of(-1.0, 1.0)
            }
        } else {
            Self::of_exact(if input.min > 0.0 { 1.0 } else { -1.0 })
        }
    }
}

/// `Interval.add(left, right)`.
impl std::ops::Add for Interval {
    type Output = Self;

    fn add(self, right: Self) -> Self {
        let left = self;
        let min = left.min + right.min;
        let max = left.max + right.max;
        if min.is_nan() || max.is_nan() {
            Self::NAI
        } else {
            Self::of(min, max)
        }
    }
}

/// `Interval.sub(left, right)`.
impl std::ops::Sub for Interval {
    type Output = Self;

    fn sub(self, right: Self) -> Self {
        let left = self;
        let min = left.min - right.max;
        let max = left.max - right.min;
        if min.is_nan() || max.is_nan() {
            Self::NAI
        } else {
            Self::of(min, max)
        }
    }
}

/// `Interval.mul(left, right)`.
impl std::ops::Mul for Interval {
    type Output = Self;

    fn mul(self, right: Self) -> Self {
        let left = self;
        if left.is_nai() || right.is_nai() {
            return Self::NAI;
        }
        let a = Self::mul_bound(left.min, right.min);
        let b = Self::mul_bound(left.min, right.max);
        let c = Self::mul_bound(left.max, right.min);
        let d = Self::mul_bound(left.max, right.max);
        Self::of(
            min_f(min_f(a, b), min_f(c, d)),
            max_f(max_f(a, b), max_f(c, d)),
        )
    }
}

/// `Interval.div(left, right)`.
impl std::ops::Div for Interval {
    type Output = Self;

    fn div(self, right: Self) -> Self {
        let left = self;
        // Vanilla: `mul(left, reciprocal(right))`.
        #[allow(clippy::suspicious_arithmetic_impl)]
        let result = left * Self::reciprocal(right);
        result
    }
}
