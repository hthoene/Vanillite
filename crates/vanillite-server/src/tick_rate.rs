//! The tick rate manager: Vanilla's `/tick` state (rate, freeze, step, sprint).

/// Nanoseconds per second.
pub const NANOS_PER_SECOND: u64 = 1_000_000_000;
/// Vanilla's tick rate.
pub const DEFAULT_TICK_RATE: f32 = 20.0;

/// Tick rate, freeze, step and sprint state of the server, as in Vanilla.
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

    /// Stops a running sprint. Returns whether one was running.
    pub fn stop_sprinting(&mut self) -> bool {
        if self.remaining_sprint_ticks > 0 {
            self.finish_sprint();
            true
        } else {
            false
        }
    }

    /// Called before each tick while sprinting: whether this tick is a sprint tick.
    pub fn check_should_sprint_this_tick(&mut self, now: u64) -> bool {
        if !self.is_sprinting() {
            return false;
        }
        if self.remaining_sprint_ticks > 0 {
            self.sprint_tick_start = now;
            self.remaining_sprint_ticks -= 1;
            true
        } else {
            self.finish_sprint();
            false
        }
    }

    /// Called after a sprint tick finished.
    pub fn end_tick_work(&mut self, now: u64) {
        self.sprint_time_spent += now.saturating_sub(self.sprint_tick_start);
    }

    fn finish_sprint(&mut self) {
        let completed = self.scheduled_sprint_ticks - self.remaining_sprint_ticks;
        let millis = (self.sprint_time_spent as f64).max(1.0) / 1e6;
        let tps = (1000.0 * completed as f64 / millis) as u64;
        let mspt = if completed == 0 {
            f64::from(self.tick_rate)
        } else {
            millis / completed as f64
        };
        log::info!("Sprint completed with {tps} ticks per second, or {mspt:.2} ms per tick");
        self.scheduled_sprint_ticks = 0;
        self.sprint_time_spent = 0;
        self.remaining_sprint_ticks = 0;
        self.frozen = self.frozen_before_sprint;
    }

    /// Advances the freeze/step state at the start of a server tick.
    pub fn tick(&mut self) {
        self.run_game_elements = !self.frozen || self.frozen_ticks_to_run > 0;
        if self.frozen_ticks_to_run > 0 {
            self.frozen_ticks_to_run -= 1;
        }
    }
}
