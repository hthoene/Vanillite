//! Native plugin host: loading and validation, event bus, handle tables, lifecycle, crash attribution and slot provider adapters.
//!
//! The server owns one [`PluginHost`]. It
//!
//! 1. discovers and validates the packages in `plugins/` and calls their `load` before the
//!    worlds exist ([`PluginHost::load`]), so plugins can register commands, events and slot
//!    providers;
//! 2. adds the plugins' commands to the command tree ([`PluginHost::register_commands`]) and
//!    hands their providers to the slots ([`PluginHost::take_chunk_generator`]);
//! 3. enables them once the server runs ([`PluginHost::enable_all`]), fires events at the
//!    server's hook points, runs scheduled tasks on their owners, and disables them at
//!    shutdown.
//!
//! The server is reached through two narrow traits it implements: [`ServerAccess`]
//! (thread-safe, server-wide) and [`WorldAccess`] (one world, borrowed by its owner for a
//! call). Everything that crosses into plugin code goes through one guard: the plugin
//! is recorded for crash reports, serialized plugins are locked, re-entrant calls are refused,
//! and a contained panic disables the plugin.

mod api;
mod commands;
pub mod crash;
mod events;
mod generator;
pub mod loader;
mod scheduler;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, RwLock, Weak};

use vanillite_abi::{self as abi, Status, status};

pub use commands::{Arg, ArgValue, SUGGEST_PREFIX, Sender};
pub use generator::PluginGeneratorProvider;
pub use loader::{Manifest, Threading};

