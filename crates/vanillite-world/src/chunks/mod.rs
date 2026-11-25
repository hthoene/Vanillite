//! The chunk runtime of one world (architecture 9.2): chunk map, tickets and levels, the chunk
//! scheduler, block access and saving. Sending chunks to players is the game's
//! (`vanillite_game::player::chunks`); players keep their chunks loaded with tickets
//! ([`Chunks::player_ticket_level`]).
//!
//! [`Chunks`] is owned by the world's thread. Everything slow runs as background jobs on the
//! [`Executor`] (the compute pool): decoding and encoding chunk records with the world's
//! [`ChunkCodec`], generation steps through `chunk-generator`, light through `light-engine`.
//! Jobs get snapshots (`Arc<ProtoChunk>`, sections are `Arc`s too) and send their results back
//! over a channel; [`Chunks::tick`] applies them on the world thread. A live (full) chunk is
//! changed only by the world thread, copy-on-write if a job still holds a snapshot.
//!
//! Records are read and written through the asynchronous `chunk-storage` slot
//! ([`ChunkStore`], #47): requests carry a token, results arrive in this world's inbox. The
//! store answers a load of a chunk whose save is still queued from the pending save
//! (read-your-writes); only the short window while a snapshot is encoded is covered here.
//!
//! Saving runs in rounds ([`Chunks::start_save_round`]): the changed chunks are captured after
//! the world tick from the tick's remaining time ([`Chunks::autosave_step`]), at least
//! `min_chunks_per_tick` per tick, encoded on the pool in jobs of about 1 ms, and committed once
//! all are submitted. Chunks that unload are saved at once and committed within the commit
//! interval.
//!
//! Scheduling follows Vanilla's generation pyramid: a chunk's level decides the status it must
//! reach; a step runs once every chunk in its requirement radius has the required status.
//! Tickets reach only the levels the game reads ([`MAX_TICKET_LEVEL`]); a chunk with a ticket
//! that must still be generated gets a generation ticket, whose levels up to [`MAX_LEVEL`] keep
//! the chunks its steps need. Those are loaded only once a step asks for them, so already
//! generated terrain keeps no pyramid in memory (#47 D3). Chunks waiting for a neighbour are parked on it and checked again only when it makes
//! progress. Steps with a block write radius (features) take their neighbours exclusively;
//! light results combine by maximum and need no exclusive access.
//!
//! Between ticks the owner may call [`Chunks::pump`] when the runtime's waker
//! ([`Chunks::set_waker`]) reports finished jobs: it applies them and starts the next steps right
//! away instead of at the next tick, so the workers never wait for the tick. Everything that
//! changes the live world (chunks becoming full, tickets, unloading) still happens in
//! [`Chunks::tick`].
//!
//! Full chunks carry their scheduled block and fluid ticks in the world's [`LevelTicks`] while
//! they are live; they go back into the chunk when it is saved or unloaded. Simulation tickets
//! (Vanilla's `SimulationChunkTracker`) decide which chunks tick.

pub mod levels;
pub mod simulation;

use std::cell::Cell;
use std::collections::BTreeSet;
use std::fmt;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use vanillite_base::hash::{FxHashMap, FxHashSet};
use vanillite_base::{BlockPos, ChunkPos};
use vanillite_chunk::light::{ChunkLight, LightNibbles};
use vanillite_chunk::packet::SectionMask;
use vanillite_chunk::slots::{
    ChunkCodec, ChunkGenerator, ChunkStore, Completion, CompletionTarget, DimensionId, LightEngine,
    Neighbours, Priority, RecordKey, RecordMeta, StoreOp, StoreResult, Token, WorldInfo,
};
use vanillite_chunk::{BlockStateId, ChunkStatus, ProtoChunk};

pub use levels::{
    BLOCK_TICKING_LEVEL, ENTITY_TICKING_LEVEL, FULL_LEVEL, LevelChange, MAX_LEVEL,
    MAX_TICKET_LEVEL, TicketLevels, required_status,
};

use simulation::SimulationLevels;

use crate::ticks::{LevelTicks, ScheduledTick, TickPriority};

/// Most focus points a world orders its chunk work by ([`Chunks::set_focus`]).
pub const MAX_FOCUS: usize = 16;

/// An empty slot of the chunk cache (`ChunkPos.INVALID_CHUNK_POS`).
const NO_CHUNK: ChunkPos = ChunkPos::new(i32::MAX, i32::MAX);

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

/// Called from a worker when a job of the runtime finished and no wake is pending; the owner
/// then calls [`Chunks::pump`] on its thread. Must not block.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// The pending-wake flag and the owner's waker, shared with the jobs.
#[derive(Default)]
struct WakeState {
    pending: AtomicBool,
    waker: OnceLock<Waker>,
}

/// The jobs' end of the result channel: sends and wakes the owner once per pump.
#[derive(Clone)]
struct DoneSender {
    tx: Sender<Done>,
    wake: Arc<WakeState>,
}

impl WakeState {
    /// Wakes the owner unless a wake is already pending.
    fn wake(&self) {
        if let Some(waker) = self.waker.get()
            && !self.pending.swap(true, Ordering::AcqRel)
        {
            waker();
        }
    }
}

impl DoneSender {
    fn send(&self, done: Done) {
        // A dropped runtime does not want results any more.
        if self.tx.send(done).is_ok() {
            self.wake.wake();
        }
    }
}

/// The slots one world's chunks run on.
#[derive(Clone)]
pub struct ChunkSlots {
    /// `chunk-storage`: the record store of this dimension.
    pub store: Arc<dyn ChunkStore>,
    /// Turns this world's records into chunks and back.
    pub codec: Arc<dyn ChunkCodec>,
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
    /// Saving.
    pub save: SaveSettings,
}

/// Tuning of saving (`storage.*`).
#[derive(Clone, Copy, Debug)]
pub struct SaveSettings {
    /// This world's dimension in record keys.
    pub dimension: DimensionId,
    /// Most time of one tick spent capturing chunks of a save round; only the tick's remaining
    /// time is used (`storage.autosave.tick-budget`).
    pub tick_budget: Duration,
    /// Chunks captured per tick even when the tick has no time left, so a round ends
    /// (`storage.autosave.min-chunks-per-tick`).
    pub min_chunks_per_tick: usize,
    /// Ticks after which saves of unloaded chunks are committed (`storage.commit-interval`).
    pub commit_interval: u64,
    /// Bytes of saves waiting for IO above which no more saves are queued: chunks stay loaded
    /// and rounds pause (`storage.max-pending-writes`).
    pub max_backlog: usize,
}

impl Default for SaveSettings {
    fn default() -> Self {
        SaveSettings {
            dimension: DimensionId(0),
            tick_budget: Duration::from_micros(500),
            min_chunks_per_tick: 16,
            commit_interval: 30 * 20,
            max_backlog: 64 << 20,
        }
    }
}

/// Runs once a save round is committed.
pub type SavedCallback = Box<dyn FnOnce() + Send>;

