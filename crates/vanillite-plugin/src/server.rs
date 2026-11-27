//! The server as a plugin sees it: [`Server`] for thread-safe calls, [`Ctx`] for the owner of
//! the current call, and the scheduler.

use std::ffi::c_void;
use std::marker::PhantomData;
use std::path::PathBuf;

use vanillite_abi::{self as abi, Status, status};

use crate::ffi::{self, Host, guard, read_string};
use crate::types::{
    AccessList, BlockBox, BlockPos, DataValue, Effect, Error, GameMode, Location, NO_BLOCK,
    Particle, PlayerInfo, ServerStats, SetMode, Sound, Text, Uuid, Vec3, Weather, World,
};

mod v16;
mod v18;
pub use v16::{RayHit, WorldBorder};
pub use v18::{Environment, WorldCreator, WorldDetails};

/// Thread-safe access to the server. Cheap to copy; usable from any thread after `load`.
#[derive(Clone, Copy, Debug)]
pub struct Server {
    host: Host,
}

/// A scheduled task, for [`Server::cancel`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TaskId(pub u64);

/// Where a task runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    /// The server thread between ticks.
    Global,
    /// A world, before its simulation in a tick.
    World(World),
    /// The owner of a block position (today: its world).
    Location(World, BlockPos),
    /// The owner of an entity (a player); the task ends when the entity is gone.
    Entity(World, Uuid),
}

impl Server {
    /// The server. `None` before the plugin is loaded.
    pub fn get() -> Option<Server> {
        ffi::host().map(|host| Server { host })
    }

    pub(crate) fn from_host(host: Host) -> Server {
        Server { host }
    }

