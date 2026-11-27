//! The host's C functions ([`abi::HostApi`]) and the per-call context.
//!
//! Every function catches panics of the host (a panic must not unwind into plugin code) and
//! checks its pointers. Thread-safe functions resolve the calling plugin from its id;
//! owner-bound functions read the [`CallCtx`] the host passed to the current callback.

use std::ffi::c_void;
use std::panic::{self, AssertUnwindSafe};
use std::path::Path;
use std::sync::{Arc, PoisonError};

use vanillite_abi::{self as abi, Status, status};

use vanillite_nbt::Tag;

use crate::{BlockBox, Inner, Plugin, SetMode, Text, Title, WorldAccess};

/// The table every plugin gets in `load`.
pub(crate) static HOST_API: abi::HostApi = abi::HostApi {
    size: abi::size_of::<abi::HostApi>(),
    abi_major: abi::ABI_MAJOR,
    abi_minor: abi::ABI_MINOR,
    log,
    data_dir,
    config_load,
    config_string,
    config_int,
    config_float,
    config_bool,
    server_rule,
    online_players,
    player_name,
    player_world,
    player_send_message,
    player_kick,
    player_has_permission,
    broadcast_message,
    world_name,
    block_state_id,
    run_command,
    schedule,
    cancel_task,
    subscribe,
    register_command,
    set_permission_provider,
    provide_chunk_generator,
    owner_world,
    block_get,
    block_set,
    player_position,
    player_set_tab_list,
    block_state_name,
    block_get_id,
    block_set_id,
    blocks_get,
    blocks_set,
    player_send_action_bar,
    player_send_title,
    entity_data_get,
    entity_data_set,
    entity_data_remove,
    entity_data_keys,
};

/// What a callback may touch: when the call runs on a world's owner, that world.
pub(crate) struct CallCtx<'w, 'p> {
    pub(crate) world: Option<&'w mut dyn WorldAccess>,
    /// The plugin called.
    pub(crate) plugin: &'p Plugin,
}

impl<'w, 'p> CallCtx<'w, 'p> {
    pub(crate) fn new(world: Option<&'w mut dyn WorldAccess>, plugin: &'p Plugin) -> Self {
        CallCtx { world, plugin }
    }

    /// The pointer handed to the plugin; valid while `self` is borrowed.
    pub(crate) fn as_raw(&mut self) -> *mut abi::Context {
        std::ptr::from_mut(self).cast()
    }
}

/// The context of the current callback.
///
/// # Safety
///
/// `raw` must be null or come from [`CallCtx::as_raw`] of a context that is still borrowed by
/// the running callback.
unsafe fn ctx<'a>(raw: *mut abi::Context) -> Option<&'a mut CallCtx<'a, 'a>> {
    // SAFETY: guaranteed by the caller.
    unsafe { raw.cast::<CallCtx<'a, 'a>>().as_mut() }
}

/// Reborrows an optional world for one call.
pub(crate) fn reborrow<'s>(
    world: &'s mut Option<&mut dyn WorldAccess>,
) -> Option<&'s mut dyn WorldAccess> {
    match world {
        Some(w) => Some(&mut **w),
        None => None,
    }
}

/// Runs `f`, turning a host panic into [`status::ERROR`].
fn guard(f: impl FnOnce() -> Status) -> Status {
    panic::catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|_| {
        log::error!("A host function panicked; the call failed");
        status::ERROR
    })
}

/// Resolves the calling plugin and runs `f` with it.
fn with(id: abi::PluginId, f: impl FnOnce(&Arc<Inner>, &Arc<Plugin>) -> Status) -> Status {
    guard(|| match Inner::resolve(id) {
        Some((inner, plugin)) => f(&inner, &plugin),
        None => status::INVALID_ARGUMENT,
    })
}

/// A borrowed ABI string as `&str`; `None` for invalid UTF-8.
///
/// # Safety
///
/// `s` must point to `s.len` readable bytes that outlive `'a`.
pub(crate) unsafe fn str_checked<'a>(s: abi::Str) -> Option<&'a str> {
    if s.len == 0 || s.ptr.is_null() {
        return Some("");
    }
    // SAFETY: guaranteed by the caller.
    std::str::from_utf8(unsafe { std::slice::from_raw_parts(s.ptr, s.len) }).ok()
}

/// A borrowed ABI string as `&str`; invalid UTF-8 becomes empty.
///
/// # Safety
///
/// As [`str_checked`].
pub(crate) unsafe fn str<'a>(s: abi::Str) -> &'a str {
    // SAFETY: guaranteed by the caller.
    unsafe { str_checked(s) }.unwrap_or_default()
}

