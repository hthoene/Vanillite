//! Startup, the tick loop (a port of Vanilla's `MinecraftServer.runServer`), world threads,
//! global phases, autosave, config reload and shutdown.

use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::Write as _;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use std::{fmt, io};

use vanillite_chunk::slots::{ChunkGeneratorProvider, ChunkStorageProvider, LightEngineProvider};
use vanillite_config::{
    Config, ConfigError, RegionFileCompression, RuleSet, VANILLITE_FILE, VanillaOr,
};
use vanillite_game::level::Level;
use vanillite_light::BfsLightEngineProvider;
use vanillite_storage::backup::BackupSettings;
use vanillite_storage::player_data::PlayerDataStore;
use vanillite_storage::saved_data::SavedDataStore;
use vanillite_storage::writer::FileWriter;
use vanillite_storage::{AnvilStorageProvider, Compression, WorldLayout};
use vanillite_world::chunks::{ChunkSettings, ChunkSlots, Chunks};
use vanillite_world::entity::NetworkIds;
use vanillite_world::tracking::{ChunkTrackerProvider, EntityTrackerProvider, TrackerSettings};

use crate::clock::Clock;
use crate::compute::{Compute, ComputePool, Scheduler};
use crate::global::Global;
use crate::net::{NetState, VanillaPack};
use crate::player_list::{PlayerEntry, PlayerList, Uuid};
use crate::slot::{Origin, Slot, SlotError};
use crate::tick_rate::{NANOS_PER_SECOND, TickRateManager};
use crate::watchdog::{Heartbeat, WatchState, Watchdog};
use crate::world::{ServerWorld, TickContext, WorldId, WorldTask};
use vanillite_net::VirtualOpener;

/// The dimensions of a server, in Vanilla's tick order.
pub const DIMENSIONS: [&str; 3] = [
    "minecraft:overworld",
    "minecraft:the_nether",
    "minecraft:the_end",
];

/// How far behind the server must be before "Can't keep up!" (plus 20 ticks).
const OVERLOADED_THRESHOLD: u64 = NANOS_PER_SECOND;
/// Minimum time between two "Can't keep up!" warnings (plus 100 ticks).
const OVERLOADED_WARNING_INTERVAL: u64 = 10 * NANOS_PER_SECOND;
/// Vanilla's autosave period in seconds of game time.
const AUTOSAVE_SECONDS: f32 = 300.0;

/// Work that runs on the server thread in the global pre-phase, with `&mut` to everything.
pub type GlobalTask = Box<dyn FnOnce(&mut Server) + Send + 'static>;

/// Work that runs once a save of every world is done.
pub type AfterSave = Box<dyn FnOnce() + Send + 'static>;

/// Counts the worlds of a save down; the last one runs the follow-up.
struct SaveDone {
    remaining: AtomicUsize,
    /// Waited for before the follow-up when the save flushes.
    writer: Option<FileWriter>,
    then: Mutex<Option<AfterSave>>,
}

impl SaveDone {
    fn world_saved(&self) {
        if self.remaining.fetch_sub(1, Ordering::AcqRel) != 1 {
            return;
        }
        if let Some(writer) = &self.writer {
            writer.flush();
        }
        let then = self
            .then
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(then) = then {
            then();
        }
    }
}

enum ServerMsg {
    Console(String),
    Global(GlobalTask),
    Stop,
    /// A task for a world owned by the server thread (sequential mode).
    WorldTask(WorldId, WorldTask),
    /// A world thread finished its tick (parallel mode).
    WorldTicked(WorldId, Vec<(WorldId, WorldTask)>),
    /// A world thread panicked.
    WorldCrashed(WorldId),
}

enum WorldMsg {
    Tick { tick: u64, runs_normally: bool },
    Task(WorldTask),
    Stop,
}

#[derive(Clone)]
enum Route {
    Server,
    World(Sender<WorldMsg>),
}

/// Who owns the worlds.
enum Worlds {
    /// The server thread ticks all worlds one after another (exact Vanilla order).
    Sequential(Vec<ServerWorld>),
    /// Each world has its own thread; the server thread coordinates.
    Parallel(Vec<WorldThread>),
}

struct WorldThread {
    sender: Sender<WorldMsg>,
    handle: JoinHandle<()>,
}

/// Configures and starts a server. Slots are filled here before startup; whatever is not
/// filled gets the built-in implementation.
#[derive(Debug)]
pub struct ServerBuilder {
    dir: PathBuf,
    scheduler: Slot<dyn Scheduler>,
    entity_tracker: Slot<dyn EntityTrackerProvider>,
    chunk_storage: Slot<dyn ChunkStorageProvider>,
    chunk_generator: Slot<dyn ChunkGeneratorProvider>,
    light_engine: Slot<dyn LightEngineProvider>,
    vanilla_pack: VanillaPack,
    network: bool,
}

impl ServerBuilder {
    /// A server running in `dir` (config files and worlds).
    pub fn new(dir: impl Into<PathBuf>) -> ServerBuilder {
        ServerBuilder {
            dir: dir.into(),
            scheduler: Slot::new("scheduler"),
            entity_tracker: Slot::new("entity-tracker"),
            chunk_storage: Slot::new("chunk-storage"),
            chunk_generator: Slot::new("chunk-generator"),
            light_engine: Slot::new("light-engine"),
            vanilla_pack: VanillaPack::default(),
            network: true,
        }
    }

    /// Where the Vanilla data pack comes from (default: downloaded once into `cache/`).
    pub fn vanilla_pack(&mut self, pack: VanillaPack) -> &mut Self {
        self.vanilla_pack = pack;
        self
    }

    /// Whether the network listens (default `true`). Without it the server only takes
    /// virtual connections ([`Server::open_virtual`]); for tests.
    pub fn network(&mut self, on: bool) -> &mut Self {
        self.network = on;
        self
    }

    /// Fills the `scheduler` slot with an embedded implementation.
    pub fn scheduler(&mut self, scheduler: Box<dyn Scheduler>) -> Result<&mut Self, SlotError> {
        self.scheduler.fill(scheduler, Origin::Embedded)?;
        Ok(self)
    }

    /// Fills the `entity-tracker` slot with an embedded implementation.
    pub fn entity_tracker(
        &mut self,
        provider: Box<dyn EntityTrackerProvider>,
    ) -> Result<&mut Self, SlotError> {
        self.entity_tracker.fill(provider, Origin::Embedded)?;
        Ok(self)
    }

