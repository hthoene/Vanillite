//! The brain AI of mobs (`Brain`, `MemoryModuleType`, `Sensor`, `Activity`, `Behavior`,
//! `OneShot`, `GateBehavior`/`RunOne`), shared by every brain driven kind (the warden here;
//! villagers, piglins, axolotls, frogs, ... later).
//!
//! # Design
//!
//! A [`Brain`] is plain data owned by the mob's kind state:
//! - [`Memories`]: the registered memory slots (a small vector, looked up linearly: brains
//!   register 10–40 memories) with Vanilla's expiry (`MemorySlot`: a value with a time to live
//!   that is counted down by [`Brain::tick`] and cleared when it reaches zero).
//! - Sensors: a kind specific sensor enum `S: Sensor<C>` with Vanilla's scan rate and the
//!   random start delay (`Sensor.randomlyDelayStart`).
//! - Behaviors: a kind specific behavior enum `B: Behavior<C>` (no trait objects; one `match`
//!   per call), each in a [`Node`] that keeps Vanilla's `status`/`endTimestamp` bookkeeping.
//!   [`Node::Gate`] is `GateBehavior` (and with [`Gate::run_one`] `RunOne`).
//! - Activities: the entry requirements and the memories erased when an activity stops; the
//!   core activities plus one other are active (`setActiveActivityToFirstValid`).
//!
//! `C` is the context the behaviors and sensors work on, for mobs the mob tick
//! ([`crate::mob::Tick`]). Because that context reaches the mob's data (where the brain lives),
//! the kind takes the brain out of its state for [`Brain::tick`] (`std::mem::take`, no
//! allocation: the default brain is empty) and puts it back afterwards; behavior code receives
//! the [`Memories`] separately.
//!
//! The tick is allocation free: sensors refill their memory vectors in place, behaviors keep
//! their state inline.
//!
//! Vanilla iterates the behaviors of one priority in the order of a `HashMap<Activity, Set>`
//! (identity hashed, so not deterministic); here they run in registration order.

use vanillite_base::BlockPos;
use vanillite_nbt::{Compound, Tag};
use vanillite_world::entity::EntityId;

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
        1 << self as u8
    }
}

/// A set of activities (`Set<Activity>`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActivitySet(u32);

impl ActivitySet {
    pub fn contains(self, a: Activity) -> bool {
        self.0 & a.bit() != 0
    }

    pub fn insert(&mut self, a: Activity) {
        self.0 |= a.bit();
    }

    pub fn of(activities: &[Activity]) -> ActivitySet {
        let mut s = ActivitySet::default();
        for &a in activities {
            s.insert(a);
        }
        s
    }
}

/// How a memory value is stored in NBT (`MemoryModuleType`'s codec), `None` if not saved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Codec {
    None,
    Unit,
    Bool,
    Int,
    Long,
    Uuid,
}

macro_rules! memory_types {
    ($($variant:ident = $name:literal : $codec:ident),* $(,)?) => {
        /// `MemoryModuleType` (the ones used so far; add more as kinds need them).
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum MemoryType {
            $(#[doc = concat!("`minecraft:", $name, "`.")] $variant,)*
        }

        impl MemoryType {
            /// The registry name without namespace.
            pub fn name(self) -> &'static str {
                match self { $(MemoryType::$variant => $name,)* }
            }

            fn codec(self) -> Codec {
                match self { $(MemoryType::$variant => Codec::$codec,)* }
            }

            fn by_name(name: &str) -> Option<MemoryType> {
                let name = name.strip_prefix("minecraft:").unwrap_or(name);
                match name { $($name => Some(MemoryType::$variant),)* _ => None }
            }
        }
    };
}