/// Writes a string into a caller buffer (see [`abi::StrBuf`]).
fn write_str(out: *mut abi::StrBuf, s: &str) -> Status {
    // SAFETY: the plugin passes a valid buffer record or null.
    let Some(out) = (unsafe { out.as_mut() }) else {
        return status::INVALID_ARGUMENT;
    };
    let n = s.len().min(out.cap);
    if n > 0 && !out.ptr.is_null() {
        // SAFETY: the buffer has `cap` writable bytes.
        unsafe { std::ptr::copy_nonoverlapping(s.as_ptr(), out.ptr, n) };
    }
    out.len = s.len();
    status::OK
}

/// Writes a value to an output pointer.
fn write<T>(out: *mut T, value: T) -> Status {
    if out.is_null() {
        return status::INVALID_ARGUMENT;
    }
    // SAFETY: non-null; the plugin passes a valid output for the call.
    unsafe { out.write(value) };
    status::OK
}

fn text<'a>(s: abi::Str, format: u32) -> Option<Text<'a>> {
    // SAFETY: the plugin's string is valid for the call.
    let s = unsafe { str_checked(s) }?;
    match format {
        abi::text_format::PLAIN => Some(Text::Plain(s)),
        abi::text_format::JSON => Some(Text::Json(s)),
        _ => None,
    }
}

/// The server, or `WRONG_PHASE` before it runs.
macro_rules! server {
    ($inner:expr) => {
        match $inner.server() {
            Some(server) => server,
            None => return status::WRONG_PHASE,
        }
    };
}

unsafe extern "C" fn log(id: abi::PluginId, level: u32, message: abi::Str) {
    let _ = with(id, |_, plugin| {
        let level = match level {
            abi::log_level::ERROR => log::Level::Error,
            abi::log_level::WARN => log::Level::Warn,
            abi::log_level::DEBUG => log::Level::Debug,
            abi::log_level::TRACE => log::Level::Trace,
            _ => log::Level::Info,
        };
        // SAFETY: the plugin's string is valid for the call.
        let message = unsafe { str(message) };
        log::log!(target: "vanillite::plugin", level, "[{}] {message}", plugin.id());
        status::OK
    });
}

unsafe extern "C" fn data_dir(id: abi::PluginId, out: *mut abi::StrBuf) -> Status {
    with(id, |_, plugin| {
        write_str(out, &plugin.data_dir.to_string_lossy())
    })
}

/// Largest plugin config read.
const MAX_CONFIG: u64 = 1024 * 1024;

unsafe extern "C" fn config_load(id: abi::PluginId, defaults: abi::Str) -> Status {
    with(id, |_, plugin| {
        // SAFETY: the plugin's string is valid for the call.
        let Some(defaults) = (unsafe { str_checked(defaults) }) else {
            return status::INVALID_ARGUMENT;
        };
        let path = plugin.data_dir.join("config.toml");
        if !path.exists()
            && !defaults.is_empty()
            && let Err(e) = std::fs::write(&path, defaults)
        {
            log::error!("[{}] Cannot write {}: {e}", plugin.id(), path.display());
            return status::ERROR;
        }
        match read_config(&path) {
            Ok(table) => {
                *plugin.config.lock().unwrap_or_else(PoisonError::into_inner) = Some(table);
                status::OK
            }
            Err(e) => {
                log::error!("[{}] {}: {e}", plugin.id(), path.display());
                status::INVALID_ARGUMENT
            }
        }
    })
}

fn read_config(path: &Path) -> Result<toml::Table, String> {
    use std::io::Read as _;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(toml::Table::new()),
        Err(e) => return Err(e.to_string()),
    };
    let mut text = String::new();
    file.take(MAX_CONFIG)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    text.parse().map_err(|e: toml::de::Error| e.to_string())
}

/// Looks up a dotted key in the plugin's config and converts it with `f`.
fn config_value(
    id: abi::PluginId,
    key: abi::Str,
    f: impl FnOnce(&toml::Value) -> Status,
) -> Status {
    with(id, |_, plugin| {
        // SAFETY: the plugin's string is valid for the call.
        let Some(key) = (unsafe { str_checked(key) }) else {
            return status::INVALID_ARGUMENT;
        };
        let config = plugin.config.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(table) = config.as_ref() else {
            return status::WRONG_PHASE;
        };
        let mut parts = key.split('.');
        let first = parts.next().and_then(|k| table.get(k));
        match parts.try_fold(first, |v, k| Some(v.and_then(|v| v.get(k)))) {
            Some(Some(value)) => f(value),
            _ => status::NOT_FOUND,
        }
    })
}

unsafe extern "C" fn config_string(
    id: abi::PluginId,
    key: abi::Str,
    out: *mut abi::StrBuf,
) -> Status {
    config_value(id, key, |v| match v {
        toml::Value::String(s) => write_str(out, s),
        _ => status::INVALID_ARGUMENT,
    })
}