impl Default for ChunkSettings {
    fn default() -> Self {
        ChunkSettings {
            view_distance: 10,
            unload_delay: 100,
            max_jobs: 64,
            simulation_distance: 10,
            max_relights: 1024,
            save: SaveSettings::default(),
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
    /// Blocks (everything but entities) changed since the last save; set through
    /// [`Holder::touch`].
    dirty: bool,
    /// Entities changed since the last save; set through [`Holder::touch_entities`].
    entities_dirty: bool,
    /// The chunk has an entities record in the store (an empty list deletes it).
    entities_stored: bool,
    /// The POI changed since the last save; set through [`Holder::touch_poi`].
    poi_dirty: bool,
    /// The load request while the chunk is read.
    loading: Option<Token>,
    /// Tick from which an unneeded chunk may be unloaded.
    unload_at: Option<u64>,
    /// Queued for a scheduling check.
    queued: bool,
    /// The order within its level it was queued with ([`Chunks::set_focus`]).
    rank: u8,
    /// Set while a queued generation step of this chunk is not needed any more (its level left
    /// [`MAX_LEVEL`]); the step is then skipped when it starts.
    unneeded: Option<Arc<AtomicBool>>,
    /// The step whose missing neighbours were last added as dependencies.
    planned: Option<ChunkStatus>,
    /// The chunk's stored entities are live (`ChunkLoadStatus.LOADED`); set by the entity
    /// storage at the end of the level tick, required for scheduled ticks.
    entities_loaded: bool,
}

impl Holder {
    fn status(&self) -> Option<ChunkStatus> {
        self.chunk.as_ref().map(|c| c.status)
    }

    /// Marks the chunk changed; the first change since the last save queues it for the next
    /// save round.
    fn touch(&mut self, pos: ChunkPos, changed: &mut Vec<ChunkPos>) {
        if !self.dirty {
            if !self.entities_dirty && !self.poi_dirty {
                changed.push(pos);
            }
            self.dirty = true;
        }
    }

    /// Marks the chunk's entities changed, like [`Holder::touch`].
    fn touch_entities(&mut self, pos: ChunkPos, changed: &mut Vec<ChunkPos>) {
        if !self.entities_dirty {
            if !self.dirty && !self.poi_dirty {
                changed.push(pos);
            }
            self.entities_dirty = true;
        }
    }

    /// Marks the chunk's POI changed, like [`Holder::touch`].
    fn touch_poi(&mut self, pos: ChunkPos, changed: &mut Vec<ChunkPos>) {
        if !self.poi_dirty {
            if !self.dirty && !self.entities_dirty {
                changed.push(pos);
            }
            self.poi_dirty = true;
        }
    }

    /// Whether anything is unsaved.
    fn unsaved(&self) -> bool {
        self.dirty || self.entities_dirty || self.poi_dirty
    }
}

/// A chunk captured for saving and the records to write.
#[derive(Clone)]
struct Snapshot {
    chunk: Arc<ProtoChunk>,
    /// Write the chunk record.
    blocks: bool,
    /// Write the entities record (delete it if the list is empty).
    entities: bool,
    /// Write the POI record (delete it if there is none).
    poi: bool,
}

/// A load waiting for the store: the chunk and the entities record.
#[derive(Default)]
struct PendingLoad {
    pos: ChunkPos,
    chunk: Option<StoreResult>,
    entities: Option<StoreResult>,
    poi: Option<StoreResult>,
}

/// The stored entities and POI of a loaded chunk.
type LoadedRecords = (
    io::Result<Option<Vec<Arc<[u8]>>>>,
    io::Result<Option<Arc<[u8]>>>,
);

/// A chunk read between ticks with its stored entities, waiting for the next tick.
type LoadedChunk = (ChunkPos, Box<ProtoChunk>, LoadedRecords);

/// A finished background job.
enum Done {
    /// A chunk was read (`None`: never saved) with its stored entities and POI (`None`: no
    /// record).
    Loaded(ChunkPos, io::Result<Option<Box<ProtoChunk>>>, LoadedRecords),
    Stepped {
        status: ChunkStatus,
        targets: Vec<ProtoChunk>,
        written: Vec<Arc<ProtoChunk>>,
    },
    Lit {
        targets: Vec<ProtoChunk>,
        neighbours: Vec<Arc<ProtoChunk>>,
    },
    /// A generation step that was not needed any more when it started: nothing changed.
    Skipped {
        target: ChunkPos,
        written: Vec<ChunkPos>,
    },
    Relit {
        center: ChunkPos,
        chunks: Vec<Arc<ProtoChunk>>,
    },
    /// An encode job submitted its records.
    Encoded {
        /// Captured for the save round.
        round: bool,
        chunks: Vec<Snapshot>,
        errors: Vec<(ChunkPos, io::Error)>,
    },
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
    /// Levels of the tickets, up to [`MAX_TICKET_LEVEL`].
    levels: TicketLevels,
    tickets: FxHashMap<TicketId, (ChunkPos, u8)>,
    /// Levels of the generation tickets, up to [`MAX_LEVEL`]; a chunk's level is the lower of
    /// both.
    generation: TicketLevels,
    /// The generation ticket of each chunk with a ticket that is being generated.
    generation_tickets: FxHashMap<ChunkPos, u8>,
    next_ticket: u64,
    level_changes: Vec<LevelChange>,
    /// Chunks to check, most urgent (lowest level) first, then by rank (nearest to a focus).
    check: BTreeSet<(u8, u8, ChunkPos)>,
    /// Points the players' views move towards ([`Chunks::set_focus`]).
    focus: Vec<ChunkPos>,
    /// Chunks parked on a neighbour.
    waiters: FxHashMap<ChunkPos, Vec<ChunkPos>>,
    in_flight: usize,
    done_tx: DoneSender,
    done_rx: Receiver<Done>,
    /// This world's inbox for store results.
    inbox_tx: CompletionTarget,
    inbox: Receiver<Completion>,
    next_token: u64,
    /// Loads waiting for the store, by token.
    loads: FxHashMap<Token, PendingLoad>,
    /// Snapshots being encoded; a load of such a chunk takes this version. Once submitted,
    /// the store answers loads from the pending write.
    encoding: FxHashMap<ChunkPos, Arc<ProtoChunk>>,
    /// Encode jobs whose records are not submitted yet.
    encode_jobs: usize,
    /// Encode jobs of the save round whose records are not submitted yet.
    round_jobs: usize,
    /// Chunks changed since they were last captured, in change order.
    changed: Vec<ChunkPos>,
    /// The running save round.
    round: Option<SaveRound>,
    /// Number of the last commit started.
    commits: u64,
    /// Callbacks waiting for a commit, by commit number.
    commit_waiters: Vec<(u64, SavedCallback)>,
    /// A commit is running.
    committing: bool,
    /// Records were submitted since the last commit started.
    uncommitted: bool,
    /// Tick of the last commit.
    last_commit: u64,
    /// Inside [`Chunks::pump`]: nothing becomes full and no loaded full chunk goes live.
    between_ticks: bool,
    /// Loaded full chunks that arrived between ticks; they go live in the next tick.
    loaded_between_ticks: Vec<LoadedChunk>,
    /// Chunks whose next step (spawn, full) waits for the next tick.
    due_next_tick: FxHashSet<ChunkPos>,
    unload_candidates: FxHashSet<ChunkPos>,
    relight: FxHashSet<ChunkPos>,
    /// Centres of relights in flight; a centre changed again waits in `relight` until its
    /// running relight is applied, so results of one centre never overtake each other.
    /// Relights do not count against the job budget of loading and generation.
    relighting: FxHashSet<ChunkPos>,
    /// Sections whose sky and block light changed, per full chunk.
    light_changes: FxHashMap<ChunkPos, LightChange>,
    block_changes: Vec<BlockPos>,
    tick: u64,
    /// Simulation tickets and their levels.
    simulation: SimulationLevels,
    simulation_tickets: FxHashMap<TicketId, (ChunkPos, u8)>,
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
    /// `ServerLevel.noSave` (`/save-off`): unneeded chunks are not unloaded.
    no_save: bool,
    /// The visibility of every chunk that is not hidden.
    visibility: FxHashMap<ChunkPos, Visibility>,
    /// Chunks to check for a visibility change at the end of the tick.
    visibility_check: Vec<ChunkPos>,
    /// Visibility changes since the last [`Chunks::take_visibility_changes`].
    visibility_changes: Vec<VisibilityChange>,
    /// The `unknown` tickets of synchronous loads ([`Chunks::load_now`]) with their ticks
    /// left (`TicketType.UNKNOWN`: level 33, timeout 1).
    unknown: FxHashMap<ChunkPos, (TicketId, Cell<i64>)>,
    /// `ServerChunkCache.lastChunkPos`: the world thread's cache of the last four accessible
    /// chunks it read, newest first. A read that hits it does not reset an `unknown` ticket.
    /// Only kept while `unknown` tickets exist: a new one clears it.
    chunk_cache: Cell<[ChunkPos; 4]>,
}

/// What the entities of a chunk do (Vanilla's `Visibility`, from the chunk's full status).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visibility {
    /// Not accessible (level above full, or not loaded): its entities are stored.
    Hidden,
    /// Full or block ticking: its entities are loaded but do not tick.
    Tracked,
    /// Entity ticking: its entities tick.
    Ticking,
}

/// A chunk's visibility changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VisibilityChange {
    /// The chunk.
    pub pos: ChunkPos,
    /// Before.
    pub old: Visibility,
    /// Now.
    pub new: Visibility,
}

