//! Simulation levels exactly like Vanilla's `SimulationChunkTracker`: the same propagation
//! (`DynamicGraphMinFixedPoint` over `ChunkTracker`'s 3x3 neighbourhood) and the same map
//! (`Long2ByteOpenHashMap`), so the chunks iterate in Vanilla's order.
//!
//! The order is part of the game: `ServerLevel.tickChunk` runs for every entity ticking chunk
//! in the iteration order of this map, and the random ticks of all chunks share one LCG
//! (`randValue`) and the level random. A different chunk order gives different random tick
//! positions; the parity scenarios compare them tick by tick.

use std::collections::VecDeque;

use vanillite_base::ChunkPos;
use vanillite_base::hash::FxHashMap;

/// Number of levels (`SimulationChunkTracker`: `super(34, ...)`).
const LEVEL_COUNT: u8 = 34;
/// Chunks at this level or above are not in the map (`SimulationChunkTracker.MAX_LEVEL`).
const UNTRACKED: u8 = 33;
/// `ChunkLevel.MAX_LEVEL + 1`: the source level of a chunk without simulation tickets.
const NO_TICKET: u8 = 45;
/// `ChunkPos.INVALID_CHUNK_POS`: the source node of `ChunkTracker`.
const SOURCE: i64 = pack(1_875_066, 1_875_066);
/// `DynamicGraphMinFixedPoint.NO_COMPUTED_LEVEL`.
const NO_COMPUTED: u8 = 255;

/// `ChunkPos.pack`.
const fn pack(x: i32, z: i32) -> i64 {
    (x as i64 & 0xFFFF_FFFF) | ((z as i64 & 0xFFFF_FFFF) << 32)
}

fn unpack(key: i64) -> ChunkPos {
    ChunkPos::new(key as i32, (key >> 32) as i32)
}

/// The simulation levels of one world.
#[derive(Debug)]
pub struct SimulationLevels {
    /// `SimulationChunkTracker.chunks`.
    chunks: OpenHashMap,
    /// Simulation tickets per chunk: level and how many tickets of that level (Vanilla keeps
    /// one ticket per type and level; equal tickets are counted here).
    tickets: FxHashMap<ChunkPos, Vec<(u8, u32)>>,
    /// `DynamicGraphMinFixedPoint.computedLevels`.
    computed: FxHashMap<i64, u8>,
    queue: LeveledQueue,
}

impl Default for SimulationLevels {
    fn default() -> Self {
        Self {
            chunks: OpenHashMap::new(16, 0.75),
            tickets: FxHashMap::default(),
            computed: FxHashMap::default(),
            queue: LeveledQueue::new(),
        }
    }
}

impl SimulationLevels {
    /// The simulation level of a chunk (33 if none).
    #[inline]
    pub fn level(&self, pos: ChunkPos) -> u8 {
        self.chunks.get(pack(pos.x, pos.z)).unwrap_or(UNTRACKED)
    }

