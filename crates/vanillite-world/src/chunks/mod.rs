//! The chunk runtime of one world (architecture 9.2): chunk map, tickets and levels, the chunk
//! scheduler, block access and saving. Sending chunks to players is the game's
//! (`vanillite_game::player::chunks`); players keep their chunks loaded with tickets
//! ([`Chunks::player_ticket_level`]).
//!
//! [`Chunks`] is owned by the world's thread. Everything slow runs as background jobs on the
//! [`Executor`] (the compute pool): loading and saving through the `chunk-storage` slot,
//! generation steps through `chunk-generator`, light through `light-engine`. Jobs get
//! snapshots (`Arc<ProtoChunk>`, sections are `Arc`s too) and send their results back over a
//! channel; [`Chunks::tick`] applies them on the world thread. A live (full) chunk is changed
//! only by the world thread, copy-on-write if a job still holds a snapshot.
//!
//! Scheduling follows Vanilla's generation pyramid: a chunk's ticket level decides the status
//! it must reach; a step runs once every chunk in its requirement radius has the required
//! status. Chunks waiting for a neighbour are parked on it and checked again only when it makes
//! progress. Steps with a block write radius (features) take their neighbours exclusively;
//! light results combine by maximum and need no exclusive access.
//!
//! Full chunks carry their scheduled block and fluid ticks in the world's [`LevelTicks`] while
//! they are live; they go back into the chunk when it is saved or unloaded. Simulation tickets
//! (Vanilla's `SimulationChunkTracker`) decide which chunks tick.

pub mod levels;

use std::collections::BTreeSet;
use std::fmt;
use std::io;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};

use vanillite_base::hash::{FxHashMap, FxHashSet};
use vanillite_base::{BlockPos, ChunkPos};
use vanillite_chunk::light::{ChunkLight, LightNibbles};
use vanillite_chunk::packet::SectionMask;
use vanillite_chunk::slots::{ChunkGenerator, ChunkStorage, LightEngine, Neighbours, WorldInfo};
use vanillite_chunk::{BlockStateId, ChunkStatus, ProtoChunk};

pub use levels::{
    BLOCK_TICKING_LEVEL, ENTITY_TICKING_LEVEL, FULL_LEVEL, LevelChange, MAX_LEVEL, TicketLevels,
    required_status,
};

use crate::ticks::{LevelTicks, ScheduledTick, TickPriority};

/// Runs background jobs; the server's compute pool, or inline in tests.
pub trait Executor: Send + Sync {
    /// Queues a job; lower `priority` runs first.
    fn spawn(&self, priority: u32, job: Box<dyn FnOnce() + Send>);
}

/// Runs every job at once on the calling thread (deterministic tests, `World::headless`).
#[derive(Debug, Default)]
pub struct InlineExecutor;

impl Executor for InlineExecutor {
    fn spawn(&self, _: u32, job: Box<dyn FnOnce() + Send>) {
        job();
    }
}

/// The slots one world's chunks run on.
#[derive(Clone)]
pub struct ChunkSlots {
    /// `chunk-storage`.
    pub storage: Arc<dyn ChunkStorage>,
    /// `chunk-generator`.
    pub generator: Arc<dyn ChunkGenerator>,
    /// `light-engine`.
    pub light: Arc<dyn LightEngine>,
    /// Where jobs run.
    pub executor: Arc<dyn Executor>,
}

impl fmt::Debug for ChunkSlots {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChunkSlots").finish_non_exhaustive()
    }
}

/// Tuning of the chunk runtime.
#[derive(Clone, Copy, Debug)]
pub struct ChunkSettings {
    /// Server view distance in chunks (2..=32): players load and see chunks this far.
    pub view_distance: i32,
    /// Ticks a chunk stays loaded after no ticket needs it any more.
    pub unload_delay: u64,
    /// Background jobs (loads, generation steps, light) in flight at once.
    pub max_jobs: usize,
    /// Simulation distance in chunks (2..=32): chunks this far from a player tick.
    pub simulation_distance: i32,
    /// Chunks whose light is recomputed per tick after block changes; the rest waits.
    pub max_relights: usize,
}

impl Default for ChunkSettings {
    fn default() -> Self {
        ChunkSettings {
            view_distance: 10,
            unload_delay: 100,
            max_jobs: 64,
            simulation_distance: 10,
            max_relights: 1024,
        }
    }
}

/// A ticket keeping chunks loaded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TicketId(u64);

/// One chunk slot of the map.
#[derive(Debug, Default)]
struct Holder {
    /// The chunk; `None` while it loads.
    chunk: Option<Arc<ProtoChunk>>,
    level: u8,
    /// A job writes this chunk (load, generation step).
    busy: bool,
    /// Changed since the last save.
    dirty: bool,
    /// Tick from which an unneeded chunk may be unloaded.
    unload_at: Option<u64>,
    /// Queued for a scheduling check.
    queued: bool,
}

impl Holder {
    fn status(&self) -> Option<ChunkStatus> {
        self.chunk.as_ref().map(|c| c.status)
    }
}

/// A finished background job.
enum Done {
    Loaded(ChunkPos, io::Result<Option<Box<ProtoChunk>>>),
    Stepped {
        status: ChunkStatus,
        targets: Vec<ProtoChunk>,
        written: Vec<Arc<ProtoChunk>>,
    },
    Lit {
        targets: Vec<ProtoChunk>,
        neighbours: Vec<Arc<ProtoChunk>>,
    },
    Relit {
        center: ChunkPos,
        chunks: Vec<Arc<ProtoChunk>>,
    },
    Saved(Vec<(Arc<ProtoChunk>, io::Result<()>)>),
}

