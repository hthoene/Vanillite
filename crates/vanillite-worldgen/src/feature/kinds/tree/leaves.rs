//! The end of `TreeFeature.place`: leaves get their `distance` from the logs
//! (`TreeFeature.updateLeaves`) and the blocks at the tree's outer faces update their shapes
//! (`StructureTemplate.updateShapeAtEdge`).

use vanillite_base::{BlockPos, Direction};

use super::{JavaHashSet, TREE_UPDATE_FLAGS, TreeBlocks, try_set_int};
use crate::behaviour;
use crate::feature::region::WorldGenRegion;

/// `LeavesBlock.DECAY_DISTANCE`: the largest leaves distance.
const MAX_DISTANCE: usize = 7;

/// A `BoundingBox` (inclusive bounds).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoundingBox {
    min: BlockPos,
    max: BlockPos,
}

impl BoundingBox {
    /// `BoundingBox.encapsulatingPositions`: `None` for no positions.
    pub fn encapsulating(mut positions: impl Iterator<Item = BlockPos>) -> Option<Self> {
        let first = positions.next()?;
        Some(positions.fold(
            Self {
                min: first,
                max: first,
            },
            |b, p| Self {
                min: BlockPos::new(b.min.x.min(p.x), b.min.y.min(p.y), b.min.z.min(p.z)),
                max: BlockPos::new(b.max.x.max(p.x), b.max.y.max(p.y), b.max.z.max(p.z)),
            },
        ))
    }

    /// The minimum corner.
    pub fn min(&self) -> BlockPos {
        self.min
    }

    /// `isInside(pos)`.
    #[inline]
    pub fn is_inside(&self, pos: BlockPos) -> bool {
        pos.x >= self.min.x
            && pos.x <= self.max.x
            && pos.z >= self.min.z
            && pos.z <= self.max.z
            && pos.y >= self.min.y
            && pos.y <= self.max.y
    }

    /// `getXSpan()`, `getYSpan()`, `getZSpan()`.
    fn spans(&self) -> [usize; 3] {
        [
            (self.max.x - self.min.x + 1) as usize,
            (self.max.y - self.min.y + 1) as usize,
            (self.max.z - self.min.z + 1) as usize,
        ]
    }
}

/// `BitSetDiscreteVoxelShape`: one bit per block of a box.
#[derive(Clone, Debug)]
pub struct VoxelShape {
    size: [usize; 3],
    bits: Vec<u64>,
}

impl VoxelShape {
    /// An empty shape of the given size.
    pub fn new(size: [usize; 3]) -> Self {
        let count = size[0] * size[1] * size[2];
        Self {
            size,
            bits: vec![0; count.div_ceil(64)],
        }
    }

    #[inline]
    fn index(&self, x: usize, y: usize, z: usize) -> usize {
        (x * self.size[1] + y) * self.size[2] + z
    }

    /// `fill(x, y, z)`.
    #[inline]
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

    /// `DiscreteVoxelShape.forAllFaces`: every face between a full and an empty (or outside)
    /// cell, in Vanilla's order: Z faces (x, y outer), then Y faces (z, x outer), then X faces
    /// (y, z outer); along each line the negative face of a run comes before its positive face.
    pub fn for_all_faces(&self, mut consumer: impl FnMut(Direction, i32, i32, i32)) {
        let [sx, sy, sz] = self.size;
        // (line axes a, b; scan axis c) → (x, y, z), with the faces of the scan axis.
        let passes: [([usize; 3], Direction, Direction); 3] = [
            ([sx, sy, sz], Direction::North, Direction::South),
            ([sz, sx, sy], Direction::Down, Direction::Up),
            ([sy, sz, sx], Direction::West, Direction::East),
        ];
        for (pass, ([a_size, b_size, c_size], negative, positive)) in passes.into_iter().enumerate()
        {
            let to_xyz = |a: usize, b: usize, c: usize| match pass {
                0 => (a, b, c),
                1 => (b, c, a),
                _ => (c, a, b),
            };
            for a in 0..a_size {
                for b in 0..b_size {
                    let mut last_full = false;
                    for c in 0..=c_size {
                        let full = c != c_size && {
                            let (x, y, z) = to_xyz(a, b, c);
                            self.is_full(x, y, z)
                        };
                        if !last_full && full {
                            let (x, y, z) = to_xyz(a, b, c);
                            consumer(negative, x as i32, y as i32, z as i32);
                        }
                        if last_full && !full {
                            let (x, y, z) = to_xyz(a, b, c - 1);
                            consumer(positive, x as i32, y as i32, z as i32);
                        }
                        last_full = full;
                    }
                }
            }
        }
    }
}

/// `LeavesBlock.getOptionalDistanceAt(state)`.
fn optional_distance(
    blocks: &TreeBlocks,
    state: vanillite_data::block::BlockState,
) -> Option<usize> {
    if blocks.prevents_nearby_leaf_decay.contains(state) {
        return Some(0);
    }
    state.value("distance").and_then(|v| v.parse().ok())
}

