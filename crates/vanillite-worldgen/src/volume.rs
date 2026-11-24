//! `DensityVolume`: a regular 3D grid of block positions that density functions are sampled on.
//!
//! Buffers are plain `[f32]` slices in the volume's index order: Y fastest, then X, then Z
//! (`index = y + (x + z * size_x) * size_y`), exactly as Vanilla's `DensityBuffer`.

/// A grid of `size_x × size_y × size_z` sample points starting at a block position with a block
/// step per axis (`DensityVolume`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DensityVolume {
    pub size_x: i32,
    pub size_y: i32,
    pub size_z: i32,
    pub min_x: i32,
    pub min_y: i32,
    pub min_z: i32,
    pub step_x: i32,
    pub step_y: i32,
    pub step_z: i32,
}

impl DensityVolume {
    /// A volume with a block step of 1 on every axis.
    pub const fn new(
        size_x: i32,
        size_y: i32,
        size_z: i32,
        min_x: i32,
        min_y: i32,
        min_z: i32,
    ) -> Self {
        Self::with_step(size_x, size_y, size_z, min_x, min_y, min_z, 1, 1, 1)
    }

    /// A volume with the given block steps. Sizes and steps must be positive.
    #[allow(clippy::too_many_arguments)]
    pub const fn with_step(
        size_x: i32,
        size_y: i32,
        size_z: i32,
        min_x: i32,
        min_y: i32,
        min_z: i32,
        step_x: i32,
        step_y: i32,
        step_z: i32,
    ) -> Self {
        assert!(
            size_x > 0 && size_y > 0 && size_z > 0,
            "volume size must be positive"
        );
        assert!(
            step_x > 0 && step_y > 0 && step_z > 0,
            "volume step must be positive"
        );
        Self {
            size_x,
            size_y,
            size_z,
            min_x,
            min_y,
            min_z,
            step_x,
            step_y,
            step_z,
        }
    }

    /// `indexUnchecked(x, y, z)`.
    #[inline]
    pub const fn index(&self, x: i32, y: i32, z: i32) -> usize {
        (y + (x + z * self.size_x) * self.size_y) as usize
    }

    /// Block X of grid column `x`.
    #[inline]
    pub const fn block_x(&self, x: i32) -> i32 {
        self.min_x + x * self.step_x
    }

    /// Block Y of grid row `y`.
    #[inline]
    pub const fn block_y(&self, y: i32) -> i32 {
        self.min_y + y * self.step_y
    }

    /// Block Z of grid column `z`.
    #[inline]
    pub const fn block_z(&self, z: i32) -> i32 {
        self.min_z + z * self.step_z
    }

    /// `maxBlockX()`.
    pub const fn max_x(&self) -> i32 {
        self.min_x + self.size_x * self.step_x - 1
    }

    /// `maxBlockY()`.
    pub const fn max_y(&self) -> i32 {
        self.min_y + self.size_y * self.step_y - 1
    }

    /// `maxBlockZ()`.
    pub const fn max_z(&self) -> i32 {
        self.min_z + self.size_z * self.step_z - 1
    }

    /// Number of sample points.
    #[inline]
    pub const fn len(&self) -> usize {
        (self.size_x * self.size_y * self.size_z) as usize
    }

    /// Always `false`: volumes have at least one point.
    pub const fn is_empty(&self) -> bool {
        false
    }

    /// `indexOfBlock(x, y, z)`: the index of a block position on the grid, if it is on it.
    pub fn index_of_block(&self, x: i32, y: i32, z: i32) -> Option<usize> {
        let rx = x.wrapping_sub(self.min_x);
        let ry = y.wrapping_sub(self.min_y);
        let rz = z.wrapping_sub(self.min_z);
        if self.step_x == 1 && self.step_y == 1 && self.step_z == 1 {
            if rx >= 0
                && ry >= 0
                && rz >= 0
                && rx < self.size_x
                && ry < self.size_y
                && rz < self.size_z
            {
                return Some(self.index(rx, ry, rz));
            }
        } else if rx >= 0
            && ry >= 0
            && rz >= 0
            && rx < self.size_x * self.step_x
            && ry < self.size_y * self.step_y
            && rz < self.size_z * self.step_z
            && rx.rem_euclid(self.step_x) == 0
            && ry.rem_euclid(self.step_y) == 0
            && rz.rem_euclid(self.step_z) == 0
        {
            return Some(self.index(rx / self.step_x, ry / self.step_y, rz / self.step_z));
        }
        None
    }
}
