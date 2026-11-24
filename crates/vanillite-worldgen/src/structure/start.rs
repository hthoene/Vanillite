//! `StructureStart`: the generated pieces of one structure in its start chunk, and how chunks
//! find the starts that reach them (`createReferences`, `StructureManager.startsForStructure`).

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use vanillite_base::{BlockPos, ChunkPos};

use vanillite_nbt::{Compound, List, Tag};

use super::piece::{PieceContext, StructurePiece, create_bounding_box, data_2d};
use super::{StructureId, StructureRegistry};
use crate::proto::ProtoChunk;
use crate::template::BoundingBox;

/// `StructureStart`: a valid start (Vanilla's `INVALID_START` is never stored).
///
/// Starts are shared (`Arc`) between the start chunk, which stores them, and every chunk that
/// references them. Pieces may change while they are placed (`ScatteredFeaturePiece` settles
/// its height in the first chunk it is placed in, chests remember that they were placed), and
/// later chunks see those changes, as in Vanilla; the pieces are therefore behind a lock.
#[derive(Debug)]
pub struct StructureStart {
    structure: StructureId,
    chunk_pos: ChunkPos,
    references: AtomicI32,
    /// `getBoundingBox()`: computed once from the pieces as generated (Vanilla caches it on
    /// first use, which is the `structure_references` stage, before any piece is placed).
    bounding_box: BoundingBox,
    pieces: Mutex<Vec<Box<dyn StructurePiece>>>,
}

impl StructureStart {
    /// `new StructureStart(structure, chunkPos, references, pieces)`; `None` without pieces
    /// (an invalid start). `bounding_box` is the structure's `adjustBoundingBox` of the pieces'
    /// box.
    pub fn new(
        structure: StructureId,
        chunk_pos: ChunkPos,
        references: i32,
        pieces: Vec<Box<dyn StructurePiece>>,
        adjust: impl FnOnce(BoundingBox) -> BoundingBox,
    ) -> Option<Self> {
        let bounding_box = adjust(create_bounding_box(&pieces)?);
        Some(Self {
            structure,
            chunk_pos,
            references: AtomicI32::new(references),
            bounding_box,
            pieces: Mutex::new(pieces),
        })
    }

    /// `getStructure()`.
    pub fn structure(&self) -> StructureId {
        self.structure
    }

    /// `getChunkPos()`.
    pub fn chunk_pos(&self) -> ChunkPos {
        self.chunk_pos
    }

    /// `getBoundingBox()`.
    pub fn bounding_box(&self) -> BoundingBox {
        self.bounding_box
    }

    /// `getReferences()`.
    pub fn references(&self) -> i32 {
        self.references.load(Ordering::Relaxed)
    }

    /// `addReference()`.
    pub fn add_reference(&self) {
        self.references.fetch_add(1, Ordering::Relaxed);
    }

    /// `canBeReferenced()`.
    pub fn can_be_referenced(&self) -> bool {
        self.references() < 1
    }

    /// `createTag(context, chunkPos)` of a valid start: `id`, `ChunkX`, `ChunkZ`, `references`
    /// and the pieces (`Children`) with their `createTag` (`id`, `BB`, `O`, `GD` and the
    /// piece's own fields).
    pub fn create_tag(&self, registry: &StructureRegistry) -> Compound {
        let children: Vec<Tag> = self
            .pieces()
            .iter()
            .map(|piece| {
                let bb = piece.bounding_box();
                let mut tag = Compound::new()
                    .with("id", piece.base().kind)
                    .with(
                        "BB",
                        vec![
                            bb.min_x(),
                            bb.min_y(),
                            bb.min_z(),
                            bb.max_x(),
                            bb.max_y(),
                            bb.max_z(),
                        ],
                    )
                    .with("O", piece.orientation().map_or(-1, |o| data_2d(o) as i32))
                    .with("GD", piece.gen_depth());
                if let Some(data) = piece.additional_save_data() {
                    for (name, value) in data.iter() {
                        tag.insert(name, value.clone());
                    }
                }
                Tag::Compound(tag)
            })
            .collect();
        Compound::new()
            .with("id", registry.structure(self.structure).name.as_str())
            .with("ChunkX", self.chunk_pos.x)
            .with("ChunkZ", self.chunk_pos.z)
            .with("references", self.references())
            .with("Children", Tag::List(List(children)))
    }

    /// `getPieces()`, locked.
    pub fn pieces(&self) -> MutexGuard<'_, Vec<Box<dyn StructurePiece>>> {
        self.pieces.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `placeInChunk(level, structureManager, generator, random, chunkBB, chunkPos)`: post
    /// processes every piece that intersects `chunk_bb`, then the structure's `afterPlace`.
    pub fn place_in_chunk(
        &self,
        registry: &StructureRegistry,
        ctx: &mut PieceContext<'_, '_>,
        chunk_bb: &mut BoundingBox,
        chunk_pos: ChunkPos,
    ) {
        let mut pieces = self.pieces();
        let Some(first) = pieces.first() else {
            return;
        };
        let center_bb = *first.bounding_box();
        let center = center_bb.center();
        let reference = BlockPos::new(center.x, center_bb.min_y(), center.z);
        for piece in pieces.iter_mut() {
            if piece.bounding_box().intersects(chunk_bb) {
                piece.post_process(ctx, chunk_bb, chunk_pos, reference);
            }
        }
        registry.structure(self.structure).kind.after_place(
            ctx,
            chunk_bb,
            chunk_pos,
            &mut pieces[..],
        );
    }
}

