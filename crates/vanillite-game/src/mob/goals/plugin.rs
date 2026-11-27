//! Plugin goals (`vanillite_abi::GoalSpec`): goals a plugin adds to one mob. The mob's goal or
//! target selector runs them after its Vanilla goals, as Paper appends custom goals to
//! `GoalSelector.availableGoals`: with the same priorities, `Goal.Flag` locks and
//! `canUse`/`canContinueToUse`/`start`/`stop`/`tick` calls. A plugin goal's tick says what
//! the mob does ([`GoalAction`]); this module turns it into the mob's navigation, look
//! control, jump and target.
//!
//! The goals live out of line in [`PluginGoals`] of the world's entities: a mob without plugin
//! goals keeps Vanilla's goal list, order and random draws, and a world without any costs one
//! emptiness check per selector tick. They are dropped (freeing the plugin's data) when the
//! mob is removed or unloaded ([`crate::entities::Entities::despawn`]).

use vanillite_abi::{event, goal_action, goal_clear, goal_flag, target_reason};
use vanillite_base::Vec3;
use vanillite_base::hash::FxHashMap;
use vanillite_config::GameMode;
use vanillite_world::entity::{EntityFlags, EntityId};

use super::{NO_SLOT, Unit, Which};
use crate::entities::Entities;
use crate::hooks::{GameHooks, GoalAction, GoalMob, PluginGoal};
use crate::level::Level;
use crate::mob::brain::MemoryType;
use crate::mob::{MobData, Tick, nav};

/// The most plugin goals one selector of a mob takes.
pub const MAX_GOALS: usize = 64;

/// The largest speed modifier a goal's walk uses (as `mob_navigate` takes).
const MAX_SPEED: f64 = 100.0;

/// How far from the origin a goal may send its mob or look (the world border's limit).
const MAX_COORD: f64 = 3.0e7;

/// One plugin goal of a mob (`WrappedGoal`).
#[derive(Debug)]
struct Slot {
    id: u64,
    goal: Box<dyn PluginGoal>,
    running: bool,
}

/// The plugin goals of one mob and the Vanilla goals a plugin cleared.
#[derive(Debug, Default)]
struct MobGoals {
    goals: Vec<Slot>,
    targets: Vec<Slot>,
    /// `goal_clear` bits of the Vanilla goals to remove with the mob's next selector tick
    /// (stopping a running goal needs the tick).
    clear: u32,
}

impl MobGoals {
    fn list(&self, which: Which) -> &Vec<Slot> {
        match which {
            Which::Goals => &self.goals,
            Which::Targets => &self.targets,
        }
    }

    fn list_mut(&mut self, which: Which) -> &mut Vec<Slot> {
        match which {
            Which::Goals => &mut self.goals,
            Which::Targets => &mut self.targets,
        }
    }

    fn is_empty(&self) -> bool {
        self.goals.is_empty() && self.targets.is_empty() && self.clear == 0
    }
}

/// The plugin goals of the mobs of one world.
#[derive(Debug, Default)]
pub struct PluginGoals {
    mobs: FxHashMap<EntityId, MobGoals>,
    /// The last goal id handed out.
    next_id: u64,
}

impl PluginGoals {
    /// Whether no mob has plugin goals (or cleared goals waiting).
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.mobs.is_empty()
    }

    /// Drops the plugin goals of a removed mob.
    pub fn remove_mob(&mut self, id: EntityId) {
        self.mobs.remove(&id);
    }

    /// How many plugin goals a mob has.
    pub fn count(&self, id: EntityId) -> usize {
        self.mobs
            .get(&id)
            .map_or(0, |g| g.goals.len() + g.targets.len())
    }
}

fn which_of(goal: &dyn PluginGoal) -> Which {
    if goal.is_target() {
        Which::Targets
    } else {
        Which::Goals
    }
}

/// The `Goal.Flag` bits of a plugin goal.
fn flags_of(goal: &dyn PluginGoal) -> u8 {
    u8::try_from(goal.flags() & 0xF).unwrap_or(0)
}