/// What a scheduling check of one chunk found.
enum Check {
    /// Nothing more to do.
    Done,
    /// Waits for this chunk to make progress.
    Blocked(ChunkPos),
    /// A job started or a step ran inline.
    Progress,
    /// The job budget is used up.
    Full,
}

/// The chunk runtime of one world.
pub struct Chunks {
    info: WorldInfo,
    settings: ChunkSettings,
    slots: ChunkSlots,
    holders: FxHashMap<ChunkPos, Holder>,
    levels: TicketLevels,
    tickets: FxHashMap<TicketId, (ChunkPos, u8)>,
    next_ticket: u64,
    level_changes: Vec<LevelChange>,
    /// Chunks to check, most urgent (lowest level) first.
    check: BTreeSet<(u8, ChunkPos)>,
    /// Chunks parked on a neighbour.
    waiters: FxHashMap<ChunkPos, Vec<ChunkPos>>,
    in_flight: usize,
    done_tx: Sender<Done>,
    done_rx: Receiver<Done>,
    /// Saves in flight; a load of such a chunk takes this version.
    saving: FxHashMap<ChunkPos, Arc<ProtoChunk>>,
    unload_candidates: FxHashSet<ChunkPos>,
    relight: FxHashSet<ChunkPos>,
    /// Sections whose sky and block light changed, per full chunk.
    light_changes: FxHashMap<ChunkPos, LightChange>,
    block_changes: Vec<BlockPos>,
    tick: u64,
    /// Simulation tickets and their levels.
    simulation: TicketLevels,
    simulation_tickets: FxHashMap<TicketId, (ChunkPos, u8)>,
    /// Level changes of the simulation tickets (not used, kept to reuse the buffer).
    simulation_changes: Vec<LevelChange>,
    /// Scheduled block ticks of the live chunks.
    block_ticks: LevelTicks,
    /// Scheduled fluid ticks of the live chunks.
    fluid_ticks: LevelTicks,
    /// The world's game time, for scheduling and saving ticks.
    game_time: i64,
    /// The level's sub-tick counter (`nextSubTickCount`).
    sub_tick: i64,
    /// Live chunks with post-processing marks not handled yet.
    post_process: Vec<ChunkPos>,
    /// Chunks that passed the spawn step since the last [`Self::take_generated`].
    generated: Vec<ChunkPos>,
    /// Chunks that became live since the last [`Chunks::take_promoted`].
    promoted: Vec<ChunkPos>,
    /// Full chunks that unloaded since the last [`Self::take_unloaded`].
    unloaded: Vec<ChunkPos>,
}

impl fmt::Debug for Chunks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Chunks")
            .field("dimension", &self.info.dimension)
            .field("holders", &self.holders.len())
            .field("in_flight", &self.in_flight)
            .finish_non_exhaustive()
    }
}

impl Chunks {
    /// The chunk runtime of a world.
    pub fn new(info: WorldInfo, settings: ChunkSettings, slots: ChunkSlots) -> Self {
        let (done_tx, done_rx) = mpsc::channel();
        Chunks {
            info,
            settings: ChunkSettings {
                view_distance: settings.view_distance.clamp(2, 32),
                simulation_distance: settings.simulation_distance.clamp(2, 32),
                ..settings
            },
            slots,
            holders: FxHashMap::default(),
            levels: TicketLevels::default(),
            tickets: FxHashMap::default(),
            next_ticket: 0,
            level_changes: Vec::new(),
            check: BTreeSet::new(),
            waiters: FxHashMap::default(),
            in_flight: 0,
            done_tx,
            done_rx,
            saving: FxHashMap::default(),
            unload_candidates: FxHashSet::default(),
            relight: FxHashSet::default(),
            light_changes: FxHashMap::default(),
            block_changes: Vec::new(),
            tick: 0,
            simulation: TicketLevels::with_max(BLOCK_TICKING_LEVEL),
            simulation_tickets: FxHashMap::default(),
            simulation_changes: Vec::new(),
            block_ticks: LevelTicks::default(),
            fluid_ticks: LevelTicks::default(),
            game_time: 0,
            sub_tick: 0,
            post_process: Vec::new(),
            generated: Vec::new(),
            promoted: Vec::new(),
            unloaded: Vec::new(),
        }
    }

    /// What the slots know about this world.
    pub fn info(&self) -> &WorldInfo {
        &self.info
    }

    /// Chunks whose light is recomputed per tick (config reload).
    pub fn set_max_relights(&mut self, max: usize) {
        self.settings.max_relights = max;
    }

    /// The settings.
    pub fn settings(&self) -> &ChunkSettings {
        &self.settings
    }

    /// Number of chunks in the map (all statuses).
    pub fn loaded(&self) -> usize {
        self.holders.len()
    }

    // --- Tickets -------------------------------------------------------------------------

    /// Adds a ticket: chunks within `33 - level` of `pos` become full, the pyramid around
    /// them is generated (forced chunks: level 31, Vanilla's `ChunkLevel.byStatus(ENTITY_TICKING)`).
    pub fn add_ticket(&mut self, pos: ChunkPos, level: u8) -> TicketId {
        let id = TicketId(self.next_ticket);
        self.next_ticket += 1;
        self.tickets.insert(id, (pos, level));
        self.levels.add(pos, level, &mut self.level_changes);
        id
    }

    /// Removes a ticket.
    pub fn remove_ticket(&mut self, id: TicketId) {
        if let Some((pos, level)) = self.tickets.remove(&id) {
            self.levels.remove(pos, level, &mut self.level_changes);
        }
    }

