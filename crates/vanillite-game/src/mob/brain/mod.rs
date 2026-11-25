//! Brain AI (`Brain`): memories, sensors, activities, schedules and behaviours of the mobs
//! that use it in Vanilla instead of goals.
//!
//! A family builds the brain in its `init` hook with a [`BrainSpec`] (sensors, extra
//! memories, activities with their behaviours, requirements and memories to erase) and ticks
//! it in its `custom_server_ai_step` like Vanilla's `customServerAiStep` (`brain.tick` and the
//! kind's `updateActivity`). Behaviours are small `Copy` values in an arena; running them
//! copies the value out, so they have full access to the world through the [`Tick`].
//!
//! Memories only exist for the types the brain registered; their slots are inline in a small
//! vector with an index table, so a lookup is one array read.

pub mod behavior;
pub mod memory;
pub mod sensing;
pub mod sensor;

use vanillite_base::BlockPos;
use vanillite_base::random::RandomSource;
use vanillite_world::entity::EntityId;

pub use behavior::{Behavior, Spec, Status};
pub use memory::{
    GlobalPos, MEMORY_COUNT, MemValue, MemoryStatus, MemoryType, Slot, Tracker, ValueKind, Visible,
    WalkTarget,
};
pub use sensor::{SensorKind, SensorState};

use super::Tick;

/// `Activity`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Activity {
    Core,
    Idle,
    Work,
    Play,
    Rest,
    Meet,
    Panic,
    Raid,
    PreRaid,
    Hide,
    Fight,
    Celebrate,
    AdmireItem,
    Avoid,
    Ride,
    PlayDead,
    LongJump,
    Ram,
    Tongue,
    Swim,
    LaySpawn,
    Sniff,
    Investigate,
    Roar,
    Emerge,
    Dig,
}

impl Activity {
    fn bit(self) -> u32 {
        1 << (self as u32)
    }
}

/// A schedule: the activity of each part of the day (`Timeline` keyframes of the
/// `villager_activity` attribute).
#[derive(Clone, Debug, PartialEq)]
pub struct Schedule {
    /// `(day time, activity)` sorted by time.
    pub keyframes: Vec<(i32, Activity)>,
}

impl Schedule {
    /// The adult villager schedule.
    pub fn villager() -> Schedule {
        Schedule {
            keyframes: vec![
                (10, Activity::Idle),
                (2000, Activity::Work),
                (9000, Activity::Meet),
                (11000, Activity::Idle),
                (12000, Activity::Rest),
            ],
        }
    }

    /// The baby villager schedule.
    pub fn baby_villager() -> Schedule {
        Schedule {
            keyframes: vec![
                (10, Activity::Idle),
                (3000, Activity::Play),
                (6000, Activity::Idle),
                (10000, Activity::Play),
                (12000, Activity::Rest),
            ],
        }
    }

    /// The activity at a day time: the last keyframe at or before it (the last of the day
    /// before the first).
    pub fn activity_at(&self, day_time: i64) -> Activity {
        let t = day_time.rem_euclid(24000);
        let mut current = self.keyframes.last().map_or(Activity::Idle, |k| k.1);
        for &(at, a) in &self.keyframes {
            if i64::from(at) <= t {
                current = a;
            }
        }
        current
    }
}

/// One behaviour in the brain's arena.
#[derive(Clone, Debug)]
pub struct Node {
    pub behavior: Behavior,
    pub status: Status,
    /// `endTimestamp` of timed behaviours.
    pub end: i64,
    pub min_duration: i32,
    pub max_duration: i32,
    /// Children of gates and sequences: node index, weight and the shuffle key.
    pub children: Vec<Child>,
}

/// A child of a gate (`ShufflingList.WeightedEntry`).
#[derive(Clone, Copy, Debug)]
pub struct Child {
    pub node: u16,
    pub weight: i32,
    pub rand_weight: f64,
}

/// An activity's requirements and the memories erased when it stops.
#[derive(Clone, Debug)]
struct ActivityData {
    activity: Activity,
    requirements: Vec<(MemoryType, MemoryStatus)>,
    erase_when_stopped: Vec<MemoryType>,
}

