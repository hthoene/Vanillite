//! Java-exact maths: equivalents of Vanilla's `Mth` and of Java's numeric conversions.
//!
//! Rules for code that must match Vanilla bit for bit:
//! - Java `int`/`long` arithmetic wraps: use `wrapping_*` operations. Java `/` and `%` on integers
//!   truncate towards zero like Rust's `wrapping_div`/`wrapping_rem`.
//! - Java `(int) double` and `(long) double` saturate and map NaN to 0, which is exactly Rust's
//!   `as` cast. The helpers below name this intent.
//! - Keep Vanilla's operation order and never use `mul_add`: Java does not fuse.

use std::sync::OnceLock;

/// Java `(int) d`: truncates towards zero, saturates, NaN becomes 0.
#[inline]
pub const fn d2i(d: f64) -> i32 {
    d as i32
}

/// Java `(long) d`: truncates towards zero, saturates, NaN becomes 0.
#[inline]
pub const fn d2l(d: f64) -> i64 {
    d as i64
}

/// Java `(int) f`: truncates towards zero, saturates, NaN becomes 0.
#[inline]
pub const fn f2i(f: f32) -> i32 {
    f as i32
}

/// `Mth.floor(double)`.
#[inline]
pub const fn floor(d: f64) -> i32 {
    let i = d as i32;
    if d < i as f64 { i.wrapping_sub(1) } else { i }
}

/// `Mth.floor(float)`.
#[inline]
pub const fn floor_f(f: f32) -> i32 {
    let i = f as i32;
    if f < i as f32 { i.wrapping_sub(1) } else { i }
}

/// `Mth.lfloor(double)`.
#[inline]
pub const fn lfloor(d: f64) -> i64 {
    let l = d as i64;
    if d < l as f64 { l.wrapping_sub(1) } else { l }
}

/// `Mth.ceil(double)`.
#[inline]
pub const fn ceil(d: f64) -> i32 {
    let i = d as i32;
    if d > i as f64 { i.wrapping_add(1) } else { i }
}

/// `Mth.ceil(float)`.
#[inline]
pub const fn ceil_f(f: f32) -> i32 {
    let i = f as i32;
    if f > i as f32 { i.wrapping_add(1) } else { i }
}

/// `Mth.lceil(double)`.
#[inline]
pub const fn lceil(d: f64) -> i64 {
    let l = d as i64;
    if d > l as f64 { l.wrapping_add(1) } else { l }
}

/// Java `Math.round(double)`: rounds half up (towards positive infinity), saturates, NaN
/// becomes 0. Differs from [`f64::round`] for negative halves (`-2.5` → `-2`).
#[inline]
pub fn java_round(d: f64) -> i64 {
    let floor = d.floor();
    // `d - floor` is exact for every finite double, so 0.49999999999999994 stays below 0.5.
    let rounded = if d - floor >= 0.5 { floor + 1.0 } else { floor };
    rounded as i64
}

/// `Mth.packDegrees(float)`: an angle as a protocol byte (256 steps per turn).
#[inline]
pub const fn pack_degrees(degrees: f32) -> i8 {
    floor_f(degrees * 256.0 / 360.0) as i8
}

/// `Mth.clamp(double, double, double)`: `value < min ? min : Math.min(value, max)`.
///
/// Unlike [`f64::clamp`] it never panics and keeps Java's NaN behaviour.
#[inline]
pub fn clamp(value: f64, min: f64, max: f64) -> f64 {
    if value < min {
        min
    } else {
        java_min(value, max)
    }
}

/// `Mth.clamp(float, float, float)`.
#[inline]
pub fn clamp_f(value: f32, min: f32, max: f32) -> f32 {
    if value < min {
        min
    } else {
        f64_to_f32(java_min(f64::from(value), f64::from(max)))
    }
}

/// `Mth.clamp(int, int, int)`: `Math.min(Math.max(value, min), max)`.
#[inline]
pub const fn clamp_i(value: i32, min: i32, max: i32) -> i32 {
    let v = if value > min { value } else { min };
    if v < max { v } else { max }
}

/// `Mth.clamp(long, long, long)`.
#[inline]
pub const fn clamp_l(value: i64, min: i64, max: i64) -> i64 {
    let v = if value > min { value } else { min };
    if v < max { v } else { max }
}