memory_types! {
    Home = "home": None,
    JobSite = "job_site": None,
    MeetingPoint = "meeting_point": None,
    NearestLivingEntities = "mobs": None,
    NearestVisibleLivingEntities = "visible_mobs": None,
    NearestPlayers = "nearest_players": None,
    NearestVisiblePlayer = "nearest_visible_player": None,
    NearestVisibleAttackablePlayer = "nearest_visible_targetable_player": None,
    NearestVisibleAttackablePlayers = "nearest_visible_targetable_players": None,
    WalkTarget = "walk_target": None,
    LookTarget = "look_target": None,
    AttackTarget = "attack_target": None,
    AttackCoolingDown = "attack_cooling_down": None,
    InteractionTarget = "interaction_target": None,
    BreedTarget = "breed_target": None,
    Path = "path": None,
    HurtBy = "hurt_by": None,
    HurtByEntity = "hurt_by_entity": None,
    AvoidTarget = "avoid_target": None,
    NearestHostile = "nearest_hostile": None,
    NearestAttackable = "nearest_attackable": None,
    CantReachWalkTargetSince = "cant_reach_walk_target_since": None,
    NearestVisibleNemesis = "nearest_visible_nemesis": None,
    LastSlept = "last_slept": Long,
    LastWoken = "last_woken": Long,
    LastWorkedAtPoi = "last_worked_at_poi": Long,
    TemptingPlayer = "tempting_player": None,
    TemptationCooldownTicks = "temptation_cooldown_ticks": Int,
    IsInWater = "is_in_water": Unit,
    IsPanicking = "is_panicking": Bool,
    AngryAt = "angry_at": Uuid,
    UniversalAnger = "universal_anger": Bool,
    RoarTarget = "roar_target": None,
    DisturbanceLocation = "disturbance_location": None,
    RecentProjectile = "recent_projectile": Unit,
    IsSniffing = "is_sniffing": Unit,
    IsEmerging = "is_emerging": Unit,
    RoarSoundDelay = "roar_sound_delay": Unit,
    DigCooldown = "dig_cooldown": Unit,
    RoarSoundCooldown = "roar_sound_cooldown": Unit,
    SniffCooldown = "sniff_cooldown": Unit,
    TouchCooldown = "touch_cooldown": Unit,
    VibrationCooldown = "vibration_cooldown": Unit,
    SonicBoomCooldown = "sonic_boom_cooldown": Unit,
    SonicBoomSoundCooldown = "sonic_boom_sound_cooldown": Unit,
    SonicBoomSoundDelay = "sonic_boom_sound_delay": Unit,
    LikedPlayer = "liked_player": Uuid,
    ItemPickupCooldownTicks = "item_pickup_cooldown_ticks": Int,
}

/// `MemoryStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryStatus {
    Present,
    Absent,
    Registered,
}

/// `PositionTracker`: a block or an entity (`BlockPosTracker`, `EntityTracker`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PositionTracker {
    Block(BlockPos),
    /// The entity; `eye` tracks its eye height (`trackEyeHeight`).
    Entity {
        id: EntityId,
        eye: bool,
    },
}

/// `WalkTarget`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WalkTarget {
    pub target: PositionTracker,
    pub speed: f32,
    pub close_enough: i32,
}

/// The value of a memory.
#[derive(Clone, Debug, PartialEq)]
pub enum MemoryValue {
    Unit,
    Bool(bool),
    Int(i32),
    Long(i64),
    Uuid(u128),
    Pos(BlockPos),
    Entity(EntityId),
    /// Entities sorted by distance (the sensors' lists).
    Entities(Vec<EntityId>),
    Tracker(PositionTracker),
    Walk(WalkTarget),
}

/// "Never expires" (`MemorySlot.NEVER_EXPIRE`).
const NEVER: i64 = i64::MAX;

#[derive(Clone, Debug, PartialEq)]
struct Slot {
    kind: MemoryType,
    value: Option<MemoryValue>,
    ttl: i64,
}

/// The memory slots of a brain (`Brain.memories`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Memories {
    slots: Vec<Slot>,
}

impl Memories {
    fn slot(&self, kind: MemoryType) -> Option<&Slot> {
        self.slots.iter().find(|s| s.kind == kind)
    }

    fn slot_mut(&mut self, kind: MemoryType) -> Option<&mut Slot> {
        self.slots.iter_mut().find(|s| s.kind == kind)
    }

    /// `registerMemory`.
    pub fn register(&mut self, kind: MemoryType) {
        if self.slot(kind).is_none() {
            self.slots.push(Slot {
                kind,
                value: None,
                ttl: NEVER,
            });
        }
    }