unsafe extern "C" fn config_int(id: abi::PluginId, key: abi::Str, out: *mut i64) -> Status {
    config_value(id, key, |v| match v {
        toml::Value::Integer(i) => write(out, *i),
        _ => status::INVALID_ARGUMENT,
    })
}

unsafe extern "C" fn config_float(id: abi::PluginId, key: abi::Str, out: *mut f64) -> Status {
    config_value(id, key, |v| match v {
        toml::Value::Float(f) => write(out, *f),
        toml::Value::Integer(i) => write(out, *i as f64),
        _ => status::INVALID_ARGUMENT,
    })
}

unsafe extern "C" fn config_bool(id: abi::PluginId, key: abi::Str, out: *mut u32) -> Status {
    config_value(id, key, |v| match v {
        toml::Value::Boolean(b) => write(out, u32::from(*b)),
        _ => status::INVALID_ARGUMENT,
    })
}

unsafe extern "C" fn server_rule(
    id: abi::PluginId,
    key: abi::Str,
    out: *mut abi::StrBuf,
) -> Status {
    with(id, |inner, _| {
        // SAFETY: the plugin's string is valid for the call.
        let key = unsafe { str(key) };
        match server!(inner).rule(key) {
            Some(value) => write_str(out, &value),
            None => status::NOT_FOUND,
        }
    })
}

unsafe extern "C" fn online_players(
    id: abi::PluginId,
    out: *mut abi::Uuid,
    cap: usize,
    count: *mut usize,
) -> Status {
    with(id, |inner, _| {
        let players = server!(inner).online_players();
        if !out.is_null() {
            for (i, player) in players.iter().take(cap).enumerate() {
                // SAFETY: the buffer has `cap` entries.
                unsafe { out.add(i).write(abi::Uuid::from_u128(*player)) };
            }
        }
        write(count, players.len())
    })
}

unsafe extern "C" fn player_name(
    id: abi::PluginId,
    player: abi::Uuid,
    out: *mut abi::StrBuf,
) -> Status {
    with(id, |inner, _| {
        match server!(inner).player_name(player.to_u128()) {
            Some(name) => write_str(out, &name),
            None => status::NOT_FOUND,
        }
    })
}

unsafe extern "C" fn player_world(id: abi::PluginId, player: abi::Uuid, out: *mut u32) -> Status {
    with(id, |inner, _| {
        match server!(inner).player_world(player.to_u128()) {
            Some(world) => write(out, world),
            None => status::NOT_FOUND,
        }
    })
}

unsafe extern "C" fn player_send_message(
    id: abi::PluginId,
    player: abi::Uuid,
    message: abi::Str,
    format: u32,
) -> Status {
    with(id, |inner, _| {
        let Some(text) = text(message, format) else {
            return status::INVALID_ARGUMENT;
        };
        if server!(inner).send_message(player.to_u128(), text) {
            status::OK
        } else {
            status::NOT_FOUND
        }
    })
}

unsafe extern "C" fn player_kick(
    id: abi::PluginId,
    player: abi::Uuid,
    reason: abi::Str,
    format: u32,
) -> Status {
    with(id, |inner, _| {
        let Some(text) = text(reason, format) else {
            return status::INVALID_ARGUMENT;
        };
        if server!(inner).kick(player.to_u128(), text) {
            status::OK
        } else {
            status::NOT_FOUND
        }
    })
}

unsafe extern "C" fn player_set_tab_list(
    id: abi::PluginId,
    player: abi::Uuid,
    header: abi::Str,
    footer: abi::Str,
    format: u32,
) -> Status {
    with(id, |inner, _| {
        let (Some(header), Some(footer)) = (text(header, format), text(footer, format)) else {
            return status::INVALID_ARGUMENT;
        };
        if server!(inner).set_tab_list(player.to_u128(), header, footer) {
            status::OK
        } else {
            status::NOT_FOUND
        }
    })
}

unsafe extern "C" fn player_send_action_bar(
    id: abi::PluginId,
    player: abi::Uuid,
    message: abi::Str,
    format: u32,
) -> Status {
    with(id, |inner, _| {
        let Some(text) = text(message, format) else {
            return status::INVALID_ARGUMENT;
        };
        if server!(inner).send_action_bar(player.to_u128(), text) {
            status::OK
        } else {
            status::NOT_FOUND
        }
    })
}