    /// Moves a ticket to `pos` (a player changing chunk): only the chunks whose level changes
    /// are touched.
    pub fn move_ticket(&mut self, id: TicketId, pos: ChunkPos) {
        if let Some((from, level)) = self.tickets.get_mut(&id) {
            self.levels
                .shift(*from, pos, *level, &mut self.level_changes);
            *from = pos;
        }
    }

    /// The ticket level that keeps a player's view full: Vanilla's view (`ChunkTrackingView`)
    /// reaches one chunk past the view distance.
    pub fn player_ticket_level(&self) -> u8 {
        FULL_LEVEL - (self.settings.view_distance as u8 + 1)
    }

    /// The level of a chunk.
    pub fn level(&self, pos: ChunkPos) -> u8 {
        self.levels.level(pos)
    }

    /// Adds a simulation ticket: chunks within `31 - level` of `pos` tick entities and random
    /// ticks, one chunk further block ticks.
    pub fn add_simulation_ticket(&mut self, pos: ChunkPos, level: u8) -> TicketId {
        let id = TicketId(self.next_ticket);
        self.next_ticket += 1;
        self.simulation_tickets.insert(id, (pos, level));
        self.simulation
            .add(pos, level, &mut self.simulation_changes);
        self.simulation_changes.clear();
        id
    }

    /// Removes a simulation ticket.
    pub fn remove_simulation_ticket(&mut self, id: TicketId) {
        if let Some((pos, level)) = self.simulation_tickets.remove(&id) {
            self.simulation
                .remove(pos, level, &mut self.simulation_changes);
            self.simulation_changes.clear();
        }
    }

    /// Moves a simulation ticket to `pos`.
    pub fn move_simulation_ticket(&mut self, id: TicketId, pos: ChunkPos) {
        if let Some((from, level)) = self.simulation_tickets.get_mut(&id) {
            self.simulation
                .shift(*from, pos, *level, &mut self.simulation_changes);
            self.simulation_changes.clear();
            *from = pos;
        }
    }

    /// The simulation ticket level that lets a player's surroundings tick
    /// (`DistanceManager.getPlayerTicketLevel`).
    pub fn player_simulation_level(&self) -> u8 {
        (ENTITY_TICKING_LEVEL as i32 - self.settings.simulation_distance).max(0) as u8
    }

    /// Whether a chunk is full and loaded to block ticking (Vanilla's ticking chunk).
    fn is_ticking_chunk(&self, pos: ChunkPos) -> bool {
        self.holders.get(&pos).is_some_and(|h| {
            h.level <= BLOCK_TICKING_LEVEL
                && h.chunk
                    .as_ref()
                    .is_some_and(|c| c.status == ChunkStatus::Full)
        })
    }

    /// Whether scheduled ticks run in a chunk (`isPositionTickingWithEntitiesLoaded`).
    pub fn is_block_ticking(&self, pos: ChunkPos) -> bool {
        tick_check(&self.holders, &self.simulation, pos)
    }

    /// The chunks that get random ticks this tick (`forEachBlockTickingChunk`: entity ticking
    /// by simulation level and a ticking chunk).
    pub fn random_ticking_chunks(&self, out: &mut Vec<ChunkPos>) {
        out.extend(
            self.simulation
                .iter()
                .filter(|&(pos, level)| level <= ENTITY_TICKING_LEVEL && self.is_ticking_chunk(pos))
                .map(|(pos, _)| pos),
        );
    }

    fn apply_level_changes(&mut self) {
        let changes = std::mem::take(&mut self.level_changes);
        for change in &changes {
            let pos = change.pos;
            if change.new <= MAX_LEVEL {
                let holder = self.holders.entry(pos).or_default();
                if holder.queued {
                    self.check.remove(&(holder.level, pos));
                    self.check.insert((change.new, pos));
                }
                holder.level = change.new;
                holder.unload_at = None;
                self.unload_candidates.remove(&pos);
                // Only a new required status gives the chunk work: a full chunk moving
                // closer to a player (level 25 to 24) needs no check.
                if change.new < change.old
                    && required_status(change.new) != required_status(change.old)
                {
                    self.enqueue(pos);
                }
            } else if let Some(holder) = self.holders.get_mut(&pos) {
                if holder.queued {
                    self.check.remove(&(holder.level, pos));
                    holder.queued = false;
                }
                holder.level = change.new;
                holder.unload_at = Some(self.tick + self.settings.unload_delay);
                self.unload_candidates.insert(pos);
            }
        }
        self.level_changes = changes;
        self.level_changes.clear();
    }

    fn enqueue(&mut self, pos: ChunkPos) {
        if let Some(holder) = self.holders.get_mut(&pos)
            && !holder.queued
            && holder.level <= MAX_LEVEL
        {
            holder.queued = true;
            self.check.insert((holder.level, pos));
        }
    }

    /// Re-checks the chunks parked on `pos`.
    fn wake(&mut self, pos: ChunkPos) {
        if let Some(waiters) = self.waiters.remove(&pos) {
            for waiter in waiters {
                self.enqueue(waiter);
            }
        }
        self.enqueue(pos);
    }

    // --- Tick ----------------------------------------------------------------------------

    /// Applies finished jobs and ticket changes, starts new jobs and unloads unneeded chunks.
    /// With an inline executor this runs until nothing can make progress.
    pub fn tick(&mut self) {
        self.tick += 1;
        for _ in 0..1024 {
            let mut progress = self.drain_done();
            self.apply_level_changes();
            progress |= self.dispatch();
            if !progress {
                break;
            }
        }
        self.start_relights();
        self.unload();
    }

