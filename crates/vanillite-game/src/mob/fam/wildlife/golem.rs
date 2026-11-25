//! Snow golems (`SnowGolem`) and iron golems (`IronGolem`).
//!
//! Iron golems keep to their village (`MoveBackToVillageGoal`, `GolemRandomStrollInVillageGoal`
//! towards villagers that want a golem and occupied POIs), offer flowers and defend the
//! villagers against players with a bad reputation (`DefendVillageTargetGoal`).

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Vec3};
use vanillite_data::block::ids;
use vanillite_protocol::packets::play::clientbound::SoundSource;
use vanillite_world::entity::{EntityFlags, EntityId};

use super::{Goal, st, st_mut};
use crate::mob::animal::{Interaction, hurt_held, sound_at, use_item};
use crate::mob::common::{self, CommonGoal};
use crate::mob::fam::{FamGoal, Hurt, Interact};
use crate::mob::goals::{Goal as MobGoal, MeleeKind};
use crate::mob::kinds::{self, Kind};
use crate::mob::missile::{MissileData, MissileKind};
use crate::mob::{MobData, Tick, WorldAction, combat, living, nav, randompos};
use crate::survival::rules::Rule;

/// `IronGolem` targets: an `Enemy` that is no creeper.
fn enemy_not_creeper(t: &Tick<'_, '_>, id: EntityId) -> bool {
    common::enemy(t, id) && !common::mob_of(t, id, Kind::Creeper)
}

/// `registerGoals` of both golems.
pub(super) fn register_goals(m: &mut MobData) {
    let g = &mut m.goals;
    let tg = &mut m.targets;
    if m.kind == Kind::SnowGolem {
        g.add(1, MobGoal::Common(CommonGoal::ranged(1.25, 20, 20, 10.0)));
        g.add(2, MobGoal::stroll_with(1.0, 1.000_000_1e-5));
        g.add(3, MobGoal::look_at_player(6.0));
        g.add(4, MobGoal::random_look());
        tg.add(
            1,
            MobGoal::Common(CommonGoal::nearest(10, true, common::enemy)),
        );
        return;
    }
    g.add(1, MobGoal::melee(MeleeKind::Plain, 1.0, true));
    g.add(
        2,
        MobGoal::Fam(FamGoal::Wildlife(Goal::MoveTowardsTarget {
            speed: 0.9,
            within: 32.0,
            target: None,
            wanted: Vec3::ZERO,
        })),
    );
    g.add(
        2,
        MobGoal::Fam(FamGoal::Wildlife(Goal::GolemStroll {
            back: true,
            speed: 0.6,
            interval: 10,
            wanted: Vec3::ZERO,
        })),
    );
    g.add(
        4,
        MobGoal::Fam(FamGoal::Wildlife(Goal::GolemStroll {
            back: false,
            speed: 0.6,
            interval: 240,
            wanted: Vec3::ZERO,
        })),
    );
    g.add(
        5,
        MobGoal::Fam(FamGoal::Wildlife(Goal::OfferFlower {
            target: None,
            tick: 0,
        })),
    );
    g.add(7, MobGoal::look_at_player(6.0));
    g.add(8, MobGoal::random_look());
    tg.add(
        1,
        MobGoal::Fam(FamGoal::Wildlife(Goal::DefendVillage {
            state: crate::mob::goals::TargetState::new(false),
            candidate: None,
        })),
    );
    tg.add(2, MobGoal::hurt_by(false));
    tg.add(
        3,
        MobGoal::Common(CommonGoal::nearest_player(true, common::angry_at)),
    );
    tg.add(
        3,
        MobGoal::Common(CommonGoal::nearest(5, false, enemy_not_creeper)),
    );
    tg.add(
        4,
        MobGoal::Common(CommonGoal::ResetUniversalAnger {
            alert_same_type: false,
            last_hurt: 0,
        }),
    );
}

/// `SnowGolem.aiStep`: a snow trail where it walks (with mob griefing).
///
/// `SNOW_GOLEM_MELTS` (hot biomes) is a biome attribute the tick cannot read yet; snow golems
/// do not melt there.
pub(super) fn snow_ai_step(t: &mut Tick<'_, '_>) {
    if !t.env.rules.bool(Rule::MOB_GRIEFING) {
        return;
    }
    let snow = ids::SNOW.default_state();
    let p = t.pos();
    for i in 0..4 {
        let x = vanillite_base::math::floor(p.x + f64::from((i % 2 * 2 - 1) as f32 * 0.25));
        let y = vanillite_base::math::floor(p.y);
        let z = vanillite_base::math::floor(p.z + f64::from((i / 2 % 2 * 2 - 1) as f32 * 0.25));
        let pos = BlockPos::new(x, y, z);
        if t.level.block(pos).is_air() && t.level.can_survive(snow, pos) {
            t.cx.actions.push(WorldAction::SetBlock(pos, snow, 3));
        }
    }
}

