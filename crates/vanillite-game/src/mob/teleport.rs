//! `LivingEntity.randomTeleport`: a mob moves to a spot below a target position, on a block
//! of `#entities_can_teleport_to`, avoiding blocks a predicate rejects.

use vanillite_base::{BlockPos, Vec3};
use vanillite_world::entity::{EntityFlags, EntityId};

use super::physics;
use crate::entities::Entities;
use crate::level::Level;

/// `randomTeleport(x, y, z, showParticles, avoid)`: walks down from the target (clamped into
/// the world border) to the first block of `#entities_can_teleport_to`; that block and the
/// mob's box there must not be in the block tag `avoid`, the box free of collisions and
/// liquids. Returns whether the mob moved. `show_particles` (entity event 46) is not done here.
pub fn random_teleport(
    level: &Level,
    ents: &mut Entities,
    id: EntityId,
    to: Vec3,
    avoid: &str,
) -> bool {
    let to = level.border.clamp_vec(to.x, to.y, to.z);
    let mut pos = BlockPos::containing(to.x, to.y, to.z);
    if !level.is_loaded(pos) {
        return false;
    }
    let mut y = to.y;
    while pos.y > level.min_y() {
        pos = BlockPos::new(pos.x, pos.y - 1, pos.z);
        let state = level.block(pos);
        if super::spawn::in_block_tag(state, "minecraft:entities_can_teleport_to") {
            if super::spawn::in_block_tag(state, avoid) {
                return false;
            }
            let Some(dims) = ents.store.get(id).map(|e| e.dims()) else {
                return false;
            };
            let dest = Vec3::new(to.x, y, to.z);
            let aabb = dims.bounding_box(dest);
            let mut scratch = Vec::new();
            if !physics::no_collision(&level.view(), &aabb, &mut scratch)
                || physics::contains_any_liquid(level, &aabb)
            {
                return false;
            }
            // `findBlocksIn(aabb).filterState(avoid).anyMatched()`.
            let (lo, hi) = (
                BlockPos::containing(aabb.min_x, aabb.min_y, aabb.min_z),
                BlockPos::containing(aabb.max_x, aabb.max_y, aabb.max_z),
            );
            for x in lo.x..=hi.x {
                for yy in lo.y..=hi.y {
                    for z in lo.z..=hi.z {
                        let b = level.block(BlockPos::new(x, yy, z));
                        if super::spawn::in_block_tag(b, avoid) {
                            return false;
                        }
                    }
                }
            }
            if let Some(mut e) = ents.store.get_mut(id) {
                e.set_pos(dest);
                e.set_velocity(Vec3::ZERO);
                e.set_flags(EntityFlags::HAS_IMPULSE, true);
            }
            if let Some(m) = ents.mob_mut(id) {
                m.nav.stop();
            }
            return true;
        }
        y -= 1.0;
    }
    false
}
