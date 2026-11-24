//! Crash attribution and safe mode.
//!
//! Around every call into a plugin the host records the plugin in a per-thread slot. The
//! watchdog lists the slots of hanging threads; a fatal signal (segfault, abort) and a
//! watchdog kill append the plugins active at that moment to `plugins/.crashed`. At the next
//! start, with `plugins.safe-mode-after-crash`, a plugin listed there is skipped as long as its
//! library is the same build (same SHA-256).

use std::cell::Cell;
use std::ffi::CString;
use std::fmt::Write as _;
use std::path::Path;
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

/// The crash file in the plugin folder.
pub(crate) const CRASH_FILE: &str = ".crashed";

/// What a crash records about a plugin: its id and a ready line for the crash file.
#[derive(Debug)]
pub(crate) struct Tag {
    pub(crate) id: String,
    /// `<id>\t<sha256>\n`, written as is from a signal handler.
    line: Box<[u8]>,
}

impl Tag {
    /// A tag for a plugin; leaked, plugins are never unloaded.
    pub(crate) fn new(id: &str, sha256: &str) -> &'static Tag {
        Box::leak(Box::new(Tag {
            id: id.to_owned(),
            line: format!("{id}\t{sha256}\n").into_bytes().into_boxed_slice(),
        }))
    }
}

/// The plugin a thread is in right now.
#[derive(Debug)]
struct Slot {
    thread: String,
    plugin: AtomicPtr<Tag>,
}

static SLOTS: Mutex<Vec<&'static Slot>> = Mutex::new(Vec::new());

thread_local! {
    static SLOT: Cell<Option<&'static Slot>> = const { Cell::new(None) };
}

fn slot() -> &'static Slot {
    SLOT.with(|cell| {
        if let Some(slot) = cell.get() {
            return slot;
        }
        let thread = std::thread::current();
        let slot: &'static Slot = Box::leak(Box::new(Slot {
            thread: thread.name().unwrap_or("unnamed").to_owned(),
            plugin: AtomicPtr::new(ptr::null_mut()),
        }));
        SLOTS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(slot);
        cell.set(Some(slot));
        slot
    })
}

/// Marks this thread as inside `tag` until dropped.
#[derive(Debug)]
pub(crate) struct Entered {
    slot: &'static Slot,
    previous: *mut Tag,
}

impl Drop for Entered {
    fn drop(&mut self) {
        self.slot.plugin.store(self.previous, Ordering::Release);
    }
}

/// Enters a plugin on this thread. `None` when this thread is already inside it: a host
/// function of the plugin led back into it, which the host does not allow (re-entrancy rule).
pub(crate) fn enter(tag: &'static Tag) -> Option<Entered> {
    let slot = slot();
    let tag = ptr::from_ref(tag).cast_mut();
    let previous = slot.plugin.load(Ordering::Acquire);
    if previous == tag {
        return None;
    }
    slot.plugin.store(tag, Ordering::Release);
    Some(Entered { slot, previous })
}

/// The plugin the current thread is in, if any.
pub fn current_plugin() -> Option<String> {
    let slot = SLOT.with(Cell::get)?;
    // SAFETY: tags are leaked and never freed.
    unsafe { slot.plugin.load(Ordering::Acquire).as_ref() }.map(|t| t.id.clone())
}

/// Threads that are inside a plugin right now: `(thread name, plugin id)`. For watchdog and
/// crash reports.
pub fn active_plugins() -> Vec<(String, String)> {
    SLOTS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .filter_map(|slot| {
            // SAFETY: tags are leaked and never freed.
            let tag = unsafe { slot.plugin.load(Ordering::Acquire).as_ref() }?;
            Some((slot.thread.clone(), tag.id.clone()))
        })
        .collect()
}

/// The crash file of the running host, as a C string for the signal handler.
static CRASH_PATH: OnceLock<CString> = OnceLock::new();

/// Remembers where crashes are recorded and installs the fatal signal handlers (once).
pub(crate) fn install(plugins_dir: &Path) {
    let path = plugins_dir.join(CRASH_FILE);
    if let Ok(c) = CString::new(path.to_string_lossy().into_owned()) {
        let _ = CRASH_PATH.set(c);
    }
    #[cfg(unix)]
    signals::install();
}

