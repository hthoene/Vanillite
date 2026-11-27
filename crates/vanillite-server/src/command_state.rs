//! Server-wide state of commands kept in [`GameSystems`](crate::systems::GameSystems):
//! scheduled functions (saved as `minecraft:scheduled_events`), random sequences, stopwatches,
//! the profilers and waypoint icons.

use std::collections::BTreeMap;

use vanillite_nbt::{Compound, List, Tag};

/// What a scheduled event runs (`TimerCallback`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TimerCallback {
    /// `FunctionCallback`: a function by id.
    Function(String),
    /// `FunctionTagCallback`: the functions of a tag.
    FunctionTag(String),
}

/// One scheduled event (`TimerQueue.Event`).
#[derive(Clone, Debug)]
pub(crate) struct TimerEvent {
    /// Game time it is due.
    pub(crate) trigger_time: i64,
    /// The function id, or `#` and the tag id.
    pub(crate) id: String,
    pub(crate) callback: TimerCallback,
}

/// `TimerQueue`: events by trigger time, then scheduling order; an id has at most one event
/// per game time.
#[derive(Debug, Default)]
pub(crate) struct TimerQueue {
    queue: BTreeMap<(i64, u64), TimerEvent>,
    /// `events`: id → trigger time → sequence number.
    events: BTreeMap<String, BTreeMap<i64, u64>>,
    sequence: u64,
    /// Changed since the last save (`setDirty`).
    dirty: bool,
}

impl TimerQueue {
    /// `schedule`: ignored when the id already has an event at this time.
    pub(crate) fn schedule(&mut self, id: &str, time: i64, callback: TimerCallback) {
        let row = self.events.entry(id.to_owned()).or_default();
        if row.contains_key(&time) {
            return;
        }
        self.sequence = self.sequence.wrapping_add(1);
        self.dirty = true;
        row.insert(time, self.sequence);
        self.queue.insert(
            (time, self.sequence),
            TimerEvent {
                trigger_time: time,
                id: id.to_owned(),
                callback,
            },
        );
    }

    /// `remove`: drops all events of an id; the number dropped.
    pub(crate) fn remove(&mut self, id: &str) -> usize {
        self.dirty = true;
        let Some(row) = self.events.remove(id) else {
            return 0;
        };
        for (time, sequence) in &row {
            self.queue.remove(&(*time, *sequence));
        }
        row.len()
    }

    /// `getEventsIds`.
    pub(crate) fn ids(&self) -> impl Iterator<Item = &str> {
        self.events
            .iter()
            .filter(|(_, row)| !row.is_empty())
            .map(|(id, _)| id.as_str())
    }

    /// One step of `tick`: the next event due at `current`, taken out of the queue. Like
    /// Vanilla, the id's entry at `current` (not at the trigger time) is forgotten.
    pub(crate) fn next_due(&mut self, current: i64) -> Option<TimerEvent> {
        let (&key, event) = self.queue.first_key_value()?;
        if event.trigger_time > current {
            return None;
        }
        let event = self.queue.remove(&key)?;
        self.dirty = true;
        if let Some(row) = self.events.get_mut(&event.id) {
            row.remove(&current);
            if row.is_empty() {
                self.events.remove(&event.id);
            }
        }
        Some(event)
    }

    /// Whether it changed since the last call; clears the flag.
    pub(crate) fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// `TimerQueue.Packed.CODEC`: the events in queue order with their callbacks
    /// (`TimerCallbacks.SERVER_CALLBACKS`).
    pub(crate) fn to_nbt(&self) -> Compound {
        let events = self
            .queue
            .values()
            .map(|e| {
                let (kind, id) = match &e.callback {
                    TimerCallback::Function(id) => ("minecraft:function", id),
                    TimerCallback::FunctionTag(id) => ("minecraft:function_tag", id),
                };
                let callback = Compound::new()
                    .with("type", kind)
                    .with("id", id.as_str())
                    .in_java_hash_order();
                Tag::Compound(
                    Compound::new()
                        .with("trigger_time", e.trigger_time)
                        .with("id", e.id.as_str())
                        .with("callback", callback)
                        .in_java_hash_order(),
                )
            })
            .collect();
        Compound::new().with("events", List(events))
    }

