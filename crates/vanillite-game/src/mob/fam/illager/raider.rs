//! `PatrollingMonster`, `Raider` and `AbstractIllager` basics (patrols, captains, holding
//! ground, retaliating) shared by the raiders, the family's generic goals (`RandomStrollGoal`,
//! `LookAtPlayerGoal` for mobs) and the vindicator.

use vanillite_base::random::RandomSource;
use vanillite_base::{Aabb, BlockPos, Vec3};
use vanillite_chunk::HeightmapKind;
use vanillite_nbt::{Compound, Tag};
use vanillite_world::entity::EntityId;

use super::super::super::goals::{self, Goal as MobGoal, MeleeKind, TargetState};
use super::super::super::kinds::Kind;
use super::super::super::living::has_line_of_sight;
use super::super::super::{MobData, Tick, equipment, nav, randompos, spawn};
use super::super::{SpawnContext, SpawnReason};
use super::{
    Gate, Goal, avoid_creaking, dirty, fam, is_illager, is_raider, look_at_player, st, st_mut,
    target_mobs, target_players,
};
use crate::item::{EquipmentSlot, ItemStack};
use crate::survival::rules::{Difficulty, Rule};

/// `LongDistancePatrolGoal` state.
#[derive(Clone, Copy, Debug)]
pub struct Patrol {
    pub speed: f64,
    pub leader_speed: f64,
    pub cooldown_until: i64,
}

/// Whose hits a [`HurtBy`] ignores.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ignore {
    /// `Raider.class`.
    Raiders,
    /// Mobs of one kind (`this.getClass()`).
    Kind(Kind),
}

/// `HurtByTargetGoal(mob, ignore...)`.
#[derive(Clone, Copy, Debug)]
pub struct HurtBy {
    pub state: TargetState,
    pub timestamp: i32,
    pub alert: bool,
    pub ignore: Ignore,
}

/// Which mobs a [`LookAt`] looks at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LookFilter {
    /// `Mob.class`.
    AnyMob,
    /// `Guardian.class`.
    Guardian,
}

/// `LookAtPlayerGoal(mob, Mob.class or Guardian.class, distance, probability)`.
#[derive(Clone, Copy, Debug)]
pub struct LookAt {
    pub distance: f32,
    pub probability: f32,
    pub filter: LookFilter,
    pub look_at: Option<EntityId>,
    pub time: i32,
}

/// `RandomStrollGoal(mob, speed, interval)`.
#[derive(Clone, Copy, Debug)]
pub struct Stroll {
    pub speed: f64,
    pub interval: i32,
    pub wanted: Vec3,
    /// Also locks looking (the guardian's stroll).
    pub look: bool,
}

/// `LongDistancePatrolGoal(mob, 0.7, 0.595)`.
pub fn patrol() -> MobGoal {
    fam(Goal::Patrol(Patrol {
        speed: 0.7,
        leader_speed: 0.595,
        cooldown_until: -1,
    }))
}

pub fn hurt_by(ignore: Ignore, alert: bool) -> MobGoal {
    fam(Goal::HurtBy(HurtBy {
        state: TargetState::new(false),
        timestamp: 0,
        alert,
        ignore,
    }))
}

pub fn look_at_mobs(distance: f32, probability: f32, filter: LookFilter) -> MobGoal {
    fam(Goal::LookAt(LookAt {
        distance,
        probability,
        filter,
        look_at: None,
        time: 0,
    }))
}

pub fn stroll(speed: f64, interval: i32) -> MobGoal {
    fam(Goal::Stroll(Stroll {
        speed,
        interval,
        wanted: Vec3::ZERO,
        look: false,
    }))
}

// --- Spawning -------------------------------------------------------------------------------

/// `Raider.finalizeSpawn` and `PatrollingMonster.finalizeSpawn`: the raid flag, captains with
/// the ominous banner, patrolling for patrol spawns.
pub fn finalize_spawn(m: &mut MobData, ctx: &mut SpawnContext<'_>) {
    let kind = m.kind;
    st_mut(m).can_join_raid = kind != Kind::Witch || ctx.reason != SpawnReason::Natural;
    let can_be_leader = !matches!(kind, Kind::Witch | Kind::Ravager);
    if !matches!(
        ctx.reason,
        SpawnReason::Patrol | SpawnReason::Event | SpawnReason::Structure
    ) && m.random.next_float() < 0.06
        && can_be_leader
    {
        st_mut(m).patrol_leader = true;
    }
    if st(m).patrol_leader {
        m.equipment.set(EquipmentSlot::Head, ominous_banner());
        m.equipment.set_drop_chance(EquipmentSlot::Head, 2.0);
    }
    if ctx.reason == SpawnReason::Patrol {
        st_mut(m).patrolling = true;
    }
}

