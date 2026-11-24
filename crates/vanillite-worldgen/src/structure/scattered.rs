//! `ScatteredFeaturePiece`: the single-piece temples (swamp hut, desert pyramid, jungle
//! temple) that settle on the ground the first time they are placed.

use vanillite_base::{BlockPos, Direction};
use vanillite_chunk::HeightmapKind;
use vanillite_nbt::Compound;

use super::piece::{PieceBase, make_bounding_box};
use crate::feature::region::WorldGenRegion;
use crate::template::BoundingBox;

/// The state of Vanilla's `ScatteredFeaturePiece`.
#[derive(Clone, Debug)]
pub struct ScatteredPiece {
    pub base: PieceBase,
    pub width: i32,
    pub height: i32,
    pub depth: i32,
    /// `heightPosition`: -1 until the piece settled.
    pub height_position: i32,
}

impl ScatteredPiece {
    /// `new ScatteredFeaturePiece(type, west, floor, north, width, height, depth, direction)`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        kind: &'static str,
        west: i32,
        floor: i32,
        north: i32,
        width: i32,
        height: i32,
        depth: i32,
        direction: Direction,
    ) -> Self {
        let bounding_box = make_bounding_box(west, floor, north, direction, width, height, depth);
        let mut base = PieceBase::new(kind, 0, bounding_box);
        base.set_orientation(Some(direction));
        Self {
            base,
            width,
            height,
            depth,
            height_position: -1,
        }
    }

    /// `ScatteredFeaturePiece.addAdditionalSaveData`: `Width`, `Height`, `Depth`, `HPos`.
    pub fn save_data(&self) -> Compound {
        Compound::new()
            .with("Width", self.width)
            .with("Height", self.height)
            .with("Depth", self.depth)
            .with("HPos", self.height_position)
    }

    /// `updateAverageGroundHeight(level, chunkBB, offset)`: settles the piece on the average
    /// `MOTION_BLOCKING_NO_LEAVES` height of its columns inside `chunk_bb`.
    pub fn update_average_ground_height(
        &mut self,
        level: &WorldGenRegion<'_>,
        chunk_bb: &BoundingBox,
        offset: i32,
    ) -> bool {
        if self.height_position >= 0 {
            return true;
        }
        let bb = self.base.bounding_box;
        let mut total = 0;
        let mut count = 0;
        for z in bb.min_z()..=bb.max_z() {
            for x in bb.min_x()..=bb.max_x() {
                if chunk_bb.is_inside(BlockPos::new(x, 64, z)) {
                    total += level.height_at(HeightmapKind::MotionBlockingNoLeaves, x, z);
                    count += 1;
                }
            }
        }
        if count == 0 {
            return false;
        }
        self.height_position = total / count;
        let dy = self.height_position - bb.min_y() + offset;
        self.base.bounding_box.translate(0, dy, 0);
        true
    }

    /// `updateHeightPositionToLowestGroundHeight(level, offset)`: settles the piece on the
    /// lowest `MOTION_BLOCKING_NO_LEAVES` height of all its columns.
    pub fn update_height_position_to_lowest_ground_height(
        &mut self,
        level: &WorldGenRegion<'_>,
        offset: i32,
    ) -> bool {
        if self.height_position >= 0 {
            return true;
        }
        let bb = self.base.bounding_box;
        let mut lowest = level.max_y() + 1;
        let mut found = false;
        for z in bb.min_z()..=bb.max_z() {
            for x in bb.min_x()..=bb.max_x() {
                lowest = lowest.min(level.height_at(HeightmapKind::MotionBlockingNoLeaves, x, z));
                found = true;
            }
        }
        if !found {
            return false;
        }
        self.height_position = lowest;
        let dy = self.height_position - bb.min_y() + offset;
        self.base.bounding_box.translate(0, dy, 0);
        true
    }
}
