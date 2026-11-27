//! The event view of the kinds since ABI 1.6 ([`GameEvent`]), plugin goals ([`Goal`]) and
//! chunk populators ([`ChunkPopulator`]).

use std::ffi::c_void;
use std::sync::Mutex;

use vanillite_abi::{self as abi, Status, status};

use crate::ffi::{guard, str};
use crate::generator::{ChunkBlocks, WorldInfo};
use crate::types::{BlockPos, Location, Text, Uuid, Vec3, World};

/// An event of the kinds added with ABI 1.6 (`abi::event::PLAYER_MOVE` and later). Which
/// fields a kind fills and which it lets handlers change is documented at its kind in
/// [`abi::event`]. Since ABI 1.6.
#[derive(Debug)]
pub struct GameEvent<'a> {
    pub(crate) raw: &'a mut abi::GameEvent,
}

fn opt_uuid(u: abi::Uuid) -> Option<Uuid> {
    (u.to_u128() != 0).then(|| Uuid::from_abi(u))
}

impl GameEvent<'_> {
    /// The kind ([`abi::event`]).
    pub fn kind(&self) -> u32 {
        self.raw.header.kind
    }

    /// The world.
    pub fn world(&self) -> World {
        World(self.raw.world)
    }

    /// The player or entity the event is about.
    pub fn entity(&self) -> Option<Uuid> {
        opt_uuid(self.raw.entity)
    }

    /// Its type, e.g. `minecraft:zombie`.
    pub fn entity_type(&self) -> &str {
        // SAFETY: the host's strings are valid for the call.
        unsafe { str(self.raw.entity_type) }
    }

    /// The second entity (target, vehicle, owner, parent, killer, item entity).
    pub fn other(&self) -> Option<Uuid> {
        opt_uuid(self.raw.other)
    }

    /// Its type.
    pub fn other_type(&self) -> &str {
        // SAFETY: the host's strings are valid for the call.
        unsafe { str(self.raw.other_type) }
    }

    /// The third entity (direct one, other parent, shooter).
    pub fn source(&self) -> Option<Uuid> {
        opt_uuid(self.raw.source)
    }

    /// The player behind it.
    pub fn player(&self) -> Option<Uuid> {
        opt_uuid(self.raw.player)
    }

    /// The kind's reason, cause or action code.
    pub fn cause(&self) -> u32 {
        self.raw.cause
    }

    /// The kind's identifier (item, statistic, menu type, recipe, old state).
    pub fn key(&self) -> &str {
        // SAFETY: the host's strings are valid for the call.
        unsafe { str(self.raw.key) }
    }

    /// The block.
    pub fn pos(&self) -> BlockPos {
        BlockPos::from_abi(self.raw.pos)
    }

    /// The block state.
    pub fn state(&self) -> &str {
        // SAFETY: the host's strings are valid for the call.
        unsafe { str(self.raw.state) }
    }

    /// Where from.
    pub fn from(&self) -> Location {
        Location::from_abi(self.raw.from)
    }

    /// Where to.
    pub fn to(&self) -> Location {
        Location::from_abi(self.raw.to)
    }

    /// Changes where to (kinds that allow it).
    pub fn set_to(&mut self, to: Location) {
        self.raw.to = to.to_abi();
    }

    /// The item or items as SNBT.
    pub fn item(&self) -> &str {
        // SAFETY: the host's strings are valid for the call.
        unsafe { str(self.raw.item) }
    }

    /// The second item as SNBT.
    pub fn other_item(&self) -> &str {
        // SAFETY: the host's strings are valid for the call.
        unsafe { str(self.raw.other_item) }
    }

    /// Replaces the item or items (SNBT; kinds that allow it).
    pub fn set_item(&mut self, snbt: &str) {
        crate::events::set_text(self.raw.item_out, &Text::Plain(snbt.to_owned()));
    }

    /// The text.
    pub fn text(&self) -> &str {
        // SAFETY: the host's strings are valid for the call.
        unsafe { str(self.raw.text) }
    }

    /// Replaces the text (kinds that allow it).
    pub fn set_text(&mut self, text: impl Into<Text>) {
        crate::events::set_text(self.raw.text_out, &text.into());
    }

    /// The number (radius, damage).
    pub fn amount(&self) -> f64 {
        self.raw.amount
    }

    /// Changes the number (kinds that allow it).
    pub fn set_amount(&mut self, amount: f64) {
        self.raw.amount = amount;
    }

    /// The integer (slot, count, experience, cost).
    pub fn value(&self) -> i64 {
        self.raw.value
    }

    /// Changes the integer (kinds that allow it).
    pub fn set_value(&mut self, value: i64) {
        self.raw.value = value;
    }

    /// The second integer (previous value, button).
    pub fn extra(&self) -> i64 {
        self.raw.extra
    }

    /// The flags.
    pub fn flags(&self) -> u32 {
        self.raw.flags
    }

    /// Changes the flags (kinds that allow it).
    pub fn set_flags(&mut self, flags: u32) {
        self.raw.flags = flags;
    }

    /// Whether a handler cancelled it.
    pub fn is_cancelled(&self) -> bool {
        self.raw.header.cancelled != 0
    }

    /// Cancels it or takes the cancellation back (cancellable kinds).
    pub fn set_cancelled(&mut self, cancelled: bool) {
        self.raw.header.cancelled = u32::from(cancelled);
    }
}

