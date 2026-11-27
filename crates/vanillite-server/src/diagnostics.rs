//! Figures for Vanillite's own commands (#250), read only when a command asks: the process
//! (memory and CPU from `/proc`) and what each world holds (chunks, entities, block entities
//! and their memory). The ticks keep nothing for them beyond the tick history of
//! [`TickTimes`](crate::tick_rate::TickTimes) and the profiler's flag.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use vanillite_base::ChunkPos;
use vanillite_base::hash::FxHashMap;
use vanillite_data::entity::EntityTypeId;

use crate::profiler::Profiler;
use crate::world::ServerWorld;

/// Server-wide state of the diagnostics (in [`Global`](crate::global::Global)).
#[derive(Debug)]
pub(crate) struct Diagnostics {
    /// `/vanillite profile`.
    pub(crate) profiler: Profiler,
    /// When the server started.
    pub(crate) started: Instant,
    /// The CPU time of the process at the last `/vanillite status`.
    cpu: Mutex<Option<(Instant, f64)>>,
}

impl Diagnostics {
    pub(crate) fn new() -> Self {
        Diagnostics {
            profiler: Profiler::default(),
            started: Instant::now(),
            cpu: Mutex::new(None),
        }
    }

    /// The CPU use of the process in percent of one core since the last call (at most the
    /// last minute is not tracked: the first call covers the time since the start), and the
    /// time it covers.
    pub(crate) fn cpu_percent(&self, cpu_seconds: f64) -> (f64, Duration) {
        let now = Instant::now();
        let mut last = self
            .cpu
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (since, before) = last.unwrap_or((self.started, 0.0));
        *last = Some((now, cpu_seconds));
        let wall = now.saturating_duration_since(since);
        let percent = if wall.as_secs_f64() > 0.0 {
            (cpu_seconds - before).max(0.0) * 100.0 / wall.as_secs_f64()
        } else {
            0.0
        };
        (percent, wall)
    }
}

/// What `/proc/self` tells about the process.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct ProcessStats {
    /// Resident memory in bytes.
    pub(crate) rss: Option<u64>,
    /// Peak resident memory in bytes.
    pub(crate) peak: Option<u64>,
    /// Threads of the process.
    pub(crate) threads: Option<u64>,
    /// User and system CPU time in seconds.
    pub(crate) cpu_seconds: Option<f64>,
}

/// Reads the process figures (Linux; elsewhere all `None`).
pub(crate) fn process() -> ProcessStats {
    let mut stats = std::fs::read_to_string("/proc/self/status")
        .map(|s| parse_status(&s))
        .unwrap_or_default();
    let ticks = clock_ticks_per_second();
    stats.cpu_seconds = std::fs::read_to_string("/proc/self/stat")
        .ok()
        .and_then(|s| parse_stat_ticks(&s))
        .filter(|_| ticks > 0)
        .map(|t| t as f64 / ticks as f64);
    stats
}

/// `sysconf(_SC_CLK_TCK)`: the unit of the CPU times in `/proc/self/stat`.
#[cfg(unix)]
#[allow(clippy::useless_conversion)] // `c_long` is `i32` on 32-bit targets
fn clock_ticks_per_second() -> i64 {
    // SAFETY: `sysconf` only reads a configuration value.
    i64::from(unsafe { libc::sysconf(libc::_SC_CLK_TCK) })
}

#[cfg(not(unix))]
fn clock_ticks_per_second() -> i64 {
    0
}

/// `VmRSS`, `VmHWM` and `Threads` of `/proc/self/status`.
fn parse_status(text: &str) -> ProcessStats {
    let mut stats = ProcessStats::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let number = || value.split_whitespace().next()?.parse::<u64>().ok();
        match key {
            "VmRSS" => stats.rss = number().map(|kib| kib * 1024),
            "VmHWM" => stats.peak = number().map(|kib| kib * 1024),
            "Threads" => stats.threads = number(),
            _ => {}
        }
    }
    stats
}

/// `utime + stime` (clock ticks) of `/proc/self/stat`: fields 14 and 15, counted after the
/// command name in parentheses (which may hold spaces).
fn parse_stat_ticks(text: &str) -> Option<u64> {
    let rest = text.get(text.rfind(')')? + 1..)?;
    let mut fields = rest.split_whitespace().skip(11);
    let user = fields.next()?.parse::<u64>().ok()?;
    let system = fields.next()?.parse::<u64>().ok()?;
    Some(user + system)
}

