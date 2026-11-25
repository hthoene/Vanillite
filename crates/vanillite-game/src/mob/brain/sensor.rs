//! Sensors (`Sensor`, `SensorType`): they fill memories every `scanRate` ticks. The shared
//! sensors are here; family sensors plug in as [`SensorKind::Fam`].

use vanillite_base::Aabb;
use vanillite_config::GameMode;
use vanillite_world::entity::EntityId;

use super::{MemValue, MemoryType, Tick, Visible, brain, brain_ref, sensing};
use crate::item::EquipmentSlot;

/// Which entities a [`SensorKind::Mob`] sensor looks for, and when it is ready.
pub type MobTest = fn(&Tick<'_, '_>, EntityId) -> bool;

/// The sensor types.
#[derive(Clone, Copy, Debug)]
pub enum SensorKind {
    /// `NEAREST_ITEMS`.
    NearestItems,
    /// `NEAREST_LIVING_ENTITIES`.
    NearestLivingEntities,
    /// `NEAREST_PLAYERS`.
    NearestPlayers,
    /// `HURT_BY`.
    HurtBy,
    /// `NEAREST_ADULT`: the nearest visible adult of the own type.
    NearestAdult,
    /// `NEAREST_ADULT_ANY_TYPE`: of `#followable_friendly_mobs`.
    NearestAdultAnyType,
    /// `IS_IN_WATER`.
    IsInWater,
    /// `TemptingSensor`: an item tag, or the animal's food (`None`).
    Tempting(Option<&'static str>),
    /// `MobSensor(scanRate, mobTest, readyTest, memory, ttl)`.
    Mob {
        scan_rate: i32,
        test: MobTest,
        ready: fn(&Tick<'_, '_>) -> bool,
        memory: MemoryType,
        ttl: i32,
    },
    /// A family sensor.
    Fam(FamSensor),
}

/// A family sensor.
#[derive(Clone, Copy, Debug)]
pub enum FamSensor {
    Brainy(crate::mob::fam::brainy::Sensor),
    Mount(crate::mob::fam::mount::Sensor),
    Warden(crate::mob::boss::warden::ai::Sensor),
}

/// A sensor with its countdown.
#[derive(Clone, Copy, Debug)]
pub struct SensorState {
    pub kind: SensorKind,
    pub scan_rate: i32,
    pub time_to_tick: i64,
}

impl SensorState {
    pub fn new(kind: SensorKind) -> SensorState {
        let scan_rate = match kind {
            SensorKind::Mob { scan_rate, .. } => scan_rate,
            SensorKind::Fam(FamSensor::Brainy(s)) => crate::mob::fam::brainy::sensor_scan_rate(s),
            SensorKind::Fam(FamSensor::Mount(s)) => crate::mob::fam::mount::sensor_scan_rate(s),
            SensorKind::Fam(FamSensor::Warden(s)) => {
                crate::mob::boss::warden::ai::sensor_scan_rate(s)
            }
            _ => 20,
        };
        SensorState {
            kind,
            scan_rate,
            time_to_tick: 0,
        }
    }
}

/// `requires()`: the memories a sensor registers.
pub fn requires(kind: &SensorKind) -> &'static [MemoryType] {
    use MemoryType as M;
    match kind {
        SensorKind::NearestItems => &[M::NearestVisibleWantedItem],
        SensorKind::NearestLivingEntities => {
            &[M::NearestLivingEntities, M::NearestVisibleLivingEntities]
        }
        SensorKind::NearestPlayers => &[
            M::NearestPlayers,
            M::NearestVisiblePlayer,
            M::NearestVisibleAttackablePlayer,
            M::NearestVisibleAttackablePlayers,
        ],
        SensorKind::HurtBy => &[M::HurtBy, M::HurtByEntity],
        SensorKind::NearestAdult | SensorKind::NearestAdultAnyType => {
            &[M::NearestVisibleAdult, M::NearestVisibleLivingEntities]
        }
        SensorKind::IsInWater => &[M::IsInWater],
        SensorKind::Tempting(_) => &[M::TemptingPlayer],
        SensorKind::Mob { memory, .. } => mob_sensor_requires(*memory),
        SensorKind::Fam(FamSensor::Brainy(s)) => crate::mob::fam::brainy::sensor_requires(s),
        SensorKind::Fam(FamSensor::Mount(s)) => crate::mob::fam::mount::sensor_requires(s),
        SensorKind::Fam(FamSensor::Warden(s)) => crate::mob::boss::warden::ai::sensor_requires(s),
    }
}

