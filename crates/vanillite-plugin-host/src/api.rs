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

mod v16;
mod v18;
use v16::*;
use v18::*;

use crate::{
    AccessOp, BlockBox, BossBarOp, Inner, Location, Particles, Plugin, ScoreReply, ScoreboardOp,
    SetMode, Sound, Text, Title, WorldAccess,
};

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
    subscribe_flags,
    unsubscribe,
    event_register,
    event_fire,
    service_register,
    service_unregister,
    service_get,
    plugin_list,
    config_set,
    config_save,
    config_toml,
    permission_register,
    player_info,
    player_address,
    player_play_sound,
    player_stop_sound,
    player_spawn_particle,
    player_transfer,
    register_channel,
    player_send_plugin_message,
    objective_add,
    objective_remove,
    objective_display,
    score_get,
    score_set,
    score_reset,
    team_add,
    team_remove,
    team_join,
    team_leave,
    team_modify,
    team_of,
    bossbar_create,
    bossbar_remove,
    bossbar_set,
    bossbar_set_name,
    bossbar_player,
    server_stats,
    server_set_motd,
    access_set,
    access_get,
    world_time,
    world_set_time,
    world_set_weather,
    game_rule_get,
    game_rule_set,
    player_get,
    player_set,
    entity_location,
    entity_type,
    entity_teleport,
    entity_set_velocity,
    entity_spawn,
    entity_remove,
    entities_near,
    entity_nbt,
    entity_nbt_merge,
    entity_effect_add,
    entity_effect_remove,
    player_give,
    entity_item_get,
    entity_item_set,
    block_entity_nbt,
    block_entity_merge,
    advancement_set,
    advancement_done,
    entity_get,
    entity_set,
    entity_name,
    entity_set_name,
    entity_passengers,
    entity_vehicle,
    entity_mount,
    entity_dismount,
    entity_attribute,
    entity_attribute_set_base,
    entity_attribute_modifier,
    mob_target,
    mob_set_target,
    mob_navigate,
    mob_goal_add,
    mob_goal_remove,
    mob_goals_clear,
    biome_get,
    biome_set,
    light_get,
    chunk_state,
    chunk_force_load,
    chunk_ticket,
    border_get,
    border_set,
    explode,
    raytrace,
    provide_chunk_populator,
    menu_open,
    menu_item,
    menu_set_item,
    menu_close,
    item_component,
    item_set_component,
    item_data_get,
    item_data_set,
    item_data_remove,
    item_data_keys,
    packet_subscribe,
    packet_unsubscribe,
    packet_id,
    packet_name,
    packet_send,
    protocol_version,
    world_create,
    world_load,
    world_unload,
    world_count,
    world_info,
    player_move,
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
    let fallback = || default_permission(inner, node, operator);
    let Some(provider) = provider.as_ref() else {
        return fallback();
    };
    let Some(plugin) = inner.plugin(provider.plugin) else {
        return fallback();
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
        _ => fallback(),
    }
}