    fn api(&self) -> &'static abi::HostApi {
        self.host.api
    }

    pub(crate) fn host(&self) -> Host {
        self.host
    }

    /// The plugin's data folder (`plugins/<id>/`).
    pub fn data_dir(&self) -> PathBuf {
        // SAFETY: valid host function and buffer.
        read_string(|out| unsafe { (self.api().data_dir)(self.host.id, out) })
            .map(PathBuf::from)
            .unwrap_or_default()
    }

    /// A string value of the plugin's `config.toml` (dotted key, e.g. `messages.join`).
    pub fn config_string(&self, key: &str) -> Option<String> {
        // SAFETY: valid host function, key and buffer.
        read_string(|out| unsafe {
            (self.api().config_string)(self.host.id, abi::Str::new(key), out)
        })
        .ok()
    }

    /// An integer value of the plugin's `config.toml`.
    pub fn config_int(&self, key: &str) -> Option<i64> {
        let mut v = 0;
        // SAFETY: valid host function, key and output.
        let s = unsafe { (self.api().config_int)(self.host.id, abi::Str::new(key), &mut v) };
        (s == status::OK).then_some(v)
    }

    /// A float value of the plugin's `config.toml` (integers are converted).
    pub fn config_float(&self, key: &str) -> Option<f64> {
        let mut v = 0.0;
        // SAFETY: valid host function, key and output.
        let s = unsafe { (self.api().config_float)(self.host.id, abi::Str::new(key), &mut v) };
        (s == status::OK).then_some(v)
    }

    /// A boolean value of the plugin's `config.toml`.
    pub fn config_bool(&self, key: &str) -> Option<bool> {
        let mut v = 0;
        // SAFETY: valid host function, key and output.
        let s = unsafe { (self.api().config_bool)(self.host.id, abi::Str::new(key), &mut v) };
        (s == status::OK).then_some(v != 0)
    }

    /// The value of a `vanillite.toml` rule as TOML text, e.g. `chat.max-pending-messages`.
    pub fn rule(&self, key: &str) -> Option<String> {
        // SAFETY: valid host function, key and buffer.
        read_string(|out| unsafe {
            (self.api().server_rule)(self.host.id, abi::Str::new(key), out)
        })
        .ok()
    }

    /// The online players.
    pub fn online_players(&self) -> Vec<Uuid> {
        let mut buf = vec![abi::Uuid::NIL; 64];
        loop {
            let mut count = 0;
            // SAFETY: valid host function; the buffer holds `buf.len()` entries.
            let s = unsafe {
                (self.api().online_players)(self.host.id, buf.as_mut_ptr(), buf.len(), &mut count)
            };
            if s != status::OK {
                return Vec::new();
            }
            if count <= buf.len() {
                return buf.iter().take(count).map(|u| Uuid::from_abi(*u)).collect();
            }
            buf.resize(count, abi::Uuid::NIL);
        }
    }

    /// A player's name, `None` when offline.
    pub fn player_name(&self, player: Uuid) -> Option<String> {
        // SAFETY: valid host function and buffer.
        read_string(|out| unsafe { (self.api().player_name)(self.host.id, player.to_abi(), out) })
            .ok()
    }

    /// A player's world, `None` when offline.
    pub fn player_world(&self, player: Uuid) -> Option<World> {
        let mut world = 0;
        // SAFETY: valid host function and output.
        let s = unsafe { (self.api().player_world)(self.host.id, player.to_abi(), &mut world) };
        (s == status::OK).then_some(World(world))
    }

    /// Sends a system message to a player.
    pub fn send_message(&self, player: Uuid, text: impl Into<Text>) -> Result<(), Error> {
        let text = text.into();
        let (s, format) = text.parts();
        // SAFETY: valid host function and string.
        Error::check(unsafe {
            (self.api().player_send_message)(
                self.host.id,
                player.to_abi(),
                abi::Str::new(s),
                format,
            )
        })
    }

    /// Disconnects a player with a reason.
    pub fn kick(&self, player: Uuid, reason: impl Into<Text>) -> Result<(), Error> {
        let text = reason.into();
        let (s, format) = text.parts();
        // SAFETY: valid host function and string.
        Error::check(unsafe {
            (self.api().player_kick)(self.host.id, player.to_abi(), abi::Str::new(s), format)
        })
    }

    /// Sets the header and footer of a player's player list (tab list). Empty text removes
    /// them. Needs a host with ABI 1.1.
    pub fn set_tab_list(
        &self,
        player: Uuid,
        header: impl Into<Text>,
        footer: impl Into<Text>,
    ) -> Result<(), Error> {
        let (mut header, mut footer) = (header.into(), footer.into());
        // One format for both: plain text becomes JSON when the other one is JSON.
        if matches!(header, Text::Json(_)) != matches!(footer, Text::Json(_)) {
            header = Text::Json(header.to_json());
            footer = Text::Json(footer.to_json());
        }
        let (h, format) = header.parts();
        let (f, _) = footer.parts();
        // SAFETY: valid host function and strings.
        Error::check(unsafe {
            (self.api().player_set_tab_list)(
                self.host.id,
                player.to_abi(),
                abi::Str::new(h),
                abi::Str::new(f),
                format,
            )
        })
    }

    /// Shows a text above a player's hotbar. Since ABI 1.4.
    pub fn send_action_bar(&self, player: Uuid, text: impl Into<Text>) -> Result<(), Error> {
        let text = text.into();
        let (s, format) = text.parts();
        // SAFETY: valid host function and string.
        Error::check(unsafe {
            (self.api().player_send_action_bar)(
                self.host.id,
                player.to_abi(),
                abi::Str::new(s),
                format,
            )
        })
    }

    /// Shows a title and a subtitle (empty text for none) with fade-in, stay and fade-out
    /// times in ticks (Vanilla's defaults are 10, 70 and 20). Since ABI 1.4.
    pub fn send_title(
        &self,
        player: Uuid,
        title: impl Into<Text>,
        subtitle: impl Into<Text>,
        [fade_in, stay, fade_out]: [i32; 3],
    ) -> Result<(), Error> {
        let (mut title, mut subtitle) = (title.into(), subtitle.into());
        let empty = |t: &Text| t.parts().0.is_empty();
        // One format for both: plain text becomes JSON when the other one is JSON.
        if matches!(title, Text::Json(_)) != matches!(subtitle, Text::Json(_)) {
            if !empty(&title) {
                title = Text::Json(title.to_json());
            }
            if !empty(&subtitle) {
                subtitle = Text::Json(subtitle.to_json());
            }
        }
        let format = if matches!(title, Text::Json(_)) || matches!(subtitle, Text::Json(_)) {
            abi::text_format::JSON
        } else {
            abi::text_format::PLAIN
        };
        let spec = abi::TitleSpec {
            size: abi::size_of::<abi::TitleSpec>(),
            title: abi::Str::new(title.parts().0),
            subtitle: abi::Str::new(subtitle.parts().0),
            format,
            fade_in,
            stay,
            fade_out,
        };
        // SAFETY: valid host function and record.
        Error::check(unsafe {
            (self.api().player_send_title)(self.host.id, player.to_abi(), &spec)
        })
    }

    /// Whether a player has a permission node. Without a permission plugin, operators have
    /// every node and other players none.
    pub fn has_permission(&self, player: Uuid, node: &str) -> bool {
        let mut out = 0;
        // SAFETY: valid host function, string and output.
        let s = unsafe {
            (self.api().player_has_permission)(
                self.host.id,
                player.to_abi(),
                abi::Str::new(node),
                &mut out,
            )
        };
        s == status::OK && out != 0
    }

    /// Sends a system message to every player and the log.
    pub fn broadcast(&self, text: impl Into<Text>) {
        let text = text.into();
        let (s, format) = text.parts();
        // SAFETY: valid host function and string.
        unsafe { (self.api().broadcast_message)(self.host.id, abi::Str::new(s), format) };
    }

    /// The dimension key of a world, e.g. `minecraft:overworld`.
    pub fn world_name(&self, world: World) -> Option<String> {
        // SAFETY: valid host function and buffer.
        read_string(|out| unsafe { (self.api().world_name)(self.host.id, world.0, out) }).ok()
    }

    /// All loaded worlds with their dimension keys.
    pub fn worlds(&self) -> Vec<(World, String)> {
        (0..self.world_count())
            .map(World)
            .filter(|&w| self.world_details(w).is_some_and(|d| d.loaded))
            .filter_map(|w| self.world_name(w).map(|name| (w, name)))
            .collect()
    }

    /// A world by its dimension key.
    pub fn world(&self, dimension: &str) -> Option<World> {
        self.worlds()
            .into_iter()
            .find(|(_, name)| name == dimension)
            .map(|(w, _)| w)
    }

    /// The numeric id of a block state (`minecraft:stone`, `minecraft:oak_log[axis=x]`) in the
    /// server's data version, for providers.
    pub fn block_state_id(&self, state: &str) -> Option<u32> {
        let mut id = 0;
        // SAFETY: valid host function, string and output.
        let s = unsafe { (self.api().block_state_id)(self.host.id, abi::Str::new(state), &mut id) };
        (s == status::OK).then_some(id)
    }

    /// The text of a numeric block state id, e.g. `minecraft:oak_log[axis=y]`.
    pub fn block_state_name(&self, state: u32) -> Option<String> {
        let api = self.api();
        let id = self.host.id;
        // SAFETY: valid host function and buffer.
        read_string(|out| unsafe { (api.block_state_name)(id, state, out) }).ok()
    }

    /// Runs a console command in the next global phase (without `/`).
    pub fn run_command(&self, command: &str) {
        // SAFETY: valid host function and string.
        unsafe { (self.api().run_command)(self.host.id, abi::Str::new(command)) };
    }

    /// Runs `task` on `owner` after `delay` ticks, then every `period` ticks (0: once).
    pub fn schedule(
        &self,
        owner: Owner,
        delay: u64,
        period: u64,
        task: impl FnMut(&mut Ctx<'_>) + Send + 'static,
    ) -> Result<TaskId, Error> {
        let task: Box<Task> = Box::new(Box::new(task));
        let (kind, world, pos, entity) = match owner {
            Owner::Global => (
                abi::owner::GLOBAL,
                abi::limits::NO_WORLD,
                BlockPos::default(),
                Uuid(0),
            ),
            Owner::World(w) => (abi::owner::WORLD, w.0, BlockPos::default(), Uuid(0)),
            Owner::Location(w, p) => (abi::owner::LOCATION, w.0, p, Uuid(0)),
            Owner::Entity(w, e) => (abi::owner::ENTITY, w.0, BlockPos::default(), e),
        };
        self.submit(
            abi::Owner {
                kind,
                world,
                pos: pos.to_abi(),
                entity: entity.to_abi(),
            },
            delay,
            period,
            run_task,
            Box::into_raw(task).cast(),
            drop_task,
        )
    }

    /// Runs `task` on the server thread after `delay` ticks.
    pub fn run_later(
        &self,
        delay: u64,
        task: impl FnMut(&mut Ctx<'_>) + Send + 'static,
    ) -> Result<TaskId, Error> {
        self.schedule(Owner::Global, delay, 0, task)
    }

    /// Runs `task` off the tick on a background thread (I/O, databases). Results go back to an
    /// owner with [`Server::schedule`].
    pub fn run_async(&self, task: impl FnOnce() + Send + 'static) -> Result<TaskId, Error> {
        let task: Box<Option<Box<dyn FnOnce() + Send>>> = Box::new(Some(Box::new(task)));
        self.submit(
            abi::Owner {
                kind: abi::owner::BACKGROUND,
                world: abi::limits::NO_WORLD,
                pos: BlockPos::default().to_abi(),
                entity: abi::Uuid::NIL,
            },
            0,
            0,
            run_once,
            Box::into_raw(task).cast(),
            drop_once,
        )
    }

    /// Runs `task` on a background thread after `delay` ticks, then every `period` ticks
    /// (`period` 0: once), until it is cancelled. Runs never overlap. Since ABI 1.4.
    pub fn run_async_timer(
        &self,
        delay: u64,
        period: u64,
        task: impl FnMut() + Send + 'static,
    ) -> Result<TaskId, Error> {
        let mut task = task;
        let task: Box<Task> = Box::new(Box::new(move |_: &mut Ctx<'_>| task()));
        self.submit(
            abi::Owner {
                kind: abi::owner::BACKGROUND,
                world: abi::limits::NO_WORLD,
                pos: BlockPos::default().to_abi(),
                entity: abi::Uuid::NIL,
            },
            delay,
            period,
            run_task,
            Box::into_raw(task).cast(),
            drop_task,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn submit(
        &self,
        owner: abi::Owner,
        delay: u64,
        period: u64,
        run: abi::TaskFn,
        user: *mut c_void,
        drop: abi::DropFn,
    ) -> Result<TaskId, Error> {
        let spec = abi::TaskSpec {
            size: abi::size_of::<abi::TaskSpec>(),
            owner,
            delay,
            period,
            run,
            drop: Some(drop),
            user,
            retired: None,
        };
        let mut id = 0;
        // SAFETY: valid host function and spec. On failure the host has called `drop`.
        Error::check(unsafe { (self.api().schedule)(self.host.id, &spec, &mut id) })?;
        Ok(TaskId(id))
    }

    /// Cancels a task.
    pub fn cancel(&self, task: TaskId) -> Result<(), Error> {
        // SAFETY: valid host function.
        Error::check(unsafe { (self.api().cancel_task)(self.host.id, task.0) })
    }

    // ---- ABI 1.5 ----

    /// Removes the plugin's handlers of an event kind ([`abi::event`] or a plugin event
    /// kind), at any time. Since ABI 1.5.
    pub fn unsubscribe(&self, kind: u32) -> Result<(), Error> {
        // SAFETY: valid host function.
        Error::check(unsafe { (self.api().unsubscribe)(self.host.id, kind) })
    }

    /// The event kind of a plugin event name (the same in every plugin). Since ABI 1.5.
    pub fn event_kind(&self, name: &str) -> Option<u32> {
        let mut kind = 0;
        // SAFETY: valid host function, string and output.
        let s =
            unsafe { (self.api().event_register)(self.host.id, abi::Str::new(name), &mut kind) };
        (s == status::OK).then_some(kind)
    }

    /// Fires a plugin event to the handlers registered with
    /// [`Registrar::on_plugin_event`](crate::Registrar::on_plugin_event), on this thread and
    /// without world access (use [`Ctx::fire_event`] for that); returns whether a handler
    /// cancelled it. Since ABI 1.5.
    pub fn fire_event(&self, name: &str, data: &[u8]) -> Result<bool, Error> {
        self.fire_event_in(std::ptr::null_mut(), name, data)
    }

    fn fire_event_in(
        &self,
        ctx: *mut abi::Context,
        name: &str,
        data: &[u8],
    ) -> Result<bool, Error> {
        let kind = self.event_kind(name).ok_or(Error::Unsupported)?;
        let mut cancelled = 0;
        // SAFETY: valid host function, context (or null), bytes and output.
        Error::check(unsafe {
            (self.api().event_fire)(
                self.host.id,
                ctx,
                kind,
                abi::Bytes::new(data),
                &mut cancelled,
            )
        })?;
        Ok(cancelled != 0)
    }

    /// Offers a service to other plugins under a name and version: a `repr(C)` table of
    /// functions that lives for the life of the library (other plugins may be written in
    /// other languages). Replaces this plugin's earlier offer of the name. Since ABI 1.5.
    pub fn register_service<T: Sync>(
        &self,
        name: &str,
        version: u32,
        table: &'static T,
    ) -> Result<(), Error> {
        // SAFETY: valid host function and string; the table is static.
        Error::check(unsafe {
            (self.api().service_register)(
                self.host.id,
                abi::Str::new(name),
                version,
                std::ptr::from_ref(table).cast(),
            )
        })
    }

    /// Withdraws a service of this plugin. Since ABI 1.5.
    pub fn unregister_service(&self, name: &str) -> Result<(), Error> {
        // SAFETY: valid host function and string.
        Error::check(unsafe { (self.api().service_unregister)(self.host.id, abi::Str::new(name)) })
    }

    /// The newest table offered under a name, with its version. Since ABI 1.5.
    ///
    /// # Safety
    ///
    /// `T` must be the `repr(C)` layout the providing plugin documents for this name and
    /// version.
    pub unsafe fn service<T>(&self, name: &str) -> Option<(u32, &'static T)> {
        let mut version = 0;
        let mut table = std::ptr::null();
        // SAFETY: valid host function, string and outputs.
        let s = unsafe {
            (self.api().service_get)(self.host.id, abi::Str::new(name), &mut version, &mut table)
        };
        if s != status::OK || table.is_null() {
            return None;
        }
        // SAFETY: guaranteed by the caller; providers' tables live as long as their library.
        Some((version, unsafe { &*table.cast::<T>() }))
    }

    /// The enabled plugins with their versions, in load order. Since ABI 1.5.
    pub fn plugins(&self) -> Vec<(String, String)> {
        // SAFETY: valid host function and sink.
        let bytes = collect_with(|sink| unsafe { (self.api().plugin_list)(self.host.id, sink) })
            .unwrap_or_default();
        String::from_utf8_lossy(&bytes)
            .lines()
            .filter_map(|l| l.split_once(' '))
            .map(|(id, v)| (id.to_owned(), v.to_owned()))
            .collect()
    }

    /// Sets a key of the loaded configuration to a TOML value (`5`, `"text"`, `[1, 2]`);
    /// [`Server::save_config`] writes it. Since ABI 1.5.
    pub fn set_config(&self, key: &str, toml: &str) -> Result<(), Error> {
        // SAFETY: valid host function and strings.
        Error::check(unsafe {
            (self.api().config_set)(self.host.id, abi::Str::new(key), abi::Str::new(toml))
        })
    }

    /// Writes the loaded configuration to `config.toml`. Since ABI 1.5.
    pub fn save_config(&self) -> Result<(), Error> {
        // SAFETY: valid host function.
        Error::check(unsafe { (self.api().config_save)(self.host.id) })
    }

    /// A configuration value as TOML text, also lists and tables. Since ABI 1.5.
    pub fn config_toml(&self, key: &str) -> Option<String> {
        // SAFETY: valid host function, key and buffer.
        read_string(|out| unsafe {
            (self.api().config_toml)(self.host.id, abi::Str::new(key), out)
        })
        .ok()
    }

    /// What the server knows about an online player. Since ABI 1.5.
    pub fn player_info(&self, player: Uuid) -> Option<PlayerInfo> {
        let mut out = abi::PlayerInfo {
            size: abi::size_of::<abi::PlayerInfo>(),
            world: 0,
            game_mode: 0,
            op_level: 0,
            ping: 0,
        };
        // SAFETY: valid host function and output.
        let s = unsafe { (self.api().player_info)(self.host.id, player.to_abi(), &mut out) };
        (s == status::OK).then(|| PlayerInfo {
            world: World(out.world),
            game_mode: GameMode::from_abi(out.game_mode),
            op_level: out.op_level,
            ping: out.ping,
        })
    }

    /// An online player's IP address. Since ABI 1.5.
    pub fn player_address(&self, player: Uuid) -> Option<String> {
        // SAFETY: valid host function and buffer.
        read_string(|out| unsafe {
            (self.api().player_address)(self.host.id, player.to_abi(), out)
        })
        .ok()
    }

    /// Plays a sound to one player. Since ABI 1.5.
    pub fn play_sound(&self, player: Uuid, sound: &Sound) -> Result<(), Error> {
        let spec = abi::SoundSpec {
            size: abi::size_of::<abi::SoundSpec>(),
            sound: abi::Str::new(&sound.sound),
            source: sound.source,
            pos: sound.pos.to_abi(),
            volume: sound.volume,
            pitch: sound.pitch,
            seed: sound.seed,
        };
        // SAFETY: valid host function and record.
        Error::check(unsafe {
            (self.api().player_play_sound)(self.host.id, player.to_abi(), &spec)
        })
    }

    /// Stops a sound (`None`: all) of a category (`None`: all, else one of
    /// [`abi::sound_source`]) for a player. Since ABI 1.5.
    pub fn stop_sound(
        &self,
        player: Uuid,
        sound: Option<&str>,
        source: Option<u32>,
    ) -> Result<(), Error> {
        // SAFETY: valid host function and string.
        Error::check(unsafe {
            (self.api().player_stop_sound)(
                self.host.id,
                player.to_abi(),
                abi::Str::new(sound.unwrap_or("")),
                source.unwrap_or(abi::sound_source::ANY),
            )
        })
    }

    /// Shows particles to one player. Since ABI 1.5.
    pub fn spawn_particle(&self, player: Uuid, particle: &Particle) -> Result<(), Error> {
        let spec = abi::ParticleSpec {
            size: abi::size_of::<abi::ParticleSpec>(),
            particle: abi::Str::new(&particle.particle),
            pos: particle.pos.to_abi(),
            delta: particle.delta.to_abi(),
            speed: particle.speed,
            count: particle.count,
            force: u32::from(particle.force),
        };
        // SAFETY: valid host function and record.
        Error::check(unsafe {
            (self.api().player_spawn_particle)(self.host.id, player.to_abi(), &spec)
        })
    }

    /// Sends a player to another server; that server must accept transfers. Since ABI 1.5.
    pub fn transfer(&self, player: Uuid, host: &str, port: u16) -> Result<(), Error> {
        // SAFETY: valid host function and string.
        Error::check(unsafe {
            (self.api().player_transfer)(
                self.host.id,
                player.to_abi(),
                abi::Str::new(host),
                u32::from(port),
            )
        })
    }

    /// Sends a plugin message (custom payload) to a player. Since ABI 1.5.
    pub fn send_plugin_message(
        &self,
        player: Uuid,
        channel: &str,
        data: &[u8],
    ) -> Result<(), Error> {
        // SAFETY: valid host function, string and bytes.
        Error::check(unsafe {
            (self.api().player_send_plugin_message)(
                self.host.id,
                player.to_abi(),
                abi::Str::new(channel),
                abi::Bytes::new(data),
            )
        })
    }

    /// Adds a scoreboard objective with a criterion (e.g. `dummy`) and a display name.
    /// Since ABI 1.5.
    pub fn add_objective(
        &self,
        name: &str,
        criteria: &str,
        display: impl Into<Text>,
    ) -> Result<(), Error> {
        let display = display.into();
        let (d, format) = display.parts();
        // SAFETY: valid host function and strings.
        Error::check(unsafe {
            (self.api().objective_add)(
                self.host.id,
                abi::Str::new(name),
                abi::Str::new(criteria),
                abi::Str::new(d),
                format,
            )
        })
    }

    /// Removes an objective. Since ABI 1.5.
    pub fn remove_objective(&self, name: &str) -> Result<(), Error> {
        // SAFETY: valid host function and string.
        Error::check(unsafe { (self.api().objective_remove)(self.host.id, abi::Str::new(name)) })
    }

    /// Shows an objective in a display slot ([`abi::display_slot`]); `None` clears it. Since
    /// ABI 1.5.
    pub fn display_objective(&self, slot: u32, objective: Option<&str>) -> Result<(), Error> {
        // SAFETY: valid host function and string.
        Error::check(unsafe {
            (self.api().objective_display)(
                self.host.id,
                slot,
                abi::Str::new(objective.unwrap_or("")),
            )
        })
    }

    /// A score of a holder (player name or entity UUID); `None` when it has none. Since
    /// ABI 1.5.
    pub fn score(&self, objective: &str, holder: &str) -> Option<i32> {
        let mut out = 0;
        // SAFETY: valid host function, strings and output.
        let s = unsafe {
            (self.api().score_get)(
                self.host.id,
                abi::Str::new(objective),
                abi::Str::new(holder),
                &mut out,
            )
        };
        (s == status::OK).then_some(out)
    }

    /// Sets a score. Since ABI 1.5.
    pub fn set_score(&self, objective: &str, holder: &str, value: i32) -> Result<(), Error> {
        // SAFETY: valid host function and strings.
        Error::check(unsafe {
            (self.api().score_set)(
                self.host.id,
                abi::Str::new(objective),
                abi::Str::new(holder),
                value,
            )
        })
    }

    /// Resets a holder's score in an objective (`None`: in all). Since ABI 1.5.
    pub fn reset_score(&self, objective: Option<&str>, holder: &str) -> Result<(), Error> {
        // SAFETY: valid host function and strings.
        Error::check(unsafe {
            (self.api().score_reset)(
                self.host.id,
                abi::Str::new(objective.unwrap_or("")),
                abi::Str::new(holder),
            )
        })
    }

    /// Adds a team. Since ABI 1.5.
    pub fn add_team(&self, name: &str) -> Result<(), Error> {
        // SAFETY: valid host function and string.
        Error::check(unsafe { (self.api().team_add)(self.host.id, abi::Str::new(name)) })
    }

    /// Removes a team. Since ABI 1.5.
    pub fn remove_team(&self, name: &str) -> Result<(), Error> {
        // SAFETY: valid host function and string.
        Error::check(unsafe { (self.api().team_remove)(self.host.id, abi::Str::new(name)) })
    }

    /// Puts a holder (player name or entity UUID) into a team. Since ABI 1.5.
    pub fn join_team(&self, team: &str, holder: &str) -> Result<(), Error> {
        // SAFETY: valid host function and strings.
        Error::check(unsafe {
            (self.api().team_join)(self.host.id, abi::Str::new(team), abi::Str::new(holder))
        })
    }

    /// Takes a holder out of its team. Since ABI 1.5.
    pub fn leave_team(&self, holder: &str) -> Result<(), Error> {
        // SAFETY: valid host function and string.
        Error::check(unsafe { (self.api().team_leave)(self.host.id, abi::Str::new(holder)) })
    }

    /// Changes a team option as `/team modify` names it, e.g. `("prefix", r#"{"text":"[A] "}"#)`,
    /// `("color", "red")`, `("friendlyFire", "false")`. Since ABI 1.5.
    pub fn modify_team(&self, team: &str, option: &str, value: &str) -> Result<(), Error> {
        // SAFETY: valid host function and strings.
        Error::check(unsafe {
            (self.api().team_modify)(
                self.host.id,
                abi::Str::new(team),
                abi::Str::new(option),
                abi::Str::new(value),
            )
        })
    }

    /// The team of a holder. Since ABI 1.5.
    pub fn team_of(&self, holder: &str) -> Option<String> {
        // SAFETY: valid host function, string and buffer.
        read_string(|out| unsafe { (self.api().team_of)(self.host.id, abi::Str::new(holder), out) })
            .ok()
    }

    /// Creates a boss bar (a Vanilla custom boss bar, saved with the world, shown to nobody
    /// yet) under a key (`namespace:path`; without a namespace the plugin's id). Since
    /// ABI 1.5.
    pub fn create_boss_bar(&self, key: &str, name: impl Into<Text>) -> Result<BossBar, Error> {
        let name = name.into();
        let (n, format) = name.parts();
        // SAFETY: valid host function and strings.
        Error::check(unsafe {
            (self.api().bossbar_create)(self.host.id, abi::Str::new(key), abi::Str::new(n), format)
        })?;
        Ok(self.boss_bar(key))
    }

    /// A boss bar created before (also by an earlier run). Since ABI 1.5.
    pub fn boss_bar(&self, key: &str) -> BossBar {
        BossBar {
            server: *self,
            key: key.to_owned(),
        }
    }

    /// Tick statistics and player counts. Since ABI 1.5.
    pub fn stats(&self) -> Option<ServerStats> {
        let mut out = abi::ServerStats {
            size: abi::size_of::<abi::ServerStats>(),
            tick: 0,
            mspt: 0.0,
            tps: 0.0,
            tick_rate: 0.0,
            online: 0,
            max_players: 0,
        };
        // SAFETY: valid host function and output.
        let s = unsafe { (self.api().server_stats)(self.host.id, &mut out) };
        (s == status::OK).then_some(ServerStats {
            tick: out.tick,
            mspt: out.mspt,
            tps: out.tps,
            tick_rate: out.tick_rate,
            online: out.online,
            max_players: out.max_players,
        })
    }

    /// Replaces the message of the day of the server list. Since ABI 1.5.
    pub fn set_motd(&self, motd: impl Into<Text>) -> Result<(), Error> {
        let motd = motd.into();
        let (m, format) = motd.parts();
        // SAFETY: valid host function and string.
        Error::check(unsafe {
            (self.api().server_set_motd)(self.host.id, abi::Str::new(m), format)
        })
    }

    /// Adds a player to an access list (or removes it): UUID and name (the IP address for
    /// [`AccessList::IpBan`]), a reason for bans. Banning kicks the player like `/ban`.
    /// Since ABI 1.5.
    pub fn set_access(
        &self,
        list: AccessList,
        player: Uuid,
        name: &str,
        add: bool,
        reason: &str,
    ) -> Result<(), Error> {
        // SAFETY: valid host function and strings.
        Error::check(unsafe {
            (self.api().access_set)(
                self.host.id,
                list.to_abi(),
                player.to_abi(),
                abi::Str::new(name),
                u32::from(add),
                abi::Str::new(reason),
            )
        })
    }

    /// Whether a player (or IP address for [`AccessList::IpBan`]) is on an access list.
    /// Since ABI 1.5.
    pub fn has_access(&self, list: AccessList, player: Uuid, name: &str) -> bool {
        let mut out = 0;
        // SAFETY: valid host function, string and output.
        let s = unsafe {
            (self.api().access_get)(
                self.host.id,
                list.to_abi(),
                player.to_abi(),
                abi::Str::new(name),
                &mut out,
            )
        };
        s == status::OK && out != 0
    }

    /// Switches the whitelist on or off. Since ABI 1.5.
    pub fn set_whitelist(&self, on: bool) -> Result<(), Error> {
        // SAFETY: valid host function.
        Error::check(unsafe {
            (self.api().access_set)(
                self.host.id,
                abi::access_list::WHITELIST_ON,
                abi::Uuid::NIL,
                abi::Str::EMPTY,
                u32::from(on),
                abi::Str::EMPTY,
            )
        })
    }

    /// The time of a world's clock in ticks (`/time query`). Since ABI 1.5.
    pub fn time(&self, world: World) -> Option<i64> {
        let mut out = 0;
        // SAFETY: valid host function and output.
        let s = unsafe { (self.api().world_time)(self.host.id, world.0, &mut out) };
        (s == status::OK).then_some(out)
    }

    /// Sets the time of a world's clock (`/time set`). Since ABI 1.5.
    pub fn set_time(&self, world: World, ticks: i64) -> Result<(), Error> {
        // SAFETY: valid host function.
        Error::check(unsafe { (self.api().world_set_time)(self.host.id, world.0, ticks) })
    }

    /// Sets the weather for a duration in ticks (`None`: Vanilla's random one). Since
    /// ABI 1.5.
    pub fn set_weather(&self, weather: Weather, duration: Option<i32>) -> Result<(), Error> {
        let kind = match weather {
            Weather::Clear => abi::weather::CLEAR,
            Weather::Rain => abi::weather::RAIN,
            Weather::Thunder => abi::weather::THUNDER,
        };
        // SAFETY: valid host function.
        Error::check(unsafe {
            (self.api().world_set_weather)(self.host.id, kind, duration.unwrap_or(-1).max(-1))
        })
    }

    /// A game rule's value, e.g. `keep_inventory` → `false`. Since ABI 1.5.
    pub fn game_rule(&self, name: &str) -> Option<String> {
        // SAFETY: valid host function, string and buffer.
        read_string(|out| unsafe {
            (self.api().game_rule_get)(self.host.id, abi::Str::new(name), out)
        })
        .ok()
    }

    /// Sets a game rule (`true`, `false` or a number). Since ABI 1.5.
    pub fn set_game_rule(&self, name: &str, value: &str) -> Result<(), Error> {
        // SAFETY: valid host function and strings.
        Error::check(unsafe {
            (self.api().game_rule_set)(self.host.id, abi::Str::new(name), abi::Str::new(value))
        })
    }
}

/// A plugin's boss bar ([`Server::create_boss_bar`]). Since ABI 1.5.
#[derive(Clone, Debug)]
pub struct BossBar {
    server: Server,
    key: String,
}

impl BossBar {
    fn set(&self, prop: u32, value: i64) -> Result<(), Error> {
        // SAFETY: valid host function and string.
        Error::check(unsafe {
            (self.server.api().bossbar_set)(
                self.server.host.id,
                abi::Str::new(&self.key),
                prop,
                value,
            )
        })
    }

    /// Its key as given.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The progress as `value` out of `max` (Vanilla boss bars count in integers).
    pub fn set_progress(&self, value: i32, max: i32) -> Result<(), Error> {
        self.set(abi::bossbar_prop::MAX, i64::from(max))?;
        self.set(abi::bossbar_prop::VALUE, i64::from(value))
    }

    /// The colour, one of [`abi::bossbar_color`].
    pub fn set_color(&self, color: u32) -> Result<(), Error> {
        self.set(abi::bossbar_prop::COLOR, i64::from(color))
    }

    /// The style, one of [`abi::bossbar_style`].
    pub fn set_style(&self, style: u32) -> Result<(), Error> {
        self.set(abi::bossbar_prop::STYLE, i64::from(style))
    }

    /// Shows or hides it for everyone it is shown to.
    pub fn set_visible(&self, visible: bool) -> Result<(), Error> {
        self.set(abi::bossbar_prop::VISIBLE, i64::from(visible))
    }

    /// Renames it.
    pub fn set_name(&self, name: impl Into<Text>) -> Result<(), Error> {
        let name = name.into();
        let (n, format) = name.parts();
        // SAFETY: valid host function and strings.
        Error::check(unsafe {
            (self.server.api().bossbar_set_name)(
                self.server.host.id,
                abi::Str::new(&self.key),
                abi::Str::new(n),
                format,
            )
        })
    }

    /// Shows it to an online player.
    pub fn show(&self, player: Uuid) -> Result<(), Error> {
        self.player(player, true)
    }

    /// Hides it from a player.
    pub fn hide(&self, player: Uuid) -> Result<(), Error> {
        self.player(player, false)
    }

    fn player(&self, player: Uuid, show: bool) -> Result<(), Error> {
        // SAFETY: valid host function and string.
        Error::check(unsafe {
            (self.server.api().bossbar_player)(
                self.server.host.id,
                abi::Str::new(&self.key),
                player.to_abi(),
                u32::from(show),
            )
        })
    }

    /// Removes it from the world.
    pub fn remove(self) -> Result<(), Error> {
        // SAFETY: valid host function and string.
        Error::check(unsafe {
            (self.server.api().bossbar_remove)(self.server.host.id, abi::Str::new(&self.key))
        })
    }
}

type Task = Box<dyn FnMut(&mut Ctx<'_>) + Send>;

unsafe extern "C" fn run_task(user: *mut c_void, ctx: *mut abi::Context) -> Status {
    guard(|| {
        // SAFETY: `user` is the `Box<Task>` of `schedule`; the host never runs a task twice at once.
        let task = unsafe { &mut *user.cast::<Task>() };
        task(&mut Ctx::new(ctx));
        status::OK
    })
}

unsafe extern "C" fn drop_task(user: *mut c_void) {
    let _ = guard(|| {
        // SAFETY: `user` is the `Box<Task>` of `schedule`; the host drops it exactly once.
        drop(unsafe { Box::from_raw(user.cast::<Task>()) });
        status::OK
    });
}

unsafe extern "C" fn run_once(user: *mut c_void, _ctx: *mut abi::Context) -> Status {
    guard(|| {
        // SAFETY: `user` is the box of `run_async`.
        let task = unsafe { &mut *user.cast::<Option<Box<dyn FnOnce() + Send>>>() };
        if let Some(task) = task.take() {
            task();
        }
        status::OK
    })
}

unsafe extern "C" fn drop_once(user: *mut c_void) {
    let _ = guard(|| {
        // SAFETY: `user` is the box of `run_async`; the host drops it exactly once.
        drop(unsafe { Box::from_raw(user.cast::<Option<Box<dyn FnOnce() + Send>>>()) });
        status::OK
    });
}

/// A [`abi::ByteSinkFn`] appending to the `Vec<u8>` behind `state`.
unsafe extern "C" fn collect(state: *mut c_void, data: abi::Bytes) {
    let _ = guard(|| {
        // SAFETY: `state` is the caller's `Vec<u8>` for the call.
        if let Some(out) = unsafe { state.cast::<Vec<u8>>().as_mut() }
            && data.len > 0
            && !data.ptr.is_null()
        {
            // SAFETY: the host passes bytes valid for the call.
            out.extend_from_slice(unsafe { std::slice::from_raw_parts(data.ptr, data.len) });
        }
        status::OK
    });
}

/// Calls `f` with a sink collecting into a buffer; returns the bytes.
fn collect_with(f: impl FnOnce(*const abi::ByteSink) -> Status) -> Result<Vec<u8>, Error> {
    let mut out: Vec<u8> = Vec::new();
    let sink = abi::ByteSink {
        size: abi::size_of::<abi::ByteSink>(),
        state: std::ptr::from_mut(&mut out).cast(),
        write: collect,
    };
    Error::check(f(&sink))?;
    Ok(out)
}

/// The context of one call: what its owner may touch. It cannot be kept beyond the call.
#[derive(Debug)]
pub struct Ctx<'a> {
    raw: *mut abi::Context,
    /// The call is an entity task's retired callback ([`Server::schedule_entity`]).
    retiring: bool,
    _call: PhantomData<&'a mut ()>,
}

impl Ctx<'_> {
    pub(crate) fn new(raw: *mut abi::Context) -> Self {
        Ctx {
            raw,
            retiring: false,
            _call: PhantomData,
        }
    }

    /// Thread-safe server access.
    ///
    /// # Panics
    ///
    /// Never after `load`: contexts only exist in calls from the host.
    pub fn server(&self) -> Server {
        Server::get().expect("contexts only exist after load")
    }

    fn api(&self) -> &'static abi::HostApi {
        self.server().api()
    }

    /// The world this context owns; `None` for global and background contexts and in handlers
    /// that run where blocks cannot be touched (chat, commands, damage).
    pub fn world(&self) -> Option<World> {
        let mut world = 0;
        // SAFETY: valid host function and context of this call.
        let s = unsafe { (self.api().owner_world)(self.raw, &mut world) };
        (s == status::OK).then_some(World(world))
    }

    /// The block state at a position, e.g. `minecraft:oak_log[axis=y]`.
    pub fn block(&mut self, world: World, pos: BlockPos) -> Result<String, Error> {
        let (api, raw) = (self.api(), self.raw);
        // SAFETY: valid host function, context of this call and buffer.
        read_string(|out| unsafe { (api.block_get)(raw, world.0, pos.to_abi(), out) })
            .map_err(|s| Error::check(s).err().unwrap_or(Error::InvalidArgument))
    }

    /// Sets a block state (with Vanilla's neighbour updates), e.g. `minecraft:stone`.
    pub fn set_block(&mut self, world: World, pos: BlockPos, state: &str) -> Result<(), Error> {
        // SAFETY: valid host function, context of this call and string.
        Error::check(unsafe {
            (self.api().block_set)(self.raw, world.0, pos.to_abi(), abi::Str::new(state))
        })
    }

    /// The numeric block state id at a position ([`Server::block_state_id`]); the fast path
    /// for code that reads many blocks.
    pub fn block_id(&mut self, world: World, pos: BlockPos) -> Result<u32, Error> {
        let mut out = 0;
        // SAFETY: valid host function, context of this call and output.
        Error::check(unsafe {
            (self.api().block_get_id)(self.raw, world.0, pos.to_abi(), &mut out)
        })?;
        Ok(out)
    }

    /// Sets a block state by numeric id.
    pub fn set_block_id(
        &mut self,
        world: World,
        pos: BlockPos,
        state: u32,
        mode: SetMode,
    ) -> Result<(), Error> {
        // SAFETY: valid host function and context of this call.
        Error::check(unsafe {
            (self.api().block_set_id)(self.raw, world.0, pos.to_abi(), state, mode.to_abi())
        })
    }

    /// The block state ids of a box in one call ([`BlockBox`] order); [`NO_BLOCK`] outside
    /// loaded chunks. At most [`abi::limits::MAX_BOX_VOLUME`] blocks.
    pub fn blocks(&mut self, world: World, area: BlockBox) -> Result<Vec<u32>, Error> {
        let mut out = vec![NO_BLOCK; area.volume()];
        self.blocks_into(world, area, &mut out)?;
        Ok(out)
    }

    /// [`Ctx::blocks`] into a buffer of at least the box's volume.
    pub fn blocks_into(
        &mut self,
        world: World,
        area: BlockBox,
        out: &mut [u32],
    ) -> Result<(), Error> {
        let area = area.to_abi();
        // SAFETY: valid host function, context of this call, box and buffer of `out.len()`.
        Error::check(unsafe {
            (self.api().blocks_get)(self.raw, world.0, &area, out.as_mut_ptr(), out.len())
        })
    }

    /// Sets the block states of a box in one call from `states` ([`BlockBox`] order, exactly
    /// its volume); [`NO_BLOCK`] entries and unloaded positions stay unchanged. Returns how
    /// many blocks changed. Unknown ids fail before any block changes.
    pub fn set_blocks(
        &mut self,
        world: World,
        area: BlockBox,
        states: &[u32],
        mode: SetMode,
    ) -> Result<usize, Error> {
        let area = area.to_abi();
        let mut changed = 0;
        // SAFETY: valid host function, context of this call, box, states and output.
        Error::check(unsafe {
            (self.api().blocks_set)(
                self.raw,
                world.0,
                &area,
                states.as_ptr(),
                states.len(),
                mode.to_abi(),
                &mut changed,
            )
        })?;
        Ok(changed)
    }

    /// The position of a player of the owned world.
    pub fn player_position(&mut self, player: Uuid) -> Result<Vec3, Error> {
        let mut out = abi::Vec3 {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        };
        // SAFETY: valid host function, context of this call and output.
        Error::check(unsafe { (self.api().player_position)(self.raw, player.to_abi(), &mut out) })?;
        Ok(Vec3::from_abi(out))
    }

    /// A persistent data value of an entity (or player) of the owned world; `None` when it
    /// has none under `key`. Keys are `namespace:path`; a key without a namespace uses the
    /// plugin's id. Values are saved with the entity (`BukkitValues`, as Paper does). Since
    /// ABI 1.4.
    pub fn entity_data(
        &mut self,
        world: World,
        entity: Uuid,
        key: &str,
    ) -> Result<Option<DataValue>, Error> {
        let (api, raw) = (self.api(), self.raw);
        let mut kind = 0;
        // SAFETY: valid host function, context of this call, key, output and sink.
        let bytes = collect_with(|sink| unsafe {
            (api.entity_data_get)(
                raw,
                world.0,
                entity.to_abi(),
                abi::Str::new(key),
                &mut kind,
                sink,
            )
        });
        match bytes {
            Ok(bytes) => Ok(Some(
                DataValue::decode(kind, &bytes).ok_or(Error::InvalidArgument)?,
            )),
            Err(Error::NotFound) if self.has_entity(world, entity) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Whether the entity is in the owned world (it answers an empty key list).
    fn has_entity(&mut self, world: World, entity: Uuid) -> bool {
        self.entity_data_keys(world, entity).is_ok()
    }

    /// Sets a persistent data value of an entity (or player) of the owned world (at most
    /// 64 KiB, at most 256 keys per entity). Since ABI 1.4.
    pub fn set_entity_data(
        &mut self,
        world: World,
        entity: Uuid,
        key: &str,
        value: &DataValue,
    ) -> Result<(), Error> {
        let (kind, bytes) = value.encode();
        // SAFETY: valid host function, context of this call, key and bytes.
        Error::check(unsafe {
            (self.api().entity_data_set)(
                self.raw,
                world.0,
                entity.to_abi(),
                abi::Str::new(key),
                kind,
                abi::Bytes::new(&bytes),
            )
        })
    }

    /// Removes a persistent data value of an entity of the owned world. Since ABI 1.4.
    pub fn remove_entity_data(
        &mut self,
        world: World,
        entity: Uuid,
        key: &str,
    ) -> Result<(), Error> {
        // SAFETY: valid host function, context of this call and key.
        Error::check(unsafe {
            (self.api().entity_data_remove)(self.raw, world.0, entity.to_abi(), abi::Str::new(key))
        })
    }

    /// The persistent data keys of an entity of the owned world (all plugins' keys, with
    /// their namespaces). Since ABI 1.4.
    pub fn entity_data_keys(&mut self, world: World, entity: Uuid) -> Result<Vec<String>, Error> {
        let (api, raw) = (self.api(), self.raw);
        // SAFETY: valid host function, context of this call and sink.
        let bytes = collect_with(|sink| unsafe {
            (api.entity_data_keys)(raw, world.0, entity.to_abi(), sink)
        })?;
        let text = String::from_utf8(bytes).map_err(|_| Error::InvalidArgument)?;
        Ok(text.lines().map(str::to_owned).collect())
    }

    // ---- ABI 1.5 ----

    /// Fires a plugin event to the handlers registered with
    /// [`Registrar::on_plugin_event`](crate::Registrar::on_plugin_event); they get this
    /// context, so they may touch its world. Returns whether a handler cancelled it. Since
    /// ABI 1.5.
    pub fn fire_event(&mut self, name: &str, data: &[u8]) -> Result<bool, Error> {
        self.server().fire_event_in(self.raw, name, data)
    }

    /// A property ([`abi::player_prop`]) of a player of the owned world. Since ABI 1.5.
    pub fn player_prop(&mut self, world: World, player: Uuid, prop: u32) -> Result<f64, Error> {
        let mut out = 0.0;
        // SAFETY: valid host function, context of this call and output.
        Error::check(unsafe {
            (self.api().player_get)(self.raw, world.0, player.to_abi(), prop, &mut out)
        })?;
        Ok(out)
    }

    /// Changes a property ([`abi::player_prop`]) of a player of the owned world, e.g.
    /// `abi::player_prop::FOOD` to 20. Since ABI 1.5.
    pub fn set_player_prop(
        &mut self,
        world: World,
        player: Uuid,
        prop: u32,
        value: f64,
    ) -> Result<(), Error> {
        // SAFETY: valid host function and context of this call.
        Error::check(unsafe {
            (self.api().player_set)(self.raw, world.0, player.to_abi(), prop, value)
        })
    }

    /// A player's game mode. Since ABI 1.5.
    pub fn game_mode(&mut self, world: World, player: Uuid) -> Result<GameMode, Error> {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        self.player_prop(world, player, abi::player_prop::GAME_MODE)
            .map(|m| GameMode::from_abi(m as u32))
    }

    /// Sets a player's game mode as `/gamemode` does (game mode change handlers may refuse
    /// it). Since ABI 1.5.
    pub fn set_game_mode(
        &mut self,
        world: World,
        player: Uuid,
        mode: GameMode,
    ) -> Result<(), Error> {
        self.set_player_prop(
            world,
            player,
            abi::player_prop::GAME_MODE,
            f64::from(mode.to_abi()),
        )
    }

    /// Where an entity (or player) of the owned world is. Since ABI 1.5.
    pub fn location(&mut self, world: World, entity: Uuid) -> Result<Location, Error> {
        let mut out = Location {
            world,
            pos: Vec3 {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
            yaw: 0.0,
            pitch: 0.0,
        }
        .to_abi();
        // SAFETY: valid host function, context of this call and output.
        Error::check(unsafe {
            (self.api().entity_location)(self.raw, world.0, entity.to_abi(), &mut out)
        })?;
        Ok(Location::from_abi(out))
    }

    /// The type of an entity of the owned world, e.g. `minecraft:zombie`. Since ABI 1.5.
    pub fn entity_type(&mut self, world: World, entity: Uuid) -> Result<String, Error> {
        let (api, raw) = (self.api(), self.raw);
        // SAFETY: valid host function, context of this call and buffer.
        read_string(|out| unsafe { (api.entity_type)(raw, world.0, entity.to_abi(), out) })
            .map_err(|s| Error::check(s).err().unwrap_or(Error::InvalidArgument))
    }

    /// Teleports an entity of the owned world; players may go to another world. Since
    /// ABI 1.5.
    pub fn teleport(&mut self, world: World, entity: Uuid, to: Location) -> Result<(), Error> {
        let to = to.to_abi();
        // SAFETY: valid host function, context of this call and record.
        Error::check(unsafe {
            (self.api().entity_teleport)(self.raw, world.0, entity.to_abi(), &to)
        })
    }

    /// Sets an entity's motion in blocks per tick. Since ABI 1.5.
    pub fn set_velocity(
        &mut self,
        world: World,
        entity: Uuid,
        velocity: Vec3,
    ) -> Result<(), Error> {
        // SAFETY: valid host function and context of this call.
        Error::check(unsafe {
            (self.api().entity_set_velocity)(self.raw, world.0, entity.to_abi(), velocity.to_abi())
        })
    }

    /// Spawns an entity as `/summon` does, with optional SNBT data (`""` for none). Since
    /// ABI 1.5.
    pub fn spawn_entity(
        &mut self,
        world: World,
        kind: &str,
        pos: Vec3,
        nbt: &str,
    ) -> Result<Uuid, Error> {
        let mut out = abi::Uuid::NIL;
        // SAFETY: valid host function, context of this call, strings and output.
        Error::check(unsafe {
            (self.api().entity_spawn)(
                self.raw,
                world.0,
                abi::Str::new(kind),
                pos.to_abi(),
                abi::Str::new(nbt),
                &mut out,
            )
        })?;
        Ok(Uuid::from_abi(out))
    }

    /// Removes an entity (not a player) of the owned world. Since ABI 1.5.
    pub fn remove_entity(&mut self, world: World, entity: Uuid) -> Result<(), Error> {
        // SAFETY: valid host function and context of this call.
        Error::check(unsafe { (self.api().entity_remove)(self.raw, world.0, entity.to_abi()) })
    }

    /// The entities of the owned world within `radius` (at most 1024) of `center`, nearest
    /// first, optionally of one type. Since ABI 1.5.
    pub fn entities_near(
        &mut self,
        world: World,
        center: Vec3,
        radius: f64,
        kind: Option<&str>,
    ) -> Result<Vec<Uuid>, Error> {
        let mut buf = vec![abi::Uuid::NIL; 64];
        loop {
            let mut count = 0;
            // SAFETY: valid host function, context of this call, string and a buffer of
            // `buf.len()` entries.
            Error::check(unsafe {
                (self.api().entities_near)(
                    self.raw,
                    world.0,
                    center.to_abi(),
                    radius,
                    abi::Str::new(kind.unwrap_or("")),
                    buf.as_mut_ptr(),
                    buf.len(),
                    &mut count,
                )
            })?;
            if count <= buf.len() {
                return Ok(buf.iter().take(count).map(|u| Uuid::from_abi(*u)).collect());
            }
            buf.resize(count, abi::Uuid::NIL);
        }
    }

    fn snbt(&mut self, f: impl FnOnce(*const abi::ByteSink) -> Status) -> Result<String, Error> {
        String::from_utf8(collect_with(f)?).map_err(|_| Error::InvalidArgument)
    }

    /// An entity's data as `/data get entity` shows it, as SNBT. Since ABI 1.5.
    pub fn entity_nbt(&mut self, world: World, entity: Uuid) -> Result<String, Error> {
        let (api, raw) = (self.api(), self.raw);
        // SAFETY: valid host function, context of this call and sink.
        self.snbt(|sink| unsafe { (api.entity_nbt)(raw, world.0, entity.to_abi(), sink) })
    }

    /// Merges SNBT into an entity's data as `/data merge entity` (not players), e.g.
    /// `{Glowing:1b,CustomName:"Bob"}`. Since ABI 1.5.
    pub fn merge_entity_nbt(&mut self, world: World, entity: Uuid, nbt: &str) -> Result<(), Error> {
        // SAFETY: valid host function, context of this call and string.
        Error::check(unsafe {
            (self.api().entity_nbt_merge)(self.raw, world.0, entity.to_abi(), abi::Str::new(nbt))
        })
    }

    /// Gives a living entity an effect. Since ABI 1.5.
    pub fn add_effect(&mut self, world: World, entity: Uuid, effect: &Effect) -> Result<(), Error> {
        // SAFETY: valid host function, context of this call and string.
        Error::check(unsafe {
            (self.api().entity_effect_add)(
                self.raw,
                world.0,
                entity.to_abi(),
                abi::Str::new(&effect.effect),
                effect.duration,
                effect.amplifier,
                effect.flags(),
            )
        })
    }

    /// Removes an effect (`None`: all) from a living entity. Since ABI 1.5.
    pub fn remove_effect(
        &mut self,
        world: World,
        entity: Uuid,
        effect: Option<&str>,
    ) -> Result<(), Error> {
        // SAFETY: valid host function, context of this call and string.
        Error::check(unsafe {
            (self.api().entity_effect_remove)(
                self.raw,
                world.0,
                entity.to_abi(),
                abi::Str::new(effect.unwrap_or("")),
            )
        })
    }

    /// Gives a player an item as `/give` does; the item as SNBT, e.g.
    /// `{id:"minecraft:diamond",count:3}`. Since ABI 1.5.
    pub fn give_item(&mut self, world: World, player: Uuid, item: &str) -> Result<(), Error> {
        // SAFETY: valid host function, context of this call and string.
        Error::check(unsafe {
            (self.api().player_give)(self.raw, world.0, player.to_abi(), abi::Str::new(item))
        })
    }

    /// The item in a slot of an entity as SNBT (`{}` for none); slots are named as `/item`
    /// names them (`container.0`, `weapon.mainhand`, `armor.head`, `enderchest.3`). Since
    /// ABI 1.5.
    pub fn item(&mut self, world: World, entity: Uuid, slot: &str) -> Result<String, Error> {
        let (api, raw) = (self.api(), self.raw);
        // SAFETY: valid host function, context of this call, string and sink.
        self.snbt(|sink| unsafe {
            (api.entity_item_get)(raw, world.0, entity.to_abi(), abi::Str::new(slot), sink)
        })
    }

    /// Puts an item (SNBT, `{}` for none) into a slot of an entity. Since ABI 1.5.
    pub fn set_item(
        &mut self,
        world: World,
        entity: Uuid,
        slot: &str,
        item: &str,
    ) -> Result<(), Error> {
        // SAFETY: valid host function, context of this call and strings.
        Error::check(unsafe {
            (self.api().entity_item_set)(
                self.raw,
                world.0,
                entity.to_abi(),
                abi::Str::new(slot),
                abi::Str::new(item),
            )
        })
    }

    /// The block entity at a position as SNBT (signs, chests, spawners, ...). Since ABI 1.5.
    pub fn block_entity_nbt(&mut self, world: World, pos: BlockPos) -> Result<String, Error> {
        let (api, raw) = (self.api(), self.raw);
        // SAFETY: valid host function, context of this call and sink.
        self.snbt(|sink| unsafe { (api.block_entity_nbt)(raw, world.0, pos.to_abi(), sink) })
    }

    /// Merges SNBT into a block entity as `/data merge block`. Since ABI 1.5.
    pub fn merge_block_entity_nbt(
        &mut self,
        world: World,
        pos: BlockPos,
        nbt: &str,
    ) -> Result<(), Error> {
        // SAFETY: valid host function, context of this call and string.
        Error::check(unsafe {
            (self.api().block_entity_merge)(self.raw, world.0, pos.to_abi(), abi::Str::new(nbt))
        })
    }

    /// Grants or revokes a player's advancement criterion (`None`: the whole advancement),
    /// as `/advancement ... only`. Since ABI 1.5.
    pub fn set_advancement(
        &mut self,
        world: World,
        player: Uuid,
        advancement: &str,
        criterion: Option<&str>,
        grant: bool,
    ) -> Result<(), Error> {
        // SAFETY: valid host function, context of this call and strings.
        Error::check(unsafe {
            (self.api().advancement_set)(
                self.raw,
                world.0,
                player.to_abi(),
                abi::Str::new(advancement),
                abi::Str::new(criterion.unwrap_or("")),
                u32::from(grant),
            )
        })
    }

    /// Whether a player has completed an advancement. Since ABI 1.5.
    pub fn advancement_done(
        &mut self,
        world: World,
        player: Uuid,
        advancement: &str,
    ) -> Result<bool, Error> {
        let mut out = 0;
        // SAFETY: valid host function, context of this call, string and output.
        Error::check(unsafe {
            (self.api().advancement_done)(
                self.raw,
                world.0,
                player.to_abi(),
                abi::Str::new(advancement),
                &mut out,
            )
        })?;
        Ok(out != 0)
    }
}