/// `Raid.getOminousBannerInstance` without its patterns (the banner pattern registry is data
/// driven): a white banner named "Ominous Banner", uncommon.
pub fn ominous_banner() -> ItemStack {
    let mut s = equipment::stack("minecraft:white_banner");
    if s.is_empty() {
        return s;
    }
    let reg = &vanillite_data::registry::DATA_COMPONENT_TYPE;
    let mut name = Compound::new();
    name.insert("translate", "block.minecraft.ominous_banner");
    if let (Some(id), Some(value)) = (
        reg.id("minecraft:item_name"),
        super::super::super::nbt::tag_component(&Tag::Compound(name)),
    ) {
        s.patch_mut().set(id, value);
    }
    if let Some(id) = reg.id("minecraft:rarity") {
        s.patch_mut().set(id, Box::new([1u8]));
    }
    s
}

/// Whether a custom name (network NBT) reads "Johnny".
pub fn is_johnny_name(encoded: &[u8]) -> bool {
    match super::super::super::nbt::component_tag(encoded) {
        Some(Tag::String(s)) => s == "Johnny",
        Some(Tag::Compound(c)) => {
            c.get("text").and_then(Tag::as_str) == Some("Johnny") && c.get("extra").is_none()
        }
        _ => false,
    }
}

// --- Vindicator -----------------------------------------------------------------------------

/// `Vindicator.registerGoals`.
pub fn register_vindicator(m: &mut MobData) {
    let g = &mut m.goals;
    let tg = &mut m.targets;
    g.add(4, patrol());
    add_raider_goals(g);
    g.add(0, MobGoal::Float);
    g.add(1, avoid_creaking(1.0, 1.2));
    g.add(
        2,
        fam(Goal::RaidDoor {
            inner: door_goal(super::super::super::movement::MoveGoal::break_door_seconds(
                6,
                |d| matches!(d, Difficulty::Normal | Difficulty::Hard),
            )),
            breaker: true,
        }),
    );
    g.add(
        3,
        fam(Goal::RaidDoor {
            inner: door_goal(super::super::super::movement::MoveGoal::open_door(false)),
            breaker: false,
        }),
    );
    g.add(4, fam(Goal::HoldGround { radius_sqr: 100.0 }));
    g.add(5, MobGoal::melee(MeleeKind::Plain, 1.0, false));
    tg.add(1, hurt_by(Ignore::Raiders, true));
    tg.add(2, target_players(true));
    tg.add(3, target_mobs(true, super::super::super::common::villager));
    tg.add(
        3,
        target_mobs(true, super::super::super::common::iron_golem),
    );
    tg.add(
        4,
        fam(Goal::Target {
            inner: super::super::super::common::CommonGoal::nearest(0, true, |t, id| {
                // `target.attackable()`: any living entity.
                t.ents.player(id).is_some() || t.ents.mob(id).is_some()
            }),
            gate: Gate::Johnny,
        }),
    );
    g.add(8, stroll(0.6, 120));
    g.add(9, look_at_player(3.0, 1.0));
    g.add(10, look_at_mobs(8.0, 0.02, LookFilter::AnyMob));
}

/// `Vindicator.finalizeSpawn`: doors and the iron axe.
pub fn finalize_vindicator(m: &mut MobData, ctx: &mut SpawnContext<'_>) {
    m.nav.can_open_doors = true;
    m.equipment.set(
        EquipmentSlot::MainHand,
        equipment::stack("minecraft:iron_axe"),
    );
    equipment::populate_default_enchantments(m, ctx.special_multiplier);
}

// --- LongDistancePatrolGoal ------------------------------------------------------------------

pub fn patrol_can_use(t: &mut Tick<'_, '_>, p: &Patrol) -> bool {
    let s = st(t.mob_ref());
    let on_cooldown = t.env.game_time < p.cooldown_until;
    s.patrolling && t.mob_ref().target.is_none() && s.patrol_target.is_some() && !on_cooldown
}

/// The patrol companions within 16 blocks (`canJoinPatrol`: raiders without an active raid).
fn patrol_companions(t: &mut Tick<'_, '_>, out: &mut Vec<EntityId>) {
    let area = t.aabb().inflate(16.0, 16.0, 16.0);
    let mut ids = std::mem::take(&mut t.cx.ids);
    ids.clear();
    t.ents.store.for_each_in(&area, false, |id| ids.push(id));
    for &id in &ids {
        if id != t.id
            && t.ents.mob(id).is_some_and(|m| {
                is_raider(m.kind) && m.is_alive() && !crate::raid::has_active_raid(t.ents, m)
            })
        {
            out.push(id);
        }
    }
    t.cx.ids = ids;
}

fn heightmap_pos(t: &Tick<'_, '_>, x: i32, z: i32) -> BlockPos {
    let y = spawn::surface_y(t.level, HeightmapKind::MotionBlockingNoLeaves, x, z);
    BlockPos::new(x, y, z)
}