    fn drain_done(&mut self) -> bool {
        let mut any = false;
        while let Ok(done) = self.done_rx.try_recv() {
            any = true;
            self.apply_done(done);
        }
        any
    }

    fn apply_done(&mut self, done: Done) {
        match done {
            Done::Loaded(pos, result) => {
                self.in_flight -= 1;
                let mut chunk = match result {
                    Ok(Some(chunk)) => *chunk,
                    Ok(None) => self.slots.generator.new_chunk(pos),
                    Err(e) => {
                        log::error!(
                            "Couldn't load chunk {}, {} in {}: {e}; generating it again",
                            pos.x,
                            pos.z,
                            self.info.dimension
                        );
                        self.slots.generator.new_chunk(pos)
                    }
                };
                let mut dirty = chunk.status == ChunkStatus::Empty;
                // Stored light that is not trusted is computed again (Vanilla: `isLightOn`).
                if chunk.status > ChunkStatus::Features && !chunk.light_on {
                    chunk.status = ChunkStatus::Features;
                    dirty = true;
                }
                if let Some(holder) = self.holders.get_mut(&pos) {
                    holder.chunk = Some(Arc::new(chunk));
                    holder.busy = false;
                    holder.dirty |= dirty;
                    self.promote(pos);
                    self.wake(pos);
                }
            }
            Done::Stepped {
                status,
                targets,
                written,
            } => {
                self.in_flight -= 1;
                for mut target in targets {
                    target.status = status;
                    let pos = target.pos;
                    self.put_back(pos, Arc::new(target));
                    if let Some(holder) = self.holders.get_mut(&pos) {
                        holder.busy = false;
                    }
                    self.wake(pos);
                }
                for chunk in written {
                    let pos = chunk.pos;
                    self.put_back(pos, chunk);
                    if let Some(holder) = self.holders.get_mut(&pos) {
                        holder.busy = false;
                    }
                    self.wake(pos);
                }
            }
            Done::Lit {
                targets,
                neighbours,
            } => {
                self.in_flight -= 1;
                for target in targets {
                    let pos = target.pos;
                    if let Some(holder) = self.holders.get_mut(&pos)
                        && let Some(current) = holder.chunk.as_mut()
                    {
                        let current = Arc::make_mut(current);
                        current.chunk.light_mut().merge_max(target.chunk.light());
                        current.light_on = true;
                        current.status = ChunkStatus::Light;
                        holder.busy = false;
                        holder.dirty = true;
                    }
                    self.wake(pos);
                }
                for neighbour in neighbours {
                    self.merge_light(&neighbour);
                }
            }
            Done::Relit { center, chunks } => {
                self.in_flight -= 1;
                for chunk in chunks {
                    if chunk.pos.chessboard_distance(center) > 1 {
                        continue;
                    }
                    if let Some(holder) = self.holders.get_mut(&chunk.pos)
                        && let Some(current) = holder.chunk.as_mut()
                    {
                        let current = Arc::make_mut(current);
                        let change =
                            LightChange::between(current.chunk.light(), chunk.chunk.light());
                        *current.chunk.light_mut() = chunk.chunk.light().clone();
                        holder.dirty = true;
                        if !change.is_empty() && current.status == ChunkStatus::Full {
                            self.light_changes
                                .entry(chunk.pos)
                                .or_default()
                                .add(&change);
                        }
                    }
                }
            }
            Done::Saved(results) => {
                for (chunk, result) in results {
                    if let Err(e) = result {
                        log::error!(
                            "Couldn't save chunk {}, {} in {}: {e}",
                            chunk.pos.x,
                            chunk.pos.z,
                            self.info.dimension
                        );
                    }
                    if self
                        .saving
                        .get(&chunk.pos)
                        .is_some_and(|saving| Arc::ptr_eq(saving, &chunk))
                    {
                        self.saving.remove(&chunk.pos);
                    }
                }
            }
        }
    }

    /// Puts a chunk a job wrote back into its holder.
    fn put_back(&mut self, pos: ChunkPos, chunk: Arc<ProtoChunk>) {
        if let Some(holder) = self.holders.get_mut(&pos) {
            holder.chunk = Some(chunk);
            holder.dirty = true;
        }
    }

    fn merge_light(&mut self, from: &ProtoChunk) {
        let pos = from.pos;
        let Some(holder) = self.holders.get_mut(&pos) else {
            return;
        };
        let Some(current) = holder.chunk.as_mut() else {
            return;
        };
        Arc::make_mut(current)
            .chunk
            .light_mut()
            .merge_max(from.chunk.light());
        holder.dirty = true;
        if current.status == ChunkStatus::Full {
            self.light_changes.insert(pos, LightChange::ALL);
        }
    }

    fn dispatch(&mut self) -> bool {
        let mut progress = false;
        while let Some(&(level, pos)) = self.check.first() {
            if self.in_flight >= self.settings.max_jobs {
                break;
            }
            self.check.remove(&(level, pos));
            if let Some(holder) = self.holders.get_mut(&pos) {
                holder.queued = false;
            }
            match self.check_chunk(pos) {
                Check::Done => {}
                Check::Progress => progress = true,
                Check::Blocked(on) => self.waiters.entry(on).or_default().push(pos),
                Check::Full => {
                    self.enqueue(pos);
                    break;
                }
            }
        }
        progress
    }

