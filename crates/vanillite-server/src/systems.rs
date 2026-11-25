//! Server-wide game systems around the simulation: the scoreboard, command storage, data pack
//! functions, scheduled functions, random sequences, stopwatches and the profilers. Each sits
//! behind its own lock, held only for one command's change.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};

use vanillite_base::Vec3;
use vanillite_game::scoreboard::Scoreboard;
use vanillite_nbt::Compound;

use crate::command_state::{Profiling, RandomSequences, Stopwatches, TimerQueue, WaypointIcon};
use crate::commands::functions::FunctionLibrary;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `CommandStorage`: compounds by id.
pub(crate) type CommandStorage = std::collections::BTreeMap<String, Compound>;

/// The game systems of the server.
pub(crate) struct GameSystems {
    scoreboard: Mutex<Scoreboard>,
    storage: Mutex<CommandStorage>,
    functions: RwLock<Arc<FunctionLibrary>>,
    post_reload: AtomicBool,
    /// `function-permission-level`.
    pub(crate) function_level: u8,
    /// The world directory.
    pub(crate) level_dir: PathBuf,
    spawn: Mutex<Vec3>,
    /// Tags of the static and dynamic registries: registry → tag → entries.
    tags: RwLock<Arc<RegistryTags>>,
    scheduled: Mutex<TimerQueue>,
    random_sequences: Mutex<RandomSequences>,
    stopwatches: Mutex<Stopwatches>,
    profiling: Mutex<Profiling>,
    waypoints: Mutex<std::collections::BTreeMap<u128, WaypointIcon>>,
}

/// Registry tags: registry name → tag id → resolved entry ids.
pub(crate) type RegistryTags =
    std::collections::BTreeMap<String, std::collections::BTreeMap<String, Vec<String>>>;

impl std::fmt::Debug for GameSystems {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GameSystems").finish_non_exhaustive()
    }
}

impl GameSystems {
    /// Empty systems; [`crate::commands::load_game_systems`] fills them once the server exists.
    pub(crate) fn new(level_dir: PathBuf, function_level: u8) -> Self {
        GameSystems {
            scoreboard: Mutex::new(Scoreboard::new()),
            storage: Mutex::new(CommandStorage::new()),
            functions: RwLock::new(Arc::new(FunctionLibrary::default())),
            post_reload: AtomicBool::new(false),
            function_level,
            level_dir,
            spawn: Mutex::new(Vec3::ZERO),
            tags: RwLock::new(Arc::new(RegistryTags::new())),
            scheduled: Mutex::default(),
            random_sequences: Mutex::default(),
            stopwatches: Mutex::default(),
            profiling: Mutex::default(),
            waypoints: Mutex::default(),
        }
    }

    /// Scheduled functions (`MinecraftServer.getScheduledEvents`).
    pub(crate) fn scheduled(&self) -> MutexGuard<'_, TimerQueue> {
        lock(&self.scheduled)
    }

    /// Random sequences (`MinecraftServer.getRandomSequences`).
    pub(crate) fn random_sequences(&self) -> MutexGuard<'_, RandomSequences> {
        lock(&self.random_sequences)
    }

    /// Stopwatches (`MinecraftServer.getStopwatches`).
    pub(crate) fn stopwatches(&self) -> MutexGuard<'_, Stopwatches> {
        lock(&self.stopwatches)
    }

    /// The profilers of `debug`, `perf` and `jfr`.
    pub(crate) fn profiling(&self) -> MutexGuard<'_, Profiling> {
        lock(&self.profiling)
    }

    /// Waypoint icons changed by `/waypoint`, by entity UUID.
    pub(crate) fn waypoints(
        &self,
    ) -> MutexGuard<'_, std::collections::BTreeMap<u128, WaypointIcon>> {
        lock(&self.waypoints)
    }

    /// The registry tags.
    pub(crate) fn tags(&self) -> Arc<RegistryTags> {
        Arc::clone(&self.tags.read().unwrap_or_else(PoisonError::into_inner))
    }

    /// Replaces the registry tags.
    pub(crate) fn set_tags(&self, tags: RegistryTags) {
        *self.tags.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(tags);
    }

    /// Whether `entry` is in `#tag` of `registry`.
    pub(crate) fn in_tag(&self, registry: &str, tag: &str, entry: &str) -> bool {
        self.tags()
            .get(registry)
            .and_then(|t| t.get(tag))
            .is_some_and(|e| e.iter().any(|e| e == entry))
    }

    /// The scoreboard.
    pub(crate) fn scoreboard(&self) -> MutexGuard<'_, Scoreboard> {
        self.scoreboard
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Command storage.
    pub(crate) fn storage(&self) -> MutexGuard<'_, CommandStorage> {
        self.storage.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The function library.
    pub(crate) fn functions(&self) -> Arc<FunctionLibrary> {
        Arc::clone(
            &self
                .functions
                .read()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }

    /// Replaces the function library; the load functions run on the next tick.
    pub(crate) fn set_functions(&self, library: FunctionLibrary) {
        *self
            .functions
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Arc::new(library);
        self.post_reload.store(true, Ordering::Release);
    }

    /// Whether the load functions are due; clears the flag.
    pub(crate) fn take_post_reload(&self) -> bool {
        self.post_reload.swap(false, Ordering::AcqRel)
    }

    /// Whether a command boss bar exists (`CustomBossEvents.get`); boss bars are not kept yet.
    pub(crate) fn bossbar_exists(&self, _id: &str) -> bool {
        false
    }

    /// Sets a command boss bar's value or maximum (`execute store bossbar`).
    pub(crate) fn set_bossbar_value(&self, _id: &str, _max: bool, _value: i32) {}

    /// The elapsed seconds of a stopwatch (`Stopwatches.get`, `Stopwatch.elapsedSeconds`).
    pub(crate) fn stopwatch_seconds(&self, id: &str) -> Option<f64> {
        let stopwatches = self.stopwatches();
        let now = stopwatches.now();
        stopwatches.get(id).map(|s| s.elapsed_seconds(now))
    }

    /// The overworld spawn (the server's command position).
    pub(crate) fn spawn(&self) -> Vec3 {
        *self.spawn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Sets the overworld spawn.
    pub(crate) fn set_spawn(&self, pos: Vec3) {
        *self.spawn.lock().unwrap_or_else(PoisonError::into_inner) = pos;
    }
}
