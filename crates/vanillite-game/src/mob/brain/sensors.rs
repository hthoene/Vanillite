//! The brain context of mobs ([`crate::mob::Tick`]) and the sensors shared by brain driven
//! kinds (`PlayerSensor`, `NearestLivingEntitySensor`), with the targeting tests they use
//! (`Sensor.isEntityTargetable`, `TargetingConditions`).

use vanillite_base::Aabb;
use vanillite_base::random::RandomSource;
use vanillite_config::GameMode;
use vanillite_world::entity::{EntityFlags, EntityId};

use super::{BrainContext, Memories, MemoryType as M, MemoryValue, PositionTracker};
use crate::mob::Tick;
use crate::mob::living::has_line_of_sight;
use crate::survival::rules::Difficulty;

impl BrainContext for Tick<'_, '_> {
    fn game_time(&self) -> i64 {
        self.env.game_time
    }

    fn level_random_int(&mut self, bound: i32) -> i32 {
        self.env.random.next_int_bounded(bound.max(1))
    }

    fn shuffle_random(&mut self) -> f32 {
        self.mob().random.next_float()
    }
}

/// Whether `id` is a living entity that is alive (players not dead, mobs alive).
pub fn is_alive(t: &Tick<'_, '_>, id: EntityId) -> bool {
    if let Some(p) = t.ents.player(id) {
        return !p.survival.is_dead_or_dying();
    }
    t.ents.mob(id).is_some_and(crate::mob::MobData::is_alive)
}

/// `TargetingConditions.test(level, body, target)` with the follow range of the body:
/// `combat` adds `canAttack` (players in survival or adventure, not in peaceful), `invisibility`
/// scales the range for sneaking players, `line_of_sight` needs a clear view.
pub fn test_target(
    t: &mut Tick<'_, '_>,
    target: EntityId,
    combat: bool,
    invisibility: bool,
    line_of_sight: bool,
) -> bool {
    if target == t.id || !is_alive(t, target) {
        return false;
    }
    let player = t.ents.player(target).map(|p| p.game_mode);
    // `canBeSeenByAnyone`: not a spectator.
    if player == Some(GameMode::Spectator) {
        return false;
    }
    if combat
        && let Some(mode) = player
        && (t.env.difficulty == Difficulty::Peaceful
            || !matches!(mode, GameMode::Survival | GameMode::Adventure))
    {
        return false;
    }
    let range = t.mob_ref().attrs.follow_range;
    let Some(e) = t.ents.store.get(target) else {
        return false;
    };
    if range > 0.0 {
        let modifier = if invisibility && e.flags().contains(EntityFlags::CROUCHING) {
            0.8
        } else {
            1.0
        };
        let visible = (range * modifier).max(2.0);
        if t.pos().distance_to_sqr(e.pos()) > visible * visible {
            return false;
        }
    }
    !line_of_sight || has_line_of_sight(t, target)
}

/// `Sensor.isEntityTargetable(level, body, target)`.
pub fn is_entity_targetable(t: &mut Tick<'_, '_>, mem: &Memories, target: EntityId) -> bool {
    let is_attack_target = mem.entity(M::AttackTarget) == Some(target);
    test_target(t, target, false, !is_attack_target, true)
}

/// `Sensor.isEntityAttackable(level, body, target)`.
pub fn is_entity_attackable(t: &mut Tick<'_, '_>, mem: &Memories, target: EntityId) -> bool {
    let is_attack_target = mem.entity(M::AttackTarget) == Some(target);
    test_target(t, target, true, !is_attack_target, true)
}

/// `NearestVisibleLivingEntities.contains(entity)`: in the sensed list and targetable.
pub fn visible_contains(t: &mut Tick<'_, '_>, mem: &Memories, target: EntityId) -> bool {
    mem.entities(M::NearestVisibleLivingEntities)
        .contains(&target)
        && is_entity_targetable(t, mem, target)
}

/// `NearestVisibleLivingEntities.findClosest(filter)`.
pub fn find_closest_visible(
    t: &mut Tick<'_, '_>,
    mem: &Memories,
    mut filter: impl FnMut(&mut Tick<'_, '_>, EntityId) -> bool,
) -> Option<EntityId> {
    let list = mem.entities(M::NearestVisibleLivingEntities).to_vec();
    list.into_iter()
        .find(|&e| filter(t, e) && is_entity_targetable(t, mem, e))
}

