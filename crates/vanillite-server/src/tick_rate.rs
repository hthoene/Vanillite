//! The tick rate manager: Vanilla's `/tick` state (rate, freeze, step, sprint).

/// Nanoseconds per second.
pub const NANOS_PER_SECOND: u64 = 1_000_000_000;
/// Vanilla's tick rate.
pub const DEFAULT_TICK_RATE: f32 = 20.0;

/// Tick rate, freeze, step and sprint state of the server, as in Vanilla
/// (`ServerTickRateManager`). Shared in [`Global`](crate::global::Global): the server loop
/// reads it once per tick, `/tick` changes it.
#[derive(Debug)]
pub struct TickRateManager {
    tick_rate: f32,
    nanos_per_tick: u64,
    frozen: bool,
    frozen_ticks_to_run: u32,
    run_game_elements: bool,
    scheduled_sprint_ticks: u64,
    remaining_sprint_ticks: u64,
    sprint_tick_start: u64,
    sprint_time_spent: u64,
    frozen_before_sprint: bool,
    /// The report of a sprint that ended in the server loop, for its announcement.
    finished_sprint: Option<SprintReport>,
}

/// `commands.tick.sprint.report`'s arguments.
#[derive(Clone, Debug, PartialEq)]
pub struct SprintReport {
    /// Ticks per second.
    pub ticks_per_second: i32,
    /// Milliseconds per tick, `%.2f`.
    pub millis_per_tick: String,
}

impl Default for TickRateManager {
    fn default() -> Self {
        TickRateManager {
            tick_rate: DEFAULT_TICK_RATE,
            nanos_per_tick: NANOS_PER_SECOND / 20,
            frozen: false,
            frozen_ticks_to_run: 0,
            run_game_elements: true,
            scheduled_sprint_ticks: 0,
            remaining_sprint_ticks: 0,
            sprint_tick_start: 0,
            sprint_time_spent: 0,
            frozen_before_sprint: false,
            finished_sprint: None,
        }
    }
}

impl TickRateManager {
    /// The target ticks per second.
    pub fn tick_rate(&self) -> f32 {
        self.tick_rate
    }

    /// Sets the target ticks per second (`/tick rate`), clamped to at least 1.
    pub fn set_tick_rate(&mut self, rate: f32) {
        self.tick_rate = rate.max(1.0);
        self.nanos_per_tick = (NANOS_PER_SECOND as f64 / f64::from(self.tick_rate)) as u64;
    }

    /// The length of one tick in nanoseconds.
    pub fn nanos_per_tick(&self) -> u64 {
        self.nanos_per_tick
    }

    /// Whether the game is frozen (`/tick freeze`).
    pub fn is_frozen(&self) -> bool {
        self.frozen
    }

    /// Freezes or unfreezes the game.
    pub fn set_frozen(&mut self, frozen: bool) {
        self.frozen = frozen;
    }

    /// Whether game elements run in the current tick (not frozen, or stepping).
    pub fn runs_normally(&self) -> bool {
        self.run_game_elements
    }

    /// `isSteppingForward`.
    pub fn is_stepping(&self) -> bool {
        self.frozen_ticks_to_run > 0
    }

    /// `frozenTicksToRun`: the steps left (`ticking_step`).
    pub fn frozen_ticks_to_run(&self) -> u32 {
        self.frozen_ticks_to_run
    }

    /// Runs `ticks` ticks while frozen (`/tick step`). Fails when not frozen.
    pub fn step(&mut self, ticks: u32) -> bool {
        if !self.frozen {
            return false;
        }
        self.frozen_ticks_to_run = ticks;
        true
    }

    /// Stops stepping. Returns whether a step was running.
    pub fn stop_stepping(&mut self) -> bool {
        let stepping = self.frozen_ticks_to_run > 0;
        self.frozen_ticks_to_run = 0;
        stepping
    }

    /// Whether a sprint is running (`/tick sprint`).
    pub fn is_sprinting(&self) -> bool {
        self.scheduled_sprint_ticks > 0
    }

