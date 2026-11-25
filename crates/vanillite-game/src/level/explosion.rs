//! Explosions (`ServerLevel.explode`, `ServerExplosion`).
//!
//! An explosion casts Vanilla's 1352 rays from its centre (the surface of a 16³ grid), each with
//! a random power that blast and fluid resistance wear down in 0.3 block steps; every block a ray
//! still has power at is blown up. Entities in range are pushed away by their exposure (the share
//! of rays from their box to the centre that no block stops), then the blown up blocks are
//! destroyed in random order and their drops spawn after all of them.
//!
//! The damage of the living entities hit is queued in [`super::LevelOutput::explosion_hurts`]
//! for the mob system, which applies it right after the explosion's tick step.
//!
//! Items lose health and are destroyed, boats take damage like living entities.
//!
//! Not ported: damage of end crystals and other non-living entities, fire of fire explosions, the
//! block reactions of `TRIGGER_BLOCK` explosions (wind charges) and the special damage
//! calculators (wither skulls, wind charges, TNT that used a portal).

use vanillite_base::hash::FxHashSet;
use vanillite_base::math::{floor, frac, lerp};
use vanillite_base::random::RandomSource;
use vanillite_base::{Aabb, BlockPos, Vec3};
use vanillite_data::block::{BlockState, ids};
use vanillite_data::entity::ids as entity_ids;
use vanillite_world::entity::{EntityFlags, EntityId};
use vanillite_worldgen::behaviour::StateExt;

use super::{Level, update};
use crate::block::{drops, tnt};

/// What an explosion does to blocks (`Explosion.BlockInteraction`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockInteraction {
    /// `KEEP`: blocks stay.
    Keep,
    /// `DESTROY`: blocks break and drop everything.
    Destroy,
    /// `DESTROY_WITH_DECAY`: blocks break and drops decay with the radius.
    DestroyWithDecay,
    /// `TRIGGER_BLOCK`: blocks react (buttons, doors) instead of breaking.
    TriggerBlock,
}

/// The exploding entity's `getBlockExplosionResistance` override.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BlockResistance {
    /// The block's and fluid's own resistance.
    #[default]
    Default,
    /// `WitherSkull` (dangerous): at most 0.8 for blocks the wither can destroy.
    DangerousWitherSkull,
    /// `MinecartTNT`: rails and the blocks under them have no resistance and do not break.
    TntMinecart,
    /// `AbstractWindCharge.EXPLOSION_DAMAGE_CALCULATOR`: `#blocks_wind_charge_explosions` are
    /// immune.
    WindCharge,
}

/// An explosion for the clients (`ClientboundExplodePacket`), sent to players within 64 blocks.
#[derive(Clone, Debug, PartialEq)]
pub struct ExplosionEvent {
    /// Centre.
    pub center: Vec3,
    /// Radius (power).
    pub radius: f32,
    /// Number of blocks blown up.
    pub block_count: i32,
    /// `isSmall()`: the small particle instead of the emitter.
    pub small: bool,
    /// Knockback of the players it hit (`getHitPlayers`).
    pub hit_players: Vec<(EntityId, Vec3)>,
    /// The burst sound of a wind charge (gust particles, no block particles); `None` for
    /// normal explosions.
    pub wind_sound: Option<&'static str>,
}

/// How an explosion treats entities (`ExplosionDamageCalculator`) and how clients show it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExplosionStyle {
    /// `shouldDamageEntity`.
    pub damages_entities: bool,
    /// `getKnockbackMultiplier`.
    pub knockback: f64,
    /// The wind charge burst sound, see [`ExplosionEvent::wind_sound`].
    pub wind_sound: Option<&'static str>,
    /// The source's block resistance override.
    pub resistance: BlockResistance,
}

impl Default for ExplosionStyle {
    fn default() -> Self {
        ExplosionStyle {
            damages_entities: true,
            knockback: 1.0,
            wind_sound: None,
            resistance: BlockResistance::Default,
        }
    }
}

