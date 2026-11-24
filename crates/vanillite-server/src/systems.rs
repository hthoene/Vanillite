//! Server-wide game systems around the simulation: the scoreboard, command storage, data pack
//! functions. Each sits behind its own lock, held only for one command's change.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};

use vanillite_base::Vec3;
use vanillite_game::scoreboard::Scoreboard;
use vanillite_nbt::Compound;

use crate::commands::functions::FunctionLibrary;

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
    /// The Vanilla data pack, when loaded.
    pub(crate) vanilla_pack: Option<PathBuf>,
    spawn: Mutex<Vec3>,
}

impl std::fmt::Debug for GameSystems {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GameSystems").finish_non_exhaustive()
    }
}

impl GameSystems {
    /// Empty systems; [`crate::commands::load_game_systems`] fills them once the server exists.
    pub(crate) fn new(level_dir: PathBuf, vanilla_pack: Option<PathBuf>, function_level: u8) -> Self {
        GameSystems {
            scoreboard: Mutex::new(Scoreboard::new()),
            storage: Mutex::new(CommandStorage::new()),
            functions: RwLock::new(Arc::new(FunctionLibrary::default())),
            post_reload: AtomicBool::new(false),
            function_level,
            level_dir,
            vanilla_pack,
            spawn: Mutex::new(Vec3::ZERO),
        }
    }

    /// The scoreboard.
    pub(crate) fn scoreboard(&self) -> MutexGuard<'_, Scoreboard> {
        self.scoreboard.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Command storage.
    pub(crate) fn storage(&self) -> MutexGuard<'_, CommandStorage> {
        self.storage.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The function library.
    pub(crate) fn functions(&self) -> Arc<FunctionLibrary> {
        Arc::clone(&self.functions.read().unwrap_or_else(PoisonError::into_inner))
    }

    /// Replaces the function library; the load functions run on the next tick.
    pub(crate) fn set_functions(&self, library: FunctionLibrary) {
        *self.functions.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(library);
        self.post_reload.store(true, Ordering::Release);
    }

    /// Whether the load functions are due; clears the flag.
    pub(crate) fn take_post_reload(&self) -> bool {
        self.post_reload.swap(false, Ordering::AcqRel)
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