    /// Advances a chunk by one step if it can.
    fn check_chunk(&mut self, pos: ChunkPos) -> Check {
        let Some(holder) = self.holders.get(&pos) else {
            return Check::Done;
        };
        let Some(required) = required_status(holder.level) else {
            return Check::Done;
        };
        if holder.busy {
            return Check::Blocked(pos);
        }
        let Some(chunk) = holder.chunk.as_ref() else {
            return self.start_load(pos);
        };
        if chunk.status >= required {
            return Check::Done;
        }
        let Some(next) = chunk.status.next() else {
            return Check::Done;
        };
        // Requirements by radius.
        let radius = next.requirement_radius() as i32;
        for dz in -radius..=radius {
            for dx in -radius..=radius {
                if dx == 0 && dz == 0 {
                    continue;
                }
                let distance = dx.unsigned_abs().max(dz.unsigned_abs());
                let Some(needed) = next.direct_requirement_at(distance) else {
                    continue;
                };
                let other = ChunkPos::new(pos.x + dx, pos.z + dz);
                match self.holders.get(&other).and_then(Holder::status) {
                    Some(status) if status >= needed => {}
                    _ => return Check::Blocked(other),
                }
            }
        }
        let write = next.write_radius() as i32;
        for dz in -write..=write {
            for dx in -write..=write {
                let other = ChunkPos::new(pos.x + dx, pos.z + dz);
                if self.holders.get(&other).is_some_and(|h| h.busy) {
                    return Check::Blocked(other);
                }
            }
        }
        self.run_step(pos, next, radius, write);
        Check::Progress
    }

    fn start_load(&mut self, pos: ChunkPos) -> Check {
        if self.in_flight >= self.settings.max_jobs {
            return Check::Full;
        }
        let Some(holder) = self.holders.get_mut(&pos) else {
            return Check::Done;
        };
        if let Some(saving) = self.saving.get(&pos) {
            // A pending save answers the load.
            holder.chunk = Some(Arc::clone(saving));
            holder.dirty = true;
            self.wake(pos);
            return Check::Progress;
        }
        holder.busy = true;
        self.in_flight += 1;
        let storage = Arc::clone(&self.slots.storage);
        let tx = self.done_tx.clone();
        self.slots.executor.spawn(
            u32::from(holder.level),
            Box::new(move || {
                let mut out = Vec::with_capacity(1);
                storage.load(&[pos], &mut out);
                let result = out.pop().unwrap_or(Ok(None));
                let _ = tx.send(Done::Loaded(pos, result.map(|c| c.map(Box::new))));
            }),
        );
        Check::Progress
    }

    fn run_step(&mut self, pos: ChunkPos, next: ChunkStatus, radius: i32, write: i32) {
        match next {
            ChunkStatus::Spawn | ChunkStatus::Full => {
                if let Some(holder) = self.holders.get_mut(&pos)
                    && let Some(chunk) = holder.chunk.as_mut()
                {
                    let chunk = Arc::make_mut(chunk);
                    chunk.status = next;
                    if next == ChunkStatus::Full {
                        chunk.generation = None;
                    }
                    holder.dirty = true;
                    if next == ChunkStatus::Spawn {
                        self.generated.push(pos);
                    }
                }
                self.promote(pos);
                self.wake(pos);
                return;
            }
            _ => {}
        }
        let Some(holder) = self.holders.get_mut(&pos) else {
            return;
        };
        let level = holder.level;
        // The holder keeps the current version for readers; the job works on a copy
        // (sections are shared until written).
        let Some(target) = holder.chunk.as_deref().cloned() else {
            return;
        };
        holder.busy = true;
        let mut neighbours = Vec::new();
        let mut written = Vec::new();
        for dz in -radius..=radius {
            for dx in -radius..=radius {
                if dx == 0 && dz == 0 {
                    continue;
                }
                let other = ChunkPos::new(pos.x + dx, pos.z + dz);
                let Some(holder) = self.holders.get_mut(&other) else {
                    continue;
                };
                let Some(chunk) = holder.chunk.as_ref() else {
                    continue;
                };
                if dx.abs() <= write && dz.abs() <= write {
                    holder.busy = true;
                    written.push(other);
                }
                neighbours.push(Arc::clone(chunk));
            }
        }
        self.in_flight += 1;
        let tx = self.done_tx.clone();
        let priority = u32::from(level);
        match next {
            ChunkStatus::InitializeLight => {
                let light = Arc::clone(&self.slots.light);
                self.slots.executor.spawn(
                    priority,
                    Box::new(move || {
                        let mut targets = vec![target];
                        light.initialize_light(&mut targets);
                        let _ = tx.send(Done::Stepped {
                            status: next,
                            targets,
                            written: Vec::new(),
                        });
                    }),
                );
            }
            ChunkStatus::Light => {
                let light = Arc::clone(&self.slots.light);
                self.slots.executor.spawn(
                    priority,
                    Box::new(move || {
                        let mut targets = vec![target];
                        let mut neighbours = neighbours;
                        light.light_chunks(&mut targets, &mut Neighbours::new(&mut neighbours));
                        let _ = tx.send(Done::Lit {
                            targets,
                            neighbours,
                        });
                    }),
                );
            }
            _ => {
                let generator = Arc::clone(&self.slots.generator);
                self.slots.executor.spawn(
                    priority,
                    Box::new(move || {
                        let mut targets = vec![target];
                        let mut neighbours = neighbours;
                        generator.generate(
                            next,
                            &mut targets,
                            &mut Neighbours::new(&mut neighbours),
                        );
                        let written = neighbours
                            .into_iter()
                            .filter(|n| written.contains(&n.pos))
                            .collect();
                        let _ = tx.send(Done::Stepped {
                            status: next,
                            targets,
                            written,
                        });
                    }),
                );
            }
        }
    }

