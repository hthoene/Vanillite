//! Target blocks (`TargetBlock`) and daylight detectors (`DaylightDetectorBlock` with its
//! block entity ticker).
//!
//! The level has no day cycle yet: [`sky_darken`] and [`sun_angle`] are the seams the time of
//! day fills in; until then it is always noon (no darkening, sun angle 0°).

use vanillite_base::{BlockPos, Direction, Vec3};
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_world::entity::EntityId;
use vanillite_worldgen::behaviour::StateExt;

use super::{BlockBehaviour, Interaction, UseContext};
use crate::level::signal::{power, with_power};
use crate::level::{BlockEntity, Level, update};

/// Fills the target and daylight detector behaviours of `block`.
pub(super) fn fill(block: BlockId, b: &mut BlockBehaviour) {
    if block == ids::TARGET {
        b.on_place = Some(target_on_place);
        b.tick = Some(target_tick);
        b.signal = Some(own_power);
    } else if block == ids::DAYLIGHT_DETECTOR {
        b.signal = Some(own_power);
        b.use_without_item = Some(daylight_use);
    }
}

/// `ownSignal`: the `power` property.
fn own_power(_level: &Level, state: BlockState, _pos: BlockPos, _direction: Direction) -> i32 {
    power(state)
}

// --- TargetBlock -------------------------------------------------------------------------

/// `BlockState.onProjectileHit` of the block at `pos` that `projectile` hit at `at` (from
/// `Projectile.onHitBlock`, as a world action after the projectile's tick): target blocks.
pub fn projectile_hit_block(level: &mut Level, pos: BlockPos, at: Vec3, projectile: EntityId) {
    if !level.block(pos).is(ids::TARGET) {
        return;
    }
    let ents = &level.entities;
    let Some(kind) = ents.store.get(projectile).map(|e| e.kind()) else {
        return;
    };
    // `projectile instanceof AbstractArrow`: arrows, spectral arrows and tridents.
    let arrow =
        crate::mob::projectile::is_arrow_kind(kind) || kind == vanillite_data::entity::ids::TRIDENT;
    let owner = ents
        .arrow(projectile)
        .and_then(|a| a.owner)
        .or_else(|| ents.missile(projectile).and_then(|m| m.owner))
        .or_else(|| match ents.special(projectile) {
            Some(crate::mob::boss::special::SpecialData::ThrownPotion(p)) => p.owner,
            _ => None,
        });
    // The face the hit point lies on (the target is a full cube).
    let local = at - Vec3::new(f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
    let face = [
        (local.x.abs(), Direction::West),
        ((1.0 - local.x).abs(), Direction::East),
        (local.y.abs(), Direction::Down),
        ((1.0 - local.y).abs(), Direction::Up),
        (local.z.abs(), Direction::North),
        ((1.0 - local.z).abs(), Direction::South),
    ]
    .into_iter()
    .min_by(|a, b| a.0.total_cmp(&b.0))
    .map_or(Direction::Up, |f| f.1);
    target_projectile_hit(level, pos, face, at, arrow, projectile, owner);
}

/// `onProjectileHit`: the projectile code calls this when `projectile` hits a target block at
/// `hit` on `face`; `arrow` is `projectile instanceof AbstractArrow`. A player owner gets the
/// `target_hit` statistic and trigger. Returns the output strength.
pub fn target_projectile_hit(
    level: &mut Level,
    pos: BlockPos,
    face: Direction,
    hit: Vec3,
    arrow: bool,
    projectile: EntityId,
    owner: Option<EntityId>,
) -> i32 {
    let state = level.block(pos);
    if !state.is(ids::TARGET) {
        return 0;
    }
    let strength = target_strength(face, hit);
    let duration = if arrow { 20 } else { 8 };
    if !level.chunks.has_block_tick(pos, ids::TARGET.0) {
        level.set_block_and_update(pos, with_power(state, strength));
        level.schedule_block_tick(pos, ids::TARGET, duration);
    }
    let owner = owner.filter(|&o| {
        level
            .entities
            .store
            .get(o)
            .is_some_and(|e| e.kind() == vanillite_data::entity::ids::PLAYER)
    });
    if let Some(owner) = owner {
        level.award_stat(owner, crate::stats::custom::TARGET_HIT, 1);
        if let Some(view) = crate::advancements::view::snapshot(&level.entities, projectile) {
            let event = crate::advancements::trigger::Event::TargetHit(view, strength);
            level.trigger(owner, event);
        }
    }
    strength
}

/// `getRedstoneStrength(hitResult, hitLocation)`: 15 in the centre, down to 1 at the edge.
fn target_strength(face: Direction, hit: Vec3) -> i32 {
    let frac = |v: f64| (v - v.floor() - 0.5).abs();
    let (dx, dy, dz) = (frac(hit.x), frac(hit.y), frac(hit.z));
    let distance = match face {
        Direction::Up | Direction::Down => dx.max(dz),
        Direction::North | Direction::South => dx.max(dy),
        Direction::West | Direction::East => dy.max(dz),
    };
    let scaled = 15.0 * ((0.5 - distance) / 0.5).clamp(0.0, 1.0);
    1.max(scaled.ceil() as i32)
}

/// `tick`: the output resets.
fn target_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    if power(state) != 0 {
        level.set_block_and_update(pos, with_power(state, 0));
    }
}