/// Adds a plugin goal to a mob (`GoalSelector.addGoal`, after its Vanilla goals); returns its
/// id. `None` when `mob` is no mob or the selector is full ([`MAX_GOALS`]); the goal is then
/// dropped.
pub fn add(ents: &mut Entities, mob: EntityId, goal: Box<dyn PluginGoal>) -> Option<u64> {
    let which = which_of(goal.as_ref());
    let m = ents.mob(mob)?;
    let vanilla = match which {
        Which::Goals => m.goals.slots.len(),
        Which::Targets => m.targets.slots.len(),
    };
    let all = &mut ents.plugin_goals;
    let entry = all.mobs.entry(mob).or_default();
    let list = entry.list_mut(which);
    // The selector's locks name goals by an index below `NO_SLOT`.
    if list.len() >= MAX_GOALS || vanilla + list.len() + 1 >= usize::from(NO_SLOT) {
        if entry.is_empty() {
            all.mobs.remove(&mob);
        }
        return None;
    }
    all.next_id += 1;
    let id = all.next_id;
    list.push(Slot {
        id,
        goal,
        running: false,
    });
    Some(id)
}

/// Removes a plugin goal of a mob (`GoalSelector.removeGoal`: a running goal stops first);
/// dropping it frees the plugin's data. Returns whether the mob had it.
pub fn remove(level: &mut Level, mob: EntityId, id: u64, game_time: i64) -> bool {
    let hooks = level.hooks_for(event::ENTITY_TARGET);
    let ents = &mut level.entities;
    let Some(entry) = ents.plugin_goals.mobs.get_mut(&mob) else {
        return false;
    };
    let Some((which, j)) = [Which::Goals, Which::Targets].into_iter().find_map(|w| {
        entry
            .list(w)
            .iter()
            .position(|s| s.id == id)
            .map(|j| (w, j))
    }) else {
        return false;
    };
    let slot = entry.list_mut(which).remove(j);
    if entry.is_empty() {
        ents.plugin_goals.mobs.remove(&mob);
    }
    // The goals after it move down one place; a lock on it goes.
    if let Some(m) = ents.mob_mut(mob) {
        let selector = match which {
            Which::Goals => &mut m.goals,
            Which::Targets => &mut m.targets,
        };
        let index = selector.slots.len() + j;
        for l in &mut selector.locked {
            let i = usize::from(*l);
            if *l == NO_SLOT || i < index {
                continue;
            }
            *l = if i == index {
                NO_SLOT
            } else {
                u8::try_from(i - 1).unwrap_or(NO_SLOT)
            };
        }
    }
    if slot.running {
        if let Some(view) = goal_mob(ents, mob, game_time) {
            slot.goal.stop(&view);
        }
        stopped(
            hooks.as_deref(),
            ents,
            mob,
            which,
            flags_of(slot.goal.as_ref()),
        );
    }
    true
}

/// Asks the mob's next selector tick to remove its Vanilla goals (`goal_clear` bits) as
/// `GoalSelector.removeAllGoals` does. Returns `false` when `mob` is no mob.
pub fn request_clear(ents: &mut Entities, mob: EntityId, which: u32) -> bool {
    if ents.mob(mob).is_none() {
        return false;
    }
    let which = which & (goal_clear::GOALS | goal_clear::TARGETS);
    if which != 0 {
        ents.plugin_goals.mobs.entry(mob).or_default().clear |= which;
    }
    true
}

/// Whether the Vanilla goals of the ticking mob's selector are to be cleared now (the request
/// is taken).
pub(super) fn take_clear(t: &mut Tick<'_, '_>, which: Which) -> bool {
    if t.ents.plugin_goals.is_empty() {
        return false;
    }
    let bit = match which {
        Which::Goals => goal_clear::GOALS,
        Which::Targets => goal_clear::TARGETS,
    };
    let mobs = &mut t.ents.plugin_goals.mobs;
    let Some(entry) = mobs.get_mut(&t.id) else {
        return false;
    };
    if entry.clear & bit == 0 {
        return false;
    }
    entry.clear &= !bit;
    if entry.is_empty() {
        mobs.remove(&t.id);
    }
    true
}

/// How many plugin goals the ticking mob's selector has.
#[inline]
pub(super) fn count(t: &Tick<'_, '_>, which: Which) -> usize {
    if t.ents.plugin_goals.is_empty() {
        return 0;
    }
    t.ents
        .plugin_goals
        .mobs
        .get(&t.id)
        .map_or(0, |g| g.list(which).len())
}

fn slot<'a>(t: &'a Tick<'_, '_>, which: Which, j: usize) -> Option<&'a Slot> {
    t.ents.plugin_goals.mobs.get(&t.id)?.list(which).get(j)
}

fn slot_mut<'a>(t: &'a mut Tick<'_, '_>, which: Which, j: usize) -> Option<&'a mut Slot> {
    t.ents
        .plugin_goals
        .mobs
        .get_mut(&t.id)?
        .list_mut(which)
        .get_mut(j)
}