    /// Makes a chunk that just became full live: its stored ticks move into the world's tick
    /// queues and its post-processing marks wait for its neighbours.
    fn promote(&mut self, pos: ChunkPos) {
        let Some(chunk) = self.holders.get_mut(&pos).and_then(|h| h.chunk.as_mut()) else {
            return;
        };
        if chunk.status != ChunkStatus::Full {
            return;
        }
        let chunk = Arc::make_mut(chunk);
        let blocks = std::mem::take(&mut chunk.block_ticks);
        let fluids = std::mem::take(&mut chunk.fluid_ticks);
        if !chunk.post_processing.is_empty() {
            self.post_process.push(pos);
        }
        self.promoted.push(pos);
        self.block_ticks.add_container(pos, &blocks, self.game_time);
        self.fluid_ticks.add_container(pos, &fluids, self.game_time);
    }

    /// Chunks that became live since the last call (the game loads their block entities).
    pub fn take_promoted(&mut self, out: &mut Vec<ChunkPos>) {
        out.append(&mut self.promoted);
    }

    /// Live chunks the next [`Chunks::tick`] unloads unless a ticket comes back meanwhile (the
    /// game writes their block entities back first).
    pub fn unloading_next_tick(&self, out: &mut Vec<ChunkPos>) {
        let next = self.tick + 1;
        out.extend(self.unload_candidates.iter().copied().filter(|pos| {
            self.holders.get(pos).is_some_and(|holder| {
                holder.level > MAX_LEVEL
                    && holder.unload_at.is_some_and(|at| at <= next)
                    && !holder.busy
                    && holder
                        .chunk
                        .as_ref()
                        .is_some_and(|c| c.status == ChunkStatus::Full)
            })
        }));
    }

    /// The stored block entities of a live chunk, for the game to write its live block
    /// entities back; the chunk counts as changed.
    pub fn block_entities_mut(
        &mut self,
        pos: ChunkPos,
    ) -> Option<&mut Vec<vanillite_chunk::slots::BlockEntityData>> {
        let holder = self.holders.get_mut(&pos)?;
        if holder.busy {
            return None;
        }
        let chunk = holder
            .chunk
            .as_mut()
            .filter(|c| c.status == ChunkStatus::Full)?;
        holder.dirty = true;
        Some(&mut Arc::make_mut(chunk).block_entities)
    }

    fn unload(&mut self) {
        let now = self.tick;
        let mut ready = Vec::new();
        for &pos in &self.unload_candidates {
            if let Some(holder) = self.holders.get(&pos)
                && holder.level > MAX_LEVEL
                && holder.unload_at.is_some_and(|at| at <= now)
                && !holder.busy
            {
                ready.push(pos);
            }
        }
        let mut save = Vec::new();
        for pos in ready {
            self.unload_candidates.remove(&pos);
            let Some(mut holder) = self.holders.remove(&pos) else {
                continue;
            };
            if let Some(chunk) = holder
                .chunk
                .as_mut()
                .filter(|c| c.status == ChunkStatus::Full)
            {
                self.unloaded.push(pos);
                let blocks = self.block_ticks.remove_container(pos, self.game_time);
                let fluids = self.fluid_ticks.remove_container(pos, self.game_time);
                if holder.dirty {
                    let chunk = Arc::make_mut(chunk);
                    chunk.block_ticks = blocks;
                    chunk.fluid_ticks = fluids;
                }
                self.post_process.retain(|&p| p != pos);
            }
            if let Some(waiters) = self.waiters.remove(&pos) {
                for waiter in waiters {
                    self.enqueue(waiter);
                }
            }
            if holder.dirty
                && let Some(chunk) = holder.chunk
            {
                save.push(chunk);
            }
        }
        self.save_chunks(save);
    }

    // --- Saving --------------------------------------------------------------------------

    fn save_chunks(&mut self, chunks: Vec<Arc<ProtoChunk>>) {
        if chunks.is_empty() {
            return;
        }
        for chunk in &chunks {
            self.saving.insert(chunk.pos, Arc::clone(chunk));
        }
        let storage = Arc::clone(&self.slots.storage);
        let tx = self.done_tx.clone();
        for batch in chunks.chunks(64) {
            let batch = batch.to_vec();
            let storage = Arc::clone(&storage);
            let tx = tx.clone();
            self.slots.executor.spawn(
                u32::from(MAX_LEVEL) + 1,
                Box::new(move || {
                    let mut out = Vec::with_capacity(batch.len());
                    storage.save(&batch, &mut out);
                    let _ = tx.send(Done::Saved(batch.into_iter().zip(out).collect()));
                }),
            );
        }
    }

    /// Saves every changed chunk. With `flush` it waits until everything is on disk
    /// (shutdown); otherwise the saves run in the background.
    pub fn save_all(&mut self, flush: bool) {
        let mut chunks = Vec::new();
        for (&pos, holder) in &mut self.holders {
            if holder.dirty
                && !holder.busy
                && let Some(chunk) = &mut holder.chunk
            {
                holder.dirty = false;
                if chunk.status == ChunkStatus::Full {
                    let chunk = Arc::make_mut(chunk);
                    chunk.block_ticks = self.block_ticks.pack(pos, self.game_time);
                    chunk.fluid_ticks = self.fluid_ticks.pack(pos, self.game_time);
                }
                chunks.push(Arc::clone(chunk));
            }
        }
        self.save_chunks(chunks);
        if flush {
            while !self.saving.is_empty() || self.in_flight > 0 {
                match self.done_rx.recv() {
                    Ok(done) => self.apply_done(done),
                    Err(_) => break,
                }
            }
            if let Err(e) = self.slots.storage.flush() {
                log::error!("Couldn't flush chunks of {}: {e}", self.info.dimension);
            }
        }
    }

    // --- Chunk and block access ----------------------------------------------------------