/// Who has a node without a provider's answer: its registered default, else operators.
fn default_permission(inner: &Inner, node: &str, operator: bool) -> bool {
    let defaults = inner
        .permission_defaults
        .read()
        .unwrap_or_else(PoisonError::into_inner);
    match defaults.get(node).copied() {
        Some(abi::permission_default::TRUE) => true,
        Some(abi::permission_default::FALSE) => false,
        Some(abi::permission_default::NOT_OP) => !operator,
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
    if spec.is_null() {
        return status::INVALID_ARGUMENT;
    }
    // SAFETY: non-null; every version of the record starts with `size`.
    let size = unsafe { spec.cast::<u32>().read() } as usize;
    let spec = if size >= size_of::<abi::TaskSpec>() {
        // SAFETY: the plugin passes a record of at least this ABI's size.
        unsafe { *spec }
    } else if size >= std::mem::offset_of!(abi::TaskSpec, retired) {
        // An ABI 1.5 record: everything before `retired`.
        // SAFETY: the plugin passes `size` readable bytes, all fields read lie within them.
        unsafe {
            abi::TaskSpec {
                size: size as u32,
                owner: (&raw const (*spec).owner).read(),
                delay: (&raw const (*spec).delay).read(),
                period: (&raw const (*spec).period).read(),
                run: (&raw const (*spec).run).read(),
                drop: (&raw const (*spec).drop).read(),
                user: (&raw const (*spec).user).read(),
                retired: None,
            }
        }
    } else {
        return status::INVALID_ARGUMENT;
    };
    let spec = &spec;
    let drop_user = || {
        if let Some(drop) = spec.drop {
            // SAFETY: the plugin's drop function for its own user data.
            unsafe { drop(spec.user) };
        }
    };
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
            .add(plugin.local, kind, priority, 0, callback, user)
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

// ---- ABI 1.5 ----

/// Borrowed bytes as a slice; `None` for a null pointer with a length.
///
/// # Safety
///
/// `b` must point to `b.len` readable bytes that outlive `'a`.
unsafe fn bytes<'a>(b: abi::Bytes) -> Option<&'a [u8]> {
    if b.len == 0 {
        return Some(&[]);
    }
    if b.ptr.is_null() {
        return None;
    }
    // SAFETY: guaranteed by the caller.
    Some(unsafe { std::slice::from_raw_parts(b.ptr, b.len) })
}

/// A plugin string, `INVALID_ARGUMENT` for bad UTF-8.
fn arg<'a>(s: abi::Str) -> Result<&'a str, Status> {
    // SAFETY: the plugin's string is valid for the call.
    unsafe { str_checked(s) }.ok_or(status::INVALID_ARGUMENT)
}

/// A plugin string, `None` when empty.
fn opt_arg<'a>(s: abi::Str) -> Result<Option<&'a str>, Status> {
    arg(s).map(|s| (!s.is_empty()).then_some(s))
}

/// A text with its format, `INVALID_ARGUMENT` for bad UTF-8 or an unknown format.
fn text_arg<'a>(s: abi::Str, format: u32) -> Result<Text<'a>, Status> {
    text(s, format).ok_or(status::INVALID_ARGUMENT)
}

/// Turns a `Result` into a status.
fn done(r: Result<(), Status>) -> Status {
    r.err().unwrap_or(status::OK)
}

/// Parses an SNBT compound of a plugin (empty text: an empty compound).
fn snbt(s: abi::Str) -> Result<vanillite_nbt::Compound, Status> {
    let text = arg(s)?;
    if text.len() > abi::limits::MAX_SNBT as usize {
        return Err(status::INVALID_ARGUMENT);
    }
    if text.trim().is_empty() {
        return Ok(vanillite_nbt::Compound::new());
    }
    vanillite_nbt::snbt::parse_compound(text).map_err(|_| status::INVALID_ARGUMENT)
}

/// Writes a compound as SNBT to a plugin's sink.
fn write_snbt(sink: *const abi::ByteSink, c: vanillite_nbt::Compound) -> Status {
    write_sink(
        sink,
        vanillite_nbt::snbt::to_string(&Tag::Compound(c)).as_bytes(),
    )
}

/// A full resource location of a plugin's key (its id as default namespace).
fn key_arg(plugin: &Plugin, key: abi::Str) -> Result<String, Status> {
    data_key(plugin, key).ok_or(status::INVALID_ARGUMENT)
}

unsafe extern "C" fn subscribe_flags(
    id: abi::PluginId,
    kind: u32,
    priority: i32,
    flags: u32,
    callback: abi::EventFn,
    user: *mut c_void,
) -> Status {
    with(id, |inner, plugin| {
        if !inner.may_register(plugin) {
            return status::WRONG_PHASE;
        }
        inner
            .events
            .add(plugin.local, kind, priority, flags, callback, user)
    })
}

unsafe extern "C" fn unsubscribe(id: abi::PluginId, kind: u32) -> Status {
    with(id, |inner, plugin| {
        inner.events.remove(plugin.local, Some(kind));
        status::OK
    })
}

unsafe extern "C" fn event_register(id: abi::PluginId, name: abi::Str, out: *mut u32) -> Status {
    with(id, |inner, _| {
        let name = match arg(name) {
            Ok(n) if !n.is_empty() && n.len() <= 256 => n,
            _ => return status::INVALID_ARGUMENT,
        };
        match inner.events.custom_kind(name) {
            Some(kind) => write(out, kind),
            None => status::UNSUPPORTED,
        }
    })
}

unsafe extern "C" fn event_fire(
    id: abi::PluginId,
    raw: *mut abi::Context,
    kind: u32,
    data: abi::Bytes,
    cancelled: *mut u32,
) -> Status {
    with(id, |inner, plugin| {
        // SAFETY: the plugin passes bytes valid for the call.
        let Some(data) = (unsafe { bytes(data) }) else {
            return status::INVALID_ARGUMENT;
        };
        // SAFETY: the plugin passes the context of its running callback, or null.
        let world = unsafe { ctx(raw) }.and_then(|c| reborrow(&mut c.world));
        match inner.fire_custom(kind, (plugin.local, plugin.id()), data, world, false) {
            Ok(_) if cancelled.is_null() => status::OK,
            Ok(c) => write(cancelled, u32::from(c)),
            Err(s) => s,
        }
    })
}

unsafe extern "C" fn service_register(
    id: abi::PluginId,
    name: abi::Str,
    version: u32,
    table: *const c_void,
) -> Status {
    with(id, |inner, plugin| {
        let name = match arg(name) {
            Ok(n) if !n.is_empty() && n.len() <= 256 => n,
            _ => return status::INVALID_ARGUMENT,
        };
        if table.is_null() {
            return status::INVALID_ARGUMENT;
        }
        let mut services = inner
            .services
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        services.retain(|s| !(s.plugin == plugin.local && s.name == name));
        services.push(crate::Service {
            plugin: plugin.local,
            name: name.to_owned(),
            version,
            table: crate::SendPtr(table.cast_mut()),
        });
        status::OK
    })
}

unsafe extern "C" fn service_unregister(id: abi::PluginId, name: abi::Str) -> Status {
    with(id, |inner, plugin| {
        let Ok(name) = arg(name) else {
            return status::INVALID_ARGUMENT;
        };
        inner
            .services
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|s| !(s.plugin == plugin.local && s.name == name));
        status::OK
    })
}

unsafe extern "C" fn service_get(
    id: abi::PluginId,
    name: abi::Str,
    version: *mut u32,
    table: *mut *const c_void,
) -> Status {
    with(id, |inner, _| {
        let Ok(name) = arg(name) else {
            return status::INVALID_ARGUMENT;
        };
        let services = inner
            .services
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let Some(service) = services.iter().rev().find(|s| {
            s.name == name
                && inner
                    .plugin(s.plugin)
                    .is_some_and(|p| p.enabled.load(std::sync::atomic::Ordering::Acquire))
        }) else {
            return status::NOT_FOUND;
        };
        if !version.is_null() {
            write(version, service.version);
        }
        write(table, service.table.0.cast_const())
    })
}

unsafe extern "C" fn plugin_list(id: abi::PluginId, sink: *const abi::ByteSink) -> Status {
    with(id, |inner, _| {
        let list = inner
            .plugins
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|p| p.enabled.load(std::sync::atomic::Ordering::Acquire))
            .map(|p| format!("{} {}", p.id(), p.manifest.version))
            .collect::<Vec<_>>()
            .join("\n");
        write_sink(sink, list.as_bytes())
    })
}