unsafe extern "C" fn player_send_title(
    id: abi::PluginId,
    player: abi::Uuid,
    spec: *const abi::TitleSpec,
) -> Status {
    with(id, |inner, _| {
        // SAFETY: the plugin passes a valid record or null.
        let Some(spec) = (unsafe { spec.as_ref() }) else {
            return status::INVALID_ARGUMENT;
        };
        if (spec.size as usize) < size_of::<abi::TitleSpec>() {
            return status::INVALID_ARGUMENT;
        }
        let part = |s: abi::Str| (s.len > 0).then(|| text(s, spec.format));
        let (Some(title), Some(subtitle)) = (
            part(spec.title).map_or(Some(None), |t| t.map(Some)),
            part(spec.subtitle).map_or(Some(None), |t| t.map(Some)),
        ) else {
            return status::INVALID_ARGUMENT;
        };
        let title = Title {
            title,
            subtitle,
            times: [spec.fade_in, spec.stay, spec.fade_out],
        };
        if server!(inner).send_title(player.to_u128(), &title) {
            status::OK
        } else {
            status::NOT_FOUND
        }
    })
}

unsafe extern "C" fn player_has_permission(
    id: abi::PluginId,
    player: abi::Uuid,
    node: abi::Str,
    out: *mut u32,
) -> Status {
    with(id, |inner, _| {
        // SAFETY: the plugin's string is valid for the call.
        let node = unsafe { str(node) };
        let operator = server!(inner).is_operator(player.to_u128());
        write(
            out,
            u32::from(has_permission(inner, player.to_u128(), node, operator)),
        )
    })
}

/// The permission provider's answer, or the default (operators have every node).
pub(crate) fn has_permission(inner: &Inner, player: u128, node: &str, operator: bool) -> bool {
    let provider = inner
        .permission
        .read()
        .unwrap_or_else(PoisonError::into_inner);
    let Some(provider) = provider.as_ref() else {
        return operator;
    };
    let Some(plugin) = inner.plugin(provider.plugin) else {
        return operator;
    };
    let mut answer = abi::permission::UNDEFINED;
    let s = plugin.call("permission check", || {
        // SAFETY: the provider's function and user data are valid for the life of its library.
        unsafe {
            (provider.check)(
                provider.user.0,
                abi::Uuid::from_u128(player),
                abi::Str::new(node),
                &mut answer,
            )
        }
    });
    match (s, answer) {
        (status::OK, abi::permission::ALLOW) => true,
        (status::OK, abi::permission::DENY) => false,
        _ => operator,
    }
}

unsafe extern "C" fn broadcast_message(
    id: abi::PluginId,
    message: abi::Str,
    format: u32,
) -> Status {
    with(id, |inner, _| {
        let Some(text) = text(message, format) else {
            return status::INVALID_ARGUMENT;
        };
        server!(inner).broadcast(text);
        status::OK
    })
}

unsafe extern "C" fn world_name(id: abi::PluginId, world: u32, out: *mut abi::StrBuf) -> Status {
    with(id, |inner, _| match server!(inner).world_name(world) {
        Some(name) => write_str(out, &name),
        None => status::NOT_FOUND,
    })
}

unsafe extern "C" fn block_state_id(id: abi::PluginId, state: abi::Str, out: *mut u32) -> Status {
    with(id, |inner, _| {
        // SAFETY: the plugin's string is valid for the call.
        let state = unsafe { str(state) };
        match server!(inner).block_state_id(state) {
            Some(v) => write(out, v),
            None => status::INVALID_ARGUMENT,
        }
    })
}

unsafe extern "C" fn block_state_name(
    id: abi::PluginId,
    state: u32,
    out: *mut abi::StrBuf,
) -> Status {
    with(id, |inner, _| {
        match server!(inner).block_state_name(state) {
            Some(name) => write_str(out, &name),
            None => status::INVALID_ARGUMENT,
        }
    })
}

unsafe extern "C" fn run_command(id: abi::PluginId, command: abi::Str) -> Status {
    with(id, |inner, plugin| {
        // SAFETY: the plugin's string is valid for the call.
        let Some(command) = (unsafe { str_checked(command) }) else {
            return status::INVALID_ARGUMENT;
        };
        log::info!("[{}] runs the console command: {command}", plugin.id());
        server!(inner).run_command(command);
        status::OK
    })
}

unsafe extern "C" fn schedule(
    id: abi::PluginId,
    spec: *const abi::TaskSpec,
    out: *mut u64,
) -> Status {
    // SAFETY: the plugin passes a valid spec or null.
    let Some(spec) = (unsafe { spec.as_ref() }) else {
        return status::INVALID_ARGUMENT;
    };
    let drop_user = || {
        if let Some(drop) = spec.drop {
            // SAFETY: the plugin's drop function for its own user data.
            unsafe { drop(spec.user) };
        }
    };
    if (spec.size as usize) < size_of::<abi::TaskSpec>() {
        drop_user();
        return status::INVALID_ARGUMENT;
    }
    let result = with(id, |inner, plugin| {
        match inner.tasks.schedule(inner, plugin, spec) {
            Ok(task) => write(out, task),
            Err(s) => s,
        }
    });
    if result != status::OK {
        drop_user();
    }
    result
}

