//! Load bot: many headless offline-mode clients in one process, one tokio task per bot.
//!
//! ```text
//! loadbot --host <host> --port <port> [--join-rate <per s>] [--join-concurrency <n>]
//!         [--radius <blocks>] [--chat-interval <s>] [--view-distance <chunks>]
//!         [--prefix <name>] [--seed <n>]
//! ```
//!
//! Control on stdin, one command per line: `target <n>` raises the number of bots to `n`
//! (bots join at the join rate), `quit` ends the process. Every second one JSON line with the
//! metrics of that second goes to stdout (see [`Metrics::report`]). Driven by
//! `cargo xtask loadtest`.

mod bot;

use std::collections::HashMap;
use std::io::BufRead;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{Semaphore, watch};

/// Bot settings shared by all bots.
#[derive(Debug, Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    /// Bot joins started per second.
    pub join_rate: f64,
    /// Bots logging in at the same time (servers limit pending connections per address).
    pub join_concurrency: usize,
    /// Bots walk within this many blocks around their spawn point.
    pub radius: f64,
    /// Seconds between chat messages of one bot; 0 turns chat off.
    pub chat_interval: f64,
    /// View distance the bots send in their client information.
    pub view_distance: i8,
    pub prefix: String,
    pub seed: u64,
}

/// Counters shared by all bots; the reporter turns them into one JSON line per second.
#[derive(Debug, Default)]
pub struct Metrics {
    pub online: AtomicU64,
    pub joined: AtomicU64,
    pub failed: AtomicU64,
    pub disconnected: AtomicU64,
    pub bytes_in: AtomicU64,
    pub bytes_out: AtomicU64,
    pub packets_in: AtomicU64,
    pub packets_out: AtomicU64,
    /// Join latencies (connect to first chunk) in milliseconds since the last report.
    pub join_ms: Mutex<Vec<f64>>,
    /// Round trips of `ping_request` in milliseconds since the last report.
    pub rtt_ms: Mutex<Vec<f64>>,
    /// Failure and disconnect reasons since the last report.
    pub errors: Mutex<HashMap<String, u64>>,
}

impl Metrics {
    /// Records a failed join or a disconnect.
    pub fn error(&self, reason: String) {
        let mut reason = reason;
        reason.truncate(120);
        if let Ok(mut errors) = self.errors.lock() {
            *errors.entry(reason).or_insert(0) += 1;
        }
    }

    /// One report: cumulative counters, the samples since the last report and the CPU time
    /// of this process (clock ticks), so a saturated bot host can be detected.
    fn report(&self, start: Instant, target: u64) -> serde_json::Value {
        let take = |v: &Mutex<Vec<f64>>| {
            v.lock()
                .map(|mut v| std::mem::take(&mut *v))
                .unwrap_or_default()
        };
        let errors = self
            .errors
            .lock()
            .map(|mut e| std::mem::take(&mut *e))
            .unwrap_or_default();
        let load = |c: &AtomicU64| c.load(Ordering::Relaxed);
        serde_json::json!({
            "t": start.elapsed().as_secs_f64(),
            "target": target,
            "online": load(&self.online),
            "joined": load(&self.joined),
            "failed": load(&self.failed),
            "disconnected": load(&self.disconnected),
            "bytes_in": load(&self.bytes_in),
            "bytes_out": load(&self.bytes_out),
            "packets_in": load(&self.packets_in),
            "packets_out": load(&self.packets_out),
            "cpu_ticks": process_cpu_ticks(),
            "join_ms": take(&self.join_ms),
            "rtt_ms": take(&self.rtt_ms),
            "errors": errors,
        })
    }
}

/// User plus system time of this process in clock ticks, from `/proc/self/stat`.
fn process_cpu_ticks() -> u64 {
    let Ok(stat) = std::fs::read_to_string("/proc/self/stat") else {
        return 0;
    };
    // Fields after the command name (which may contain spaces): state is field 3, utime 14.
    let rest = stat.rsplit_once(") ").map_or("", |(_, r)| r);
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let field = |i: usize| {
        fields
            .get(i)
            .and_then(|f| f.parse::<u64>().ok())
            .unwrap_or(0)
    };
    field(11) + field(12)
}

