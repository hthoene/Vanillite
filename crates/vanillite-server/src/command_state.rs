//! Server-wide state of commands kept in [`GameSystems`](crate::systems::GameSystems):
//! scheduled functions (saved as `minecraft:scheduled_events`), random sequences, stopwatches,
//! the profilers and waypoint icons.

use std::collections::BTreeMap;

use vanillite_base::random::{Seed128, XoroshiroRandom};
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

/// `RandomSequences`: named random generators and the defaults of new ones.
#[derive(Debug)]
pub(crate) struct RandomSequences {
    salt: i32,
    include_world_seed: bool,
    include_sequence_id: bool,
    sequences: BTreeMap<String, XoroshiroRandom>,
}

impl Default for RandomSequences {
    fn default() -> Self {
        RandomSequences {
            salt: 0,
            include_world_seed: true,
            include_sequence_id: true,
            sequences: BTreeMap::new(),
        }
    }
}

/// The seed options of a random sequence: salt, include the world seed, include the id.
pub(crate) type SeedOptions = (i32, bool, bool);

impl RandomSequences {
    /// `RandomSequences.createSequence` with `RandomSequence.createSequence`.
    fn create(id: &str, world_seed: i64, (salt, world, with_id): SeedOptions) -> XoroshiroRandom {
        let seed = (if world { world_seed } else { 0 }) ^ i64::from(salt);
        let mut seed128 = Seed128::upgrade_unmixed(seed);
        if with_id {
            let key = Seed128::from_hash_of(id);
            seed128 = seed128.xor(key.lo, key.hi);
        }
        XoroshiroRandom::from_seed128(seed128.mixed())
    }

    fn defaults(&self) -> SeedOptions {
        (self.salt, self.include_world_seed, self.include_sequence_id)
    }

    /// `get`: the sequence, created with the defaults when missing.
    pub(crate) fn get(&mut self, id: &str, world_seed: i64) -> &mut XoroshiroRandom {
        let defaults = self.defaults();
        self.sequences
            .entry(id.to_owned())
            .or_insert_with(|| Self::create(id, world_seed, defaults))
    }

    /// `reset`: a new sequence with the defaults or the given options.
    pub(crate) fn reset(&mut self, id: &str, world_seed: i64, options: Option<SeedOptions>) {
        let options = options.unwrap_or_else(|| self.defaults());
        self.sequences
            .insert(id.to_owned(), Self::create(id, world_seed, options));
    }

    /// `setSeedDefaults`.
    pub(crate) fn set_defaults(&mut self, (salt, world, with_id): SeedOptions) {
        self.salt = salt;
        self.include_world_seed = world;
        self.include_sequence_id = with_id;
    }

    /// `clear`: the number of sequences dropped.
    pub(crate) fn clear(&mut self) -> usize {
        let count = self.sequences.len();
        self.sequences.clear();
        count
    }

    /// The ids of the sequences (`forAllSequences`).
    pub(crate) fn ids(&self) -> impl Iterator<Item = &str> {
        self.sequences.keys().map(String::as_str)
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

/// `Stopwatches`: by id, timed with a monotonic clock (`Util.getMillis`).
#[derive(Debug)]
pub(crate) struct Stopwatches {
    start: std::time::Instant,
    stopwatches: BTreeMap<String, Stopwatch>,
}

impl Default for Stopwatches {
    fn default() -> Self {
        Stopwatches {
            start: std::time::Instant::now(),
            stopwatches: BTreeMap::new(),
        }
    }
}

impl Stopwatches {
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
        true
    }

    /// `remove`.
    pub(crate) fn remove(&mut self, id: &str) -> bool {
        self.stopwatches.remove(id).is_some()
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
    fn random_sequences_are_stable() {
        use vanillite_base::random::RandomSource;
        let mut a = RandomSequences::default();
        let mut b = RandomSequences::default();
        let x = a.get("minecraft:test", 42).next_int();
        let y = b.get("minecraft:test", 42).next_int();
        assert_eq!(x, y);
        b.reset("minecraft:test", 42, None);
        assert_eq!(b.get("minecraft:test", 42).next_int(), x);
        assert_eq!(a.clear(), 1);
    }
}
