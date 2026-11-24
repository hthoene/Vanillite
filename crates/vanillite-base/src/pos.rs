//! Block, section and chunk positions with Vanilla's packed `long` encodings.

/// A block position (`BlockPos`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockPos {
    /// X coordinate.
    pub x: i32,
    /// Y coordinate.
    pub y: i32,
    /// Z coordinate.
    pub z: i32,
}

const PACKED_XZ_BITS: u32 = 26;
const PACKED_Y_BITS: u32 = 64 - 2 * PACKED_XZ_BITS;
const PACKED_XZ_MASK: i64 = (1 << PACKED_XZ_BITS) - 1;
const PACKED_Y_MASK: i64 = (1 << PACKED_Y_BITS) - 1;
const PACKED_Z_OFFSET: u32 = PACKED_Y_BITS;
const PACKED_X_OFFSET: u32 = PACKED_Y_BITS + PACKED_XZ_BITS;

impl BlockPos {
    /// The origin.
    pub const ZERO: Self = Self::new(0, 0, 0);

    /// A position from coordinates.
    #[inline]
    pub const fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }

    /// The block containing a point (`BlockPos.containing`).
    #[inline]
    pub const fn containing(x: f64, y: f64, z: f64) -> Self {
        Self::new(
            crate::math::floor(x),
            crate::math::floor(y),
            crate::math::floor(z),
        )
    }

    /// `asLong()`: x in 26 bits, z in 26 bits, y in 12 bits; also the protocol `Position`.
    #[inline]
    pub const fn as_long(self) -> i64 {
        ((self.x as i64 & PACKED_XZ_MASK) << PACKED_X_OFFSET)
            | ((self.z as i64 & PACKED_XZ_MASK) << PACKED_Z_OFFSET)
            | (self.y as i64 & PACKED_Y_MASK)
    }

    /// `BlockPos.of(long)`: the inverse of [`Self::as_long`].
    #[inline]
    pub const fn from_long(packed: i64) -> Self {
        Self::new(
            (packed >> PACKED_X_OFFSET) as i32,
            ((packed << (64 - PACKED_Y_BITS)) >> (64 - PACKED_Y_BITS)) as i32,
            ((packed << (64 - PACKED_Z_OFFSET - PACKED_XZ_BITS)) >> (64 - PACKED_XZ_BITS)) as i32,
        )
    }

    /// This position moved by the given amounts.
    #[inline]
    pub const fn offset(self, dx: i32, dy: i32, dz: i32) -> Self {
        Self::new(
            self.x.wrapping_add(dx),
            self.y.wrapping_add(dy),
            self.z.wrapping_add(dz),
        )
    }

    /// The neighbour in a direction (`relative`).
    #[inline]
    pub const fn relative(self, direction: Direction) -> Self {
        let [dx, dy, dz] = direction.normal();
        self.offset(dx, dy, dz)
    }

    /// The section containing this block.
    #[inline]
    pub const fn section(self) -> SectionPos {
        SectionPos::new(self.x >> 4, self.y >> 4, self.z >> 4)
    }

    /// The chunk containing this block.
    #[inline]
    pub const fn chunk(self) -> ChunkPos {
        ChunkPos::new(self.x >> 4, self.z >> 4)
    }

    /// `distSqr`: squared distance between the block corners.
    #[inline]
    pub fn dist_sqr(self, other: Self) -> f64 {
        let dx = f64::from(self.x) - f64::from(other.x);
        let dy = f64::from(self.y) - f64::from(other.y);
        let dz = f64::from(self.z) - f64::from(other.z);
        dx * dx + dy * dy + dz * dz
    }
}

/// A 16×16×16 section position (`SectionPos`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SectionPos {
    /// Section X (block X >> 4).
    pub x: i32,
    /// Section Y (block Y >> 4).
    pub y: i32,
    /// Section Z (block Z >> 4).
    pub z: i32,
}

impl SectionPos {
    /// A position from section coordinates.
    #[inline]
    pub const fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }

    /// `asLong()`: x in 22 bits at 42, z in 22 bits at 20, y in 20 bits at 0.
    #[inline]
    pub const fn as_long(self) -> i64 {
        ((self.x as i64 & 0x3F_FFFF) << 42)
            | ((self.z as i64 & 0x3F_FFFF) << 20)
            | (self.y as i64 & 0xF_FFFF)
    }

    /// The inverse of [`Self::as_long`].
    #[inline]
    pub const fn from_long(packed: i64) -> Self {
        Self::new(
            (packed >> 42) as i32,
            ((packed << 44) >> 44) as i32,
            ((packed << 22) >> 42) as i32,
        )
    }

    /// The lowest-coordinate block of this section.
    #[inline]
    pub const fn origin(self) -> BlockPos {
        BlockPos::new(self.x << 4, self.y << 4, self.z << 4)
    }

    /// The chunk containing this section.
    #[inline]
    pub const fn chunk(self) -> ChunkPos {
        ChunkPos::new(self.x, self.z)
    }
}