/// The damage an explosion deals to a living entity (`entity.hurtServer(level,
/// damageSources().explosion(source, indirect), amount)`), applied by the mob system.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExplosionHurt {
    /// The entity hit.
    pub target: EntityId,
    /// `getEntityDamageAmount`.
    pub amount: f32,
    /// The exploding entity (`getDirectSourceEntity`).
    pub source: Option<EntityId>,
    /// The entity that caused it (`getIndirectSourceEntity`).
    pub indirect: Option<EntityId>,
    /// Centre (the damage position without a source entity).
    pub center: Vec3,
}

impl Level {
    /// `ServerLevel.explode(source, ..., x, y, z, radius, fire = false, interaction)` with the
    /// default particles and sound. `source` is the exploding entity (already removed or not),
    /// `indirect` the entity that caused it (`getIndirectSourceEntity`). Returns the number of
    /// blocks blown up.
    pub fn explode(
        &mut self,
        source: Option<EntityId>,
        indirect: Option<EntityId>,
        center: Vec3,
        radius: f32,
        interaction: BlockInteraction,
    ) -> usize {
        self.explode_with(
            source,
            indirect,
            center,
            radius,
            interaction,
            ExplosionStyle::default(),
        )
    }

    /// [`Self::explode`] with a damage calculator and client style (wind charges: no damage,
    /// more knockback, gusts).
    pub fn explode_with(
        &mut self,
        source: Option<EntityId>,
        indirect: Option<EntityId>,
        center: Vec3,
        radius: f32,
        interaction: BlockInteraction,
        style: ExplosionStyle,
    ) -> usize {
        let context = super::game_event::GameEventContext::of(source);
        self.game_event(super::game_event::events().explode, center, context);
        let mut targets = self.exploded_positions(center, radius, style.resistance);
        let hit_players = self.hurt_entities(source, indirect, center, radius, style);
        if interaction != BlockInteraction::Keep {
            self.interact_with_blocks(&mut targets, source, indirect, interaction, radius);
        }
        let count = targets.len();
        self.output.explosions.push(ExplosionEvent {
            center,
            radius,
            block_count: i32::try_from(count).unwrap_or(i32::MAX),
            small: radius < 2.0 || interaction == BlockInteraction::Keep,
            hit_players,
            wind_sound: style.wind_sound,
        });
        count
    }

    /// `calculateExplodedPositions`: the blocks the rays reach, in the iteration order of
    /// Vanilla's `HashSet<BlockPos>`.
    fn exploded_positions(
        &mut self,
        center: Vec3,
        radius: f32,
        override_resistance: BlockResistance,
    ) -> Vec<BlockPos> {
        let mut seen = FxHashSet::default();
        let mut positions = Vec::new();
        for xx in 0..16 {
            for yy in 0..16 {
                for zz in 0..16 {
                    if !(xx == 0 || xx == 15 || yy == 0 || yy == 15 || zz == 0 || zz == 15) {
                        continue;
                    }
                    // Float arithmetic, widened to double.
                    let mut xd = f64::from(xx as f32 / 15.0 * 2.0 - 1.0);
                    let mut yd = f64::from(yy as f32 / 15.0 * 2.0 - 1.0);
                    let mut zd = f64::from(zz as f32 / 15.0 * 2.0 - 1.0);
                    let d = (xd * xd + yd * yd + zd * zd).sqrt();
                    xd /= d;
                    yd /= d;
                    zd /= d;
                    let mut power = radius * (0.7 + self.random.next_float() * 0.6);
                    let (mut xp, mut yp, mut zp) = (center.x, center.y, center.z);
                    let step = f64::from(0.3_f32);
                    while power > 0.0 {
                        let pos = BlockPos::containing(xp, yp, zp);
                        if !self.in_height(pos.y) || !in_world_bounds_horizontal(pos) {
                            break;
                        }
                        let state = self.block(pos);
                        let fluid = state.fluid_state();
                        // `ExplosionDamageCalculator.getBlockExplosionResistance`.
                        let rail_protected = override_resistance == BlockResistance::TntMinecart
                            && (crate::block::is_rail(state.block())
                                || crate::block::is_rail(self.block(pos.offset(0, 1, 0)).block()));
                        if !(state.is_air() && fluid.is_empty()) {
                            let mut resistance = state
                                .block()
                                .info()
                                .blast_resistance
                                .max(fluid.info().explosion_resistance);
                            if override_resistance == BlockResistance::DangerousWitherSkull {
                                resistance = crate::mob::boss::wither::skull_block_resistance(
                                    true, state, resistance,
                                );
                            }
                            if override_resistance == BlockResistance::WindCharge
                                && crate::block::block_tags()
                                    .blocks_wind_charge_explosions
                                    .contains(state)
                            {
                                resistance = 3_600_000.0;
                            }
                            power -= (resistance + 0.3) * 0.3;
                        }
                        if power > 0.0 && !rail_protected && seen.insert(pos) {
                            positions.push(pos);
                        }
                        xp += xd * step;
                        yp += yd * step;
                        zp += zd * step;
                        power -= 0.225_000_01;
                    }
                }
            }
        }
        java_hash_set_order(&mut positions);
        positions
    }