/// The starts stored in one chunk (`ChunkAccess.structureStarts`), in insertion order.
pub type ChunkStarts = Vec<(StructureId, Arc<StructureStart>)>;

/// The structure starts of the 17×17 chunks around a chunk, as `createReferences` reads them
/// (`Stage::StructureReferences` needs radius 8 of `Stage::StructureStarts`).
#[derive(Clone, Debug)]
pub struct NeighbourStarts {
    center: ChunkPos,
    /// Per chunk, `dx` outer and `dz` inner over `-8..=8` (Vanilla's loop order).
    chunks: Vec<ChunkStarts>,
}

/// `createReferences` range.
pub const REFERENCE_RANGE: i32 = 8;

impl NeighbourStarts {
    /// Collects the starts of the chunks within [`REFERENCE_RANGE`] of `center`; `chunk(dx, dz)`
    /// returns the chunk at that offset, which must have finished `Stage::StructureStarts`.
    pub fn collect<'a>(
        center: ChunkPos,
        mut chunk: impl FnMut(i32, i32) -> &'a ProtoChunk,
    ) -> Self {
        Self::from_starts(center, |dx, dz| chunk(dx, dz).structure_starts().clone())
    }

    /// Like [`Self::collect`], with `starts(dx, dz)` returning the starts of the chunk at that
    /// offset.
    pub fn from_starts(center: ChunkPos, mut starts: impl FnMut(i32, i32) -> ChunkStarts) -> Self {
        let mut chunks = Vec::with_capacity(((2 * REFERENCE_RANGE + 1) as usize).pow(2));
        for dx in -REFERENCE_RANGE..=REFERENCE_RANGE {
            for dz in -REFERENCE_RANGE..=REFERENCE_RANGE {
                chunks.push(starts(dx, dz));
            }
        }
        Self { center, chunks }
    }

    /// The centre chunk.
    pub fn center(&self) -> ChunkPos {
        self.center
    }

    /// The starts of each chunk in `createReferences` order: source x outer, z inner.
    pub fn iter(&self) -> impl Iterator<Item = (ChunkPos, &ChunkStarts)> {
        let side = 2 * REFERENCE_RANGE + 1;
        self.chunks.iter().enumerate().map(move |(i, starts)| {
            let i = i as i32;
            let pos = ChunkPos::new(
                self.center.x + i / side - REFERENCE_RANGE,
                self.center.z + i % side - REFERENCE_RANGE,
            );
            (pos, starts)
        })
    }
}

/// The starts a chunk references (`StructureManager.startsForStructure` over the chunk's
/// references), as the terrain stage (`Beardifier`) and the features stage read them.
///
/// Entries are grouped per structure in the order the chunk's references were first added;
/// within a structure they follow the reference set's iteration order (fastutil
/// `LongOpenHashSet`, see [`super::long_set`]). Cloning is cheap.
#[derive(Clone, Debug, Default)]
pub struct ReferencedStarts {
    entries: Arc<[(StructureId, Arc<StructureStart>)]>,
}

impl ReferencedStarts {
    /// No starts (a world without structures, or a chunk no structure reaches).
    pub fn empty() -> Self {
        Self::default()
    }

    /// Resolves the references of `chunk` (past `Stage::StructureReferences`): `starts_at(pos)`
    /// returns the chunk at a referenced position (within 8 chunks, past
    /// `Stage::StructureStarts`); references to chunks without a start of the structure are
    /// skipped, as `fillStartsForStructure` does.
    pub fn collect<'a>(
        chunk: &ProtoChunk,
        mut starts_at: impl FnMut(ChunkPos) -> &'a ProtoChunk,
    ) -> Self {
        Self::collect_with(chunk, |pos, structure| {
            starts_at(pos).start_for_structure(structure).cloned()
        })
    }

    /// Like [`Self::collect`], with `start_of(pos, structure)` returning the start of the
    /// structure in the chunk at `pos`, if there is one.
    pub fn collect_with(
        chunk: &ProtoChunk,
        mut start_of: impl FnMut(ChunkPos, StructureId) -> Option<Arc<StructureStart>>,
    ) -> Self {
        let mut entries = Vec::new();
        for (structure, references) in chunk.structure_references() {
            for key in references.iter() {
                if let Some(start) = start_of(ChunkPos::from_long(key), *structure) {
                    entries.push((*structure, start));
                }
            }
        }
        Self {
            entries: entries.into(),
        }
    }

    /// Every referenced start.
    pub fn all(&self) -> &[(StructureId, Arc<StructureStart>)] {
        &self.entries
    }

    /// Whether no start is referenced.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// `startsForStructure(sectionPos, structure)`.
    pub fn for_structure(
        &self,
        structure: StructureId,
    ) -> impl Iterator<Item = &Arc<StructureStart>> {
        self.entries
            .iter()
            .filter(move |(s, _)| *s == structure)
            .map(|(_, start)| start)
    }
}
