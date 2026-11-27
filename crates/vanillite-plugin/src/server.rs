//! The server as a plugin sees it: [`Server`] for thread-safe calls, [`Ctx`] for the owner of
//! the current call, and the scheduler.

use std::ffi::c_void;
use std::marker::PhantomData;
use std::path::PathBuf;

use vanillite_abi::{self as abi, Status, status};

use crate::ffi::{self, Host, guard, read_string};
use crate::types::{
    BlockBox, BlockPos, DataValue, Error, NO_BLOCK, SetMode, Text, Uuid, Vec3, World,
};

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

    /// All worlds with their dimension keys.
    pub fn worlds(&self) -> Vec<(World, String)> {
        (0..)
            .map(World)
            .map_while(|w| self.world_name(w).map(|name| (w, name)))
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
    _call: PhantomData<&'a mut ()>,
}

impl Ctx<'_> {
    pub(crate) fn new(raw: *mut abi::Context) -> Self {
        Ctx {
            raw,
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
}