/// What a plugin goal sees of its mob. Since ABI 1.6.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GoalMob {
    /// The mob.
    pub mob: Uuid,
    /// Its world.
    pub world: World,
    /// Its position.
    pub pos: Vec3,
    /// Its yaw.
    pub yaw: f32,
    /// Its pitch.
    pub pitch: f32,
    /// Its health.
    pub health: f32,
    /// On the ground.
    pub on_ground: bool,
    /// Its navigation follows a path.
    pub navigating: bool,
    /// Its target.
    pub target: Option<Uuid>,
    /// The target's position.
    pub target_pos: Vec3,
    /// The world's game time.
    pub game_time: i64,
}

impl GoalMob {
    fn from_abi(m: &abi::GoalMob) -> GoalMob {
        GoalMob {
            mob: Uuid::from_abi(m.mob),
            world: World(m.world),
            pos: Vec3::from_abi(m.pos),
            yaw: m.yaw,
            pitch: m.pitch,
            health: m.health,
            on_ground: m.on_ground != 0,
            navigating: m.navigating != 0,
            target: opt_uuid(m.target),
            target_pos: Vec3::from_abi(m.target_pos),
            game_time: m.game_time,
        }
    }
}

/// What a plugin goal's tick makes its mob do; [`GoalAction::default`] does nothing. Since
/// ABI 1.6.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GoalAction {
    /// Walk there at this speed modifier (1 normal) by the mob's path finding.
    pub move_to: Option<(Vec3, f64)>,
    /// Look there.
    pub look_at: Option<Vec3>,
    /// Jump.
    pub jump: bool,
    /// Change the target (`Some(None)` forgets it).
    pub target: Option<Option<Uuid>>,
    /// Stop moving.
    pub stop: bool,
}

/// A goal a plugin adds to one mob ([`Ctx::add_goal`](crate::Ctx::add_goal)). Runs on the
/// mob's owner during its AI tick, like Vanilla's goals. Since ABI 1.6.
pub trait Goal: Send + 'static {
    /// Whether it should start.
    fn can_use(&mut self, mob: &GoalMob) -> bool;
    /// Whether it should go on.
    fn can_continue(&mut self, mob: &GoalMob) -> bool {
        self.can_use(mob)
    }
    /// It starts.
    fn start(&mut self, _mob: &GoalMob) {}
    /// It stops.
    fn stop(&mut self, _mob: &GoalMob) {}
    /// One tick.
    fn tick(&mut self, mob: &GoalMob) -> GoalAction;
}

pub(crate) type GoalBox = Mutex<Box<dyn Goal>>;

fn with_goal<R>(user: *mut c_void, f: impl FnOnce(&mut dyn Goal) -> R) -> Option<R> {
    // SAFETY: `user` is the `GoalBox` of `add_goal`, alive until `goal_drop`.
    let goal = unsafe { &*user.cast::<GoalBox>() };
    let mut goal = goal.lock().ok()?;
    Some(f(&mut **goal))
}

pub(crate) unsafe extern "C" fn goal_can_use(user: *mut c_void, mob: *const abi::GoalMob) -> u32 {
    let mut out = 0;
    let _ = guard(|| {
        // SAFETY: the host passes a valid record for the call.
        if let Some(mob) = unsafe { mob.as_ref() } {
            let mob = GoalMob::from_abi(mob);
            out = u32::from(with_goal(user, |g| g.can_use(&mob)).unwrap_or(false));
        }
        status::OK
    });
    out
}