    /// Fills the `chunk-storage` slot with an embedded implementation.
    pub fn chunk_storage(
        &mut self,
        provider: Box<dyn ChunkStorageProvider>,
    ) -> Result<&mut Self, SlotError> {
        self.chunk_storage.fill(provider, Origin::Embedded)?;
        Ok(self)
    }

    /// Fills the `chunk-generator` slot with an embedded implementation.
    pub fn chunk_generator(
        &mut self,
        provider: Box<dyn ChunkGeneratorProvider>,
    ) -> Result<&mut Self, SlotError> {
        self.chunk_generator.fill(provider, Origin::Embedded)?;
        Ok(self)
    }

    /// Fills the `light-engine` slot with an embedded implementation.
    pub fn light_engine(
        &mut self,
        provider: Box<dyn LightEngineProvider>,
    ) -> Result<&mut Self, SlotError> {
        self.light_engine.fill(provider, Origin::Embedded)?;
        Ok(self)
    }

    /// Loads the configuration, starts the threads and creates the worlds.
    pub fn start(self) -> Result<Server, StartError> {
        let started = Instant::now();
        log::info!("Starting Vanillite {}", env!("CARGO_PKG_VERSION"));
        let dir = self.dir;
        fs::create_dir_all(&dir).map_err(|e| StartError::io(&dir, e))?;
        log::info!("Loading properties");
        let config = Config::load(&dir)?;
        log::info!("Using profile {}", config.profile);
        if !crate::eula::check(&dir).map_err(|e| StartError::io(&dir, e))? {
            return Err(StartError::Eula);
        }

        let parallel = config.server.tick.parallel_dimensions;
        let busy_tick_threads = if parallel { DIMENSIONS.len() } else { 1 };
        let workers = match config.server.threads.compute {
            0 => std::thread::available_parallelism()
                .map_or(1, usize::from)
                .saturating_sub(busy_tick_threads)
                .max(1),
            n => usize::from(n),
        };
        let (scheduler, origin) = match self.scheduler.take() {
            Some(filled) => filled,
            None => {
                let pool = ComputePool::new(workers).map_err(|e| StartError::io(&dir, e))?;
                (Box::new(pool) as Box<dyn Scheduler>, Origin::BuiltIn)
            }
        };
        log::info!(
            "Slot scheduler: {origin} ({} compute workers)",
            scheduler.workers()
        );
        let compute = Compute::new(Arc::from(scheduler));
        let (tracker_provider, origin) = self
            .entity_tracker
            .take()
            .unwrap_or_else(|| (Box::new(ChunkTrackerProvider), Origin::BuiltIn));
        log::info!("Slot entity-tracker: {origin}");

        let level_name = config.properties.level_name.clone();
        let level_dir = dir.join(&level_name);
        let session_lock = lock_world(&level_dir)?;
        let layout = match config.server.storage.layout {
            vanillite_config::StorageLayout::Vanilla => WorldLayout::Vanilla,
            vanillite_config::StorageLayout::Spigot => WorldLayout::Spigot,
        };
        let moved = vanillite_storage::layout::adopt_layout(&dir, &level_name, layout, &DIMENSIONS)
            .map_err(|e| StartError::Io(format!("world layout of \"{level_name}\": {e}")))?;
        for line in moved {
            log::warn!("storage.layout = {layout:?}: {line}");
        }
        let writer = FileWriter::start().map_err(|e| StartError::io(&level_dir, e))?;
        let player_store = PlayerDataStore::new(&level_dir).with_writer(writer.clone());
        // Mojang's profile key signers are fetched while the data packs load.
        let services_keys = (config.properties.online_mode && self.network).then(|| {
            std::thread::spawn(|| {
                vanillite_net::services::fetch_profile_keys(Duration::from_secs(10))
            })
        });
        let prepared =
            crate::net::prepare(&config, &dir, &level_dir, &self.vanilla_pack, &player_store)?;
        let chat = chat_settings(&config, services_keys);
        let level_setup = crate::level::LevelSetup {
            level_name: level_name.clone(),
            game_type: crate::players::game_mode_id(config.properties.gamemode),
            hardcore: config.properties.hardcore,
            spawn: prepared
                .worlds
                .first()
                .map_or((vanillite_base::Vec3::ZERO, 0.0), |w| w.level.spawn),
        };
        let mut world_players = prepared.worlds.into_iter();
        let packs = prepared.packs.map(Arc::new);
        // Block behaviour reads its tags in every world, also without the noise generator.
        if let Some(packs) = packs.as_deref() {
            vanillite_worldgen::behaviour::init(packs);
        }
        let seed = prepared.world_gen.seed;
        let world_gen = prepared.world_gen.clone();
        let behaviours = Arc::new(vanillite_game::block::Behaviours::vanilla());
        let spawn_table = crate::world_setup::spawn_table(packs.as_deref()).map(Arc::new);
        let slot = |name: &str, origin: &Origin| log::info!("Slot {name}: {origin}");
        let entity_compression = match config.properties.region_file_compression {
            RegionFileCompression::Deflate => Compression::Deflate,
            RegionFileCompression::Lz4 => Compression::Lz4,
            RegionFileCompression::None => Compression::None,
        };
        let (storage_provider, origin) = self.chunk_storage.take().unwrap_or_else(|| {
            let compression = match config.properties.region_file_compression {
                RegionFileCompression::Deflate => Compression::Deflate,
                RegionFileCompression::Lz4 => Compression::Lz4,
                RegionFileCompression::None => Compression::None,
            };
            (
                Box::new(AnvilStorageProvider { compression }),
                Origin::BuiltIn,
            )
        });
        slot("chunk-storage", &origin);
        let (generator_provider, origin) = self.chunk_generator.take().unwrap_or_else(|| {
            let generators = crate::world_setup::BuiltInGenerators {
                packs: packs.clone(),
                world_gen: prepared.world_gen,
            };
            (Box::new(generators), Origin::BuiltIn)
        });
        slot("chunk-generator", &origin);
        let (light_provider, origin) = self
            .light_engine
            .take()
            .unwrap_or_else(|| (Box::new(BfsLightEngineProvider), Origin::BuiltIn));
        slot("light-engine", &origin);
        let executor = Arc::new(crate::world_setup::ComputeExecutor(compute.clone()));

        let clock = Clock::new();
        let max_players = usize::try_from(config.properties.max_players).unwrap_or(0);
        let commands = crate::commands::build_dispatcher(command_limits(&config));
        let game = Arc::new(crate::blocks::game_data(packs.as_deref()));
        vanillite_game::player::play::install_game_data(Arc::clone(&game));
        let level = crate::level::LevelState::load(
            &level_dir,
            packs.as_deref(),
            crate::survival::difficulty(config.properties.difficulty),
            &level_setup,
            writer.clone(),
        );
        let global = Arc::new_cyclic(|this| Global {
            clock,
            players: PlayerList::new(max_players),
            network_ids: Arc::new(NetworkIds::new()),
            chat,
            chat_types: prepared.chat_types,
            access: crate::access::Access::load(
                &dir,
                config.properties.white_list,
                config.properties.op_permission_level,
                config.properties.enforce_whitelist,
            ),
            handle: std::sync::OnceLock::new(),
            commands,
            online_mode: config.properties.online_mode,
            level,
            seed: level_seed(&config.properties.level_seed),
            registries: prepared.registries.clone(),
            game,
            spawn_protection: config.properties.spawn_protection,
            this: this.clone(),
        });
        let watch = Arc::new(WatchState::new(
            config.server.watchdog.early_warning_after.get(),
            config.server.watchdog.warning_interval,
        ));
        let heartbeat = Arc::new(Heartbeat::new("Server thread"));
        watch.register(Arc::clone(&heartbeat));
        let (sender, receiver) = mpsc::channel();

        log::info!("Preparing level \"{level_name}\"");
        let mut worlds = Vec::with_capacity(DIMENSIONS.len());
        for (dimension, index) in DIMENSIONS.iter().zip(0u16..) {
            let rules = Arc::new(config.world_rules(dimension));
            let view_distance = rules
                .distances
                .view_distance
                .or(u8::try_from(config.properties.view_distance.clamp(2, 32)).unwrap_or(10));
            let tracker_settings = TrackerSettings::new(
                i32::from(view_distance),
                config.properties.entity_broadcast_range_percentage,
                &rules.tracking,
            );
            let tracker = tracker_provider.create(&tracker_settings);
            let player_range = tracker_settings.range(vanillite_data::entity::ids::PLAYER);
            let info = crate::world_setup::world_info(
                packs.as_deref(),
                layout.dimension_dir(&dir, &level_name, dimension),
                dimension,
                seed,
            )?;
            let entity_io =
                crate::entity_io::EntityChunks::new(&info.dir, entity_compression, dimension);
            let slots = ChunkSlots {
                storage: storage_provider
                    .create(&info)
                    .map_err(|e| StartError::io(&info.dir, e))?,
                generator: generator_provider
                    .create(&info)
                    .map_err(|e| StartError::Io(format!("chunk generator of {dimension}: {e}")))?,
                light: light_provider.create(&info),
                executor: Arc::clone(&executor) as _,
            };
            let settings = ChunkSettings {
                view_distance: i32::from(view_distance),
                unload_delay: u64::from(rules.chunks.unload_delay),
                max_jobs: usize::from(rules.chunks.max_jobs),
                simulation_distance: i32::from(rules.distances.simulation_distance.or(
                    u8::try_from(config.properties.simulation_distance.clamp(2, 32)).unwrap_or(10),
                )),
                max_relights: usize::try_from(rules.simulation.max_relights_per_tick)
                    .unwrap_or(usize::MAX),
            };
            let level_settings = crate::world_setup::level_settings(
                packs.as_deref(),
                &info,
                &rules,
                &config.properties,
            );
            let climate = crate::world_setup::climate(packs.as_deref(), &world_gen, dimension);
            let players = world_players
                .next()
                .ok_or_else(|| StartError::Io("world setup missing".to_owned()))?;
            let bounds = (info.min_y, info.height);
            let data = SavedDataStore::new(info.dir.clone(), writer.clone());
            let mut level = Level::new(
                Chunks::new(info, settings, slots),
                level_settings,
                Arc::clone(&behaviours),
                Arc::clone(&global.network_ids),
                seed,
            );
            level.set_game_time(global.level.game_time());
            let survival = crate::survival::WorldSurvival::new(
                dimension,
                &global.level,
                &players.level,
                bounds,
                player_range,
                climate,
            );
            let mobs = crate::mobs::WorldMobs::new(spawn_table.clone(), seed, &rules);
            worlds.push(ServerWorld::new(
                WorldId(index),
                (*dimension).to_owned(),
                rules,
                tracker,
                players,
                level,
                survival,
<<<<<<< HEAD
                mobs,
                entity_io,
=======
                data,
>>>>>>> origin/dev
            ));
        }
        let (worlds, routes) = if parallel {
            log::info!("Dimensions tick in parallel");
            let mut threads = Vec::with_capacity(worlds.len());
            for world in worlds {
                let heartbeat = Arc::new(Heartbeat::new(format!("World {}", world.dimension())));
                watch.register(Arc::clone(&heartbeat));
                threads.push(spawn_world_thread(
                    world,
                    &compute,
                    &global,
                    sender.clone(),
                    heartbeat,
                )?);
            }
            let routes = threads
                .iter()
                .map(|t| Route::World(t.sender.clone()))
                .collect();
            (Worlds::Parallel(threads), routes)
        } else {
            log::info!("Dimensions tick sequentially");
            let routes = worlds.iter().map(|_| Route::Server).collect();
            (Worlds::Sequential(worlds), routes)
        };

        let net = NetState::start(
            &config,
            prepared.configuration,
            self.network,
            global.chat.enforce_secure,
        )?;
        if !config.properties.online_mode {
            log::warn!("**** SERVER IS RUNNING IN OFFLINE/INSECURE MODE!");
        }

        let watchdog = Watchdog::start(Arc::clone(&watch), clock, config.properties.max_tick_time)
            .map_err(|e| StartError::io(&dir, e))?;

        let mut server = Server {
            dir,
            config,
            compute,
            global,
            worlds,
            routes,
            sender,
            receiver,
            global_queue: VecDeque::new(),
            tick_rate: TickRateManager::default(),
            tick_count: 0,
            next_tick: 0,
            empty_ticks: 0,
            ticks_until_autosave: 0,
            average_tick_nanos: 0.0,
            tick_log: open_tick_log(),
            running: true,
            heartbeat,
            watch,
            watchdog: Some(watchdog),
            net: Some(net),
            test_bots: Vec::new(),
            layout,
            player_store,
            next_backup: Instant::now(),
            backup: None,
            _session_lock: session_lock,
        };
        server.next_backup = server.backup_due_after();
        server.ticks_until_autosave = server.autosave_interval();
        let _ = server.global.handle.set(server.handle());
        let bots = server.config.server.debug.test_bots;
        if bots > 0
            && let Some(opener) = server.virtual_opener()
        {
            log::warn!("Starting {bots} test bot(s) (debug.test-bots); not for production use");
            server.test_bots = crate::test_bot::uuids(bots);
            crate::test_bot::spawn(opener, bots);
        }
        log::info!(
            "Done ({:.3}s)! For help, type \"help\"",
            started.elapsed().as_secs_f64()
        );
        Ok(server)
    }
}

