//! The players near each chunk for natural spawning: an index from every chunk within
//! [`RADIUS`] chunks of a player's chunk (`DistanceManager.hasPlayersNearby`) to those
//! players, updated when a player changes chunk, so spawning asks only the players around a
//! chunk instead of all of them.

use vanillite_base::hash::FxHashMap;
use vanillite_base::{BlockPos, ChunkPos, Vec3};
use vanillite_world::entity::EntityId;

/// Chunk radius of the natural spawn chunk counter (`NaturalSpawnChunkCounter`).
pub const RADIUS: i32 = 8;

/// Squared distance from a chunk's centre within which a player allows spawning there
/// (`playerIsCloseEnoughForSpawning`); every such player is in the chunk's entry.
pub const SPAWN_DISTANCE_SQR: f64 = 16384.0;

#[derive(Debug)]
struct Player {
    id: EntityId,
    pos: Vec3,
    chunk: ChunkPos,
    stamp: u32,
    live: bool,
}

/// The players of one world by chunk. Players are known by a slot that stays theirs while
/// they are in the index.
#[derive(Debug, Default)]
pub struct NearbyPlayers {
    slots: FxHashMap<EntityId, u32>,
    players: Vec<Player>,
    free: Vec<u32>,
    chunks: FxHashMap<ChunkPos, Vec<u32>>,
    stamp: u32,
}

fn within(a: ChunkPos, b: ChunkPos) -> bool {
    (a.x - b.x).abs() <= RADIUS && (a.z - b.z).abs() <= RADIUS
}

/// `ChunkMap.euclideanDistanceSquared`: from the chunk's centre to a position.
pub fn chunk_center_distance_sqr(chunk: ChunkPos, pos: Vec3) -> f64 {
    let x = f64::from(chunk.x) * 16.0 + 8.0;
    let z = f64::from(chunk.z) * 16.0 + 8.0;
    let dx = x - pos.x;
    let dz = z - pos.z;
    dx * dx + dz * dz
}

impl NearbyPlayers {
    /// Takes the tick's players (unique ids) and their positions: players that changed
    /// chunk move in the index, new ones are added and missing ones removed.
    pub fn update(&mut self, players: &[(EntityId, Vec3)]) {
        self.stamp = self.stamp.wrapping_add(1);
        let stamp = self.stamp;
        for &(id, pos) in players {
            let chunk = BlockPos::containing(pos.x, pos.y, pos.z).chunk();
            if let Some(&slot) = self.slots.get(&id) {
                let p = &mut self.players[slot as usize];
                let old = p.chunk;
                p.pos = pos;
                p.chunk = chunk;
                p.stamp = stamp;
                if old != chunk {
                    self.move_square(slot, Some(old), Some(chunk));
                }
                continue;
            }
            let player = Player {
                id,
                pos,
                chunk,
                stamp,
                live: true,
            };
            let slot = if let Some(slot) = self.free.pop() {
                self.players[slot as usize] = player;
                slot
            } else {
                self.players.push(player);
                u32::try_from(self.players.len() - 1).unwrap_or(u32::MAX)
            };
            self.slots.insert(id, slot);
            self.move_square(slot, None, Some(chunk));
        }
        if self.slots.len() > players.len() {
            for slot in 0..self.players.len() {
                let p = &mut self.players[slot];
                if !p.live || p.stamp == stamp {
                    continue;
                }
                p.live = false;
                let (id, chunk) = (p.id, p.chunk);
                let slot = u32::try_from(slot).unwrap_or(u32::MAX);
                self.slots.remove(&id);
                self.free.push(slot);
                self.move_square(slot, Some(chunk), None);
            }
        }
    }

