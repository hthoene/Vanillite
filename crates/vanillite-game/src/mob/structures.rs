//! The structures at a position for natural spawning (`StructureManager.getAllStructuresAt`,
//! `structureHasPieceAt`, `getStructureAt`), read from the `structures` tag the world keeps
//! with every chunk (starts and references) and cached per chunk.

use std::sync::Arc;

use vanillite_base::hash::FxHashMap;
use vanillite_base::{BlockPos, ChunkPos};
use vanillite_chunk::ProtoChunk;
use vanillite_nbt::{Compound, Tag};

use crate::level::Level;

/// A box of block positions (`BoundingBox`, inclusive).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockBox {
    pub min: BlockPos,
    pub max: BlockPos,
}

impl BlockBox {
    /// `isInside(pos)`.
    pub fn contains(&self, p: BlockPos) -> bool {
        p.x >= self.min.x
            && p.x <= self.max.x
            && p.y >= self.min.y
            && p.y <= self.max.y
            && p.z >= self.min.z
            && p.z <= self.max.z
    }

    fn of(v: &[i32]) -> Option<BlockBox> {
        (v.len() == 6).then(|| BlockBox {
            min: BlockPos::new(v[0], v[1], v[2]),
            max: BlockPos::new(v[3], v[4], v[5]),
        })
    }

    fn encapsulate(&mut self, o: &BlockBox) {
        self.min = BlockPos::new(
            self.min.x.min(o.min.x),
            self.min.y.min(o.min.y),
            self.min.z.min(o.min.z),
        );
        self.max = BlockPos::new(
            self.max.x.max(o.max.x),
            self.max.y.max(o.max.y),
            self.max.z.max(o.max.z),
        );
    }
}

/// A structure start of a chunk.
#[derive(Clone, Debug)]
pub struct Start {
    pub structure: Box<str>,
    /// The box around all pieces.
    pub bounds: BlockBox,
    pub pieces: Vec<BlockBox>,
}

/// The structure data of one chunk.
#[derive(Clone, Debug, Default)]
pub struct ChunkStructures {
    pub starts: Vec<Start>,
    /// Structure → chunks with its starts that reach into this chunk.
    pub references: Vec<(Box<str>, Vec<ChunkPos>)>,
}

impl ChunkStructures {
    /// Parses the `structures` tag of a chunk's stored data.
    pub fn parse(tag: &Compound) -> ChunkStructures {
        let mut out = ChunkStructures::default();
        if let Some(Tag::Compound(starts)) = tag.get("starts") {
            for (name, t) in starts.iter() {
                let Tag::Compound(c) = t else {
                    continue;
                };
                if c.get("id").and_then(Tag::as_str) == Some("INVALID") {
                    continue;
                }
                let mut pieces = Vec::new();
                if let Some(Tag::List(children)) = c.get("Children") {
                    for child in &children.0 {
                        if let Tag::Compound(p) = child
                            && let Some(Tag::IntArray(bb)) = p.get("BB")
                            && let Some(b) = BlockBox::of(bb)
                        {
                            pieces.push(b);
                        }
                    }
                }
                let Some(first) = pieces.first().copied() else {
                    continue;
                };
                let mut bounds = first;
                for p in &pieces[1..] {
                    bounds.encapsulate(p);
                }
                out.starts.push(Start {
                    structure: Box::from(name),
                    bounds,
                    pieces,
                });
            }
        }
        if let Some(Tag::Compound(refs)) = tag.get("References") {
            for (name, t) in refs.iter() {
                let Tag::LongArray(v) = t else {
                    continue;
                };
                #[allow(clippy::cast_possible_truncation)]
                let chunks = v
                    .iter()
                    .map(|&l| ChunkPos::new(l as i32, (l >> 32) as i32))
                    .collect();
                out.references.push((Box::from(name), chunks));
            }
        }
        out
    }

    fn start(&self, structure: &str) -> Option<&Start> {
        self.starts.iter().find(|s| &*s.structure == structure)
    }
}