/// Holds `session.lock` of the world folder so no second server opens the same world.
/// The world seed from `level-seed`: a number, else Java's hash of the text, 0 when empty
/// (`WorldOptions.parseSeed`; the world runtime will store a random seed instead).
fn level_seed(text: &str) -> i64 {
    let text = text.trim();
    if text.is_empty() {
        return 0;
    }
    text.parse::<i64>()
        .unwrap_or_else(|_| i64::from(vanillite_base::math::java_string_hash(text)))
}

/// The command parser limits from `vanillite.toml` (`commands.*`).
fn command_limits(config: &Config) -> vanillite_command::limits::Limits {
    let c = &config.server.commands;
    let size = |v: u32| usize::try_from(v).unwrap_or(usize::MAX);
    vanillite_command::limits::Limits {
        max_command_length: size(c.max_command_length),
        max_suggestion_input: size(c.max_suggestion_input),
        max_suggestions: size(c.max_suggestions),
        max_message_length: size(c.max_message_length),
        max_nbt_depth: c.max_nbt_depth,
        max_nbt_path_nodes: size(c.max_nbt_path_nodes),
        max_selector_options: size(c.max_selector_options),
        max_message_selectors: size(c.max_message_selectors),
        max_redirect_depth: size(c.max_redirect_depth),
    }
}