/// What a command wants to know of each world.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Detail {
    /// Counts only.
    Counts,
    /// Counts and memory.
    Memory,
    /// Counts and the chunks with the most entities and block entities.
    Chunks,
    /// Counts, entities by type and the chunks with the most entities (of one type).
    Entities(Option<EntityTypeId>),
}

/// The memory a world's data takes, in bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct WorldMemory {
    pub(crate) chunks: usize,
    pub(crate) entities: usize,
    pub(crate) block_entities: usize,
}

/// One chunk of a top list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChunkCount {
    pub(crate) pos: ChunkPos,
    pub(crate) entities: usize,
    pub(crate) block_entities: usize,
    /// The most frequent entity types in the chunk.
    pub(crate) types: Vec<(EntityTypeId, usize)>,
}

/// What a world reports of itself.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct WorldSnapshot {
    pub(crate) dimension: String,
    pub(crate) chunks: usize,
    pub(crate) full_chunks: usize,
    pub(crate) entities: usize,
    pub(crate) block_entities: usize,
    pub(crate) ticking_block_entities: usize,
    pub(crate) players: usize,
    pub(crate) memory: Option<WorldMemory>,
    pub(crate) top_chunks: Vec<ChunkCount>,
    /// Entities by type, most first.
    pub(crate) types: Vec<(EntityTypeId, usize)>,
}

/// Entries of the top lists.
pub(crate) const TOP: usize = 10;

/// The figures of a world (on its thread, between its ticks).
pub(crate) fn snapshot(world: &ServerWorld, detail: Detail) -> WorldSnapshot {
    let level = world.level();
    let entities = &level.entities;
    let store = &entities.store;
    let mut snapshot = WorldSnapshot {
        dimension: world.dimension().to_owned(),
        chunks: level.chunks.loaded(),
        full_chunks: level.chunks.full(),
        entities: store.len(),
        block_entities: level.block_entities.len(),
        ticking_block_entities: level.block_entities.ticking(),
        players: entities.players.len(),
        ..WorldSnapshot::default()
    };
    let columns = store.columns();
    let kinds = columns.kind.iter().zip(&columns.pos);
    match detail {
        Detail::Counts => {}
        Detail::Memory => {
            snapshot.memory = Some(WorldMemory {
                chunks: level.chunks.memory_bytes(),
                entities: entity_bytes(entities),
                block_entities: level.block_entities.memory_bytes(),
            });
        }
        Detail::Chunks | Detail::Entities(_) => {
            let only = match detail {
                Detail::Entities(only) => only,
                _ => None,
            };
            let mut types: FxHashMap<EntityTypeId, usize> = FxHashMap::default();
            let mut chunks: FxHashMap<ChunkPos, (usize, usize)> = FxHashMap::default();
            for (kind, pos) in kinds.clone() {
                *types.entry(*kind).or_default() += 1;
                if only.is_none_or(|o| o == *kind) {
                    let chunk =
                        ChunkPos::new((pos.x.floor() as i32) >> 4, (pos.z.floor() as i32) >> 4);
                    chunks.entry(chunk).or_default().0 += 1;
                }
            }
            if detail == Detail::Chunks {
                for (chunk, count) in level.block_entities.per_chunk() {
                    chunks.entry(chunk).or_default().1 += count;
                }
            }
            snapshot.types = top(types.into_iter().collect(), usize::MAX);
            let mut list: Vec<(ChunkPos, (usize, usize))> = chunks.into_iter().collect();
            list.sort_by(|a, b| {
                (b.1.0 + b.1.1)
                    .cmp(&(a.1.0 + a.1.1))
                    .then(a.0.x.cmp(&b.0.x))
                    .then(a.0.z.cmp(&b.0.z))
            });
            list.truncate(TOP);
            snapshot.top_chunks = list
                .into_iter()
                .map(|(pos, (entities, block_entities))| {
                    let mut in_chunk: FxHashMap<EntityTypeId, usize> = FxHashMap::default();
                    for (kind, p) in kinds.clone() {
                        if (p.x.floor() as i32) >> 4 == pos.x && (p.z.floor() as i32) >> 4 == pos.z
                        {
                            *in_chunk.entry(*kind).or_default() += 1;
                        }
                    }
                    ChunkCount {
                        pos,
                        entities,
                        block_entities,
                        types: top(in_chunk.into_iter().collect(), 3),
                    }
                })
                .collect();
        }
    }
    snapshot
}

