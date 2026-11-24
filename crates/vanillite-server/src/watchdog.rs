//! The watchdog: detects a hanging server, warns early and stops the process at the hard limit
//! (`max-tick-time`), like Vanilla's `ServerWatchdog` with Paper-style early warnings.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::clock::Clock;
use crate::tick_rate::NANOS_PER_SECOND;

/// Marks a thread as busy with a tick or a task, so the watchdog can name the culprit.
#[derive(Debug)]
pub struct Heartbeat {
    name: String,
    busy_since: AtomicU64,
}

impl Heartbeat {
    /// A heartbeat for the thread or world `name`.
    pub fn new(name: impl Into<String>) -> Heartbeat {
        Heartbeat {
            name: name.into(),
            busy_since: AtomicU64::new(0),
        }
    }

    /// Starts a unit of work.
    pub fn enter(&self, clock: &Clock) {
        self.busy_since.store(clock.now(), Ordering::Relaxed);
    }

    /// Ends the unit of work.
    pub fn leave(&self) {
        self.busy_since.store(0, Ordering::Relaxed);
    }
}

/// State the tick loop publishes for the watchdog.
#[derive(Debug)]
pub struct WatchState {
    /// Clock time of the last tick start or end; 0 = nothing to watch (before the first tick,
    /// during shutdown). An overloaded server that falls behind still makes progress; only a
    /// tick that does not finish (or a server thread that never starts the next one) counts.
    pub progress: AtomicU64,
    /// Nanoseconds after the deadline before the first warning; 0 = no early warnings.
    pub early_warning: AtomicU64,
    /// Nanoseconds between further warnings.
    pub warning_interval: AtomicU64,
    heartbeats: Mutex<Vec<Arc<Heartbeat>>>,
}

impl WatchState {
    /// New state with the given warning settings in seconds.
    pub fn new(early_warning_secs: Option<u32>, interval_secs: u32) -> WatchState {
        let state = WatchState {
            progress: AtomicU64::new(0),
            early_warning: AtomicU64::new(0),
            warning_interval: AtomicU64::new(0),
            heartbeats: Mutex::new(Vec::new()),
        };
        state.configure(early_warning_secs, interval_secs);
        state
    }

    /// Changes the warning settings (config reload).
    pub fn configure(&self, early_warning_secs: Option<u32>, interval_secs: u32) {
        let early = early_warning_secs.map_or(0, |s| u64::from(s) * NANOS_PER_SECOND);
        self.early_warning.store(early, Ordering::Relaxed);
        self.warning_interval.store(
            u64::from(interval_secs) * NANOS_PER_SECOND,
            Ordering::Relaxed,
        );
    }

    /// Registers a heartbeat to report.
    pub fn register(&self, heartbeat: Arc<Heartbeat>) {
        self.heartbeats
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(heartbeat);
    }

    fn report_busy(&self, now: u64, log: fn(&str)) {
        let heartbeats = self
            .heartbeats
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        for heartbeat in heartbeats.iter() {
            let since = heartbeat.busy_since.load(Ordering::Relaxed);
            if since != 0 {
                let secs = now.saturating_sub(since) as f64 / 1e9;
                log(&format!(
                    "  {} has been busy for {secs:.2} s",
                    heartbeat.name
                ));
            }
        }
        for (thread, plugin) in vanillite_plugin_host::crash::active_plugins() {
            log(&format!("  thread {thread} is inside plugin {plugin}"));
        }
    }
}

/// The running watchdog thread.
#[derive(Debug)]
pub struct Watchdog {
    stop: Sender<()>,
    handle: Option<JoinHandle<()>>,
}

impl Watchdog {
    /// Starts the watchdog. `max_tick_millis <= 0` disables the hard limit (Vanilla).
    pub fn start(
        state: Arc<WatchState>,
        clock: Clock,
        max_tick_millis: i64,
    ) -> std::io::Result<Watchdog> {
        let (stop, stopped) = mpsc::channel::<()>();
        let hard_limit = u64::try_from(max_tick_millis)
            .ok()
            .filter(|&ms| ms > 0)
            .map(|ms| ms.saturating_mul(1_000_000));
        let handle = std::thread::Builder::new()
            .name("Server Watchdog".to_owned())
            .spawn(move || {
                let mut last_warning = 0u64;
                loop {
                    match stopped.recv_timeout(Duration::from_secs(1)) {
                        Err(RecvTimeoutError::Timeout) => {}
                        Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
                    }
                    check(&state, &clock, hard_limit, &mut last_warning);
                }
            })?;
        Ok(Watchdog {
            stop,
            handle: Some(handle),
        })
    }

    /// Stops the watchdog thread.
    pub fn stop(mut self) {
        let _ = self.stop.send(());
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn check(state: &WatchState, clock: &Clock, hard_limit: Option<u64>, last_warning: &mut u64) {
    let progress = state.progress.load(Ordering::Relaxed);
    let now = clock.now();
    if progress == 0 || now <= progress {
        return;
    }
    let behind = now - progress;
    if let Some(limit) = hard_limit
        && behind > limit
    {
        log::error!(
            "A single server tick took {:.2} seconds (should be max {:.2})",
            behind as f64 / 1e9,
            0.05
        );
        state.report_busy(now, |line| log::error!("{line}"));
        let plugins = vanillite_plugin_host::crash::record_crash();
        if !plugins.is_empty() {
            log::error!(
                "The hang is attributed to plugin(s) {}; with plugins.safe-mode-after-crash \
                 they are skipped at the next start",
                plugins.join(", ")
            );
        }
        log::error!("Considering it to be crashed, server will forcibly shutdown.");
        std::process::exit(1);
    }
    let early = state.early_warning.load(Ordering::Relaxed);
    let interval = state.warning_interval.load(Ordering::Relaxed);
    if early != 0 && behind > early && now.saturating_sub(*last_warning) >= interval {
        *last_warning = now;
        log::warn!(
            "The server has not responded for {:.1} seconds",
            behind as f64 / 1e9
        );
        state.report_busy(now, |line| log::warn!("{line}"));
    }
}