/// Plugin goal `j` of a selector of the ticking mob.
pub(super) fn unit(t: &Tick<'_, '_>, which: Which, j: usize) -> Option<Unit> {
    let s = slot(t, which, j)?;
    Some(Unit {
        running: s.running,
        flags: flags_of(s.goal.as_ref()),
        priority: s.goal.priority(),
        interruptable: true,
        // Its tick runs every tick (`requiresUpdateEveryTick`), as `GoalSpec.tick` says.
        every_tick: true,
    })
}

fn view(t: &Tick<'_, '_>) -> Option<GoalMob> {
    goal_mob(t.ents, t.id, t.game_time())
}

pub(super) fn can_use(t: &mut Tick<'_, '_>, which: Which, j: usize) -> bool {
    let Some(mob) = view(t) else {
        return false;
    };
    slot(t, which, j).is_some_and(|s| s.goal.can_use(&mob))
}

pub(super) fn can_continue(t: &mut Tick<'_, '_>, which: Which, j: usize) -> bool {
    let Some(mob) = view(t) else {
        return false;
    };
    slot(t, which, j).is_some_and(|s| s.goal.can_continue(&mob))
}

pub(super) fn start(t: &mut Tick<'_, '_>, which: Which, j: usize) {
    let Some(mob) = view(t) else {
        return;
    };
    match slot_mut(t, which, j) {
        Some(s) if !s.running => s.running = true,
        _ => return,
    }
    if let Some(s) = slot(t, which, j) {
        s.goal.start(&mob);
    }
}

pub(super) fn stop(t: &mut Tick<'_, '_>, which: Which, j: usize) {
    match slot_mut(t, which, j) {
        Some(s) if s.running => s.running = false,
        _ => return,
    }
    let Some(mob) = view(t) else {
        return;
    };
    let Some(flags) = slot(t, which, j).map(|s| {
        s.goal.stop(&mob);
        flags_of(s.goal.as_ref())
    }) else {
        return;
    };
    let hooks = t.level.hooks_for(event::ENTITY_TARGET);
    stopped(hooks.as_deref(), t.ents, t.id, which, flags);
}

/// What a stopping plugin goal leaves behind, as Vanilla's goals do in `stop`: a moving goal
/// stops the navigation (`navigation.stop()`), a target goal forgets the target
/// (`TargetGoal.stop`).
fn stopped(
    hooks: Option<&(dyn GameHooks + Send)>,
    ents: &mut Entities,
    mob: EntityId,
    which: Which,
    flags: u8,
) {
    let Some(m) = ents.mob_mut(mob) else {
        return;
    };
    if flags & super::flag::MOVE != 0 {
        m.nav.stop();
    }
    if which == Which::Targets
        && flags & super::flag::TARGET != 0
        && target_of(ents.mob(mob)).is_some()
    {
        change_target(hooks, ents, mob, None, target_reason::FORGET);
    }
}

pub(super) fn tick(t: &mut Tick<'_, '_>, which: Which, j: usize) {
    let Some(mob) = view(t) else {
        return;
    };
    let Some(action) = slot(t, which, j).map(|s| s.goal.tick(&mob)) else {
        return;
    };
    act(t, &action);
}

/// A position a goal may send its mob to or look at.
fn place(p: [f64; 3]) -> Option<Vec3> {
    p.iter()
        .all(|c| c.is_finite() && c.abs() <= MAX_COORD)
        .then(|| Vec3::new(p[0], p[1], p[2]))
}

/// Carries out a plugin goal's tick: its walk through the navigation
/// (`navigation.moveTo`), its look through the look control, its jump through the jump
/// control and its target (`setTarget`, with the target event). Values a plugin got wrong are
/// left out.
fn act(t: &mut Tick<'_, '_>, action: &GoalAction) {
    let a = action.actions;
    if a & goal_action::STOP != 0 {
        t.nav().stop();
    }
    if a & goal_action::MOVE != 0
        && let Some(to) = place(action.move_to)
        && action.speed.is_finite()
        && action.speed > 0.0
    {
        nav::move_to_pos(t, to, action.speed.min(MAX_SPEED));
    }
    if a & goal_action::LOOK != 0
        && let Some(at) = place(action.look_at)
    {
        nav::look_at(t, at);
    }
    if a & goal_action::JUMP != 0 {
        t.mob().jump = true;
    }
    if a & goal_action::TARGET != 0 {
        let target = if action.target == 0 {
            None
        } else {
            match crate::mob::animal::entity_by_uuid(t.ents, action.target) {
                Some(id) if valid_target(t.ents, t.id, id) => Some(id),
                _ => return,
            }
        };
        if target != target_of(t.ents.mob(t.id)) {
            let hooks = t.level.hooks_for(event::ENTITY_TARGET);
            change_target(
                hooks.as_deref(),
                t.ents,
                t.id,
                target,
                target_reason::PLUGIN,
            );
        }
    }
}