/// The chat settings: secure chat is enforced when `enforce-secure-profile` is on, the server
/// runs in online mode and Mojang's keys could be fetched (`enforceSecureProfile`).
fn chat_settings(
    config: &Config,
    services_keys: Option<JoinHandle<Result<Vec<Vec<u8>>, String>>>,
) -> vanillite_game::player::chat::ChatSettings {
    use vanillite_game::player::chat::{ChatSettings, ServicesKeys};
    let keys = match services_keys.map(JoinHandle::join) {
        Some(Ok(Ok(keys))) => ServicesKeys::from_der(keys.iter().map(Vec::as_slice)),
        Some(Ok(Err(e))) => {
            log::warn!("Failed to fetch Mojang's services keys, chat cannot be verified: {e}");
            ServicesKeys::default()
        }
        Some(Err(_)) | None => ServicesKeys::default(),
    };
    let p = &config.properties;
    ChatSettings {
        enforce_secure: p.enforce_secure_profile && p.online_mode && !keys.is_empty(),
        keys: Arc::new(keys),
        chat_spam_threshold: p.chat_spam_threshold_seconds.saturating_mul(20),
        command_spam_threshold: p.command_spam_threshold_seconds.saturating_mul(20),
        max_pending_messages: usize::try_from(config.server.chat.max_pending_messages)
            .unwrap_or(4096),
    }
}

fn lock_world(level_dir: &Path) -> Result<File, StartError> {
    fs::create_dir_all(level_dir).map_err(|e| StartError::io(level_dir, e))?;
    let path = level_dir.join("session.lock");
    let mut file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| StartError::io(&path, e))?;
    match file.try_lock() {
        Ok(()) => {}
        Err(fs::TryLockError::WouldBlock) => return Err(StartError::WorldInUse(path)),
        Err(fs::TryLockError::Error(e)) => return Err(StartError::io(&path, e)),
    }
    // Vanilla writes a snowman into the lock file.
    file.set_len(0)
        .and_then(|()| file.write_all("\u{2603}".as_bytes()))
        .map_err(|e| StartError::io(&path, e))?;
    Ok(file)
}

fn spawn_world_thread(
    mut world: ServerWorld,
    compute: &Compute,
    global: &Arc<Global>,
    server: Sender<ServerMsg>,
    heartbeat: Arc<Heartbeat>,
) -> Result<WorldThread, StartError> {
    let (sender, receiver) = mpsc::channel::<WorldMsg>();
    let name = format!("World {}", world.dimension());
    let id = world.id();
    let thread_compute = compute.clone();
    let global = Arc::clone(global);
    let body = move || {
        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            world_loop(
                &mut world,
                &receiver,
                &server,
                &thread_compute,
                &global,
                &heartbeat,
            );
        }));
        if result.is_err() {
            let _ = server.send(ServerMsg::WorldCrashed(id));
        }
    };
    let handle = compute
        .scheduler()
        .spawn_thread(name, Box::new(body))
        .map_err(|e| StartError::Io(format!("cannot start a world thread: {e}")))?;
    Ok(WorldThread { sender, handle })
}

/// The loop of a world thread: ticks on command and runs the world's tasks in between.
fn world_loop(
    world: &mut ServerWorld,
    receiver: &Receiver<WorldMsg>,
    server: &Sender<ServerMsg>,
    compute: &Compute,
    global: &Global,
    heartbeat: &Heartbeat,
) {
    for msg in receiver {
        heartbeat.enter(&global.clock);
        match msg {
            WorldMsg::Tick {
                tick,
                runs_normally,
            } => {
                let mut outbox = Vec::new();
                let mut ctx = TickContext::new(
                    world.id(),
                    tick,
                    runs_normally,
                    compute,
                    global,
                    &mut outbox,
                );
                world.tick(&mut ctx);
                let _ = server.send(ServerMsg::WorldTicked(world.id(), outbox));
            }
            WorldMsg::Task(task) => task(world),
            WorldMsg::Stop => {
                world.save(true, false);
                heartbeat.leave();
                return;
            }
        }
        heartbeat.leave();
    }
}

/// A handle to a running server, usable from any thread (console, network, signals).
#[derive(Clone)]
pub struct ServerHandle {
    sender: Sender<ServerMsg>,
    routes: Arc<[Route]>,
}

impl fmt::Debug for ServerHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerHandle")
            .field("worlds", &self.routes.len())
            .finish()
    }
}