    /// The queue of saved events, scheduled again in their order (`new TimerQueue(packed)`).
    pub(crate) fn from_nbt(data: &Compound) -> TimerQueue {
        let mut queue = TimerQueue::default();
        let events = match data.get("events") {
            Some(Tag::List(List(events))) => events.as_slice(),
            _ => &[],
        };
        for event in events.iter().filter_map(Tag::as_compound) {
            let str_of =
                |c: &Compound, key: &str| c.get(key).and_then(Tag::as_str).map(str::to_owned);
            let callback = event.get("callback").and_then(Tag::as_compound);
            let (Some(time), Some(id), Some(kind), Some(target)) = (
                event.get("trigger_time").and_then(Tag::as_i64),
                str_of(event, "id"),
                callback.and_then(|c| str_of(c, "type")),
                callback.and_then(|c| str_of(c, "id")),
            ) else {
                log::error!("Failed to parse scheduled event {event:?}");
                continue;
            };
            let callback = match kind.strip_prefix("minecraft:").unwrap_or(&kind) {
                "function" => TimerCallback::Function(target),
                "function_tag" => TimerCallback::FunctionTag(target),
                _ => {
                    log::error!("Unknown scheduled event callback {kind}");
                    continue;
                }
            };
            queue.schedule(&id, time, callback);
        }
        queue.dirty = false;
        queue
    }
}

/// `Stopwatch`, in milliseconds of the stopwatch clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Stopwatch {
    /// `creationTime`.
    pub(crate) creation_time: i64,
    /// `accumulatedElapsedTime`.
    pub(crate) accumulated: i64,
}

impl Stopwatch {
    /// `elapsedMilliseconds`.
    pub(crate) fn elapsed_ms(self, now: i64) -> i64 {
        self.accumulated
            .saturating_add(now.saturating_sub(self.creation_time))
    }

    /// `elapsedSeconds`.
    #[allow(clippy::cast_precision_loss, clippy::as_conversions)]
    pub(crate) fn elapsed_seconds(self, now: i64) -> f64 {
        self.elapsed_ms(now) as f64 / 1000.0
    }
}

/// `Stopwatches`: by id, timed with a monotonic clock (`Util.getMillis`); saved as
/// `minecraft:stopwatches`.
#[derive(Debug)]
pub(crate) struct Stopwatches {
    start: std::time::Instant,
    stopwatches: BTreeMap<String, Stopwatch>,
    /// Changed since the last save (`setDirty`).
    dirty: bool,
}

impl Default for Stopwatches {
    fn default() -> Self {
        Stopwatches {
            start: std::time::Instant::now(),
            stopwatches: BTreeMap::new(),
            dirty: false,
        }
    }
}

impl Stopwatches {
    /// `CODEC`'s `unpack`: every stopwatch goes on from its saved elapsed milliseconds.
    pub(crate) fn from_nbt(data: &Compound) -> Stopwatches {
        let mut out = Stopwatches::default();
        let now = out.now();
        if let Some(Tag::Compound(saved)) = data.get("stopwatches") {
            for (id, tag) in saved.iter() {
                if let Some(ms) = tag.as_i64() {
                    out.stopwatches.insert(
                        id.to_owned(),
                        Stopwatch {
                            creation_time: now,
                            accumulated: ms,
                        },
                    );
                }
            }
        }
        out
    }

    /// `pack` when the data is to be saved: `isDirty` holds while there are stopwatches.
    pub(crate) fn take_dirty_nbt(&mut self) -> Option<Compound> {
        if !std::mem::take(&mut self.dirty) && self.stopwatches.is_empty() {
            return None;
        }
        let now = self.now();
        let mut saved = Compound::with_capacity(self.stopwatches.len());
        for (id, s) in &self.stopwatches {
            saved.insert(id.as_str(), Tag::Long(s.elapsed_ms(now)));
        }
        Some(Compound::new().with("stopwatches", saved))
    }

    /// `currentTime`.
    pub(crate) fn now(&self) -> i64 {
        i64::try_from(self.start.elapsed().as_millis()).unwrap_or(i64::MAX)
    }

    /// `get`.
    pub(crate) fn get(&self, id: &str) -> Option<Stopwatch> {
        self.stopwatches.get(id).copied()
    }

    /// `add` of a stopwatch started now; `false` if the id exists.
    pub(crate) fn add(&mut self, id: &str) -> bool {
        if self.stopwatches.contains_key(id) {
            return false;
        }
        let started = Stopwatch {
            creation_time: self.now(),
            accumulated: 0,
        };
        self.stopwatches.insert(id.to_owned(), started);
        self.dirty = true;
        true
    }

    /// `update` with a stopwatch started now; `false` if the id is unknown.
    pub(crate) fn restart(&mut self, id: &str) -> bool {
        let now = self.now();
        let Some(s) = self.stopwatches.get_mut(id) else {
            return false;
        };
        *s = Stopwatch {
            creation_time: now,
            accumulated: 0,
        };
        self.dirty = true;
        true
    }