pub fn patrol_tick(t: &mut Tick<'_, '_>, p: &mut Patrol) {
    let leader = st(t.mob_ref()).patrol_leader;
    if !t.mob_ref().nav.is_done() {
        return;
    }
    let mut companions = Vec::new();
    patrol_companions(t, &mut companions);
    let Some(target) = st(t.mob_ref()).patrol_target else {
        return;
    };
    let pos = t.pos();
    let center = |b: BlockPos| {
        Vec3::new(
            f64::from(b.x) + 0.5,
            f64::from(b.y) + 0.5,
            f64::from(b.z) + 0.5,
        )
    };
    if st(t.mob_ref()).patrolling && companions.is_empty() {
        st_mut(t.mob()).patrolling = false;
    } else if leader && center(target).distance_to_sqr(pos) < 100.0 {
        find_patrol_target(t);
    } else {
        let long = randompos::bottom_center(target);
        let d = pos - long;
        // `distance.yRot(90.0F)`: 90 radians, as Vanilla passes it.
        let (cos, sin) = (
            f64::from(vanillite_base::math::cos(f64::from(90.0_f32))),
            f64::from(vanillite_base::math::sin(f64::from(90.0_f32))),
        );
        let rotated = Vec3::new(d.x * cos + d.z * sin, d.y, d.z * cos - d.x * sin);
        let long = rotated * 0.4 + long;
        let move_target = (long - pos).normalize() * 10.0 + pos;
        let b = BlockPos::containing(move_target.x, move_target.y, move_target.z);
        let path_target = heightmap_pos(t, b.x, b.z);
        let speed = if leader { p.leader_speed } else { p.speed };
        let at = Vec3::new(
            f64::from(path_target.x),
            f64::from(path_target.y),
            f64::from(path_target.z),
        );
        if !nav::move_to_pos(t, at, speed) {
            // `moveRandomly`.
            let r = &mut t.mob().random;
            let (dx, dz) = (-8 + r.next_int_bounded(16), -8 + r.next_int_bounded(16));
            let here = t.block_pos();
            let rp = heightmap_pos(t, here.x + dx, here.z + dz);
            let at = Vec3::new(f64::from(rp.x), f64::from(rp.y), f64::from(rp.z));
            nav::move_to_pos(t, at, p.speed);
            p.cooldown_until = t.env.game_time + 200;
        } else if leader {
            for id in companions {
                if let Some(m) = t.ents.mob_mut(id) {
                    let s = st_mut(m);
                    s.patrol_target = Some(path_target);
                    s.patrolling = true;
                }
            }
        }
    }
}

/// `findPatrolTarget`: a point up to 500 blocks away.
pub fn find_patrol_target(t: &mut Tick<'_, '_>) {
    let here = t.block_pos();
    let r = &mut t.mob().random;
    let (dx, dz) = (
        -500 + r.next_int_bounded(1000),
        -500 + r.next_int_bounded(1000),
    );
    let s = st_mut(t.mob());
    s.patrol_target = Some(BlockPos::new(here.x + dx, here.y, here.z + dz));
    s.patrolling = true;
}

// --- HoldGroundAttackGoal --------------------------------------------------------------------

pub fn hold_ground_can_use(t: &mut Tick<'_, '_>) -> bool {
    let m = t.mob_ref();
    let by_player = m
        .last_hurt_by_mob
        .is_some_and(|by| t.ents.player(by).is_some());
    st(m).raid.is_none() && st(m).patrolling && m.target.is_some() && !m.aggressive && !by_player
}

/// The raiders within 8 blocks (`getNearbyEntities(Raider.class, shoutTargeting, ...)`).
fn nearby_raiders(t: &mut Tick<'_, '_>, out: &mut Vec<EntityId>) {
    let area: Aabb = t.aabb().inflate(8.0, 8.0, 8.0);
    let p = t.pos();
    let mut ids = std::mem::take(&mut t.cx.ids);
    ids.clear();
    t.ents.store.for_each_in(&area, false, |id| ids.push(id));
    for &id in &ids {
        if id == t.id {
            continue;
        }
        let ok = t
            .ents
            .mob(id)
            .is_some_and(|m| is_raider(m.kind) && m.is_alive())
            && t.ents
                .store
                .get(id)
                .is_some_and(|e| e.pos().distance_to_sqr(p) <= 64.0);
        if ok {
            out.push(id);
        }
    }
    t.cx.ids = ids;
}

pub fn hold_ground_start(t: &mut Tick<'_, '_>) {
    t.mob().nav.stop();
    let target = t.mob_ref().target;
    let mut raiders = Vec::new();
    nearby_raiders(t, &mut raiders);
    for id in raiders {
        if let Some(m) = t.ents.mob_mut(id) {
            m.target = target;
        }
    }
}

