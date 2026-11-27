//! `/vanillite profile`: a phase profiler that runs only while a profile is taken (#250).
//!
//! Without a profile a world tick pays one relaxed atomic load and a branch per phase. While
//! one runs, each world times its tick phases and every ticked entity (by type and chunk)
//! into its own [`WorldProfile`]: nothing on the hot path is shared between worlds. At the
//! end the server takes the worlds' profiles between their ticks, writes a text report under
//! the server folder and sends a summary to whoever started the profile.

use std::fmt::Write as _;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use vanillite_base::ChunkPos;
use vanillite_base::hash::FxHashMap;
use vanillite_data::entity::EntityTypeId;

use crate::commands::CommandSource;

/// A phase of a world tick, in tick order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// The players' packets.
    NetworkIn,
    /// Plugin tasks and movement events (also the Paper/Spigot layer).
    Plugins,
    /// Player moves, actions, respawns and travel.
    Players,
    /// Data pack functions and schedules.
    Functions,
    /// Border, weather, time and the mob environment.
    WorldState,
    /// Scheduled block and fluid ticks (redstone, fluids, command blocks).
    BlockTicks,
    /// Raids.
    Raids,
    /// Chunk tickets, loading, unloading, light hand-off.
    Chunks,
    /// Natural spawning.
    Spawning,
    /// Random ticks, weather ticks of the chunks.
    RandomTicks,
    /// Block events (pistons, note blocks, chests).
    BlockEvents,
    /// Entity ticks.
    Entities,
    /// Block entity ticks (hoppers, furnaces, spawners).
    BlockEntities,
    /// The entity tracker and its packets.
    Tracking,
    /// Chunk packets.
    ChunkSending,
    /// The player ticks (sleep, survival, items).
    PlayerTicks,
    /// Connections, scoreboard and the packet flush.
    NetworkOut,
    /// Autosave rounds.
    Saving,
}

impl Phase {
    /// Every phase.
    pub(crate) const ALL: [Phase; 18] = [
        Phase::NetworkIn,
        Phase::Plugins,
        Phase::Players,
        Phase::Functions,
        Phase::WorldState,
        Phase::BlockTicks,
        Phase::Raids,
        Phase::Chunks,
        Phase::Spawning,
        Phase::RandomTicks,
        Phase::BlockEvents,
        Phase::Entities,
        Phase::BlockEntities,
        Phase::Tracking,
        Phase::ChunkSending,
        Phase::PlayerTicks,
        Phase::NetworkOut,
        Phase::Saving,
    ];

    fn index(self) -> usize {
        Phase::ALL.iter().position(|p| *p == self).unwrap_or(0)
    }

    /// The name in the report.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Phase::NetworkIn => "network in (packets)",
            Phase::Plugins => "plugins",
            Phase::Players => "player actions",
            Phase::Functions => "functions",
            Phase::WorldState => "world state (weather, time)",
            Phase::BlockTicks => "block ticks (redstone, fluids)",
            Phase::Raids => "raids",
            Phase::Chunks => "chunk system (load, light)",
            Phase::Spawning => "mob spawning",
            Phase::RandomTicks => "random ticks",
            Phase::BlockEvents => "block events (pistons)",
            Phase::Entities => "entities",
            Phase::BlockEntities => "block entities",
            Phase::Tracking => "entity tracking",
            Phase::ChunkSending => "chunk sending",
            Phase::PlayerTicks => "player ticks",
            Phase::NetworkOut => "network out (flush)",
            Phase::Saving => "saving",
        }
    }
}

/// The time of the ticked entities by type and by chunk.
#[derive(Debug, Default)]
pub(crate) struct EntityProfile {
    by_type: FxHashMap<EntityTypeId, (Duration, u64)>,
    by_chunk: FxHashMap<ChunkPos, (Duration, u64)>,
    current: Option<(EntityTypeId, ChunkPos, Instant)>,
}

impl EntityProfile {
    /// The next entity's tick starts; the one before ends.
    pub(crate) fn next(&mut self, kind: EntityTypeId, chunk: ChunkPos) {
        let now = Instant::now();
        self.close(now);
        self.current = Some((kind, chunk, now));
    }

    /// The last entity's tick ended.
    pub(crate) fn finish(&mut self) {
        self.close(Instant::now());
    }

