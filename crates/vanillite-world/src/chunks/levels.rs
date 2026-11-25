//! Tickets and chunk levels (Vanilla's `DistanceManager` and `ChunkLevel`).
//!
//! A ticket of level `L` at a chunk gives every chunk at Chebyshev distance `d` the level
//! `L + d`; a chunk's level is the lowest one any ticket gives it. Levels up to 33 mean a full
//! chunk, 34 to 44 a generation status (the pyramid around full chunks), above 44 the chunk is
//! not needed. Tickets reach only up to [`MAX_TICKET_LEVEL`]: what the game reads (full chunks
//! and the ring around them that light needs). Levels 35 to 44 come only from generation
//! tickets the scheduler places at chunks that really must be generated (#47 D3), so already
//! generated terrain keeps no pyramid in memory. Each chunk keeps a histogram of the levels its tickets give it, so adding or
//! removing a ticket costs one update per chunk in its reach and never rescans other tickets.

use vanillite_base::ChunkPos;
use vanillite_base::hash::FxHashMap;
use vanillite_chunk::ChunkStatus;
use vanillite_chunk::slots::RADIUS_AROUND_FULL;

/// Level of a full chunk (`ChunkLevel.FULL_CHUNK_LEVEL`).
pub const FULL_LEVEL: u8 = 33;
/// Highest level of a loaded chunk (`ChunkLevel.MAX_LEVEL`).
pub const MAX_LEVEL: u8 = FULL_LEVEL + RADIUS_AROUND_FULL as u8;
/// Highest level a ticket gives (`FULL_LEVEL + 1`): full chunks and the ring of chunks around
/// them that light reads. Everything above exists only to generate missing chunks.
pub const MAX_TICKET_LEVEL: u8 = FULL_LEVEL + 1;
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
    counts: FxHashMap<ChunkPos, Counts>,
    /// Levels above this are not tracked (they read as [`UNLOADED`]).
    max: u8,
}

/// How many tickets give a chunk each level, with a bit per level that has any.
#[derive(Debug)]
struct Counts {
    present: u64,
    counts: [u16; UNLOADED as usize],
}