/// `SnowGolem.performRangedAttack`: a snowball at the target's chest.
pub(super) fn snow_ranged_attack(t: &mut Tick<'_, '_>, target: EntityId) {
    let Some(o) = t.other(target) else {
        return;
    };
    let p = t.pos();
    let start = Vec3::new(p.x, t.eye_y() - f64::from(0.1_f32), p.z);
    let xd = o.pos.x - p.x;
    let yd = o.eye_y - f64::from(1.1_f32);
    let zd = o.pos.z - p.z;
    let yo = (xd * xd + zd * zd).sqrt() * f64::from(0.2_f32);
    let mut own = crate::mob::missile::new_projectile_random();
    let (velocity, rot) =
        super::snowball::shoot(&mut own, Vec3::new(xd, yd + yo - start.y, zd), 1.6, 12.0);
    let mut data = MissileData::new(MissileKind::Snowball);
    data.item = crate::mob::equipment::stack("minecraft:snowball");
    crate::mob::missile::spawn_shot(t, start, velocity, rot, data);
    let r = t.mob().random.next_float();
    kinds::play_sound(
        t,
        "minecraft:entity.snow_golem.shoot",
        1.0,
        0.4 / (r * 0.4 + 0.8),
    );
}

/// `SnowGolem.mobInteract`: shears take the pumpkin off.
pub(super) fn snow_interact(ix: &mut Interact<'_, '_>) -> Interaction {
    if crate::item::item_id(ix.stack).name() != "minecraft:shears" {
        return Interaction::Pass;
    }
    if !ix.ents.mob(ix.target).is_some_and(|m| st(m).pumpkin) {
        return Interaction::Pass;
    }
    sound_at(
        ix.ents,
        ix.env,
        ix.target,
        "minecraft:entity.snow_golem.shear",
        SoundSource::Players,
        1.0,
    );
    let eye = ix.ents.mob(ix.target).map_or(1.7, crate::mob::eye_height);
    if let Some(m) = ix.ents.mob_mut(ix.target) {
        st_mut(m).pumpkin = false;
    }
    if let Some(mut e) = ix.ents.store.get_mut(ix.target) {
        e.mark_data_dirty();
    }
    // `shearing/snow_golem`: the carved pumpkin, dropped at the eyes.
    if let Some(pos) = ix.ents.store.get(ix.target).map(|e| e.pos()) {
        drop_at(
            ix,
            pos + Vec3::new(0.0, eye, 0.0),
            "minecraft:carved_pumpkin",
            1,
        );
    }
    crate::mob::animal::entity_game_event(
        ix.ents,
        ix.env,
        ix.target,
        crate::level::game_event::events().shear,
        Some(ix.player),
    );
    hurt_held(ix.ents, ix.env, ix.player, ix.hand, 1);
    Interaction::Success
}

/// `spawnAtLocation`: an item entity with the default motion.
pub(super) fn drop_at(ix: &mut Interact<'_, '_>, pos: Vec3, item: &str, count: i32) {
    let stack = crate::mob::equipment::stack(item).with_count(count);
    if stack.is_empty() {
        return;
    }
    let (uuid, y_rot, velocity) = crate::mob::new_item_entity();
    ix.env.out.push(crate::survival::Effect::DropItem {
        uuid,
        pos,
        velocity,
        y_rot,
        pickup_delay: 10,
        stack,
    });
}

/// `IronGolem.aiStep`: the attack and flower animations and the anger.
pub(super) fn iron_ai_step(t: &mut Tick<'_, '_>) {
    {
        let s = st_mut(t.mob());
        if s.attack_animation > 0 {
            s.attack_animation -= 1;
        }
        if s.offer_flower > 0 {
            s.offer_flower -= 1;
        }
    }
    let range = crate::mob::fam::anger_range(Kind::IronGolem);
    common::update_persistent_anger(t, true, range);
}