    /// `hurtEntities`: queues the damage of the living entities in range and pushes the
    /// entities away; players get their knockback with the explosion packet instead.
    fn hurt_entities(
        &mut self,
        source: Option<EntityId>,
        indirect: Option<EntityId>,
        center: Vec3,
        radius: f32,
        style: ExplosionStyle,
    ) -> Vec<(EntityId, Vec3)> {
        let mut hit_players = Vec::new();
        if radius < 1.0e-5 {
            return hit_players;
        }
        let double_radius = radius * 2.0;
        let r = f64::from(double_radius) + 1.0;
        let area = Aabb::new(
            f64::from(floor(center.x - r)),
            f64::from(floor(center.y - r)),
            f64::from(floor(center.z - r)),
            f64::from(floor(center.x + r)),
            f64::from(floor(center.y + r)),
            f64::from(floor(center.z + r)),
        );
        let mut found = Vec::new();
        self.entities.store.for_each_in(&area, false, |id| {
            if Some(id) != source {
                found.push(id);
            }
        });
        for id in found {
            let Some(entity) = self.entities.store.get(id) else {
                continue;
            };
            let kind = entity.kind();
            let (pos, aabb) = (entity.pos(), entity.aabb());
            // `getEntities(..., NO_SPECTATORS)`.
            if self
                .entities
                .player(id)
                .is_some_and(|p| p.game_mode == vanillite_config::GameMode::Spectator)
            {
                continue;
            }
            let dist = pos.distance_to_sqr(center).sqrt() / f64::from(double_radius);
            if dist > 1.0 {
                continue;
            }
            // `ExplosionDamageCalculator.getEntityDamageAmount`: living entities and boats
            // through the mob system, items here (`ItemEntity.hurtServer`: health 5).
            let is_boat = crate::vehicle::BoatKind::of(kind).is_some()
                || crate::minecart::MinecartKind::of(kind).is_some();
            if kind == entity_ids::ITEM && style.damages_entities {
                let double_radius = f64::from(double_radius);
                let pow = (1.0 - dist) * f64::from(self.seen_percent(center, aabb));
                #[allow(clippy::cast_possible_truncation)]
                let amount = ((pow * pow + pow) / 2.0 * 7.0 * double_radius + 1.0) as f32;
                let destroyed = self.entities.item_mut(id).is_some_and(|d| {
                    // The nether star resists explosions (`damage_resistant`).
                    if crate::item::item_id(&d.stack).name() == "minecraft:nether_star" {
                        return false;
                    }
                    #[allow(clippy::cast_possible_truncation)]
                    let health = (f32::from(d.health) - amount) as i16;
                    d.health = health;
                    health <= 0
                });
                if destroyed {
                    self.entities.despawn(id);
                    continue;
                }
            }
            let player = self.entities.player(id);
            let mob = self.entities.mob(id);
            let eye = match mob {
                Some(mob) => crate::mob::eye_height(mob),
                None => f64::from(kind.info().eye_height),
            };
            let origin = if kind == entity_ids::TNT {
                pos
            } else {
                Vec3::new(pos.x, pos.y + eye, pos.z)
            };
            let direction = (origin - center).normalize();
            let seen = self.seen_percent(center, aabb);
            let exposure = f64::from(seen);
            // `ExplosionDamageCalculator.getEntityDamageAmount` for the living entities.
            let crystal = kind == entity_ids::END_CRYSTAL;
            if style.damages_entities
                && (player.is_some()
                    || crystal
                    || mob.is_some_and(crate::mob::MobData::is_alive)
                    || is_boat)
            {
                let double_radius = f64::from(double_radius);
                let pow = (1.0 - dist) * exposure;
                #[allow(clippy::cast_possible_truncation)]
                let amount = ((pow * pow + pow) / 2.0 * 7.0 * double_radius + 1.0) as f32;
                self.output.explosion_hurts.push(ExplosionHurt {
                    target: id,
                    amount,
                    source,
                    indirect,
                    center,
                });
            }
            let resistance = mob.map_or(0.0, |m| m.attrs.explosion_knockback_resistance);
            let knockback =
                direction * ((1.0 - dist) * exposure * style.knockback * (1.0 - resistance));
            if let Some(p) = player {
                hit_players.push((id, knockback));
                // A client moves the player by the packet, unless the server moves it itself.
                if !p.survival.simulate_movement {
                    continue;
                }
            }
            // `pushFromExplosion`.
            if let Some(mut entity) = self.entities.store.get_mut(id)
                && knockback.x.is_finite()
                && knockback.y.is_finite()
                && knockback.z.is_finite()
            {
                let velocity = entity.as_ref().velocity() + knockback;
                entity.set_velocity(velocity);
                entity.set_flags(EntityFlags::HAS_IMPULSE, true);
            }
        }
        hit_players
    }