    /// `checkMemory(type, status)`.
    pub fn check(&self, kind: MemoryType, status: MemoryStatus) -> bool {
        let Some(slot) = self.slot(kind) else {
            return false;
        };
        match status {
            MemoryStatus::Registered => true,
            MemoryStatus::Present => slot.value.is_some(),
            MemoryStatus::Absent => slot.value.is_none(),
        }
    }

    /// Whether all `(memory, status)` conditions hold (`hasRequiredMemories`).
    pub fn check_all(&self, conditions: &[(MemoryType, MemoryStatus)]) -> bool {
        conditions.iter().all(|&(m, s)| self.check(m, s))
    }

    /// `hasMemoryValue`.
    pub fn has(&self, kind: MemoryType) -> bool {
        self.check(kind, MemoryStatus::Present)
    }

    /// `getMemory`: the value, `None` when empty or not registered.
    pub fn get(&self, kind: MemoryType) -> Option<&MemoryValue> {
        self.slot(kind)?.value.as_ref()
    }

    /// An entity memory.
    pub fn entity(&self, kind: MemoryType) -> Option<EntityId> {
        match self.get(kind) {
            Some(MemoryValue::Entity(e)) => Some(*e),
            _ => None,
        }
    }

    /// A block position memory.
    pub fn pos(&self, kind: MemoryType) -> Option<BlockPos> {
        match self.get(kind) {
            Some(MemoryValue::Pos(p)) => Some(*p),
            _ => None,
        }
    }

    /// A long memory.
    pub fn long(&self, kind: MemoryType) -> Option<i64> {
        match self.get(kind) {
            Some(MemoryValue::Long(v)) => Some(*v),
            _ => None,
        }
    }

    /// An entity list memory (empty when absent).
    pub fn entities(&self, kind: MemoryType) -> &[EntityId] {
        match self.get(kind) {
            Some(MemoryValue::Entities(v)) => v,
            _ => &[],
        }
    }

    /// A walk target memory.
    pub fn walk_target(&self) -> Option<WalkTarget> {
        match self.get(MemoryType::WalkTarget) {
            Some(MemoryValue::Walk(w)) => Some(*w),
            _ => None,
        }
    }

    /// A position tracker memory.
    pub fn tracker(&self, kind: MemoryType) -> Option<PositionTracker> {
        match self.get(kind) {
            Some(MemoryValue::Tracker(t)) => Some(*t),
            _ => None,
        }
    }

    /// `getTimeUntilExpiry`.
    pub fn time_until_expiry(&self, kind: MemoryType) -> Option<i64> {
        self.slot(kind).map(|s| s.ttl)
    }

    /// `setMemory(type, value)`: only registered memories take values.
    pub fn set(&mut self, kind: MemoryType, value: MemoryValue) {
        self.set_with_expiry(kind, value, NEVER);
    }

    /// `setMemoryWithExpiry(type, value, timeToLive)`.
    pub fn set_with_expiry(&mut self, kind: MemoryType, value: MemoryValue, ttl: i64) {
        if let Some(slot) = self.slot_mut(kind) {
            // `isEmptyCollection`: an empty list clears the memory.
            if matches!(&value, MemoryValue::Entities(v) if v.is_empty()) {
                slot.value = None;
                slot.ttl = NEVER;
            } else {
                slot.value = Some(value);
                slot.ttl = ttl;
            }
        }
    }

    /// `setMemory(type, Optional)`.
    pub fn set_opt(&mut self, kind: MemoryType, value: Option<MemoryValue>) {
        match value {
            Some(v) => self.set(kind, v),
            None => self.erase(kind),
        }
    }

    /// Refills an entity list memory in place (the sensors' lists): `fill` writes into the
    /// slot's vector, an empty result clears the memory.
    pub fn set_entities_with(&mut self, kind: MemoryType, fill: impl FnOnce(&mut Vec<EntityId>)) {
        let Some(slot) = self.slot_mut(kind) else {
            return;
        };
        let mut list = match slot.value.take() {
            Some(MemoryValue::Entities(v)) => v,
            _ => Vec::new(),
        };
        list.clear();
        fill(&mut list);
        slot.ttl = NEVER;
        slot.value = (!list.is_empty()).then_some(MemoryValue::Entities(list));
    }