    /// Every chunk below level 33 with its level, in Vanilla's iteration order.
    pub fn iter(&self) -> impl Iterator<Item = (ChunkPos, u8)> + '_ {
        self.chunks.iter().map(|(k, v)| (unpack(k), v))
    }

    /// `TicketStorage.addTicket` of a simulating ticket; run [`Self::run_updates`] after.
    pub fn add_ticket(&mut self, pos: ChunkPos, level: u8) {
        let tickets = self.tickets.entry(pos).or_default();
        let old = lowest(tickets);
        match tickets.iter_mut().find(|t| t.0 == level) {
            // The same ticket again: Vanilla only resets its timeout.
            Some(t) => t.1 += 1,
            None => {
                tickets.push((level, 1));
                if level < old {
                    self.update(pack(pos.x, pos.z), level, true);
                }
            }
        }
    }

    /// `TicketStorage.removeTicket` of a simulating ticket; run [`Self::run_updates`] after.
    pub fn remove_ticket(&mut self, pos: ChunkPos, level: u8) {
        let Some(tickets) = self.tickets.get_mut(&pos) else {
            return;
        };
        let Some(i) = tickets.iter().position(|t| t.0 == level) else {
            return;
        };
        tickets[i].1 -= 1;
        if tickets[i].1 > 0 {
            return;
        }
        tickets.swap_remove(i);
        let new = lowest(tickets);
        if tickets.is_empty() {
            self.tickets.remove(&pos);
        }
        self.update(pack(pos.x, pos.z), new, false);
    }

    /// `DynamicGraphMinFixedPoint.runUpdates(Integer.MAX_VALUE)`.
    pub fn run_updates(&mut self) {
        while let Some(node) = self.queue.remove_first() {
            let level = self.get_level(node).min(LEVEL_COUNT - 1);
            let computed = self.computed.remove(&node).unwrap_or(NO_COMPUTED);
            if computed < level {
                self.set_level(node, computed);
                self.check_neighbors_after_update(node, computed, true);
            } else if computed > level {
                self.set_level(node, LEVEL_COUNT - 1);
                if computed != LEVEL_COUNT - 1 {
                    self.queue
                        .enqueue(node, priority(LEVEL_COUNT - 1, computed));
                    self.computed.insert(node, computed);
                }
                self.check_neighbors_after_update(node, level, false);
            }
        }
    }

    /// `ChunkTracker.update`.
    fn update(&mut self, node: i64, level: u8, only_decreased: bool) {
        self.check_edge(SOURCE, node, level, only_decreased);
    }

    fn source_level(&self, node: i64) -> u8 {
        self.tickets
            .get(&unpack(node))
            .map_or(NO_TICKET, |t| lowest(t))
    }

    fn get_level(&self, node: i64) -> u8 {
        self.chunks.get(node).unwrap_or(UNTRACKED)
    }

    fn set_level(&mut self, node: i64, level: u8) {
        if level >= UNTRACKED {
            self.chunks.remove(node);
        } else {
            self.chunks.put(node, level);
        }
    }

    /// `checkEdge(from, to, newLevelFrom, onlyDecreased)`.
    fn check_edge(&mut self, from: i64, to: i64, level_from: u8, only_decreased: bool) {
        let level_to = self.get_level(to);
        let old_computed = self.computed.get(&to).copied().unwrap_or(NO_COMPUTED);
        self.check_edge_with(from, to, level_from, level_to, old_computed, only_decreased);
    }

    fn check_edge_with(
        &mut self,
        from: i64,
        to: i64,
        level_from: u8,
        level_to: u8,
        old_computed: u8,
        only_decreased: bool,
    ) {
        if to == SOURCE {
            return;
        }
        let level_from = level_from.min(LEVEL_COUNT - 1);
        let level_to = level_to.min(LEVEL_COUNT - 1);
        let was_consistent = old_computed == NO_COMPUTED;
        let old_computed = if was_consistent {
            level_to
        } else {
            old_computed
        };
        let new_computed = if only_decreased {
            old_computed.min(level_from)
        } else {
            self.computed_level(to, from, level_from)
                .min(LEVEL_COUNT - 1)
        };
        let old_priority = priority(level_to, old_computed);
        if level_to != new_computed {
            let new_priority = priority(level_to, new_computed);
            if old_priority != new_priority && !was_consistent {
                self.queue.dequeue(to, old_priority, new_priority);
            }
            self.queue.enqueue(to, new_priority);
            self.computed.insert(to, new_computed);
        } else if !was_consistent {
            self.queue.dequeue(to, old_priority, LEVEL_COUNT);
            self.computed.remove(&to);
        }
    }

    /// `checkNeighbor`.
    fn check_neighbor(&mut self, from: i64, to: i64, level: u8, only_decreased: bool) {
        let stored = self.computed.get(&to).copied().unwrap_or(NO_COMPUTED);
        let level_from = self
            .level_from_neighbor(from, to, level)
            .min(LEVEL_COUNT - 1);
        if only_decreased {
            let level_to = self.get_level(to);
            self.check_edge_with(from, to, level_from, level_to, stored, true);
        } else {
            let was_consistent = stored == NO_COMPUTED;
            let old_computed = if was_consistent {
                self.get_level(to).min(LEVEL_COUNT - 1)
            } else {
                stored
            };
            if level_from == old_computed {
                let level_to = if was_consistent {
                    old_computed
                } else {
                    self.get_level(to)
                };
                self.check_edge_with(from, to, LEVEL_COUNT - 1, level_to, stored, false);
            }
        }
    }

    /// `ChunkTracker.checkNeighborsAfterUpdate`.
    fn check_neighbors_after_update(&mut self, node: i64, level: u8, only_decrease: bool) {
        if only_decrease && level >= LEVEL_COUNT - 2 {
            return;
        }
        let pos = unpack(node);
        for dx in -1..=1 {
            for dz in -1..=1 {
                let neighbor = pack(pos.x + dx, pos.z + dz);
                if neighbor != node {
                    self.check_neighbor(node, neighbor, level, only_decrease);
                }
            }
        }
    }

    /// `ChunkTracker.getComputedLevel`.
    fn computed_level(&self, node: i64, known_parent: i64, known_level: u8) -> u8 {
        let mut computed = known_level;
        let pos = unpack(node);
        for dx in -1..=1 {
            for dz in -1..=1 {
                let mut neighbor = pack(pos.x + dx, pos.z + dz);
                if neighbor == node {
                    neighbor = SOURCE;
                }
                if neighbor != known_parent {
                    let level = self.get_level(neighbor);
                    let cost = self.level_from_neighbor(neighbor, node, level);
                    computed = computed.min(cost);
                    if computed == 0 {
                        return 0;
                    }
                }
            }
        }
        computed
    }

    /// `ChunkTracker.computeLevelFromNeighbor`.
    fn level_from_neighbor(&self, from: i64, to: i64, from_level: u8) -> u8 {
        if from == SOURCE {
            self.source_level(to)
        } else {
            from_level.saturating_add(1)
        }
    }
}