    /// `remove`.
    pub(crate) fn remove(&mut self, id: &str) -> bool {
        let removed = self.stopwatches.remove(id).is_some();
        self.dirty |= removed;
        removed
    }

    /// `ids`.
    pub(crate) fn ids(&self) -> impl Iterator<Item = &str> {
        self.stopwatches.keys().map(String::as_str)
    }
}

/// A running measurement of `debug start` or `perf start`: when it started, in
/// server clock nanoseconds and game time.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Measurement {
    pub(crate) nanos: u64,
    pub(crate) game_time: i64,
}

/// The command profilers. There is no tick profiler or metrics recorder: the commands measure
/// the duration and the ticks between start and stop.
#[derive(Debug, Default)]
pub(crate) struct Profiling {
    /// `debug start` (`isTimeProfilerRunning`).
    pub(crate) debug: Option<Measurement>,
    /// `perf start` (`isRecordingMetrics`).
    pub(crate) perf: Option<Measurement>,
}

/// `Waypoint.Icon` as changed by `/waypoint`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct WaypointIcon {
    /// The style asset id; `None` is `minecraft:default`.
    pub(crate) style: Option<String>,
    /// RGB colour.
    pub(crate) color: Option<i32>,
}

impl WaypointIcon {
    /// `Waypoint.Icon.CODEC` (`locator_bar_icon`); `None` without data (`hasData`).
    pub(crate) fn to_nbt(&self) -> Option<Compound> {
        if self.style.is_none() && self.color.is_none() {
            return None;
        }
        let mut out = Compound::with_capacity(2);
        out.insert(
            "style",
            Tag::String(
                self.style
                    .clone()
                    .unwrap_or_else(|| "minecraft:default".to_owned()),
            ),
        );
        if let Some(color) = self.color {
            out.insert("color", Tag::Int(color));
        }
        Some(out)
    }

    /// Reads `locator_bar_icon`; `None` without data.
    pub(crate) fn from_nbt(data: &Compound) -> Option<WaypointIcon> {
        let style = data
            .get("style")
            .and_then(Tag::as_str)
            .filter(|s| *s != "minecraft:default")
            .map(str::to_owned);
        let color = match data.get("color") {
            Some(Tag::Int(c)) => Some(*c),
            _ => None,
        };
        let icon = WaypointIcon { style, color };
        (icon.style.is_some() || icon.color.is_some()).then_some(icon)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_queue_orders_and_removes() {
        let mut q = TimerQueue::default();
        q.schedule("a:f", 10, TimerCallback::Function("a:f".into()));
        q.schedule("a:f", 10, TimerCallback::Function("a:f".into()));
        q.schedule("#a:t", 5, TimerCallback::FunctionTag("a:t".into()));
        q.schedule("a:f", 7, TimerCallback::Function("a:f".into()));
        assert_eq!(q.ids().count(), 2);
        assert_eq!(q.next_due(4).map(|e| e.id), None);
        assert_eq!(q.next_due(7).map(|e| e.id), Some("#a:t".to_owned()));
        assert_eq!(q.next_due(7).map(|e| e.trigger_time), Some(7));
        assert_eq!(q.next_due(7).map(|e| e.id), None);
        assert_eq!(q.remove("a:f"), 1);
        // The late tag event stays listed under its trigger time, as in Vanilla.
        assert_eq!(q.ids().collect::<Vec<_>>(), ["#a:t"]);
        q.schedule("b:g", 3, TimerCallback::Function("b:g".into()));
        let saved = q.to_nbt();
        let mut loaded = TimerQueue::from_nbt(&saved);
        assert_eq!(loaded.to_nbt(), saved);
        assert_eq!(
            loaded.next_due(3).map(|e| e.callback),
            Some(TimerCallback::Function("b:g".into()))
        );
    }

    #[test]
    fn stopwatches_round_trip() {
        let mut s = Stopwatches::default();
        assert_eq!(s.take_dirty_nbt(), None);
        assert!(s.add("test:a"));
        let saved = s.take_dirty_nbt().unwrap();
        let ms = match &saved.get("stopwatches") {
            Some(Tag::Compound(c)) => c.get("test:a").and_then(Tag::as_i64).unwrap(),
            _ => panic!("no stopwatches"),
        };
        let loaded = Stopwatches::from_nbt(&saved);
        assert!(loaded.get("test:a").unwrap().elapsed_ms(loaded.now()) >= ms);
        assert!(s.remove("test:a"));
        assert_eq!(
            s.take_dirty_nbt(),
            Some(Compound::new().with("stopwatches", Compound::new()))
        );
        assert_eq!(s.take_dirty_nbt(), None);
    }
}
