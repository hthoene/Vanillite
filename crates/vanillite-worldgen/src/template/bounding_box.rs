//! `net.minecraft.world.level.levelgen.structure.BoundingBox`.

use vanillite_base::{BlockPos, ChunkPos, Direction};

/// An axis aligned box of blocks, both corners inclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BoundingBox {
    min_x: i32,
    min_y: i32,
    min_z: i32,
    max_x: i32,
    max_y: i32,
    max_z: i32,
}

impl BoundingBox {
    /// `new BoundingBox(minX, minY, minZ, maxX, maxY, maxZ)`; inverted bounds are swapped, as
    /// Vanilla does after logging.
    pub fn new(min_x: i32, min_y: i32, min_z: i32, max_x: i32, max_y: i32, max_z: i32) -> Self {
        Self {
            min_x: min_x.min(max_x),
            min_y: min_y.min(max_y),
            min_z: min_z.min(max_z),
            max_x: min_x.max(max_x),
            max_y: min_y.max(max_y),
            max_z: min_z.max(max_z),
        }
    }

    /// `new BoundingBox(pos)`: a single block.
    pub fn of_block(pos: BlockPos) -> Self {
        Self::new(pos.x, pos.y, pos.z, pos.x, pos.y, pos.z)
    }

    /// `fromCorners(a, b)`.
    pub fn from_corners(a: BlockPos, b: BlockPos) -> Self {
        Self::new(a.x, a.y, a.z, b.x, b.y, b.z)
    }

    /// `infinite()`.
    pub fn infinite() -> Self {
        Self::new(i32::MIN, i32::MIN, i32::MIN, i32::MAX, i32::MAX, i32::MAX)
    }

    /// `orientBox(footX, footY, footZ, offX, offY, offZ, width, height, depth, direction)`.
    #[allow(clippy::too_many_arguments)]
    pub fn orient_box(
        foot: BlockPos,
        off_x: i32,
        off_y: i32,
        off_z: i32,
        width: i32,
        height: i32,
        depth: i32,
        direction: Direction,
    ) -> Self {
        let (fx, fy, fz) = (foot.x, foot.y, foot.z);
        match direction {
            Direction::North => Self::new(
                fx + off_x,
                fy + off_y,
                fz - depth + 1 + off_z,
                fx + width - 1 + off_x,
                fy + height - 1 + off_y,
                fz + off_z,
            ),
            Direction::West => Self::new(
                fx - depth + 1 + off_z,
                fy + off_y,
                fz + off_x,
                fx + off_z,
                fy + height - 1 + off_y,
                fz + width - 1 + off_x,
            ),
            Direction::East => Self::new(
                fx + off_z,
                fy + off_y,
                fz + off_x,
                fx + depth - 1 + off_z,
                fy + height - 1 + off_y,
                fz + width - 1 + off_x,
            ),
            _ => Self::new(
                fx + off_x,
                fy + off_y,
                fz + off_z,
                fx + width - 1 + off_x,
                fy + height - 1 + off_y,
                fz + depth - 1 + off_z,
            ),
        }
    }

    pub fn min_x(&self) -> i32 {
        self.min_x
    }

    pub fn min_y(&self) -> i32 {
        self.min_y
    }

    pub fn min_z(&self) -> i32 {
        self.min_z
    }

    pub fn max_x(&self) -> i32 {
        self.max_x
    }

    pub fn max_y(&self) -> i32 {
        self.max_y
    }

    pub fn max_z(&self) -> i32 {
        self.max_z
    }

    /// `intersects(other)`.
    pub fn intersects(&self, other: &Self) -> bool {
        self.max_x >= other.min_x
            && self.min_x <= other.max_x
            && self.max_z >= other.min_z
            && self.min_z <= other.max_z
            && self.max_y >= other.min_y
            && self.min_y <= other.max_y
    }