    /// Starts a sprint of `ticks` ticks. Returns whether a running sprint was interrupted.
    pub fn request_sprint(&mut self, ticks: u64) -> bool {
        let interrupted = self.remaining_sprint_ticks > 0;
        self.sprint_time_spent = 0;
        self.scheduled_sprint_ticks = ticks;
        self.remaining_sprint_ticks = ticks;
        self.frozen_before_sprint = self.frozen;
        self.frozen = false;
        interrupted
    }

    /// Stops a running sprint: its report, `None` if none was running.
    pub fn stop_sprinting(&mut self) -> Option<SprintReport> {
        (self.remaining_sprint_ticks > 0).then(|| self.finish_sprint())
    }

    /// Called before each tick while sprinting: whether this tick is a sprint tick. A sprint
    /// that ends leaves its report for [`take_finished_sprint`](Self::take_finished_sprint).
    pub fn check_should_sprint_this_tick(&mut self, now: u64) -> bool {
        if !self.run_game_elements {
            return false;
        }
        if self.remaining_sprint_ticks > 0 {
            self.sprint_tick_start = now;
            self.remaining_sprint_ticks -= 1;
            true
        } else {
            let report = self.finish_sprint();
            self.finished_sprint = Some(report);
            false
        }
    }

    /// The report of a sprint that ended in [`check_should_sprint_this_tick`].
    ///
    /// [`check_should_sprint_this_tick`]: Self::check_should_sprint_this_tick
    pub fn take_finished_sprint(&mut self) -> Option<SprintReport> {
        self.finished_sprint.take()
    }

    /// Called after a sprint tick finished.
    pub fn end_tick_work(&mut self, now: u64) {
        self.sprint_time_spent += now.saturating_sub(self.sprint_tick_start);
    }

    /// `finishTickSprint`: the sprint ends and the freeze before it comes back.
    fn finish_sprint(&mut self) -> SprintReport {
        let completed = self.scheduled_sprint_ticks - self.remaining_sprint_ticks;
        let millis = (self.sprint_time_spent as f64).max(1.0) / 1e6;
        let ticks_per_second = (1000.0 * completed as f64 / millis) as i32;
        let mspt = if completed == 0 {
            f64::from(self.nanos_per_tick as f32 / 1e6)
        } else {
            millis / completed as f64
        };
        log::info!(
            "Sprint completed with {ticks_per_second} ticks per second, or {mspt:.2} ms per tick"
        );
        self.scheduled_sprint_ticks = 0;
        self.sprint_time_spent = 0;
        self.remaining_sprint_ticks = 0;
        self.frozen = self.frozen_before_sprint;
        SprintReport {
            ticks_per_second,
            millis_per_tick: format!("{mspt:.2}"),
        }
    }

    /// Advances the freeze/step state at the start of a server tick.
    pub fn tick(&mut self) {
        self.run_game_elements = !self.frozen || self.frozen_ticks_to_run > 0;
        if self.frozen_ticks_to_run > 0 {
            self.frozen_ticks_to_run -= 1;
        }
    }
}

/// `MinecraftServer.tickTimesNanos`: the durations of the last 100 ticks, plus the last
/// [`HISTORY`] ticks' durations and start intervals for Vanillite's `tps`, `mspt` and `status`
/// (5 s, 1 min and 5 min windows; two stores per tick into a buffer allocated once).
#[derive(Debug)]
pub struct TickTimes {
    samples: [u64; 100],
    sum: u64,
    ticks: u64,
    history: Box<[TickSample]>,
    last_start: Option<u64>,
}

/// Ticks of the longest window of [`TickTimes::window`] (5 minutes at 20 ticks per second).
pub const HISTORY: usize = 6000;

/// One tick of the history: its duration and the time since the previous tick started
/// (nanoseconds, saturated; 0 = unknown).
#[derive(Debug, Clone, Copy, Default)]
struct TickSample {
    nanos: u32,
    interval: u32,
}

