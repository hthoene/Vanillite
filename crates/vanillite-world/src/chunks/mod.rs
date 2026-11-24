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

pub mod levels;

use std::collections::BTreeSet;
use std::fmt;
use std::io;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};

use vanillite_base::hash::{FxHashMap, FxHashSet};
use vanillite_base::{BlockPos, ChunkPos};
use vanillite_chunk::slots::{ChunkGenerator, ChunkStorage, LightEngine, Neighbours, WorldInfo};
use vanillite_chunk::{BlockStateId, ChunkStatus, ProtoChunk};

pub use levels::{FULL_LEVEL, LevelChange, MAX_LEVEL, TicketLevels, required_status};

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
}

impl Default for ChunkSettings {
    fn default() -> Self {
        ChunkSettings {
            view_distance: 10,
            unload_delay: 100,
            max_jobs: 64,
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
    light_changes: FxHashSet<ChunkPos>,
    block_changes: Vec<BlockPos>,
    tick: u64,
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
            light_changes: FxHashSet::default(),
            block_changes: Vec::new(),
            tick: 0,
        }
    }

    /// What the slots know about this world.
    pub fn info(&self) -> &WorldInfo {
        &self.info
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

    /// The ticket level that keeps a player's view full: Vanilla's view (`ChunkTrackingView`)
    /// reaches one chunk past the view distance.
    pub fn player_ticket_level(&self) -> u8 {
        FULL_LEVEL - (self.settings.view_distance as u8 + 1)
    }

    /// The level of a chunk.
    pub fn level(&self, pos: ChunkPos) -> u8 {
        self.levels.level(pos)
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
                if change.new < change.old {
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
                        *Arc::make_mut(current).chunk.light_mut() = chunk.chunk.light().clone();
                        holder.dirty = true;
                        self.light_changes.insert(chunk.pos);
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
            self.light_changes.insert(pos);
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
                    Arc::make_mut(chunk).status = next;
                    holder.dirty = true;
                }
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
            let Some(holder) = self.holders.remove(&pos) else {
                continue;
            };
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
        for holder in self.holders.values_mut() {
            if holder.dirty
                && !holder.busy
                && let Some(chunk) = &holder.chunk
            {
                holder.dirty = false;
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

    /// Full chunks whose light changed since the last call (for light update packets).
    pub fn take_light_changes(&mut self, out: &mut Vec<ChunkPos>) {
        out.extend(self.light_changes.drain());
    }

    /// Starts light recomputation around changed chunks whose 5 × 5 area is loaded.
    fn start_relights(&mut self) {
        if self.relight.is_empty() {
            return;
        }
        let changed: Vec<ChunkPos> = self.relight.drain().collect();
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