pub fn hold_ground_stop(t: &mut Tick<'_, '_>) {
    let Some(target) = t.mob_ref().target else {
        return;
    };
    let mut raiders = Vec::new();
    nearby_raiders(t, &mut raiders);
    for id in raiders {
        if let Some(m) = t.ents.mob_mut(id) {
            m.target = Some(target);
            if !m.aggressive {
                m.aggressive = true;
                if let Some(mut e) = t.ents.store.get_mut(id) {
                    e.mark_data_dirty();
                }
            }
        }
    }
    goals::set_aggressive(t, true);
}

pub fn hold_ground_tick(t: &mut Tick<'_, '_>, radius_sqr: f32) {
    let Some(target) = t.mob_ref().target else {
        return;
    };
    let Some(o) = t.other(target) else {
        return;
    };
    if t.pos().distance_to_sqr(o.pos) > f64::from(radius_sqr) {
        t.mob()
            .look
            .set_look_at(Vec3::new(o.pos.x, o.eye_y, o.pos.z), 30.0, 30.0);
        if t.mob().random.next_int_bounded(50) == 0 {
            super::super::super::kinds::play_ambient_sound(t);
        }
    } else {
        goals::set_aggressive(t, true);
    }
}

// --- HurtByTargetGoal ------------------------------------------------------------------------

fn ignored(t: &Tick<'_, '_>, ignore: Ignore, by: EntityId) -> bool {
    let Some(m) = t.ents.mob(by) else {
        return false;
    };
    match ignore {
        Ignore::Raiders => is_raider(m.kind),
        Ignore::Kind(k) => m.kind == k,
    }
}

pub fn hurt_by_can_use(t: &mut Tick<'_, '_>, h: &HurtBy) -> bool {
    let m = t.mob_ref();
    let (Some(by), stamp) = (m.last_hurt_by_mob, m.last_hurt_by_mob_time) else {
        return false;
    };
    if stamp == h.timestamp {
        return false;
    }
    if t.ents.player(by).is_some() && t.env.rules.bool(Rule::UNIVERSAL_ANGER) {
        return false;
    }
    if ignored(t, h.ignore, by) {
        return false;
    }
    if t.ents.player(by).is_some() {
        goals::test_player(t, by, true, -1.0, false)
    } else {
        goals::can_attack(t, by) && !allied(t, by)
    }
}

/// `isAlliedTo` without teams: illagers stand together.
pub fn allied(t: &Tick<'_, '_>, other: EntityId) -> bool {
    let own = t.mob_ref().kind;
    t.ents
        .mob(other)
        .is_some_and(|m| is_illager(own) && is_illager(m.kind))
}

pub fn hurt_by_start(t: &mut Tick<'_, '_>, h: &mut HurtBy) {
    let by = t.mob_ref().last_hurt_by_mob;
    goals::set_target(t, by);
    h.state.target_mob = by;
    h.timestamp = t.mob_ref().last_hurt_by_mob_time;
    h.state.unseen_memory = 300;
    if h.alert
        && let Some(by) = by
    {
        alert_others(t, by);
    }
    h.state.unseen_ticks = 0;
}

/// `alertOthers`: mobs of the same kind without a target take it over.
fn alert_others(t: &mut Tick<'_, '_>, by: EntityId) {
    let range = t.mob_ref().attrs.follow_range;
    let p = t.pos();
    let area =
        Aabb::new(p.x, p.y, p.z, p.x + 1.0, p.y + 1.0, p.z + 1.0).inflate(range, 10.0, range);
    let kind = t.mob_ref().kind;
    let by_illager = t.ents.mob(by).is_some_and(|m| is_illager(m.kind));
    let mut ids = std::mem::take(&mut t.cx.ids);
    ids.clear();
    t.ents.store.for_each_in(&area, false, |id| ids.push(id));
    for &id in &ids {
        if id == t.id {
            continue;
        }
        if let Some(m) = t.ents.mob_mut(id)
            && m.kind == kind
            && m.target.is_none()
            && m.is_alive()
            && !(is_illager(kind) && by_illager)
        {
            m.target = Some(by);
        }
    }
    t.cx.ids = ids;
}

// --- LookAtPlayerGoal for mobs -----------------------------------------------------------------

fn looks_at(t: &Tick<'_, '_>, filter: LookFilter, id: EntityId) -> bool {
    t.ents.mob(id).is_some_and(|m| {
        m.is_alive()
            && match filter {
                LookFilter::AnyMob => true,
                LookFilter::Guardian => m.kind == Kind::Guardian,
            }
    })
}

