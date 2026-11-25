//! The plugin side of the C ABI: the entry point, the lifecycle functions, the host table and
//! the panic guard around every function the host calls.

use std::any::Any;
use std::panic::{self, AssertUnwindSafe};
use std::sync::{Mutex, OnceLock, PoisonError};

use vanillite_abi::{self as abi, Status, status};

use crate::server::Ctx;
use crate::{Plugin, Registrar};

/// The host's table and this plugin's id, set once in `load`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Host {
    pub(crate) api: &'static abi::HostApi,
    pub(crate) id: abi::PluginId,
}

static HOST: OnceLock<Host> = OnceLock::new();

/// The plugin instance (one per library).
static PLUGIN: Mutex<Option<Box<dyn Any + Send>>> = Mutex::new(None);

/// The host, after `load`.
pub(crate) fn host() -> Option<Host> {
    HOST.get().copied()
}

/// Runs `f` with the plugin instance. `None` before `load` finished.
pub(crate) fn with_plugin<P: Plugin, R>(f: impl FnOnce(&mut P) -> R) -> Option<R> {
    // A panic in a handler poisons the lock; the host disables the plugin then, so the state
    // is only reached again by `disable`, which may still clean up.
    let mut guard = PLUGIN.lock().unwrap_or_else(PoisonError::into_inner);
    let plugin = guard.as_mut()?.downcast_mut::<P>()?;
    Some(f(plugin))
}

/// Runs `f`, turning a panic into [`status::PANICKED`].
pub(crate) fn guard(f: impl FnOnce() -> Status) -> Status {
    panic::catch_unwind(AssertUnwindSafe(f)).unwrap_or(status::PANICKED)
}

/// A borrowed ABI string as `&str`; invalid UTF-8 becomes empty.
///
/// # Safety
///
/// `s` must point to `s.len` readable bytes that outlive `'a`.
pub(crate) unsafe fn str<'a>(s: abi::Str) -> &'a str {
    if s.len == 0 || s.ptr.is_null() {
        return "";
    }
    // SAFETY: guaranteed by the caller.
    let bytes = unsafe { std::slice::from_raw_parts(s.ptr, s.len) };
    std::str::from_utf8(bytes).unwrap_or_default()
}

/// Reads a string through a host function that fills a [`abi::StrBuf`], growing the buffer
/// until it fits.
pub(crate) fn read_string(
    mut call: impl FnMut(*mut abi::StrBuf) -> Status,
) -> Result<String, Status> {
    let mut buf = vec![0u8; 64];
    loop {
        let mut out = abi::StrBuf {
            ptr: buf.as_mut_ptr(),
            cap: buf.len(),
            len: 0,
        };
        let status = call(&mut out);
        if status != status::OK {
            return Err(status);
        }
        if out.len <= buf.len() {
            buf.truncate(out.len);
            return String::from_utf8(buf).map_err(|_| status::INVALID_ARGUMENT);
        }
        buf.resize(out.len, 0);
    }
}

/// Sync wrapper of the static plugin info (it holds pointers to static strings).
struct Info(abi::PluginInfo);

// SAFETY: the info only points to static data and functions.
unsafe impl Sync for Info {}
// SAFETY: as above.
unsafe impl Send for Info {}

static INFO: OnceLock<Info> = OnceLock::new();

/// The plugin info of `P` (the entry point of [`export_plugin!`](crate::export_plugin)).
pub fn info<P: Plugin>() -> *const abi::PluginInfo {
    let info = INFO.get_or_init(|| {
        Info(abi::PluginInfo {
            size: abi::size_of::<abi::PluginInfo>(),
            abi_major: abi::ABI_MAJOR,
            abi_minor: abi::ABI_MINOR,
            panic_strategy: if cfg!(panic = "unwind") {
                abi::panic_strategy::UNWIND
            } else {
                abi::panic_strategy::ABORT
            },
            data_version: 0,
            sdk_version: abi::Str::new(env!("CARGO_PKG_VERSION")),
            target: abi::Str::new(env!("VANILLITE_PLUGIN_TARGET")),
            load: load::<P>,
            enable: enable::<P>,
            disable: disable::<P>,
            save_state: Some(save_state::<P>),
            restore_state: Some(restore_state::<P>),
        })
    });
    &info.0
}

unsafe extern "C" fn load<P: Plugin>(api: *const abi::HostApi, id: abi::PluginId) -> Status {
    guard(|| {
        if api.is_null() {
            return status::INVALID_ARGUMENT;
        }
        // SAFETY: the host passes a table that lives as long as the process.
        let api: &'static abi::HostApi = unsafe { &*api };
        if api.abi_major != abi::ABI_MAJOR || (api.size as usize) < size_of::<abi::HostApi>() {
            return status::UNSUPPORTED;
        }
        let host = *HOST.get_or_init(|| Host { api, id });
        panic::set_hook(Box::new(|info| {
            crate::log(abi::log_level::ERROR, &format!("panic: {info}"));
        }));
        let mut registrar = Registrar::new(host);
        match P::load(&mut registrar) {
            Ok(plugin) => {
                *PLUGIN.lock().unwrap_or_else(PoisonError::into_inner) = Some(Box::new(plugin));
                registrar.finish()
            }
            Err(e) => {
                crate::log(abi::log_level::ERROR, &format!("load failed: {e}"));
                e.status()
            }
        }
    })
}

unsafe extern "C" fn enable<P: Plugin>(ctx: *mut abi::Context) -> Status {
    guard(|| {
        let mut ctx = Ctx::new(ctx);
        with_plugin::<P, _>(|p| p.enable(&mut ctx)).map_or(status::WRONG_PHASE, |()| status::OK)
    })
}

unsafe extern "C" fn disable<P: Plugin>(ctx: *mut abi::Context) -> Status {
    guard(|| {
        let mut ctx = Ctx::new(ctx);
        with_plugin::<P, _>(|p| p.disable(&mut ctx)).map_or(status::WRONG_PHASE, |()| status::OK)
    })
}

unsafe extern "C" fn save_state<P: Plugin>(sink: *const abi::ByteSink) -> Status {
    guard(|| {
        if sink.is_null() {
            return status::INVALID_ARGUMENT;
        }
        let Some(state) = with_plugin::<P, _>(Plugin::save_state) else {
            return status::WRONG_PHASE;
        };
        // SAFETY: the host passes a valid sink for the call; it copies the bytes.
        unsafe { ((*sink).write)((*sink).state, abi::Bytes::new(&state)) };
        status::OK
    })
}

unsafe extern "C" fn restore_state<P: Plugin>(state: abi::Bytes) -> Status {
    guard(|| {
        let bytes: &[u8] = if state.len == 0 || state.ptr.is_null() {
            &[]
        } else {
            // SAFETY: the host passes bytes valid for the call.
            unsafe { std::slice::from_raw_parts(state.ptr, state.len) }
        };
        with_plugin::<P, _>(|p| p.restore_state(bytes)).map_or(status::WRONG_PHASE, |()| status::OK)
    })
}