/// Java `Math.min(double, double)`: NaN if either is NaN, and `-0.0 < 0.0`.
#[inline]
pub fn java_min(a: f64, b: f64) -> f64 {
    if a.is_nan() {
        a
    } else if b.is_nan() {
        b
    } else if a == 0.0 && b == 0.0 {
        if a.is_sign_negative() { a } else { b }
    } else if a <= b {
        a
    } else {
        b
    }
}

/// Java `Math.max(double, double)`: NaN if either is NaN, and `0.0 > -0.0`.
#[inline]
pub fn java_max(a: f64, b: f64) -> f64 {
    if a.is_nan() {
        a
    } else if b.is_nan() {
        b
    } else if a == 0.0 && b == 0.0 {
        if a.is_sign_negative() { b } else { a }
    } else if a >= b {
        a
    } else {
        b
    }
}

/// Java `(float) d` (round to nearest even), named for intent.
#[inline]
pub const fn f64_to_f32(d: f64) -> f32 {
    d as f32
}

/// `Mth.clampedLerp(start, end, delta)`.
#[inline]
pub fn clamped_lerp(start: f64, end: f64, delta: f64) -> f64 {
    if delta < 0.0 {
        start
    } else if delta > 1.0 {
        end
    } else {
        lerp(delta, start, end)
    }
}

/// `Mth.lerp(delta, start, end)`: `start + delta * (end - start)`.
#[inline]
pub fn lerp(delta: f64, start: f64, end: f64) -> f64 {
    start + delta * (end - start)
}

/// `Mth.lerp(float, float, float)`.
#[inline]
pub fn lerp_f(delta: f32, start: f32, end: f32) -> f32 {
    start + delta * (end - start)
}

/// `Mth.lerp2`.
#[inline]
pub fn lerp2(dx: f64, dy: f64, x0y0: f64, x1y0: f64, x0y1: f64, x1y1: f64) -> f64 {
    lerp(dy, lerp(dx, x0y0, x1y0), lerp(dx, x0y1, x1y1))
}

/// `Mth.lerp3`.
#[allow(clippy::too_many_arguments)]
#[inline]
pub fn lerp3(
    dx: f64,
    dy: f64,
    dz: f64,
    x0y0z0: f64,
    x1y0z0: f64,
    x0y1z0: f64,
    x1y1z0: f64,
    x0y0z1: f64,
    x1y0z1: f64,
    x0y1z1: f64,
    x1y1z1: f64,
) -> f64 {
    lerp(
        dz,
        lerp2(dx, dy, x0y0z0, x1y0z0, x0y1z0, x1y1z0),
        lerp2(dx, dy, x0y0z1, x1y0z1, x0y1z1, x1y1z1),
    )
}

/// `Mth.inverseLerp`: `(value - start) / (end - start)`.
#[inline]
pub fn inverse_lerp(value: f64, start: f64, end: f64) -> f64 {
    (value - start) / (end - start)
}

/// `Mth.map`.
#[inline]
pub fn map(value: f64, from_start: f64, from_end: f64, to_start: f64, to_end: f64) -> f64 {
    lerp(inverse_lerp(value, from_start, from_end), to_start, to_end)
}

/// `Mth.clampedMap`.
#[inline]
pub fn clamped_map(value: f64, from_start: f64, from_end: f64, to_start: f64, to_end: f64) -> f64 {
    clamped_lerp(to_start, to_end, inverse_lerp(value, from_start, from_end))
}

/// `Mth.smoothstep`: `d³(d(6d − 15) + 10)`, used by Perlin noise.
#[inline]
pub fn smoothstep(d: f64) -> f64 {
    d * d * d * (d * (d * 6.0 - 15.0) + 10.0)
}

/// `Mth.smoothstepDerivative`.
#[inline]
pub fn smoothstep_derivative(d: f64) -> f64 {
    30.0 * d * d * (d - 1.0) * (d - 1.0)
}

/// `Mth.square(double)`.
#[inline]
pub fn square(d: f64) -> f64 {
    d * d
}

/// `Mth.frac(double)`: `d - lfloor(d)`.
#[inline]
pub fn frac(d: f64) -> f64 {
    d - lfloor(d) as f64
}

/// `Mth.positiveModulo(int, int)` (`Math.floorMod`). Panics if `m` is 0, like Java throws.
#[inline]
pub const fn floor_mod(x: i32, m: i32) -> i32 {
    x.wrapping_rem(m).wrapping_add(m).wrapping_rem(m)
}