impl ServerHandle {
    /// Asks the server to stop after the current tick.
    pub fn stop(&self) {
        let _ = self.sender.send(ServerMsg::Stop);
    }

    /// Queues a console command for the next global pre-phase.
    pub fn console(&self, line: String) {
        let _ = self.sender.send(ServerMsg::Console(line));
    }

    /// Queues work for the next global pre-phase.
    pub fn run_global(&self, task: impl FnOnce(&mut Server) + Send + 'static) {
        let _ = self.sender.send(ServerMsg::Global(Box::new(task)));
    }

    /// Runs work on a world between its ticks. Returns `false` for an unknown world or a
    /// stopped server.
    pub fn run_in_world(
        &self,
        world: WorldId,
        task: impl FnOnce(&mut ServerWorld) + Send + 'static,
    ) -> bool {
        let task: WorldTask = Box::new(task);
        match self.routes.get(world.index()) {
            Some(Route::Server) => self.sender.send(ServerMsg::WorldTask(world, task)).is_ok(),
            Some(Route::World(sender)) => sender.send(WorldMsg::Task(task)).is_ok(),
            None => false,
        }
    }
}

/// A running server. [`Server::run`] runs the tick loop on the calling thread until stopped.
pub struct Server {
    dir: PathBuf,
    config: Config,
    compute: Compute,
    global: Arc<Global>,
    worlds: Worlds,
    routes: Arc<[Route]>,
    sender: Sender<ServerMsg>,
    receiver: Receiver<ServerMsg>,
    /// Console commands and global tasks waiting for the next global pre-phase.
    global_queue: VecDeque<ServerMsg>,
    tick_rate: TickRateManager,
    tick_count: u64,
    next_tick: u64,
    empty_ticks: u64,
    ticks_until_autosave: i64,
    average_tick_nanos: f64,
    /// Load test tick log (`VANILLITE_TICK_LOG`): one line with the duration in nanoseconds per
    /// tick. A stand-in for the metrics endpoint of architecture 3.8.
    tick_log: Option<io::BufWriter<File>>,
    running: bool,
    heartbeat: Arc<Heartbeat>,
    watch: Arc<WatchState>,
    watchdog: Option<Watchdog>,
    net: Option<NetState>,
    /// UUIDs of the test bots this server started (`debug.test-bots`).
    test_bots: Vec<u128>,
    layout: WorldLayout,
    /// Player data files, written in the background.
    player_store: PlayerDataStore,
    /// When the next automatic backup is due.
    next_backup: Instant,
    /// The thread of the running or last backup.
    backup: Option<JoinHandle<()>>,
    _session_lock: File,
}

impl fmt::Debug for Server {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Server")
            .field("dir", &self.dir)
            .field("tick_count", &self.tick_count)
            .field("running", &self.running)
            .finish_non_exhaustive()
    }
}

impl Server {
    /// Opens in-memory connections for vouched-for profiles (architecture 14.1), from any
    /// thread while the server runs; they log in like a client from the handshake on.
    pub fn virtual_opener(&self) -> Option<VirtualOpener> {
        self.net.as_ref().map(|n| n.network.virtual_opener())
    }

    /// The UDP address of the Bedrock frontend, if it runs.
    pub fn bedrock_addr(&self) -> Option<std::net::SocketAddr> {
        self.net
            .as_ref()?
            .bedrock
            .as_ref()
            .map(|(f, _)| f.local_addr())
    }

    /// The address the network listens on.
    pub fn local_addr(&self) -> Option<std::net::SocketAddr> {
        self.net.as_ref().map(|n| n.network.local_addr())
    }

    /// A handle for other threads.
    pub fn handle(&self) -> ServerHandle {
        ServerHandle {
            sender: self.sender.clone(),
            routes: Arc::clone(&self.routes),
        }
    }

    /// The loaded configuration.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Server-wide state.
    pub fn global(&self) -> &Global {
        &self.global
    }

    /// The compute pool.
    pub fn compute(&self) -> &Compute {
        &self.compute
    }

    /// The tick rate state (`/tick`).
    pub fn tick_rate(&mut self) -> &mut TickRateManager {
        &mut self.tick_rate
    }

    /// Ticks run since start.
    pub fn tick_count(&self) -> u64 {
        self.tick_count
    }

    /// The smoothed time of a server tick in milliseconds (MSPT).
    pub fn average_tick_millis(&self) -> f64 {
        self.average_tick_nanos / 1e6
    }

    /// Stops the server after the current tick.
    pub fn stop(&mut self) {
        self.running = false;
    }

    /// Runs the tick loop until the server is stopped, then shuts down.
    pub fn run(mut self) {
        let clock = self.global.clock;
        self.next_tick = clock.now();
        let mut last_overload_warning = self.next_tick;
        while self.running {
            let now = clock.now();
            let this_tick = if self.tick_rate.is_sprinting()
                && self.tick_rate.check_should_sprint_this_tick(now)
            {
                self.next_tick = now;
                last_overload_warning = now;
                0
            } else {
                let this_tick = self.tick_rate.nanos_per_tick();
                let behind = now.saturating_sub(self.next_tick);
                if behind > OVERLOADED_THRESHOLD + 20 * this_tick
                    && self.next_tick.saturating_sub(last_overload_warning)
                        >= OVERLOADED_WARNING_INTERVAL + 100 * this_tick
                {
                    let missed = behind / this_tick;
                    log::warn!(
                        "Can't keep up! Is the server overloaded? Running {}ms or {missed} ticks behind",
                        behind / 1_000_000
                    );
                    self.next_tick += missed * this_tick;
                    last_overload_warning = self.next_tick;
                }
                this_tick
            };
            let sprinting = this_tick == 0;
            self.next_tick += this_tick;
            self.watch
                .next_tick
                .store(self.next_tick, Ordering::Relaxed);
            self.tick_server();
            self.wait_until_next_tick(this_tick, sprinting);
            if sprinting {
                self.tick_rate.end_tick_work(clock.now());
            }
        }
        self.shutdown();
    }