/// A cache of parsed chunk structure data (per world, or per tile in tile region mode).
///
/// Entries are keyed by the chunk's stored data, so a reloaded chunk is parsed again. When the
/// cache reaches its sweep size, entries of chunks that are no longer loaded (or were reloaded)
/// are dropped and the next sweep waits until it has doubled again: it stays bounded by twice
/// the loaded chunks it was asked about, at amortised constant cost, and never drops the
/// chunks still in use.
#[derive(Debug, Default)]
pub struct StructureIndex {
    chunks: FxHashMap<ChunkPos, (usize, Arc<ChunkStructures>)>,
    /// Size at which the next sweep runs (at least [`MIN_SWEEP`]).
    sweep_at: usize,
}

/// The cache is not swept below this many chunks.
const MIN_SWEEP: usize = 4_096;

/// The cache key of a chunk's stored data.
fn key_of(chunk: &ProtoChunk) -> usize {
    chunk.extra.as_ref().map_or(0, |a| a.as_ptr() as usize)
}

impl StructureIndex {
    /// The structure data of a loaded chunk (`None` when it is not loaded).
    pub fn chunk(&mut self, level: &Level, pos: ChunkPos) -> Option<Arc<ChunkStructures>> {
        let chunk = level.chunks.chunk(pos)?;
        let key = key_of(chunk);
        if let Some((k, s)) = self.chunks.get(&pos)
            && *k == key
        {
            return Some(Arc::clone(s));
        }
        let parsed = chunk
            .extra
            .as_ref()
            .and_then(|bytes| {
                vanillite_nbt::read::read_network(
                    &mut &bytes[..],
                    &mut vanillite_nbt::read::Budget::file(),
                )
                .ok()
            })
            .and_then(|t| match t {
                Some(Tag::Compound(c)) => Some(c),
                _ => None,
            })
            .and_then(|c| match c.get("structures") {
                Some(Tag::Compound(s)) => Some(ChunkStructures::parse(s)),
                _ => None,
            })
            .unwrap_or_default();
        if self.chunks.len() >= self.sweep_at.max(MIN_SWEEP) {
            self.chunks
                .retain(|&p, (k, _)| level.chunks.chunk(p).is_some_and(|c| key_of(c) == *k));
            self.sweep_at = self.chunks.len() * 2;
        }
        let parsed = Arc::new(parsed);
        self.chunks.insert(pos, (key, Arc::clone(&parsed)));
        Some(parsed)
    }

    /// The structures whose starts reach `pos`, with whether `pos` is in one of their pieces
    /// (`structureHasPieceAt`) and in their whole box (`getBoundingBox().isInside`).
    pub fn structures_at(
        &mut self,
        level: &Level,
        pos: BlockPos,
        mut f: impl FnMut(&str, bool, bool),
    ) {
        let Some(here) = self.chunk(level, pos.chunk()) else {
            return;
        };
        for (name, refs) in &here.references {
            let (mut in_piece, mut in_box) = (false, false);
            for &c in refs {
                let Some(other) = self.chunk(level, c) else {
                    continue;
                };
                if let Some(start) = other.start(name) {
                    in_box |= start.bounds.contains(pos);
                    in_piece |= start.pieces.iter().any(|p| p.contains(pos));
                }
            }
            if in_piece || in_box {
                f(name, in_piece, in_box);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vanillite_nbt::List;

    #[test]
    fn parses_starts_and_references() {
        let mut piece = Compound::new();
        piece.insert("BB", vec![0, 60, 0, 6, 66, 8]);
        let mut start = Compound::new();
        start.insert("id", "minecraft:swamp_hut");
        start.insert("Children", List(vec![Tag::Compound(piece)]));
        let mut starts = Compound::new();
        starts.insert("minecraft:swamp_hut", start);
        let mut refs = Compound::new();
        refs.insert("minecraft:swamp_hut", Tag::LongArray(vec![0]));
        let mut tag = Compound::new();
        tag.insert("starts", starts);
        tag.insert("References", refs);
        let s = ChunkStructures::parse(&tag);
        assert_eq!(s.starts.len(), 1);
        assert!(s.starts[0].bounds.contains(BlockPos::new(3, 62, 3)));
        assert_eq!(s.references[0].1, vec![ChunkPos::new(0, 0)]);
    }
}
