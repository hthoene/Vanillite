//! A small logger in Vanilla's console format: `[HH:MM:SS] [thread/LEVEL]: message`.
//!
//! Times are UTC: the standard library has no time zone database and the logger is not worth
//! a dependency.

use std::io::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

use log::{Level, LevelFilter, Log, Metadata, Record};

struct ConsoleLogger {
    level: LevelFilter,
}

impl Log for ConsoleLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("unnamed");
        let level = match record.level() {
            Level::Error => "ERROR",
            Level::Warn => "WARN",
            Level::Info => "INFO",
            Level::Debug => "DEBUG",
            Level::Trace => "TRACE",
        };
        let mut out = std::io::stderr().lock();
        let _ = writeln!(
            out,
            "[{h:02}:{m:02}:{s:02}] [{name}/{level}]: {}",
            record.args()
        );
    }

    fn flush(&self) {
        let _ = std::io::stderr().flush();
    }
}

/// Installs the logger. The level comes from `VANILLITE_LOG` (default `info`).
pub fn init() {
    let level = std::env::var("VANILLITE_LOG")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(LevelFilter::Info);
    let logger = Box::leak(Box::new(ConsoleLogger { level }));
    if log::set_logger(logger).is_ok() {
        log::set_max_level(level);
    }
}