    /// `eraseMemory`.
    pub fn erase(&mut self, kind: MemoryType) {
        if let Some(slot) = self.slot_mut(kind) {
            slot.value = None;
            slot.ttl = NEVER;
        }
    }

    /// `clearMemories`.
    pub fn clear(&mut self) {
        for slot in &mut self.slots {
            slot.value = None;
            slot.ttl = NEVER;
        }
    }

    /// `forgetOutdatedMemories` (`MemorySlot.tick`).
    pub fn tick(&mut self) {
        for slot in &mut self.slots {
            if slot.value.is_some() && slot.ttl != NEVER {
                if slot.ttl <= 0 {
                    slot.value = None;
                    slot.ttl = NEVER;
                } else {
                    slot.ttl -= 1;
                }
            }
        }
    }

    /// `Brain.pack()` as NBT (`{memories: {"minecraft:<name>": {value, ttl}}}`): the memories
    /// with a codec.
    pub fn save(&self) -> Compound {
        let mut memories = Compound::new();
        for slot in &self.slots {
            let Some(value) = &slot.value else { continue };
            let tag = match (slot.kind.codec(), value) {
                (Codec::Unit, _) => Tag::Compound(Compound::new()),
                (Codec::Bool, MemoryValue::Bool(b)) => Tag::Byte(i8::from(*b)),
                (Codec::Int, MemoryValue::Int(v)) => Tag::Int(*v),
                (Codec::Long, MemoryValue::Long(v)) => Tag::Long(*v),
                (Codec::Uuid, MemoryValue::Uuid(u)) => Tag::IntArray(uuid_ints(*u)),
                _ => continue,
            };
            let mut entry = Compound::new();
            entry.insert("value", tag);
            if slot.ttl != NEVER {
                entry.insert("ttl", Tag::Long(slot.ttl));
            }
            memories.insert(
                format!("minecraft:{}", slot.kind.name()),
                Tag::Compound(entry),
            );
        }
        let mut out = Compound::new();
        out.insert("memories", Tag::Compound(memories));
        out
    }

    /// Reads `Brain.Packed`: values of registered memories only.
    pub fn load(&mut self, tag: &Compound) {
        let Some(Tag::Compound(memories)) = tag.get("memories") else {
            return;
        };
        for (name, entry) in memories.iter() {
            let (Some(kind), Tag::Compound(entry)) = (MemoryType::by_name(name), entry) else {
                continue;
            };
            let Some(value) = entry.get("value") else {
                continue;
            };
            let value = match (kind.codec(), value) {
                (Codec::Unit, _) => MemoryValue::Unit,
                (Codec::Bool, Tag::Byte(b)) => MemoryValue::Bool(*b != 0),
                (Codec::Int, Tag::Int(v)) => MemoryValue::Int(*v),
                (Codec::Long, Tag::Long(v)) => MemoryValue::Long(*v),
                (Codec::Uuid, Tag::IntArray(v)) if v.len() == 4 => MemoryValue::Uuid(
                    v.iter()
                        .fold(0u128, |acc, &w| acc << 32 | u128::from(w as u32)),
                ),
                _ => continue,
            };
            let ttl = match entry.get("ttl") {
                Some(Tag::Long(t)) => *t,
                _ => NEVER,
            };
            self.set_with_expiry(kind, value, ttl);
        }
    }
}

fn uuid_ints(uuid: u128) -> Vec<i32> {
    (0..4)
        .map(|i| (uuid >> (96 - 32 * i)) as u32 as i32)
        .collect()
}

/// What brains need from the world they tick in.
pub trait BrainContext {
    /// `level.getGameTime()`.
    fn game_time(&self) -> i64;
    /// `level.getRandom().nextInt(bound)`.
    fn level_random_int(&mut self, bound: i32) -> i32;
    /// A float from the random `ShufflingList` uses.
    fn shuffle_random(&mut self) -> f32;
}

