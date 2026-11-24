//! Brain mechanics on a toy context: memory expiry, activity selection and erasure, timed and
//! one shot behaviors, `RunOne`.

use super::*;

/// A context with a clock and a fixed random.
struct Cx {
    time: i64,
    /// Calls of each toy behavior's start.
    started: Vec<&'static str>,
}

impl BrainContext for Cx {
    fn game_time(&self) -> i64 {
        self.time
    }
    fn level_random_int(&mut self, bound: i32) -> i32 {
        bound - 1
    }
    fn shuffle_random(&mut self) -> f32 {
        0.5
    }
}

#[derive(Debug, Clone)]
enum Toy {
    /// A timed behavior of 3 ticks needing `is_sniffing`.
    Timed,
    /// A one shot that sets `roar_sound_delay` when `attack_target` is absent.
    Shot,
    /// Gate children.
    A,
    B,
}

impl BehaviorInfo for Toy {
    fn conditions(&self) -> &'static [(MemoryType, MemoryStatus)] {
        match self {
            Toy::Timed => &[(MemoryType::IsSniffing, MemoryStatus::Present)],
            Toy::Shot => &[
                (MemoryType::AttackTarget, MemoryStatus::Absent),
                (MemoryType::RoarSoundDelay, MemoryStatus::Registered),
            ],
            Toy::A | Toy::B => &[],
        }
    }
    fn flavor(&self) -> Flavor {
        match self {
            Toy::Shot => Flavor::OneShot,
            _ => Flavor::Timed { min: 3, max: 3 },
        }
    }
}

impl Behavior<Cx> for Toy {
    fn start(&mut self, mem: &mut Memories, cx: &mut Cx, _: i64) -> bool {
        cx.started.push(match self {
            Toy::Timed => "timed",
            Toy::Shot => "shot",
            Toy::A => "a",
            Toy::B => "b",
        });
        if let Toy::Shot = self {
            mem.set(MemoryType::RoarSoundDelay, MemoryValue::Unit);
        }
        true
    }
    fn can_still_use(&mut self, _: &mut Memories, _: &mut Cx, _: i64) -> bool {
        true
    }
}

#[derive(Debug)]
struct NoSensor;

impl SensorInfo for NoSensor {
    fn requires(&self) -> &'static [MemoryType] {
        &[]
    }
}

impl Sensor<Cx> for NoSensor {
    fn sense(&mut self, _: &mut Memories, _: &mut Cx) {}
}

fn brain() -> Brain<Toy, NoSensor> {
    let core = ActivityData::create(Activity::Core, 0, vec![Node::leaf(Toy::Shot)], &[]);
    let idle = ActivityData::create(
        Activity::Idle,
        10,
        vec![Node::Gate(Box::new(Gate::run_one(
            &[],
            vec![(Node::leaf(Toy::A), 1), (Node::leaf(Toy::B), 1)],
        )))],
        &[],
    );
    let sniff = ActivityData::create(
        Activity::Sniff,
        5,
        vec![Node::leaf(Toy::Timed)],
        &[(MemoryType::IsSniffing, MemoryStatus::Present)],
    )
    .erase(&[MemoryType::IsSniffing]);
    Brain::build(
        &[MemoryType::AttackTarget],
        vec![NoSensor],
        vec![core, idle, sniff],
        |b| b - 1,
    )
}

#[test]
fn memories_expire_after_their_time_to_live() {
    let mut mem = Memories::default();
    mem.register(MemoryType::SniffCooldown);
    // Unregistered memories take no values.
    mem.set(MemoryType::DigCooldown, MemoryValue::Unit);
    assert!(!mem.check(MemoryType::DigCooldown, MemoryStatus::Registered));
    mem.set_with_expiry(MemoryType::SniffCooldown, MemoryValue::Unit, 2);
    mem.tick();
    mem.tick();
    assert!(mem.has(MemoryType::SniffCooldown));
    mem.tick();
    assert!(!mem.has(MemoryType::SniffCooldown));
    assert!(mem.check(MemoryType::SniffCooldown, MemoryStatus::Absent));
}

#[test]
fn empty_entity_lists_clear_the_memory_and_nbt_keeps_codec_memories() {
    let mut mem = Memories::default();
    mem.register(MemoryType::NearestPlayers);
    mem.register(MemoryType::DigCooldown);
    mem.set_entities_with(MemoryType::NearestPlayers, |_| {});
    assert!(!mem.has(MemoryType::NearestPlayers));
    mem.set_with_expiry(MemoryType::DigCooldown, MemoryValue::Unit, 1200);
    let saved = mem.save();
    let mut back = Memories::default();
    back.register(MemoryType::DigCooldown);
    back.load(&saved);
    assert!(back.has(MemoryType::DigCooldown));
    assert_eq!(back.time_until_expiry(MemoryType::DigCooldown), Some(1200));
}

#[test]
fn activities_switch_by_requirements_and_erase_on_stop() {
    let mut b = brain();
    assert!(b.is_active(Activity::Core) && b.is_active(Activity::Idle));
    b.memories.set(MemoryType::IsSniffing, MemoryValue::Unit);
    b.set_active_activity_to_first_valid(&[Activity::Sniff, Activity::Idle]);
    assert!(b.is_active(Activity::Sniff) && !b.is_active(Activity::Idle));
    assert_eq!(b.active_non_core(), Some(Activity::Sniff));
    // Leaving sniff for idle erases `is_sniffing`.
    b.set_active_activity_if_possible(Activity::Idle);
    assert!(b.is_active(Activity::Idle));
    assert!(!b.memories.has(MemoryType::IsSniffing));
}

#[test]
fn timed_behaviors_run_for_their_duration_and_one_shots_once() {
    let mut b = brain();
    let mut cx = Cx {
        time: 100,
        started: Vec::new(),
    };
    b.memories.set(MemoryType::IsSniffing, MemoryValue::Unit);
    b.set_active_activity_to_first_valid(&[Activity::Sniff]);
    b.tick(&mut cx);
    // The one shot ran and is stopped again; the timed behavior runs.
    assert_eq!(cx.started, vec!["shot", "timed"]);
    assert!(b.memories.has(MemoryType::RoarSoundDelay));
    assert_eq!(b.running().count(), 1);
    // Duration 3: running at 101..=103, stopped at 104.
    for time in 101..=103 {
        cx.time = time;
        b.tick(&mut cx);
        assert_eq!(b.running().count(), 1, "time {time}");
    }
    cx.time = 104;
    b.tick(&mut cx);
    assert_eq!(b.running().count(), 0);
}

#[test]
fn run_one_starts_a_single_child() {
    let mut b = brain();
    let mut cx = Cx {
        time: 0,
        started: Vec::new(),
    };
    b.memories.set(MemoryType::AttackTarget, MemoryValue::Unit);
    b.tick(&mut cx);
    let children = cx
        .started
        .iter()
        .filter(|s| **s == "a" || **s == "b")
        .count();
    assert_eq!(children, 1);
    // The gate keeps running while its child runs, without starting the other one.
    cx.time = 1;
    b.tick(&mut cx);
    let children = cx
        .started
        .iter()
        .filter(|s| **s == "a" || **s == "b")
        .count();
    assert_eq!(children, 1);
}
