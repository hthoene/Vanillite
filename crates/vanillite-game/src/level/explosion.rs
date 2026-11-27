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
//! Fire explosions (ghast fireballs) set fire to a third of the blown up air above solid
//! blocks. `TRIGGER_BLOCK` explosions (wind charges) leave the blocks and open doors,
//! trapdoors and fence gates, press buttons, pull levers, ring bells and blow out candles.
//!
//! Not ported: damage of end crystals and other non-living entities and the special damage
//! calculators (wither skulls, wind charges, TNT that used a portal).

use vanillite_base::hash::FxHashSet;
use vanillite_base::math::{floor, frac, lerp};
use vanillite_base::random::RandomSource;
use vanillite_base::{Aabb, BlockPos, Vec3};
use vanillite_data::block::{BlockKind, BlockState, ids};
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
    /// `RespawnAnchorBlock.explode` in water: the anchor's position resists like water.
    WaterAt(BlockPos),
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
    /// `fire`: sets fire to some of the blown up positions (`createFire`).
    pub fire: bool,
    /// The damage is `badRespawnPointExplosion` (beds and respawn anchors).
    pub bad_respawn_point: bool,
}

impl Default for ExplosionStyle {
    fn default() -> Self {
        ExplosionStyle {
            damages_entities: true,
            knockback: 1.0,
            wind_sound: None,
            resistance: BlockResistance::Default,
            fire: false,
            bad_respawn_point: false,
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
    /// `ServerExplosion.shouldAffectBlocklikeEntities` with mob griefing off: the explosion
    /// breaks blocks and is no wind charge (with mob griefing on only the wind charge part
    /// counts, see [`ExplosionHurt::wind`]).
    pub blocklike: bool,
    /// The source is a wind charge.
    pub wind: bool,
    /// `badRespawnPointExplosion` instead of `explosion`.
    pub bad_respawn_point: bool,
}

impl ExplosionHurt {
    /// `shouldAffectBlocklikeEntities`.
    pub fn affects_blocklike(&self, mob_griefing: bool) -> bool {
        if mob_griefing {
            !self.wind
        } else {
            self.blocklike
        }
    }
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
        self.explode_full(source, indirect, center, radius, interaction, style, None)
    }

    /// [`Self::explode_with`] of a block without a source entity (a bed or respawn anchor):
    /// `block` is the exploding block and its state before it went, for the plugins.
    pub fn explode_block(
        &mut self,
        block: (BlockPos, BlockState),
        center: Vec3,
        radius: f32,
        interaction: BlockInteraction,
        style: ExplosionStyle,
    ) -> usize {
        self.explode_full(None, None, center, radius, interaction, style, Some(block))
    }

    #[allow(clippy::too_many_arguments)]
    fn explode_full(
        &mut self,
        source: Option<EntityId>,
        indirect: Option<EntityId>,
        center: Vec3,
        radius: f32,
        interaction: BlockInteraction,
        style: ExplosionStyle,
        block: Option<(BlockPos, BlockState)>,
    ) -> usize {
        let (mut radius, mut interaction, mut style) = (radius, interaction, style);
        if let Some(hooks) = self.hooks_for(vanillite_abi::event::ENTITY_EXPLODE) {
            let Some((r, i, fire)) =
                self.explode_event(&*hooks, source, center, radius, interaction, &style, block)
            else {
                return 0;
            };
            (radius, interaction, style.fire) = (r, i, fire);
        }
        let context = super::game_event::GameEventContext::of(source);
        self.game_event(super::game_event::events().explode, center, context);
        let mut targets = self.exploded_positions(center, radius, style.resistance);
        let destroys = matches!(
            interaction,
            BlockInteraction::Destroy | BlockInteraction::DestroyWithDecay
        );
        let hit_players = self.hurt_entities(source, indirect, center, radius, style, destroys);
        if interaction != BlockInteraction::Keep {
            self.interact_with_blocks(&mut targets, source, indirect, interaction, radius, center);
        }
        if style.fire {
            self.create_fire(&targets);
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

    /// The plugins' explosion event (`vanillite_abi::event::ENTITY_EXPLODE`): `None` when a
    /// plugin cancelled it, else the radius, block interaction and fire it may have changed.
    #[allow(clippy::too_many_arguments)]
    fn explode_event(
        &self,
        hooks: &dyn crate::hooks::GameHooks,
        source: Option<EntityId>,
        center: Vec3,
        radius: f32,
        interaction: BlockInteraction,
        style: &ExplosionStyle,
        block: Option<(BlockPos, BlockState)>,
    ) -> Option<(f32, BlockInteraction, bool)> {
        use vanillite_abi::explode_flag;
        let destroys = matches!(
            interaction,
            BlockInteraction::Destroy | BlockInteraction::DestroyWithDecay
        );
        let mut flags = 0;
        if destroys {
            flags |= explode_flag::BREAK_BLOCKS;
        }
        if style.fire {
            flags |= explode_flag::FIRE;
        }
        let entity = source.and_then(|s| self.entities.store.get(s));
        let mut ev = crate::hooks::Event {
            entity: entity.as_ref().map_or(0, |e| e.uuid()),
            entity_type: entity
                .as_ref()
                .map_or_else(String::new, |e| e.kind().name().to_owned()),
            pos: block.map_or(BlockPos::containing(center.x, center.y, center.z), |b| b.0),
            state: block.map(|b| b.1),
            to: crate::hooks::Place::at(center),
            amount: f64::from(radius),
            flags,
            ..crate::hooks::Event::new(vanillite_abi::event::ENTITY_EXPLODE)
        };
        hooks.event(&mut ev);
        if ev.cancelled {
            return None;
        }
        #[allow(clippy::cast_possible_truncation)]
        let radius = if ev.amount.is_finite() {
            ev.amount.clamp(0.0, crate::hooks::MAX_EXPLOSION_RADIUS) as f32
        } else {
            radius
        };
        let breaks = ev.flags & explode_flag::BREAK_BLOCKS != 0;
        let interaction = match (destroys, breaks) {
            (true, false) => BlockInteraction::Keep,
            (false, true) => BlockInteraction::Destroy,
            _ => interaction,
        };
        Some((radius, interaction, ev.flags & explode_flag::FIRE != 0))
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
                        if override_resistance == BlockResistance::WindCharge {
                            // `SimpleExplosionDamageCalculator` with immune blocks: only those
                            // resist, everything else lets the ray through.
                            if crate::block::block_tags()
                                .blocks_wind_charge_explosions
                                .contains(state)
                            {
                                power -= (3_600_000.0 + 0.3) * 0.3;
                            }
                        } else if override_resistance == BlockResistance::WaterAt(pos) {
                            let water = vanillite_data::block::ids::WATER.default_state();
                            power -= (water.block().info().blast_resistance + 0.3) * 0.3;
                        } else if !(state.is_air() && fluid.is_empty()) {
                            let mut resistance = if rail_protected {
                                0.0
                            } else {
                                state
                                    .block()
                                    .info()
                                    .blast_resistance
                                    .max(fluid.info().explosion_resistance)
                            };
                            if override_resistance == BlockResistance::DangerousWitherSkull {
                                resistance = crate::mob::boss::wither::skull_block_resistance(
                                    true, state, resistance,
                                );
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
        destroys: bool,
    ) -> Vec<(EntityId, Vec3)> {
        let mut hit_players = Vec::new();
        let wind = source
            .and_then(|s| self.entities.store.get(s))
            .is_some_and(|e| {
                e.kind() == entity_ids::WIND_CHARGE || e.kind() == entity_ids::BREEZE_WIND_CHARGE
            });
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
            // `ItemEntity.hurtServer`: a mob's explosion spares items without `mob_griefing`.
            let mob_spared = !self.settings.mob_griefing
                && indirect.is_some_and(|i| self.entities.mob(i).is_some());
            if kind == entity_ids::ITEM && style.damages_entities && !mob_spared {
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
                    // `Item.onDestroyed`: a bundle's or a container item's contents drop.
                    let contents = self
                        .entities
                        .item_mut(id)
                        .map(|d| crate::item_entity::take_destroyed_contents(&mut d.stack))
                        .unwrap_or_default();
                    for stack in contents {
                        crate::block::drops::spawn_item_moving(self, pos, stack, None, 0, None);
                    }
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
            let decor = crate::decoration::ticks(kind);
            if style.damages_entities
                && (player.is_some()
                    || crystal
                    || mob.is_some_and(crate::mob::MobData::is_alive)
                    || is_boat
                    || decor)
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
                    blocklike: destroys && !wind,
                    wind,
                    bad_respawn_point: style.bad_respawn_point,
                });
            }
            if decor || crate::decoration::is_decor_kind(kind) {
                // Block-attached entities break when pushed; displays and markers do not move.
                continue;
            }
            let resistance = mob.map_or(0.0, |m| m.attrs.explosion_knockback_resistance);
            let knockback =
                direction * ((1.0 - dist) * exposure * style.knockback * (1.0 - resistance));
            // `onExplosionHit`: where the explosion hit the player and what caused it.
            if let Some(p) = self.entities.player_mut(id) {
                crate::advancements::tracking::explosion_hit(p, pos, source);
            }
            let player = self.entities.player(id);
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
        center: Vec3,
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
        let trigger =
            interaction == BlockInteraction::TriggerBlock && self.can_trigger_blocks(source);
        for &pos in targets.iter() {
            // `Block.onExplosionHit`.
            let state = self.block(pos);
            if trigger {
                self.trigger_block(state, pos);
            }
            let hive = crate::block::beehive::is_beehive(state.block());
            if state.is_air() || interaction == BlockInteraction::TriggerBlock {
                // `BeehiveBlock.onExplosionHit` angers the bees around in any explosion.
                if hive {
                    crate::block::beehive::anger_nearby_bees(self, pos);
                }
                continue;
            }
            if state.is(ids::CREAKING_HEART) {
                crate::block::creaking_heart::on_explosion_hit(
                    self, pos, state, source, indirect, center,
                );
            }
            // `dropFromExplosion`: everything but TNT; the loot is rolled block by block.
            if !state.is(ids::TNT) {
                if hive {
                    crate::block::beehive::exploded(self, pos, state, source);
                }
                loot.clear();
                // `spawnAfterBreak` with experience only for a player's explosion.
                let by_player = indirect.is_some_and(|i| self.entities.player(i).is_some());
                crate::block::break_hooks::spawn_after_break(
                    self,
                    state,
                    pos,
                    &crate::item::ItemStack::EMPTY,
                    by_player,
                );
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
            if hive {
                crate::block::beehive::anger_nearby_bees(self, pos);
            }
        }
        // `Block.popResource` of the merged stacks (it draws the positions even without
        // `block_drops`).
        for (pos, stack) in stacks {
            drops::pop_resource(self, pos, stack);
        }
    }

    /// `canTriggerBlocks`: a breeze's wind charge only with `mobGriefing`.
    fn can_trigger_blocks(&self, source: Option<EntityId>) -> bool {
        let breeze = source
            .and_then(|s| self.entities.store.get(s))
            .is_some_and(|e| e.kind() == entity_ids::BREEZE_WIND_CHARGE);
        !breeze || self.settings.mob_griefing
    }

    /// The `onExplosionHit` overrides of the blocks a triggering explosion reaches.
    fn trigger_block(&mut self, state: BlockState, pos: BlockPos) {
        use crate::block::redstone;
        match state.block().info().kind {
            BlockKind::DoorBlock | BlockKind::WeatheringCopperDoorBlock => {
                redstone::door_explosion_hit(self, state, pos);
            }
            BlockKind::TrapDoorBlock | BlockKind::WeatheringCopperTrapDoorBlock => {
                redstone::trapdoor_explosion_hit(self, state, pos);
            }
            BlockKind::FenceGateBlock => redstone::fence_gate_explosion_hit(self, state, pos),
            BlockKind::ButtonBlock => redstone::button_explosion_hit(self, state, pos),
            BlockKind::LeverBlock => redstone::lever_pull(self, state, pos),
            BlockKind::BellBlock => {
                crate::block::bell::attempt_to_ring(self, None, pos, None);
            }
            BlockKind::CandleBlock | BlockKind::CandleCakeBlock if state.flag("lit") => {
                // `AbstractCandleBlock.extinguish(null, ...)`; the smoke is the client's.
                self.set_block(
                    pos,
                    state.set_flag("lit", false),
                    update::ALL | update::IMMEDIATE,
                );
                let sound = crate::block::sound_id("block.candle.extinguish");
                self.play_block_sound(None, pos, sound, crate::level::SOUND_BLOCKS, 1.0, 1.0);
                let event = super::game_event::events().block_change;
                self.game_event_at(None, event, pos);
            }
            _ => {}
        }
    }

    /// `createFire`: a third of the blown up air positions above solid blocks catch fire.
    fn create_fire(&mut self, targets: &[BlockPos]) {
        for &pos in targets {
            if self.random.next_int_bounded(3) == 0
                && self.block(pos).is_air()
                && vanillite_worldgen::behaviour::is_solid_render(self.block(pos.offset(0, -1, 0)))
            {
                let fire = crate::block::place::fire_state(&*self, pos);
                self.set_block_and_update(pos, fire);
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
