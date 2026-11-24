//! Float conversions with Rust's saturating `as` semantics, in one place (SEC-07 forbids `as`
//! elsewhere in this crate).

/// `f64` -> `f32` (rounds; out-of-range values become infinite).
#[allow(clippy::as_conversions, clippy::cast_possible_truncation)]
#[inline]
pub fn f32_of(v: f64) -> f32 {
    v as f32
}

/// `f32` -> `u8`, saturating (NaN becomes 0).
#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
#[inline]
pub fn f32_to_u8(v: f32) -> u8 {
    v as u8
}

/// `f64` -> `i32`, saturating (NaN becomes 0), rounding towards negative infinity.
#[allow(clippy::as_conversions, clippy::cast_possible_truncation)]
#[inline]
pub fn floor_i32(v: f64) -> i32 {
    v.floor() as i32
}

/// A Java protocol angle byte in degrees.
#[inline]
pub fn angle_deg(b: i8) -> f32 {
    f32::from(b) * (360.0 / 256.0)
}

/// `f32` -> `i32`, saturating (NaN becomes 0), rounding towards zero.
#[allow(clippy::as_conversions, clippy::cast_possible_truncation)]
#[inline]
pub fn f32_to_i32(v: f32) -> i32 {
    v as i32
}