/// Records the plugins active right now in the crash file (watchdog kill). Returns their ids.
pub fn record_crash() -> Vec<String> {
    let mut ids = Vec::new();
    let mut text = String::new();
    for slot in SLOTS.lock().unwrap_or_else(PoisonError::into_inner).iter() {
        // SAFETY: tags are leaked and never freed.
        if let Some(tag) = unsafe { slot.plugin.load(Ordering::Acquire).as_ref() } {
            text.push_str(&String::from_utf8_lossy(&tag.line));
            ids.push(tag.id.clone());
        }
    }
    if let (false, Some(path)) = (text.is_empty(), CRASH_PATH.get()) {
        let path = Path::new(path.to_str().unwrap_or_default());
        let mut old = std::fs::read_to_string(path).unwrap_or_default();
        old.push_str(&text);
        let _ = std::fs::write(path, old);
    }
    ids
}

/// Reads the crash file: `(plugin id, sha256)` of plugins that crashed the server.
pub(crate) fn read_crashed(plugins_dir: &Path) -> Vec<(String, String)> {
    let text = std::fs::read_to_string(plugins_dir.join(CRASH_FILE)).unwrap_or_default();
    let mut out: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        if let Some((id, sha)) = line.split_once('\t')
            && !out.iter().any(|(i, s)| i == id && s == sha)
        {
            out.push((id.to_owned(), sha.to_owned()));
        }
    }
    out
}

/// Rewrites the crash file with `entries` (deletes it when empty).
pub(crate) fn write_crashed(plugins_dir: &Path, entries: &[(String, String)]) {
    let path = plugins_dir.join(CRASH_FILE);
    if entries.is_empty() {
        let _ = std::fs::remove_file(path);
        return;
    }
    let mut text = String::new();
    for (id, sha) in entries {
        let _ = writeln!(text, "{id}\t{sha}");
    }
    let _ = std::fs::write(path, text);
}

#[cfg(unix)]
mod signals {
    //! Fatal signal handlers: append the active plugin to the crash file, then hand the signal
    //! to the previous handler (Rust's stack overflow handler or the default action).

    use std::sync::Once;
    use std::sync::atomic::Ordering;

    const SIGNALS: [libc::c_int; 5] = [
        libc::SIGSEGV,
        libc::SIGBUS,
        libc::SIGILL,
        libc::SIGFPE,
        libc::SIGABRT,
    ];

    static mut PREVIOUS: [Option<libc::sigaction>; 5] = [None; 5];
    static INSTALL: Once = Once::new();

    pub(super) fn install() {
        INSTALL.call_once(|| {
            for (i, &signal) in SIGNALS.iter().enumerate() {
                // SAFETY: plain sigaction calls with zero-initialised structs; `PREVIOUS` is
                // only written here, once, before any handler can run.
                unsafe {
                    let mut action: libc::sigaction = std::mem::zeroed();
                    action.sa_sigaction = handler as *const () as libc::sighandler_t;
                    action.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
                    libc::sigemptyset(&mut action.sa_mask);
                    let mut previous: libc::sigaction = std::mem::zeroed();
                    if libc::sigaction(signal, &action, &mut previous) == 0 {
                        (*std::ptr::addr_of_mut!(PREVIOUS))[i] = Some(previous);
                    }
                }
            }
        });
    }

    extern "C" fn handler(signal: libc::c_int, _: *mut libc::siginfo_t, _: *mut libc::c_void) {
        // Only async-signal-safe work: reads of a thread-local pointer, open, write, close and
        // sigaction.
        if let Some(slot) = super::SLOT.with(std::cell::Cell::get)
            // SAFETY: tags are leaked and never freed.
            && let Some(tag) = unsafe { slot.plugin.load(Ordering::Acquire).as_ref() }
            && let Some(path) = super::CRASH_PATH.get()
        {
            // SAFETY: valid C string and buffer; the descriptor is closed again.
            unsafe {
                let fd = libc::open(
                    path.as_ptr(),
                    libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND,
                    0o644,
                );
                if fd >= 0 {
                    libc::write(fd, tag.line.as_ptr().cast(), tag.line.len());
                    libc::close(fd);
                }
            }
        }
        let index = SIGNALS.iter().position(|&s| s == signal);
        // SAFETY: `PREVIOUS` is written once before handlers are installed. Restoring the
        // previous action and returning re-raises the fault (or lets `abort` finish) under it.
        unsafe {
            match index.and_then(|i| (*std::ptr::addr_of!(PREVIOUS))[i]) {
                Some(previous) => {
                    libc::sigaction(signal, &previous, std::ptr::null_mut());
                }
                None => {
                    libc::signal(signal, libc::SIG_DFL);
                }
            }
        }
    }
}