fn mob_sensor_requires(memory: MemoryType) -> &'static [MemoryType] {
    use MemoryType as M;
    // `MobSensor.requires` is only `NEAREST_LIVING_ENTITIES`; the memory it sets is
    // registered by the behaviours that read it.
    match memory {
        M::DangerDetectedRecently => &[M::NearestLivingEntities, M::DangerDetectedRecently],
        _ => &[M::NearestLivingEntities],
    }
}

/// One sensor's `doTick`.
pub fn tick(t: &mut Tick<'_, '_>, kind: SensorKind) {
    match kind {
        SensorKind::NearestItems => nearest_items(t),
        SensorKind::NearestLivingEntities => nearest_living_entities(t),
        SensorKind::NearestPlayers => nearest_players(t),
        SensorKind::HurtBy => hurt_by(t),
        SensorKind::NearestAdult => nearest_adult(t, false),
        SensorKind::NearestAdultAnyType => nearest_adult(t, true),
        SensorKind::IsInWater => {
            let in_water = t.mob_ref().fluids.in_water();
            let b = brain(t);
            if in_water {
                b.set(MemoryType::IsInWater, MemValue::Unit);
            } else {
                b.erase(MemoryType::IsInWater);
            }
        }
        SensorKind::Tempting(tag) => tempting(t, tag),
        SensorKind::Mob {
            test,
            ready,
            memory,
            ttl,
            ..
        } => {
            if !ready(t) {
                brain(t).erase(memory);
                return;
            }
            if !brain_ref(t).has(MemoryType::NearestLivingEntities) {
                return;
            }
            let list = brain_ref(t)
                .entities(MemoryType::NearestLivingEntities)
                .to_vec();
            if list.iter().any(|&e| test(t, e)) {
                brain(t).set_with_expiry(memory, MemValue::Bool(true), i64::from(ttl));
            }
        }
        SensorKind::Fam(FamSensor::Brainy(s)) => crate::mob::fam::brainy::sensor_tick(t, s),
        SensorKind::Fam(FamSensor::Mount(s)) => crate::mob::fam::mount::sensor_tick(t, s),
        SensorKind::Fam(FamSensor::Warden(s)) => crate::mob::boss::warden::ai::sensor_tick(t, s),
    }
}

/// The living entities (players and mobs) in a box around the mob, sorted by distance.
pub fn living_in(t: &mut Tick<'_, '_>, area: &Aabb, out: &mut Vec<EntityId>) {
    let mut found = std::mem::take(&mut t.cx.ids);
    found.clear();
    t.ents.store.for_each_in(area, false, |id| found.push(id));
    let p = t.pos();
    let me = t.id;
    let mut with_dist: Vec<(f64, EntityId)> = found
        .iter()
        .copied()
        .filter(|&id| id != me && t.other(id).is_some())
        .filter_map(|id| Some((t.ents.store.get(id)?.pos().distance_to_sqr(p), id)))
        .collect();
    with_dist.sort_by(|a, b| a.0.total_cmp(&b.0));
    out.extend(with_dist.into_iter().map(|(_, id)| id));
    t.cx.ids = found;
}

/// `NearestLivingEntitySensor`: the living entities within the follow range by distance and
/// their visible view.
fn nearest_living_entities(t: &mut Tick<'_, '_>) {
    let r = t.mob_ref().attrs.follow_range;
    let area = t.aabb().inflate(r, r, r);
    let mut list = match brain(t).get_mut(MemoryType::NearestLivingEntities) {
        Some(MemValue::Entities(v)) => std::mem::take(v),
        _ => Vec::new(),
    };
    list.clear();
    living_in(t, &area, &mut list);
    let b = brain(t);
    let mut visible = match b.get_mut(MemoryType::NearestVisibleLivingEntities) {
        Some(MemValue::Visible(v)) => std::mem::take(v),
        _ => Visible::default(),
    };
    visible.entities.clear();
    visible.entities.extend_from_slice(&list);
    visible.sight.clear();
    visible.sight.resize(list.len(), 0);
    if list.is_empty() {
        b.erase(MemoryType::NearestLivingEntities);
    } else {
        b.set(MemoryType::NearestLivingEntities, MemValue::Entities(list));
    }
    // `NearestVisibleLivingEntities` is never an empty collection.
    b.set(
        MemoryType::NearestVisibleLivingEntities,
        MemValue::Visible(visible),
    );
}