/// How a behavior starts and ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavor {
    /// `Behavior`: runs from `start` until it times out (a duration drawn between `min` and
    /// `max` with the level random) or `can_still_use` fails.
    Timed { min: i32, max: i32 },
    /// `OneShot` (`BehaviorBuilder`): `trigger` once per tick when its conditions hold.
    OneShot,
}

/// The context free part of a behavior: its entry conditions and how it runs.
pub trait BehaviorInfo {
    /// The entry conditions (`entryCondition`, or the `BehaviorBuilder` group).
    fn conditions(&self) -> &'static [(MemoryType, MemoryStatus)];
    /// Timed or one shot.
    fn flavor(&self) -> Flavor;
}

/// A kind's behavior (one enum per kind; each variant is one Vanilla behavior with its state).
pub trait Behavior<C>: BehaviorInfo {
    /// `checkExtraStartConditions` (timed behaviors).
    fn check_extra(&mut self, mem: &mut Memories, cx: &mut C) -> bool {
        let _ = (mem, cx);
        true
    }
    /// `start` (timed) or the trigger (one shot; returns whether it ran).
    fn start(&mut self, mem: &mut Memories, cx: &mut C, time: i64) -> bool;
    /// `canStillUse`.
    fn can_still_use(&mut self, mem: &mut Memories, cx: &mut C, time: i64) -> bool {
        let _ = (mem, cx, time);
        false
    }
    /// `tick`.
    fn tick(&mut self, mem: &mut Memories, cx: &mut C, time: i64) {
        let _ = (mem, cx, time);
    }
    /// `stop`.
    fn stop(&mut self, mem: &mut Memories, cx: &mut C, time: i64) {
        let _ = (mem, cx, time);
    }
}

/// The context free part of a sensor.
pub trait SensorInfo {
    /// The memories it writes (`requires`).
    fn requires(&self) -> &'static [MemoryType];
    /// `scanRate` (default 20).
    fn scan_rate(&self) -> i32 {
        20
    }
}

/// A kind's sensor (one enum per kind).
pub trait Sensor<C>: SensorInfo {
    /// `doTick`.
    fn sense(&mut self, mem: &mut Memories, cx: &mut C);
}

/// `Behavior.Status`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Stopped,
    Running,
}

/// A behavior with its run state.
#[derive(Clone, Debug)]
pub struct Leaf<B> {
    pub behavior: B,
    status: Status,
    end: i64,
}

/// `GateBehavior` (`RunOne` with [`Gate::run_one`]).
#[derive(Clone, Debug)]
pub struct Gate<B> {
    conditions: &'static [(MemoryType, MemoryStatus)],
    exit_erased: &'static [MemoryType],
    shuffled: bool,
    run_one: bool,
    /// Children with their weight and their last shuffle key.
    children: Vec<(Node<B>, i32, f64)>,
    status: Status,
}

impl<B> Gate<B> {
    /// `new RunOne(entryCondition, weightedBehaviors)`.
    pub fn run_one(
        conditions: &'static [(MemoryType, MemoryStatus)],
        children: Vec<(Node<B>, i32)>,
    ) -> Gate<B> {
        Gate {
            conditions,
            exit_erased: &[],
            shuffled: true,
            run_one: true,
            children: children.into_iter().map(|(n, w)| (n, w, 0.0)).collect(),
            status: Status::Stopped,
        }
    }

    /// `new GateBehavior(entryCondition, exitErasedMemories, order, running, behaviors)`.
    pub fn new(
        conditions: &'static [(MemoryType, MemoryStatus)],
        exit_erased: &'static [MemoryType],
        shuffled: bool,
        run_one: bool,
        children: Vec<(Node<B>, i32)>,
    ) -> Gate<B> {
        Gate {
            conditions,
            exit_erased,
            shuffled,
            run_one,
            children: children.into_iter().map(|(n, w)| (n, w, 0.0)).collect(),
            status: Status::Stopped,
        }
    }
}

/// One node of a brain's behavior table.
#[derive(Clone, Debug)]
pub enum Node<B> {
    Leaf(Leaf<B>),
    Gate(Box<Gate<B>>),
}

impl<B> Node<B> {
    /// A single behavior.
    pub fn leaf(behavior: B) -> Node<B> {
        Node::Leaf(Leaf {
            behavior,
            status: Status::Stopped,
            end: 0,
        })
    }