pub(crate) unsafe extern "C" fn goal_can_continue(
    user: *mut c_void,
    mob: *const abi::GoalMob,
) -> u32 {
    let mut out = 0;
    let _ = guard(|| {
        // SAFETY: the host passes a valid record for the call.
        if let Some(mob) = unsafe { mob.as_ref() } {
            let mob = GoalMob::from_abi(mob);
            out = u32::from(with_goal(user, |g| g.can_continue(&mob)).unwrap_or(false));
        }
        status::OK
    });
    out
}

pub(crate) unsafe extern "C" fn goal_start(user: *mut c_void, mob: *const abi::GoalMob) {
    let _ = guard(|| {
        // SAFETY: the host passes a valid record for the call.
        if let Some(mob) = unsafe { mob.as_ref() } {
            let mob = GoalMob::from_abi(mob);
            with_goal(user, |g| g.start(&mob));
        }
        status::OK
    });
}

pub(crate) unsafe extern "C" fn goal_stop(user: *mut c_void, mob: *const abi::GoalMob) {
    let _ = guard(|| {
        // SAFETY: the host passes a valid record for the call.
        if let Some(mob) = unsafe { mob.as_ref() } {
            let mob = GoalMob::from_abi(mob);
            with_goal(user, |g| g.stop(&mob));
        }
        status::OK
    });
}

pub(crate) unsafe extern "C" fn goal_tick(
    user: *mut c_void,
    mob: *const abi::GoalMob,
    action: *mut abi::GoalAction,
) {
    let _ = guard(|| {
        // SAFETY: the host passes valid records for the call.
        let (Some(mob), Some(out)) = (unsafe { mob.as_ref() }, unsafe { action.as_mut() }) else {
            return status::INVALID_ARGUMENT;
        };
        let mob = GoalMob::from_abi(mob);
        let Some(a) = with_goal(user, |g| g.tick(&mob)) else {
            return status::OK;
        };
        let mut bits = 0;
        if let Some((to, speed)) = a.move_to {
            bits |= abi::goal_action::MOVE;
            out.move_to = to.to_abi();
            out.speed = speed;
        }
        if let Some(at) = a.look_at {
            bits |= abi::goal_action::LOOK;
            out.look_at = at.to_abi();
        }
        if a.jump {
            bits |= abi::goal_action::JUMP;
        }
        if let Some(target) = a.target {
            bits |= abi::goal_action::TARGET;
            out.target = target.map_or(abi::Uuid::NIL, Uuid::to_abi);
        }
        if a.stop {
            bits |= abi::goal_action::STOP;
        }
        out.actions = bits;
        status::OK
    });
}

pub(crate) unsafe extern "C" fn goal_drop(user: *mut c_void) {
    let _ = guard(|| {
        // SAFETY: `user` is the box of `add_goal`; the host drops it exactly once.
        drop(unsafe { Box::from_raw(user.cast::<GoalBox>()) });
        status::OK
    });
}

/// A chunk populator (Bukkit's `BlockPopulator`): changes generated chunks after a stage of
/// the built-in generation. Called from many threads at once. Since ABI 1.6.
pub trait ChunkPopulator: Send + Sync + 'static {
    /// Populates one chunk; its blocks hold what the generation made so far.
    fn populate(&self, world: &WorldInfo, chunk: &mut ChunkBlocks<'_>);
}

pub(crate) unsafe extern "C" fn populate(
    user: *mut c_void,
    world: *const abi::WorldInfo,
    chunks: *const abi::GenChunk,
    count: usize,
    stride: usize,
) -> Status {
    guard(|| {
        // SAFETY: the host passes a valid record for the call.
        let Some(world) = (unsafe { world.as_ref() }) else {
            return status::INVALID_ARGUMENT;
        };
        // SAFETY: `user` is the leaked populator box of the registration.
        let populator = unsafe { &*user.cast::<Box<dyn ChunkPopulator>>() };
        let info = WorldInfo {
            // SAFETY: the host's strings are valid for the call.
            dimension: unsafe { str(world.dimension) }.to_owned(),
            min_y: world.min_y,
            height: world.height,
            seed: world.seed,
        };
        for i in 0..count {
            // SAFETY: the host passes `count` records `stride` bytes apart, each with its own
            // block array of `len` entries, valid for the call.
            let chunk = unsafe { &*chunks.cast::<u8>().add(i * stride).cast::<abi::GenChunk>() };
            // SAFETY: as above; every chunk has its own array.
            let blocks = unsafe { std::slice::from_raw_parts_mut(chunk.blocks, chunk.len) };
            let mut view = ChunkBlocks::new(chunk.x, chunk.z, info.min_y, info.height, blocks);
            populator.populate(&info, &mut view);
        }
        status::OK
    })
}
