//! Force-loaded chunks (`TicketStorage`'s `forced` tickets): each keeps its chunk loaded and
//! entity ticking (`ChunkMap.FORCED_TICKET_LEVEL`).

use vanillite_base::ChunkPos;
use vanillite_base::hash::FxHashMap;
use vanillite_world::chunks::{Chunks, TicketId};

/// `ChunkLevel.byStatus(FullChunkStatus.ENTITY_TICKING)`.
pub const FORCED_TICKET_LEVEL: u8 = 31;

/// `ChunkPos.pack`.
pub fn pack(pos: ChunkPos) -> i64 {
    (i64::from(pos.x) & 0xFFFF_FFFF) | ((i64::from(pos.z) & 0xFFFF_FFFF) << 32)
}

/// The forced chunks of a level with their tickets.
#[derive(Debug, Default)]
pub struct ForcedChunks {
    tickets: FxHashMap<ChunkPos, (TicketId, TicketId)>,
    /// Changed since the last save.
    dirty: bool,
}

impl ForcedChunks {
    /// Whether `pos` is forced.
    pub fn contains(&self, pos: ChunkPos) -> bool {
        self.tickets.contains_key(&pos)
    }

    /// The number of forced chunks.
    pub fn len(&self) -> usize {
        self.tickets.len()
    }

    /// Whether no chunk is forced.
    pub fn is_empty(&self) -> bool {
        self.tickets.is_empty()
    }

    /// The forced chunks sorted like Vanilla's `forcedChunks.longStream().sorted()`.
    pub fn sorted(&self) -> Vec<ChunkPos> {
        let mut out: Vec<ChunkPos> = self.tickets.keys().copied().collect();
        out.sort_by_key(|p| pack(*p));
        out
    }

    /// `updateChunkForced`: adds or removes the ticket; whether anything changed.
    pub fn set(&mut self, chunks: &mut Chunks, pos: ChunkPos, forced: bool) -> bool {
        let changed = if forced {
            match self.tickets.entry(pos) {
                std::collections::hash_map::Entry::Occupied(_) => false,
                std::collections::hash_map::Entry::Vacant(slot) => {
                    let load = chunks.add_ticket(pos, FORCED_TICKET_LEVEL);
                    let simulation = chunks.add_simulation_ticket(pos, FORCED_TICKET_LEVEL);
                    slot.insert((load, simulation));
                    true
                }
            }
        } else if let Some((load, simulation)) = self.tickets.remove(&pos) {
            chunks.remove_ticket(load);
            chunks.remove_simulation_ticket(simulation);
            true
        } else {
            false
        };
        self.dirty |= changed;
        changed
    }

    /// Whether the set changed since the last call (`SavedData.isDirty`).
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }
}