/// `PlayerSensor`.
fn nearest_players(t: &mut Tick<'_, '_>) {
    let r = t.mob_ref().attrs.follow_range;
    let area = t.aabb().inflate(r, r, r);
    let mut found = std::mem::take(&mut t.cx.ids);
    found.clear();
    t.ents.store.for_each_in(&area, true, |id| found.push(id));
    let p = t.pos();
    let mut players: Vec<(f64, EntityId)> = found
        .iter()
        .copied()
        .filter(|&id| {
            t.ents
                .player(id)
                .is_some_and(|pd| pd.game_mode != GameMode::Spectator)
        })
        .filter_map(|id| {
            let d = t.ents.store.get(id)?.pos().distance_to_sqr(p);
            (d < r * r).then_some((d, id))
        })
        .collect();
    t.cx.ids = found;
    players.sort_by(|a, b| a.0.total_cmp(&b.0));
    let players: Vec<EntityId> = players.into_iter().map(|(_, id)| id).collect();
    let mut visible = Vec::new();
    for &id in &players {
        if sensing::is_targetable(t, id) {
            visible.push(id);
        }
    }
    let mut attackable = Vec::new();
    for &id in &visible {
        if sensing::is_attackable(t, id) {
            attackable.push(id);
        }
    }
    let b = brain(t);
    let first_visible = visible.first().copied();
    let first_attackable = attackable.first().copied();
    b.set_entities(MemoryType::NearestPlayers, players.into_iter());
    b.set_opt(
        MemoryType::NearestVisiblePlayer,
        first_visible.map(MemValue::Entity),
    );
    b.set_entities(
        MemoryType::NearestVisibleAttackablePlayers,
        attackable.into_iter(),
    );
    b.set_opt(
        MemoryType::NearestVisibleAttackablePlayer,
        first_attackable.map(MemValue::Entity),
    );
}

/// `NearestItemSensor`: the nearest wanted item in sight within 32 blocks.
fn nearest_items(t: &mut Tick<'_, '_>) {
    let area = t.aabb().inflate(32.0, 16.0, 32.0);
    let mut found = std::mem::take(&mut t.cx.ids);
    found.clear();
    t.ents.store.for_each_in(&area, false, |id| found.push(id));
    let p = t.pos();
    let mut items: Vec<(f64, EntityId)> = found
        .iter()
        .copied()
        .filter(|&id| t.ents.item(id).is_some())
        .filter_map(|id| Some((t.ents.store.get(id)?.pos().distance_to_sqr(p), id)))
        .collect();
    t.cx.ids = found;
    items.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut best = None;
    for (d, id) in items {
        let Some(stack) = t.ents.item(id).map(|i| i.stack.clone()) else {
            continue;
        };
        if !crate::mob::fam::wants_to_pick_up(t, &stack) || d >= 32.0 * 32.0 {
            continue;
        }
        if item_in_sight(t, id) {
            best = Some(id);
            break;
        }
    }
    brain(t).set_opt(
        MemoryType::NearestVisibleWantedItem,
        best.map(MemValue::Entity),
    );
}

/// `hasLineOfSight` to an item entity (eyes to its position at its eye height).
fn item_in_sight(t: &Tick<'_, '_>, item: EntityId) -> bool {
    let Some(e) = t.ents.store.get(item) else {
        return false;
    };
    let p = t.pos();
    let from = vanillite_base::Vec3::new(p.x, t.eye_y(), p.z);
    let ip = e.pos();
    let to = vanillite_base::Vec3::new(ip.x, ip.y + f64::from(e.dims().height) * 0.85, ip.z);
    to.distance_to_sqr(from) <= 128.0 * 128.0
        && crate::mob::physics::clip(t.level as &dyn crate::mob::physics::Blocks, from, to)
            .is_none()
}