/// `Brain`.
#[derive(Clone, Debug)]
pub struct Brain {
    /// Memory type → slot index (`u8::MAX`: not registered).
    index: [u8; MEMORY_COUNT],
    pub slots: Vec<Slot>,
    pub sensors: Vec<SensorState>,
    pub nodes: Vec<Node>,
    /// `availableBehaviorsByPriority`: `(priority, activity, node)` sorted by priority.
    by_priority: Vec<(i32, Activity, u16)>,
    activities: Vec<ActivityData>,
    core: u32,
    active: u32,
    default_activity: Activity,
    pub schedule: Option<Schedule>,
    last_schedule_update: i64,
    running: Vec<u16>,
    /// Whether the behaviour being stopped timed out (`Behavior.timedOut` in `stop`).
    pub stop_timed_out: bool,
    /// The random of the shuffled gates (`ShufflingList.random`), created on first use.
    pub shuffle_random: Option<vanillite_base::random::LegacyRandom>,
}

impl Default for Brain {
    fn default() -> Self {
        Brain {
            index: [u8::MAX; MEMORY_COUNT],
            slots: Vec::new(),
            sensors: Vec::new(),
            nodes: Vec::new(),
            by_priority: Vec::new(),
            activities: Vec::new(),
            core: Activity::Core.bit(),
            active: 0,
            default_activity: Activity::Idle,
            schedule: None,
            last_schedule_update: -9999,
            running: Vec::new(),
            stop_timed_out: false,
            shuffle_random: None,
        }
    }
}

/// What a family declares for its brain (`Brain.Provider` and the `XAi.getActivities`).
#[derive(Debug, Default)]
pub struct BrainSpec {
    pub memories: Vec<MemoryType>,
    pub sensors: Vec<SensorKind>,
    /// `(activity, first priority, behaviours, requirements, memories erased when stopped)`.
    pub activities: Vec<ActivitySpec>,
    pub core: Vec<Activity>,
    pub default_activity: Option<Activity>,
    pub schedule: Option<Schedule>,
}

/// One activity of a [`BrainSpec`] (`ActivityData`).
#[derive(Debug)]
pub struct ActivitySpec {
    pub activity: Activity,
    /// Behaviours with their priorities.
    pub behaviors: Vec<(i32, Spec)>,
    pub requirements: Vec<(MemoryType, MemoryStatus)>,
    pub erase_when_stopped: Vec<MemoryType>,
}

impl ActivitySpec {
    /// `ActivityData.create(activity, firstPriority, behaviours)`.
    pub fn new(activity: Activity, first_priority: i32, behaviors: Vec<Spec>) -> ActivitySpec {
        ActivitySpec {
            activity,
            behaviors: behaviors
                .into_iter()
                .zip(first_priority..)
                .map(|(b, p)| (p, b))
                .collect(),
            requirements: Vec::new(),
            erase_when_stopped: Vec::new(),
        }
    }

    /// `ActivityData.create(activity, firstPriority, behaviours, memory)`: the memory must be
    /// present and is erased when the activity stops.
    pub fn with_memory(
        activity: Activity,
        first_priority: i32,
        behaviors: Vec<Spec>,
        memory: MemoryType,
    ) -> ActivitySpec {
        let mut a = ActivitySpec::new(activity, first_priority, behaviors);
        a.requirements.push((memory, MemoryStatus::ValuePresent));
        a.erase_when_stopped.push(memory);
        a
    }

    /// `ActivityData.create(activity, pairs)` with explicit priorities.
    pub fn with_priorities(activity: Activity, behaviors: Vec<(i32, Spec)>) -> ActivitySpec {
        ActivitySpec {
            activity,
            behaviors,
            requirements: Vec::new(),
            erase_when_stopped: Vec::new(),
        }
    }

    /// Adds requirements (`conditions`).
    pub fn requires(mut self, requirements: &[(MemoryType, MemoryStatus)]) -> ActivitySpec {
        self.requirements.extend_from_slice(requirements);
        self
    }