    /// Moves a slot from the chunks around `old` to the chunks around `new`.
    fn move_square(&mut self, slot: u32, old: Option<ChunkPos>, new: Option<ChunkPos>) {
        if let Some(o) = old {
            for x in o.x - RADIUS..=o.x + RADIUS {
                for z in o.z - RADIUS..=o.z + RADIUS {
                    let key = ChunkPos::new(x, z);
                    if new.is_some_and(|n| within(key, n)) {
                        continue;
                    }
                    if let Some(list) = self.chunks.get_mut(&key) {
                        if let Some(i) = list.iter().position(|&s| s == slot) {
                            list.swap_remove(i);
                        }
                        if list.is_empty() {
                            self.chunks.remove(&key);
                        }
                    }
                }
            }
        }
        if let Some(n) = new {
            for x in n.x - RADIUS..=n.x + RADIUS {
                for z in n.z - RADIUS..=n.z + RADIUS {
                    let key = ChunkPos::new(x, z);
                    if old.is_some_and(|o| within(key, o)) {
                        continue;
                    }
                    self.chunks.entry(key).or_default().push(slot);
                }
            }
        }
    }

    /// The number of chunks within [`RADIUS`] of a player's chunk
    /// (`getNaturalSpawnChunkCount`).
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// The chunks within [`RADIUS`] of a player's chunk, in no particular order.
    pub fn chunks(&self) -> impl Iterator<Item = ChunkPos> + '_ {
        self.chunks.keys().copied()
    }

    /// One more than the highest slot in use: the length of per-slot arrays.
    pub fn slot_count(&self) -> usize {
        self.players.len()
    }

    /// The slots of the players whose chunk is within [`RADIUS`] of `chunk`, in no order.
    pub fn around(&self, chunk: ChunkPos) -> &[u32] {
        self.chunks.get(&chunk).map_or(&[], Vec::as_slice)
    }

    /// Whether a player is close enough to `chunk` for spawning
    /// (`anyPlayerCloseEnoughForSpawning`).
    pub fn any_close_for_spawning(&self, chunk: ChunkPos) -> bool {
        self.around(chunk).iter().any(|&s| {
            chunk_center_distance_sqr(chunk, self.players[s as usize].pos) < SPAWN_DISTANCE_SQR
        })
    }

    /// Appends the slots of the players close enough to `chunk` for spawning
    /// (`getPlayersCloseForSpawning`), in no order.
    pub fn close_for_spawning(&self, chunk: ChunkPos, out: &mut Vec<u32>) {
        out.extend(self.around(chunk).iter().copied().filter(|&s| {
            chunk_center_distance_sqr(chunk, self.players[s as usize].pos) < SPAWN_DISTANCE_SQR
        }));
    }

    /// The squared distance from `at` to the nearest player as natural spawning needs it:
    /// exact between `near` and 128², otherwise only some value at most `near` (a player
    /// that close rejects the position anyway) or above 128² (infinity when no player is
    /// within [`RADIUS`] chunks).
    pub fn nearest_distance_sqr(&self, at: Vec3, near: f64) -> f64 {
        let chunk = BlockPos::containing(at.x, at.y, at.z).chunk();
        let mut best = f64::INFINITY;
        for &s in self.around(chunk) {
            let d = self.players[s as usize].pos.distance_to_sqr(at);
            if d <= near {
                return d;
            }
            best = best.min(d);
        }
        best
    }
}

/// The players close enough to each chunk for spawning during one tick, computed once per
/// chunk (`LocalMobCapCalculator.playersNearChunk`).
#[derive(Debug, Default)]
pub struct CloseForSpawning {
    ranges: FxHashMap<ChunkPos, (u32, u32)>,
    slots: Vec<u32>,
}

impl CloseForSpawning {
    /// Forgets the last tick's lists.
    pub fn clear(&mut self) {
        self.ranges.clear();
        self.slots.clear();
    }

    /// The slots of the players close enough to `chunk` for spawning.
    pub fn get(&mut self, nearby: &NearbyPlayers, chunk: ChunkPos) -> &[u32] {
        let (start, end) = *self.ranges.entry(chunk).or_insert_with(|| {
            let start = u32::try_from(self.slots.len()).unwrap_or(u32::MAX);
            nearby.close_for_spawning(chunk, &mut self.slots);
            (start, u32::try_from(self.slots.len()).unwrap_or(u32::MAX))
        });
        &self.slots[start as usize..end as usize]
    }
}