/// `onPlace`: a powered target without a pending reset loses its power (flags 18: clients,
/// known shape).
fn target_on_place(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    old: BlockState,
    _moved: bool,
) {
    if !state.is(old.block())
        && power(state) > 0
        && !level.chunks.has_block_tick(pos, ids::TARGET.0)
    {
        level.set_block(
            pos,
            with_power(state, 0),
            update::CLIENTS | update::KNOWN_SHAPE,
        );
    }
}

// --- DaylightDetectorBlock ---------------------------------------------------------------

/// `level.getSkyDarken()`: 0 until the level has a time of day.
pub fn sky_darken(_level: &Level) -> i32 {
    0
}

/// The `SUN_ANGLE` environment attribute at `pos` in degrees: 0 (noon) until the level has a
/// time of day.
pub fn sun_angle(_level: &Level, _pos: BlockPos) -> f32 {
    0.0
}

/// `updateSignalStrength(state, level, pos)`.
fn update_signal_strength(level: &mut Level, state: BlockState, pos: BlockPos) {
    // `getEffectiveSkyBrightness`.
    let mut target = level.light(pos).0 - sky_darken(level);
    let mut angle = sun_angle(level, pos) * (std::f32::consts::PI / 180.0);
    if state.flag("inverted") {
        target = 15 - target;
    } else if target > 0 {
        let offset = if angle < std::f32::consts::PI {
            0.0
        } else {
            std::f32::consts::PI * 2.0
        };
        angle += (offset - angle) * 0.2;
        // `Math.round(float)`.
        target = (target as f32 * mth_cos(angle) + 0.5).floor() as i32;
    }
    let target = target.clamp(0, 15);
    if power(state) != target {
        level.set_block_and_update(pos, with_power(state, target));
    }
}

/// `useWithoutItem`: builders switch between day and night mode.
fn daylight_use(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    context: &UseContext,
) -> Interaction {
    if !context.may_build {
        return Interaction::Pass;
    }
    let new_state = state.set_flag("inverted", !state.flag("inverted"));
    level.set_block(pos, new_state, update::CLIENTS);
    let context =
        crate::level::game_event::GameEventContext::with_state(Some(context.player), new_state);
    level.game_event_block(
        crate::level::game_event::events().block_change,
        pos,
        context,
    );
    update_signal_strength(level, new_state, pos);
    Interaction::Success
}

/// The new block entity of a daylight detector.
pub fn new_daylight_detector() -> BlockEntity {
    BlockEntity::DaylightDetector
}

/// `DaylightDetectorBlock.tickEntity`: every 20 game ticks in dimensions with sky light (no
/// ticker without sky light).
pub fn daylight_detector_tick(level: &mut Level, pos: BlockPos, state: BlockState) {
    if level.settings.has_sky_light && level.game_time() % 20 == 0 {
        update_signal_strength(level, state, pos);
    }
}

/// `Mth.cos(value)`: Vanilla's 65536 entry sine table.
fn mth_cos(value: f32) -> f32 {
    let index = ((value * 10430.378 + 16384.0) as i32) & 65535;
    (f64::from(index) * std::f64::consts::PI * 2.0 / 65536.0).sin() as f32
}

#[cfg(test)]
mod tests {
    use vanillite_world::entity::NewEntity;

    use super::*;
    use crate::level::tests::flat_level;

    /// A projectile hitting a target block powers it by how close to the centre of the face it
    /// hit, for 8 ticks (20 for arrows) (`TargetBlock.onProjectileHit`, #119).
    #[test]
    fn projectiles_power_target_blocks() {
        use vanillite_data::entity::ids as entities;
        for (kind, at, strength) in [
            (entities::SNOWBALL, Vec3::new(0.5, -59.0, 0.5), 15),
            (entities::ARROW, Vec3::new(0.0, -59.75, 0.5), 8),
        ] {
            let mut level = flat_level();
            let pos = BlockPos::new(0, -60, 0);
            assert!(level.set_block(pos, ids::TARGET.default_state(), update::ALL));
            let network_id = level.network_ids().next();
            let new = NewEntity::new(kind, network_id, 1, at);
            let id = level.entities.store.spawn(new, |_| 0);
            projectile_hit_block(&mut level, pos, at, id);
            assert_eq!(power(level.block(pos)), strength, "{}", kind.name());
            assert!(level.chunks.has_block_tick(pos, ids::TARGET.0));
        }
    }
}