    /// `intersects(minX, minZ, maxX, maxZ)`: the XZ projection intersects.
    pub fn intersects_xz(&self, min_x: i32, min_z: i32, max_x: i32, max_z: i32) -> bool {
        self.max_x >= min_x && self.min_x <= max_x && self.max_z >= min_z && self.min_z <= max_z
    }

    /// `encapsulate(other)`.
    pub fn encapsulate(&mut self, other: &Self) {
        self.min_x = self.min_x.min(other.min_x);
        self.min_y = self.min_y.min(other.min_y);
        self.min_z = self.min_z.min(other.min_z);
        self.max_x = self.max_x.max(other.max_x);
        self.max_y = self.max_y.max(other.max_y);
        self.max_z = self.max_z.max(other.max_z);
    }

    /// `encapsulate(pos)`.
    pub fn encapsulate_pos(&mut self, pos: BlockPos) {
        self.encapsulate(&Self::of_block(pos));
    }

    /// `move(dx, dy, dz)` in place.
    pub fn translate(&mut self, dx: i32, dy: i32, dz: i32) {
        self.min_x = self.min_x.wrapping_add(dx);
        self.min_y = self.min_y.wrapping_add(dy);
        self.min_z = self.min_z.wrapping_add(dz);
        self.max_x = self.max_x.wrapping_add(dx);
        self.max_y = self.max_y.wrapping_add(dy);
        self.max_z = self.max_z.wrapping_add(dz);
    }

    /// `moved(dx, dy, dz)`.
    pub fn moved(&self, dx: i32, dy: i32, dz: i32) -> Self {
        let mut moved = *self;
        moved.translate(dx, dy, dz);
        moved
    }

    /// `inflatedBy(x, y, z)`.
    pub fn inflated_by(&self, x: i32, y: i32, z: i32) -> Self {
        Self::new(
            self.min_x - x,
            self.min_y - y,
            self.min_z - z,
            self.max_x + x,
            self.max_y + y,
            self.max_z + z,
        )
    }

    /// `isInside(pos)`.
    #[inline]
    pub fn is_inside(&self, pos: BlockPos) -> bool {
        pos.x >= self.min_x
            && pos.x <= self.max_x
            && pos.z >= self.min_z
            && pos.z <= self.max_z
            && pos.y >= self.min_y
            && pos.y <= self.max_y
    }

    pub fn x_span(&self) -> i32 {
        self.max_x - self.min_x + 1
    }

    pub fn y_span(&self) -> i32 {
        self.max_y - self.min_y + 1
    }

    pub fn z_span(&self) -> i32 {
        self.max_z - self.min_z + 1
    }

    /// `getCenter()`.
    pub fn center(&self) -> BlockPos {
        BlockPos::new(
            self.min_x + (self.max_x - self.min_x + 1) / 2,
            self.min_y + (self.max_y - self.min_y + 1) / 2,
            self.min_z + (self.max_z - self.min_z + 1) / 2,
        )
    }

    /// `forAllCorners`: the eight corners in Vanilla's order.
    pub fn corners(&self) -> [BlockPos; 8] {
        let (a, b) = (self.min_x, self.max_x);
        let (c, d) = (self.min_y, self.max_y);
        let (e, f) = (self.min_z, self.max_z);
        [
            BlockPos::new(b, d, f),
            BlockPos::new(a, d, f),
            BlockPos::new(b, c, f),
            BlockPos::new(a, c, f),
            BlockPos::new(b, d, e),
            BlockPos::new(a, d, e),
            BlockPos::new(b, c, e),
            BlockPos::new(a, c, e),
        ]
    }

    /// The chunks the box overlaps (`intersectingChunks`), row by row.
    pub fn intersecting_chunks(&self) -> impl Iterator<Item = ChunkPos> {
        let (min_cx, max_cx) = (self.min_x >> 4, self.max_x >> 4);
        let (min_cz, max_cz) = (self.min_z >> 4, self.max_z >> 4);
        (min_cz..=max_cz).flat_map(move |z| (min_cx..=max_cx).map(move |x| ChunkPos::new(x, z)))
    }
}