/// `Math.floorDiv(int, int)`. Panics if `y` is 0, like Java throws.
#[inline]
pub const fn floor_div(x: i32, y: i32) -> i32 {
    let q = x.wrapping_div(y);
    if (x ^ y) < 0 && q.wrapping_mul(y) != x {
        q.wrapping_sub(1)
    } else {
        q
    }
}

/// `Mth.positiveModulo(double, double)`: `(x % m + m) % m`.
#[inline]
pub fn floor_mod_d(x: f64, m: f64) -> f64 {
    (x % m + m) % m
}

/// `Mth.wrapDegrees(float)`: wraps into `[-180, 180)`.
#[inline]
pub fn wrap_degrees_f(degrees: f32) -> f32 {
    let mut f = degrees % 360.0;
    if f >= 180.0 {
        f -= 360.0;
    }
    if f < -180.0 {
        f += 360.0;
    }
    f
}

/// `Mth.wrapDegrees(double)`: wraps into `[-180, 180)`.
#[inline]
pub fn wrap_degrees(degrees: f64) -> f64 {
    let mut d = degrees % 360.0;
    if d >= 180.0 {
        d -= 360.0;
    }
    if d < -180.0 {
        d += 360.0;
    }
    d
}

/// `Mth.wrapDegrees(int)`.
#[inline]
pub const fn wrap_degrees_i(degrees: i32) -> i32 {
    let mut i = degrees % 360;
    if i >= 180 {
        i -= 360;
    }
    if i < -180 {
        i += 360;
    }
    i
}

/// `Mth.sqrt(float)`: `(float) Math.sqrt(f)`.
#[inline]
pub fn sqrt_f(f: f32) -> f32 {
    f64_to_f32(f64::from(f).sqrt())
}

/// `Mth.length(x, y, z)`.
#[inline]
pub fn length(x: f64, y: f64, z: f64) -> f64 {
    (x * x + y * y + z * z).sqrt()
}

/// `Mth.lengthSquared(x, y, z)`.
#[inline]
pub fn length_squared(x: f64, y: f64, z: f64) -> f64 {
    x * x + y * y + z * z
}

/// Scale of `Mth.sin`/`Mth.cos`: table entries per radian (65536 / 2π).
const SIN_SCALE: f64 = 10430.378350470453;

fn sin_table() -> &'static [f32; 65536] {
    static TABLE: OnceLock<Box<[f32; 65536]>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = Box::new([0.0_f32; 65536]);
        for (i, v) in table.iter_mut().enumerate() {
            *v = f64_to_f32((i as f64 * std::f64::consts::PI * 2.0 / 65536.0).sin());
        }
        table
    })
}

#[inline]
fn sin_lookup(index: f64) -> f32 {
    let table = sin_table();
    table[(d2l(index) & 0xFFFF) as usize]
}

/// `Mth.sin(double)`: table-based, 65536 steps per turn.
#[inline]
pub fn sin(radians: f64) -> f32 {
    sin_lookup(radians * SIN_SCALE)
}

/// `Mth.cos(double)`: table-based, 65536 steps per turn.
#[inline]
pub fn cos(radians: f64) -> f32 {
    sin_lookup(radians * SIN_SCALE + 16384.0)
}

/// `Mth.getSeed(x, y, z)`: the positional seed used by random offsets and positional RNGs.
#[inline]
pub const fn get_seed(x: i32, y: i32, z: i32) -> i64 {
    let mut l = (x.wrapping_mul(3129871) as i64) ^ (z as i64).wrapping_mul(116129781) ^ (y as i64);
    l = l
        .wrapping_mul(l)
        .wrapping_mul(42317861)
        .wrapping_add(l.wrapping_mul(11));
    l >> 16
}

/// `Mth.murmurHash3Mixer(long)`.
#[inline]
pub const fn murmur_hash3_mixer(mut l: i64) -> i64 {
    l ^= ((l as u64) >> 33) as i64;
    l = l.wrapping_mul(-49064778989728563);
    l ^= ((l as u64) >> 33) as i64;
    l = l.wrapping_mul(-4265267296055464877);
    l ^= ((l as u64) >> 33) as i64;
    l
}

