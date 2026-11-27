//! The positions `ServerPlayer` remembers for its distance triggers (`levitationStartPos`,
//! `startingToFallPosition`, `enteredNetherPosition`, `enteredLavaOnVehiclePosition`,
//! `currentExplosionImpactPos`) and the triggers of `ServerPlayer.tick` that use them.

use vanillite_base::Vec3;
use vanillite_nbt::{Compound, Tag};
use vanillite_world::entity::EntityId;

use super::trigger::{Event, Snapshot, TriggerKind};
use crate::entities::Entities;
use crate::player::PlayerData;

/// A player's remembered positions.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Tracking {
    /// `levitationStartPos` and `levitationStartTime`.
    pub levitation: Option<(Vec3, i32)>,
    /// `startingToFallPosition`.
    pub falling_from: Option<Vec3>,
    /// `enteredNetherPosition` (saved as `entered_nether_pos`).
    pub entered_nether: Option<Vec3>,
    /// `enteredLavaOnVehiclePosition`.
    pub lava_vehicle_from: Option<Vec3>,
    /// `currentExplosionImpactPos` (saved as `last_explosion_impact_pos`); the cause is
    /// [`super::PlayerAdvancements::explosion_cause`].
    pub explosion: Option<Vec3>,
}

fn vec3_tag(v: Vec3) -> Tag {
    Tag::List(vanillite_nbt::List(vec![
        Tag::Double(v.x),
        Tag::Double(v.y),
        Tag::Double(v.z),
    ]))
}

fn vec3_of(tag: Option<&Tag>) -> Option<Vec3> {
    match tag {
        Some(Tag::List(l)) => match l.0.as_slice() {
            [Tag::Double(x), Tag::Double(y), Tag::Double(z)]
                if x.is_finite() && y.is_finite() && z.is_finite() =>
            {
                Some(Vec3::new(*x, *y, *z))
            }
            _ => None,
        },
        _ => None,
    }
}

impl Tracking {
    /// Reads `entered_nether_pos` and `last_explosion_impact_pos` of a player data file.
    pub fn load(data: &Compound) -> Tracking {
        Tracking {
            entered_nether: vec3_of(data.get("entered_nether_pos")),
            explosion: vec3_of(data.get("last_explosion_impact_pos")),
            ..Tracking::default()
        }
    }

    /// Writes `entered_nether_pos` and `last_explosion_impact_pos`.
    pub fn save(&self, out: &mut Compound) {
        out.remove("entered_nether_pos");
        out.remove("last_explosion_impact_pos");
        if let Some(p) = self.entered_nether {
            out.insert("entered_nether_pos", vec3_tag(p));
        }
        if let Some(p) = self.explosion {
            out.insert("last_explosion_impact_pos", vec3_tag(p));
        }
    }

    /// What `restoreFrom` keeps for the respawned player.
    pub fn respawned(&self) -> Tracking {
        Tracking {
            entered_nether: self.entered_nether,
            explosion: self.explosion,
            ..Tracking::default()
        }
    }
}

/// `ServerPlayer.resetFallDistance`: `FALL_FROM_HEIGHT` from where the fall started.
pub fn reset_fall_distance(player: &mut PlayerData) {
    let s = &mut player.survival;
    if let Some(start) = s.tracking.falling_from.take()
        && s.health > 0.0
    {
        let kind = TriggerKind::FALL_FROM_HEIGHT;
        player
            .advancements
            .trigger_with(kind, || Event::Distance(kind, start));
    }
    player.survival.fall_distance = 0.0;
}

/// `ServerPlayer.onExplosionHit`: where the explosion hit the player and the view of what
/// caused it, kept as it is now (`currentExplosionCause` keeps the entity object, also after
/// it is gone).
pub fn explosion_hit(player: &mut PlayerData, pos: Vec3, cause: Option<Snapshot>) {
    player.survival.tracking.explosion = Some(pos);
    player.advancements.explosion_cause = cause;
}

/// `ServerPlayer.onEffectAdded` of Levitation.
pub fn levitation_started(player: &mut PlayerData, pos: Vec3) {
    player.survival.tracking.levitation = Some((pos, player.survival.tick_count));
}

/// `ServerPlayer.tick`: `LEVITATION`, `trackStartFallingPosition` (with
/// `FALL_AFTER_EXPLOSION`) and `trackEnteredOrExitedLavaOnVehicle` (`RIDE_ENTITY_IN_LAVA`).
pub fn tick(ents: &mut Entities, id: EntityId) {
    let Some(pos) = ents.store.get(id).map(|e| e.pos()) else {
        return;
    };
    let vehicle = ents.riding.vehicle_of(id);
    let vehicle_in_lava = vehicle
        .and_then(|v| ents.mob(v))
        .is_some_and(|m| m.fluids.in_lava());
    let Some(p) = ents.player_mut(id) else {
        return;
    };
    {
        let s = &mut p.survival;
        let t = &mut s.tracking;
        if let Some((start, since)) = t.levitation {
            let ticks = s.tick_count.wrapping_sub(since);
            p.advancements
                .trigger_with(TriggerKind::LEVITATION, || Event::Levitation(start, ticks));
        }
        let s = &mut p.survival;
        let t = &mut s.tracking;
        if s.fall_distance > 0.0 && t.falling_from.is_none() {
            t.falling_from = Some(pos);
            let impact = t.explosion.take();
            let cause = p.advancements.explosion_cause.take();
            if let Some(impact) = impact
                && impact.y <= pos.y
            {
                let kind = TriggerKind::FALL_AFTER_EXPLOSION;
                p.advancements
                    .trigger_with(kind, || Event::FallAfterExplosion(impact, cause));
            }
        }
        let t = &mut p.survival.tracking;
        if vehicle_in_lava {
            match t.lava_vehicle_from {
                None => t.lava_vehicle_from = Some(pos),
                Some(start) => {
                    let kind = TriggerKind::RIDE_ENTITY_IN_LAVA;
                    p.advancements
                        .trigger_with(kind, || Event::Distance(kind, start));
                }
            }
        }
        let t = &mut p.survival.tracking;
        if t.lava_vehicle_from.is_some() && !vehicle_in_lava {
            t.lava_vehicle_from = None;
        }
    }
}