    /// `getStatus`.
    pub fn status(&self) -> Status {
        match self {
            Node::Leaf(l) => l.status,
            Node::Gate(g) => g.status,
        }
    }
}

impl<B> Node<B> {
    /// `tryStart`.
    fn try_start<C: BrainContext>(&mut self, mem: &mut Memories, cx: &mut C, time: i64) -> bool
    where
        B: Behavior<C>,
    {
        match self {
            Node::Leaf(l) => {
                if !mem.check_all(l.behavior.conditions()) {
                    return false;
                }
                match l.behavior.flavor() {
                    Flavor::OneShot => {
                        if l.behavior.start(mem, cx, time) {
                            l.status = Status::Running;
                            true
                        } else {
                            false
                        }
                    }
                    Flavor::Timed { min, max } => {
                        if !l.behavior.check_extra(mem, cx) {
                            return false;
                        }
                        l.status = Status::Running;
                        let duration = min + cx.level_random_int(max + 1 - min);
                        l.end = time + i64::from(duration);
                        l.behavior.start(mem, cx, time);
                        true
                    }
                }
            }
            Node::Gate(g) => {
                if !mem.check_all(g.conditions) {
                    return false;
                }
                g.status = Status::Running;
                if g.shuffled {
                    // `ShufflingList.shuffle`: `-pow(random, 1 / weight)` ascending.
                    for child in &mut g.children {
                        let r = cx.shuffle_random();
                        child.2 = -f64::from(r).powf(f64::from(1.0 / child.1 as f32));
                    }
                    g.children.sort_by(|a, b| a.2.total_cmp(&b.2));
                }
                for child in &mut g.children {
                    if child.0.status() == Status::Stopped
                        && child.0.try_start(mem, cx, time)
                        && g.run_one
                    {
                        break;
                    }
                }
                true
            }
        }
    }

    /// `tickOrStop`.
    fn tick_or_stop<C: BrainContext>(&mut self, mem: &mut Memories, cx: &mut C, time: i64)
    where
        B: Behavior<C>,
    {
        match self {
            Node::Leaf(l) => match l.behavior.flavor() {
                Flavor::OneShot => l.status = Status::Stopped,
                Flavor::Timed { .. } => {
                    if time <= l.end && l.behavior.can_still_use(mem, cx, time) {
                        l.behavior.tick(mem, cx, time);
                    } else {
                        l.status = Status::Stopped;
                        l.behavior.stop(mem, cx, time);
                    }
                }
            },
            Node::Gate(g) => {
                for child in &mut g.children {
                    if child.0.status() == Status::Running {
                        child.0.tick_or_stop(mem, cx, time);
                    }
                }
                if g.children.iter().all(|c| c.0.status() != Status::Running) {
                    self.do_stop(mem, cx, time);
                }
            }
        }
    }

    /// `doStop`.
    fn do_stop<C: BrainContext>(&mut self, mem: &mut Memories, cx: &mut C, time: i64)
    where
        B: Behavior<C>,
    {
        match self {
            Node::Leaf(l) => {
                l.status = Status::Stopped;
                if let Flavor::Timed { .. } = l.behavior.flavor() {
                    l.behavior.stop(mem, cx, time);
                }
            }
            Node::Gate(g) => {
                g.status = Status::Stopped;
                for child in &mut g.children {
                    if child.0.status() == Status::Running {
                        child.0.do_stop(mem, cx, time);
                    }
                }
                for &m in g.exit_erased {
                    mem.erase(m);
                }
            }
        }
    }
}

/// A sensor with its countdown.
#[derive(Clone, Debug)]
struct SensorSlot<S> {
    sensor: S,
    time_to_tick: i64,
}

/// `ActivityData`: an activity's behaviors by priority, its entry requirements and the
/// memories erased when it stops.
#[derive(Debug)]
pub struct ActivityData<B> {
    pub activity: Activity,
    pub behaviors: Vec<(i32, Node<B>)>,
    pub requirements: &'static [(MemoryType, MemoryStatus)],
    pub erase_on_stop: &'static [MemoryType],
}

