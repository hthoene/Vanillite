//! The activation range (`performance.activation-range.*`, off in the Vanilla profile): mobs
//! farther than their group's range from every player are inactive. An inactive mob gets a
//! full tick only every 20th tick (staggered by its network id); in between it only ages
//! ([`age`]) and, with the goal selector throttle, chooses goals now and then ([`super::think`]).
//!
//! Immunities keep mobs active that are busy with something a player may care about: fresh
//! mobs, leashed, burning or just hurt ones at once; mobs in water, falling, riding or ridden,
//! with a target, recently attacked, in love, and panicking or working villagers from the
//! check every 20 ticks of inactivity. The wake-up plan lets a capped number of long inactive
//! mobs per group and tick run fully for a while, so breeders and villages still progress.
//!
//! The players come from a [`NearbyPlayers`] index of the world (or region) by chunk, so a
//! mob's check costs one chunk lookup and the players around it.

use std::sync::OnceLock;

use vanillite_base::Vec3;
use vanillite_world::entity::{EntityFlags, EntityId};

use super::brain::Activity;
use super::nearby::NearbyPlayers;
use super::spawn::Category;
use super::{Kind, MobData};
use crate::entities::Entities;

/// The activation group of a mob kind: each has its own range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Animals,
    Monsters,
    Raiders,
    Misc,
    Water,
    Villagers,
    Flying,
}

impl Group {
    /// Number of groups.
    pub const COUNT: usize = 7;

    /// The group of a kind; `None` for the kinds that always tick (the bosses).
    pub fn of(kind: Kind) -> Option<Group> {
        static GROUPS: OnceLock<Vec<Option<Group>>> = OnceLock::new();
        GROUPS
            .get_or_init(|| Kind::ALL.iter().map(|&k| Group::compute(k)).collect())
            .get(kind as usize)
            .copied()
            .flatten()
    }

    fn compute(kind: Kind) -> Option<Group> {
        Some(match kind {
            Kind::Wither | Kind::EnderDragon => return None,
            Kind::Villager | Kind::WanderingTrader => Group::Villagers,
            Kind::Ghast | Kind::Phantom | Kind::Blaze | Kind::Vex => Group::Flying,
            k if k.is_raider() => Group::Raiders,
            k => match Category::of(k.type_id()) {
                Some(
                    Category::WaterCreature
                    | Category::WaterAmbient
                    | Category::UndergroundWaterCreature
                    | Category::Axolotls,
                ) => Group::Water,
                Some(Category::Monster) => Group::Monsters,
                Some(Category::Creature | Category::Ambient) => Group::Animals,
                None if k.extends_monster() => Group::Monsters,
                None => Group::Misc,
            },
        })
    }
}

/// The wake-up plan of one group (`wake-up-inactive.<group>-*`): one wake-up per tick on
/// average, up to `max_per_tick` in one tick when earlier ticks woke none.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WakeUp {
    /// Mobs of the group woken up in one tick at most; 0 = none.
    pub max_per_tick: u16,
    /// Ticks a mob must have been inactive before it may be woken up.
    pub every: u32,
    /// Ticks a woken up mob stays active.
    pub ticks: u32,
}

/// The activation range rules of a world. The default is off (Vanilla).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActivationSettings {
    /// Range in blocks by [`Group`]; 0 = the group is always active.
    pub ranges: [u16; Group::COUNT],
    /// The wake-up plan by [`Group`].
    pub wake_up: [WakeUp; Group::COUNT],
    /// Ticks of inactivity after which a working villager becomes active; 0 = never.
    pub villager_work_after: u32,
    /// Ticks a working villager then stays active.
    pub villager_work_for: u32,
    /// Panicking villagers stay active.
    pub villager_panic: bool,
    /// Ticks between the goal and target selector runs of an inactive mob between its full
    /// ticks; `None` = none.
    pub goal_throttle: Option<u32>,
}

/// Largest range: the index around a player reaches 8 chunks.
pub const MAX_RANGE: u16 = 128;

impl ActivationSettings {
    /// Whether any group has a range.
    pub fn enabled(&self) -> bool {
        self.ranges.iter().any(|&r| r > 0)
    }
}

/// What a mob does this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// The full tick.
    Active,
    /// Only [`age`].
    Inactive,
    /// [`super::think`]: ageing and the goal and target selectors.
    Think,
}

/// Ticks a mob stays immune after it was created or loaded (`tickCount`).
const FRESH_TICKS: i32 = 200;
/// Ticks between the full ticks of an inactive mob (and its immunity checks).
const INACTIVE_INTERVAL: u64 = 20;