/// The lowest ticket level (`getTicketLevelAt`).
fn lowest(tickets: &[(u8, u32)]) -> u8 {
    tickets.iter().map(|t| t.0).min().unwrap_or(NO_TICKET)
}

/// `calculatePriority`.
fn priority(level: u8, computed: u8) -> u8 {
    level.min(computed).min(LEVEL_COUNT - 1)
}

/// `LeveledPriorityQueue`: one insertion-ordered set (`LongLinkedOpenHashSet`) per level.
#[derive(Debug)]
struct LeveledQueue {
    queues: Vec<LinkedSet>,
    first: u8,
}

impl LeveledQueue {
    fn new() -> Self {
        Self {
            queues: (0..LEVEL_COUNT).map(|_| LinkedSet::default()).collect(),
            first: LEVEL_COUNT,
        }
    }

    /// `removeFirstLong` of the lowest queued level; `None` if the queue is empty.
    fn remove_first(&mut self) -> Option<i64> {
        if self.first >= LEVEL_COUNT {
            return None;
        }
        let queue = &mut self.queues[usize::from(self.first)];
        let node = queue.remove_first();
        if queue.is_empty() {
            self.check_first(LEVEL_COUNT);
        }
        node
    }

    fn dequeue(&mut self, node: i64, key: u8, upper_bound: u8) {
        let queue = &mut self.queues[usize::from(key)];
        queue.remove(node);
        if queue.is_empty() && self.first == key {
            self.check_first(upper_bound);
        }
    }

    fn enqueue(&mut self, node: i64, key: u8) {
        self.queues[usize::from(key)].add(node);
        if self.first > key {
            self.first = key;
        }
    }

    fn check_first(&mut self, upper_bound: u8) {
        let old = self.first;
        self.first = upper_bound;
        for i in old + 1..upper_bound {
            if !self.queues[usize::from(i)].is_empty() {
                self.first = i;
                break;
            }
        }
    }
}

/// An insertion-ordered set of longs: adding a member keeps its place, removing and adding it
/// again moves it to the end. Removed entries stay in the deque until they reach its front.
#[derive(Debug, Default)]
struct LinkedSet {
    order: VecDeque<(i64, u64)>,
    live: FxHashMap<i64, u64>,
    next: u64,
}

impl LinkedSet {
    fn is_empty(&self) -> bool {
        self.live.is_empty()
    }

    fn add(&mut self, node: i64) {
        if self.live.contains_key(&node) {
            return;
        }
        self.next += 1;
        self.live.insert(node, self.next);
        self.order.push_back((node, self.next));
    }

    fn remove(&mut self, node: i64) {
        self.live.remove(&node);
        if self.live.is_empty() {
            self.order.clear();
        }
    }

    fn remove_first(&mut self) -> Option<i64> {
        while let Some((node, stamp)) = self.order.pop_front() {
            if self.live.get(&node) == Some(&stamp) {
                self.live.remove(&node);
                return Some(node);
            }
        }
        None
    }
}

/// fastutil's `Long2ByteOpenHashMap` (8.5): open addressing with linear probing, key 0 kept
/// apart, iteration from the last slot down with key 0 first.
#[derive(Debug)]
struct OpenHashMap {
    key: Vec<i64>,
    value: Vec<u8>,
    n: usize,
    mask: usize,
    min_n: usize,
    max_fill: usize,
    size: usize,
    contains_null: bool,
    f: f32,
}

/// `HashCommon.mix`.
fn mix(x: i64) -> i64 {
    let h = x.wrapping_mul(-7_046_029_254_386_353_131);
    let h = h ^ ((h as u64) >> 32) as i64;
    h ^ ((h as u64) >> 16) as i64
}

/// `HashCommon.arraySize`.
fn array_size(expected: usize, f: f32) -> usize {
    let needed = (expected as f64 / f64::from(f)).ceil() as u64;
    needed.max(2).next_power_of_two() as usize
}

/// `HashCommon.maxFill`.
fn max_fill(n: usize, f: f32) -> usize {
    ((n as f64 * f64::from(f)).ceil() as usize).min(n - 1)
}