/// Tick figures over a window of recent ticks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TickWindow {
    /// Ticks in the window.
    pub ticks: usize,
    /// Ticks per second (ticks over the time between their starts).
    pub tps: f64,
    /// Mean milliseconds per tick.
    pub mean: f64,
    /// Median milliseconds per tick.
    pub p50: f64,
    /// 95th percentile.
    pub p95: f64,
    /// Longest tick.
    pub max: f64,
    /// Shortest tick.
    pub min: f64,
}

impl Default for TickTimes {
    fn default() -> Self {
        TickTimes {
            samples: [0; 100],
            sum: 0,
            ticks: 0,
            history: vec![TickSample::default(); HISTORY].into_boxed_slice(),
            last_start: None,
        }
    }
}

impl TickTimes {
    /// Records the duration of tick number `tick`.
    pub fn record(&mut self, tick: u64, nanos: u64) {
        let index = usize::try_from(tick % 100).unwrap_or(0);
        if let Some(slot) = self.samples.get_mut(index) {
            self.sum = self.sum.saturating_sub(*slot).saturating_add(nanos);
            *slot = nanos;
        }
        self.ticks = tick;
    }

    /// Records tick number `tick` that started at `start` (clock nanoseconds) and took
    /// `nanos`: [`TickTimes::record`] plus the history.
    pub fn record_tick(&mut self, tick: u64, start: u64, nanos: u64) {
        self.record(tick, nanos);
        let interval = self.last_start.map_or(0, |last| start.saturating_sub(last));
        self.last_start = Some(start);
        let index = usize::try_from(tick % HISTORY as u64).unwrap_or(0);
        if let Some(slot) = self.history.get_mut(index) {
            *slot = TickSample {
                nanos: u32::try_from(nanos).unwrap_or(u32::MAX),
                interval: u32::try_from(interval).unwrap_or(u32::MAX),
            };
        }
    }

    /// The figures of the last `ticks` recorded ticks (fewer when fewer ran); `None` before
    /// the first tick.
    pub fn window(&self, ticks: usize) -> Option<TickWindow> {
        let recorded = usize::try_from(self.ticks)
            .unwrap_or(usize::MAX)
            .min(HISTORY);
        let n = ticks.min(recorded);
        if n == 0 {
            return None;
        }
        let mut nanos = Vec::with_capacity(n);
        let mut intervals = 0u64;
        let mut timed = 0u64;
        for back in 0..n {
            let tick = self.ticks.saturating_sub(back as u64);
            let index = usize::try_from(tick % HISTORY as u64).unwrap_or(0);
            let Some(sample) = self.history.get(index) else {
                continue;
            };
            nanos.push(sample.nanos);
            if sample.interval > 0 {
                intervals += u64::from(sample.interval);
                timed += 1;
            }
        }
        nanos.sort_unstable();
        let millis = |v: u32| f64::from(v) / 1e6;
        let at = |q: f64| {
            let last = nanos.len().saturating_sub(1);
            let i = ((last as f64) * q).round() as usize;
            nanos.get(i.min(last)).copied().map_or(0.0, millis)
        };
        let total: u64 = nanos.iter().map(|&v| u64::from(v)).sum();
        let tps = if intervals > 0 {
            timed as f64 * 1e9 / intervals as f64
        } else {
            0.0
        };
        Some(TickWindow {
            ticks: nanos.len(),
            tps,
            mean: total as f64 / nanos.len().max(1) as f64 / 1e6,
            p50: at(0.5),
            p95: at(0.95),
            max: nanos.last().copied().map_or(0.0, millis),
            min: nanos.first().copied().map_or(0.0, millis),
        })
    }

    /// The number of the last recorded tick.
    pub fn ticks(&self) -> u64 {
        self.ticks
    }

    /// The samples (`getTickTimesNanos`).
    pub fn samples(&self) -> &[u64; 100] {
        &self.samples
    }

    /// `getAverageTickTimeNanos`.
    pub fn average_nanos(&self) -> u64 {
        self.sum / self.ticks.clamp(1, 100)
    }
}