/// `IronGolem.doHurtTarget`: half to one and a half times the attack damage, flinging the
/// target up.
pub(super) fn iron_hurt_target(t: &mut Tick<'_, '_>, target: EntityId) -> bool {
    st_mut(t.mob()).attack_animation = 10;
    let id = t.id;
    living::entity_event(t, id, 4);
    #[allow(clippy::cast_possible_truncation)]
    let attack = t.mob_ref().attrs.attack_damage as f32;
    #[allow(clippy::cast_possible_truncation)]
    let whole = attack as i32;
    #[allow(clippy::cast_precision_loss)]
    let damage = if whole > 0 {
        attack / 2.0 + t.mob().random.next_int_bounded(whole) as f32
    } else {
        attack
    };
    let source = combat::entity_source(t.ents, t.env.types.mob_attack, id, id);
    let hurt = combat::hurt(t.level, t.ents, t.env, t.cx, target, source, damage);
    if hurt {
        let resistance = t
            .ents
            .mob(target)
            .map_or(0.0, |m| m.attrs.knockback_resistance);
        if let Some(mut e) = t.ents.store.get_mut(target) {
            let v = fling(e.as_ref().velocity(), resistance);
            e.set_velocity(v);
            e.set_flags(EntityFlags::HURT_MARKED, true);
        }
        t.mob().last_hurt_mob = Some(target);
    }
    if t.exists() {
        kinds::play_sound(t, "minecraft:entity.iron_golem.attack", 1.0, 1.0);
    }
    hurt
}

/// The iron golem's fling: up by 0.4 scaled by what the knockback resistance leaves.
pub(crate) fn fling(v: Vec3, knockback_resistance: f64) -> Vec3 {
    let scale = (1.0 - knockback_resistance).max(0.0);
    Vec3::new(v.x, v.y + f64::from(0.4_f32) * scale, v.z)
}

/// `Crackiness.GOLEM.byFraction`: 0 none, 1 low, 2 medium, 3 high.
pub(crate) fn crackiness(fraction: f32) -> u8 {
    if fraction < 0.25 {
        3
    } else if fraction < 0.5 {
        2
    } else if fraction < 0.75 {
        1
    } else {
        0
    }
}

/// `IronGolem.hurtServer`: a crack sound when the golem looks more damaged.
pub(super) fn iron_after_hurt(h: &mut Hurt<'_, '_>, amount: f32) {
    let Some(m) = h.ents.mob(h.target) else {
        return;
    };
    let max = m.max_health();
    let before = (m.health + amount).min(max);
    if crackiness(before / max) != crackiness(m.health / max) {
        combat::mob_sound(
            h.ents,
            h.env,
            h.target,
            "minecraft:entity.iron_golem.damage",
        );
    }
}

/// `IronGolem.mobInteract`: an iron ingot heals 25.
pub(super) fn iron_interact(ix: &mut Interact<'_, '_>) -> Interaction {
    if crate::item::item_id(ix.stack).name() != "minecraft:iron_ingot" {
        return Interaction::Pass;
    }
    let Some(m) = ix.ents.mob_mut(ix.target) else {
        return Interaction::Pass;
    };
    let before = m.health;
    m.health = (m.health + 25.0).min(m.max_health());
    #[allow(clippy::float_cmp)]
    if m.health == before {
        return Interaction::Pass;
    }
    let pitch = 1.0 + (m.random.next_float() - m.random.next_float()) * 0.2;
    if let Some(mut e) = ix.ents.store.get_mut(ix.target) {
        e.mark_data_dirty();
    }
    sound_at(
        ix.ents,
        ix.env,
        ix.target,
        "minecraft:entity.iron_golem.repair",
        SoundSource::Neutral,
        pitch,
    );
    use_item(ix.ents, ix.player, ix.hand);
    Interaction::Success
}

pub(super) fn can_use(t: &mut Tick<'_, '_>, g: &mut Goal) -> bool {
    match g {
        Goal::GolemStroll {
            back,
            interval,
            wanted,
            ..
        } => return stroll_can_use(t, *back, *interval, wanted),
        Goal::OfferFlower { target, .. } => return offer_flower_can_use(t, target),
        Goal::DefendVillage { candidate, .. } => return defend_can_use(t, candidate),
        _ => {}
    }
    let Goal::MoveTowardsTarget {
        within,
        target,
        wanted,
        ..
    } = g
    else {
        return false;
    };
    *target = t.mob_ref().target;
    let Some(o) = target.and_then(|id| t.other(id)) else {
        return false;
    };
    let w = f64::from(*within);
    if o.pos.distance_to_sqr(t.pos()) > w * w {
        return false;
    }
    match randompos::default_pos_towards(t, 16, 7, o.pos, randompos::half_pi()) {
        Some(p) => {
            *wanted = p;
            true
        }
        None => false,
    }
}