/// A save round: chunks changed before it started, captured from the ticks' remaining time.
#[derive(Default)]
struct SaveRound {
    pending: std::collections::VecDeque<ChunkPos>,
    waiters: Vec<SavedCallback>,
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
        let wake = Arc::<WakeState>::default();
        // Store results wake the owner like job results, batched by the same pending flag.
        let inbox_wake = Arc::clone(&wake);
        let (inbox_tx, inbox) = CompletionTarget::with_waker(Arc::new(move || inbox_wake.wake()));
        Chunks {
            info,
            settings: ChunkSettings {
                view_distance: settings.view_distance.clamp(2, 32),
                simulation_distance: settings.simulation_distance.clamp(2, 32),
                ..settings
            },
            slots,
            holders: FxHashMap::default(),
            levels: TicketLevels::with_max(MAX_TICKET_LEVEL),
            tickets: FxHashMap::default(),
            generation: TicketLevels::default(),
            generation_tickets: FxHashMap::default(),
            next_ticket: 0,
            level_changes: Vec::new(),
            check: BTreeSet::new(),
            focus: Vec::new(),
            waiters: FxHashMap::default(),
            in_flight: 0,
            done_tx: DoneSender { tx: done_tx, wake },
            done_rx,
            inbox_tx,
            inbox,
            next_token: 0,
            loads: FxHashMap::default(),
            encoding: FxHashMap::default(),
            encode_jobs: 0,
            round_jobs: 0,
            changed: Vec::new(),
            round: None,
            commits: 0,
            commit_waiters: Vec::new(),
            committing: false,
            uncommitted: false,
            last_commit: 0,
            between_ticks: false,
            loaded_between_ticks: Vec::new(),
            due_next_tick: FxHashSet::default(),
            unload_candidates: FxHashSet::default(),
            relight: FxHashSet::default(),
            relighting: FxHashSet::default(),
            light_changes: FxHashMap::default(),
            block_changes: Vec::new(),
            tick: 0,
            simulation: SimulationLevels::default(),
            simulation_tickets: FxHashMap::default(),
            block_ticks: LevelTicks::default(),
            fluid_ticks: LevelTicks::default(),
            game_time: 0,
            sub_tick: 0,
            post_process: Vec::new(),
            generated: Vec::new(),
            promoted: Vec::new(),
            no_save: false,
            visibility: FxHashMap::default(),
            visibility_check: Vec::new(),
            visibility_changes: Vec::new(),
            unknown: FxHashMap::default(),
            chunk_cache: Cell::new([NO_CHUNK; 4]),
        }
    }

    /// Sets the waker that asks the owner to call [`Chunks::pump`] between ticks (once; later
    /// calls are ignored). Without a waker, finished jobs are applied in [`Chunks::tick`].
    pub fn set_waker(&mut self, waker: Waker) {
        let _ = self.done_tx.wake.waker.set(waker);
    }

    /// Applies finished jobs and starts the next steps between ticks. Chunks only make progress
    /// towards full here: steps to [`ChunkStatus::Spawn`] and [`ChunkStatus::Full`] and loaded
    /// full chunks wait for the next [`Chunks::tick`], so the live world only changes in ticks
    /// (light of full chunks excepted, it is sent with the next tick's light changes).
    pub fn pump(&mut self) {
        self.done_tx.wake.pending.store(false, Ordering::Release);
        self.between_ticks = true;
        for _ in 0..1024 {
            let progress = self.drain_done() | self.dispatch();
            if !progress {
                break;
            }
        }
        self.between_ticks = false;
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

    /// Adds a ticket: chunks within `33 - level` of `pos` become full, one ring further at
    /// least lit; what is missing is generated (forced chunks: level 31, Vanilla's `ChunkLevel.byStatus(ENTITY_TICKING)`).
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

    /// The ticket level of a chunk (above [`MAX_TICKET_LEVEL`]: no ticket reaches it).
    pub fn level(&self, pos: ChunkPos) -> u8 {
        self.levels.level(pos)
    }

    /// Adds a simulation ticket: chunks within `31 - level` of `pos` tick entities and random
    /// ticks, one chunk further block ticks.
    pub fn add_simulation_ticket(&mut self, pos: ChunkPos, level: u8) -> TicketId {
        let id = TicketId(self.next_ticket);
        self.next_ticket += 1;
        self.simulation_tickets.insert(id, (pos, level));
        self.simulation.add_ticket(pos, level);
        self.simulation.run_updates();
        id
    }

    /// Removes a simulation ticket.
    pub fn remove_simulation_ticket(&mut self, id: TicketId) {
        if let Some((pos, level)) = self.simulation_tickets.remove(&id) {
            self.simulation.remove_ticket(pos, level);
            self.simulation.run_updates();
        }
    }

    /// Moves a simulation ticket to `pos`: like a player changing chunks in Vanilla
    /// (`ChunkMap.move`), the old ticket goes, the new one comes, then the levels update.
    pub fn move_simulation_ticket(&mut self, id: TicketId, pos: ChunkPos) {
        if let Some((from, level)) = self.simulation_tickets.get_mut(&id) {
            if *from == pos {
                return;
            }
            self.simulation.remove_ticket(*from, *level);
            self.simulation.add_ticket(pos, *level);
            self.simulation.run_updates();
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

    /// Whether the stored entities of a chunk are live (`areEntitiesLoaded`).
    pub fn entities_loaded(&self, pos: ChunkPos) -> bool {
        self.holders.get(&pos).is_some_and(|h| h.entities_loaded)
    }

    /// Marks the stored entities of a loaded chunk live or not (`ChunkLoadStatus`).
    pub fn set_entities_loaded(&mut self, pos: ChunkPos, loaded: bool) {
        if let Some(holder) = self.holders.get_mut(&pos) {
            holder.entities_loaded = loaded;
        }
    }

    /// The chunks that get random ticks this tick (`forEachBlockTickingChunk`: entity ticking
    /// by simulation level and a ticking chunk), in Vanilla's order.
    pub fn random_ticking_chunks(&self, out: &mut Vec<ChunkPos>) {
        out.extend(
            self.simulation
                .iter()
                .filter(|&(pos, level)| level <= ENTITY_TICKING_LEVEL && self.is_ticking_chunk(pos))
                .map(|(pos, _)| pos),
        );
    }

    fn apply_level_changes(&mut self) {
        if !self.level_changes.is_empty() {
            // `runDistanceManagerUpdates` clears the chunk cache after level changes.
            self.chunk_cache.set([NO_CHUNK; 4]);
        }
        // Moving a generation ticket reports more changes; apply them too.
        while !self.level_changes.is_empty() {
            let mut changes = std::mem::take(&mut self.level_changes);
            for change in &changes {
                self.apply_level_change(change.pos);
            }
            changes.clear();
            if self.level_changes.is_empty() {
                self.level_changes = changes;
            }
        }
    }

    /// Brings a chunk's holder to its current level (the lower of ticket and generation level).
    fn apply_level_change(&mut self, pos: ChunkPos) {
        let ticket = self.levels.level(pos);
        if self.generation_tickets.contains_key(&pos) {
            // The pyramid of a chunk being generated follows its ticket level.
            self.set_generation_ticket(pos, generation_ticket_level(ticket));
        }
        let new = ticket.min(self.generation.level(pos));
        let old = self
            .holders
            .get(&pos)
            .map_or(levels::UNLOADED, |holder| holder.level);
        if old == new {
            return;
        }
        if visibility_of_level(old) != visibility_of_level(new) {
            self.visibility_check.push(pos);
        }
        if new <= MAX_LEVEL {
            // Chunks only the generation pyramid reaches are added once a step needs them.
            if ticket > MAX_TICKET_LEVEL && !self.holders.contains_key(&pos) {
                return;
            }
            let holder = self.holders.entry(pos).or_default();
            if holder.queued {
                self.check.remove(&(holder.level, holder.rank, pos));
                self.check.insert((new, holder.rank, pos));
            }
            if let Some(unneeded) = &holder.unneeded {
                unneeded.store(false, Ordering::Relaxed);
            }
            holder.level = new;
            holder.unload_at = None;
            self.unload_candidates.remove(&pos);
            // Only a new required status gives the chunk work: a full chunk moving
            // closer to a player (level 25 to 24) needs no check.
            if new < old && required_status(new) != required_status(old) {
                self.enqueue(pos);
            }
        } else if let Some(holder) = self.holders.get_mut(&pos) {
            if holder.queued {
                self.check.remove(&(holder.level, holder.rank, pos));
                holder.queued = false;
            }
            // A generation step nobody needs any more is skipped if it did not start.
            if let Some(unneeded) = &holder.unneeded {
                unneeded.store(true, Ordering::Relaxed);
            }
            holder.level = new;
            // At least one tick later: the game writes back what the chunk holds first.
            holder.unload_at = Some(self.tick + self.settings.unload_delay.max(1));
            self.unload_candidates.insert(pos);
            // A queued load nobody needs any more is dropped before IO and decoding.
            if holder.chunk.is_none()
                && let Some(token) = holder.loading
            {
                self.slots.store.cancel(token);
            }
        }
    }

    /// Places, moves or (`None`) removes the generation ticket of a chunk.
    fn set_generation_ticket(&mut self, pos: ChunkPos, level: Option<u8>) {
        let current = self.generation_tickets.get(&pos).copied();
        if current == level {
            return;
        }
        if let Some(current) = current {
            self.generation
                .remove(pos, current, &mut self.level_changes);
            self.generation_tickets.remove(&pos);
        }
        if let Some(level) = level {
            self.generation.add(pos, level, &mut self.level_changes);
            self.generation_tickets.insert(pos, level);
        }
    }

    fn enqueue(&mut self, pos: ChunkPos) {
        let rank = self.rank(pos);
        if let Some(holder) = self.holders.get_mut(&pos)
            && !holder.queued
            && holder.level <= MAX_LEVEL
        {
            holder.queued = true;
            holder.rank = rank;
            self.check.insert((holder.level, rank, pos));
        }
    }

    /// The order of a chunk within its level: its chessboard distance to the nearest focus
    /// point, 0 without any.
    fn rank(&self, pos: ChunkPos) -> u8 {
        self.focus
            .iter()
            .map(|focus| pos.chessboard_distance(*focus))
            .min()
            .map_or(0, |distance| u8::try_from(distance).unwrap_or(u8::MAX))
    }

    /// Sets the points the players' views move towards (a flying player's view centre ahead in
    /// its direction; at most [`MAX_FOCUS`]). Within one level, chunks nearer to a focus point
    /// are loaded and generated first, so a moving view fills ahead first. Only the order of
    /// the work changes, never its result.
    pub fn set_focus(&mut self, focus: &[ChunkPos]) {
        self.focus.clear();
        self.focus.extend(focus.iter().take(MAX_FOCUS).copied());
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

    // --- Synchronous loads ---------------------------------------------------------------

    /// Whether a chunk is accessible as a full chunk (ticket level up to full).
    #[inline]
    fn is_full_at(&self, pos: ChunkPos) -> bool {
        self.holders
            .get(&pos)
            .is_some_and(|h| h.level <= FULL_LEVEL && h.status() == Some(ChunkStatus::Full))
    }

    /// A read of an accessible chunk on the world thread (`ServerChunkCache.getChunk`): unless
    /// the chunk cache holds it, it goes into the cache and `reset` restarts its `unknown`
    /// ticket. Only tracked while `unknown` tickets exist.
    #[inline]
    fn note_read(&self, pos: ChunkPos, reset: bool) {
        if self.unknown.is_empty() {
            return;
        }
        let mut cache = self.chunk_cache.get();
        if cache.contains(&pos) {
            return;
        }
        cache.copy_within(0..3, 1);
        cache[0] = pos;
        self.chunk_cache.set(cache);
        if reset && let Some((_, left)) = self.unknown.get(&pos) {
            left.set(1);
        }
    }

    /// `ServerChunkCache.getChunkNow(x, z) != null`: whether the chunk is accessible, which
    /// puts it into the chunk cache without touching tickets.
    pub fn chunk_now(&self, pos: ChunkPos) -> bool {
        let accessible = self.is_full_at(pos);
        if accessible {
            self.note_read(pos, false);
        }
        accessible
    }

    /// `ServerChunkCache.getChunk(x, z, FULL, true)` on the world thread, as `getBlockState`
    /// and `setBlock` call it: a chunk that is not accessible gets an `unknown` ticket at
    /// level 33 (timeout 1 tick, [`Chunks::purge_stale_tickets`]) and is loaded or generated
    /// while the world thread waits (`managedBlock`). Returns whether it had to wait; the
    /// chunks that became live are in [`Chunks::take_promoted`].
    #[inline]
    pub fn load_now(&mut self, pos: ChunkPos) -> bool {
        if self.is_full_at(pos) {
            self.note_read(pos, true);
            return false;
        }
        self.load_blocking(pos);
        true
    }

    #[cold]
    fn load_blocking(&mut self, pos: ChunkPos) {
        match self.unknown.get(&pos) {
            Some((_, left)) => left.set(1),
            None => {
                let id = self.add_ticket(pos, FULL_LEVEL);
                self.unknown.insert(pos, (id, Cell::new(1)));
            }
        }
        // What waits for the next tick is wanted now.
        for (at, chunk, entities) in std::mem::take(&mut self.loaded_between_ticks) {
            self.apply_loaded(at, Ok(Some(chunk)), entities);
        }
        for at in std::mem::take(&mut self.due_next_tick) {
            self.enqueue(at);
        }
        loop {
            let mut progress = self.drain_done();
            self.apply_level_changes();
            if self.is_full_at(pos) {
                break;
            }
            progress |= self.dispatch();
            if progress {
                continue;
            }
            if self.in_flight == 0 && self.check.is_empty() {
                log::error!(
                    "Couldn't load chunk {}, {} in {} synchronously",
                    pos.x,
                    pos.z,
                    self.info.dimension
                );
                break;
            }
            match self.done_rx.recv_timeout(Duration::from_millis(1)) {
                Ok(job) => self.apply_done(job),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        self.check_visibility();
        // The ticket changed levels, which cleared the cache; the chunk goes in.
        self.chunk_cache.set([NO_CHUNK; 4]);
        self.note_read(pos, false);
    }

    /// `TicketStorage.purgeStaleTickets` for the `unknown` tickets of synchronous loads: each
    /// counts down and goes once below zero, so a ticket lives through two purges. Runs once
    /// per world tick at the start of the chunk source tick while the game runs normally.
    pub fn purge_stale_tickets(&mut self) {
        if self.unknown.is_empty() {
            return;
        }
        let mut expired = Vec::new();
        self.unknown.retain(|_, (id, left)| {
            left.set(left.get() - 1);
            if left.get() < 0 {
                expired.push(*id);
                false
            } else {
                true
            }
        });
        for id in expired {
            self.remove_ticket(id);
        }
    }

    // --- Tick ----------------------------------------------------------------------------

    /// Applies finished jobs and ticket changes, starts new jobs and unloads unneeded chunks.
    /// With an inline executor this runs until nothing can make progress.
    pub fn tick(&mut self) {
        self.tick += 1;
        for (pos, chunk, entities) in std::mem::take(&mut self.loaded_between_ticks) {
            self.apply_loaded(pos, Ok(Some(chunk)), entities);
        }
        for pos in std::mem::take(&mut self.due_next_tick) {
            self.enqueue(pos);
        }
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
        self.check_visibility();
        // `ServerChunkCache.tick` ends with `clearCache`.
        self.chunk_cache.set([NO_CHUNK; 4]);
    }

    /// The visibility of a chunk (`ChunkMap.onFullChunkStatusChange`): hidden unless it is
    /// full with a level up to full.
    pub fn visibility(&self, pos: ChunkPos) -> Visibility {
        self.visibility
            .get(&pos)
            .copied()
            .unwrap_or(Visibility::Hidden)
    }

    fn current_visibility(&self, pos: ChunkPos) -> Visibility {
        self.holders
            .get(&pos)
            .filter(|h| h.status() == Some(ChunkStatus::Full))
            .map_or(Visibility::Hidden, |h| visibility_of_level(h.level))
    }

    /// Records the visibility changes of the chunks touched this tick, in touch order.
    fn check_visibility(&mut self) {
        let check = std::mem::take(&mut self.visibility_check);
        for &pos in &check {
            let new = self.current_visibility(pos);
            let old = if new == Visibility::Hidden {
                self.visibility.remove(&pos)
            } else {
                self.visibility.insert(pos, new)
            }
            .unwrap_or(Visibility::Hidden);
            if old != new {
                self.visibility_changes
                    .push(VisibilityChange { pos, old, new });
            }
        }
        self.visibility_check = check;
        self.visibility_check.clear();
    }

    /// Visibility changes since the last call, in order
    /// (`PersistentEntitySectionManager.updateChunkStatus`). A chunk becomes hidden before it
    /// unloads, at least one [`Chunks::tick`] earlier.
    pub fn take_visibility_changes(&mut self, out: &mut Vec<VisibilityChange>) {
        out.append(&mut self.visibility_changes);
    }

    /// Whether no job, load, save or unload is pending: the chunks are as their tickets say.
    pub fn is_idle(&self) -> bool {
        self.in_flight == 0
            && self.encode_jobs == 0
            && self.check.is_empty()
            && self.unload_candidates.is_empty()
            && self.visibility_changes.is_empty()
    }

    /// The chunks whose ticket level asks for a full chunk that is not full yet
    /// (`ChunkLoadCounter.pendingChunks`); ticket changes count from the next [`Chunks::tick`].
    pub fn pending_full(&self) -> usize {
        self.holders
            .values()
            .filter(|h| h.level <= FULL_LEVEL && h.status() != Some(ChunkStatus::Full))
            .count()
    }

    /// Whether a chunk is accessible as a full chunk (`hasChunk`: level up to full).
    pub fn is_accessible(&self, pos: ChunkPos) -> bool {
        self.visibility.contains_key(&pos)
    }

    /// `DistanceManager.inEntityTickingRange`: entities in this chunk tick by simulation
    /// distance.
    pub fn in_entity_ticking_range(&self, pos: ChunkPos) -> bool {
        self.simulation.level(pos) <= ENTITY_TICKING_LEVEL
    }

    fn drain_done(&mut self) -> bool {
        let mut any = false;
        while let Ok(done) = self.done_rx.try_recv() {
            any = true;
            match done {
                Done::Loaded(pos, Ok(Some(chunk)), entities)
                    if self.between_ticks
                        && chunk.status == ChunkStatus::Full
                        && chunk.light_on =>
                {
                    // The job is done; the chunk stays busy until it goes live.
                    self.in_flight -= 1;
                    self.loaded_between_ticks.push((pos, chunk, entities));
                }
                done => self.apply_done(done),
            }
        }
        while let Ok(completion) = self.inbox.try_recv() {
            any = true;
            self.apply_completion(completion);
        }
        any
    }

    /// Installs a result of the store.
    fn apply_completion(&mut self, completion: Completion) {
        match completion {
            Completion::Op {
                token,
                index,
                result,
            } => {
                let Some(load) = self.loads.get_mut(&token) else {
                    // A save.
                    if let StoreResult::Failed(e) = result {
                        log::error!("Couldn't save a chunk in {}: {e}", self.info.dimension);
                    }
                    return;
                };
                match index {
                    0 => load.chunk = Some(result),
                    1 => load.entities = Some(result),
                    _ => load.poi = Some(result),
                }
                if load.chunk.is_none() || load.entities.is_none() || load.poi.is_none() {
                    return;
                }
                let Some(PendingLoad {
                    pos,
                    chunk: Some(chunk),
                    entities: Some(entities),
                    poi: Some(poi),
                }) = self.loads.remove(&token)
                else {
                    return;
                };
                if let Some(holder) = self.holders.get_mut(&pos) {
                    holder.loading = None;
                }
                if matches!(chunk, StoreResult::Cancelled)
                    || matches!(entities, StoreResult::Cancelled)
                    || matches!(poi, StoreResult::Cancelled)
                {
                    self.in_flight -= 1;
                    if let Some(holder) = self.holders.get_mut(&pos) {
                        holder.busy = false;
                    }
                    self.wake(pos);
                    return;
                }
                let needs_decode = matches!(chunk, StoreResult::Record(..))
                    || matches!(entities, StoreResult::Record(..))
                    || matches!(poi, StoreResult::Record(..));
                if !needs_decode {
                    let chunk = load_result(chunk).map(|_| None);
                    let entities = load_result(entities).map(|_| None);
                    let poi = load_result(poi).map(|_| None);
                    self.apply_done(Done::Loaded(pos, chunk, (entities, poi)));
                    return;
                }
                let codec = Arc::clone(&self.slots.codec);
                let tx = self.done_tx.clone();
                let priority = self
                    .holders
                    .get(&pos)
                    .map_or(job_priority(MAX_LEVEL, 0), |h| {
                        job_priority(h.level, h.rank)
                    });
                self.slots.executor.spawn(
                    priority,
                    Box::new(move || {
                        let chunk = load_result(chunk).and_then(|record| {
                            record
                                .map(|(meta, bytes)| codec.decode(pos, &meta, &bytes).map(Box::new))
                                .transpose()
                        });
                        let entities = load_result(entities).and_then(|record| {
                            record
                                .map(|(meta, bytes)| codec.decode_entities(pos, &meta, &bytes))
                                .transpose()
                        });
                        let poi = load_result(poi).and_then(|record| {
                            Ok(record
                                .map(|(meta, bytes)| codec.decode_poi(pos, &meta, &bytes))
                                .transpose()?
                                .flatten())
                        });
                        tx.send(Done::Loaded(pos, chunk, (entities, poi)));
                    }),
                );
            }
            Completion::Commit { round, result } => {
                self.committing = false;
                if let Err(e) = result {
                    log::error!(
                        "Couldn't commit saved chunks of {}: {e}",
                        self.info.dimension
                    );
                }
                let mut index = 0;
                while index < self.commit_waiters.len() {
                    if self.commit_waiters[index].0 <= round {
                        let (_, then) = self.commit_waiters.swap_remove(index);
                        then();
                    } else {
                        index += 1;
                    }
                }
            }
        }
    }

    /// A finished load: the chunk (or a new one) goes into its holder with its stored entities.
    fn apply_loaded(
        &mut self,
        pos: ChunkPos,
        result: io::Result<Option<Box<ProtoChunk>>>,
        (entities, poi): LoadedRecords,
    ) {
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
        let entities_stored = match entities {
            Ok(Some(entities)) => {
                chunk.entities = entities;
                true
            }
            Ok(None) => false,
            Err(e) => {
                log::error!(
                    "Couldn't load the entities of chunk {}, {} in {}: {e}",
                    pos.x,
                    pos.z,
                    self.info.dimension
                );
                false
            }
        };
        match poi {
            Ok(poi) => chunk.poi = poi,
            Err(e) => log::error!(
                "Couldn't load the POI of chunk {}, {} in {}: {e}",
                pos.x,
                pos.z,
                self.info.dimension
            ),
        }
        let mut dirty = chunk.status == ChunkStatus::Empty;
        // Stored light that is not trusted is computed again (Vanilla: `isLightOn`).
        if chunk.status > ChunkStatus::Features && !chunk.light_on {
            chunk.status = ChunkStatus::Features;
            dirty = true;
        }
        if let Some(holder) = self.holders.get_mut(&pos) {
            holder.chunk = Some(Arc::new(chunk));
            holder.busy = false;
            holder.entities_stored = entities_stored;
            if dirty {
                holder.touch(pos, &mut self.changed);
            }
            self.promote(pos);
            self.wake(pos);
        }
    }

    fn apply_done(&mut self, done: Done) {
        match done {
            Done::Loaded(pos, result, entities) => {
                self.in_flight -= 1;
                self.apply_loaded(pos, result, entities);
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
                        holder.unneeded = None;
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
                        holder.touch(pos, &mut self.changed);
                    }
                    self.wake(pos);
                }
                for neighbour in neighbours {
                    self.merge_light(&neighbour);
                }
            }
            Done::Skipped { target, written } => {
                self.in_flight -= 1;
                for pos in std::iter::once(target).chain(written) {
                    if let Some(holder) = self.holders.get_mut(&pos) {
                        holder.busy = false;
                        if pos == target {
                            holder.unneeded = None;
                        }
                    }
                    self.wake(pos);
                }
            }
            Done::Relit { center, chunks } => {
                self.relighting.remove(&center);
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
                        let full = current.status == ChunkStatus::Full;
                        holder.touch(chunk.pos, &mut self.changed);
                        if !change.is_empty() && full {
                            self.light_changes
                                .entry(chunk.pos)
                                .or_default()
                                .add(&change);
                        }
                    }
                }
            }
            Done::Encoded {
                round,
                chunks,
                errors,
            } => {
                self.encode_jobs -= 1;
                self.round_jobs -= usize::from(round);
                self.uncommitted = true;
                for (pos, e) in errors {
                    log::error!(
                        "Couldn't save chunk {}, {} in {}: {e}",
                        pos.x,
                        pos.z,
                        self.info.dimension
                    );
                }
                for Snapshot { chunk, .. } in chunks {
                    if self
                        .encoding
                        .get(&chunk.pos)
                        .is_some_and(|encoding| Arc::ptr_eq(encoding, &chunk))
                    {
                        self.encoding.remove(&chunk.pos);
                    }
                }
            }
        }
    }

    /// Puts a chunk a job wrote back into its holder.
    fn put_back(&mut self, pos: ChunkPos, chunk: Arc<ProtoChunk>) {
        if let Some(holder) = self.holders.get_mut(&pos) {
            holder.chunk = Some(chunk);
            holder.touch(pos, &mut self.changed);
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
        let full = current.status == ChunkStatus::Full;
        holder.touch(pos, &mut self.changed);
        if full {
            self.light_changes.insert(pos, LightChange::ALL);
        }
    }

    fn dispatch(&mut self) -> bool {
        let mut progress = false;
        while let Some(&(level, rank, pos)) = self.check.first() {
            if self.in_flight >= self.settings.max_jobs {
                break;
            }
            self.check.remove(&(level, rank, pos));
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
        let Some(status) = holder.status() else {
            return self.start_load(pos);
        };
        let next = status.next().filter(|_| status < required);
        // The first check of a step adds every missing neighbour it needs, so they load
        // together; later checks stop at the first one not ready.
        let plan = holder.planned != next;
        let Some(next) = next else {
            self.set_generation_ticket(pos, None);
            return Check::Done;
        };
        if self.between_ticks && matches!(next, ChunkStatus::Spawn | ChunkStatus::Full) {
            self.due_next_tick.insert(pos);
            return Check::Done;
        }
        // Requirements by radius.
        let radius = next.requirement_radius() as i32;
        let mut blocked = None;
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
                let other_holder = self.holders.get(&other);
                if other_holder
                    .and_then(Holder::status)
                    .is_some_and(|status| status >= needed)
                {
                    continue;
                }
                match other_holder {
                    None => self.add_dependency(pos, other),
                    // Kept only by a generation ticket that is gone: this chunk's own
                    // ticket keeps it.
                    Some(other) if required_status(other.level) < Some(needed) => {
                        self.add_generation_ticket(pos);
                    }
                    Some(_) => {}
                }
                if !plan {
                    return Check::Blocked(other);
                }
                blocked.get_or_insert(other);
            }
        }
        if let Some(holder) = self.holders.get_mut(&pos) {
            holder.planned = Some(next);
        }
        if let Some(other) = blocked {
            return Check::Blocked(other);
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

    /// Gives a chunk with a ticket that is being generated its generation ticket: it keeps the
    /// chunks its steps need while no ticket reaches them. Chunks without a ticket are needed
    /// only within such a pyramid, which already reaches everything they need.
    fn add_generation_ticket(&mut self, pos: ChunkPos) {
        let ticket = self.levels.level(pos);
        if ticket <= MAX_TICKET_LEVEL {
            self.set_generation_ticket(pos, generation_ticket_level(ticket));
        }
    }

    /// Adds the holder of a chunk that a step of `of` needs but no ticket reaches.
    fn add_dependency(&mut self, of: ChunkPos, pos: ChunkPos) {
        self.add_generation_ticket(of);
        let level = self.levels.level(pos).min(self.generation.level(pos));
        if level > MAX_LEVEL {
            return;
        }
        self.holders.insert(
            pos,
            Holder {
                level,
                ..Holder::default()
            },
        );
        self.enqueue(pos);
    }

    fn start_load(&mut self, pos: ChunkPos) -> Check {
        if self.in_flight >= self.settings.max_jobs {
            return Check::Full;
        }
        let Some(holder) = self.holders.get_mut(&pos) else {
            return Check::Done;
        };
        if let Some(encoding) = self.encoding.get(&pos) {
            // A save being encoded answers the load.
            holder.chunk = Some(Arc::clone(encoding));
            holder.entities_stored = true;
            holder.touch(pos, &mut self.changed);
            holder.touch_entities(pos, &mut self.changed);
            self.promote(pos);
            self.wake(pos);
            return Check::Progress;
        }
        holder.busy = true;
        self.in_flight += 1;
        self.next_token += 1;
        let token = Token(self.next_token);
        holder.loading = Some(token);
        let priority = Priority::View(holder.level);
        self.loads.insert(
            token,
            PendingLoad {
                pos,
                ..PendingLoad::default()
            },
        );
        let dimension = self.settings.save.dimension;
        self.slots.store.submit(
            token,
            vec![
                StoreOp::Get(RecordKey::chunk(dimension, pos)),
                StoreOp::Get(RecordKey::entities(dimension, pos)),
                StoreOp::Get(RecordKey::poi(dimension, pos)),
            ],
            priority,
            self.inbox_tx.clone(),
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
                    holder.touch(pos, &mut self.changed);
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
        let rank = holder.rank;
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
        let priority = job_priority(level, rank);
        match next {
            ChunkStatus::InitializeLight => {
                let light = Arc::clone(&self.slots.light);
                self.slots.executor.spawn(
                    priority,
                    Box::new(move || {
                        let mut targets = vec![target];
                        light.initialize_light(&mut targets);
                        tx.send(Done::Stepped {
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
                        tx.send(Done::Lit {
                            targets,
                            neighbours,
                        });
                    }),
                );
            }
            _ => {
                let generator = Arc::clone(&self.slots.generator);
                let unneeded = Arc::new(AtomicBool::new(false));
                if let Some(holder) = self.holders.get_mut(&pos) {
                    holder.unneeded = Some(Arc::clone(&unneeded));
                }
                self.slots.executor.spawn(
                    priority,
                    Box::new(move || {
                        if unneeded.load(Ordering::Relaxed) {
                            tx.send(Done::Skipped {
                                target: pos,
                                written,
                            });
                            return;
                        }
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
                        tx.send(Done::Stepped {
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
    /// queues (and the chunk is saved again for them) and its post-processing marks wait for
    /// its neighbours.
    fn promote(&mut self, pos: ChunkPos) {
        let Some(chunk) = self.holders.get_mut(&pos).and_then(|h| h.chunk.as_mut()) else {
            return;
        };
        if chunk.status != ChunkStatus::Full {
            return;
        }
        let chunk = Arc::make_mut(chunk);
        self.visibility_check.push(pos);
        let blocks = std::mem::take(&mut chunk.block_ticks);
        let fluids = std::mem::take(&mut chunk.fluid_ticks);
        if !chunk.post_processing.is_empty() {
            self.post_process.push(pos);
        }
        self.promoted.push(pos);
        // Stored ticks hold delays relative to the save; the next save writes them anew from
        // the game time then, even if nothing else changes (the new `LevelChunk` is unsaved).
        let ticks = !blocks.is_empty() || !fluids.is_empty();
        self.block_ticks.add_container(pos, &blocks, self.game_time);
        self.fluid_ticks.add_container(pos, &fluids, self.game_time);
        if ticks {
            self.mark_dirty(pos);
        }
    }

    /// Chunks that became live since the last call (the game loads their block entities).
    pub fn take_promoted(&mut self, out: &mut Vec<ChunkPos>) {
        out.append(&mut self.promoted);
    }

    /// Live chunks the next [`Chunks::tick`] unloads unless a ticket comes back meanwhile (the
    /// game writes their block entities back first).
    pub fn unloading_next_tick(&self, out: &mut Vec<ChunkPos>) {
        if self.no_save {
            return;
        }
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
        if holder.status() != Some(ChunkStatus::Full) {
            return None;
        }
        holder.touch(pos, &mut self.changed);
        Some(&mut Arc::make_mut(holder.chunk.as_mut()?).block_entities)
    }

    /// The stored entities of a full chunk ([`ProtoChunk::entities`]): what it was loaded
    /// with, or what the game last wrote back.
    pub fn stored_entities(&self, pos: ChunkPos) -> &[Arc<[u8]>] {
        self.chunk(pos).map_or(&[], |c| &c.entities[..])
    }

    /// The stored POI of a full chunk ([`ProtoChunk::poi`]).
    pub fn stored_poi(&self, pos: ChunkPos) -> Option<&Arc<[u8]>> {
        self.chunk(pos).and_then(|c| c.poi.as_ref())
    }

    /// Replaces the stored POI of a full chunk (the game writes its POI back before saving and
    /// unloading); the chunk's POI count as changed if they differ.
    pub fn set_stored_poi(&mut self, pos: ChunkPos, poi: Option<Arc<[u8]>>) {
        let Some(holder) = self.holders.get_mut(&pos) else {
            return;
        };
        if holder.busy || holder.status() != Some(ChunkStatus::Full) {
            return;
        }
        let Some(chunk) = holder.chunk.as_mut() else {
            return;
        };
        if chunk.poi == poi {
            return;
        }
        Arc::make_mut(chunk).poi = poi;
        holder.touch_poi(pos, &mut self.changed);
    }

    /// Replaces the stored entities of a full chunk (the game writes its live entities back
    /// before saving and unloading); the chunk's entities count as changed if they differ.
    pub fn set_stored_entities(&mut self, pos: ChunkPos, entities: Vec<Arc<[u8]>>) {
        let Some(holder) = self.holders.get_mut(&pos) else {
            return;
        };
        if holder.busy || holder.status() != Some(ChunkStatus::Full) {
            return;
        }
        let Some(chunk) = holder.chunk.as_mut() else {
            return;
        };
        if chunk.entities == entities {
            return;
        }
        Arc::make_mut(chunk).entities = entities;
        holder.touch_entities(pos, &mut self.changed);
    }

    /// Sets `noSave` (`/save-off`): while set, chunks no ticket needs stay loaded instead of
    /// being saved and unloaded (`ChunkMap.tick` skips `processUnloads`).
    pub fn set_no_save(&mut self, no_save: bool) {
        self.no_save = no_save;
    }

    fn unload(&mut self) {
        if self.no_save {
            return;
        }
        let now = self.tick;
        // Above the backlog limit changed chunks stay loaded until the IO threads caught up.
        let save_allowed = self.slots.store.backlog() <= self.settings.save.max_backlog;
        let mut ready = Vec::new();
        for &pos in &self.unload_candidates {
            if let Some(holder) = self.holders.get(&pos)
                && holder.level > MAX_LEVEL
                && holder.unload_at.is_some_and(|at| at <= now)
                && !holder.busy
                && (save_allowed || !holder.unsaved())
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
                let blocks = self.block_ticks.remove_container(pos, self.game_time);
                let fluids = self.fluid_ticks.remove_container(pos, self.game_time);
                if holder.dirty {
                    let chunk = Arc::make_mut(chunk);
                    chunk.block_ticks = blocks;
                    chunk.fluid_ticks = fluids;
                    chunk.last_update = self.game_time;
                }
                self.post_process.retain(|&p| p != pos);
                self.visibility_check.push(pos);
            }
            if let Some(waiters) = self.waiters.remove(&pos) {
                for waiter in waiters {
                    self.enqueue(waiter);
                }
            }
            if holder.unsaved()
                && let Some(chunk) = holder.chunk
            {
                save.push(Snapshot {
                    entities: holder.entities_dirty
                        && (holder.entities_stored || !chunk.entities.is_empty()),
                    poi: holder.poi_dirty,
                    blocks: holder.dirty,
                    chunk,
                });
            }
        }
        self.save_chunks(save, false);
    }

    // --- Saving --------------------------------------------------------------------------

    /// Encodes snapshots on the pool in jobs of about 1 ms; each job submits its records.
    fn save_chunks(&mut self, chunks: Vec<Snapshot>, round: bool) {
        if chunks.is_empty() {
            return;
        }
        for snapshot in &chunks {
            self.encoding
                .insert(snapshot.chunk.pos, Arc::clone(&snapshot.chunk));
        }
        let per_job = self.slots.codec.chunks_per_job().max(1);
        let dimension = self.settings.save.dimension;
        for batch in chunks.chunks(per_job) {
            let batch = batch.to_vec();
            let codec = Arc::clone(&self.slots.codec);
            let store = Arc::clone(&self.slots.store);
            let tx = self.done_tx.clone();
            let inbox = self.inbox_tx.clone();
            self.next_token += 1;
            let token = Token(self.next_token);
            self.encode_jobs += 1;
            self.round_jobs += usize::from(round);
            self.slots.executor.spawn(
                job_priority(MAX_LEVEL + 1, 0),
                Box::new(move || {
                    let mut ops = Vec::with_capacity(batch.len());
                    let mut errors = Vec::new();
                    for snapshot in &batch {
                        let chunk = &snapshot.chunk;
                        if snapshot.blocks {
                            match codec.encode(chunk) {
                                Ok((meta, bytes)) => ops.push(StoreOp::Put {
                                    key: RecordKey::chunk(dimension, chunk.pos),
                                    meta,
                                    bytes,
                                }),
                                Err(e) => errors.push((chunk.pos, e)),
                            }
                        }
                        if snapshot.entities {
                            let key = RecordKey::entities(dimension, chunk.pos);
                            if chunk.entities.is_empty() {
                                // `EntityStorage.storeEntities`: no entities, no record.
                                ops.push(StoreOp::Delete(key));
                            } else {
                                match codec.encode_entities(chunk) {
                                    Ok((meta, bytes)) => {
                                        ops.push(StoreOp::Put { key, meta, bytes });
                                    }
                                    Err(e) => errors.push((chunk.pos, e)),
                                }
                            }
                        }
                        if snapshot.poi {
                            let key = RecordKey::poi(dimension, chunk.pos);
                            if chunk.poi.is_none() {
                                ops.push(StoreOp::Delete(key));
                            } else {
                                match codec.encode_poi(chunk) {
                                    Ok((meta, bytes)) => {
                                        ops.push(StoreOp::Put { key, meta, bytes });
                                    }
                                    Err(e) => errors.push((chunk.pos, e)),
                                }
                            }
                        }
                    }
                    if !ops.is_empty() {
                        store.submit(token, ops, Priority::Background, inbox);
                    }
                    tx.send(Done::Encoded {
                        round,
                        chunks: batch,
                        errors,
                    });
                }),
            );
        }
    }

    /// Takes the snapshot of a changed chunk for saving; `None` if it is unchanged or a job
    /// works on it (it is then saved in the next round).
    fn capture(&mut self, pos: ChunkPos) -> Option<Snapshot> {
        let holder = self.holders.get_mut(&pos)?;
        if !holder.unsaved() {
            return None;
        }
        if holder.busy {
            self.changed.push(pos);
            return None;
        }
        let chunk = holder.chunk.as_mut()?;
        let blocks = std::mem::take(&mut holder.dirty);
        let entities = std::mem::take(&mut holder.entities_dirty)
            && (holder.entities_stored || !chunk.entities.is_empty());
        if entities {
            holder.entities_stored = !chunk.entities.is_empty();
        }
        let poi = std::mem::take(&mut holder.poi_dirty);
        if blocks && chunk.status == ChunkStatus::Full {
            let chunk = Arc::make_mut(chunk);
            chunk.block_ticks = self.block_ticks.pack(pos, self.game_time);
            chunk.fluid_ticks = self.fluid_ticks.pack(pos, self.game_time);
            chunk.last_update = self.game_time;
        }
        Some(Snapshot {
            chunk: Arc::clone(chunk),
            blocks,
            entities,
            poi,
        })
    }

    /// Starts a save round (autosave, `/save-all`): every chunk changed so far is captured by
    /// [`Chunks::autosave_step`] from the remaining time of the next ticks. A round that is
    /// still running takes the new changes over. `then` runs once the round is committed.
    pub fn start_save_round(&mut self, then: Option<SavedCallback>) {
        let round = self.round.get_or_insert_with(SaveRound::default);
        round.pending.extend(self.changed.drain(..));
        round.waiters.extend(then);
    }

    /// Whether a save round is running.
    pub fn save_round_running(&self) -> bool {
        self.round.is_some()
    }

    /// Runs after the world tick, before the post-tick phase: captures chunks of the running
    /// save round for at most `budget` (the tick's remaining time, capped by the tick budget),
    /// but at least the minimum per tick; seals and commits the round once every chunk is
    /// captured and submitted. Saves of unloaded chunks are committed every commit interval.
    pub fn autosave_step(&mut self, budget: Duration) {
        if self.round.is_some() && self.slots.store.backlog() <= self.settings.save.max_backlog {
            let started = Instant::now();
            let budget = budget.min(self.settings.save.tick_budget);
            let min = self.settings.save.min_chunks_per_tick;
            let per_job = self.slots.codec.chunks_per_job().max(1);
            let mut batch = Vec::with_capacity(per_job);
            let (mut captured, mut visited) = (0usize, 0usize);
            while let Some(pos) = self.round.as_mut().and_then(|r| r.pending.pop_front()) {
                visited += 1;
                if let Some(chunk) = self.capture(pos) {
                    batch.push(chunk);
                    captured += 1;
                    // Jobs are queued as they fill, so the budget covers queueing them too.
                    if batch.len() >= per_job {
                        self.save_chunks(std::mem::take(&mut batch), true);
                    }
                }
                if captured >= min && visited.is_multiple_of(8) && started.elapsed() >= budget {
                    break;
                }
            }
            self.save_chunks(batch, true);
        }
        let sealed =
            self.round.as_ref().is_some_and(|r| r.pending.is_empty()) && self.round_jobs == 0;
        if sealed {
            let waiters = self.round.take().map(|r| r.waiters).unwrap_or_default();
            self.commit(waiters);
        } else if self.round.is_none()
            && self.uncommitted
            && !self.committing
            && self.tick.saturating_sub(self.last_commit) >= self.settings.save.commit_interval
        {
            self.commit(Vec::new());
        }
    }

    /// Commits everything submitted so far; `waiters` run when it is done.
    fn commit(&mut self, waiters: Vec<SavedCallback>) {
        self.commits += 1;
        let round = self.commits;
        self.committing = true;
        self.uncommitted = false;
        self.last_commit = self.tick;
        self.commit_waiters
            .extend(waiters.into_iter().map(|then| (round, then)));
        self.slots.store.commit(round, self.inbox_tx.clone());
    }

    /// Runs `then` once the running save round is committed, at once if none runs.
    pub fn when_saved(&mut self, then: SavedCallback) {
        match &mut self.round {
            Some(round) => round.waiters.push(then),
            None => then(),
        }
    }

    /// Saves every changed chunk. With `flush` everything is captured now, written and
    /// committed before this returns (shutdown, `save-all flush`); otherwise a save round
    /// starts and runs from the remaining time of the next ticks.
    pub fn save_all(&mut self, flush: bool) {
        self.start_save_round(None);
        if !flush {
            return;
        }
        // Jobs that write chunks finish first, so no changed chunk is busy.
        self.wait_until(|chunks| chunks.in_flight == 0 && chunks.relighting.is_empty());
        // Relights applied while waiting changed chunks too.
        self.start_save_round(None);
        let pending: Vec<ChunkPos> = self
            .round
            .as_mut()
            .map(|r| r.pending.drain(..).collect())
            .unwrap_or_default();
        let mut captured = Vec::with_capacity(pending.len());
        for pos in pending {
            captured.extend(self.capture(pos));
        }
        self.save_chunks(captured, true);
        // Every encode job submits its records first, so the commit covers them.
        self.wait_until(|chunks| chunks.encode_jobs == 0);
        let waiters = self.round.take().map(|r| r.waiters).unwrap_or_default();
        self.commit(waiters);
        let round = self.commits;
        let (tx, rx) = mpsc::channel();
        self.commit_waiters.push((
            round,
            Box::new(move || {
                let _ = tx.send(());
            }),
        ));
        self.wait_until(|_| rx.try_recv().is_ok());
    }

    /// Applies results until `done` holds (blocking; flush and shutdown only).
    fn wait_until(&mut self, mut done: impl FnMut(&Self) -> bool) {
        loop {
            self.drain_done();
            if done(self) {
                return;
            }
            match self.done_rx.recv_timeout(Duration::from_millis(1)) {
                Ok(job) => self.apply_done(job),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
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
        let chunk_pos = pos.chunk();
        let holder = self.holders.get(&chunk_pos)?;
        let chunk = holder
            .chunk
            .as_ref()
            .filter(|c| c.status == ChunkStatus::Full)?;
        if holder.level <= FULL_LEVEL {
            self.note_read(chunk_pos, true);
        }
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
        holder.touch(chunk_pos, &mut self.changed);
        self.block_changes.push(pos);
        if self.slots.light.changes_light(old, state) {
            self.relight.insert(chunk_pos);
        }
        Some(old)
    }

    /// How many block changes are recorded since the last [`Self::take_block_changes`].
    pub fn block_change_count(&self) -> usize {
        self.block_changes.len()
    }

    /// The block changes recorded after the first `start` (see [`Self::block_change_count`]),
    /// without taking them.
    pub fn block_changes_from(&self, start: usize) -> &[BlockPos] {
        &self.block_changes[start.min(self.block_changes.len())..]
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
                holder.touch(pos, &mut self.changed);
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
            holder.touch(chunk, &mut self.changed);
        }
    }

    /// `getBlockTicks().copyAreaFrom(blockTicks, area, offset)` within this world: the block
    /// ticks scheduled in the box `min..=max` again at `pos + offset`, after all others.
    pub fn copy_block_ticks(&mut self, min: BlockPos, max: BlockPos, offset: BlockPos) {
        let inside = |p: BlockPos| {
            (min.x..=max.x).contains(&p.x)
                && (min.y..=max.y).contains(&p.y)
                && (min.z..=max.z).contains(&p.z)
        };
        let mut ticks: Vec<ScheduledTick> = Vec::new();
        for cz in (min.z >> 4)..=(max.z >> 4) {
            for cx in (min.x >> 4)..=(max.x >> 4) {
                ticks.extend(
                    self.block_ticks
                        .ticks_in(ChunkPos::new(cx, cz))
                        .filter(|t| inside(t.pos))
                        .copied(),
                );
            }
        }
        let min_sub = ticks.iter().map(|t| t.sub_tick).min().unwrap_or(0);
        let max_sub = ticks.iter().map(|t| t.sub_tick).max().unwrap_or(0);
        for tick in ticks {
            let pos = BlockPos::new(
                tick.pos.x.saturating_add(offset.x),
                tick.pos.y.saturating_add(offset.y),
                tick.pos.z.saturating_add(offset.z),
            );
            let moved = ScheduledTick {
                pos,
                sub_tick: tick
                    .sub_tick
                    .saturating_sub(min_sub)
                    .saturating_add(max_sub)
                    .saturating_add(1),
                ..tick
            };
            if self.block_ticks.schedule(moved) {
                self.mark_dirty(pos.chunk());
            }
        }
    }

    /// Changes a full chunk in place (`/fillbiome`'s `fillBiomesFromNoise`): the chunk is saved
    /// again and its cached packets are dropped. `None` if the chunk is not full.
    pub fn modify_chunk<R>(
        &mut self,
        pos: ChunkPos,
        f: impl FnOnce(&mut ProtoChunk) -> R,
    ) -> Option<R> {
        let holder = self.holders.get_mut(&pos)?;
        if holder.busy {
            return None;
        }
        let chunk = holder.chunk.as_mut()?;
        if chunk.status != ChunkStatus::Full {
            return None;
        }
        let result = f(Arc::make_mut(chunk));
        holder.dirty = true;
        // The packet cache drops chunks with block changes.
        self.block_changes
            .push(BlockPos::new(pos.x << 4, self.info.min_y, pos.z << 4));
        Some(result)
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

    /// Full chunks whose light changed since the last call (for light update packets).
    pub fn take_light_changes(&mut self, out: &mut Vec<(ChunkPos, LightChange)>) {
        out.extend(self.light_changes.drain());
    }

    /// Recomputes the light around changed chunks now and applies the results that are done:
    /// with an inline executor, all of them (scenario worlds observe settled light).
    pub fn flush_light(&mut self) {
        // Relights still to apply would hold back new ones of the same centres.
        self.drain_done();
        self.start_relights();
        self.drain_done();
    }

    /// Starts light recomputation around changed chunks whose 5 × 5 area is loaded.
    fn start_relights(&mut self) {
        if self.relight.is_empty() {
            return;
        }
        let max = self.settings.max_relights.max(1);
        let changed: Vec<ChunkPos> = self
            .relight
            .iter()
            .copied()
            .filter(|pos| !self.relighting.contains(pos))
            .take(max)
            .collect();
        for center in changed {
            self.relight.remove(&center);
            self.relighting.insert(center);
            let mut area = Vec::with_capacity(25);
            for dz in -2..=2 {
                for dx in -2..=2 {
                    let pos = ChunkPos::new(center.x + dx, center.z + dz);
                    if let Some(chunk) = self.holders.get(&pos).and_then(|h| h.chunk.as_ref()) {
                        area.push(Arc::clone(chunk));
                    }
                }
            }
            let light = Arc::clone(&self.slots.light);
            let tx = self.done_tx.clone();
            self.slots.executor.spawn(
                0,
                Box::new(move || {
                    let mut area = area;
                    light.relight(&[center], &mut Neighbours::new(&mut area));
                    tx.send(Done::Relit {
                        center,
                        chunks: area,
                    });
                }),
            );
        }
    }
}

/// The level of the generation ticket of a chunk with this ticket level: the pyramid of a
/// full chunk (or of the ring around full chunks); `None` above [`MAX_TICKET_LEVEL`].
/// The compute-pool priority of a chunk job: by level, then by rank ([`Chunks::set_focus`]).
fn job_priority(level: u8, rank: u8) -> u32 {
    u32::from(level) << 8 | u32::from(rank)
}

fn generation_ticket_level(ticket: u8) -> Option<u8> {
    (ticket <= MAX_TICKET_LEVEL).then_some(ticket.max(FULL_LEVEL))
}

/// The visibility of a full chunk at `level`.
fn visibility_of_level(level: u8) -> Visibility {
    if level <= ENTITY_TICKING_LEVEL {
        Visibility::Ticking
    } else if level <= FULL_LEVEL {
        Visibility::Tracked
    } else {
        Visibility::Hidden
    }
}

/// A store result of a load: the record, `None` if it is missing.
fn load_result(result: StoreResult) -> io::Result<Option<(RecordMeta, Arc<[u8]>)>> {
    match result {
        StoreResult::Record(meta, bytes) => Ok(Some((meta, bytes))),
        StoreResult::Missing => Ok(None),
        StoreResult::Failed(e) => Err(e),
        StoreResult::Meta(_) | StoreResult::Done | StoreResult::Cancelled => {
            Err(io::Error::other("unexpected store result for a load"))
        }
    }
}

/// [`Chunks::is_block_ticking`] over the fields, for use while the tick queues are borrowed.
fn tick_check(
    holders: &FxHashMap<ChunkPos, Holder>,
    simulation: &SimulationLevels,
    pos: ChunkPos,
) -> bool {
    simulation.level(pos) <= BLOCK_TICKING_LEVEL
        && holders.get(&pos).is_some_and(|h| {
            h.entities_loaded
                && h.level <= BLOCK_TICKING_LEVEL
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