    /// `interactWithBlocks`: the blocks in random order, then their drops.
    fn interact_with_blocks(
        &mut self,
        targets: &mut [BlockPos],
        source: Option<EntityId>,
        indirect: Option<EntityId>,
        interaction: BlockInteraction,
        radius: f32,
    ) {
        // `Util.shuffle(targetBlocks, level.random)`.
        for i in (2..=targets.len()).rev() {
            let swap = self.random.next_int_bounded(i as i32) as usize;
            targets.swap(i - 1, swap);
        }
        // `StackCollector`s: the drops merged into stacks of up to 16 (`addOrAppendStack`).
        let mut stacks: Vec<(BlockPos, crate::item::ItemStack)> = Vec::new();
        let mut loot = Vec::new();
        let decay = (interaction == BlockInteraction::DestroyWithDecay).then_some(radius);
        for &pos in targets.iter() {
            // `Block.onExplosionHit`.
            let state = self.block(pos);
            if state.is_air() || interaction == BlockInteraction::TriggerBlock {
                continue;
            }
            // `dropFromExplosion`: everything but TNT; the loot is rolled block by block.
            if !state.is(ids::TNT) {
                loot.clear();
                drops::explosion_loot(self, pos, state, decay, source, &mut loot);
                for mut stack in loot.drain(..) {
                    for (_, collected) in &mut stacks {
                        if crate::item_entity::are_mergable(collected, &stack) {
                            *collected =
                                crate::item_entity::merge_stacks_max(collected, &mut stack, 16);
                        }
                        if stack.is_empty() {
                            break;
                        }
                    }
                    if !stack.is_empty() {
                        stacks.push((pos, stack));
                    }
                }
            }
            self.set_block(pos, BlockState::AIR, update::ALL);
            // `wasExploded`.
            if state.is(ids::TNT) {
                tnt::was_exploded(self, pos, indirect);
            }
        }
        // `Block.popResource` of the merged stacks.
        if self.settings.block_drops {
            for (pos, stack) in stacks {
                drops::pop_resource(self, pos, stack);
            }
        }
    }