/// A chunk column position (`ChunkPos`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChunkPos {
    /// Chunk X (block X >> 4).
    pub x: i32,
    /// Chunk Z (block Z >> 4).
    pub z: i32,
}

impl ChunkPos {
    /// A position from chunk coordinates.
    #[inline]
    pub const fn new(x: i32, z: i32) -> Self {
        Self { x, z }
    }

    /// `toLong()`: x in the low 32 bits, z in the high 32 bits.
    #[inline]
    pub const fn as_long(self) -> i64 {
        (self.x as u32 as i64) | ((self.z as u32 as i64) << 32)
    }

    /// The inverse of [`Self::as_long`].
    #[inline]
    pub const fn from_long(packed: i64) -> Self {
        Self::new(packed as i32, (packed >> 32) as i32)
    }

    /// The lowest X block coordinate (`getMinBlockX`).
    #[inline]
    pub const fn min_block_x(self) -> i32 {
        self.x << 4
    }

    /// The lowest Z block coordinate (`getMinBlockZ`).
    #[inline]
    pub const fn min_block_z(self) -> i32 {
        self.z << 4
    }

    /// The region file this chunk is stored in (`getRegionX`, `getRegionZ`).
    #[inline]
    pub const fn region(self) -> (i32, i32) {
        (self.x >> 5, self.z >> 5)
    }

    /// Chebyshev distance in chunks (`getChessboardDistance`).
    #[inline]
    pub const fn chessboard_distance(self, other: Self) -> i32 {
        let dx = self.x.wrapping_sub(other.x).wrapping_abs();
        let dz = self.z.wrapping_sub(other.z).wrapping_abs();
        if dx > dz { dx } else { dz }
    }
}

/// Vanilla's `Direction`, in Vanilla order (its 3D data value).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Direction {
    /// -Y.
    Down = 0,
    /// +Y.
    Up = 1,
    /// -Z.
    North = 2,
    /// +Z.
    South = 3,
    /// -X.
    West = 4,
    /// +X.
    East = 5,
}

impl Direction {
    /// All directions in Vanilla order.
    pub const ALL: [Self; 6] = [
        Self::Down,
        Self::Up,
        Self::North,
        Self::South,
        Self::West,
        Self::East,
    ];

    /// The unit vector of this direction.
    #[inline]
    pub const fn normal(self) -> [i32; 3] {
        match self {
            Self::Down => [0, -1, 0],
            Self::Up => [0, 1, 0],
            Self::North => [0, 0, -1],
            Self::South => [0, 0, 1],
            Self::West => [-1, 0, 0],
            Self::East => [1, 0, 0],
        }
    }

    /// The opposite direction.
    #[inline]
    pub const fn opposite(self) -> Self {
        match self {
            Self::Down => Self::Up,
            Self::Up => Self::Down,
            Self::North => Self::South,
            Self::South => Self::North,
            Self::West => Self::East,
            Self::East => Self::West,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_pos_packing_round_trips() {
        for pos in [
            BlockPos::new(0, 0, 0),
            BlockPos::new(-1, -64, -1),
            BlockPos::new(29_999_999, 2047, -30_000_000),
            BlockPos::new(-33_554_432 / 2, -2048, 33_554_431 / 2),
        ] {
            assert_eq!(BlockPos::from_long(pos.as_long()), pos);
        }
        // Protocol `Position` layout: x << 38 | z << 12 | y.
        assert_eq!(BlockPos::new(1, 2, 3).as_long(), (1 << 38) | (3 << 12) | 2);
    }

    #[test]
    fn section_and_chunk_packing_round_trips() {
        let section = SectionPos::new(-1_875_000, -4, 1_875_000);
        assert_eq!(SectionPos::from_long(section.as_long()), section);
        let chunk = ChunkPos::new(-5, 7);
        assert_eq!(chunk.as_long(), (7_i64 << 32) | 0xFFFF_FFFB);
        assert_eq!(ChunkPos::from_long(chunk.as_long()), chunk);
    }

    #[test]
    fn containing_floors() {
        assert_eq!(
            BlockPos::containing(-0.5, 64.9, 0.0),
            BlockPos::new(-1, 64, 0)
        );
        assert_eq!(BlockPos::new(-1, -1, 17).chunk(), ChunkPos::new(-1, 1));
    }
}