    /// Vanilla's `tickServer`: pause check, global pre-phase, world phase, global post-phase.
    fn tick_server(&mut self) {
        let clock = self.global.clock;
        let started = clock.now();
        self.heartbeat.enter(&clock);

        let pause_seconds =
            u64::try_from(self.config.properties.pause_when_empty_seconds).unwrap_or(0);
        if pause_seconds > 0 {
            if self.global.players.count() == 0 && !self.tick_rate.is_sprinting() {
                self.empty_ticks += 1;
            } else {
                self.empty_ticks = 0;
            }
            let threshold = pause_seconds * 20;
            if self.empty_ticks >= threshold {
                if self.empty_ticks == threshold {
                    log::info!("Server empty for {pause_seconds} seconds, pausing");
                    self.autosave();
                }
                // The console stays responsive while paused, like Vanilla.
                self.global_pre_phase();
                self.heartbeat.leave();
                return;
            }
        }

        self.tick_count += 1;
        self.tick_rate.tick();
        self.global_pre_phase();
        self.tick_clocks();
        self.tick_worlds();
        self.global_post_phase();

        let elapsed = clock.now().saturating_sub(started) as f64;
        self.average_tick_nanos = self.average_tick_nanos * 0.8 + elapsed * 0.2;
        if let Some(log) = &mut self.tick_log {
            let written = writeln!(log, "{}", clock.now().saturating_sub(started));
            if written.is_err() || (self.tick_count.is_multiple_of(20) && log.flush().is_err()) {
                log::warn!("Cannot write the tick log, disabling it");
                self.tick_log = None;
            }
        }
        self.heartbeat.leave();
    }

    /// G1: console commands and global tasks, with `&mut` to everything because no world is
    /// ticking. Accepted joins (bounded by `security.max-joins-per-tick`) are added here once
    /// the network exists.
    fn global_pre_phase(&mut self) {
        while let Some(msg) = self.global_queue.pop_front() {
            match msg {
                ServerMsg::Console(line) => self.run_console_command(&line),
                ServerMsg::Global(task) => task(self),
                other => self.dispatch(other),
            }
        }
        self.accept_joins();
    }

    /// `tickChildren`'s clocks and the game time sync every second (by default).
    fn tick_clocks(&mut self) {
        let level = &self.global.level;
        if self.tick_rate.runs_normally() {
            let advance = level
                .rules()
                .bool(vanillite_game::survival::rules::Rule::ADVANCE_TIME);
            level.clocks().tick(advance);
        }
        let interval = u64::from(self.config.server.survival.time_sync_interval.max(1));
        if self.tick_count.is_multiple_of(interval) {
            crate::survival::broadcast_all(&self.global, &crate::survival::game_time_sync(level));
        }
    }

    /// Takes joined connections into the game, at most `security.max-joins-per-tick`.
    fn accept_joins(&mut self) {
        let max = self.config.server.security.max_joins_per_tick;
        for _ in 0..max {
            let Some(joined) = self.net.as_ref().and_then(NetState::next_join) else {
                break;
            };
            self.accept(joined);
        }
    }

    /// `PlayerList.placeNewPlayer`, global part: checks, player data, the player list; the
    /// world part runs on the player's world.
    fn accept(&mut self, joined: vanillite_net::Joined) {
        let uuid = joined.profile.id.0;
        let saved = match self.player_store.load(uuid) {
            Ok(saved) => saved,
            Err(e) => {
                log::error!("Failed to load player data of {}: {e}", joined.profile.name);
                None
            }
        };
        let properties = &self.config.properties;
        let (arrival, sink) = crate::net::arrival(
            joined,
            saved,
            properties.gamemode,
            properties.force_gamemode,
        );
        let players = &self.global.players;
        let access = &self.global.access;
        // The server's own test bots come over virtual connections it opened itself.
        let test_bot = arrival.memory_connection && self.test_bots.contains(&uuid);
        let refuse = if !test_bot && !access.is_whitelisted(uuid) {
            Some("multiplayer.disconnect.not_whitelisted")
        } else if players.contains(Uuid(uuid)) {
            // Vanilla kicks the older session; refusing the new one keeps a single owner of the
            // player without a cross-world removal.
            Some("multiplayer.disconnect.duplicate_login")
        } else if players.count() >= players.max_players() && !access.bypasses_player_limit(uuid) {
            Some("multiplayer.disconnect.server_full")
        } else {
            None
        };
        if let Some(key) = refuse {
            sink.kick(vanillite_protocol::text::Component::translatable(
                key,
                vec![],
            ));
            return;
        }
        let world = arrival
            .saved
            .as_ref()
            .and_then(|s| DIMENSIONS.iter().position(|d| *d == s.dimension))
            .and_then(|i| u16::try_from(i).ok())
            .map_or(WorldId(0), WorldId);
        players.add(PlayerEntry {
            uuid: Uuid(uuid),
            name: arrival.profile.name.clone(),
            world,
            info: crate::players::info_entry(&arrival),
            sink,
            inbox: Arc::clone(&arrival.inbox),
        });
        let global = Arc::clone(&self.global);
        self.run_in_world(
            world,
            Box::new(move |w: &mut ServerWorld| w.join_player(&global, arrival)),
        );
    }

    /// The world phase and the cross-world hand-offs of G2, applied in world order.
    fn tick_worlds(&mut self) {
        let tick = self.tick_count;
        let runs_normally = self.tick_rate.runs_normally();
        let mut outboxes: Vec<Vec<(WorldId, WorldTask)>> =
            self.routes.iter().map(|_| Vec::new()).collect();
        match &mut self.worlds {
            Worlds::Sequential(worlds) => {
                for (index, outbox) in outboxes.iter_mut().enumerate() {
                    let (before, rest) = worlds.split_at_mut(index);
                    let Some((world, after)) = rest.split_first_mut() else {
                        break;
                    };
                    let mut ctx = TickContext::new(
                        world.id(),
                        tick,
                        runs_normally,
                        &self.compute,
                        &self.global,
                        outbox,
                    )
                    .with_others(before, after);
                    world.tick(&mut ctx);
                }
            }
            Worlds::Parallel(threads) => {
                for thread in threads.iter() {
                    let _ = thread.sender.send(WorldMsg::Tick {
                        tick,
                        runs_normally,
                    });
                }
                let mut remaining = threads.len();
                while remaining > 0 {
                    let Ok(msg) = self.receiver.recv() else {
                        break;
                    };
                    match msg {
                        ServerMsg::WorldTicked(id, outbox) => {
                            if let Some(slot) = outboxes.get_mut(id.index()) {
                                *slot = outbox;
                            }
                            remaining -= 1;
                        }
                        ServerMsg::WorldCrashed(id) => {
                            self.world_crashed(id);
                            remaining -= 1;
                        }
                        other => self.global_queue.push_back(other),
                    }
                }
            }
        }
        for (target, op) in outboxes.into_iter().flatten() {
            self.run_in_world(target, op);
        }
    }

