//! Bats (`Bat`): they hang upside down under solid blocks until a player comes close, then
//! flutter towards random nearby spots.

use vanillite_base::random::{LegacyRandom, RandomSource};
use vanillite_base::{BlockPos, Vec3};
use vanillite_chunk::HeightmapKind;
use vanillite_config::GameMode;
use vanillite_data::block::flags;
use vanillite_world::entity::Rotation;

use super::{block_tags, closer_to_center, mob_spawn_rules, st, st_mut};
use crate::level::Level;
use crate::mob::fam::SpawnReason;
use crate::mob::{Environment, Tick};

/// `isRedstoneConductor`.
fn conductor(level: &Level, pos: BlockPos) -> bool {
    level.block(pos).has(flags::REDSTONE_CONDUCTOR)
}

fn set_resting(t: &mut Tick<'_, '_>, on: bool) {
    if st(t.mob_ref()).resting != on {
        st_mut(t.mob()).resting = on;
        t.core_mut().mark_data_dirty();
    }
}

/// Plays the take-off level event (1025) unless silent.
fn take_off(t: &mut Tick<'_, '_>, pos: BlockPos) {
    if !t.mob_ref().silent {
        t.cx.actions
            .push(crate::mob::WorldAction::LevelEvent(1025, pos, 0));
    }
}

/// `Bat.customServerAiStep`.
pub(super) fn custom_server_ai_step(t: &mut Tick<'_, '_>) {
    let pos = t.block_pos();
    let above = BlockPos::new(pos.x, pos.y + 1, pos.z);
    if st(t.mob_ref()).resting {
        if conductor(t.level, above) {
            if t.mob().random.next_int_bounded(200) == 0 {
                #[allow(clippy::cast_precision_loss)]
                let head = t.mob().random.next_int_bounded(360) as f32;
                let rot = t.core().rot();
                t.core_mut().set_rot(Rotation {
                    y_head_rot: head,
                    ..rot
                });
            }
            // `getNearestPlayer(BAT_RESTING_TARGETING)`: any player within 4 blocks.
            let p = t.pos();
            let area = t.aabb().inflate(4.0, 4.0, 4.0);
            let mut near = false;
            let ents = &*t.ents;
            ents.store.for_each_in(&area, true, |id| {
                if let (Some(e), Some(pd)) = (ents.store.get(id), ents.player(id))
                    && pd.game_mode != GameMode::Spectator
                    && !pd.survival.is_dead_or_dying()
                    && e.pos().distance_to_sqr(p) <= 16.0
                {
                    near = true;
                }
            });
            if near {
                set_resting(t, false);
                take_off(t, pos);
            }
        } else {
            set_resting(t, false);
            take_off(t, pos);
        }
        return;
    }
    let s = st(t.mob_ref());
    if let Some(target) = s.bat_target
        && (!t.level.block(target).is_air() || target.y <= t.level.min_y())
    {
        st_mut(t.mob()).bat_target = None;
    }
    let p = t.pos();
    let reroll = match st(t.mob_ref()).bat_target {
        None => true,
        Some(target) => {
            t.mob().random.next_int_bounded(30) == 0 || closer_to_center(target, p, 2.0)
        }
    };
    if reroll {
        let r = &mut t.mob().random;
        let x = p.x + f64::from(r.next_int_bounded(7)) - f64::from(r.next_int_bounded(7));
        let y = p.y + f64::from(r.next_int_bounded(6)) - 2.0;
        let z = p.z + f64::from(r.next_int_bounded(7)) - f64::from(r.next_int_bounded(7));
        st_mut(t.mob()).bat_target = Some(BlockPos::containing(x, y, z));
    }
    let Some(target) = st(t.mob_ref()).bat_target else {
        return;
    };
    let dx = f64::from(target.x) + 0.5 - p.x;
    let dy = f64::from(target.y) + 0.1 - p.y;
    let dz = f64::from(target.z) + 0.5 - p.z;
    let v = t.velocity();
    let nv = Vec3::new(
        v.x + (dx.signum() * 0.5 - v.x) * f64::from(0.1_f32),
        v.y + (dy.signum() * f64::from(0.7_f32) - v.y) * f64::from(0.1_f32),
        v.z + (dz.signum() * 0.5 - v.z) * f64::from(0.1_f32),
    );
    t.set_velocity(nv);
    #[allow(clippy::cast_possible_truncation)]
    let y_rot_d = crate::mob::atan2_deg(nv.z, nv.x) as f32 - 90.0;
    let rot = t.core().rot();
    let diff = vanillite_base::math::wrap_degrees_f(y_rot_d - rot.y_rot);
    t.mob().zza = 0.5;
    t.core_mut().set_rot(Rotation {
        y_rot: rot.y_rot + diff,
        ..rot
    });
    if t.mob().random.next_int_bounded(100) == 0 && conductor(t.level, above) {
        set_resting(t, true);
    }
}

/// `Bat.tick` after `super.tick()`: resting bats hang still under their block, flying ones
/// lose some vertical speed.
pub(super) fn after_tick(t: &mut Tick<'_, '_>) {
    if st(t.mob_ref()).resting {
        t.set_velocity(Vec3::ZERO);
        let p = t.pos();
        let height = f64::from(t.core().dims().height);
        let y = vanillite_base::math::floor(p.y);
        t.core_mut()
            .set_pos(Vec3::new(p.x, f64::from(y) + 1.0 - height, p.z));
    } else {
        let v = t.velocity();
        t.set_velocity(v.multiply(1.0, 0.6, 1.0));
    }
}

/// `Bat.checkBatSpawnRules`: below the surface, in the dark, on `#bats_spawnable_on`.
pub(super) fn spawn_rules(
    level: &Level,
    env: &Environment,
    pos: BlockPos,
    random: &mut LegacyRandom,
    reason: SpawnReason,
) -> bool {
    let surface = crate::mob::spawn::heightmap_y(level, HeightmapKind::WorldSurface, pos.x, pos.z);
    if pos.y >= surface {
        return false;
    }
    if random.next_boolean() {
        return false;
    }
    let brightness = crate::mob::max_local_raw_brightness(level, env, pos);
    if brightness > random.next_int_bounded(4) {
        return false;
    }
    let below = level.block(BlockPos::new(pos.x, pos.y - 1, pos.z));
    block_tags().bats_spawnable_on.contains(below) && mob_spawn_rules(level, pos, reason)
}