    /// A full chunk.
    #[inline]
    pub fn chunk(&self, pos: ChunkPos) -> Option<&Arc<ProtoChunk>> {
        self.holders
            .get(&pos)
            .and_then(|h| h.chunk.as_ref())
            .filter(|c| c.status == ChunkStatus::Full)
    }

    /// The block state at `pos`, `None` outside full chunks and the build height.
    pub fn block(&self, pos: BlockPos) -> Option<BlockStateId> {
        let chunk = self.chunk(pos.chunk())?;
        chunk
            .chunk
            .block((pos.x & 15) as u32, pos.y, (pos.z & 15) as u32)
    }

    /// Sets a block state in a full chunk like `LevelChunk.setBlockState` (heightmaps follow,
    /// light is recomputed after the tick). Returns the previous state, `None` if nothing
    /// changed or the chunk is not loaded. No neighbour or shape updates: those are gameplay.
    pub fn set_block(&mut self, pos: BlockPos, state: BlockStateId) -> Option<BlockStateId> {
        let chunk_pos = pos.chunk();
        let holder = self.holders.get_mut(&chunk_pos)?;
        if holder.busy {
            return None;
        }
        let chunk = holder.chunk.as_mut()?;
        if chunk.status != ChunkStatus::Full {
            return None;
        }
        let old = Arc::make_mut(chunk).chunk.set_block(
            (pos.x & 15) as u32,
            pos.y,
            (pos.z & 15) as u32,
            state,
        )?;
        holder.dirty = true;
        self.block_changes.push(pos);
        self.relight.insert(chunk_pos);
        Some(old)
    }

    /// Blocks changed since the last call, in change order (for block update packets).
    pub fn take_block_changes(&mut self, out: &mut Vec<BlockPos>) {
        out.append(&mut self.block_changes);
    }

    /// Live chunks whose post-processing marks can be handled now (their 3 × 3 area is full),
    /// with the marks; the marks are removed from the chunks.
    pub fn take_post_processing(&mut self, out: &mut Vec<(ChunkPos, Vec<BlockPos>)>) {
        let mut index = 0;
        while index < self.post_process.len() {
            let pos = self.post_process[index];
            let ready = (-1..=1).all(|dz| {
                (-1..=1).all(|dx| self.chunk(ChunkPos::new(pos.x + dx, pos.z + dz)).is_some())
            });
            if !ready {
                index += 1;
                continue;
            }
            self.post_process.swap_remove(index);
            if let Some(holder) = self.holders.get_mut(&pos)
                && let Some(chunk) = holder.chunk.as_mut()
            {
                let marks = std::mem::take(&mut Arc::make_mut(chunk).post_processing);
                holder.dirty = true;
                out.push((pos, marks));
            }
        }
    }

    // --- Scheduled ticks -----------------------------------------------------------------

    /// The game time ticks are scheduled and saved against.
    pub fn game_time(&self) -> i64 {
        self.game_time
    }

    /// Sets the game time (once per tick, by the world).
    pub fn set_game_time(&mut self, game_time: i64) {
        self.game_time = game_time;
    }

    fn new_tick(
        &mut self,
        pos: BlockPos,
        kind: u16,
        delay: i32,
        priority: TickPriority,
    ) -> ScheduledTick {
        let sub_tick = self.sub_tick;
        self.sub_tick += 1;
        ScheduledTick {
            kind,
            pos,
            trigger: self.game_time + i64::from(delay),
            priority,
            sub_tick,
        }
    }

    fn mark_dirty(&mut self, chunk: ChunkPos) {
        if let Some(holder) = self.holders.get_mut(&chunk) {
            holder.dirty = true;
        }
    }

    /// `scheduleTick(pos, block, delay, priority)`; `block` is a block id. Ignored outside live
    /// chunks or if the block already has a tick there. The sub-tick counter advances either
    /// way, as in Vanilla.
    pub fn schedule_block_tick(
        &mut self,
        pos: BlockPos,
        block: u16,
        delay: i32,
        priority: TickPriority,
    ) {
        let tick = self.new_tick(pos, block, delay, priority);
        if self.block_ticks.schedule(tick) {
            self.mark_dirty(pos.chunk());
        }
    }

    /// `scheduleTick(pos, fluid, delay, priority)`; `fluid` is a fluid id.
    pub fn schedule_fluid_tick(
        &mut self,
        pos: BlockPos,
        fluid: u16,
        delay: i32,
        priority: TickPriority,
    ) {
        let tick = self.new_tick(pos, fluid, delay, priority);
        if self.fluid_ticks.schedule(tick) {
            self.mark_dirty(pos.chunk());
        }
    }

    /// `getBlockTicks().hasScheduledTick(pos, block)`.
    pub fn has_block_tick(&self, pos: BlockPos, block: u16) -> bool {
        self.block_ticks.has_scheduled(pos, block)
    }

    /// `getFluidTicks().hasScheduledTick(pos, fluid)`.
    pub fn has_fluid_tick(&self, pos: BlockPos, fluid: u16) -> bool {
        self.fluid_ticks.has_scheduled(pos, fluid)
    }

    /// The scheduled block ticks (read only).
    pub fn block_ticks(&self) -> &LevelTicks {
        &self.block_ticks
    }

    /// The scheduled fluid ticks (read only).
    pub fn fluid_ticks(&self) -> &LevelTicks {
        &self.fluid_ticks
    }

    /// `getBlockTicks().willTickThisTick(pos, block)`.
    pub fn block_tick_runs_this_tick(&mut self, pos: BlockPos, block: u16) -> bool {
        self.block_ticks.will_tick_this_tick(pos, block)
    }