unsafe extern "C" fn cancel_task(id: abi::PluginId, task: u64) -> Status {
    with(id, |inner, plugin| {
        inner.tasks.cancel(inner, plugin.local, task)
    })
}

unsafe extern "C" fn subscribe(
    id: abi::PluginId,
    kind: u32,
    priority: i32,
    callback: abi::EventFn,
    user: *mut c_void,
) -> Status {
    with(id, |inner, plugin| {
        if !inner.may_register(plugin) {
            return status::WRONG_PHASE;
        }
        inner
            .events
            .add(plugin.local, kind, priority, callback, user)
    })
}

unsafe extern "C" fn register_command(id: abi::PluginId, spec: *const abi::CommandSpec) -> Status {
    with(id, |inner, plugin| {
        if !inner.may_register(plugin) {
            return status::WRONG_PHASE;
        }
        // SAFETY: the plugin passes a valid spec or null.
        let Some(spec) = (unsafe { spec.as_ref() }) else {
            return status::INVALID_ARGUMENT;
        };
        inner.commands.register(inner, plugin, spec)
    })
}

unsafe extern "C" fn set_permission_provider(
    id: abi::PluginId,
    check: abi::PermissionFn,
    user: *mut c_void,
) -> Status {
    with(id, |inner, plugin| {
        if !inner.may_register(plugin) {
            return status::WRONG_PHASE;
        }
        let mut provider = inner
            .permission
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(existing) = provider.as_ref()
            && existing.plugin != plugin.local
        {
            let other = inner.plugin(existing.plugin).map(|p| p.id().to_owned());
            log::error!(
                "Plugin {} cannot become the permission provider: {} already is",
                plugin.id(),
                other.unwrap_or_default()
            );
            return status::ERROR;
        }
        *provider = Some(crate::PermissionProvider {
            plugin: plugin.local,
            check,
            user: crate::SendPtr(user),
        });
        status::OK
    })
}

unsafe extern "C" fn provide_chunk_generator(
    id: abi::PluginId,
    table: *const abi::ChunkGeneratorProvider,
) -> Status {
    with(id, |inner, plugin| {
        if inner.is_reloading(plugin) {
            // The slot was filled at startup; the old build's provider stays.
            log::warn!(
                "[{}] a new chunk generator takes effect after a restart",
                plugin.id()
            );
            return status::OK;
        }
        if !inner.may_register(plugin) {
            return status::WRONG_PHASE;
        }
        // SAFETY: the plugin passes a valid table or null.
        let Some(table) = (unsafe { table.as_ref() }) else {
            return status::INVALID_ARGUMENT;
        };
        if (table.size as usize) < size_of::<abi::ChunkGeneratorProvider>() {
            return status::INVALID_ARGUMENT;
        }
        if !plugin
            .manifest
            .provides
            .iter()
            .any(|p| p == "chunk-generator")
        {
            log::error!(
                "Plugin {} provides a chunk generator but does not declare it: add \
                 provides = [\"chunk-generator\"] to its plugin.toml",
                plugin.id()
            );
            return status::UNSUPPORTED;
        }
        if table.data_version != 0 && table.data_version != inner.settings.data_version {
            log::error!(
                "Plugin {}: its chunk generator is built for data version {}, this server has {}",
                plugin.id(),
                table.data_version,
                inner.settings.data_version
            );
            return status::UNSUPPORTED;
        }
        let mut slot = inner
            .generator
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some((other, _)) = slot.as_ref() {
            let other = inner
                .plugin(*other)
                .map(|p| p.id().to_owned())
                .unwrap_or_default();
            log::error!(
                "Slot chunk-generator is already filled by plugin {other}; plugin {} cannot replace it",
                plugin.id()
            );
            return status::ERROR;
        }
        *slot = Some((plugin.local, *table));
        status::OK
    })
}

/// The owned world of a context, if it is `world`.
fn owned<'a>(raw: *mut abi::Context, world: u32) -> Result<&'a mut dyn WorldAccess, Status> {
    // SAFETY: the plugin passes the context of the running callback.
    let ctx = unsafe { ctx(raw) }.ok_or(status::INVALID_ARGUMENT)?;
    match ctx.world.as_deref_mut() {
        Some(w) if w.id() == world => Ok(w),
        _ => Err(status::WRONG_OWNER),
    }
}

unsafe extern "C" fn owner_world(raw: *mut abi::Context, out: *mut u32) -> Status {
    guard(|| {
        // SAFETY: the plugin passes the context of the running callback.
        let Some(ctx) = (unsafe { ctx(raw) }) else {
            return status::INVALID_ARGUMENT;
        };
        match ctx.world.as_deref() {
            Some(w) => write(out, w.id()),
            None => status::NOT_FOUND,
        }
    })
}