fn parse_args() -> Result<Config, String> {
    let mut config = Config {
        host: "127.0.0.1".into(),
        port: 25565,
        join_rate: 20.0,
        join_concurrency: 4,
        radius: 48.0,
        chat_interval: 0.0,
        view_distance: 10,
        prefix: "bot".into(),
        seed: 1,
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{arg} needs a value"));
        let bad = |e: &dyn std::fmt::Display| format!("{arg}: {e}");
        match arg.as_str() {
            "--host" => config.host = value()?.clone(),
            "--port" => config.port = value()?.parse().map_err(|e| bad(&e))?,
            "--join-rate" => config.join_rate = value()?.parse().map_err(|e| bad(&e))?,
            "--join-concurrency" => {
                config.join_concurrency = value()?.parse().map_err(|e| bad(&e))?;
            }
            "--radius" => config.radius = value()?.parse().map_err(|e| bad(&e))?,
            "--chat-interval" => config.chat_interval = value()?.parse().map_err(|e| bad(&e))?,
            "--view-distance" => config.view_distance = value()?.parse().map_err(|e| bad(&e))?,
            "--prefix" => config.prefix = value()?.clone(),
            "--seed" => config.seed = value()?.parse().map_err(|e| bad(&e))?,
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    let rate_ok = config.join_rate > 0.0;
    if config.prefix.len() > 10 || !rate_ok || config.join_concurrency == 0 {
        return Err("--prefix must be at most 10 characters, --join-rate and \
                    --join-concurrency positive"
            .into());
    }
    Ok(config)
}

fn main() {
    let config = match parse_args() {
        Ok(c) => Arc::new(c),
        Err(e) => {
            eprintln!("loadbot: {e}");
            std::process::exit(2);
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("loadbot: cannot start the runtime: {e}");
            std::process::exit(1);
        }
    };
    let metrics = Arc::new(Metrics::default());
    let (target_tx, target_rx) = watch::channel(0u64);
    let start = Instant::now();

    runtime.spawn(spawner(
        Arc::clone(&config),
        Arc::clone(&metrics),
        target_rx.clone(),
    ));
    let reporter_metrics = Arc::clone(&metrics);
    runtime.spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        loop {
            interval.tick().await;
            let target = *target_rx.borrow();
            println!("{}", reporter_metrics.report(start, target));
        }
    });

    // Control commands on stdin; the runtime keeps running on its own threads.
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        match line.split_whitespace().collect::<Vec<_>>().as_slice() {
            ["target", n] => match n.parse::<u64>() {
                Ok(n) => {
                    let _ = target_tx.send(n);
                }
                Err(e) => eprintln!("loadbot: target: {e}"),
            },
            ["quit"] => break,
            [] => {}
            _ => eprintln!("loadbot: unknown command {line:?}"),
        }
    }
    println!("{}", metrics.report(start, *target_tx.borrow()));
    runtime.shutdown_timeout(Duration::from_secs(1));
}

/// Starts bots until the target count is reached, at the join rate and with at most
/// `join_concurrency` bots logging in at once.
async fn spawner(config: Arc<Config>, metrics: Arc<Metrics>, mut target: watch::Receiver<u64>) {
    let pending = Arc::new(Semaphore::new(config.join_concurrency));
    let mut interval = tokio::time::interval(Duration::from_secs_f64(1.0 / config.join_rate));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut started = 0u64;
    loop {
        while started >= *target.borrow() {
            if target.changed().await.is_err() {
                return;
            }
        }
        interval.tick().await;
        let Ok(permit) = Arc::clone(&pending).acquire_owned().await else {
            return;
        };
        let index = started;
        started += 1;
        tokio::spawn(bot::run(
            index,
            Arc::clone(&config),
            Arc::clone(&metrics),
            permit,
        ));
    }
}