pub fn look_at_can_use(t: &mut Tick<'_, '_>, l: &mut LookAt) -> bool {
    if t.mob().random.next_float() >= l.probability {
        return false;
    }
    if t.mob_ref().target.is_some() {
        l.look_at = t.mob_ref().target;
    }
    let d = f64::from(l.distance);
    let area = t.aabb().inflate(d, 3.0, d);
    let p = t.pos();
    let at = Vec3::new(p.x, t.eye_y(), p.z);
    let mut ids = std::mem::take(&mut t.cx.ids);
    ids.clear();
    t.ents.store.for_each_in(&area, false, |id| ids.push(id));
    let mut best: Option<(EntityId, f64)> = None;
    for &id in &ids {
        if id == t.id || !looks_at(t, l.filter, id) {
            continue;
        }
        let Some(e) = t.ents.store.get(id) else {
            continue;
        };
        let pos = e.pos();
        let invisible = t
            .ents
            .mob(id)
            .is_some_and(|m| m.effects.has(crate::effect::MobEffect::INVISIBILITY));
        let visible = (d * if invisible { 0.07 } else { 1.0 }).max(2.0);
        if p.distance_to_sqr(pos) > visible * visible || !has_line_of_sight(t, id) {
            continue;
        }
        let dist = pos.distance_to_sqr(at);
        if best.is_none_or(|(_, b)| dist < b) {
            best = Some((id, dist));
        }
    }
    t.cx.ids = ids;
    l.look_at = best.map(|(id, _)| id);
    l.look_at.is_some()
}

pub fn look_at_can_continue(t: &mut Tick<'_, '_>, l: &LookAt) -> bool {
    let Some(o) = l.look_at.and_then(|id| t.other(id)) else {
        return false;
    };
    let d = f64::from(l.distance);
    t.pos().distance_to_sqr(o.pos) <= d * d && l.time > 0
}

pub fn look_at_tick(t: &mut Tick<'_, '_>, l: &mut LookAt) {
    if let Some(o) = l.look_at.and_then(|id| t.other(id)) {
        nav::look_at(t, Vec3::new(o.pos.x, o.eye_y, o.pos.z));
        l.time -= 1;
    }
}

// --- RandomStrollGoal --------------------------------------------------------------------------

pub fn stroll_can_use(t: &mut Tick<'_, '_>, s: &mut Stroll) -> bool {
    let forced = st(t.mob_ref()).stroll_triggered;
    if !forced {
        if t.mob_ref().no_action_time >= 100 {
            return false;
        }
        if t.mob()
            .random
            .next_int_bounded(goals::reduced(s.interval).max(1))
            != 0
        {
            return false;
        }
    }
    let Some(pos) = randompos::default_pos(t, 10, 7) else {
        return false;
    };
    s.wanted = pos;
    st_mut(t.mob()).stroll_triggered = false;
    true
}

/// Marks the data dirty after a raider state change.
pub fn set_celebrating(t: &mut Tick<'_, '_>, on: bool) {
    if st(t.mob_ref()).celebrating != on {
        st_mut(t.mob()).celebrating = on;
        dirty(t);
    }
}

// --- Raids -------------------------------------------------------------------------------------

/// `Raider.registerGoals` after `PatrollingMonster`'s patrol goal: the banner, the way to the
/// raid, moving through the village and the celebration.
pub fn add_raider_goals(g: &mut goals::GoalSelector) {
    g.add(1, fam(Goal::ObtainBanner { banner: None }));
    g.add(3, fam(Goal::PathfindToRaid { recruitment_tick: 0 }));
    g.add(4, fam(Goal::MoveThroughVillage(MoveThroughVillage::new())));
    g.add(5, fam(Goal::Celebration));
}

/// The shared goal inside a movement goal constructor.
fn door_goal(g: MobGoal) -> super::super::super::common::CommonGoal {
    match g {
        MobGoal::Common(c) => c,
        _ => unreachable!("the door goals are shared goals"),
    }
}

/// `Raider.aiStep` before `Mob.aiStep`: a raider that may join one joins the raid around it
/// every second; raiders of a raid fighting players or golems are not idle.
pub fn ai_step(t: &mut Tick<'_, '_>) {
    if !t.mob_ref().is_alive() || !st(t.mob_ref()).can_join_raid {
        return;
    }
    match st(t.mob_ref()).raid {
        None => {
            if t.env.game_time % 20 == 0
                && let Some((id, raid)) = t
                    .ents
                    .raids
                    .nearby_raid(t.block_pos(), crate::raid::VALID_RAID_RADIUS_SQR)
                && crate::raid::can_join_raid(t.mob_ref())
            {
                let wave = raid.groups_spawned;
                let me = t.id;
                crate::raid::join_existing(t.ents, id, wave, me, t.env.out);
            }
        }
        Some(_) => {
            let fights = t.mob_ref().target.is_some_and(|target| {
                t.ents.player(target).is_some()
                    || t.ents
                        .mob(target)
                        .is_some_and(|m| m.kind == Kind::IronGolem)
            });
            if fights {
                t.mob().no_action_time = 0;
            }
        }
    }
}