unsafe extern "C" fn config_set(id: abi::PluginId, key: abi::Str, value: abi::Str) -> Status {
    with(id, |_, plugin| {
        let (Ok(key), Ok(value)) = (arg(key), arg(value)) else {
            return status::INVALID_ARGUMENT;
        };
        // A TOML value parses as the value of a one-key document.
        let Ok(mut doc) = format!("v = {value}").parse::<toml::Table>() else {
            return status::INVALID_ARGUMENT;
        };
        let Some(value) = doc.remove("v") else {
            return status::INVALID_ARGUMENT;
        };
        let mut config = plugin.config.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(table) = config.as_mut() else {
            return status::WRONG_PHASE;
        };
        let parts: Vec<&str> = key.split('.').collect();
        let Some((last, path)) = parts.split_last() else {
            return status::INVALID_ARGUMENT;
        };
        if parts.iter().any(|p| p.is_empty()) {
            return status::INVALID_ARGUMENT;
        }
        let mut at = table;
        for part in path {
            let entry = at
                .entry((*part).to_owned())
                .or_insert_with(|| toml::Value::Table(toml::Table::new()));
            let toml::Value::Table(next) = entry else {
                return status::INVALID_ARGUMENT;
            };
            at = next;
        }
        at.insert((*last).to_owned(), value);
        status::OK
    })
}

unsafe extern "C" fn config_save(id: abi::PluginId) -> Status {
    with(id, |_, plugin| {
        let text = {
            let config = plugin.config.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(table) = config.as_ref() else {
                return status::WRONG_PHASE;
            };
            match toml::to_string_pretty(table) {
                Ok(text) => text,
                Err(_) => return status::ERROR,
            }
        };
        let path = plugin.data_dir.join("config.toml");
        match std::fs::write(&path, text) {
            Ok(()) => status::OK,
            Err(e) => {
                log::error!("[{}] Cannot write {}: {e}", plugin.id(), path.display());
                status::ERROR
            }
        }
    })
}

unsafe extern "C" fn config_toml(
    id: abi::PluginId,
    key: abi::Str,
    out: *mut abi::StrBuf,
) -> Status {
    config_value(id, key, |v| match v {
        toml::Value::Table(t) => match toml::to_string(t) {
            Ok(text) => write_str(out, &text),
            Err(_) => status::ERROR,
        },
        v => write_str(out, &v.to_string()),
    })
}

unsafe extern "C" fn permission_register(
    id: abi::PluginId,
    node: abi::Str,
    default: u32,
) -> Status {
    with(id, |inner, _| {
        let node = match arg(node) {
            Ok(n) if !n.is_empty() && n.len() <= 256 => n,
            _ => return status::INVALID_ARGUMENT,
        };
        if default > abi::permission_default::NOT_OP {
            return status::INVALID_ARGUMENT;
        }
        inner
            .permission_defaults
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(node.to_owned(), default);
        status::OK
    })
}

unsafe extern "C" fn player_info(
    id: abi::PluginId,
    player: abi::Uuid,
    out: *mut abi::PlayerInfo,
) -> Status {
    with(id, |inner, _| {
        let Some(info) = server!(inner).player_info(player.to_u128()) else {
            return status::NOT_FOUND;
        };
        write(
            out,
            abi::PlayerInfo {
                size: abi::size_of::<abi::PlayerInfo>(),
                world: info.world,
                game_mode: info.game_mode,
                op_level: info.op_level,
                ping: info.ping,
            },
        )
    })
}

unsafe extern "C" fn player_address(
    id: abi::PluginId,
    player: abi::Uuid,
    out: *mut abi::StrBuf,
) -> Status {
    with(id, |inner, _| {
        match server!(inner).player_info(player.to_u128()) {
            Some(info) => write_str(out, &info.address),
            None => status::NOT_FOUND,
        }
    })
}

unsafe extern "C" fn player_play_sound(
    id: abi::PluginId,
    player: abi::Uuid,
    spec: *const abi::SoundSpec,
) -> Status {
    with(id, |inner, _| {
        // SAFETY: the plugin passes a valid record or null.
        let Some(spec) = (unsafe { spec.as_ref() }) else {
            return status::INVALID_ARGUMENT;
        };
        let Ok(sound) = arg(spec.sound) else {
            return status::INVALID_ARGUMENT;
        };
        if spec.source > abi::sound_source::UI {
            return status::INVALID_ARGUMENT;
        }
        let sound = Sound {
            sound,
            source: spec.source,
            pos: [spec.pos.x, spec.pos.y, spec.pos.z],
            volume: spec.volume,
            pitch: spec.pitch,
            seed: spec.seed,
        };
        if server!(inner).play_sound(player.to_u128(), &sound) {
            status::OK
        } else {
            status::NOT_FOUND
        }
    })
}

unsafe extern "C" fn player_stop_sound(
    id: abi::PluginId,
    player: abi::Uuid,
    sound: abi::Str,
    source: u32,
) -> Status {
    with(id, |inner, _| {
        let Ok(sound) = opt_arg(sound) else {
            return status::INVALID_ARGUMENT;
        };
        let source = match source {
            abi::sound_source::ANY => None,
            s if s <= abi::sound_source::UI => Some(s),
            _ => return status::INVALID_ARGUMENT,
        };
        if server!(inner).stop_sound(player.to_u128(), sound, source) {
            status::OK
        } else {
            status::NOT_FOUND
        }
    })
}