unsafe extern "C" fn block_get(
    raw: *mut abi::Context,
    world: u32,
    pos: abi::BlockPos,
    out: *mut abi::StrBuf,
) -> Status {
    guard(|| match owned(raw, world) {
        Ok(w) => match w.block([pos.x, pos.y, pos.z]) {
            Some(state) => write_str(out, &state),
            None => status::NOT_FOUND,
        },
        Err(s) => s,
    })
}

unsafe extern "C" fn block_set(
    raw: *mut abi::Context,
    world: u32,
    pos: abi::BlockPos,
    state: abi::Str,
) -> Status {
    guard(|| match owned(raw, world) {
        Ok(w) => {
            // SAFETY: the plugin's string is valid for the call.
            let Some(state) = (unsafe { str_checked(state) }) else {
                return status::INVALID_ARGUMENT;
            };
            match w.set_block([pos.x, pos.y, pos.z], state) {
                Ok(()) => status::OK,
                Err(s) => s,
            }
        }
        Err(s) => s,
    })
}

unsafe extern "C" fn block_get_id(
    raw: *mut abi::Context,
    world: u32,
    pos: abi::BlockPos,
    out: *mut u32,
) -> Status {
    guard(|| match owned(raw, world) {
        Ok(w) => match w.block_id([pos.x, pos.y, pos.z]) {
            Some(state) => write(out, state),
            None => status::NOT_FOUND,
        },
        Err(s) => s,
    })
}

unsafe extern "C" fn block_set_id(
    raw: *mut abi::Context,
    world: u32,
    pos: abi::BlockPos,
    state: u32,
    mode: u32,
) -> Status {
    guard(|| match owned(raw, world) {
        Ok(w) => match SetMode::from_abi(mode) {
            Some(mode) => match w.set_block_id([pos.x, pos.y, pos.z], state, mode) {
                Ok(_) => status::OK,
                Err(s) => s,
            },
            None => status::INVALID_ARGUMENT,
        },
        Err(s) => s,
    })
}

/// The box behind `area` and its volume, if it is valid and not larger than
/// [`abi::limits::MAX_BOX_VOLUME`].
fn block_box(area: *const abi::BlockBox) -> Option<(BlockBox, usize)> {
    // SAFETY: the plugin passes a valid record or null.
    let area = unsafe { area.as_ref() }?;
    let (min, max) = (area.min, area.max);
    let size = |lo: i32, hi: i32| (hi >= lo).then(|| u64::from(hi.abs_diff(lo)) + 1);
    let volume = size(min.x, max.x)? * size(min.y, max.y)? * size(min.z, max.z)?;
    if volume > u64::from(abi::limits::MAX_BOX_VOLUME) {
        return None;
    }
    let area = BlockBox {
        min: [min.x, min.y, min.z],
        max: [max.x, max.y, max.z],
    };
    Some((area, usize::try_from(volume).ok()?))
}

unsafe extern "C" fn blocks_get(
    raw: *mut abi::Context,
    world: u32,
    area: *const abi::BlockBox,
    out: *mut u32,
    cap: usize,
) -> Status {
    guard(|| match owned(raw, world) {
        Ok(w) => {
            let Some((area, volume)) = block_box(area) else {
                return status::INVALID_ARGUMENT;
            };
            if out.is_null() || cap < volume {
                return status::INVALID_ARGUMENT;
            }
            // SAFETY: the plugin passes `cap` writable entries.
            let out = unsafe { std::slice::from_raw_parts_mut(out, volume) };
            w.blocks(area, out);
            status::OK
        }
        Err(s) => s,
    })
}

unsafe extern "C" fn blocks_set(
    raw: *mut abi::Context,
    world: u32,
    area: *const abi::BlockBox,
    states: *const u32,
    len: usize,
    mode: u32,
    changed: *mut usize,
) -> Status {
    guard(|| match owned(raw, world) {
        Ok(w) => {
            let Some((area, volume)) = block_box(area) else {
                return status::INVALID_ARGUMENT;
            };
            let Some(mode) = SetMode::from_abi(mode) else {
                return status::INVALID_ARGUMENT;
            };
            if states.is_null() || len != volume || changed.is_null() {
                return status::INVALID_ARGUMENT;
            }
            // SAFETY: the plugin passes `len` readable entries.
            let states = unsafe { std::slice::from_raw_parts(states, len) };
            match w.set_blocks(area, states, mode) {
                Ok(n) => write(changed, n),
                Err(s) => s,
            }
        }
        Err(s) => s,
    })
}