impl<B> ActivityData<B> {
    /// `ActivityData.create(activity, priorityStart, behaviors, conditions...)`: priorities
    /// count up from `priority_start`.
    pub fn create(
        activity: Activity,
        priority_start: i32,
        behaviors: Vec<Node<B>>,
        requirements: &'static [(MemoryType, MemoryStatus)],
    ) -> ActivityData<B> {
        ActivityData {
            activity,
            behaviors: behaviors
                .into_iter()
                .zip(priority_start..)
                .map(|(b, p)| (p, b))
                .collect(),
            requirements,
            erase_on_stop: &[],
        }
    }
}

impl<B> ActivityData<B> {
    /// The memories erased when the activity stops (`memoriesToEraseWhenStopped`).
    pub fn erase(mut self, memories: &'static [MemoryType]) -> ActivityData<B> {
        self.erase_on_stop = memories;
        self
    }
}

/// `Brain<E>` with the kind's behavior enum `B` and sensor enum `S`.
#[derive(Debug)]
pub struct Brain<B, S> {
    pub memories: Memories,
    sensors: Vec<SensorSlot<S>>,
    /// `availableBehaviorsByPriority`: (priority, activity, behavior), sorted by priority
    /// (stable: registration order within a priority).
    behaviors: Vec<(i32, Activity, Node<B>)>,
    requirements: Vec<(Activity, &'static [(MemoryType, MemoryStatus)])>,
    erase_on_stop: Vec<(Activity, &'static [MemoryType])>,
    core: ActivitySet,
    active: ActivitySet,
    default_activity: Activity,
}

impl<B, S> Default for Brain<B, S> {
    fn default() -> Self {
        Brain {
            memories: Memories::default(),
            sensors: Vec::new(),
            behaviors: Vec::new(),
            requirements: Vec::new(),
            erase_on_stop: Vec::new(),
            core: ActivitySet::of(&[Activity::Core]),
            active: ActivitySet::of(&[Activity::Core, Activity::Idle]),
            default_activity: Activity::Idle,
        }
    }
}

impl<B, S> Brain<B, S> {
    /// Builds a brain (`Brain.Provider.makeBrain`): registers `memories`, the sensors (each
    /// with a random start delay `random_int(scanRate)`, `randomlyDelayStart`) and their
    /// memories, and the activities with their behaviors' memories; the default activity is
    /// active.
    pub fn build(
        memories: &[MemoryType],
        sensors: Vec<S>,
        activities: Vec<ActivityData<B>>,
        mut random_int: impl FnMut(i32) -> i32,
    ) -> Brain<B, S>
    where
        B: BehaviorInfo,
        S: SensorInfo,
    {
        let mut brain = Brain::default();
        for &m in memories {
            brain.memories.register(m);
        }
        for sensor in sensors {
            let time_to_tick = i64::from(random_int(sensor.scan_rate()));
            for &m in sensor.requires() {
                brain.memories.register(m);
            }
            brain.sensors.push(SensorSlot {
                sensor,
                time_to_tick,
            });
        }
        for data in activities {
            brain.requirements.push((data.activity, data.requirements));
            if !data.erase_on_stop.is_empty() {
                brain
                    .erase_on_stop
                    .push((data.activity, data.erase_on_stop));
            }
            for (priority, node) in data.behaviors {
                register_node(&mut brain.memories, &node);
                brain.behaviors.push((priority, data.activity, node));
            }
        }
        brain.behaviors.sort_by_key(|(p, _, _)| *p);
        brain
    }

    /// `isActive(activity)`.
    pub fn is_active(&self, activity: Activity) -> bool {
        self.active.contains(activity)
    }

    /// `getActiveNonCoreActivity`.
    pub fn active_non_core(&self) -> Option<Activity> {
        ALL_ACTIVITIES
            .into_iter()
            .find(|&a| self.active.contains(a) && !self.core.contains(a))
    }

    /// `setDefaultActivity`.
    pub fn set_default_activity(&mut self, activity: Activity) {
        self.default_activity = activity;
    }

    /// `setCoreActivities`.
    pub fn set_core_activities(&mut self, activities: &[Activity]) {
        self.core = ActivitySet::of(activities);
    }

    /// `useDefaultActivity`.
    pub fn use_default_activity(&mut self) {
        self.set_active_activity(self.default_activity);
    }

    fn requirements_met(&self, activity: Activity) -> bool {
        self.requirements
            .iter()
            .find(|(a, _)| *a == activity)
            .is_some_and(|(_, req)| self.memories.check_all(req))
    }

    /// `setActiveActivityIfPossible`.
    pub fn set_active_activity_if_possible(&mut self, activity: Activity) {
        if self.requirements_met(activity) {
            self.set_active_activity(activity);
        } else {
            self.use_default_activity();
        }
    }

    /// `setActiveActivityToFirstValid`.
    pub fn set_active_activity_to_first_valid(&mut self, activities: &[Activity]) {
        if let Some(&a) = activities.iter().find(|&&a| self.requirements_met(a)) {
            self.set_active_activity(a);
        }
    }

    /// `setActiveActivity`: erases the stopped activities' memories, then the core activities
    /// plus `activity` are active.
    fn set_active_activity(&mut self, activity: Activity) {
        if self.active.contains(activity) {
            return;
        }
        for &(old, memories) in &self.erase_on_stop {
            if old != activity && self.active.contains(old) {
                for &m in memories {
                    self.memories.erase(m);
                }
            }
        }
        self.active = self.core;
        self.active.insert(activity);
    }

    /// `Brain.tick`: forget outdated memories, tick the sensors, start the stopped behaviors
    /// of the active activities, tick the running ones.
    pub fn tick<C: BrainContext>(&mut self, cx: &mut C)
    where
        B: Behavior<C>,
        S: Sensor<C>,
    {
        let time = cx.game_time();
        self.memories.tick();
        let Brain {
            memories,
            sensors,
            behaviors,
            active,
            ..
        } = self;
        for slot in sensors.iter_mut() {
            slot.time_to_tick -= 1;
            if slot.time_to_tick <= 0 {
                slot.time_to_tick = i64::from(slot.sensor.scan_rate());
                slot.sensor.sense(memories, cx);
            }
        }
        for (_, activity, node) in behaviors.iter_mut() {
            if active.contains(*activity) && node.status() == Status::Stopped {
                node.try_start(memories, cx, time);
            }
        }
        for (_, _, node) in behaviors.iter_mut() {
            if node.status() == Status::Running {
                node.tick_or_stop(memories, cx, time);
            }
        }
    }

    /// `stopAll`.
    pub fn stop_all<C: BrainContext>(&mut self, cx: &mut C)
    where
        B: Behavior<C>,
    {
        let time = cx.game_time();
        let Brain {
            memories,
            behaviors,
            ..
        } = self;
        for (_, _, node) in behaviors.iter_mut() {
            if node.status() == Status::Running {
                node.do_stop(memories, cx, time);
            }
        }
    }

    /// The running behaviors (`getRunningBehaviors`, leaves only, for tests and debugging).
    pub fn running(&self) -> impl Iterator<Item = &B> {
        let mut out = Vec::new();
        for (_, _, node) in &self.behaviors {
            collect_running(node, &mut out);
        }
        out.into_iter()
    }
}

fn collect_running<'a, B>(node: &'a Node<B>, out: &mut Vec<&'a B>) {
    match node {
        Node::Leaf(l) => {
            if l.status == Status::Running {
                out.push(&l.behavior);
            }
        }
        Node::Gate(g) => {
            for c in &g.children {
                collect_running(&c.0, out);
            }
        }
    }
}

fn register_node<B: BehaviorInfo>(memories: &mut Memories, node: &Node<B>) {
    match node {
        Node::Leaf(l) => {
            for &(m, _) in l.behavior.conditions() {
                memories.register(m);
            }
        }
        Node::Gate(g) => {
            for &(m, _) in g.conditions {
                memories.register(m);
            }
            for c in &g.children {
                register_node(memories, &c.0);
            }
        }
    }
}

/// Every activity in declaration order.
const ALL_ACTIVITIES: [Activity; 26] = [
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

pub mod sensors;

#[cfg(test)]
mod tests;