unsafe extern "C" fn player_spawn_particle(
    id: abi::PluginId,
    player: abi::Uuid,
    spec: *const abi::ParticleSpec,
) -> Status {
    with(id, |inner, _| {
        // SAFETY: the plugin passes a valid record or null.
        let Some(spec) = (unsafe { spec.as_ref() }) else {
            return status::INVALID_ARGUMENT;
        };
        let Ok(text) = arg(spec.particle) else {
            return status::INVALID_ARGUMENT;
        };
        let (particle, options) = match text.find('{') {
            Some(at) => {
                let Ok(options) = vanillite_nbt::snbt::parse_compound(&text[at..]) else {
                    return status::INVALID_ARGUMENT;
                };
                (text[..at].trim(), options)
            }
            None => (text.trim(), vanillite_nbt::Compound::new()),
        };
        let particles = Particles {
            particle,
            options,
            pos: [spec.pos.x, spec.pos.y, spec.pos.z],
            delta: [spec.delta.x, spec.delta.y, spec.delta.z],
            speed: spec.speed,
            count: spec.count,
            force: spec.force != 0,
        };
        done(server!(inner).spawn_particle(player.to_u128(), &particles))
    })
}

unsafe extern "C" fn player_transfer(
    id: abi::PluginId,
    player: abi::Uuid,
    host: abi::Str,
    port: u32,
) -> Status {
    with(id, |inner, _| {
        let (Ok(host), Ok(port)) = (arg(host), u16::try_from(port)) else {
            return status::INVALID_ARGUMENT;
        };
        if host.is_empty() || host.len() > 255 {
            return status::INVALID_ARGUMENT;
        }
        if server!(inner).transfer(player.to_u128(), host, port) {
            status::OK
        } else {
            status::NOT_FOUND
        }
    })
}

/// Whether a channel name is a resource location.
fn valid_channel(channel: &str) -> bool {
    channel.len() <= 256
        && channel.split_once(':').is_some_and(|(ns, path)| {
            !ns.is_empty()
                && !path.is_empty()
                && ns.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-' | b'.')
                })
                && path.bytes().all(|b| {
                    b.is_ascii_lowercase()
                        || b.is_ascii_digit()
                        || matches!(b, b'_' | b'-' | b'.' | b'/')
                })
        })
}

unsafe extern "C" fn register_channel(id: abi::PluginId, channel: abi::Str) -> Status {
    with(id, |inner, plugin| {
        if !inner.may_register(plugin) {
            return status::WRONG_PHASE;
        }
        let channel = match arg(channel) {
            Ok(c) if valid_channel(c) => c,
            _ => return status::INVALID_ARGUMENT,
        };
        let mut channels = inner
            .channels
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        let mine = channels.iter().filter(|(p, _)| *p == plugin.local).count();
        if mine >= abi::limits::MAX_CHANNELS as usize {
            return status::INVALID_ARGUMENT;
        }
        if !channels
            .iter()
            .any(|(p, c)| *p == plugin.local && c == channel)
        {
            channels.push((plugin.local, channel.to_owned()));
        }
        status::OK
    })
}

unsafe extern "C" fn player_send_plugin_message(
    id: abi::PluginId,
    player: abi::Uuid,
    channel: abi::Str,
    data: abi::Bytes,
) -> Status {
    with(id, |inner, _| {
        let channel = match arg(channel) {
            Ok(c) if valid_channel(c) => c,
            _ => return status::INVALID_ARGUMENT,
        };
        // SAFETY: the plugin passes bytes valid for the call.
        let Some(data) = (unsafe { bytes(data) }) else {
            return status::INVALID_ARGUMENT;
        };
        if data.len() > 1 << 20 {
            return status::INVALID_ARGUMENT;
        }
        if server!(inner).send_plugin_message(player.to_u128(), channel, data) {
            status::OK
        } else {
            status::NOT_FOUND
        }
    })
}

/// Runs a scoreboard operation.
fn scoreboard(
    id: abi::PluginId,
    op: impl FnOnce() -> Result<ScoreboardOp<'static>, Status>,
) -> Result<ScoreReply, Status> {
    let mut reply = Err(status::ERROR);
    let s = with(id, |inner, _| {
        let op = match op() {
            Ok(op) => op,
            Err(s) => return s,
        };
        reply = server!(inner).scoreboard(op);
        match &reply {
            Ok(_) => status::OK,
            Err(s) => *s,
        }
    });
    if s == status::OK { reply } else { Err(s) }
}

/// A non-empty name of a plugin (objective, holder, team, entity type).
fn name_arg<'a>(s: abi::Str) -> Result<&'a str, Status> {
    match arg(s) {
        Ok(n) if !n.is_empty() && n.len() <= 32767 => Ok(n),
        _ => Err(status::INVALID_ARGUMENT),
    }
}

unsafe extern "C" fn objective_add(
    id: abi::PluginId,
    name: abi::Str,
    criteria: abi::Str,
    display: abi::Str,
    format: u32,
) -> Status {
    done(
        scoreboard(id, || {
            Ok(ScoreboardOp::AddObjective {
                name: name_arg(name)?,
                criteria: name_arg(criteria)?,
                display: text_arg(display, format)?,
            })
        })
        .map(drop),
    )
}

unsafe extern "C" fn objective_remove(id: abi::PluginId, name: abi::Str) -> Status {
    done(
        scoreboard(id, || {
            Ok(ScoreboardOp::RemoveObjective {
                name: name_arg(name)?,
            })
        })
        .map(drop),
    )
}

