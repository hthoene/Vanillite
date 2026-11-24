//! World clocks (Vanilla's `ServerClockManager`): the `minecraft:world_clock` registry, their
//! time markers from the timelines, ticking, commands and the `set_time` packet.

use vanillite_nbt::{Compound, Tag};
use vanillite_protocol::packets::play::clientbound::{ClockUpdate, SetTime};

/// `ClockTimeMarker`: a named point of a clock, optionally repeating every `period` ticks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimeMarker {
    /// Registry-style id, e.g. `minecraft:noon`.
    pub id: String,
    pub ticks: i32,
    pub period: Option<i32>,
    pub show_in_commands: bool,
}

impl TimeMarker {
    /// `occursAt`.
    pub fn occurs_at(&self, total: i64) -> bool {
        match self.period {
            None => i64::from(self.ticks) == total,
            Some(p) => i64::from(self.ticks) == total.rem_euclid(i64::from(p)),
        }
    }

    /// `resolveTimeToMoveTo`.
    pub fn resolve_time_to_move_to(&self, total: i64) -> i64 {
        match self.period {
            None => i64::from(self.ticks),
            Some(p) => {
                let p = i64::from(p);
                let duration = i64::from(self.ticks) - total % p;
                total + if duration > 0 { duration } else { p + duration }
            }
        }
    }
}

/// A timeline (`minecraft:timeline`) as far as commands need it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimelineInfo {
    pub id: String,
    /// Index of its clock in [`Clocks`].
    pub clock: usize,
    pub period: Option<i32>,
}

impl TimelineInfo {
    /// `getCurrentTicks`.
    pub fn current_ticks(&self, total: i64) -> i64 {
        match self.period {
            Some(p) => total.rem_euclid(i64::from(p)),
            None => total,
        }
    }

    /// `getPeriodCount`.
    pub fn period_count(&self, total: i64) -> i64 {
        match self.period {
            Some(p) => total / i64::from(p),
            None => 0,
        }
    }
}

/// One clock (`ServerClockInstance`).
#[derive(Clone, Debug, PartialEq)]
pub struct Clock {
    /// Registry id, e.g. `minecraft:overworld`.
    pub id: String,
    pub total_ticks: i64,
    pub partial_tick: f32,
    pub rate: f32,
    pub paused: bool,
    pub markers: Vec<TimeMarker>,
}

impl Clock {
    /// A clock at zero.
    pub fn new(id: impl Into<String>) -> Clock {
        Clock {
            id: id.into(),
            total_ticks: 0,
            partial_tick: 0.0,
            rate: 1.0,
            paused: false,
            markers: Vec::new(),
        }
    }

    fn tick(&mut self) {
        if !self.paused {
            self.partial_tick += self.rate;
            let full = self.partial_tick.floor();
            self.partial_tick -= full;
            // `Mth.floor` of a float that is at most a few thousand.
            #[allow(clippy::as_conversions, clippy::cast_possible_truncation)]
            let full = full as i64;
            self.total_ticks = self.total_ticks.saturating_add(full);
        }
    }

    /// The marker `id`.
    pub fn marker(&self, id: &str) -> Option<&TimeMarker> {
        self.markers.iter().find(|m| m.id == id)
    }
}

/// `ServerClockManager.MoveResult`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveResult {
    NoTimeMarkerFound,
    NotMoved,
    Moved,
}

/// All clocks of the server in registry (network id) order, and the timelines.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Clocks {
    pub clocks: Vec<Clock>,
    pub timelines: Vec<TimelineInfo>,
}

impl Clocks {
    /// The index of the clock `id`.
    pub fn index(&self, id: &str) -> Option<usize> {
        self.clocks.iter().position(|c| c.id == id)
    }

    /// The timeline `id`.
    pub fn timeline(&self, id: &str) -> Option<&TimelineInfo> {
        self.timelines.iter().find(|t| t.id == id)
    }

    /// `tick`: all clocks advance when `advance_time` is on.
    pub fn tick(&mut self, advance_time: bool) {
        if advance_time {
            for clock in &mut self.clocks {
                clock.tick();
            }
        }
    }

    /// `setTotalTicks`.
    pub fn set_total_ticks(&mut self, clock: usize, total: i64) {
        if let Some(c) = self.clocks.get_mut(clock) {
            c.total_ticks = total;
            c.partial_tick = 0.0;
        }
    }

    /// `addTicks`.
    pub fn add_ticks(&mut self, clock: usize, ticks: i32) {
        if let Some(c) = self.clocks.get_mut(clock) {
            c.total_ticks = c.total_ticks.saturating_add(i64::from(ticks)).max(0);
        }
    }