pub(super) fn can_continue(t: &mut Tick<'_, '_>, g: &mut Goal) -> bool {
    match g {
        Goal::GolemStroll { .. } => {
            return !t.mob_ref().nav.is_done() && !has_controlling_passenger(t);
        }
        Goal::OfferFlower { tick, .. } => return *tick > 0,
        Goal::DefendVillage { state, .. } => {
            return crate::mob::goals::target_can_continue(t, state);
        }
        _ => {}
    }
    let Goal::MoveTowardsTarget { within, target, .. } = g else {
        return false;
    };
    let w = f64::from(*within);
    !t.mob_ref().nav.is_done()
        && target
            .and_then(|id| t.other(id))
            .is_some_and(|o| o.pos.distance_to_sqr(t.pos()) < w * w)
}

pub(super) fn start(t: &mut Tick<'_, '_>, g: &mut Goal) {
    match g {
        Goal::MoveTowardsTarget { speed, wanted, .. } | Goal::GolemStroll { speed, wanted, .. } => {
            nav::move_to_pos(t, *wanted, *speed);
        }
        Goal::OfferFlower { tick, .. } => {
            *tick = crate::mob::goals::reduced(400);
            offer_flower(t, true);
        }
        Goal::DefendVillage { state, candidate } => {
            crate::mob::goals::set_target(t, *candidate);
            state.target_mob = *candidate;
            state.unseen_ticks = 0;
        }
        _ => {}
    }
}

pub(super) fn stop(t: &mut Tick<'_, '_>, g: &mut Goal) {
    match g {
        Goal::GolemStroll { .. } => t.mob().nav.stop(),
        Goal::OfferFlower { target, tick } => {
            offer_flower(t, false);
            if *tick == 0
                && let Some(id) = *target
            {
                give_flower(t, id);
            }
            *target = None;
        }
        Goal::DefendVillage { state, .. } => {
            crate::mob::goals::set_target(t, None);
            state.target_mob = None;
        }
        _ => {}
    }
}

pub(super) fn tick(t: &mut Tick<'_, '_>, g: &mut Goal) {
    if let Goal::OfferFlower { target, tick } = g {
        if let Some(o) = target.and_then(|id| t.other(id)) {
            t.mob()
                .look
                .set_look_at(Vec3::new(o.pos.x, o.eye_y, o.pos.z), 30.0, 30.0);
        }
        *tick -= 1;
    }
}

// --- Village goals ---------------------------------------------------------------------------

/// `hasControllingPassenger`.
fn has_controlling_passenger(t: &Tick<'_, '_>) -> bool {
    t.ents
        .riding
        .passengers_of(t.id)
        .first()
        .is_some_and(|&p| crate::riding::controls(t.ents, p, t.id))
}

/// `Villager.wantsToSpawnGolem(gameTime)`: slept within a day and saw no golem recently.
fn villager_wants_golem(m: &MobData, game_time: i64) -> bool {
    use crate::mob::brain::MemoryType;
    let Some(brain) = m.brain.as_deref() else {
        return false;
    };
    brain
        .long(MemoryType::LastSlept)
        .is_some_and(|slept| game_time - slept < 24000)
        && !brain.has(MemoryType::GolemDetectedRecently)
}

/// `RandomStrollGoal.canUse` of `GolemRandomStrollInVillageGoal` (`back`:
/// `MoveBackToVillageGoal`, only outside villages); both skip the idle check.
fn stroll_can_use(t: &mut Tick<'_, '_>, back: bool, interval: i32, wanted: &mut Vec3) -> bool {
    if back && t.ents.poi.is_village(t.block_pos()) {
        return false;
    }
    if has_controlling_passenger(t) {
        return false;
    }
    let r = crate::mob::goals::reduced(interval).max(1);
    if t.mob().random.next_int_bounded(r) != 0 {
        return false;
    }
    let pos = if back {
        back_to_village_pos(t)
    } else {
        village_stroll_pos(t)
    };
    match pos {
        Some(p) => {
            *wanted = p;
            true
        }
        None => false,
    }
}