    /// Adds memories to erase when the activity stops.
    pub fn erases(mut self, memories: &[MemoryType]) -> ActivitySpec {
        self.erase_when_stopped.extend_from_slice(memories);
        self
    }
}

impl Brain {
    /// Builds a brain (`Brain(memoryTypes, sensorTypes, activities, memories, random)`): the
    /// sensors start at a random delay from the mob's random.
    pub fn new(spec: BrainSpec, random: &mut impl RandomSource) -> Brain {
        let mut b = Brain::default();
        for &m in &spec.memories {
            b.register(m);
        }
        for kind in spec.sensors {
            let mut s = SensorState::new(kind);
            s.time_to_tick = i64::from(random.next_int_bounded(s.scan_rate.max(1)));
            for &m in sensor::requires(&s.kind) {
                b.register(m);
            }
            b.sensors.push(s);
        }
        for a in spec.activities {
            let mut data = ActivityData {
                activity: a.activity,
                requirements: a.requirements,
                erase_when_stopped: a.erase_when_stopped,
            };
            for (priority, s) in a.behaviors {
                let node = b.add_spec(s);
                b.by_priority.push((priority, a.activity, node));
            }
            // Only the first data of an activity counts (`activityRequirements.put`).
            if let Some(old) = b.activities.iter_mut().find(|d| d.activity == a.activity) {
                std::mem::swap(old, &mut data);
            } else {
                b.activities.push(data);
            }
        }
        b.by_priority.sort_by_key(|e| e.0);
        let all: Vec<MemoryType> = b
            .nodes
            .iter()
            .flat_map(|n| behavior::requirements(&n.behavior).iter().map(|r| r.0))
            .collect();
        for m in all {
            b.register(m);
        }
        if !spec.core.is_empty() {
            b.core = spec.core.iter().fold(0, |acc, a| acc | a.bit());
        }
        if let Some(a) = spec.default_activity {
            b.default_activity = a;
        }
        b.schedule = spec.schedule;
        b.use_default_activity();
        b
    }

    /// Adds a behaviour spec to the arena; returns its node.
    fn add_spec(&mut self, s: Spec) -> u16 {
        let (behavior, min, max, children) = s.into_parts();
        let idx = u16::try_from(self.nodes.len()).unwrap_or(u16::MAX);
        self.nodes.push(Node {
            behavior,
            status: Status::Stopped,
            end: 0,
            min_duration: min,
            max_duration: max,
            children: Vec::new(),
        });
        let mut list = Vec::with_capacity(children.len());
        for (child, weight) in children {
            let node = self.add_spec(child);
            list.push(Child {
                node,
                weight,
                rand_weight: 0.0,
            });
        }
        if let Some(n) = self.nodes.get_mut(usize::from(idx)) {
            n.children = list;
        }
        idx
    }

    /// `registerMemory`.
    pub fn register(&mut self, m: MemoryType) {
        let i = m as usize;
        if self.index[i] != u8::MAX {
            return;
        }
        self.index[i] = u8::try_from(self.slots.len()).unwrap_or(u8::MAX);
        self.slots.push(Slot {
            memory: m,
            value: None,
            ttl: Slot::NEVER,
        });
    }

    fn slot(&self, m: MemoryType) -> Option<&Slot> {
        self.slots.get(usize::from(self.index[m as usize]))
    }

    fn slot_mut(&mut self, m: MemoryType) -> Option<&mut Slot> {
        self.slots.get_mut(usize::from(self.index[m as usize]))
    }

    /// Whether the memory type is registered.
    pub fn is_registered(&self, m: MemoryType) -> bool {
        self.index[m as usize] != u8::MAX
    }

    /// `hasMemoryValue`.
    pub fn has(&self, m: MemoryType) -> bool {
        self.slot(m).is_some_and(|s| s.value.is_some())
    }

    /// `checkMemory`.
    pub fn check(&self, m: MemoryType, status: MemoryStatus) -> bool {
        let Some(s) = self.slot(m) else {
            return false;
        };
        match status {
            MemoryStatus::Registered => true,
            MemoryStatus::ValuePresent => s.value.is_some(),
            MemoryStatus::ValueAbsent => s.value.is_none(),
        }
    }