/// `ItemStack.matches(stack, Raid.getOminousBannerInstance())`.
pub fn is_ominous_banner(stack: &ItemStack) -> bool {
    let banner = ominous_banner();
    !stack.is_empty() && stack.same_item_same_components(&banner) && stack.count == banner.count
}

/// `Raider.pickUpItem` (`Pillager.pickUpItem` sends banners here): an ominous banner makes a
/// raider of a raid without a leader its wave's leader; other items are equipped as usual.
pub fn pick_up_item(t: &mut Tick<'_, '_>, item: EntityId) -> bool {
    let Some(stack) = t.ents.item(item).map(|d| d.stack.clone()) else {
        return false;
    };
    let active = crate::raid::has_active_raid(t.ents, t.mob_ref());
    let wave = st(t.mob_ref()).wave;
    let raid_id = st(t.mob_ref()).raid;
    let has_leader = raid_id
        .and_then(|id| t.ents.raids.get(id))
        .is_some_and(|r| r.leader(wave).is_some());
    if active && !has_leader && is_ominous_banner(&stack) {
        let current = t.mob_ref().equipment.get(EquipmentSlot::Head).clone();
        let chance = t.mob_ref().equipment.drop_chance(EquipmentSlot::Head);
        if !current.is_empty() && (t.mob().random.next_float() - 0.1).max(0.0) < chance {
            let pos = t.pos();
            let (uuid, y_rot, velocity) = crate::mob::new_item_entity();
            t.env.out.push(crate::survival::Effect::DropItem {
                uuid,
                pos,
                velocity,
                y_rot,
                pickup_delay: 10,
                stack: current,
            });
        }
        t.mob().equipment.set(EquipmentSlot::Head, stack.clone());
        equipment::take(t, item, stack.count);
        if !t.cx.discarded.contains(&item) {
            t.cx.discarded.push(item);
        }
        let me = t.id;
        if let Some(raid) = raid_id.and_then(|id| t.ents.raids.get_mut(id)) {
            raid.leaders.retain(|l| l.0 != wave);
            raid.leaders.push((wave, me));
        }
        // `Raid.setLeader`: the banner and its drop chance.
        t.mob()
            .equipment
            .set(EquipmentSlot::Head, ominous_banner());
        t.mob().equipment.set_drop_chance(EquipmentSlot::Head, 2.0);
        st_mut(t.mob()).patrol_leader = true;
        return true;
    }
    // `Mob.pickUpItem`.
    let rules = if t.mob_ref().kind == Kind::Pillager {
        equipment::PickupRules {
            wants: |_, s| crate::item::item_id(s).name().ends_with("_banner"),
            preferred_weapon: Some("minecraft:pillager_preferred_weapons"),
        }
    } else {
        equipment::PickupRules::default()
    };
    let equipped = equipment::equip_item_if_possible(t, &stack, &rules);
    if equipped.is_empty() {
        return false;
    }
    equipment::take(t, item, equipped.count);
    true
}

/// `ObtainRaidLeaderBannerGoal.cannotPickUpBanner`.
fn cannot_pick_up_banner(t: &Tick<'_, '_>) -> bool {
    let m = t.mob_ref();
    let Some(raid) = crate::raid::current_raid(t.ents, m).filter(|r| r.is_active()) else {
        return true;
    };
    if raid.is_over() || !crate::raid::can_be_leader(m.kind) {
        return true;
    }
    if is_ominous_banner(m.equipment.get(EquipmentSlot::Head)) {
        return true;
    }
    raid.leader(st(m).wave)
        .is_some_and(|l| t.ents.mob(l).is_some_and(MobData::is_alive))
}

pub fn banner_can_use(t: &mut Tick<'_, '_>, banner: &mut Option<EntityId>) -> bool {
    if cannot_pick_up_banner(t) {
        return false;
    }
    let range = t.mob_ref().attrs.follow_range;
    let area = t.aabb().inflate(range, 8.0, range);
    let mut ids = std::mem::take(&mut t.cx.ids);
    ids.clear();
    t.ents.store.for_each_in(&area, false, |id| ids.push(id));
    let now = t.env.game_time;
    let old = std::mem::take(&mut st_mut(t.mob()).banner_cache);
    let mut cache = Vec::new();
    let mut found = false;
    for &id in &ids {
        let allowed = t.ents.item(id).is_some_and(|d| {
            d.pickup_delay <= 0 && !t.cx.discarded.contains(&id) && is_ominous_banner(&d.stack)
        });
        if !allowed {
            continue;
        }
        let until = old.iter().find(|c| c.0 == id).map_or(i64::MIN, |c| c.1);
        if now < until {
            cache.push((id, until));
            continue;
        }
        let Some(pos) = t.ents.store.get(id).map(|e| e.pos()) else {
            continue;
        };
        let path = nav::create_path_to(t, BlockPos::containing(pos.x, pos.y, pos.z), 1);
        if let Some(path) = path.filter(|p| p.reached) {
            t.mob().goal_path = Some(path);
            *banner = Some(id);
            found = true;
            break;
        }
        cache.push((id, now + 600));
    }
    t.cx.ids = ids;
    st_mut(t.mob()).banner_cache = if found { old } else { cache };
    found
}

