//! Axis-aligned bounding boxes (`AABB`).

use crate::math::{java_max, java_min};
use crate::{BlockPos, Vec3};

/// An axis-aligned box in world space (`AABB`). Construction orders the corners like Vanilla.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Aabb {
    /// Minimum X.
    pub min_x: f64,
    /// Minimum Y.
    pub min_y: f64,
    /// Minimum Z.
    pub min_z: f64,
    /// Maximum X.
    pub max_x: f64,
    /// Maximum Y.
    pub max_y: f64,
    /// Maximum Z.
    pub max_z: f64,
}

impl Aabb {
    /// A box spanning two corners in any order.
    #[inline]
    pub fn new(x1: f64, y1: f64, z1: f64, x2: f64, y2: f64, z2: f64) -> Self {
        Self {
            min_x: java_min(x1, x2),
            min_y: java_min(y1, y2),
            min_z: java_min(z1, z2),
            max_x: java_max(x1, x2),
            max_y: java_max(y1, y2),
            max_z: java_max(z1, z2),
        }
    }

    /// The unit box of a block.
    #[inline]
    pub fn of_block(pos: BlockPos) -> Self {
        let (x, y, z) = (f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
        Self::new(x, y, z, x + 1.0, y + 1.0, z + 1.0)
    }

    /// `move(dx, dy, dz)`.
    #[inline]
    pub fn offset(self, dx: f64, dy: f64, dz: f64) -> Self {
        Self::new(
            self.min_x + dx,
            self.min_y + dy,
            self.min_z + dz,
            self.max_x + dx,
            self.max_y + dy,
            self.max_z + dz,
        )
    }

    /// `inflate(x, y, z)`: grows every side by the given amounts.
    #[inline]
    pub fn inflate(self, x: f64, y: f64, z: f64) -> Self {
        Self::new(
            self.min_x - x,
            self.min_y - y,
            self.min_z - z,
            self.max_x + x,
            self.max_y + y,
            self.max_z + z,
        )
    }

    /// `expandTowards(x, y, z)`: stretches the box in the direction of a movement.
    #[inline]
    pub fn expand_towards(self, x: f64, y: f64, z: f64) -> Self {
        let mut b = self;
        if x < 0.0 {
            b.min_x += x;
        } else if x > 0.0 {
            b.max_x += x;
        }
        if y < 0.0 {
            b.min_y += y;
        } else if y > 0.0 {
            b.max_y += y;
        }
        if z < 0.0 {
            b.min_z += z;
        } else if z > 0.0 {
            b.max_z += z;
        }
        Self::new(b.min_x, b.min_y, b.min_z, b.max_x, b.max_y, b.max_z)
    }

    /// `intersects(AABB)`: strict overlap; touching faces do not intersect.
    #[inline]
    pub fn intersects(&self, other: &Self) -> bool {
        // Non-short-circuit `&`: no branches to mispredict in the hot neighbour scans.
        (self.min_x < other.max_x)
            & (self.max_x > other.min_x)
            & (self.min_y < other.max_y)
            & (self.max_y > other.min_y)
            & (self.min_z < other.max_z)
            & (self.max_z > other.min_z)
    }

    /// `contains(x, y, z)`: min inclusive, max exclusive.
    #[inline]
    pub fn contains(&self, x: f64, y: f64, z: f64) -> bool {
        x >= self.min_x
            && x < self.max_x
            && y >= self.min_y
            && y < self.max_y
            && z >= self.min_z
            && z < self.max_z
    }

    /// `getCenter()`.
    #[inline]
    pub fn center(&self) -> Vec3 {
        Vec3::new(
            crate::math::lerp(0.5, self.min_x, self.max_x),
            crate::math::lerp(0.5, self.min_y, self.max_y),
            crate::math::lerp(0.5, self.min_z, self.max_z),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touching_boxes_do_not_intersect() {
        let a = Aabb::new(0.0, 0.0, 0.0, 1.0, 1.0, 1.0);
        assert!(!a.intersects(&a.offset(1.0, 0.0, 0.0)));
        assert!(a.intersects(&a.offset(0.5, 0.5, 0.5)));
        assert_eq!(Aabb::new(1.0, 1.0, 1.0, 0.0, 0.0, 0.0), a);
    }

    #[test]
    fn expand_towards_grows_one_side() {
        let a = Aabb::new(0.0, 0.0, 0.0, 1.0, 1.0, 1.0).expand_towards(-2.0, 3.0, 0.0);
        assert_eq!(a, Aabb::new(-2.0, 0.0, 0.0, 1.0, 4.0, 1.0));
    }
}