    /// Whether all requirements hold (`hasRequiredMemories`).
    pub fn check_all(&self, requirements: &[(MemoryType, MemoryStatus)]) -> bool {
        requirements.iter().all(|&(m, s)| self.check(m, s))
    }

    /// `eraseMemory`.
    pub fn erase(&mut self, m: MemoryType) {
        if let Some(s) = self.slot_mut(m) {
            s.clear();
        }
    }

    /// `setMemory(type, value)`; empty lists erase it.
    pub fn set(&mut self, m: MemoryType, v: MemValue) {
        self.set_with_expiry(m, v, Slot::NEVER);
    }

    /// `setMemory(type, Optional)`.
    pub fn set_opt(&mut self, m: MemoryType, v: Option<MemValue>) {
        match v {
            Some(v) => self.set(m, v),
            None => self.erase(m),
        }
    }

    /// `setMemoryWithExpiry`.
    pub fn set_with_expiry(&mut self, m: MemoryType, v: MemValue, ttl: i64) {
        if let Some(s) = self.slot_mut(m) {
            if v.is_empty_collection() {
                s.clear();
            } else {
                s.value = Some(v);
                s.ttl = ttl;
            }
        }
    }

    /// `getMemory`.
    pub fn get(&self, m: MemoryType) -> Option<&MemValue> {
        self.slot(m).and_then(|s| s.value.as_ref())
    }

    /// The memory's value, mutably.
    pub fn get_mut(&mut self, m: MemoryType) -> Option<&mut MemValue> {
        self.slot_mut(m).and_then(|s| s.value.as_mut())
    }

    /// `getTimeUntilExpiry`.
    pub fn ttl(&self, m: MemoryType) -> i64 {
        self.slot(m).map_or(Slot::NEVER, |s| s.ttl)
    }

    /// An entity memory.
    pub fn entity(&self, m: MemoryType) -> Option<EntityId> {
        match self.get(m)? {
            MemValue::Entity(e) => Some(*e),
            _ => None,
        }
    }

    /// An entity list memory (empty when absent).
    pub fn entities(&self, m: MemoryType) -> &[EntityId] {
        match self.get(m) {
            Some(MemValue::Entities(v)) => v,
            Some(MemValue::Visible(v)) => &v.entities,
            _ => &[],
        }
    }

    /// A block position memory.
    pub fn pos(&self, m: MemoryType) -> Option<BlockPos> {
        match self.get(m)? {
            MemValue::Pos(p) => Some(*p),
            MemValue::GlobalPos(g) => Some(g.pos),
            _ => None,
        }
    }

    /// An integer memory.
    pub fn int(&self, m: MemoryType) -> Option<i32> {
        match self.get(m)? {
            MemValue::Int(v) => Some(*v),
            _ => None,
        }
    }

    /// A long memory.
    pub fn long(&self, m: MemoryType) -> Option<i64> {
        match self.get(m)? {
            MemValue::Long(v) => Some(*v),
            _ => None,
        }
    }

    /// A boolean memory.
    pub fn boolean(&self, m: MemoryType) -> Option<bool> {
        match self.get(m)? {
            MemValue::Bool(v) => Some(*v),
            _ => None,
        }
    }

    /// A UUID memory.
    pub fn uuid(&self, m: MemoryType) -> Option<u128> {
        match self.get(m)? {
            MemValue::Uuid(v) => Some(*v),
            _ => None,
        }
    }

    /// A vector memory.
    pub fn vec3(&self, m: MemoryType) -> Option<vanillite_base::Vec3> {
        match self.get(m)? {
            MemValue::Vec3(v) => Some(*v),
            _ => None,
        }
    }

    /// `WALK_TARGET`.
    pub fn walk_target(&self) -> Option<WalkTarget> {
        match self.get(MemoryType::WalkTarget)? {
            MemValue::Walk(w) => Some(*w),
            _ => None,
        }
    }

    /// `LOOK_TARGET`.
    pub fn look_target(&self) -> Option<Tracker> {
        match self.get(MemoryType::LookTarget)? {
            MemValue::Look(l) => Some(*l),
            _ => None,
        }
    }