unsafe extern "C" fn player_position(
    raw: *mut abi::Context,
    player: abi::Uuid,
    out: *mut abi::Vec3,
) -> Status {
    guard(|| {
        // SAFETY: the plugin passes the context of the running callback.
        let Some(ctx) = (unsafe { ctx(raw) }) else {
            return status::INVALID_ARGUMENT;
        };
        let Some(world) = ctx.world.as_deref() else {
            return status::WRONG_OWNER;
        };
        match world.player_position(player.to_u128()) {
            Some([x, y, z]) => write(out, abi::Vec3 { x, y, z }),
            None => status::WRONG_OWNER,
        }
    })
}

/// A [`abi::ByteSinkFn`] that appends to the `Vec<u8>` behind `state`.
pub(crate) unsafe extern "C" fn collect_bytes(state: *mut c_void, data: abi::Bytes) {
    let _ = guard(|| {
        // SAFETY: the host passes its own `Vec<u8>` as state for the call.
        let Some(out) = (unsafe { state.cast::<Vec<u8>>().as_mut() }) else {
            return status::INVALID_ARGUMENT;
        };
        if data.len > 0 && !data.ptr.is_null() {
            // SAFETY: the plugin passes bytes valid for the call.
            out.extend_from_slice(unsafe { std::slice::from_raw_parts(data.ptr, data.len) });
        }
        status::OK
    });
}

/// The full persistent data key of a plugin's key: `namespace:path` as given, or the plugin's
/// id as namespace. `None` for keys that are no resource location or too long.
fn data_key(plugin: &Plugin, key: abi::Str) -> Option<String> {
    // SAFETY: the plugin's string is valid for the call.
    let key = unsafe { str_checked(key) }?;
    let (namespace, path) = key.split_once(':').unwrap_or((plugin.id(), key));
    let valid = |s: &str, slash: bool| {
        !s.is_empty()
            && s.bytes().all(|b| {
                b.is_ascii_lowercase()
                    || b.is_ascii_digit()
                    || matches!(b, b'_' | b'-' | b'.')
                    || (slash && b == b'/')
            })
    };
    let full = format!("{namespace}:{path}");
    (valid(namespace, false)
        && valid(path, true)
        && full.len() <= abi::data_limits::MAX_KEY as usize)
        .then_some(full)
}

/// A persistent data value from its type and bytes.
fn data_tag(kind: u32, bytes: &[u8]) -> Option<Tag> {
    fn array<const N: usize, T>(bytes: &[u8], f: fn([u8; N]) -> T) -> Option<Vec<T>> {
        let (chunks, rest) = bytes.as_chunks::<N>();
        rest.is_empty()
            .then(|| chunks.iter().map(|c| f(*c)).collect())
    }
    use abi::data_type as t;
    Some(match kind {
        t::BYTE => Tag::Byte(i8::from_le_bytes(bytes.try_into().ok()?)),
        t::SHORT => Tag::Short(i16::from_le_bytes(bytes.try_into().ok()?)),
        t::INT => Tag::Int(i32::from_le_bytes(bytes.try_into().ok()?)),
        t::LONG => Tag::Long(i64::from_le_bytes(bytes.try_into().ok()?)),
        t::FLOAT => Tag::Float(f32::from_le_bytes(bytes.try_into().ok()?)),
        t::DOUBLE => Tag::Double(f64::from_le_bytes(bytes.try_into().ok()?)),
        t::BYTE_ARRAY => Tag::ByteArray(array(bytes, i8::from_le_bytes)?),
        t::STRING => Tag::String(std::str::from_utf8(bytes).ok()?.to_owned()),
        t::INT_ARRAY => Tag::IntArray(array(bytes, i32::from_le_bytes)?),
        t::LONG_ARRAY => Tag::LongArray(array(bytes, i64::from_le_bytes)?),
        _ => return None,
    })
}

/// The type and bytes of a persistent data value; `None` for lists and compounds (written
/// by other servers), which plugins cannot read here.
fn tag_data(tag: &Tag) -> Option<(u32, Vec<u8>)> {
    use abi::data_type as t;
    Some(match tag {
        Tag::Byte(v) => (t::BYTE, v.to_le_bytes().to_vec()),
        Tag::Short(v) => (t::SHORT, v.to_le_bytes().to_vec()),
        Tag::Int(v) => (t::INT, v.to_le_bytes().to_vec()),
        Tag::Long(v) => (t::LONG, v.to_le_bytes().to_vec()),
        Tag::Float(v) => (t::FLOAT, v.to_le_bytes().to_vec()),
        Tag::Double(v) => (t::DOUBLE, v.to_le_bytes().to_vec()),
        Tag::ByteArray(v) => (
            t::BYTE_ARRAY,
            v.iter().flat_map(|b| b.to_le_bytes()).collect(),
        ),
        Tag::String(v) => (t::STRING, v.as_bytes().to_vec()),
        Tag::IntArray(v) => (
            t::INT_ARRAY,
            v.iter().flat_map(|b| b.to_le_bytes()).collect(),
        ),
        Tag::LongArray(v) => (
            t::LONG_ARRAY,
            v.iter().flat_map(|b| b.to_le_bytes()).collect(),
        ),
        Tag::List(_) | Tag::Compound(_) => return None,
    })
}