/// `HurtBySensor`.
fn hurt_by(t: &mut Tick<'_, '_>) {
    let now = t.game_time();
    let source = t.mob_ref().last_damage_source(now);
    let cause = t
        .mob_ref()
        .last_hurt_by_mob
        .filter(|_| t.mob_ref().tick_count - t.mob_ref().last_hurt_by_mob_time <= 40);
    if let Some(kind) = source {
        let cause = cause.filter(|&c| t.other(c).is_some());
        let b = brain(t);
        b.set(MemoryType::HurtBy, MemValue::Damage(kind, cause));
        if let Some(c) = cause {
            b.set(MemoryType::HurtByEntity, MemValue::Entity(c));
        }
    } else {
        brain(t).erase(MemoryType::HurtBy);
    }
    if let Some(e) = brain_ref(t).entity(MemoryType::HurtByEntity)
        && t.other(e).is_none()
    {
        brain(t).erase(MemoryType::HurtByEntity);
    }
}

/// Whether an entity's type is in an entity type tag of the data packs.
pub fn in_entity_tag(t: &Tick<'_, '_>, id: EntityId, tag: &str) -> bool {
    use crate::predicate::EntityTags;
    let Some(kind) = t.ents.store.get(id).map(|e| e.kind()) else {
        return false;
    };
    crate::player::play::game_data().is_some_and(|d| d.entity_in_tag(kind, tag))
}

/// Whether an entity is a baby (players never are).
pub fn is_baby(t: &Tick<'_, '_>, id: EntityId) -> bool {
    t.ents.mob(id).is_some_and(|m| m.baby)
}

/// `AdultSensor` / `AdultSensorAnyType`.
fn nearest_adult(t: &mut Tick<'_, '_>, any_type: bool) {
    if !brain_ref(t).has(MemoryType::NearestVisibleLivingEntities) {
        return;
    }
    let own = t.mob_ref().kind;
    let adult = sensing::visible_find_closest(t, |t, id| {
        let Some(m) = t.ents.mob(id) else {
            return false;
        };
        let matches = if any_type {
            in_entity_tag(t, id, "minecraft:followable_friendly_mobs")
        } else {
            m.kind == own
        };
        matches && !m.baby
    });
    brain(t).set_opt(MemoryType::NearestVisibleAdult, adult.map(MemValue::Entity));
}

/// `TemptingSensor`: the nearest player in the tempt range holding a temptation in a hand.
fn tempting(t: &mut Tick<'_, '_>, tag: Option<&'static str>) {
    // `minecraft:tempt_range` is 10 for the brain mobs.
    let range = 10.0;
    let area = t.aabb().inflate(range, range, range);
    let mut found = std::mem::take(&mut t.cx.ids);
    found.clear();
    t.ents.store.for_each_in(&area, true, |id| found.push(id));
    let kind = t.mob_ref().kind;
    let p = t.pos();
    let mut best: Option<(f64, EntityId)> = None;
    for &id in &found {
        let Some(pd) = t.ents.player(id) else {
            continue;
        };
        if pd.game_mode == GameMode::Spectator || pd.survival.is_dead_or_dying() {
            continue;
        }
        let holds = [EquipmentSlot::MainHand, EquipmentSlot::OffHand]
            .iter()
            .any(|&s| {
                let stack = pd.inventory.equipment(s);
                !stack.is_empty()
                    && match tag {
                        Some(tag) => crate::player::play::game_data()
                            .is_some_and(|d| d.item_in_tag(stack.item, tag)),
                        None => crate::mob::animal::is_food(kind, stack.item),
                    }
            });
        if !holds || t.ents.riding.passengers_of(t.id).contains(&id) {
            continue;
        }
        let Some(e) = t.ents.store.get(id) else {
            continue;
        };
        let d = e.pos().distance_to_sqr(p);
        // `forNonCombat().ignoreLineOfSight().range(temptRange)` with the visibility percent.
        let visible = (range * sensing::visibility_percent(t, id)).max(2.0);
        if d > visible * visible {
            continue;
        }
        if best.is_none_or(|(b, _)| d < b) {
            best = Some((d, id));
        }
    }
    t.cx.ids = found;
    brain(t).set_opt(
        MemoryType::TemptingPlayer,
        best.map(|(_, id)| MemValue::Entity(id)),
    );
}