    /// `ServerExplosion.getSeenPercent(center, entity)`: the share of points on the entity's box
    /// from which the line to the centre hits no block.
    fn seen_percent(&self, center: Vec3, bb: Aabb) -> f32 {
        let xs = 1.0 / ((bb.max_x - bb.min_x) * 2.0 + 1.0);
        let ys = 1.0 / ((bb.max_y - bb.min_y) * 2.0 + 1.0);
        let zs = 1.0 / ((bb.max_z - bb.min_z) * 2.0 + 1.0);
        let x_offset = (1.0 - (1.0 / xs).floor() * xs) / 2.0;
        let z_offset = (1.0 - (1.0 / zs).floor() * zs) / 2.0;
        if xs < 0.0 || ys < 0.0 || zs < 0.0 {
            return 0.0;
        }
        let mut hits = 0;
        let mut count = 0;
        let mut xx = 0.0;
        while xx <= 1.0 {
            let mut yy = 0.0;
            while yy <= 1.0 {
                let mut zz = 0.0;
                while zz <= 1.0 {
                    let x = lerp(xx, bb.min_x, bb.max_x);
                    let y = lerp(yy, bb.min_y, bb.max_y);
                    let z = lerp(zz, bb.min_z, bb.max_z);
                    let from = Vec3::new(x + x_offset, y, z + z_offset);
                    if !self.clip_hits(from, center) {
                        hits += 1;
                    }
                    count += 1;
                    zz += zs;
                }
                yy += ys;
            }
            xx += xs;
        }
        hits as f32 / count as f32
    }

    /// `clip(new ClipContext(from, to, COLLIDER, NONE, entity)).getType() != MISS`: whether a
    /// collision shape lies on the line (`BlockGetter.traverseBlocks`).
    fn clip_hits(&self, from: Vec3, to: Vec3) -> bool {
        if from == to {
            return false;
        }
        let to_x = lerp(-1.0e-7, to.x, from.x);
        let to_y = lerp(-1.0e-7, to.y, from.y);
        let to_z = lerp(-1.0e-7, to.z, from.z);
        let from_x = lerp(-1.0e-7, from.x, to.x);
        let from_y = lerp(-1.0e-7, from.y, to.y);
        let from_z = lerp(-1.0e-7, from.z, to.z);
        let mut bx = floor(from_x);
        let mut by = floor(from_y);
        let mut bz = floor(from_z);
        if self.shape_clips(BlockPos::new(bx, by, bz), from, to) {
            return true;
        }
        let dx = to_x - from_x;
        let dy = to_y - from_y;
        let dz = to_z - from_z;
        let sign = |d: f64| -> i32 {
            if d == 0.0 {
                0
            } else if d > 0.0 {
                1
            } else {
                -1
            }
        };
        let (sx, sy, sz) = (sign(dx), sign(dy), sign(dz));
        let t_delta = |s: i32, d: f64| {
            if s == 0 { f64::MAX } else { f64::from(s) / d }
        };
        let (tdx, tdy, tdz) = (t_delta(sx, dx), t_delta(sy, dy), t_delta(sz, dz));
        let start = |s: i32, f: f64| if s > 0 { 1.0 - frac(f) } else { frac(f) };
        let mut tx = tdx * start(sx, from_x);
        let mut ty = tdy * start(sy, from_y);
        let mut tz = tdz * start(sz, from_z);
        while tx <= 1.0 || ty <= 1.0 || tz <= 1.0 {
            if tx < ty {
                if tx < tz {
                    bx += sx;
                    tx += tdx;
                } else {
                    bz += sz;
                    tz += tdz;
                }
            } else if ty < tz {
                by += sy;
                ty += tdy;
            } else {
                bz += sz;
                tz += tdz;
            }
            if self.shape_clips(BlockPos::new(bx, by, bz), from, to) {
                return true;
            }
        }
        false
    }