    /// G2: player list and autosave.
    fn global_post_phase(&mut self) {
        let online = self.global.players.count();
        if let Some(net) = self.net.as_mut() {
            net.set_online(online);
        }
        // Player list maintenance (latency updates, idle kicks) comes with players.
        self.ticks_until_autosave -= 1;
        if self.ticks_until_autosave <= 0 {
            self.autosave();
        }
        if self.config.server.backup.interval.get().is_some() && Instant::now() >= self.next_backup
        {
            self.next_backup = self.backup_due_after();
            self.start_backup();
        }
    }

    /// Runs messages between ticks until the next tick is due (Vanilla's `waitUntilNextTick`:
    /// when the tick ran late, queued work may still use up to one tick length).
    fn wait_until_next_tick(&mut self, this_tick: u64, sprinting: bool) {
        let clock = self.global.clock;
        if sprinting {
            while let Ok(msg) = self.receiver.try_recv() {
                self.dispatch(msg);
            }
            return;
        }
        let task_deadline = (clock.now() + this_tick).max(self.next_tick);
        let mut may_have_delayed = true;
        while self.running {
            let now = clock.now();
            let limit = if may_have_delayed {
                task_deadline
            } else {
                self.next_tick
            };
            if now >= limit {
                break;
            }
            match self.receiver.try_recv() {
                Ok(msg) => self.dispatch(msg),
                Err(TryRecvError::Disconnected) => break,
                Err(TryRecvError::Empty) => {
                    may_have_delayed = false;
                    if now >= self.next_tick {
                        break;
                    }
                    match self
                        .receiver
                        .recv_timeout(Duration::from_nanos(self.next_tick - now))
                    {
                        Ok(msg) => self.dispatch(msg),
                        Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
                    }
                }
            }
        }
    }

    /// Handles a message outside the world phase.
    fn dispatch(&mut self, msg: ServerMsg) {
        match msg {
            ServerMsg::Stop => self.running = false,
            ServerMsg::Console(_) | ServerMsg::Global(_) => self.global_queue.push_back(msg),
            ServerMsg::WorldTask(id, task) => self.run_in_world(id, task),
            ServerMsg::WorldCrashed(id) => self.world_crashed(id),
            ServerMsg::WorldTicked(id, _) => {
                log::warn!("Unexpected tick completion of world {}", id.0);
            }
        }
    }

    fn world_crashed(&mut self, id: WorldId) {
        log::error!(
            "The thread of world {} crashed; stopping the server",
            DIMENSIONS.get(id.index()).copied().unwrap_or("?")
        );
        self.running = false;
    }

    /// Runs a task on a world: directly in sequential mode, between its ticks on its thread in
    /// parallel mode.
    fn run_in_world(&mut self, id: WorldId, task: WorldTask) {
        match &mut self.worlds {
            Worlds::Sequential(worlds) => match worlds.get_mut(id.index()) {
                Some(world) => task(world),
                None => log::warn!("Task for unknown world {} dropped", id.0),
            },
            Worlds::Parallel(threads) => match threads.get(id.index()) {
                Some(thread) => {
                    if thread.sender.send(WorldMsg::Task(task)).is_err() {
                        log::warn!("Task for stopped world {} dropped", id.0);
                    }
                }
                None => log::warn!("Task for unknown world {} dropped", id.0),
            },
        }
    }

    /// A console command: `vanillite reload config` here, everything else through the command
    /// tree on the overworld.
    fn run_console_command(&mut self, line: &str) {
        let command = line.trim().trim_start_matches('/');
        if command == "vanillite reload config" {
            self.reload_config();
            return;
        }
        let global = Arc::clone(&self.global);
        let line = line.to_owned();
        self.run_in_world(
            WorldId(0),
            Box::new(move |w: &mut ServerWorld| {
                crate::commands::run_console_command(w, &global, &line);
            }),
        );
    }

    /// Reloads `vanillite.toml` and swaps the rules at this tick boundary. `server.properties`
    /// is read at startup only, like Vanilla.
    fn reload_config(&mut self) {
        let mut new = match Config::load(&self.dir) {
            Ok(config) => config,
            Err(e) => {
                log::error!("Reloading {VANILLITE_FILE} failed, keeping the old values: {e}");
                return;
            }
        };
        new.properties = self.config.properties.clone();
        warn_restart_needed(&self.config.server, &new.server);
        for dimension in DIMENSIONS {
            warn_restart_needed(
                &self.config.world_rules(dimension),
                &new.world_rules(dimension),
            );
        }
        self.watch.configure(
            new.server.watchdog.early_warning_after.get(),
            new.server.watchdog.warning_interval,
        );
        for (dimension, index) in DIMENSIONS.iter().zip(0u16..) {
            let rules = Arc::new(new.world_rules(dimension));
            let max_chained = new.properties.max_chained_neighbor_updates;
            self.run_in_world(
                WorldId(index),
                Box::new(move |w| w.set_rules(rules, max_chained)),
            );
        }
        self.config = new;
        log::info!("Reloaded {VANILLITE_FILE}");
    }

    /// Ticks until the next autosave (Vanilla's `computeNextAutosaveInterval`).
    fn autosave_interval(&self) -> i64 {
        match self.config.server.autosave.interval {
            VanillaOr::Value(ticks) => i64::from(ticks),
            VanillaOr::Vanilla => {
                let rate = if self.tick_rate.is_sprinting() {
                    1e9 / (self.average_tick_nanos as f32 + 1.0)
                } else {
                    self.tick_rate.tick_rate()
                };
                ((rate * AUTOSAVE_SECONDS) as i64).max(100)
            }
        }
    }

    fn autosave(&mut self) {
        self.ticks_until_autosave = self.autosave_interval();
        log::debug!("Autosave started");
        self.save_everything(false, false, None);
    }