/// A message for players.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Text<'a> {
    /// Plain text.
    Plain(&'a str),
    /// A JSON text component.
    Json(&'a str),
}

/// Server-wide services for plugins, callable from any thread.
pub trait ServerAccess: Send + Sync {
    /// UUIDs of the online players.
    fn online_players(&self) -> Vec<u128>;
    /// A player's name.
    fn player_name(&self, player: u128) -> Option<String>;
    /// A player's world index.
    fn player_world(&self, player: u128) -> Option<u32>;
    /// Sends a system message; `false` when the player is offline.
    fn send_message(&self, player: u128, text: Text<'_>) -> bool;
    /// Disconnects a player; `false` when offline.
    fn kick(&self, player: u128, reason: Text<'_>) -> bool;
    /// Sets the header and footer of a player's player list; `false` when offline.
    fn set_tab_list(&self, player: u128, header: Text<'_>, footer: Text<'_>) -> bool;
    /// Sends a system message to everyone and the log.
    fn broadcast(&self, text: Text<'_>);
    /// Whether a player is an operator.
    fn is_operator(&self, player: u128) -> bool;
    /// The dimension key of a world.
    fn world_name(&self, world: u32) -> Option<String>;
    /// The numeric id of a block state text.
    fn block_state_id(&self, state: &str) -> Option<u32>;
    /// A `vanillite.toml` rule as TOML text.
    fn rule(&self, key: &str) -> Option<String>;
    /// Queues a console command.
    fn run_command(&self, line: &str);
    /// Runs a job on a background thread.
    fn spawn_background(&self, job: Box<dyn FnOnce() + Send>);
    /// Saves and stops the server (a provider failed).
    fn shutdown(&self, reason: &str);
}

/// One world, borrowed by its owner for the duration of a plugin call.
pub trait WorldAccess {
    /// The world's index.
    fn id(&self) -> u32;
    /// The block state at a position as text; `None` outside loaded chunks.
    fn block(&self, pos: [i32; 3]) -> Option<String>;
    /// Sets a block state from text with Vanilla's updates.
    fn set_block(&mut self, pos: [i32; 3], state: &str) -> Result<(), Status>;
    /// A player's position in this world.
    fn player_position(&self, player: u128) -> Option<[f64; 3]>;
    /// Whether an entity is in this world.
    fn has_entity(&self, entity: u128) -> bool;
}

/// Settings of the host (`plugins.*` in `vanillite.toml`).
#[derive(Clone, Debug, Default)]
pub struct Settings {
    /// Load plugins at all.
    pub enabled: bool,
    /// Skip a plugin that crashed the server at the last run (same build).
    pub safe_mode_after_crash: bool,
    /// Development mode: `vanillite plugin reload <id>` loads a new build at runtime.
    pub development: bool,
    /// Pinned SHA-256 hashes: `id=hex,id=hex`.
    pub pins: String,
    /// The server's data version (providers and plugins built for another one are refused).
    pub data_version: i32,
}

const LOADING: u8 = 0;
const RUNNING: u8 = 1;

/// A pointer that may cross threads (plugin user data; the plugin guarantees its use).
#[derive(Clone, Copy, Debug)]
pub(crate) struct SendPtr(pub(crate) *mut std::ffi::c_void);

// SAFETY: user data belongs to the plugin, which declares how it may be used (threading mode).
unsafe impl Send for SendPtr {}
// SAFETY: as above.
unsafe impl Sync for SendPtr {}

/// A loaded plugin. Never unloaded: its library stays mapped until the process ends.
pub(crate) struct Plugin {
    pub(crate) manifest: Manifest,
    pub(crate) local: u32,
    pub(crate) abi_id: abi::PluginId,
    pub(crate) data_dir: PathBuf,
    pub(crate) sha256: String,
    pub(crate) info: abi::PluginInfo,
    pub(crate) tag: &'static crash::Tag,
    serialized: Option<Mutex<()>>,
    pub(crate) enabled: AtomicBool,
    pub(crate) config: Mutex<Option<toml::Table>>,
}

// SAFETY: `info` points to static data and functions of the plugin's library, which is never
// unloaded.
unsafe impl Send for Plugin {}
// SAFETY: as above; mutable state is behind atomics and locks.
unsafe impl Sync for Plugin {}

impl std::fmt::Debug for Plugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Plugin")
            .field("id", &self.manifest.id)
            .field("version", &self.manifest.version)
            .finish_non_exhaustive()
    }
}

impl Plugin {
    /// The plugin's id.
    pub(crate) fn id(&self) -> &str {
        &self.manifest.id
    }

    /// Calls into the plugin: records it for crash reports, holds its lock in serialized mode,
    /// refuses re-entrant calls and disables it when it panicked. `what` names the call in logs.
    pub(crate) fn call(&self, what: &str, f: impl FnOnce() -> Status) -> Status {
        if !self.enabled.load(Ordering::Acquire) {
            return status::DISABLED;
        }
        self.call_always(what, f)
    }

    /// [`Plugin::call`] also for a disabled plugin (cleanup: `disable`, dropping user data).
    pub(crate) fn call_always(&self, what: &str, f: impl FnOnce() -> Status) -> Status {
        let Some(_entered) = crash::enter(self.tag) else {
            log::warn!(
                "Plugin {} was called again while in a call ({what}); the call is dropped",
                self.id()
            );
            return status::WRONG_PHASE;
        };
        let _lock = self
            .serialized
            .as_ref()
            .map(|m| m.lock().unwrap_or_else(PoisonError::into_inner));
        let result = f();
        if result == status::PANICKED && self.enabled.swap(false, Ordering::AcqRel) {
            log::error!(
                "Plugin {} panicked in {what} and is disabled; the server keeps running",
                self.id()
            );
        }
        result
    }

    /// Calls a provider of the plugin: attributed like any call, but without the plugin lock
    /// (providers are thread-safe by contract) and regardless of the enabled flag.
    pub(crate) fn call_provider(&self, f: impl FnOnce() -> Status) -> Status {
        let _entered = crash::enter(self.tag);
        f()
    }
}

/// An event subscription.
#[derive(Debug)]
pub(crate) struct Subscriber {
    pub(crate) plugin: u32,
    pub(crate) priority: i32,
    pub(crate) callback: abi::EventFn,
    pub(crate) user: SendPtr,
}

/// The permission provider.
#[derive(Debug)]
pub(crate) struct PermissionProvider {
    pub(crate) plugin: u32,
    pub(crate) check: abi::PermissionFn,
    pub(crate) user: SendPtr,
}

/// State of one host, shared with the C functions through [`HOSTS`].
pub(crate) struct Inner {
    pub(crate) index: u16,
    pub(crate) dir: PathBuf,
    pub(crate) settings: Settings,
    pub(crate) plugins: RwLock<Vec<Arc<Plugin>>>,
    phase: AtomicU8,
    /// Local index + 1 of the plugin being reloaded (its registrations replace the old ones).
    reloading: AtomicU32,
    pub(crate) events: events::Events,
    pub(crate) commands: commands::Commands,
    pub(crate) permission: RwLock<Option<PermissionProvider>>,
    pub(crate) generator: Mutex<Option<(u32, abi::ChunkGeneratorProvider)>>,
    pub(crate) tasks: scheduler::Tasks,
    pub(crate) server: OnceLock<Arc<dyn ServerAccess>>,
    pub(crate) tick: AtomicU64,
    /// Number of the next development copy of a library.
    dev_copies: AtomicU32,
}

// SAFETY: the provider table holds the plugin's function pointers and user data, which the
// plugin declares thread-safe (providers are called from many threads by contract).
unsafe impl Send for Inner {}
// SAFETY: as above.
unsafe impl Sync for Inner {}

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginHost")
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

/// All hosts of the process (normally one; tests may run several servers). A plugin id holds
/// the host's index in its upper 16 bits.
static HOSTS: RwLock<Vec<Weak<Inner>>> = RwLock::new(Vec::new());

/// Libraries loaded in this process: a library's statics are shared, so it can only serve one
/// host.
static LOADED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

impl Inner {
    /// The host and plugin of a plugin id from a C call.
    pub(crate) fn resolve(id: abi::PluginId) -> Option<(Arc<Inner>, Arc<Plugin>)> {
        let host = usize::try_from(id >> 16).ok()?;
        let inner = HOSTS
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(host)?
            .upgrade()?;
        let plugin = inner.plugin(id & 0xffff)?;
        Some((inner, plugin))
    }

    pub(crate) fn plugin(&self, local: u32) -> Option<Arc<Plugin>> {
        self.plugins
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(usize::try_from(local).ok()?)
            .cloned()
    }

    /// Whether `plugin` may register now: during `load`, or its own development reload.
    pub(crate) fn may_register(&self, plugin: &Plugin) -> bool {
        self.phase.load(Ordering::Acquire) == LOADING
            || self.reloading.load(Ordering::Acquire) == plugin.local + 1
    }

    /// Whether `plugin` is being reloaded right now.
    pub(crate) fn is_reloading(&self, plugin: &Plugin) -> bool {
        self.reloading.load(Ordering::Acquire) == plugin.local + 1
    }

    pub(crate) fn server(&self) -> Option<&Arc<dyn ServerAccess>> {
        self.server.get()
    }
}

/// The plugin host of a server.
#[derive(Clone, Debug)]
pub struct PluginHost {
    inner: Arc<Inner>,
}

impl PluginHost {
    /// A host without plugins.
    pub fn empty() -> PluginHost {
        Self::new(PathBuf::new(), Settings::default())
    }

    fn new(dir: PathBuf, settings: Settings) -> PluginHost {
        let mut hosts = HOSTS.write().unwrap_or_else(PoisonError::into_inner);
        let index = hosts
            .iter()
            .position(|w| w.strong_count() == 0)
            .unwrap_or(hosts.len());
        let inner = Arc::new(Inner {
            index: u16::try_from(index).unwrap_or(u16::MAX),
            dir,
            settings,
            plugins: RwLock::new(Vec::new()),
            phase: AtomicU8::new(LOADING),
            reloading: AtomicU32::new(0),
            events: events::Events::default(),
            commands: commands::Commands::default(),
            permission: RwLock::new(None),
            generator: Mutex::new(None),
            tasks: scheduler::Tasks::default(),
            server: OnceLock::new(),
            tick: AtomicU64::new(0),
            dev_copies: AtomicU32::new(0),
        });
        if index == hosts.len() {
            hosts.push(Arc::downgrade(&inner));
        } else if let Some(slot) = hosts.get_mut(index) {
            *slot = Arc::downgrade(&inner);
        }
        PluginHost { inner }
    }

    /// Discovers, validates and loads the plugins in `dir` (normally `plugins/`), in
    /// dependency order, and calls their `load`. Failures are logged; the plugin is skipped.
    pub fn load(dir: &Path, settings: Settings) -> PluginHost {
        let enabled = settings.enabled;
        let host = Self::new(dir.to_owned(), settings);
        if !enabled {
            return host;
        }
        if let Err(e) = std::fs::create_dir_all(dir) {
            log::error!("Cannot create {}: {e}", dir.display());
            return host;
        }
        let packages = loader::order(loader::discover(dir));
        if packages.is_empty() {
            return host;
        }
        crash::install(dir);
        let crashed = crash::read_crashed(dir);
        let mut still_crashed = Vec::new();
        let pins = parse_pins(&host.inner.settings.pins);
        for package in packages {
            let id = package.manifest.id.clone();
            if let Some((_, sha)) = crashed.iter().find(|(i, _)| *i == id) {
                if *sha == package.sha256 && host.inner.settings.safe_mode_after_crash {
                    log::warn!(
                        "Safe mode: plugin {id} crashed the server at the last run and is skipped \
                         until its library changes or plugins/{} is removed",
                        crash::CRASH_FILE
                    );
                    still_crashed.push((id, sha.clone()));
                    continue;
                }
                if *sha == package.sha256 {
                    log::warn!("Plugin {id} crashed the server at the last run");
                }
            }
            if let Some(pin) = pins.get(id.as_str())
                && !pin.eq_ignore_ascii_case(&package.sha256)
            {
                log::error!(
                    "Skipping plugin {id}: its SHA-256 {} does not match the pin {pin} \
                     (plugins.pins)",
                    package.sha256
                );
                continue;
            }
            if let Err(e) = host.load_package(&package) {
                log::error!("Skipping plugin {id}: {e}");
            }
        }
        crash::write_crashed(dir, &still_crashed);
        host
    }

    /// Loads one package and calls its `load`; the plugin's registrations are kept.
    fn load_package(&self, package: &loader::Package) -> Result<(), String> {
        let manifest = &package.manifest;
        for dependency in &manifest.depends {
            if !self.is_loaded(dependency) {
                return Err(format!("its dependency {dependency} did not load"));
            }
        }
        let library = canonical(&package.library);
        {
            let mut loaded = LOADED.lock().unwrap_or_else(PoisonError::into_inner);
            if loaded.contains(&library) {
                return Err(format!(
                    "{} is already loaded by another server in this process",
                    library.display()
                ));
            }
            loaded.push(library.clone());
        }
        log::info!(
            "Loading plugin {} {} (SHA-256 {})",
            manifest.id,
            manifest.version,
            package.sha256
        );
        if !manifest.uses.is_empty() {
            log::info!(
                "Plugin {} declares that it uses: {}",
                manifest.id,
                manifest.uses.join(", ")
            );
        }
        let info = open(&library, manifest, self.inner.settings.data_version)?;
        let local = {
            let mut plugins = self
                .inner
                .plugins
                .write()
                .unwrap_or_else(PoisonError::into_inner);
            let local = u32::try_from(plugins.len()).map_err(|_| "too many plugins")?;
            let data_dir = self.inner.dir.join(&manifest.id);
            std::fs::create_dir_all(&data_dir)
                .map_err(|e| format!("{}: {e}", data_dir.display()))?;
            plugins.push(Arc::new(Plugin {
                manifest: manifest.clone(),
                local,
                abi_id: (u32::from(self.inner.index) << 16) | local,
                data_dir,
                sha256: package.sha256.clone(),
                info,
                tag: crash::Tag::new(&manifest.id, &package.sha256),
                serialized: (manifest.threading == Threading::Serialized).then(|| Mutex::new(())),
                enabled: AtomicBool::new(true),
                config: Mutex::new(None),
            }));
            local
        };
        let plugin = self.inner.plugin(local).ok_or("plugin vanished")?;
        let result = plugin.call("load", || {
            // SAFETY: the host table is static; `load` is the validated entry of the plugin.
            unsafe { (plugin.info.load)(&api::HOST_API, plugin.abi_id) }
        });
        if result != status::OK {
            plugin.enabled.store(false, Ordering::Release);
            self.inner.forget(local);
            return Err(format!("its load failed (status {result})"));
        }
        Ok(())
    }

    fn is_loaded(&self, id: &str) -> bool {
        self.inner
            .plugins
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|p| p.id() == id && p.enabled.load(Ordering::Acquire))
    }

    /// Ids of the enabled plugins in load order.
    pub fn plugins(&self) -> Vec<String> {
        self.inner
            .plugins
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|p| p.enabled.load(Ordering::Acquire))
            .map(|p| p.id().to_owned())
            .collect()
    }

    /// Connects the host to the running server. Until then server functions fail with
    /// `WRONG_PHASE`.
    pub fn set_server(&self, server: Arc<dyn ServerAccess>) {
        let _ = self.inner.server.set(server);
    }

    /// Ends the registration phase and enables every plugin in load order.
    pub fn enable_all(&self) {
        self.inner.phase.store(RUNNING, Ordering::Release);
        let plugins = self
            .inner
            .plugins
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        for plugin in plugins {
            let mut ctx = api::CallCtx::new(None);
            let result = plugin.call("enable", || {
                // SAFETY: validated entry; the context lives for the call.
                unsafe { (plugin.info.enable)(ctx.as_raw()) }
            });
            if result == status::OK {
                log::info!("Enabled plugin {} {}", plugin.id(), plugin.manifest.version);
            } else if result != status::DISABLED {
                log::error!(
                    "Enabling plugin {} failed (status {result}); it is disabled",
                    plugin.id()
                );
                plugin.enabled.store(false, Ordering::Release);
                self.inner.forget(plugin.local);
            }
        }
    }

    /// Disables every plugin in reverse load order (shutdown) and drops their tasks.
    pub fn disable_all(&self) {
        let plugins = self
            .inner
            .plugins
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        for plugin in plugins.iter().rev() {
            if !plugin.enabled.load(Ordering::Acquire) {
                continue;
            }
            let mut ctx = api::CallCtx::new(None);
            log::info!("Disabling plugin {}", plugin.id());
            plugin.call("disable", || {
                // SAFETY: validated entry; the context lives for the call.
                unsafe { (plugin.info.disable)(ctx.as_raw()) }
            });
            plugin.enabled.store(false, Ordering::Release);
        }
        self.inner.tasks.clear(&self.inner);
    }

    /// Development reload (`plugins.development`): loads the current build of a plugin under a
    /// fresh file name next to the old one, moves its state with `save_state`/`restore_state`
    /// and replaces its event handlers, command callbacks and tasks. The old library stays
    /// mapped (a deliberate leak). Command trees and providers are fixed at startup: a changed
    /// tree or a new provider needs a restart.
    pub fn reload(&self, id: &str) -> Result<(), String> {
        if !self.inner.settings.development {
            return Err(
                "reloading needs plugins.development = true; restart the server instead".into(),
            );
        }
        let old = self
            .inner
            .plugins
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|p| p.id() == id)
            .cloned()
            .ok_or_else(|| format!("no plugin {id}"))?;
        let package = loader::discover(&self.inner.dir)
            .into_iter()
            .find(|p| p.manifest.id == id)
            .ok_or_else(|| format!("the package of {id} is gone"))?;
        let copies = self.inner.dir.join(".dev");
        std::fs::create_dir_all(&copies).map_err(|e| e.to_string())?;
        let n = self.inner.dev_copies.fetch_add(1, Ordering::Relaxed);
        let copy = copies.join(format!(
            "{id}-{}-{n}-{}",
            std::process::id(),
            loader::library_file(&package.manifest.library)
        ));
        std::fs::copy(&package.library, &copy).map_err(|e| format!("{}: {e}", copy.display()))?;
        let info = open(&copy, &package.manifest, self.inner.settings.data_version)?;

        // Save the state and stop the old build.
        let mut state = Vec::new();
        if let Some(save) = old.info.save_state {
            let sink = abi::ByteSink {
                size: abi::size_of::<abi::ByteSink>(),
                state: std::ptr::from_mut(&mut state).cast(),
                write: api::collect_bytes,
            };
            // SAFETY: validated entry; the sink lives for the call.
            old.call("save_state", || unsafe { save(&sink) });
        }
        if old.enabled.load(Ordering::Acquire) {
            let mut ctx = api::CallCtx::new(None);
            // SAFETY: validated entry; the context lives for the call.
            old.call("disable", || unsafe { (old.info.disable)(ctx.as_raw()) });
        }
        old.enabled.store(false, Ordering::Release);
        self.inner.tasks.drop_plugin(&self.inner, old.local);

        // Start the new build in the old one's place.
        let new = Arc::new(Plugin {
            manifest: package.manifest.clone(),
            local: old.local,
            abi_id: old.abi_id,
            data_dir: old.data_dir.clone(),
            sha256: package.sha256.clone(),
            info,
            tag: crash::Tag::new(id, &package.sha256),
            serialized: (package.manifest.threading == Threading::Serialized)
                .then(|| Mutex::new(())),
            enabled: AtomicBool::new(true),
            config: Mutex::new(None),
        });
        if let Some(slot) = self
            .inner
            .plugins
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .get_mut(usize::try_from(old.local).unwrap_or(usize::MAX))
        {
            *slot = Arc::clone(&new);
        }
        self.inner.events.remove_plugin(&self.inner, old.local);
        self.inner.commands.begin_reload(old.local);
        self.inner.reloading.store(old.local + 1, Ordering::Release);
        // SAFETY: the host table is static; `load` is the validated entry of the new build.
        let loaded = new.call("load", || unsafe {
            (new.info.load)(&api::HOST_API, new.abi_id)
        });
        self.inner.reloading.store(0, Ordering::Release);
        if loaded != status::OK {
            new.enabled.store(false, Ordering::Release);
            self.inner.forget(new.local);
            return Err(format!(
                "the new build's load failed (status {loaded}); {id} is disabled"
            ));
        }
        if let Some(restore) = new.info.restore_state {
            // SAFETY: validated entry; the bytes live for the call.
            new.call("restore_state", || unsafe {
                restore(abi::Bytes::new(&state))
            });
        }
        let mut ctx = api::CallCtx::new(None);
        // SAFETY: validated entry; the context lives for the call.
        let enabled = new.call("enable", || unsafe { (new.info.enable)(ctx.as_raw()) });
        if enabled != status::OK {
            new.enabled.store(false, Ordering::Release);
            self.inner.forget(new.local);
            return Err(format!(
                "the new build's enable failed (status {enabled}); {id} is disabled"
            ));
        }
        log::info!(
            "Reloaded plugin {id} {} (SHA-256 {})",
            new.manifest.version,
            new.sha256
        );
        Ok(())
    }

    /// Whether any plugin listens to an event kind ([`abi::event`]); hook points check this
    /// before they build an event.
    #[inline]
    pub fn wants(&self, kind: u32) -> bool {
        self.inner.events.wants(kind)
    }

    /// Runs the tasks due on the global owner (call once per tick in the global pre-phase).
    pub fn run_global_tasks(&self, tick: u64) {
        self.inner.tick.store(tick, Ordering::Release);
        self.inner.tasks.run_global(&self.inner, tick);
    }

    /// Runs the tasks due on a world (call once per world tick, with the world borrowed).
    pub fn run_world_tasks(&self, world: &mut dyn WorldAccess, tick: u64) {
        self.inner.tasks.run_world(&self.inner, world, tick);
    }

    /// Whether a player has a permission node: the permission provider decides; without one
    /// or when it does not decide, operators have every node.
    pub fn has_permission(&self, player: u128, node: &str, operator: bool) -> bool {
        api::has_permission(&self.inner, player, node, operator)
    }
}