    /// `isMemoryValue(type, entity)`.
    pub fn is_entity(&self, m: MemoryType, id: EntityId) -> bool {
        self.entity(m) == Some(id)
    }

    /// Sets an entity list memory, reusing the stored vector.
    pub fn set_entities(&mut self, m: MemoryType, ids: impl Iterator<Item = EntityId>) {
        let Some(s) = self.slot_mut(m) else {
            return;
        };
        let mut v = match s.value.take() {
            Some(MemValue::Entities(v)) => v,
            _ => Vec::new(),
        };
        v.clear();
        v.extend(ids);
        if v.is_empty() {
            s.clear();
        } else {
            s.value = Some(MemValue::Entities(v));
            s.ttl = Slot::NEVER;
        }
    }

    /// `forgetOutdatedMemories`.
    pub fn forget_outdated(&mut self) {
        for s in &mut self.slots {
            s.tick();
        }
    }

    /// `clearMemories`.
    pub fn clear_memories(&mut self) {
        for s in &mut self.slots {
            s.clear();
        }
    }

    // --- Activities ---------------------------------------------------------------------

    /// `isActive`.
    pub fn is_active(&self, a: Activity) -> bool {
        self.active & a.bit() != 0
    }

    /// `getActiveNonCoreActivity`.
    pub fn active_non_core(&self) -> Option<Activity> {
        let rest = self.active & !self.core;
        (0..32u32).find(|i| rest & (1 << i) != 0).and_then(|i| {
            ACTIVITIES
                .get(usize::try_from(i).unwrap_or(usize::MAX))
                .copied()
        })
    }

    /// `setCoreActivities`.
    pub fn set_core_activities(&mut self, activities: &[Activity]) {
        self.core = activities.iter().fold(0, |acc, a| acc | a.bit());
    }

    /// `setDefaultActivity`.
    pub fn set_default_activity(&mut self, a: Activity) {
        self.default_activity = a;
    }

    /// `useDefaultActivity`.
    pub fn use_default_activity(&mut self) {
        let a = self.default_activity;
        self.set_active_activity(a);
    }

    fn requirements_met(&self, a: Activity) -> bool {
        match self.activities.iter().find(|d| d.activity == a) {
            Some(d) => self.check_all(&d.requirements),
            // `activityRequirements.get` of an unknown activity: no conditions to check.
            None => false,
        }
    }

    /// `setActiveActivityIfPossible`.
    pub fn set_active_activity_if_possible(&mut self, a: Activity) {
        if self.requirements_met(a) {
            self.set_active_activity(a);
        } else {
            self.use_default_activity();
        }
    }

    /// `setActiveActivityToFirstValid`.
    pub fn set_active_activity_to_first_valid(&mut self, list: &[Activity]) {
        for &a in list {
            if self.requirements_met(a) {
                self.set_active_activity(a);
                break;
            }
        }
    }

    fn set_active_activity(&mut self, a: Activity) {
        if self.is_active(a) {
            return;
        }
        // `eraseMemoriesForOtherActivitesThan`.
        let mut erase = Vec::new();
        for d in &self.activities {
            if d.activity != a && self.active & d.activity.bit() != 0 {
                erase.extend_from_slice(&d.erase_when_stopped);
            }
        }
        for m in erase {
            self.erase(m);
        }
        self.active = self.core | a.bit();
    }

    /// `updateActivityFromSchedule`: at most every 20 ticks.
    pub fn update_activity_from_schedule(&mut self, game_time: i64, day_time: i64) {
        if game_time - self.last_schedule_update > 20 {
            self.last_schedule_update = game_time;
            let scheduled = self
                .schedule
                .as_ref()
                .map_or(Activity::Idle, |s| s.activity_at(day_time));
            if !self.is_active(scheduled) {
                self.set_active_activity_if_possible(scheduled);
            }
        }
    }
}