/// Hands bytes to a plugin's sink.
fn write_sink(sink: *const abi::ByteSink, data: &[u8]) -> Status {
    // SAFETY: the plugin passes a valid sink record or null.
    let Some(sink) = (unsafe { sink.as_ref() }) else {
        return status::INVALID_ARGUMENT;
    };
    // SAFETY: the plugin's sink copies the bytes before it returns.
    unsafe { (sink.write)(sink.state, abi::Bytes::new(data)) };
    status::OK
}

/// The full data key of a context call's plugin.
fn ctx_key(raw: *mut abi::Context, key: abi::Str) -> Option<String> {
    // SAFETY: the plugin passes the context of the running callback.
    data_key(unsafe { ctx(raw) }?.plugin, key)
}

unsafe extern "C" fn entity_data_get(
    raw: *mut abi::Context,
    world: u32,
    entity: abi::Uuid,
    key: abi::Str,
    kind: *mut u32,
    sink: *const abi::ByteSink,
) -> Status {
    guard(|| {
        let Some(key) = ctx_key(raw, key) else {
            return status::INVALID_ARGUMENT;
        };
        let w = match owned(raw, world) {
            Ok(w) => w,
            Err(s) => return s,
        };
        let data = match w.entity_data(entity.to_u128()) {
            Ok(data) => data,
            Err(s) => return s,
        };
        let Some(tag) = data.and_then(|d| d.get(&key)) else {
            return status::NOT_FOUND;
        };
        let Some((t, bytes)) = tag_data(tag) else {
            return status::UNSUPPORTED;
        };
        match write(kind, t) {
            status::OK => write_sink(sink, &bytes),
            s => s,
        }
    })
}

unsafe extern "C" fn entity_data_set(
    raw: *mut abi::Context,
    world: u32,
    entity: abi::Uuid,
    key: abi::Str,
    kind: u32,
    value: abi::Bytes,
) -> Status {
    guard(|| {
        let Some(key) = ctx_key(raw, key) else {
            return status::INVALID_ARGUMENT;
        };
        if value.len > abi::data_limits::MAX_VALUE as usize
            || (value.len > 0 && value.ptr.is_null())
        {
            return status::INVALID_ARGUMENT;
        }
        let bytes = if value.len == 0 {
            &[][..]
        } else {
            // SAFETY: the plugin passes `len` readable bytes.
            unsafe { std::slice::from_raw_parts(value.ptr, value.len) }
        };
        let Some(tag) = data_tag(kind, bytes) else {
            return status::INVALID_ARGUMENT;
        };
        let w = match owned(raw, world) {
            Ok(w) => w,
            Err(s) => return s,
        };
        let data = match w.entity_data_mut(entity.to_u128()) {
            Ok(data) => data,
            Err(s) => return s,
        };
        if data.get(&key).is_none() && data.len() >= abi::data_limits::MAX_KEYS as usize {
            return status::INVALID_ARGUMENT;
        }
        data.insert(key, tag);
        status::OK
    })
}

unsafe extern "C" fn entity_data_remove(
    raw: *mut abi::Context,
    world: u32,
    entity: abi::Uuid,
    key: abi::Str,
) -> Status {
    guard(|| {
        let Some(key) = ctx_key(raw, key) else {
            return status::INVALID_ARGUMENT;
        };
        let w = match owned(raw, world) {
            Ok(w) => w,
            Err(s) => return s,
        };
        match w.entity_data_mut(entity.to_u128()) {
            Ok(data) => {
                data.remove(&key);
                status::OK
            }
            Err(s) => s,
        }
    })
}

unsafe extern "C" fn entity_data_keys(
    raw: *mut abi::Context,
    world: u32,
    entity: abi::Uuid,
    sink: *const abi::ByteSink,
) -> Status {
    guard(|| {
        let w = match owned(raw, world) {
            Ok(w) => w,
            Err(s) => return s,
        };
        let keys = match w.entity_data(entity.to_u128()) {
            Ok(data) => data
                .map(|d| d.iter().map(|(k, _)| k).collect::<Vec<_>>().join("\n"))
                .unwrap_or_default(),
            Err(s) => return s,
        };
        write_sink(sink, keys.as_bytes())
    })
}
