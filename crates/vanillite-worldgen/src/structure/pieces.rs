//! `StructurePiecesBuilder`: collects the pieces of a start while it is generated.

use vanillite_base::random::RandomSource;

use super::piece::{StructurePiece, create_bounding_box, find_collision_piece};
use crate::template::BoundingBox;

/// `StructurePiecesBuilder`.
#[derive(Debug, Default)]
pub struct StructurePiecesBuilder {
    /// The pieces in insertion order (the first one is the start's centre piece).
    pub pieces: Vec<Box<dyn StructurePiece>>,
}

impl StructurePiecesBuilder {
    /// An empty builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// `addPiece(piece)`.
    pub fn add_piece(&mut self, piece: Box<dyn StructurePiece>) {
        self.pieces.push(piece);
    }

    /// `findCollisionPiece(box)`: the index of the first piece intersecting `bx`.
    pub fn find_collision_piece(&self, bx: &BoundingBox) -> Option<usize> {
        find_collision_piece(&self.pieces, bx)
    }

    /// `offsetPiecesVertically(dy)`.
    pub fn offset_pieces_vertically(&mut self, dy: i32) {
        for piece in &mut self.pieces {
            piece.move_by(0, dy, 0);
        }
    }

    /// `moveBelowSeaLevel(seaLevel, minY, random, offset)`: returns the offset applied.
    pub fn move_below_sea_level<R: RandomSource + ?Sized>(
        &mut self,
        sea_level: i32,
        min_y: i32,
        random: &mut R,
        offset: i32,
    ) -> i32 {
        let max_y = sea_level - offset;
        let bounding_box = self.bounding_box();
        let mut y1 = bounding_box.y_span() + min_y + 1;
        if y1 < max_y {
            y1 += random.next_int_bounded(max_y - y1);
        }
        let dy = y1 - bounding_box.max_y();
        self.offset_pieces_vertically(dy);
        dy
    }

    /// `moveInsideHeights(random, lowestAllowed, highestAllowed)`.
    pub fn move_inside_heights<R: RandomSource + ?Sized>(
        &mut self,
        random: &mut R,
        lowest_allowed: i32,
        highest_allowed: i32,
    ) {
        let bounding_box = self.bounding_box();
        let span = highest_allowed - lowest_allowed + 1 - bounding_box.y_span();
        let y0 = if span > 1 {
            lowest_allowed + random.next_int_bounded(span)
        } else {
            lowest_allowed
        };
        let dy = y0 - bounding_box.min_y();
        self.offset_pieces_vertically(dy);
    }

    /// `build()`: the pieces.
    pub fn build(self) -> Vec<Box<dyn StructurePiece>> {
        self.pieces
    }

    /// `clear()`.
    pub fn clear(&mut self) {
        self.pieces.clear();
    }

    /// `isEmpty()`.
    pub fn is_empty(&self) -> bool {
        self.pieces.is_empty()
    }

    /// `getBoundingBox()`. Panics without pieces, as Vanilla throws.
    pub fn bounding_box(&self) -> BoundingBox {
        create_bounding_box(&self.pieces).expect("Unable to calculate boundingbox without pieces")
    }
}