    /// `VoxelShape.clip(from, to, pos) != null` for the collision shape at `pos`.
    fn shape_clips(&self, pos: BlockPos, from: Vec3, to: Vec3) -> bool {
        let shape = self.block(pos).collision_shape();
        if shape.is_empty() {
            return false;
        }
        let diff = to - from;
        if diff.length_sqr() < 1.0e-7 {
            return false;
        }
        let origin = [f64::from(pos.x), f64::from(pos.y), f64::from(pos.z)];
        let test = from + diff * 0.001;
        let test = [test.x - origin[0], test.y - origin[1], test.z - origin[2]];
        if shape
            .iter()
            .any(|b| (0..3).all(|a| b[a] <= test[a] && test[a] < b[a + 3]))
        {
            return true;
        }
        // `AABB.clip(boxes, from, to, pos)`.
        let from = [from.x, from.y, from.z];
        let d = [diff.x, diff.y, diff.z];
        let mut scale = 1.0;
        let mut hit = false;
        for b in shape {
            let min = [b[0] + origin[0], b[1] + origin[1], b[2] + origin[2]];
            let max = [b[3] + origin[0], b[4] + origin[1], b[5] + origin[2]];
            // X, then Y, then Z, as `getDirection`.
            for (a, (b1, c1)) in [(0, (1, 2)), (1, (2, 0)), (2, (0, 1))] {
                let plane = if d[a] > 1.0e-7 {
                    min[a]
                } else if d[a] < -1.0e-7 {
                    max[a]
                } else {
                    continue;
                };
                let s = (plane - from[a]) / d[a];
                let pb = from[b1] + s * d[b1];
                let pc = from[c1] + s * d[c1];
                if 0.0 < s
                    && s < scale
                    && min[b1] - 1.0e-7 < pb
                    && pb < max[b1] + 1.0e-7
                    && min[c1] - 1.0e-7 < pc
                    && pc < max[c1] + 1.0e-7
                {
                    scale = s;
                    hit = true;
                }
            }
        }
        hit
    }
}

/// `Level.isInWorldBoundsHorizontal(pos)`.
fn in_world_bounds_horizontal(pos: BlockPos) -> bool {
    pos.x >= -30_000_000 && pos.z >= -30_000_000 && pos.x < 30_000_000 && pos.z < 30_000_000
}

/// Orders unique positions, given in insertion order, as Java's `HashSet<BlockPos>` iterates
/// them: by bucket of the final table (`Vec3i.hashCode`, spread by `HashMap.hash`), insertion
/// order within a bucket. Buckets of eight or more entries, which Java turns into trees, do not
/// occur for the positions of one explosion.
fn java_hash_set_order(positions: &mut [BlockPos]) {
    let mut capacity: usize = 16;
    while positions.len() * 4 > capacity * 3 {
        capacity *= 2;
    }
    let mask = capacity - 1;
    positions.sort_by_key(|pos| {
        let h = pos
            .y
            .wrapping_add(pos.z.wrapping_mul(31))
            .wrapping_mul(31)
            .wrapping_add(pos.x);
        let spread = h ^ ((h as u32) >> 16) as i32;
        (spread as u32 as usize) & mask
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_set_order_matches_java() {
        // Hashes 0, 1, 961 and 31 fall into buckets 0, 1, 1 and 15: Java iterates (0,0,0),
        // then (1,0,0) and (0,0,1) in insertion order, then (0,1,0).
        let mut positions = vec![
            BlockPos::new(0, 0, 0),
            BlockPos::new(1, 0, 0),
            BlockPos::new(0, 0, 1),
            BlockPos::new(0, 1, 0),
        ];
        java_hash_set_order(&mut positions);
        assert_eq!(
            positions,
            vec![
                BlockPos::new(0, 0, 0),
                BlockPos::new(1, 0, 0),
                BlockPos::new(0, 0, 1),
                BlockPos::new(0, 1, 0),
            ]
        );
    }
}