impl Inner {
    /// Removes everything a plugin registered (after a failed load or enable).
    pub(crate) fn forget(&self, local: u32) {
        self.events.remove_plugin(self, local);
        self.tasks.drop_plugin(self, local);
        let mut permission = self
            .permission
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        if permission.as_ref().is_some_and(|p| p.plugin == local) {
            *permission = None;
        }
    }
}

/// Opens a library and reads and validates its plugin info.
fn open(library: &Path, manifest: &Manifest, data_version: i32) -> Result<abi::PluginInfo, String> {
    // SAFETY: loading a library runs its initialisers: plugins are trusted code (architecture
    // 6.5). The library is never unloaded, so everything it hands out stays valid.
    let lib = unsafe { libloading::Library::new(library) }
        .map_err(|e| format!("cannot load {}: {e}", library.display()))?;
    // SAFETY: the symbol has the ABI's entry type by contract; checked by its results below.
    let entry: abi::EntryFn = unsafe {
        *lib.get::<abi::EntryFn>(abi::ENTRY_SYMBOL.as_bytes())
            .map_err(|_| format!("{} exports no {}", library.display(), abi::ENTRY_SYMBOL))?
    };
    // SAFETY: the entry returns a pointer to static plugin info, or null.
    let info = unsafe { entry() };
    if info.is_null() {
        return Err("the plugin returned no plugin info".into());
    }
    // SAFETY: non-null and static; every version of the record starts with its size.
    let size = unsafe { (*info).size } as usize;
    if size < size_of::<abi::PluginInfo>() {
        return Err(format!(
            "the plugin info has {size} bytes, expected {}",
            size_of::<abi::PluginInfo>()
        ));
    }
    // SAFETY: non-null, static and at least as large as this version's record.
    let info = unsafe { *info };
    loader::validate(&info, manifest, data_version)?;
    std::mem::forget(lib);
    Ok(info)
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned())
}

fn parse_pins(pins: &str) -> HashMap<&str, &str> {
    pins.split(',')
        .filter_map(|p| p.split_once('='))
        .map(|(id, sha)| (id.trim(), sha.trim()))
        .collect()
}