unsafe extern "C" fn objective_display(id: abi::PluginId, slot: u32, name: abi::Str) -> Status {
    done(
        scoreboard(id, || {
            Ok(ScoreboardOp::Display {
                slot,
                objective: opt_arg(name)?,
            })
        })
        .map(drop),
    )
}

unsafe extern "C" fn score_get(
    id: abi::PluginId,
    objective: abi::Str,
    holder: abi::Str,
    out: *mut i32,
) -> Status {
    match scoreboard(id, || {
        Ok(ScoreboardOp::GetScore {
            objective: name_arg(objective)?,
            holder: name_arg(holder)?,
        })
    }) {
        Ok(ScoreReply::Score(v)) => write(out, v),
        Ok(_) => status::NOT_FOUND,
        Err(s) => s,
    }
}

unsafe extern "C" fn score_set(
    id: abi::PluginId,
    objective: abi::Str,
    holder: abi::Str,
    value: i32,
) -> Status {
    done(
        scoreboard(id, || {
            Ok(ScoreboardOp::SetScore {
                objective: name_arg(objective)?,
                holder: name_arg(holder)?,
                value,
            })
        })
        .map(drop),
    )
}

unsafe extern "C" fn score_reset(
    id: abi::PluginId,
    objective: abi::Str,
    holder: abi::Str,
) -> Status {
    done(
        scoreboard(id, || {
            Ok(ScoreboardOp::ResetScore {
                objective: opt_arg(objective)?,
                holder: name_arg(holder)?,
            })
        })
        .map(drop),
    )
}

unsafe extern "C" fn team_add(id: abi::PluginId, name: abi::Str) -> Status {
    done(
        scoreboard(id, || {
            Ok(ScoreboardOp::AddTeam {
                name: name_arg(name)?,
            })
        })
        .map(drop),
    )
}

unsafe extern "C" fn team_remove(id: abi::PluginId, name: abi::Str) -> Status {
    done(
        scoreboard(id, || {
            Ok(ScoreboardOp::RemoveTeam {
                name: name_arg(name)?,
            })
        })
        .map(drop),
    )
}

unsafe extern "C" fn team_join(id: abi::PluginId, team: abi::Str, holder: abi::Str) -> Status {
    done(
        scoreboard(id, || {
            Ok(ScoreboardOp::JoinTeam {
                team: name_arg(team)?,
                holder: name_arg(holder)?,
            })
        })
        .map(drop),
    )
}

unsafe extern "C" fn team_leave(id: abi::PluginId, holder: abi::Str) -> Status {
    done(
        scoreboard(id, || {
            Ok(ScoreboardOp::LeaveTeam {
                holder: name_arg(holder)?,
            })
        })
        .map(drop),
    )
}

unsafe extern "C" fn team_modify(
    id: abi::PluginId,
    team: abi::Str,
    option: abi::Str,
    value: abi::Str,
) -> Status {
    done(
        scoreboard(id, || {
            Ok(ScoreboardOp::ModifyTeam {
                team: name_arg(team)?,
                option: name_arg(option)?,
                value: arg(value)?,
            })
        })
        .map(drop),
    )
}

unsafe extern "C" fn team_of(id: abi::PluginId, holder: abi::Str, out: *mut abi::StrBuf) -> Status {
    match scoreboard(id, || {
        Ok(ScoreboardOp::TeamOf {
            holder: name_arg(holder)?,
        })
    }) {
        Ok(ScoreReply::Team(team)) => write_str(out, &team),
        Ok(_) => status::NOT_FOUND,
        Err(s) => s,
    }
}

/// Runs a boss bar operation with the plugin's full key.
fn boss_bar(
    id: abi::PluginId,
    key: abi::Str,
    op: impl FnOnce(&str) -> Result<BossBarOp<'_>, Status>,
) -> Status {
    with(id, |inner, plugin| {
        let key = match key_arg(plugin, key) {
            Ok(key) => key,
            Err(s) => return s,
        };
        match op(&key) {
            Ok(op) => done(server!(inner).boss_bar(op)),
            Err(s) => s,
        }
    })
}

unsafe extern "C" fn bossbar_create(
    id: abi::PluginId,
    key: abi::Str,
    name: abi::Str,
    format: u32,
) -> Status {
    boss_bar(id, key, |key| {
        Ok(BossBarOp::Create {
            key,
            name: text_arg(name, format)?,
        })
    })
}

unsafe extern "C" fn bossbar_remove(id: abi::PluginId, key: abi::Str) -> Status {
    boss_bar(id, key, |key| Ok(BossBarOp::Remove { key }))
}

unsafe extern "C" fn bossbar_set(
    id: abi::PluginId,
    key: abi::Str,
    prop: u32,
    value: i64,
) -> Status {
    boss_bar(id, key, |key| Ok(BossBarOp::Set { key, prop, value }))
}

unsafe extern "C" fn bossbar_set_name(
    id: abi::PluginId,
    key: abi::Str,
    name: abi::Str,
    format: u32,
) -> Status {
    boss_bar(id, key, |key| {
        Ok(BossBarOp::SetName {
            key,
            name: text_arg(name, format)?,
        })
    })
}

unsafe extern "C" fn bossbar_player(
    id: abi::PluginId,
    key: abi::Str,
    player: abi::Uuid,
    show: u32,
) -> Status {
    boss_bar(id, key, |key| {
        Ok(BossBarOp::Player {
            key,
            player: player.to_u128(),
            show: show != 0,
        })
    })
}