    /// Collects the block ticks due now (at most `max`) in Vanilla's order; run them with
    /// [`Self::next_block_tick`].
    pub fn collect_block_ticks(&mut self, max: usize) {
        let mut drained = Vec::new();
        let (holders, simulation) = (&self.holders, &self.simulation);
        let ticking = |pos: ChunkPos| tick_check(holders, simulation, pos);
        self.block_ticks
            .collect(self.game_time, max, ticking, |pos| drained.push(pos));
        for pos in drained {
            self.mark_dirty(pos);
        }
    }

    /// Collects the fluid ticks due now, like [`Self::collect_block_ticks`].
    pub fn collect_fluid_ticks(&mut self, max: usize) {
        let mut drained = Vec::new();
        let (holders, simulation) = (&self.holders, &self.simulation);
        let ticking = |pos: ChunkPos| tick_check(holders, simulation, pos);
        self.fluid_ticks
            .collect(self.game_time, max, ticking, |pos| drained.push(pos));
        for pos in drained {
            self.mark_dirty(pos);
        }
    }

    /// The next collected block tick: position and block id.
    pub fn next_block_tick(&mut self) -> Option<(BlockPos, u16)> {
        self.block_ticks.next_to_run()
    }

    /// The next collected fluid tick: position and fluid id.
    pub fn next_fluid_tick(&mut self) -> Option<(BlockPos, u16)> {
        self.fluid_ticks.next_to_run()
    }

    /// Scheduled block and fluid ticks of the live chunks.
    pub fn scheduled_ticks(&self) -> (usize, usize) {
        (self.block_ticks.count(), self.fluid_ticks.count())
    }

    /// Chunks that passed the spawn step (were generated, not loaded) since the last call, for
    /// the mobs a fresh chunk starts with (`spawnMobsForChunkGeneration`).
    pub fn take_generated(&mut self, out: &mut Vec<ChunkPos>) {
        out.append(&mut self.generated);
    }

    /// Full chunks that unloaded since the last call (their entities are saved now).
    pub fn take_unloaded(&mut self, out: &mut Vec<ChunkPos>) {
        out.append(&mut self.unloaded);
    }

    /// Full chunks whose light changed since the last call (for light update packets).
    pub fn take_light_changes(&mut self, out: &mut Vec<(ChunkPos, LightChange)>) {
        out.extend(self.light_changes.drain());
    }

    /// Starts light recomputation around changed chunks whose 5 × 5 area is loaded.
    fn start_relights(&mut self) {
        if self.relight.is_empty() {
            return;
        }
        let max = self.settings.max_relights.max(1);
        let changed: Vec<ChunkPos> = if self.relight.len() <= max {
            self.relight.drain().collect()
        } else {
            let some: Vec<ChunkPos> = self.relight.iter().copied().take(max).collect();
            for pos in &some {
                self.relight.remove(pos);
            }
            some
        };
        for center in changed {
            let mut area = Vec::with_capacity(25);
            for dz in -2..=2 {
                for dx in -2..=2 {
                    let pos = ChunkPos::new(center.x + dx, center.z + dz);
                    if let Some(chunk) = self.holders.get(&pos).and_then(|h| h.chunk.as_ref()) {
                        area.push(Arc::clone(chunk));
                    }
                }
            }
            self.in_flight += 1;
            let light = Arc::clone(&self.slots.light);
            let tx = self.done_tx.clone();
            self.slots.executor.spawn(
                0,
                Box::new(move || {
                    let mut area = area;
                    light.relight(&[center], &mut Neighbours::new(&mut area));
                    let _ = tx.send(Done::Relit {
                        center,
                        chunks: area,
                    });
                }),
            );
        }
    }
}

/// [`Chunks::is_block_ticking`] over the fields, for use while the tick queues are borrowed.
fn tick_check(
    holders: &FxHashMap<ChunkPos, Holder>,
    simulation: &TicketLevels,
    pos: ChunkPos,
) -> bool {
    simulation.level(pos) <= BLOCK_TICKING_LEVEL
        && holders.get(&pos).is_some_and(|h| {
            h.level <= BLOCK_TICKING_LEVEL
                && h.chunk
                    .as_ref()
                    .is_some_and(|c| c.status == ChunkStatus::Full)
        })
}

/// The light sections of a chunk that changed: bit `i` is light section `i` (one below the
/// lowest block section first).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LightChange {
    /// Changed sky light sections.
    pub sky: SectionMask,
    /// Changed block light sections.
    pub block: SectionMask,
}

impl LightChange {
    /// Every section changed.
    pub const ALL: LightChange = LightChange {
        sky: SectionMask([u64::MAX; 4]),
        block: SectionMask([u64::MAX; 4]),
    };

    /// The sections that differ between two light states of a chunk.
    pub fn between(old: &ChunkLight, new: &ChunkLight) -> LightChange {
        let diff = |a: &[LightNibbles], b: &[LightNibbles]| {
            let mut mask = SectionMask::default();
            for index in 0..a.len().max(b.len()).min(256) {
                if a.get(index) != b.get(index) {
                    mask.set(index);
                }
            }
            mask
        };
        LightChange {
            sky: diff(old.sky(), new.sky()),
            block: diff(old.block(), new.block()),
        }
    }

    /// No section changed.
    pub fn is_empty(&self) -> bool {
        self.sky.is_empty() && self.block.is_empty()
    }

    /// Adds the sections of `other`.
    pub fn add(&mut self, other: &LightChange) {
        for i in 0..4 {
            self.sky.0[i] |= other.sky.0[i];
            self.block.0[i] |= other.block.0[i];
        }
    }
}