    fn close(&mut self, now: Instant) {
        if let Some((kind, chunk, start)) = self.current.take() {
            let spent = now.saturating_duration_since(start);
            let t = self.by_type.entry(kind).or_default();
            t.0 += spent;
            t.1 += 1;
            let c = self.by_chunk.entry(chunk).or_default();
            c.0 += spent;
            c.1 += 1;
        }
    }
}

/// What a world measured during a profile.
#[derive(Debug)]
pub(crate) struct WorldProfile {
    phases: [Duration; Phase::ALL.len()],
    ticks: u64,
    last: Instant,
    /// Synchronous chunk loads (count, waited) when the profile started.
    pub(crate) sync_loads_at_start: (u64, Duration),
    /// Synchronous chunk loads during the profile.
    pub(crate) sync_loads: (u64, Duration),
    pub(crate) entities: EntityProfile,
}

impl WorldProfile {
    pub(crate) fn new(sync_loads: (u64, Duration)) -> Self {
        WorldProfile {
            phases: [Duration::ZERO; Phase::ALL.len()],
            ticks: 0,
            last: Instant::now(),
            sync_loads_at_start: sync_loads,
            sync_loads: (0, Duration::ZERO),
            entities: EntityProfile::default(),
        }
    }

    /// A world tick starts.
    pub(crate) fn begin_tick(&mut self) {
        self.ticks += 1;
        self.last = Instant::now();
    }

    /// The time since the last lap belongs to `phase`.
    pub(crate) fn lap(&mut self, phase: Phase) {
        let now = Instant::now();
        if let Some(slot) = self.phases.get_mut(phase.index()) {
            *slot += now.saturating_duration_since(self.last);
        }
        self.last = now;
    }

    #[cfg(test)]
    pub(crate) fn add(&mut self, phase: Phase, time: Duration) {
        if let Some(slot) = self.phases.get_mut(phase.index()) {
            *slot += time;
        }
    }
}

/// A profile being taken.
struct Session<S> {
    source: S,
    until: Instant,
    started: Instant,
    server: [Duration; 3],
    tick_time: Duration,
    ticks: u64,
}

/// What the server tick reports of itself while a profile runs.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ServerLaps {
    pub(crate) before: Duration,
    pub(crate) worlds: Duration,
    pub(crate) after: Duration,
    pub(crate) total: Duration,
}

/// A finished profile's server part, handed to the report with the worlds' parts.
pub(crate) struct Finished<S = CommandSource> {
    pub(crate) source: S,
    pub(crate) wall: Duration,
    pub(crate) ticks: u64,
    pub(crate) server: [Duration; 3],
    pub(crate) tick_time: Duration,
}

/// The profiler of the server (in [`Global`](crate::global::Global)).
pub(crate) struct Profiler<S = CommandSource> {
    active: AtomicBool,
    session: Mutex<Option<Session<S>>>,
    /// A finished profile waits for the worlds' parts.
    collecting: AtomicBool,
}

impl<S> std::fmt::Debug for Profiler<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Profiler")
            .field("active", &self.is_active())
            .finish_non_exhaustive()
    }
}

impl<S> Default for Profiler<S> {
    fn default() -> Self {
        Profiler {
            active: AtomicBool::new(false),
            session: Mutex::new(None),
            collecting: AtomicBool::new(false),
        }
    }
}

impl<S> Profiler<S> {
    /// Whether a profile runs: one relaxed load, what the ticks pay without one.
    #[inline]
    pub(crate) fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    /// Starts a profile of `seconds`; `false` when one runs or is still being collected.
    pub(crate) fn start(&self, source: S, seconds: u32) -> bool {
        let mut session = self
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if session.is_some() || self.collecting.load(Ordering::Acquire) {
            return false;
        }
        let now = Instant::now();
        *session = Some(Session {
            source,
            until: now + Duration::from_secs(u64::from(seconds)),
            started: now,
            server: [Duration::ZERO; 3],
            tick_time: Duration::ZERO,
            ticks: 0,
        });
        self.active.store(true, Ordering::Release);
        true
    }

    /// The server tick ended (only called while active): adds its laps; returns the finished
    /// profile once its time is up.
    pub(crate) fn server_tick(&self, laps: ServerLaps) -> Option<Finished<S>> {
        let mut guard = self
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = guard.as_mut()?;
        session.ticks += 1;
        session.tick_time += laps.total;
        for (slot, lap) in session
            .server
            .iter_mut()
            .zip([laps.before, laps.worlds, laps.after])
        {
            *slot += lap;
        }
        if Instant::now() < session.until {
            return None;
        }
        let session = guard.take()?;
        self.collecting.store(true, Ordering::Release);
        self.active.store(false, Ordering::Release);
        Some(Finished {
            source: session.source,
            wall: session.started.elapsed(),
            ticks: session.ticks,
            server: session.server,
            tick_time: session.tick_time,
        })
    }