    /// `moveToTimeMarker`.
    pub fn move_to_marker(&mut self, clock: usize, marker: &str) -> MoveResult {
        let Some(c) = self.clocks.get_mut(clock) else {
            return MoveResult::NoTimeMarkerFound;
        };
        let Some(m) = c.marker(marker) else {
            return MoveResult::NoTimeMarkerFound;
        };
        if m.occurs_at(c.total_ticks) {
            return MoveResult::NotMoved;
        }
        c.total_ticks = m.resolve_time_to_move_to(c.total_ticks);
        c.partial_tick = 0.0;
        MoveResult::Moved
    }

    /// The network state of one clock (`packNetworkState`): standing still while paused or
    /// while `advance_time` is off.
    pub fn network_state(&self, clock: usize, advance_time: bool) -> Option<ClockUpdate> {
        let c = self.clocks.get(clock)?;
        let paused = c.paused || !advance_time;
        Some(ClockUpdate {
            clock: i32::try_from(clock).unwrap_or(0),
            total_ticks: c.total_ticks,
            partial_tick: c.partial_tick,
            rate: if paused { 0.0 } else { c.rate },
        })
    }

    /// `createFullSyncPacket`.
    pub fn full_sync(&self, game_time: i64, advance_time: bool) -> SetTime {
        SetTime {
            game_time,
            clocks: (0..self.clocks.len())
                .filter_map(|i| self.network_state(i, advance_time))
                .collect(),
        }
    }

    /// The `set_time` after one clock changed.
    pub fn update_packet(&self, clock: usize, game_time: i64, advance_time: bool) -> SetTime {
        SetTime {
            game_time,
            clocks: self
                .network_state(clock, advance_time)
                .into_iter()
                .collect(),
        }
    }

    /// Loads `world_clocks.dat`'s `data` (`PackedClockStates`: clock id → state).
    pub fn load(&mut self, data: &Compound) {
        for clock in &mut self.clocks {
            let Some(Tag::Compound(state)) = data.get(&clock.id) else {
                continue;
            };
            if let Some(t) = state.get("total_ticks").and_then(Tag::as_i64) {
                clock.total_ticks = t;
            }
            if let Some(Tag::Float(p)) = state.get("partial_tick") {
                clock.partial_tick = *p;
            }
            if let Some(Tag::Float(r)) = state.get("rate")
                && *r > 0.0
            {
                clock.rate = *r;
            }
            if let Some(p) = state.get("paused").and_then(Tag::as_i64) {
                clock.paused = p != 0;
            }
        }
    }

    /// The `data` compound of `world_clocks.dat`.
    pub fn save(&self) -> Compound {
        let mut out = Compound::with_capacity(self.clocks.len());
        for c in &self.clocks {
            let mut state = Compound::with_capacity(4);
            state.insert("total_ticks", Tag::Long(c.total_ticks));
            state.insert("partial_tick", Tag::Float(c.partial_tick));
            state.insert("rate", Tag::Float(c.rate));
            state.insert("paused", Tag::Byte(i8::from(c.paused)));
            out.insert(c.id.clone(), Tag::Compound(state));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day() -> Clocks {
        let mut clock = Clock::new("minecraft:overworld");
        for (id, ticks) in [("minecraft:day", 1000), ("minecraft:noon", 6000)] {
            clock.markers.push(TimeMarker {
                id: id.to_owned(),
                ticks,
                period: Some(24000),
                show_in_commands: true,
            });
        }
        Clocks {
            clocks: vec![clock],
            timelines: Vec::new(),
        }
    }

    #[test]
    fn markers_move_forward_to_the_next_occurrence() {
        let mut clocks = day();
        clocks.set_total_ticks(0, 30_000);
        assert_eq!(
            clocks.move_to_marker(0, "minecraft:noon"),
            MoveResult::NotMoved
        );
        assert_eq!(clocks.move_to_marker(0, "minecraft:day"), MoveResult::Moved);
        assert_eq!(clocks.clocks[0].total_ticks, 49_000);
        clocks.set_total_ticks(0, 30_001);
        clocks.move_to_marker(0, "minecraft:day");
        assert_eq!(clocks.clocks[0].total_ticks, 49_000);
        assert_eq!(
            clocks.move_to_marker(0, "minecraft:day"),
            MoveResult::NotMoved
        );
        assert_eq!(
            clocks.move_to_marker(0, "minecraft:nope"),
            MoveResult::NoTimeMarkerFound
        );
    }

    #[test]
    fn rate_accumulates_partial_ticks() {
        let mut clocks = day();
        clocks.clocks[0].rate = 0.5;
        clocks.tick(true);
        assert_eq!(clocks.clocks[0].total_ticks, 0);
        clocks.tick(true);
        assert_eq!(clocks.clocks[0].total_ticks, 1);
        clocks.tick(false);
        assert_eq!(clocks.clocks[0].total_ticks, 1);
    }
}