/// All activities by discriminant.
pub const ACTIVITIES: [Activity; 26] = [
    Activity::Core,
    Activity::Idle,
    Activity::Work,
    Activity::Play,
    Activity::Rest,
    Activity::Meet,
    Activity::Panic,
    Activity::Raid,
    Activity::PreRaid,
    Activity::Hide,
    Activity::Fight,
    Activity::Celebrate,
    Activity::AdmireItem,
    Activity::Avoid,
    Activity::Ride,
    Activity::PlayDead,
    Activity::LongJump,
    Activity::Ram,
    Activity::Tongue,
    Activity::Swim,
    Activity::LaySpawn,
    Activity::Sniff,
    Activity::Investigate,
    Activity::Roar,
    Activity::Emerge,
    Activity::Dig,
];

/// The ticking mob's brain. Panics are impossible for brain mobs; callers use it only for
/// mobs whose family built one.
pub fn brain<'a>(t: &'a mut Tick<'_, '_>) -> &'a mut Brain {
    t.mob().brain.get_or_insert_with(Box::default)
}

/// The ticking mob's brain, read only (an empty brain for mobs without one).
pub fn brain_ref<'a>(t: &'a Tick<'_, '_>) -> &'a Brain {
    static EMPTY: std::sync::OnceLock<Brain> = std::sync::OnceLock::new();
    t.mob_ref()
        .brain
        .as_deref()
        .unwrap_or_else(|| EMPTY.get_or_init(Brain::default))
}

/// `Brain.tick`: forget outdated memories, tick the sensors, start the behaviours of the
/// active activities by priority, then tick the running ones.
pub fn tick(t: &mut Tick<'_, '_>) {
    if t.mob_ref().brain.is_none() {
        return;
    }
    brain(t).forget_outdated();
    let n = brain_ref(t).sensors.len();
    for i in 0..n {
        let due = {
            let s = &mut brain(t).sensors[i];
            s.time_to_tick -= 1;
            if s.time_to_tick <= 0 {
                s.time_to_tick = i64::from(s.scan_rate);
                true
            } else {
                false
            }
        };
        if due {
            let kind = brain_ref(t).sensors[i].kind;
            sensor::tick(t, kind);
            if !t.exists() {
                return;
            }
        }
    }
    let now = t.game_time();
    let count = brain_ref(t).by_priority.len();
    for i in 0..count {
        if !t.exists() || t.mob_ref().brain.is_none() {
            return;
        }
        let (_, activity, node) = brain_ref(t).by_priority[i];
        let b = brain_ref(t);
        if b.is_active(activity) && b.nodes[usize::from(node)].status == Status::Stopped {
            behavior::try_start(t, node, now);
        }
    }
    if !t.exists() || t.mob_ref().brain.is_none() {
        return;
    }
    // `getRunningBehaviors` in priority order, then `tickOrStop` each.
    let mut running = std::mem::take(&mut brain(t).running);
    running.clear();
    {
        let b = brain_ref(t);
        running.extend(
            b.by_priority
                .iter()
                .map(|e| e.2)
                .filter(|&n| b.nodes[usize::from(n)].status == Status::Running),
        );
    }
    for &node in &running {
        if !t.exists() || t.mob_ref().brain.is_none() {
            return;
        }
        behavior::tick_or_stop(t, node, now);
    }
    if t.exists()
        && let Some(b) = t.mob().brain.as_mut()
    {
        b.running = running;
    }
}

/// `stopAll`: stops every running behaviour.
pub fn stop_all(t: &mut Tick<'_, '_>) {
    if t.mob_ref().brain.is_none() {
        return;
    }
    let now = t.game_time();
    let tops: Vec<u16> = brain_ref(t).by_priority.iter().map(|e| e.2).collect();
    for node in tops {
        if brain_ref(t).nodes[usize::from(node)].status == Status::Running {
            behavior::do_stop(t, node, now);
        }
    }
}

// --- NBT ------------------------------------------------------------------------------------