pub fn banner_can_continue(t: &mut Tick<'_, '_>, banner: &Option<EntityId>) -> bool {
    let Some(id) = *banner else {
        return false;
    };
    if t.ents.item(id).is_none() || t.cx.discarded.contains(&id) {
        return false;
    }
    let done = t.mob_ref().nav.path.as_ref().is_none_or(|p| p.is_done());
    !done && !cannot_pick_up_banner(t)
}

pub fn banner_start(t: &mut Tick<'_, '_>) {
    let path = t.mob().goal_path.take();
    nav::move_to_path(t, path, 1.15);
}

pub fn banner_tick(t: &mut Tick<'_, '_>, banner: &mut Option<EntityId>) {
    let Some(id) = *banner else {
        return;
    };
    let Some(pos) = t.ents.store.get(id).map(|e| e.pos()) else {
        return;
    };
    if pos.distance_to_sqr(t.pos()) < 1.414 * 1.414 {
        pick_up_item(t, id);
    }
}

/// `hasControllingPassenger`.
fn has_controlling_passenger(t: &Tick<'_, '_>) -> bool {
    t.ents
        .riding
        .passengers_of(t.id)
        .first()
        .is_some_and(|&p| crate::riding::controls(t.ents, p, t.id))
}

/// `hasActiveRaid() && !getCurrentRaid().isOver()`: the raid id.
fn ongoing_raid(t: &Tick<'_, '_>) -> Option<i32> {
    let id = st(t.mob_ref()).raid?;
    let raid = t.ents.raids.get(id)?;
    (raid.is_active() && !raid.is_over()).then_some(id)
}

pub fn pathfind_can_use(t: &mut Tick<'_, '_>) -> bool {
    t.mob_ref().target.is_none()
        && !has_controlling_passenger(t)
        && ongoing_raid(t).is_some()
        && !t.ents.poi.is_village(t.block_pos())
}

pub fn pathfind_can_continue(t: &mut Tick<'_, '_>) -> bool {
    ongoing_raid(t).is_some() && !t.ents.poi.is_village(t.block_pos())
}

pub fn pathfind_tick(t: &mut Tick<'_, '_>, recruitment_tick: &mut i32) {
    let Some(raid_id) = st(t.mob_ref()).raid else {
        return;
    };
    let Some((center, wave)) = t
        .ents
        .raids
        .get(raid_id)
        .filter(|r| r.is_active())
        .map(|r| (r.center, r.groups_spawned))
    else {
        return;
    };
    if t.mob_ref().tick_count > *recruitment_tick {
        *recruitment_tick = t.mob_ref().tick_count + 20;
        // `recruitNearby`.
        let area = t.aabb().inflate(16.0, 16.0, 16.0);
        let mut ids = std::mem::take(&mut t.cx.ids);
        ids.clear();
        t.ents.store.for_each_in(&area, false, |id| ids.push(id));
        let recruits: Vec<EntityId> = ids
            .iter()
            .copied()
            .filter(|&id| {
                t.ents.mob(id).is_some_and(|m| {
                    super::is_raider(m.kind)
                        && !crate::raid::has_active_raid(t.ents, m)
                        && crate::raid::can_join_raid(m)
                })
            })
            .collect();
        t.cx.ids = ids;
        for id in recruits {
            crate::raid::join_existing(t.ents, raid_id, wave, id, t.env.out);
        }
    }
    if t.mob_ref().nav.is_done() {
        let towards = randompos::bottom_center(center);
        if let Some(p) = randompos::default_pos_towards(t, 15, 4, towards, randompos::half_pi()) {
            nav::move_to_pos(t, p, 1.0);
        }
    }
}

/// `RaiderMoveThroughVillageGoal` state: the home it walks to and the last homes it reached.
#[derive(Clone, Copy, Debug)]
pub struct MoveThroughVillage {
    pub poi: BlockPos,
    pub visited: [BlockPos; 3],
    pub visited_len: u8,
    pub stuck: bool,
}

impl MoveThroughVillage {
    pub fn new() -> MoveThroughVillage {
        MoveThroughVillage {
            poi: BlockPos::ZERO,
            visited: [BlockPos::ZERO; 3],
            visited_len: 0,
            stuck: false,
        }
    }