/// `PositionTracker.isVisibleBy(body)`.
pub fn tracker_visible(t: &mut Tick<'_, '_>, mem: &Memories, tracker: PositionTracker) -> bool {
    match tracker {
        PositionTracker::Block(_) => true,
        PositionTracker::Entity { id, .. } => {
            let living = t.ents.player(id).is_some() || t.ents.mob(id).is_some();
            if !living {
                return t.ents.store.get(id).is_some();
            }
            is_alive(t, id) && visible_contains(t, mem, id)
        }
    }
}

/// `PositionTracker.currentPosition()`: a block's centre, an entity's position or eyes.
pub fn tracker_position(
    t: &Tick<'_, '_>,
    tracker: PositionTracker,
) -> Option<vanillite_base::Vec3> {
    match tracker {
        PositionTracker::Block(p) => Some(vanillite_base::Vec3::new(
            f64::from(p.x) + 0.5,
            f64::from(p.y) + 0.5,
            f64::from(p.z) + 0.5,
        )),
        PositionTracker::Entity { id, eye } => {
            let o = t.other(id)?;
            Some(if eye {
                vanillite_base::Vec3::new(o.pos.x, o.eye_y, o.pos.z)
            } else {
                o.pos
            })
        }
    }
}

/// `PositionTracker.currentBlockPosition()`.
pub fn tracker_block(
    t: &Tick<'_, '_>,
    tracker: PositionTracker,
) -> Option<vanillite_base::BlockPos> {
    match tracker {
        PositionTracker::Block(p) => Some(p),
        PositionTracker::Entity { id, .. } => {
            let p = t.ents.store.get(id)?.pos();
            Some(vanillite_base::BlockPos::containing(p.x, p.y, p.z))
        }
    }
}

/// `PlayerSensor.doTick`: the players within follow range by distance, the nearest visible
/// one, the visible attackable ones and the nearest of those.
pub fn sense_players(t: &mut Tick<'_, '_>, mem: &mut Memories) {
    let range = t.mob_ref().attrs.follow_range;
    let p = t.pos();
    let mut players: Vec<(EntityId, f64)> = t
        .ents
        .players
        .iter()
        .filter(|(_, d)| d.game_mode != GameMode::Spectator)
        .filter_map(|(id, _)| {
            let d = t.ents.store.get(id)?.pos().distance_to_sqr(p);
            (d < range * range).then_some((id, d))
        })
        .collect();
    players.sort_by(|a, b| a.1.total_cmp(&b.1));
    mem.set_entities_with(M::NearestPlayers, |v| {
        v.extend(players.iter().map(|&(id, _)| id));
    });
    let mut visible = Vec::new();
    for &(id, _) in &players {
        if is_entity_targetable(t, mem, id) {
            visible.push(id);
        }
    }
    mem.set_opt(
        M::NearestVisiblePlayer,
        visible.first().map(|&e| MemoryValue::Entity(e)),
    );
    let mut attackable = Vec::new();
    for &id in &visible {
        if is_entity_attackable(t, mem, id) {
            attackable.push(id);
        }
    }
    mem.set_opt(
        M::NearestVisibleAttackablePlayer,
        attackable.first().map(|&e| MemoryValue::Entity(e)),
    );
    mem.set_entities_with(M::NearestVisibleAttackablePlayers, |v| {
        v.extend_from_slice(&attackable);
    });
}

/// `NearestLivingEntitySensor.doTick`: the living entities in the box inflated by the follow
/// range, by distance (visibility is tested when the visible list is read).
pub fn sense_living_entities(t: &mut Tick<'_, '_>, mem: &mut Memories) {
    let range = t.mob_ref().attrs.follow_range;
    let b = t.aabb();
    let area = Aabb::new(
        b.min_x - range,
        b.min_y - range,
        b.min_z - range,
        b.max_x + range,
        b.max_y + range,
        b.max_z + range,
    );
    let id = t.id;
    let p = t.pos();
    let mut found = std::mem::take(&mut t.cx.ids);
    found.clear();
    t.ents.store.for_each_in(&area, false, |e| {
        if e != id {
            found.push(e);
        }
    });
    let mut living: Vec<(EntityId, f64)> = found
        .iter()
        .filter(|&&e| is_alive(t, e))
        .filter_map(|&e| {
            let ent = t.ents.store.get(e)?;
            ent.aabb()
                .intersects(&area)
                .then(|| (e, ent.pos().distance_to_sqr(p)))
        })
        .collect();
    t.cx.ids = found;
    living.sort_by(|a, b| a.1.total_cmp(&b.1));
    mem.set_entities_with(M::NearestLivingEntities, |v| {
        v.extend(living.iter().map(|&(e, _)| e));
    });
    mem.set_entities_with(M::NearestVisibleLivingEntities, |v| {
        v.extend(living.iter().map(|&(e, _)| e));
    });
}