/// What a plugin goal sees of a mob; `None` when it is gone.
pub fn goal_mob(ents: &Entities, mob: EntityId, game_time: i64) -> Option<GoalMob> {
    let e = ents.store.get(mob)?;
    let m = ents.mob(mob)?;
    let (pos, rot) = (e.pos(), e.rot());
    let target = target_of(Some(m)).and_then(|id| ents.store.get(id));
    let target_pos = target.as_ref().map_or(Vec3::ZERO, |t| t.pos());
    Some(GoalMob {
        mob: e.uuid(),
        pos: [pos.x, pos.y, pos.z],
        yaw: rot.y_rot,
        pitch: rot.x_rot,
        health: m.health,
        on_ground: e.flags().contains(EntityFlags::ON_GROUND),
        navigating: m.nav.is_in_progress(),
        target: target.as_ref().map_or(0, |t| t.uuid()),
        target_pos: [target_pos.x, target_pos.y, target_pos.z],
        game_time,
    })
}

/// `Mob.getTarget`: the attack target memory of brain mobs, else the target.
pub fn target_of(m: Option<&MobData>) -> Option<EntityId> {
    let m = m?;
    match m.brain.as_deref() {
        Some(b) if b.is_registered(MemoryType::AttackTarget) => b.entity(MemoryType::AttackTarget),
        _ => m.target,
    }
}

/// Whether `mob` may target `target` (`asValidTarget` with `canAttack`): a living mob that can
/// be attacked or a living player in survival or adventure mode, not itself.
pub fn valid_target(ents: &Entities, mob: EntityId, target: EntityId) -> bool {
    if target == mob {
        return false;
    }
    if let Some(p) = ents.player(target) {
        return !p.survival.is_dead_or_dying()
            && matches!(p.game_mode, GameMode::Survival | GameMode::Adventure);
    }
    ents.mob(target)
        .is_some_and(|m| m.is_alive() && crate::decoration::stand::attackable(m))
}

/// `Mob.setTarget` with the plugins' target event ([`event::ENTITY_TARGET`], `reason` from
/// `target_reason`): brain mobs take it as their attack target memory. Returns `false` when a
/// plugin cancelled it (the mob keeps its target) or `mob` is no mob. `hooks` are the level's
/// hooks for the event, if a plugin listens.
pub fn change_target(
    hooks: Option<&(dyn GameHooks + Send)>,
    ents: &mut Entities,
    mob: EntityId,
    target: Option<EntityId>,
    reason: u32,
) -> bool {
    if ents.mob(mob).is_none() {
        return false;
    }
    if let Some(hooks) = hooks {
        let named = |id: EntityId| {
            ents.store
                .get(id)
                .map(|e| (e.uuid(), e.kind().name().to_owned()))
        };
        let (entity, entity_type) = named(mob).unwrap_or_default();
        let (other, other_type) = target.and_then(named).unwrap_or_default();
        let mut ev = crate::hooks::Event {
            entity,
            entity_type,
            other,
            other_type,
            cause: reason,
            ..crate::hooks::Event::new(event::ENTITY_TARGET)
        };
        hooks.event(crate::hooks::HookWorld::entities(ents), &mut ev);
        if ev.cancelled {
            return false;
        }
    }
    let Some(m) = ents.mob_mut(mob) else {
        return false;
    };
    if let Some(b) = m.brain.as_deref_mut()
        && b.is_registered(MemoryType::AttackTarget)
    {
        b.set_opt(
            MemoryType::AttackTarget,
            target.map(crate::mob::brain::MemValue::Entity),
        );
    }
    super::assign_target(m, target);
    true
}

/// The plugin goal flags as `Goal.Flag` bits: the ABI's `goal_flag` has the same bits.
const _: () = assert!(
    goal_flag::MOVE == 1 && goal_flag::LOOK == 2 && goal_flag::JUMP == 4 && goal_flag::TARGET == 8
);