    fn visited(&self) -> &[BlockPos] {
        &self.visited[..usize::from(self.visited_len)]
    }
}

impl Default for MoveThroughVillage {
    fn default() -> Self {
        MoveThroughVillage::new()
    }
}

/// `BlockPos.closerToCenterThan(pos, dist)`.
fn closer_to_center(p: BlockPos, pos: Vec3, dist: f64) -> bool {
    let c = Vec3::new(
        f64::from(p.x) + 0.5,
        f64::from(p.y) + 0.5,
        f64::from(p.z) + 0.5,
    );
    c.distance_to_sqr(pos) < dist * dist
}

pub fn move_through_can_use(t: &mut Tick<'_, '_>, v: &mut MoveThroughVillage) -> bool {
    // `updateVisited`.
    if v.visited_len > 2 {
        v.visited.copy_within(1.., 0);
        v.visited_len -= 1;
    }
    if ongoing_raid(t).is_none() {
        return false;
    }
    let visited: Vec<BlockPos> = v.visited().to_vec();
    let pos = t.block_pos();
    let mut random = std::mem::replace(
        &mut t.mob().random,
        vanillite_base::random::LegacyRandom::new(0),
    );
    let home = t.ents.poi.get_random(
        |ty| ty == crate::poi::PoiType::Home,
        |p| !visited.contains(&p),
        crate::poi::Occupancy::Any,
        pos,
        48,
        &mut random,
    );
    t.mob().random = random;
    let Some(home) = home else {
        return false;
    };
    v.poi = home;
    t.mob_ref().target.is_none()
}

pub fn move_through_can_continue(t: &mut Tick<'_, '_>, v: &MoveThroughVillage) -> bool {
    if t.mob_ref().nav.is_done() {
        return false;
    }
    let width = f64::from(t.core().dims().width);
    t.mob_ref().target.is_none() && !closer_to_center(v.poi, t.pos(), width + 1.0) && !v.stuck
}

pub fn move_through_start(t: &mut Tick<'_, '_>, v: &mut MoveThroughVillage) {
    t.mob().no_action_time = 0;
    let p = v.poi;
    nav::move_to_pos(
        t,
        Vec3::new(f64::from(p.x), f64::from(p.y), f64::from(p.z)),
        1.05,
    );
    v.stuck = false;
}

pub fn move_through_stop(t: &mut Tick<'_, '_>, v: &mut MoveThroughVillage) {
    if closer_to_center(v.poi, t.pos(), 1.0) {
        let n = usize::from(v.visited_len);
        if n < v.visited.len() {
            v.visited[n] = v.poi;
            v.visited_len += 1;
        }
    }
}

pub fn move_through_tick(t: &mut Tick<'_, '_>, v: &mut MoveThroughVillage) {
    if !t.mob_ref().nav.is_done() {
        return;
    }
    let towards = randompos::bottom_center(v.poi);
    let next = randompos::default_pos_towards(t, 16, 7, towards, std::f64::consts::PI / 10.0)
        .or_else(|| randompos::default_pos_towards(t, 8, 7, towards, randompos::half_pi()));
    match next {
        Some(p) => {
            nav::move_to_pos(t, p, 1.05);
        }
        None => v.stuck = true,
    }
}

pub fn celebration_can_use(t: &mut Tick<'_, '_>) -> bool {
    let m = t.mob_ref();
    m.is_alive()
        && m.target.is_none()
        && crate::raid::current_raid(t.ents, m).is_some_and(crate::raid::Raid::is_loss)
}

/// `Raider.getCelebrateSound`.
fn celebrate_sound(kind: Kind) -> &'static str {
    match kind {
        Kind::Pillager => "minecraft:entity.pillager.celebrate",
        Kind::Vindicator => "minecraft:entity.vindicator.celebrate",
        Kind::Evoker => "minecraft:entity.evoker.celebrate",
        Kind::Illusioner => "minecraft:entity.illusioner.ambient",
        Kind::Witch => "minecraft:entity.witch.celebrate",
        _ => "minecraft:entity.ravager.celebrate",
    }
}

pub fn celebration_tick(t: &mut Tick<'_, '_>) {
    if !t.mob_ref().silent && t.mob().random.next_int_bounded(goals::reduced(100).max(1)) == 0 {
        // `makeSound`: the voice pitch of an adult.
        let r = &mut t.mob().random;
        let pitch = (r.next_float() - r.next_float()) * 0.2 + 1.0;
        let sound = celebrate_sound(t.mob_ref().kind);
        let volume = crate::mob::fam::mob_sound_volume(t.mob_ref());
        super::super::super::kinds::play_sound(t, sound, volume, pitch);
    }
    if t.ents.vehicle_of(t.id).is_none()
        && t.mob().random.next_int_bounded(goals::reduced(50).max(1)) == 0
    {
        t.mob().jump = true;
    }
}