impl Counts {
    #[inline]
    fn lowest(&self) -> u8 {
        if self.present == 0 {
            UNLOADED
        } else {
            self.present.trailing_zeros() as u8
        }
    }
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
            .map(|(&pos, counts)| (pos, counts.lowest()))
    }

    /// The level of a chunk.
    #[inline]
    pub fn level(&self, pos: ChunkPos) -> u8 {
        self.counts.get(&pos).map_or(UNLOADED, Counts::lowest)
    }

    /// Adds a ticket of `level` at `pos`.
    pub fn add(&mut self, pos: ChunkPos, level: u8, changes: &mut Vec<LevelChange>) {
        self.apply(None, Some(pos), level, changes);
    }

    /// Removes a ticket of `level` at `pos` that was added before.
    pub fn remove(&mut self, pos: ChunkPos, level: u8, changes: &mut Vec<LevelChange>) {
        self.apply(Some(pos), None, level, changes);
    }

    /// Moves a ticket of `level` that was added before from `from` to `to`: the same result
    /// as removing and adding it, but only chunks whose given level changes are touched (for
    /// a move by one chunk the leaving and entering edges and the half of the square that
    /// gets closer or further).
    pub fn shift(
        &mut self,
        from: ChunkPos,
        to: ChunkPos,
        level: u8,
        changes: &mut Vec<LevelChange>,
    ) {
        self.apply(Some(from), Some(to), level, changes);
    }

    fn apply(
        &mut self,
        from: Option<ChunkPos>,
        to: Option<ChunkPos>,
        level: u8,
        changes: &mut Vec<LevelChange>,
    ) {
        if level > self.max || from == to {
            return;
        }
        let reach = i32::from(self.max - level);
        if let (Some(a), Some(b)) = (from, to)
            && (a.x.abs_diff(b.x) > 2 * reach as u32 || a.z.abs_diff(b.z) > 2 * reach as u32)
        {
            // The squares do not overlap: sweeping their bounding box would only waste time.
            self.sweep(Some(a), None, level, reach, changes);
            self.sweep(None, Some(b), level, reach, changes);
        } else {
            self.sweep(from, to, level, reach, changes);
        }
    }

    /// Takes the ticket's contribution at `from` away and gives it at `to`, chunk by chunk
    /// over the bounding box of both squares.
    fn sweep(
        &mut self,
        from: Option<ChunkPos>,
        to: Option<ChunkPos>,
        level: u8,
        reach: i32,
        changes: &mut Vec<LevelChange>,
    ) {
        let (lo, hi) = match (from, to) {
            (Some(a), Some(b)) => ((a.x.min(b.x), a.z.min(b.z)), (a.x.max(b.x), a.z.max(b.z))),
            (Some(c), None) | (None, Some(c)) => ((c.x, c.z), (c.x, c.z)),
            (None, None) => return,
        };
        let given = |center: Option<ChunkPos>, x: i32, z: i32| match center {
            Some(c) => {
                let d = c.x.abs_diff(x).max(c.z.abs_diff(z));
                if d > reach as u32 {
                    UNLOADED
                } else {
                    level + d as u8
                }
            }
            None => UNLOADED,
        };
        for z in lo.1 - reach..=hi.1 + reach {
            for x in lo.0 - reach..=hi.0 + reach {
                let old = given(from, x, z);
                let new = given(to, x, z);
                if old != new {
                    self.update(ChunkPos::new(x, z), old, new, changes);
                }
            }
        }
    }

    /// One ticket gives `pos` the level `new` instead of `old` ([`UNLOADED`]: none).
    #[inline]
    fn update(&mut self, pos: ChunkPos, old: u8, new: u8, changes: &mut Vec<LevelChange>) {
        let counts = if new == UNLOADED {
            match self.counts.get_mut(&pos) {
                Some(counts) => counts,
                None => return,
            }
        } else {
            self.counts.entry(pos).or_insert_with(|| Counts {
                present: 0,
                counts: [0; UNLOADED as usize],
            })
        };
        let before = counts.lowest();
        if old != UNLOADED {
            let count = &mut counts.counts[usize::from(old)];
            *count = count.saturating_sub(1);
            if *count == 0 {
                counts.present &= !(1 << old);
            }
        }
        if new != UNLOADED {
            let count = &mut counts.counts[usize::from(new)];
            *count = count.saturating_add(1);
            counts.present |= 1 << new;
        }
        let after = counts.lowest();
        if after == UNLOADED {
            self.counts.remove(&pos);
        }
        if after != before {
            changes.push(LevelChange {
                pos,
                old: before,
                new: after,
            });
        }
    }
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

    /// Random adds, removes and moves (by one chunk, a few chunks and far) give the same
    /// levels as computing them from scratch, and the reported changes are exactly the
    /// chunks whose level changed.
    #[test]
    fn moves_equal_full_recompute() {
        let mut seed = 0x9E37_79B9_7F4A_7C15_u64;
        let mut next = move |bound: u32| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % u64::from(bound)) as i32
        };
        for max in [MAX_LEVEL, BLOCK_TICKING_LEVEL] {
            let mut levels = TicketLevels::with_max(max);
            let mut tickets: Vec<(ChunkPos, u8)> = Vec::new();
            let mut changes = Vec::new();
            for _ in 0..600 {
                let before = expected(&tickets, max);
                changes.clear();
                match next(4) {
                    0 | 1 if !tickets.is_empty() => {
                        let i = next(tickets.len() as u32) as usize;
                        let (from, level) = tickets[i];
                        let step = [1, 1, 1, 3, 60][next(5) as usize];
                        let to = ChunkPos::new(
                            from.x + next(2 * step + 1) - step as i32,
                            from.z + next(2 * step + 1) - step as i32,
                        );
                        levels.shift(from, to, level, &mut changes);
                        tickets[i].0 = to;
                    }
                    2 if !tickets.is_empty() => {
                        let (pos, level) = tickets.swap_remove(next(tickets.len() as u32) as usize);
                        levels.remove(pos, level, &mut changes);
                    }
                    _ => {
                        let pos = ChunkPos::new(next(40) - 20, next(40) - 20);
                        let level = 18 + next(30) as u8;
                        levels.add(pos, level, &mut changes);
                        tickets.push((pos, level));
                    }
                }
                let after = expected(&tickets, max);
                let mut want: Vec<LevelChange> = before
                    .keys()
                    .chain(after.keys())
                    .filter_map(|&pos| {
                        let old = before.get(&pos).copied().unwrap_or(UNLOADED);
                        let new = after.get(&pos).copied().unwrap_or(UNLOADED);
                        (old != new).then_some(LevelChange { pos, old, new })
                    })
                    .collect();
                want.sort_by_key(|c| (c.pos.x, c.pos.z));
                want.dedup();
                changes.sort_by_key(|c| (c.pos.x, c.pos.z));
                assert_eq!(changes, want);
                let mut got: Vec<(ChunkPos, u8)> = levels.iter().collect();
                got.sort_by_key(|&(pos, _)| (pos.x, pos.z));
                let mut all: Vec<(ChunkPos, u8)> = after.into_iter().collect();
                all.sort_by_key(|&(pos, _)| (pos.x, pos.z));
                assert_eq!(got, all);
            }
        }
    }

    /// Levels from scratch: the lowest `level + distance` of any ticket, up to `max`.
    fn expected(tickets: &[(ChunkPos, u8)], max: u8) -> FxHashMap<ChunkPos, u8> {
        let mut out = FxHashMap::default();
        for &(c, level) in tickets {
            if level > max {
                continue;
            }
            let reach = i32::from(max - level);
            for z in c.z - reach..=c.z + reach {
                for x in c.x - reach..=c.x + reach {
                    let given = level + c.x.abs_diff(x).max(c.z.abs_diff(z)) as u8;
                    let entry = out.entry(ChunkPos::new(x, z)).or_insert(UNLOADED);
                    *entry = (*entry).min(given);
                }
            }
        }
        out
    }
}