/// `TreeFeature.updateLeaves`: gives leaves within `bounds` their distance to the nearest log
/// (processing positions in Vanilla's `HashSet` order) and returns the shape of everything the
/// tree consists of.
pub fn update_leaves(
    level: &mut WorldGenRegion<'_>,
    blocks: &TreeBlocks,
    bounds: &BoundingBox,
    logs: &JavaHashSet,
    decorations: &JavaHashSet,
    roots: &JavaHashSet,
) -> VoxelShape {
    let mut shape = VoxelShape::new(bounds.spans());
    let local = |pos: BlockPos| {
        (
            (pos.x - bounds.min.x) as usize,
            (pos.y - bounds.min.y) as usize,
            (pos.z - bounds.min.z) as usize,
        )
    };
    for pos in decorations.iter().chain(roots.iter()) {
        if bounds.is_inside(pos) {
            let (x, y, z) = local(pos);
            shape.fill(x, y, z);
        }
    }
    let mut to_check: [JavaHashSet; MAX_DISTANCE] = Default::default();
    for pos in logs.iter() {
        to_check[0].insert(pos);
    }
    let mut smallest = 0;
    loop {
        while smallest < MAX_DISTANCE {
            let Some(pos) = to_check[smallest].pop_first() else {
                break;
            };
            if !bounds.is_inside(pos) {
                continue;
            }
            if smallest != 0 {
                let state = level.block(pos);
                level.set_block(
                    pos,
                    try_set_int(state, "distance", smallest as i32),
                    TREE_UPDATE_FLAGS,
                );
            }
            let (x, y, z) = local(pos);
            shape.fill(x, y, z);
            for direction in Direction::ALL {
                let neighbour = pos.relative(direction);
                if !bounds.is_inside(neighbour) {
                    continue;
                }
                let (nx, ny, nz) = local(neighbour);
                if shape.is_full(nx, ny, nz) {
                    continue;
                }
                if let Some(distance) = optional_distance(blocks, level.block(neighbour)) {
                    let new_distance = distance.min(smallest + 1);
                    if new_distance < MAX_DISTANCE {
                        to_check[new_distance].insert(neighbour);
                        smallest = smallest.min(new_distance);
                    }
                }
            }
        }
        if smallest >= MAX_DISTANCE {
            return shape;
        }
        smallest += 1;
    }
}

/// `StructureTemplate.updateShapeAtEdge(level, updateMode, shape, x, y, z)`: updates the shapes
/// of the blocks on both sides of every outer face of `shape` placed at `start`.
pub fn update_shape_at_edge(
    level: &mut WorldGenRegion<'_>,
    update_mode: i32,
    shape: &VoxelShape,
    start: BlockPos,
) {
    let flags = update_mode & !1;
    shape.for_all_faces(|direction, x, y, z| {
        let pos = start.offset(x, y, z);
        let neighbour_pos = pos.relative(direction);
        let state = level.block(pos);
        let neighbour = level.block(neighbour_pos);
        let new_state =
            behaviour::update_shape(level, state, pos, direction, neighbour_pos, neighbour);
        if state != new_state {
            level.set_block(pos, new_state, flags);
        }
        let new_neighbour = behaviour::update_shape(
            level,
            neighbour,
            neighbour_pos,
            direction.opposite(),
            pos,
            new_state,
        );
        if neighbour != new_neighbour {
            level.set_block(neighbour_pos, new_neighbour, flags);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn faces_of_a_single_cell_in_vanilla_order() {
        let mut shape = VoxelShape::new([1, 1, 1]);
        shape.fill(0, 0, 0);
        let mut faces = Vec::new();
        shape.for_all_faces(|d, x, y, z| faces.push((d, x, y, z)));
        assert_eq!(
            faces,
            vec![
                (Direction::North, 0, 0, 0),
                (Direction::South, 0, 0, 0),
                (Direction::Down, 0, 0, 0),
                (Direction::Up, 0, 0, 0),
                (Direction::West, 0, 0, 0),
                (Direction::East, 0, 0, 0),
            ]
        );
    }

    #[test]
    fn face_scan_orders_match_axis_cycles() {
        // Two cells along X: the X pass (y, z outer) reports one run; Y faces iterate z, then x.
        let mut shape = VoxelShape::new([2, 1, 1]);
        shape.fill(0, 0, 0);
        shape.fill(1, 0, 0);
        let mut faces = Vec::new();
        shape.for_all_faces(|d, x, y, z| faces.push((d, x, y, z)));
        assert_eq!(
            faces,
            vec![
                (Direction::North, 0, 0, 0),
                (Direction::South, 0, 0, 0),
                (Direction::North, 1, 0, 0),
                (Direction::South, 1, 0, 0),
                (Direction::Down, 0, 0, 0),
                (Direction::Up, 0, 0, 0),
                (Direction::Down, 1, 0, 0),
                (Direction::Up, 1, 0, 0),
                (Direction::West, 0, 0, 0),
                (Direction::East, 1, 0, 0),
            ]
        );
    }
}