    /// The report of the last profile is out; a new one may start.
    pub(crate) fn collected(&self) {
        self.collecting.store(false, Ordering::Release);
    }
}

/// One line of a report table: a name, the time and the count behind it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Row {
    pub(crate) name: String,
    pub(crate) millis_per_tick: f64,
    pub(crate) percent: f64,
    pub(crate) count: u64,
    /// `x z` of a chunk row, for the teleport click.
    pub(crate) chunk: Option<(String, ChunkPos)>,
}

/// A finished profile in numbers.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Report {
    pub(crate) seconds: f64,
    pub(crate) ticks: u64,
    pub(crate) mspt: f64,
    pub(crate) phases: Vec<Row>,
    pub(crate) entity_types: Vec<Row>,
    pub(crate) chunks: Vec<Row>,
    pub(crate) sync_loads: (u64, Duration),
}

/// Rows of the report's top lists.
const TOP: usize = 10;

/// Builds the report of a profile from the server's part and each world's
/// (dimension name, profile).
pub(crate) fn report<S>(finished: &Finished<S>, worlds: &[(String, WorldProfile)]) -> Report {
    let ticks = finished.ticks.max(1);
    let per_tick = |d: Duration| d.as_secs_f64() * 1000.0 / ticks as f64;
    let tick_ms = per_tick(finished.tick_time);
    let percent = |ms: f64| {
        if tick_ms > 0.0 {
            ms * 100.0 / tick_ms
        } else {
            0.0
        }
    };
    let row = |name: String, time: Duration, count: u64| {
        let ms = per_tick(time);
        Row {
            name,
            millis_per_tick: ms,
            percent: percent(ms),
            count,
            chunk: None,
        }
    };
    let mut phases = Vec::new();
    phases.push(row(
        "server: before worlds (console, global tasks, joins)".into(),
        finished.server[0],
        0,
    ));
    phases.push(row(
        "server: after worlds (hand-offs, saving)".into(),
        finished.server[2],
        0,
    ));
    let mut types: FxHashMap<EntityTypeId, (Duration, u64)> = FxHashMap::default();
    let mut chunks = Vec::new();
    let mut sync_loads = (0, Duration::ZERO);
    for (dimension, world) in worlds {
        let short = dimension.strip_prefix("minecraft:").unwrap_or(dimension);
        for phase in Phase::ALL {
            let time = world.phases.get(phase.index()).copied().unwrap_or_default();
            if time > Duration::ZERO {
                phases.push(row(
                    format!("{short}: {}", phase.label()),
                    time,
                    world.ticks,
                ));
            }
        }
        for (kind, (time, count)) in &world.entities.by_type {
            let t = types.entry(*kind).or_default();
            t.0 += *time;
            t.1 += *count;
        }
        for (pos, (time, count)) in &world.entities.by_chunk {
            let mut r = row(format!("{short} {} {}", pos.x, pos.z), *time, *count);
            r.chunk = Some((dimension.clone(), *pos));
            chunks.push(r);
        }
        sync_loads.0 += world.sync_loads.0;
        sync_loads.1 += world.sync_loads.1;
    }
    let by_time = |a: &Row, b: &Row| b.millis_per_tick.total_cmp(&a.millis_per_tick);
    phases.sort_by(by_time);
    let mut entity_types: Vec<Row> = types
        .into_iter()
        .map(|(kind, (time, count))| {
            let name = kind.name();
            row(
                name.strip_prefix("minecraft:").unwrap_or(name).to_owned(),
                time,
                count,
            )
        })
        .collect();
    entity_types.sort_by(by_time);
    entity_types.truncate(TOP);
    chunks.sort_by(by_time);
    chunks.truncate(TOP);
    Report {
        seconds: finished.wall.as_secs_f64(),
        ticks: finished.ticks,
        mspt: tick_ms,
        phases,
        entity_types,
        chunks,
        sync_loads,
    }
}