unsafe extern "C" fn server_stats(id: abi::PluginId, out: *mut abi::ServerStats) -> Status {
    with(id, |inner, _| {
        let Some(stats) = server!(inner).stats() else {
            return status::UNSUPPORTED;
        };
        write(
            out,
            abi::ServerStats {
                size: abi::size_of::<abi::ServerStats>(),
                tick: stats.tick,
                mspt: stats.mspt,
                tps: stats.tps,
                tick_rate: stats.tick_rate,
                online: stats.online,
                max_players: stats.max_players,
            },
        )
    })
}

unsafe extern "C" fn server_set_motd(id: abi::PluginId, motd: abi::Str, format: u32) -> Status {
    with(id, |inner, _| match text_arg(motd, format) {
        Ok(motd) if server!(inner).set_motd(motd) => status::OK,
        Ok(_) => status::UNSUPPORTED,
        Err(s) => s,
    })
}

unsafe extern "C" fn access_set(
    id: abi::PluginId,
    list: u32,
    player: abi::Uuid,
    name: abi::Str,
    add: u32,
    reason: abi::Str,
) -> Status {
    with(id, |inner, _| {
        let (Ok(name), Ok(reason)) = (arg(name), arg(reason)) else {
            return status::INVALID_ARGUMENT;
        };
        let op = AccessOp {
            list,
            player: player.to_u128(),
            name,
            change: Some((add != 0, reason)),
        };
        done(server!(inner).access(op).map(drop))
    })
}

unsafe extern "C" fn access_get(
    id: abi::PluginId,
    list: u32,
    player: abi::Uuid,
    name: abi::Str,
    out: *mut u32,
) -> Status {
    with(id, |inner, _| {
        let Ok(name) = arg(name) else {
            return status::INVALID_ARGUMENT;
        };
        let op = AccessOp {
            list,
            player: player.to_u128(),
            name,
            change: None,
        };
        match server!(inner).access(op) {
            Ok(on) => write(out, u32::from(on)),
            Err(s) => s,
        }
    })
}

unsafe extern "C" fn world_time(id: abi::PluginId, world: u32, out: *mut i64) -> Status {
    with(id, |inner, _| match server!(inner).world_time(world) {
        Some(time) => write(out, time),
        None => status::NOT_FOUND,
    })
}

unsafe extern "C" fn world_set_time(id: abi::PluginId, world: u32, time: i64) -> Status {
    with(id, |inner, _| {
        done(server!(inner).set_world_time(world, time))
    })
}

unsafe extern "C" fn world_set_weather(id: abi::PluginId, kind: u32, duration: i32) -> Status {
    with(id, |inner, _| {
        if kind > abi::weather::THUNDER || duration < -1 {
            return status::INVALID_ARGUMENT;
        }
        done(server!(inner).set_weather(0, kind, duration))
    })
}

unsafe extern "C" fn game_rule_get(
    id: abi::PluginId,
    name: abi::Str,
    out: *mut abi::StrBuf,
) -> Status {
    with(id, |inner, _| {
        let Ok(name) = arg(name) else {
            return status::INVALID_ARGUMENT;
        };
        match server!(inner).game_rule(name) {
            Some(value) => write_str(out, &value),
            None => status::NOT_FOUND,
        }
    })
}

unsafe extern "C" fn game_rule_set(id: abi::PluginId, name: abi::Str, value: abi::Str) -> Status {
    with(id, |inner, _| {
        let (Ok(name), Ok(value)) = (arg(name), arg(value)) else {
            return status::INVALID_ARGUMENT;
        };
        done(server!(inner).set_game_rule(name, value))
    })
}

/// Runs `f` on the owned world of a context call.
fn on_world(
    raw: *mut abi::Context,
    world: u32,
    f: impl FnOnce(&mut dyn WorldAccess) -> Status,
) -> Status {
    guard(|| match owned(raw, world) {
        Ok(w) => f(w),
        Err(s) => s,
    })
}

unsafe extern "C" fn player_get(
    raw: *mut abi::Context,
    world: u32,
    player: abi::Uuid,
    prop: u32,
    out: *mut f64,
) -> Status {
    on_world(raw, world, |w| match w.player_get(player.to_u128(), prop) {
        Ok(v) => write(out, v),
        Err(s) => s,
    })
}

unsafe extern "C" fn player_set(
    raw: *mut abi::Context,
    world: u32,
    player: abi::Uuid,
    prop: u32,
    value: f64,
) -> Status {
    on_world(raw, world, |w| {
        if !value.is_finite() {
            return status::INVALID_ARGUMENT;
        }
        done(w.player_set(player.to_u128(), prop, value))
    })
}

fn to_abi_location(l: Location) -> abi::Location {
    abi::Location {
        world: l.world,
        pos: abi::Vec3 {
            x: l.pos[0],
            y: l.pos[1],
            z: l.pos[2],
        },
        yaw: l.yaw,
        pitch: l.pitch,
    }
}

unsafe extern "C" fn entity_location(
    raw: *mut abi::Context,
    world: u32,
    entity: abi::Uuid,
    out: *mut abi::Location,
) -> Status {
    on_world(raw, world, |w| match w.entity_location(entity.to_u128()) {
        Ok(l) => write(out, to_abi_location(l)),
        Err(s) => s,
    })
}

unsafe extern "C" fn entity_type(
    raw: *mut abi::Context,
    world: u32,
    entity: abi::Uuid,
    out: *mut abi::StrBuf,
) -> Status {
    on_world(raw, world, |w| match w.entity_type(entity.to_u128()) {
        Ok(t) => write_str(out, &t),
        Err(s) => s,
    })
}

/// Whether a position is finite and inside the world's coordinate limits.
fn valid_pos(p: [f64; 3]) -> bool {
    p.iter().all(|v| v.is_finite() && v.abs() < 3.0e7)
}