    /// Vanilla's `saveEverything`: the players, `level.dat`, the saved data and the chunks of
    /// every world; after `/save-off` the chunks only when `force`d. Each world saves between
    /// its ticks; files are written in the background. `then` runs once every world has saved
    /// (with `flush`: once everything is on disk).
    pub(crate) fn save_everything(&mut self, flush: bool, force: bool, then: Option<AfterSave>) {
        let skip_chunks = !force && !self.global.level.auto_save.load(Ordering::Relaxed);
        self.global.level.save();
        let done = Arc::new(SaveDone {
            remaining: AtomicUsize::new(self.routes.len()),
            writer: flush.then(|| self.global.level.writer().clone()),
            then: Mutex::new(then),
        });
        for index in 0..self.routes.len() {
            let Ok(id) = u16::try_from(index).map(WorldId) else {
                break;
            };
            let done = Arc::clone(&done);
            self.run_in_world(
                id,
                Box::new(move |w| {
                    w.save(flush, skip_chunks);
                    done.world_saved();
                }),
            );
        }
    }

    /// When the backup after one starting now is due.
    fn backup_due_after(&self) -> Instant {
        let minutes = self.config.server.backup.interval.get().unwrap_or(0);
        Instant::now() + Duration::from_secs(u64::from(minutes) * 60)
    }

    /// Saves everything and archives the world on a background thread once the saves are
    /// written (`backup.*`).
    fn start_backup(&mut self) {
        if self.backup.as_ref().is_some_and(|b| !b.is_finished()) {
            log::warn!("Skipping a backup: the previous one is still running");
            return;
        }
        let rules = &self.config.server.backup;
        let settings = BackupSettings {
            directory: self.dir.join(&rules.directory),
            keep: rules.keep,
            compression_level: rules.compression_level,
        };
        let level_name = self.config.properties.level_name.clone();
        let mut sources: Vec<PathBuf> = Vec::new();
        for dimension in DIMENSIONS {
            let dir = self.layout.world_dir(&self.dir, &level_name, dimension);
            if !sources.contains(&dir) {
                sources.push(dir);
            }
        }
        let (saved, wait) = mpsc::channel::<()>();
        self.save_everything(
            false,
            false,
            Some(Box::new(move || {
                let _ = saved.send(());
            })),
        );
        let writer = self.global.level.writer().clone();
        let thread = std::thread::Builder::new()
            .name("Backup".to_owned())
            .spawn(move || {
                // A crashed world drops the sender; the backup is made anyway.
                let _ = wait.recv();
                writer.flush();
                log::info!("Backing up level '{level_name}'");
                let started = Instant::now();
                match vanillite_storage::backup::create_backup(&level_name, &sources, &settings) {
                    Ok(path) => log::info!(
                        "Backup saved to {} ({:.1}s)",
                        path.display(),
                        started.elapsed().as_secs_f64()
                    ),
                    Err(e) => log::error!("Backup of level '{level_name}' failed: {e}"),
                }
            });
        match thread {
            Ok(thread) => self.backup = Some(thread),
            Err(e) => log::error!("Cannot start the backup thread: {e}"),
        }
    }

    /// Vanilla's `stopServer`: save everything with flush, stop the threads, release the world.
    fn shutdown(mut self) {
        // Saving may take long; the watchdog must not kill the process now.
        self.watch.next_tick.store(0, Ordering::Relaxed);
        log::info!("Stopping server");
        for player in self.global.players.snapshot() {
            player
                .sink
                .kick(vanillite_protocol::text::Component::translatable(
                    "multiplayer.disconnect.server_shutdown",
                    vec![],
                ));
        }
        log::info!("Saving players");
        log::info!("Saving worlds");
        let level = &self.config.properties.level_name;
        match self.worlds {
            Worlds::Sequential(mut worlds) => {
                for world in &mut worlds {
                    log::info!("Saving chunks for level '{level}'/{}", world.dimension());
                    world.save(true, false);
                }
            }
            Worlds::Parallel(threads) => {
                for (thread, dimension) in threads.iter().zip(DIMENSIONS) {
                    log::info!("Saving chunks for level '{level}'/{dimension}");
                    let _ = thread.sender.send(WorldMsg::Stop);
                }
                for thread in threads {
                    let _ = thread.handle.join();
                }
            }
        }
        self.global.level.save();
        self.global.level.writer().flush();
        log::info!("All dimensions are saved");
        if let Some(backup) = self.backup.take()
            && !backup.is_finished()
        {
            log::info!("Waiting for the running backup to finish");
            let _ = backup.join();
        }
        if let Some(net) = self.net.take() {
            net.shutdown();
        }
        self.compute.scheduler().shutdown();
        if let Some(watchdog) = self.watchdog.take() {
            watchdog.stop();
        }
    }
}

fn warn_restart_needed<R: RuleSet>(old: &R, new: &R) {
    for info in R::RULES.iter().filter(|r| r.restart) {
        if old.get(info.key) != new.get(info.key) {
            log::warn!(
                "{} changed; the new value takes effect after a restart",
                info.key
            );
        }
    }
}

/// The server could not start.
#[derive(Debug)]
pub enum StartError {
    /// A configuration file is missing or invalid.
    Config(ConfigError),
    /// A file or thread operation failed.
    Io(String),
    /// Another server holds the world's `session.lock`.
    WorldInUse(PathBuf),
    /// A slot was filled twice.
    Slot(SlotError),
    /// The EULA is not accepted in `eula.txt`.
    Eula,
}

impl StartError {
    pub(crate) fn io(path: &Path, error: io::Error) -> StartError {
        StartError::Io(format!("{}: {error}", path.display()))
    }
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StartError::Config(e) => write!(f, "invalid configuration: {e}"),
            StartError::Io(e) => f.write_str(e),
            StartError::WorldInUse(path) => write!(
                f,
                "the world is in use by another server ({} is locked)",
                path.display()
            ),
            StartError::Slot(e) => e.fmt(f),
            StartError::Eula => f.write_str(
                "you need to agree to the EULA in order to run the server; go to eula.txt for \
                 more info (or set VANILLITE_EULA=true)",
            ),
        }
    }
}

impl std::error::Error for StartError {}

impl From<ConfigError> for StartError {
    fn from(e: ConfigError) -> Self {
        StartError::Config(e)
    }
}

impl From<SlotError> for StartError {
    fn from(e: SlotError) -> Self {
        StartError::Slot(e)
    }
}

/// Opens the tick log named by `VANILLITE_TICK_LOG` (relative to the working directory), if set.
fn open_tick_log() -> Option<io::BufWriter<File>> {
    let path = std::env::var_os("VANILLITE_TICK_LOG")?;
    match File::create(&path) {
        Ok(file) => Some(io::BufWriter::new(file)),
        Err(e) => {
            log::warn!(
                "Cannot create the tick log {}: {e}",
                Path::new(&path).display()
            );
            None
        }
    }
}
