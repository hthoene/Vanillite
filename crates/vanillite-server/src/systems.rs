//! Server-wide game systems around the simulation: the scoreboard, command storage, data pack
//! functions and advancements, scheduled functions, random sequences, stopwatches and the
//! profilers. Each sits behind its own lock, held only for one command's change. The
//! scoreboard, the command storage and the scheduled functions are the world's saved data.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};

use vanillite_base::Vec3;
use vanillite_game::advancements::Advancements;
use vanillite_game::scoreboard::Scoreboard;
use vanillite_nbt::{Compound, Tag};
use vanillite_storage::saved_data::SavedDataStore;

use crate::command_state::{Profiling, RandomSequences, Stopwatches, TimerQueue, WaypointIcon};
use crate::commands::functions::FunctionLibrary;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The saved data of the scoreboard (`ScoreboardSaveData.TYPE`).
const SCOREBOARD: &str = "minecraft:scoreboard";
/// The saved data of the scheduled functions (`TimerQueue.TYPE`).
const SCHEDULED_EVENTS: &str = "minecraft:scheduled_events";
/// The path of a namespace's command storage (`CommandStorage.Container.type`).
const COMMAND_STORAGE: &str = "command_storage";

/// `CommandStorage`: compounds by id, saved per namespace as `<namespace>:command_storage`.
#[derive(Debug, Default)]
pub(crate) struct CommandStorage {
    entries: BTreeMap<String, Compound>,
    /// Namespaces changed since the last save.
    dirty: BTreeSet<String>,
}

fn split_id(id: &str) -> (&str, &str) {
    id.split_once(':').unwrap_or(("minecraft", id))
}

impl CommandStorage {
    /// `get`: the compound of an id, if any.
    pub(crate) fn get(&self, id: &str) -> Option<&Compound> {
        self.entries.get(id)
    }

    /// `set`: an empty compound removes the id.
    pub(crate) fn set(&mut self, id: String, data: Compound) {
        self.dirty.insert(split_id(&id).0.to_owned());
        if data.is_empty() {
            self.entries.remove(&id);
        } else {
            self.entries.insert(id, data);
        }
    }

    /// `keys`.
    pub(crate) fn keys(&self) -> impl Iterator<Item = &String> {
        self.entries.keys()
    }

    /// Every `<namespace>:command_storage` below `data/` of the world.
    fn load(data: &SavedDataStore) -> CommandStorage {
        let mut storage = CommandStorage::default();
        let Ok(dirs) = std::fs::read_dir(data.dir().join("data")) else {
            return storage;
        };
        for dir in dirs.flatten() {
            let Ok(namespace) = dir.file_name().into_string() else {
                continue;
            };
            let id = format!("{namespace}:{COMMAND_STORAGE}");
            let contents = match data.read(&id) {
                Ok(Some(mut c)) => match c.remove("contents") {
                    Some(Tag::Compound(contents)) => contents,
                    _ => continue,
                },
                Ok(None) => continue,
                Err(e) => {
                    log::error!("Cannot read {id}: {e}");
                    continue;
                }
            };
            for (path, tag) in contents.iter() {
                if let Tag::Compound(c) = tag {
                    storage
                        .entries
                        .insert(format!("{namespace}:{path}"), c.clone());
                }
            }
        }
        storage
    }

    /// The changed namespaces' `Container.CODEC` data, as saved data ids.
    fn take_dirty(&mut self) -> Vec<(String, Compound)> {
        std::mem::take(&mut self.dirty)
            .into_iter()
            .map(|namespace| {
                let mut contents = Compound::new();
                for (id, data) in &self.entries {
                    let (ns, path) = split_id(id);
                    if ns == namespace {
                        contents.insert(path, data.clone());
                    }
                }
                let data = Compound::new().with("contents", contents.in_java_hash_order());
                (format!("{namespace}:{COMMAND_STORAGE}"), data)
            })
            .collect()
    }
}

/// The game systems of the server.
pub(crate) struct GameSystems {
    scoreboard: Mutex<Scoreboard>,
    /// What the clients know of the scoreboard; locked before the scoreboard.
    scoreboard_sync: Mutex<crate::scoreboard::ScoreboardSync>,
    storage: Mutex<CommandStorage>,
    functions: RwLock<Arc<FunctionLibrary>>,
    advancements: RwLock<Arc<Advancements>>,
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
            scoreboard_sync: Mutex::default(),
            storage: Mutex::default(),
            functions: RwLock::new(Arc::new(FunctionLibrary::default())),
            advancements: RwLock::default(),
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

    /// The scoreboard as the clients know it ([`crate::scoreboard::flush`]).
    pub(crate) fn scoreboard_sync(&self) -> MutexGuard<'_, crate::scoreboard::ScoreboardSync> {
        lock(&self.scoreboard_sync)
    }

    /// Loads the scoreboard, the command storage and the scheduled functions from the world's
    /// saved data (`ServerScoreboard.load`, `CommandStorage`, `TimerQueue`).
    pub(crate) fn load(&self, data: &SavedDataStore) {
        let read = |id: &str| {
            data.read(id).unwrap_or_else(|e| {
                log::error!("Cannot read {id}: {e}");
                None
            })
        };
        if let Some(c) = read(SCOREBOARD) {
            *self.scoreboard() = Scoreboard::from_nbt(&c);
        }
        if let Some(c) = read(SCHEDULED_EVENTS) {
            *self.scheduled() = TimerQueue::from_nbt(&c);
        }
        *self.storage() = CommandStorage::load(data);
    }

    /// Writes what changed since the last save (`storeToSaveDataIfDirty`, `setDirty`).
    pub(crate) fn save(&self, data: &SavedDataStore) {
        let scoreboard = {
            let mut board = self.scoreboard();
            board.take_dirty().then(|| board.to_nbt())
        };
        if let Some(c) = scoreboard {
            data.write(SCOREBOARD, c);
        }
        let scheduled = {
            let mut queue = self.scheduled();
            queue.take_dirty().then(|| queue.to_nbt())
        };
        if let Some(c) = scheduled {
            data.write(SCHEDULED_EVENTS, c);
        }
        for (id, c) in self.storage().take_dirty() {
            data.write(&id, c);
        }
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

    /// The advancements (`ServerAdvancementManager`).
    pub(crate) fn advancements(&self) -> Arc<Advancements> {
        Arc::clone(
            &self
                .advancements
                .read()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }

    /// Replaces the advancements; players that join afterwards use them.
    pub(crate) fn set_advancements(&self, advancements: Advancements) {
        *self
            .advancements
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Arc::new(advancements);
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
