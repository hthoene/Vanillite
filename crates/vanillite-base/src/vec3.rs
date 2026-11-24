//! A double-precision vector (`Vec3`).

use std::ops::{Add, Mul, Neg, Sub};

/// A point or direction in world space (`Vec3`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec3 {
    /// X component.
    pub x: f64,
    /// Y component.
    pub y: f64,
    /// Z component.
    pub z: f64,
}

impl Vec3 {
    /// The zero vector.
    pub const ZERO: Self = Self::new(0.0, 0.0, 0.0);

    /// A vector from components.
    #[inline]
    pub const fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }

    /// `dot`.
    #[inline]
    pub fn dot(self, other: Self) -> f64 {
        self.x * other.x + self.y * other.y + self.z * other.z
    }

    /// `lengthSqr`.
    #[inline]
    pub fn length_sqr(self) -> f64 {
        self.x * self.x + self.y * self.y + self.z * self.z
    }

    /// `length`.
    #[inline]
    pub fn length(self) -> f64 {
        self.length_sqr().sqrt()
    }

    /// `distanceToSqr`.
    #[inline]
    pub fn distance_to_sqr(self, other: Self) -> f64 {
        let dx = other.x - self.x;
        let dy = other.y - self.y;
        let dz = other.z - self.z;
        dx * dx + dy * dy + dz * dz
    }

    /// `normalize`: the zero vector if the length is below `1.0E-5F` (a float literal in Vanilla).
    #[inline]
    pub fn normalize(self) -> Self {
        let d = self.length();
        if d < f64::from(1.0E-5_f32) {
            Self::ZERO
        } else {
            Self::new(self.x / d, self.y / d, self.z / d)
        }
    }

    /// `multiply(x, y, z)`: component-wise product.
    #[inline]
    pub fn multiply(self, x: f64, y: f64, z: f64) -> Self {
        Self::new(self.x * x, self.y * y, self.z * z)
    }
}

impl Add for Vec3 {
    type Output = Self;

    #[inline]
    fn add(self, rhs: Self) -> Self {
        Self::new(self.x + rhs.x, self.y + rhs.y, self.z + rhs.z)
    }
}

impl Sub for Vec3 {
    type Output = Self;

    #[inline]
    fn sub(self, rhs: Self) -> Self {
        Self::new(self.x - rhs.x, self.y - rhs.y, self.z - rhs.z)
    }
}

impl Mul<f64> for Vec3 {
    type Output = Self;

    #[inline]
    fn mul(self, rhs: f64) -> Self {
        Self::new(self.x * rhs, self.y * rhs, self.z * rhs)
    }
}

impl Neg for Vec3 {
    type Output = Self;

    #[inline]
    fn neg(self) -> Self {
        // Vanilla's `reverse()` is `scale(-1.0)`.
        self * -1.0
    }
}