/// The activation state of a world (or a region): the players by chunk and the wake-up
/// budgets of the tick.
#[derive(Debug)]
pub struct Activation {
    settings: ActivationSettings,
    nearby: NearbyPlayers,
    budget: [u16; Group::COUNT],
    now: u64,
}

impl Default for Activation {
    fn default() -> Self {
        Activation {
            settings: ActivationSettings::default(),
            nearby: NearbyPlayers::new(0, false),
            budget: [0; Group::COUNT],
            now: 0,
        }
    }
}

impl Activation {
    /// Starts world tick `now` with the players that activate mobs (no spectators).
    pub fn begin_tick(
        &mut self,
        settings: ActivationSettings,
        now: u64,
        players: &[(EntityId, Vec3)],
    ) {
        self.settings = settings;
        self.now = now;
        if !settings.enabled() {
            if !self.nearby.is_empty() {
                self.nearby.update(&[]);
            }
            return;
        }
        let max = settings.ranges.iter().copied().max().unwrap_or(0);
        self.nearby
            .set_radius(i32::from(max.min(MAX_RANGE)) / 16 + 1);
        self.nearby.update(players);
        // One more wake-up per tick, saved up to the maximum: about one per tick on average.
        for (budget, wake) in self.budget.iter_mut().zip(&settings.wake_up) {
            *budget = budget.saturating_add(1).min(wake.max_per_tick);
        }
    }

    /// Whether the activation range is on this tick.
    #[inline]
    pub fn enabled(&self) -> bool {
        self.settings.enabled()
    }

    /// What mob `id` does this tick.
    pub fn check(&mut self, ents: &mut Entities, id: EntityId) -> Decision {
        let now = self.now;
        let Some(e) = ents.store.get(id) else {
            return Decision::Active;
        };
        let (pos, velocity, flags) = (e.pos(), e.velocity(), e.flags());
        let phase = u64::from(e.network_id().0.unsigned_abs());
        let leashed = ents.leashes.contains_key(&id);
        let riding =
            ents.riding.vehicle_of(id).is_some() || !ents.riding.passengers_of(id).is_empty();
        let Some(m) = ents.mob_mut(id) else {
            return Decision::Active;
        };
        let Some(group) = Group::of(m.kind) else {
            return Decision::Active;
        };
        let range = self.settings.ranges[group as usize].min(MAX_RANGE);
        if range == 0 {
            return Decision::Active;
        }
        if now <= m.active_until {
            return Decision::Active;
        }
        if m.tick_count < FRESH_TICKS
            || !m.is_alive()
            || m.hurt_time > 0
            || m.fire_ticks > 0
            || leashed
            || self.nearby.any_within_xz(pos, f64::from(range))
        {
            m.active_until = now;
            return Decision::Active;
        }
        if (now + phase).is_multiple_of(INACTIVE_INTERVAL) {
            let inactive_for = now - m.active_until;
            if let Some(ticks) = self.immunity(m, group, flags, velocity, riding, inactive_for) {
                m.active_until = now + ticks - 1;
                return Decision::Active;
            }
            let wake = self.settings.wake_up[group as usize];
            let budget = &mut self.budget[group as usize];
            if *budget > 0 && wake.ticks > 0 && inactive_for >= u64::from(wake.every) {
                *budget -= 1;
                m.active_until = now + u64::from(wake.ticks) - 1;
            }
            // The full tick of an inactive mob.
            return Decision::Active;
        }
        match self.settings.goal_throttle {
            Some(n) if n > 0 && (now + phase).is_multiple_of(u64::from(n)) => Decision::Think,
            _ => Decision::Inactive,
        }
    }

    /// Ticks an inactive mob stays active for what it is doing; `None`: it is idle.
    fn immunity(
        &self,
        m: &MobData,
        group: Group,
        flags: EntityFlags,
        velocity: Vec3,
        riding: bool,
        inactive_for: u64,
    ) -> Option<u64> {
        let until_next_check = Some(INACTIVE_INTERVAL);
        let falling = !flags.contains(EntityFlags::ON_GROUND) && velocity.y < 0.0;
        let attacked = (m.last_hurt_by_mob.is_some()
            && m.tick_count - m.last_hurt_by_mob_time < 100)
            || (m.last_hurt_by_player.is_some() && m.last_hurt_by_player_time > 0);
        if (group != Group::Water && m.fluids.in_water())
            || (falling && group != Group::Flying)
            || riding
            || m.target.is_some()
            || attacked
            || m.in_love > 0
        {
            return until_next_check;
        }
        if group == Group::Villagers
            && let Some(brain) = &m.brain
        {
            if self.settings.villager_panic && brain.is_active(Activity::Panic) {
                return Some(100);
            }
            let s = &self.settings;
            if s.villager_work_after > 0
                && s.villager_work_for > 0
                && inactive_for >= u64::from(s.villager_work_after)
                && brain.is_active(Activity::Work)
            {
                return Some(u64::from(s.villager_work_for));
            }
        }
        None
    }
}