/// `MoveBackToVillageGoal.getPosition`: towards the section around it closest to a village.
fn back_to_village_pos(t: &mut Tick<'_, '_>) -> Option<Vec3> {
    let section = t.block_pos().section();
    let best = find_section_closest_to_village(&t.ents.poi, section, 2);
    if best == section {
        return None;
    }
    let center = crate::raid::section_center(best);
    randompos::default_pos_towards(
        t,
        10,
        7,
        randompos::bottom_center(center),
        randompos::half_pi(),
    )
}

/// `BehaviorUtils.findSectionClosestToVillage(level, center, radius)`.
pub fn find_section_closest_to_village(
    poi: &crate::poi::PoiManager,
    center: vanillite_base::SectionPos,
    radius: i32,
) -> vanillite_base::SectionPos {
    let here = poi.sections_to_village(center);
    let mut best: Option<(i32, vanillite_base::SectionPos)> = None;
    crate::raid::for_each_section_in_cube(center, radius, |s| {
        let d = poi.sections_to_village(s);
        if d < here && best.is_none_or(|(b, _)| d < b) {
            best = Some((d, s));
        }
    });
    best.map_or(center, |(_, s)| s)
}

/// `GolemRandomStrollInVillageGoal.getPosition` (its choices use the level random).
fn village_stroll_pos(t: &mut Tick<'_, '_>) -> Option<Vec3> {
    if t.env.random.next_float() < 0.3 {
        return randompos::land_pos(t, 10, 7);
    }
    let target = if t.env.random.next_float() < 0.7 {
        towards_villager_who_wants_golem(t).or_else(|| towards_poi(t))
    } else {
        towards_poi(t).or_else(|| towards_villager_who_wants_golem(t))
    };
    target.or_else(|| randompos::land_pos(t, 10, 7))
}

fn towards_villager_who_wants_golem(t: &mut Tick<'_, '_>) -> Option<Vec3> {
    let area = t.aabb().inflate(32.0, 32.0, 32.0);
    let now = t.env.game_time;
    let mut ids = std::mem::take(&mut t.cx.ids);
    ids.clear();
    t.ents.store.for_each_in(&area, false, |id| ids.push(id));
    let villagers: Vec<EntityId> = ids
        .iter()
        .copied()
        .filter(|&id| {
            t.ents.mob(id).is_some_and(|m| {
                m.kind == Kind::Villager && m.is_alive() && villager_wants_golem(m, now)
            })
        })
        .collect();
    t.cx.ids = ids;
    if villagers.is_empty() {
        return None;
    }
    let n = i32::try_from(villagers.len()).unwrap_or(i32::MAX);
    let pick = usize::try_from(t.env.random.next_int_bounded(n)).unwrap_or(0);
    let pos = t.ents.store.get(villagers[pick])?.pos();
    randompos::land_pos_towards(t, 10, 7, pos)
}

fn towards_poi(t: &mut Tick<'_, '_>) -> Option<Vec3> {
    // `getRandomVillageSection`.
    let mut sections = Vec::new();
    crate::raid::for_each_section_in_cube(t.block_pos().section(), 2, |s| {
        if t.ents.poi.sections_to_village(s) == 0 {
            sections.push(s);
        }
    });
    if sections.is_empty() {
        return None;
    }
    let n = i32::try_from(sections.len()).unwrap_or(i32::MAX);
    let section = sections[usize::try_from(t.env.random.next_int_bounded(n)).unwrap_or(0)];
    // `getRandomPoiWithinSection`.
    let pois = t.ents.poi.in_range(
        |_| true,
        crate::raid::section_center(section),
        8,
        crate::poi::Occupancy::IsOccupied,
    );
    if pois.is_empty() {
        return None;
    }
    let n = i32::try_from(pois.len()).unwrap_or(i32::MAX);
    let poi = pois[usize::try_from(t.env.random.next_int_bounded(n)).unwrap_or(0)].pos;
    randompos::land_pos_towards(t, 10, 7, randompos::bottom_center(poi))
}