/// The report as text (the file under `profiles/`).
pub(crate) fn report_text(report: &Report, version: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Vanillite profile ({version})");
    let _ = writeln!(
        out,
        "{:.1} s, {} ticks, {:.2} ms per tick on average (TPS {:.2})",
        report.seconds,
        report.ticks,
        report.mspt,
        if report.seconds > 0.0 {
            report.ticks as f64 / report.seconds
        } else {
            0.0
        }
    );
    let _ = writeln!(
        out,
        "Synchronous chunk loads that blocked the tick: {} ({} ms)",
        report.sync_loads.0,
        report.sync_loads.1.as_millis()
    );
    for (title, rows) in [
        ("Tick phases", &report.phases),
        ("Entities by type", &report.entity_types),
        ("Chunks by entity time", &report.chunks),
    ] {
        let _ = writeln!(out, "\n{title}:");
        for r in rows {
            let _ = writeln!(
                out,
                "  {:>8.3} ms/tick {:>6.2} %  {:>9}  {}",
                r.millis_per_tick, r.percent, r.count, r.name
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finished(ticks: u64, tick_ms: u64) -> Finished<()> {
        Finished {
            source: (),
            wall: Duration::from_secs(1),
            ticks,
            server: [Duration::from_millis(ticks), Duration::ZERO, Duration::ZERO],
            tick_time: Duration::from_millis(ticks * tick_ms),
        }
    }

    #[test]
    fn report_sorts_phases_types_and_chunks_by_time() {
        let mut world = WorldProfile::new((0, Duration::ZERO));
        world.ticks = 20;
        world.add(Phase::Entities, Duration::from_millis(60));
        world.add(Phase::Chunks, Duration::from_millis(30));
        let zombie = EntityTypeId::by_name("minecraft:zombie").unwrap();
        let cow = EntityTypeId::by_name("minecraft:cow").unwrap();
        world
            .entities
            .by_type
            .insert(zombie, (Duration::from_millis(40), 100));
        world
            .entities
            .by_type
            .insert(cow, (Duration::from_millis(20), 40));
        world
            .entities
            .by_chunk
            .insert(ChunkPos::new(1, 2), (Duration::from_millis(50), 120));
        let report = report(
            &finished(20, 5),
            &[("minecraft:overworld".to_owned(), world)],
        );
        assert_eq!(report.ticks, 20);
        assert!((report.mspt - 5.0).abs() < 1e-9);
        let names: Vec<&str> = report.phases.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "overworld: entities",
                "overworld: chunk system (load, light)",
                "server: before worlds (console, global tasks, joins)",
                "server: after worlds (hand-offs, saving)",
            ]
        );
        let first = &report.phases[0];
        assert!((first.millis_per_tick - 3.0).abs() < 1e-9);
        assert!((first.percent - 60.0).abs() < 1e-9);
        assert_eq!(report.entity_types[0].name, "zombie");
        assert_eq!(report.entity_types[1].name, "cow");
        assert_eq!(report.chunks[0].name, "overworld 1 2");
        assert_eq!(
            report.chunks[0].chunk,
            Some(("minecraft:overworld".to_owned(), ChunkPos::new(1, 2)))
        );
        let text = report_text(&report, "Vanillite test");
        assert!(text.contains("20 ticks"), "{text}");
        assert!(text.contains("zombie"), "{text}");
    }

    #[test]
    fn entity_laps_go_to_the_entity_before() {
        let mut p = EntityProfile::default();
        let pig = EntityTypeId::by_name("minecraft:pig").unwrap();
        p.next(pig, ChunkPos::new(0, 0));
        p.next(pig, ChunkPos::new(0, 1));
        p.finish();
        assert_eq!(p.by_type.get(&pig).map(|t| t.1), Some(2));
        assert_eq!(p.by_chunk.len(), 2);
        assert!(p.current.is_none());
    }

    #[test]
    fn one_profile_at_a_time() {
        let profiler = Profiler::<()>::default();
        assert!(!profiler.is_active());
        assert!(profiler.start((), 0));
        assert!(profiler.is_active());
        assert!(!profiler.start((), 0));
        let laps = ServerLaps {
            before: Duration::from_millis(1),
            worlds: Duration::from_millis(2),
            after: Duration::ZERO,
            total: Duration::from_millis(3),
        };
        let done = profiler
            .server_tick(laps)
            .expect("zero seconds end at once");
        assert_eq!(done.ticks, 1);
        assert!(!profiler.is_active());
        assert!(!profiler.start((), 0), "still collecting");
        profiler.collected();
        assert!(profiler.start((), 0));
    }
}