/// What an inactive mob does every tick: `tickCount` and the age of animals go on, so babies
/// grow up and breeding cooldowns end in time (the change of size waits for the next full
/// tick).
pub fn age(m: &mut MobData) {
    m.tick_count += 1;
    if m.kind.ageable() && m.is_alive() {
        if m.age < 0 && !m.age_locked {
            m.age += 1;
        } else if m.age > 0 {
            m.age -= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use vanillite_world::entity::{NetworkId, NewEntity};

    use super::super::brain;
    use super::*;

    fn spawn(ents: &mut Entities, kind: Kind, nid: i32, x: f64) -> EntityId {
        let mut new = NewEntity::new(kind.type_id(), NetworkId(nid), 0, Vec3::new(x, 64.0, 0.0));
        new.flags = EntityFlags::ON_GROUND;
        let mut m = MobData::new(kind, 1);
        m.tick_count = 1000;
        ents.spawn_mob(new, m)
    }

    fn settings() -> ActivationSettings {
        let mut s = ActivationSettings {
            ranges: [32; Group::COUNT],
            villager_work_after: 100,
            villager_work_for: 20,
            villager_panic: true,
            ..ActivationSettings::default()
        };
        s.wake_up[Group::Animals as usize] = WakeUp {
            max_per_tick: 1,
            every: 1200,
            ticks: 100,
        };
        s
    }

    /// Active ticks of each of `ids` over ticks `from..from + 20`, starting each tick with one
    /// player at the origin 60 blocks up (any id: the index only keys by it).
    fn run(a: &mut Activation, ents: &mut Entities, ids: &[EntityId], from: u64) -> Vec<usize> {
        let player = [(ids[0], Vec3::new(0.0, 124.0, 0.0))];
        let mut active = vec![0; ids.len()];
        for now in from..from + 20 {
            a.begin_tick(settings(), now, &player);
            for (i, &id) in ids.iter().enumerate() {
                if a.check(ents, id) == Decision::Active {
                    active[i] += 1;
                }
            }
        }
        active
    }

    /// Mobs in range tick fully at any height; idle far ones once per 20 ticks; far animals in
    /// love, leashed mobs and working villagers stay active; the wake-up plan wakes one far
    /// animal per tick.
    #[test]
    fn farms_and_villagers_keep_working() {
        let mut ents = Entities::default();
        let near = spawn(&mut ents, Kind::Cow, 1, 20.0);
        let far = spawn(&mut ents, Kind::Cow, 2, 200.0);
        let in_love = spawn(&mut ents, Kind::Cow, 3, 210.0);
        let leashed = spawn(&mut ents, Kind::Sheep, 4, 220.0);
        let villager = spawn(&mut ents, Kind::Villager, 5, 230.0);
        ents.mob_mut(in_love).unwrap().in_love = 600;
        ents.leashes.insert(leashed, near);
        let mut brain = brain::Brain::default();
        brain.set_default_activity(Activity::Work);
        brain.use_default_activity();
        ents.mob_mut(villager).unwrap().brain = Some(Box::new(brain));
        let ids = [near, far, in_love, leashed, villager];
        let mut a = Activation::default();
        run(&mut a, &mut ents, &ids, 1000);
        // The villager works again only after 100 inactive ticks: 15 + its full tick.
        assert_eq!(run(&mut a, &mut ents, &ids, 1020), [20, 1, 20, 20, 16]);
        // Two far animals inactive for longer than `every` with the same phase: one wake-up
        // per tick.
        let pig = spawn(&mut ents, Kind::Pig, 22, 240.0);
        assert_eq!(run(&mut a, &mut ents, &[far, pig], 2400), [2, 1]);
        assert_eq!(ents.mob(far).unwrap().active_until, 2418 + 99);
        assert_eq!(ents.mob(pig).unwrap().active_until, 0);
    }
}