unsafe extern "C" fn entity_teleport(
    raw: *mut abi::Context,
    world: u32,
    entity: abi::Uuid,
    to: *const abi::Location,
) -> Status {
    on_world(raw, world, |w| {
        // SAFETY: the plugin passes a valid record or null.
        let Some(to) = (unsafe { to.as_ref() }) else {
            return status::INVALID_ARGUMENT;
        };
        let pos = [to.pos.x, to.pos.y, to.pos.z];
        if !valid_pos(pos) || !to.yaw.is_finite() || !to.pitch.is_finite() {
            return status::INVALID_ARGUMENT;
        }
        done(w.teleport(
            entity.to_u128(),
            Location {
                world: to.world,
                pos,
                yaw: to.yaw,
                pitch: to.pitch,
            },
        ))
    })
}

unsafe extern "C" fn entity_set_velocity(
    raw: *mut abi::Context,
    world: u32,
    entity: abi::Uuid,
    v: abi::Vec3,
) -> Status {
    on_world(raw, world, |w| {
        let v = [v.x, v.y, v.z];
        if v.iter().any(|c| !c.is_finite() || c.abs() > 10.0) {
            return status::INVALID_ARGUMENT;
        }
        done(w.set_velocity(entity.to_u128(), v))
    })
}

unsafe extern "C" fn entity_spawn(
    raw: *mut abi::Context,
    world: u32,
    kind: abi::Str,
    pos: abi::Vec3,
    nbt: abi::Str,
    out: *mut abi::Uuid,
) -> Status {
    on_world(raw, world, |w| {
        let (Ok(kind), Ok(nbt)) = (name_arg(kind), snbt(nbt)) else {
            return status::INVALID_ARGUMENT;
        };
        let pos = [pos.x, pos.y, pos.z];
        if !valid_pos(pos) {
            return status::INVALID_ARGUMENT;
        }
        match w.spawn_entity(kind, pos, &nbt) {
            Ok(_) if out.is_null() => status::OK,
            Ok(uuid) => write(out, abi::Uuid::from_u128(uuid)),
            Err(s) => s,
        }
    })
}

