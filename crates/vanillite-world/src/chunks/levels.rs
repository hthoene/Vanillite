//! Tickets and chunk levels (Vanilla's `DistanceManager` and `ChunkLevel`).
//!
//! A ticket of level `L` at a chunk gives every chunk at Chebyshev distance `d` the level
//! `L + d`; a chunk's level is the lowest one any ticket gives it. Levels up to 33 mean a full
//! chunk, 34 to 44 a generation status (the pyramid around full chunks), above 44 the chunk is
//! not needed. Each chunk keeps a histogram of the levels its tickets give it, so adding or
//! removing a ticket costs one update per chunk in its reach and never rescans other tickets.

use vanillite_base::ChunkPos;
use vanillite_base::hash::FxHashMap;
use vanillite_chunk::ChunkStatus;
use vanillite_chunk::slots::RADIUS_AROUND_FULL;

/// Level of a full chunk (`ChunkLevel.FULL_CHUNK_LEVEL`).
pub const FULL_LEVEL: u8 = 33;
/// Highest level of a loaded chunk (`ChunkLevel.MAX_LEVEL`).
pub const MAX_LEVEL: u8 = FULL_LEVEL + RADIUS_AROUND_FULL as u8;
/// The level of chunks no ticket reaches.
pub const UNLOADED: u8 = MAX_LEVEL + 1;

/// The status a chunk of this level must reach; `None` if it is not needed.
pub fn required_status(level: u8) -> Option<ChunkStatus> {
    match level {
        0..=FULL_LEVEL => Some(ChunkStatus::Full),
        _ => ChunkStatus::around_full(u32::from(level - FULL_LEVEL)),
    }
}

/// A level change of one chunk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LevelChange {
    /// The chunk.
    pub pos: ChunkPos,
    /// Level before.
    pub old: u8,
    /// Level after.
    pub new: u8,
}

/// Level of block ticking chunks (`ChunkLevel.byStatus(BLOCK_TICKING)`).
pub const BLOCK_TICKING_LEVEL: u8 = 32;
/// Level of entity ticking chunks (`ChunkLevel.byStatus(ENTITY_TICKING)`).
pub const ENTITY_TICKING_LEVEL: u8 = 31;

/// The ticket levels of one world.
#[derive(Debug)]
pub struct TicketLevels {
    counts: FxHashMap<ChunkPos, Box<[u16; UNLOADED as usize]>>,
    /// Levels above this are not tracked (they read as [`UNLOADED`]).
    max: u8,
}

impl Default for TicketLevels {
    fn default() -> Self {
        TicketLevels::with_max(MAX_LEVEL)
    }
}

impl TicketLevels {
    /// Levels that track only chunks up to `max` (at most [`MAX_LEVEL`]): the simulation
    /// distance needs levels up to [`BLOCK_TICKING_LEVEL`].
    pub fn with_max(max: u8) -> Self {
        TicketLevels {
            counts: FxHashMap::default(),
            max: max.min(MAX_LEVEL),
        }
    }

    /// Every tracked chunk with its level.
    pub fn iter(&self) -> impl Iterator<Item = (ChunkPos, u8)> + '_ {
        self.counts
            .iter()
            .map(|(&pos, counts)| (pos, lowest(counts)))
    }

    /// The level of a chunk.
    #[inline]
    pub fn level(&self, pos: ChunkPos) -> u8 {
        self.counts
            .get(&pos)
            .map_or(UNLOADED, |counts| lowest(counts))
    }

    /// Adds a ticket of `level` at `pos`.
    pub fn add(&mut self, pos: ChunkPos, level: u8, changes: &mut Vec<LevelChange>) {
        self.apply(pos, level, true, changes);
    }

    /// Removes a ticket of `level` at `pos` that was added before.
    pub fn remove(&mut self, pos: ChunkPos, level: u8, changes: &mut Vec<LevelChange>) {
        self.apply(pos, level, false, changes);
    }

    fn apply(&mut self, center: ChunkPos, level: u8, add: bool, changes: &mut Vec<LevelChange>) {
        if level > self.max {
            return;
        }
        let reach = i32::from(self.max - level);
        for dz in -reach..=reach {
            for dx in -reach..=reach {
                let pos = ChunkPos::new(center.x + dx, center.z + dz);
                let given = level + dx.unsigned_abs().max(dz.unsigned_abs()) as u8;
                let counts = self
                    .counts
                    .entry(pos)
                    .or_insert_with(|| Box::new([0; UNLOADED as usize]));
                let old = lowest(counts);
                let count = &mut counts[usize::from(given)];
                *count = if add {
                    count.saturating_add(1)
                } else {
                    count.saturating_sub(1)
                };
                let new = lowest(counts);
                if new == UNLOADED {
                    self.counts.remove(&pos);
                }
                if new != old {
                    changes.push(LevelChange { pos, old, new });
                }
            }
        }
    }
}

#[inline]
fn lowest(counts: &[u16; UNLOADED as usize]) -> u8 {
    counts
        .iter()
        .position(|&c| c > 0)
        .map_or(UNLOADED, |level| level as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_follow_tickets() {
        let mut levels = TicketLevels::default();
        let mut changes = Vec::new();
        levels.add(ChunkPos::new(0, 0), 31, &mut changes);
        assert_eq!(changes.len(), 27 * 27);
        assert_eq!(levels.level(ChunkPos::new(2, -1)), 33);
        levels.add(ChunkPos::new(1, 0), 30, &mut changes);
        assert_eq!(levels.level(ChunkPos::new(2, -1)), 31);
        changes.clear();
        levels.remove(ChunkPos::new(1, 0), 30, &mut changes);
        assert_eq!(levels.level(ChunkPos::new(2, -1)), 33);
        levels.remove(ChunkPos::new(0, 0), 31, &mut changes);
        assert_eq!(levels.level(ChunkPos::new(0, 0)), UNLOADED);
        assert!(levels.counts.is_empty());
        assert_eq!(required_status(34), Some(ChunkStatus::InitializeLight));
        assert_eq!(required_status(45), None);
    }
}