impl OpenHashMap {
    fn new(expected: usize, f: f32) -> Self {
        let n = array_size(expected, f);
        Self {
            key: vec![0; n + 1],
            value: vec![0; n + 1],
            n,
            mask: n - 1,
            min_n: n,
            max_fill: max_fill(n, f),
            size: 0,
            contains_null: false,
            f,
        }
    }

    #[inline]
    fn slot(&self, k: i64) -> usize {
        (mix(k) as i32 as usize) & self.mask
    }

    /// `find`: `Ok(slot)` of the key or `Err(free slot)`.
    fn find(&self, k: i64) -> Result<usize, usize> {
        if k == 0 {
            return if self.contains_null {
                Ok(self.n)
            } else {
                Err(self.n)
            };
        }
        let mut pos = self.slot(k);
        loop {
            let curr = self.key[pos];
            if curr == 0 {
                return Err(pos);
            }
            if curr == k {
                return Ok(pos);
            }
            pos = (pos + 1) & self.mask;
        }
    }

    #[inline]
    fn get(&self, k: i64) -> Option<u8> {
        self.find(k).ok().map(|pos| self.value[pos])
    }

    fn put(&mut self, k: i64, v: u8) {
        match self.find(k) {
            Ok(pos) => self.value[pos] = v,
            Err(pos) => {
                if pos == self.n {
                    self.contains_null = true;
                }
                self.key[pos] = k;
                self.value[pos] = v;
                let full = self.size >= self.max_fill;
                self.size += 1;
                if full {
                    self.rehash(array_size(self.size + 1, self.f));
                }
            }
        }
    }

    fn remove(&mut self, k: i64) {
        let Ok(pos) = self.find(k) else {
            return;
        };
        self.size -= 1;
        if pos == self.n {
            self.contains_null = false;
        } else {
            self.shift_keys(pos);
        }
        if self.n > self.min_n && self.size < self.max_fill / 4 && self.n > 16 {
            self.rehash(self.n / 2);
        }
    }

    fn shift_keys(&mut self, mut pos: usize) {
        'outer: loop {
            let last = pos;
            pos = (pos + 1) & self.mask;
            loop {
                let curr = self.key[pos];
                if curr == 0 {
                    self.key[last] = 0;
                    return;
                }
                let slot = self.slot(curr);
                let moves = if last <= pos {
                    last >= slot || slot > pos
                } else {
                    last >= slot && slot > pos
                };
                if moves {
                    self.key[last] = curr;
                    self.value[last] = self.value[pos];
                    continue 'outer;
                }
                pos = (pos + 1) & self.mask;
            }
        }
    }

    fn rehash(&mut self, new_n: usize) {
        let mask = new_n - 1;
        let mut key = vec![0; new_n + 1];
        let mut value = vec![0; new_n + 1];
        let mut i = self.n;
        let real = self.size - usize::from(self.contains_null);
        for _ in 0..real {
            i -= 1;
            while self.key[i] == 0 {
                i -= 1;
            }
            let mut pos = (mix(self.key[i]) as i32 as usize) & mask;
            while key[pos] != 0 {
                pos = (pos + 1) & mask;
            }
            key[pos] = self.key[i];
            value[pos] = self.value[i];
        }
        value[new_n] = self.value[self.n];
        self.n = new_n;
        self.mask = mask;
        self.max_fill = max_fill(new_n, self.f);
        self.key = key;
        self.value = value;
    }

    /// `fastIterable().iterator()`: key 0 first, then the slots from the last down.
    fn iter(&self) -> impl Iterator<Item = (i64, u8)> + '_ {
        let null = self.contains_null.then(|| (0, self.value[self.n]));
        null.into_iter().chain(
            (0..self.n)
                .rev()
                .filter(|&i| self.key[i] != 0)
                .map(|i| (self.key[i], self.value[i])),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forced_ticket_levels() {
        let mut s = SimulationLevels::default();
        s.add_ticket(ChunkPos::new(0, 0), 31);
        s.run_updates();
        assert_eq!(s.level(ChunkPos::new(0, 0)), 31);
        assert_eq!(s.level(ChunkPos::new(1, -1)), 32);
        assert_eq!(s.level(ChunkPos::new(2, 0)), 33);
        assert_eq!(s.iter().count(), 9);
        s.add_ticket(ChunkPos::new(5, 5), 22);
        s.run_updates();
        assert_eq!(s.iter().count(), 21 * 21);
        s.remove_ticket(ChunkPos::new(5, 5), 22);
        s.run_updates();
        assert_eq!(s.iter().count(), 9);
        s.remove_ticket(ChunkPos::new(0, 0), 31);
        s.run_updates();
        assert_eq!(s.iter().count(), 0);
    }
}