/// Writes `Brain: {memories: {...}}` with the serializable memories (`Brain.pack`).
pub fn save(brain: &Brain, out: &mut vanillite_nbt::Compound) {
    use vanillite_nbt::{Compound, List, Tag};
    let mut memories = Compound::new();
    for s in &brain.slots {
        if !s.memory.serializable() {
            continue;
        }
        let Some(v) = &s.value else {
            continue;
        };
        let value: Tag = match v {
            MemValue::Unit => Tag::Compound(Compound::new()),
            MemValue::Bool(b) => Tag::Byte(i8::from(*b)),
            MemValue::Int(i) => Tag::Int(*i),
            MemValue::Long(l) => Tag::Long(*l),
            MemValue::Uuid(u) => uuid_tag(*u),
            MemValue::GlobalPos(g) => global_pos_tag(g),
            MemValue::GlobalPositions(list) => {
                Tag::List(List(list.iter().map(global_pos_tag).collect()))
            }
            _ => continue,
        };
        let mut entry = Compound::new();
        entry.insert("value", value);
        if s.ttl != Slot::NEVER {
            entry.insert("ttl", s.ttl);
        }
        memories.insert(format!("minecraft:{}", s.memory.name()), entry);
    }
    let mut c = Compound::new();
    c.insert("memories", memories);
    out.insert("Brain", c);
}

fn uuid_tag(u: u128) -> vanillite_nbt::Tag {
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    let v = vec![
        (u >> 96) as i32,
        (u >> 64) as i32,
        (u >> 32) as i32,
        u as i32,
    ];
    vanillite_nbt::Tag::IntArray(v)
}

fn global_pos_tag(g: &GlobalPos) -> vanillite_nbt::Tag {
    let mut c = vanillite_nbt::Compound::new();
    c.insert("dimension", &*g.dimension);
    c.insert("pos", vec![g.pos.x, g.pos.y, g.pos.z]);
    vanillite_nbt::Tag::Compound(c)
}

fn global_pos_of(t: &vanillite_nbt::Tag) -> Option<GlobalPos> {
    use vanillite_nbt::Tag;
    let Tag::Compound(c) = t else {
        return None;
    };
    let dimension = c.get("dimension").and_then(Tag::as_str)?.into();
    let Some(Tag::IntArray(p)) = c.get("pos") else {
        return None;
    };
    (p.len() == 3).then(|| GlobalPos {
        dimension,
        pos: BlockPos::new(p[0], p[1], p[2]),
    })
}

/// Reads `Brain.memories` into the registered memories (`makeBrain(packed)`).
pub fn load(brain: &mut Brain, tag: &vanillite_nbt::Compound) {
    use vanillite_nbt::Tag;
    let Some(Tag::Compound(b)) = tag.get("Brain") else {
        return;
    };
    let Some(Tag::Compound(memories)) = b.get("memories") else {
        return;
    };
    for &m in &MemoryType::ALL {
        if !m.serializable() || !brain.is_registered(m) {
            continue;
        }
        let key = format!("minecraft:{}", m.name());
        let Some(Tag::Compound(entry)) = memories.get(&key) else {
            continue;
        };
        let Some(v) = entry.get("value") else {
            continue;
        };
        let value = match (m.kind(), v) {
            (ValueKind::Unit, _) => Some(MemValue::Unit),
            (ValueKind::Bool, Tag::Byte(b)) => Some(MemValue::Bool(*b != 0)),
            (ValueKind::Int, Tag::Int(i)) => Some(MemValue::Int(*i)),
            (ValueKind::Long, Tag::Long(l)) => Some(MemValue::Long(*l)),
            (ValueKind::Uuid, Tag::IntArray(a)) if a.len() == 4 => {
                #[allow(clippy::cast_sign_loss)]
                let u = a
                    .iter()
                    .fold(0u128, |acc, &x| (acc << 32) | u128::from(x as u32));
                Some(MemValue::Uuid(u))
            }
            (ValueKind::GlobalPos, t) => global_pos_of(t).map(MemValue::GlobalPos),
            (ValueKind::GlobalPositions, Tag::List(l)) => Some(MemValue::GlobalPositions(
                l.0.iter().filter_map(global_pos_of).collect(),
            )),
            _ => None,
        };
        let Some(value) = value else {
            continue;
        };
        let ttl = match entry.get("ttl") {
            Some(Tag::Long(l)) => *l,
            _ => Slot::NEVER,
        };
        brain.set_with_expiry(m, value, ttl);
    }
}

#[cfg(test)]
pub(crate) mod tests;