/// `OfferFlowerGoal.canUse`: by day, rarely, to the nearest villager or copper golem it sees
/// within 6 blocks.
fn offer_flower_can_use(t: &mut Tick<'_, '_>, target: &mut Option<EntityId>) -> bool {
    if !t.cx.env.is_bright_outside() {
        return false;
    }
    if t.mob().random.next_int_bounded(8000) != 0 {
        return false;
    }
    let area = t.aabb().inflate(6.0, 2.0, 6.0);
    let p = t.pos();
    let mut ids = std::mem::take(&mut t.cx.ids);
    ids.clear();
    t.ents.store.for_each_in(&area, false, |id| ids.push(id));
    let mut best: Option<(f64, EntityId)> = None;
    for &id in &ids {
        if id == t.id
            || !t.ents.mob(id).is_some_and(|m| {
                m.is_alive() && matches!(m.kind, Kind::Villager | Kind::CopperGolem)
            })
        {
            continue;
        }
        let Some(pos) = t.ents.store.get(id).map(|e| e.pos()) else {
            continue;
        };
        let d = pos.distance_to_sqr(p);
        if d > 36.0 || !living::has_line_of_sight(t, id) {
            continue;
        }
        if best.is_none_or(|(b, _)| d < b) {
            best = Some((d, id));
        }
    }
    t.cx.ids = ids;
    *target = best.map(|(_, id)| id);
    target.is_some()
}

/// `IronGolem.offerFlower(offer)`: the flower animation (events 11 and 34).
fn offer_flower(t: &mut Tick<'_, '_>, offer: bool) {
    st_mut(t.mob()).offer_flower = if offer { 400 } else { 0 };
    let id = t.id;
    living::entity_event(t, id, if offer { 11 } else { 34 });
}

/// `OfferFlowerGoal.stop`: a copper golem in reach gets the poppy on its antenna.
fn give_flower(t: &mut Tick<'_, '_>, id: EntityId) {
    let area = t.aabb().inflate(6.0, 2.0, 6.0);
    let Some(other) = t.ents.store.get(id).map(|e| e.aabb()) else {
        return;
    };
    let Some(m) = t.ents.mob_mut(id) else {
        return;
    };
    if m.kind != Kind::CopperGolem
        || !m
            .equipment
            .get(crate::item::EquipmentSlot::Saddle)
            .is_empty()
        || !area.intersects(&other)
    {
        return;
    }
    m.equipment.set(
        crate::item::EquipmentSlot::Saddle,
        crate::mob::equipment::stack("minecraft:poppy"),
    );
    m.equipment
        .set_guaranteed_drop(crate::item::EquipmentSlot::Saddle);
    if let Some(mut e) = t.ents.store.get_mut(id) {
        e.mark_data_dirty();
    }
}

/// `DefendVillageTargetGoal.canUse`: a player that a villager it sees thinks very badly of
/// (reputation -100 or less) becomes its target; the last one found stays a candidate.
fn defend_can_use(t: &mut Tick<'_, '_>, candidate: &mut Option<EntityId>) -> bool {
    let area = t.aabb().inflate(10.0, 8.0, 10.0);
    let p = t.pos();
    let mut ids = std::mem::take(&mut t.cx.ids);
    ids.clear();
    t.ents.store.for_each_in(&area, false, |id| ids.push(id));
    let mut villagers = Vec::new();
    let mut players = Vec::new();
    for &id in &ids {
        let Some(pos) = t.ents.store.get(id).map(|e| e.pos()) else {
            continue;
        };
        if pos.distance_to_sqr(p) > 64.0 * 64.0 || id == t.id {
            continue;
        }
        let villager = t
            .ents
            .mob(id)
            .is_some_and(|m| m.kind == Kind::Villager && m.is_alive());
        let player = t.ents.player(id).is_some() && crate::mob::goals::can_attack(t, id);
        if (villager || player) && living::has_line_of_sight(t, id) {
            if villager {
                villagers.push(id);
            } else {
                players.push(id);
            }
        }
    }
    t.cx.ids = ids;
    for &v in &villagers {
        for &pl in &players {
            let Some(uuid) = t.ents.store.get(pl).map(|e| e.uuid()) else {
                continue;
            };
            let Some(m) = t.ents.mob(v) else {
                continue;
            };
            if crate::mob::fam::village::villager::player_reputation(m, uuid) <= -100 {
                *candidate = Some(pl);
            }
        }
    }
    let Some(c) = *candidate else {
        return false;
    };
    !t.ents.player(c).is_some_and(|p| {
        matches!(
            p.game_mode,
            vanillite_config::GameMode::Spectator | vanillite_config::GameMode::Creative
        )
    })
}