/// Sorted by count (most first, then by id), at most `n`.
fn top(mut list: Vec<(EntityTypeId, usize)>, n: usize) -> Vec<(EntityTypeId, usize)> {
    list.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    list.truncate(n);
    list
}

/// The bytes of the entities' columns and kind data (heap data behind them not counted).
fn entity_bytes(entities: &vanillite_game::entities::Entities) -> usize {
    fn vec<T>(v: &[T]) -> usize {
        std::mem::size_of_val(v)
    }
    let c = entities.store.columns();
    let columns = vec(&c.id)
        + vec(&c.network_id)
        + vec(&c.uuid)
        + vec(&c.kind)
        + vec(&c.kind_row)
        + vec(&c.pos)
        + vec(&c.old_pos)
        + vec(&c.velocity)
        + vec(&c.rot)
        + vec(&c.dims)
        + vec(&c.aabb)
        + vec(&c.flags)
        + vec(&c.pose)
        + vec(&c.section);
    fn dense<T>(d: &vanillite_world::entity::Dense<T>) -> usize {
        d.len() * size_of::<T>()
    }
    columns
        + dense(&entities.players)
        + dense(&entities.items)
        + dense(&entities.falling_blocks)
        + dense(&entities.mobs)
        + dense(&entities.arrows)
        + dense(&entities.primed_tnts)
        + dense(&entities.clouds)
        + dense(&entities.experience_orbs)
        + dense(&entities.specials)
}

/// Bytes as `12.3 MiB`.
pub(crate) fn bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS.get(unit).copied().unwrap_or("B"))
    }
}

/// A duration as `2d 3h 4m 5s` (leading zero units left out).
pub(crate) fn uptime(d: Duration) -> String {
    let s = d.as_secs();
    let (days, hours, minutes, seconds) = (s / 86_400, s / 3600 % 24, s / 60 % 60, s % 60);
    if days > 0 {
        format!("{days}d {hours}h {minutes}m {seconds}s")
    } else if hours > 0 {
        format!("{hours}h {minutes}m {seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_proc_status_and_stat() {
        let status = "Name:\tvanillite\nVmHWM:\t  2048 kB\nVmRSS:\t  1024 kB\nThreads:\t17\n";
        let s = parse_status(status);
        assert_eq!(s.rss, Some(1024 * 1024));
        assert_eq!(s.peak, Some(2048 * 1024));
        assert_eq!(s.threads, Some(17));
        let stat = "42 (my server) S 1 42 42 0 -1 4194560 100 0 0 0 250 50 0 0 20 0 17 0";
        assert_eq!(parse_stat_ticks(stat), Some(300));
        assert_eq!(parse_stat_ticks("garbage"), None);
        // The live process answers on Linux.
        if cfg!(target_os = "linux") {
            let p = process();
            assert!(p.rss.is_some_and(|r| r > 0));
            assert!(p.cpu_seconds.is_some());
        }
    }

    #[test]
    fn formats_bytes_and_uptime() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1536), "1.5 KiB");
        assert_eq!(bytes(3 * 1024 * 1024 * 1024), "3.0 GiB");
        assert_eq!(uptime(Duration::from_secs(59)), "59s");
        assert_eq!(uptime(Duration::from_secs(3 * 3600 + 61)), "3h 1m 1s");
        assert_eq!(uptime(Duration::from_secs(90_061)), "1d 1h 1m 1s");
    }

    #[test]
    fn cpu_percent_covers_the_time_since_the_last_call() {
        let d = Diagnostics::new();
        let (_, first) = d.cpu_percent(0.0);
        let (percent, second) = d.cpu_percent(0.0);
        assert!(second <= first + Duration::from_secs(1));
        assert!(percent.abs() < f64::EPSILON);
    }

    #[test]
    fn top_sorts_by_count_then_id() {
        let a = EntityTypeId(1);
        let b = EntityTypeId(2);
        let c = EntityTypeId(3);
        assert_eq!(top(vec![(c, 1), (b, 5), (a, 5)], 2), vec![(a, 5), (b, 5)]);
    }
}