unsafe extern "C" fn entity_remove(
    raw: *mut abi::Context,
    world: u32,
    entity: abi::Uuid,
) -> Status {
    on_world(raw, world, |w| done(w.remove_entity(entity.to_u128())))
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn entities_near(
    raw: *mut abi::Context,
    world: u32,
    center: abi::Vec3,
    radius: f64,
    kind: abi::Str,
    out: *mut abi::Uuid,
    cap: usize,
    count: *mut usize,
) -> Status {
    on_world(raw, world, |w| {
        let Ok(kind) = opt_arg(kind) else {
            return status::INVALID_ARGUMENT;
        };
        let center = [center.x, center.y, center.z];
        if !valid_pos(center) || !radius.is_finite() || !(0.0..=1024.0).contains(&radius) {
            return status::INVALID_ARGUMENT;
        }
        if cap > 0 && out.is_null() {
            return status::INVALID_ARGUMENT;
        }
        let found = w.entities_near(center, radius, kind);
        for (i, uuid) in found.iter().take(cap).enumerate() {
            // SAFETY: the plugin passes `cap` writable entries.
            unsafe { out.add(i).write(abi::Uuid::from_u128(*uuid)) };
        }
        write(count, found.len())
    })
}

unsafe extern "C" fn entity_nbt(
    raw: *mut abi::Context,
    world: u32,
    entity: abi::Uuid,
    sink: *const abi::ByteSink,
) -> Status {
    on_world(raw, world, |w| match w.entity_nbt(entity.to_u128()) {
        Ok(c) => write_snbt(sink, c),
        Err(s) => s,
    })
}

unsafe extern "C" fn entity_nbt_merge(
    raw: *mut abi::Context,
    world: u32,
    entity: abi::Uuid,
    nbt: abi::Str,
) -> Status {
    on_world(raw, world, |w| match snbt(nbt) {
        Ok(nbt) => done(w.merge_entity_nbt(entity.to_u128(), &nbt)),
        Err(s) => s,
    })
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn entity_effect_add(
    raw: *mut abi::Context,
    world: u32,
    entity: abi::Uuid,
    effect: abi::Str,
    duration: i32,
    amplifier: i32,
    flags: u32,
) -> Status {
    on_world(raw, world, |w| {
        let Ok(effect) = name_arg(effect) else {
            return status::INVALID_ARGUMENT;
        };
        if duration < -1 || !(0..=255).contains(&amplifier) || flags > 7 {
            return status::INVALID_ARGUMENT;
        }
        done(w.add_effect(entity.to_u128(), effect, duration, amplifier, flags))
    })
}

unsafe extern "C" fn entity_effect_remove(
    raw: *mut abi::Context,
    world: u32,
    entity: abi::Uuid,
    effect: abi::Str,
) -> Status {
    on_world(raw, world, |w| match opt_arg(effect) {
        Ok(effect) => done(w.remove_effect(entity.to_u128(), effect)),
        Err(s) => s,
    })
}

unsafe extern "C" fn player_give(
    raw: *mut abi::Context,
    world: u32,
    player: abi::Uuid,
    item: abi::Str,
) -> Status {
    on_world(raw, world, |w| match snbt(item) {
        Ok(item) if !item.is_empty() => done(w.give_item(player.to_u128(), &item)),
        Ok(_) => status::INVALID_ARGUMENT,
        Err(s) => s,
    })
}

unsafe extern "C" fn entity_item_get(
    raw: *mut abi::Context,
    world: u32,
    entity: abi::Uuid,
    slot: abi::Str,
    sink: *const abi::ByteSink,
) -> Status {
    on_world(raw, world, |w| {
        let Ok(slot) = name_arg(slot) else {
            return status::INVALID_ARGUMENT;
        };
        match w.item(entity.to_u128(), slot) {
            Ok(item) => write_snbt(sink, item),
            Err(s) => s,
        }
    })
}

unsafe extern "C" fn entity_item_set(
    raw: *mut abi::Context,
    world: u32,
    entity: abi::Uuid,
    slot: abi::Str,
    item: abi::Str,
) -> Status {
    on_world(raw, world, |w| {
        let (Ok(slot), Ok(item)) = (name_arg(slot), snbt(item)) else {
            return status::INVALID_ARGUMENT;
        };
        done(w.set_item(entity.to_u128(), slot, &item))
    })
}

fn block_pos(p: abi::BlockPos) -> [i32; 3] {
    [p.x, p.y, p.z]
}

unsafe extern "C" fn block_entity_nbt(
    raw: *mut abi::Context,
    world: u32,
    pos: abi::BlockPos,
    sink: *const abi::ByteSink,
) -> Status {
    on_world(raw, world, |w| match w.block_entity_nbt(block_pos(pos)) {
        Ok(c) => write_snbt(sink, c),
        Err(s) => s,
    })
}

unsafe extern "C" fn block_entity_merge(
    raw: *mut abi::Context,
    world: u32,
    pos: abi::BlockPos,
    nbt: abi::Str,
) -> Status {
    on_world(raw, world, |w| match snbt(nbt) {
        Ok(nbt) => done(w.merge_block_entity_nbt(block_pos(pos), &nbt)),
        Err(s) => s,
    })
}

unsafe extern "C" fn advancement_set(
    raw: *mut abi::Context,
    world: u32,
    player: abi::Uuid,
    advancement: abi::Str,
    criterion: abi::Str,
    grant: u32,
) -> Status {
    on_world(raw, world, |w| {
        let (Ok(advancement), Ok(criterion)) = (name_arg(advancement), opt_arg(criterion)) else {
            return status::INVALID_ARGUMENT;
        };
        done(w.set_advancement(player.to_u128(), advancement, criterion, grant != 0))
    })
}

unsafe extern "C" fn advancement_done(
    raw: *mut abi::Context,
    world: u32,
    player: abi::Uuid,
    advancement: abi::Str,
    out: *mut u32,
) -> Status {
    on_world(raw, world, |w| {
        let Ok(advancement) = name_arg(advancement) else {
            return status::INVALID_ARGUMENT;
        };
        match w.advancement_done(player.to_u128(), advancement) {
            Ok(done) => write(out, u32::from(done)),
            Err(s) => s,
        }
    })
}

unsafe extern "C" fn packet_subscribe(
    id: abi::PluginId,
    spec: *const abi::PacketSpec,
    out: *mut u64,
) -> Status {
    // SAFETY: the plugin passes a valid spec or null.
    let Some(spec) = (unsafe { spec.as_ref() }) else {
        return status::INVALID_ARGUMENT;
    };
    let result = if (spec.size as usize) < size_of::<abi::PacketSpec>() || out.is_null() {
        status::INVALID_ARGUMENT
    } else {
        with(id, |inner, plugin| match inner.packets.add(plugin, spec) {
            Ok(handle) => write(out, handle),
            Err(s) => s,
        })
    };
    if result != status::OK
        && let Some(drop) = spec.drop
    {
        // SAFETY: the plugin's drop function for its own user data, which no listener took.
        unsafe { drop(spec.user) };
    }
    result
}

unsafe extern "C" fn packet_unsubscribe(id: abi::PluginId, handle: u64) -> Status {
    with(id, |inner, plugin| {
        inner.packets.remove(plugin.local, Some(handle))
    })
}

unsafe extern "C" fn packet_id(
    id: abi::PluginId,
    state: u32,
    direction: u32,
    name: abi::Str,
    out: *mut i32,
) -> Status {
    with(id, |_, _| {
        let name = match arg(name) {
            Ok(name) => name,
            Err(s) => return s,
        };
        match crate::packets::id_of(direction, state, name) {
            Ok(packet) => write(out, packet),
            Err(s) => s,
        }
    })
}

unsafe extern "C" fn packet_name(
    id: abi::PluginId,
    state: u32,
    direction: u32,
    packet: i32,
    out: *mut abi::StrBuf,
) -> Status {
    with(id, |_, _| {
        match crate::packets::name_of(direction, state, packet) {
            Ok(name) => write_str(out, name),
            Err(s) => s,
        }
    })
}

unsafe extern "C" fn packet_send(
    id: abi::PluginId,
    player: abi::Uuid,
    body: abi::Bytes,
    flags: u32,
) -> Status {
    with(id, |inner, _| {
        if flags & !abi::packet_send::SKIP_LISTENERS != 0 {
            return status::UNSUPPORTED;
        }
        // SAFETY: the plugin passes bytes valid for the call.
        let Some(body) = (unsafe { bytes(body) }) else {
            return status::INVALID_ARGUMENT;
        };
        if !crate::packets::sendable(body) {
            return status::INVALID_ARGUMENT;
        }
        let skip = flags & abi::packet_send::SKIP_LISTENERS != 0;
        if server!(inner).send_packet(player.to_u128(), body.to_vec(), skip) {
            status::OK
        } else {
            status::NOT_FOUND
        }
    })
}

unsafe extern "C" fn protocol_version(id: abi::PluginId, out: *mut i32) -> Status {
    with(id, |_, _| write(out, vanillite_protocol::PROTOCOL_VERSION))
}
