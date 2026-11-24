//! `BitSetDiscreteVoxelShape` with `DiscreteVoxelShape.forAllFaces`, as
//! `StructureTemplate.updateShapeAtEdge` uses it.

use vanillite_base::Direction;

/// A box of voxels, each full or empty.
#[derive(Clone, Debug)]
pub struct BitSetVoxelShape {
    x_size: usize,
    y_size: usize,
    z_size: usize,
    bits: Vec<u64>,
}

impl BitSetVoxelShape {
    /// An empty shape of the given size.
    pub fn new(x_size: usize, y_size: usize, z_size: usize) -> Self {
        let len = x_size * y_size * z_size;
        Self {
            x_size,
            y_size,
            z_size,
            bits: vec![0; len.div_ceil(64)],
        }
    }

    #[inline]
    fn index(&self, x: usize, y: usize, z: usize) -> usize {
        (x * self.y_size + y) * self.z_size + z
    }

    /// `fill(x, y, z)`.
    pub fn fill(&mut self, x: usize, y: usize, z: usize) {
        let i = self.index(x, y, z);
        self.bits[i / 64] |= 1 << (i % 64);
    }

    /// `isFull(x, y, z)`.
    #[inline]
    pub fn is_full(&self, x: usize, y: usize, z: usize) -> bool {
        let i = self.index(x, y, z);
        self.bits[i / 64] & (1 << (i % 64)) != 0
    }

    /// `forAllFaces(consumer)`: every face between a full and an empty (or outside) voxel, as
    /// `(direction, x, y, z)` of the full voxel, in Vanilla's order: the Z faces (x, y, z
    /// loops), then the Y faces (z, x, y loops), then the X faces (y, z, x loops).
    pub fn for_all_faces(&self, mut consumer: impl FnMut(Direction, usize, usize, usize)) {
        let (xs, ys, zs) = (self.x_size, self.y_size, self.z_size);
        // AxisCycle.NONE: a = x, b = y, c = z.
        for x in 0..xs {
            for y in 0..ys {
                self.scan(
                    zs,
                    |z| (x, y, z),
                    Direction::North,
                    Direction::South,
                    &mut consumer,
                );
            }
        }
        // AxisCycle.FORWARD (inverse BACKWARD): a = z, b = x, c = y.
        for z in 0..zs {
            for x in 0..xs {
                self.scan(
                    ys,
                    |y| (x, y, z),
                    Direction::Down,
                    Direction::Up,
                    &mut consumer,
                );
            }
        }
        // AxisCycle.BACKWARD (inverse FORWARD): a = y, b = z, c = x.
        for y in 0..ys {
            for z in 0..zs {
                self.scan(
                    xs,
                    |x| (x, y, z),
                    Direction::West,
                    Direction::East,
                    &mut consumer,
                );
            }
        }
    }

    /// One line of `forAllAxisFaces` along the `c` axis.
    fn scan(
        &self,
        c_size: usize,
        at: impl Fn(usize) -> (usize, usize, usize),
        negative: Direction,
        positive: Direction,
        consumer: &mut impl FnMut(Direction, usize, usize, usize),
    ) {
        let mut last_full = false;
        for c in 0..=c_size {
            let full = c != c_size && {
                let (x, y, z) = at(c);
                self.is_full(x, y, z)
            };
            if !last_full && full {
                let (x, y, z) = at(c);
                consumer(negative, x, y, z);
            }
            if last_full && !full {
                let (x, y, z) = at(c - 1);
                consumer(positive, x, y, z);
            }
            last_full = full;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_voxel_has_six_faces_in_vanilla_order() {
        let mut shape = BitSetVoxelShape::new(1, 1, 1);
        shape.fill(0, 0, 0);
        let mut faces = Vec::new();
        shape.for_all_faces(|d, _, _, _| faces.push(d));
        assert_eq!(
            faces,
            [
                Direction::North,
                Direction::South,
                Direction::Down,
                Direction::Up,
                Direction::West,
                Direction::East
            ]
        );
    }
}