/// `Mth.smallestEncompassingPowerOfTwo(int)`.
#[inline]
pub const fn smallest_encompassing_power_of_two(value: i32) -> i32 {
    let mut i = value.wrapping_sub(1);
    i |= i >> 1;
    i |= i >> 2;
    i |= i >> 4;
    i |= i >> 8;
    i |= i >> 16;
    i.wrapping_add(1)
}

/// `Mth.isPowerOfTwo(int)`.
#[inline]
pub const fn is_power_of_two(value: i32) -> bool {
    value != 0 && (value & value.wrapping_sub(1)) == 0
}

/// `Mth.ceillog2(int)`.
#[inline]
pub const fn ceillog2(value: i32) -> i32 {
    let v = if is_power_of_two(value) {
        value
    } else {
        smallest_encompassing_power_of_two(value)
    };
    // Java: MULTIPLY_DE_BRUIJN_BIT_POSITION[(int)((long)v * 125613361L >> 27) & 31]
    (v as u32).trailing_zeros() as i32 & 31
}

/// `Mth.log2(int)`.
#[inline]
pub const fn log2(value: i32) -> i32 {
    ceillog2(value) - if is_power_of_two(value) { 0 } else { 1 }
}

/// Java `String.hashCode()`: polynomial hash over UTF-16 code units.
#[inline]
pub fn java_string_hash(s: &str) -> i32 {
    s.encode_utf16()
        .fold(0_i32, |h, c| h.wrapping_mul(31).wrapping_add(i32::from(c)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floor_and_casts_match_java() {
        assert_eq!(floor(-0.5), -1);
        assert_eq!(floor(-1.0), -1);
        assert_eq!(floor(2.9), 2);
        assert_eq!(floor(f64::NAN), 0);
        assert_eq!(floor(1e300), i32::MAX);
        // Java: (int) -1e300 is MIN_VALUE, and MIN_VALUE - 1 wraps.
        assert_eq!(floor(-1e300), i32::MAX);
        assert_eq!(lfloor(-1e-9), -1);
        assert_eq!(ceil(-0.5), 0);
        assert_eq!(ceil(0.1), 1);
        assert_eq!(d2i(-2.7), -2);
    }

    #[test]
    fn modulo_and_division_match_java() {
        assert_eq!(floor_mod(-1, 16), 15);
        assert_eq!(floor_mod(17, 16), 1);
        assert_eq!(floor_div(-1, 16), -1);
        assert_eq!(floor_div(-16, 16), -1);
        assert_eq!(floor_div(-17, 16), -2);
        assert_eq!(floor_div(15, 16), 0);
    }

    #[test]
    fn wrap_degrees_range() {
        assert_eq!(wrap_degrees(180.0), -180.0);
        assert_eq!(wrap_degrees(-190.0), 170.0);
        assert_eq!(wrap_degrees_f(540.0), -180.0);
        assert_eq!(wrap_degrees_i(-181), 179);
    }

    #[test]
    fn powers_of_two() {
        assert_eq!(smallest_encompassing_power_of_two(30_000_000), 33_554_432);
        assert_eq!(ceillog2(33_554_432), 25);
        assert_eq!(ceillog2(30_000_000), 25);
        assert_eq!(log2(30_000_000), 24);
        assert_eq!(ceillog2(1), 0);
        assert_eq!(ceillog2(5), 3);
    }

    #[test]
    fn java_string_hash_codes() {
        assert_eq!(java_string_hash(""), 0);
        assert_eq!(java_string_hash("hello"), 99_162_322);
        assert_eq!(
            java_string_hash("minecraft:overworld"),
            "minecraft:overworld"
                .encode_utf16()
                .fold(0_i32, |h, c| h.wrapping_mul(31).wrapping_add(i32::from(c)))
        );
    }

    #[test]
    fn sin_cos_table() {
        assert_eq!(sin(0.0), 0.0);
        assert_eq!(cos(0.0), 1.0);
        assert_eq!(sin(std::f64::consts::FRAC_PI_2), 1.0);
        assert!((sin(1.0) - 1.0_f64.sin() as f32).abs() < 1e-4);
    }

    #[test]
    fn clamp_java_semantics() {
        assert!(clamp(f64::NAN, 0.0, 1.0).is_nan());
        assert_eq!(clamp(-1.0, 0.0, 1.0), 0.0);
        assert_eq!(clamp(2.0, 0.0, 1.0), 1.0);
        assert_eq!(clamp_i(5, 0, 3), 3);
    }
}
