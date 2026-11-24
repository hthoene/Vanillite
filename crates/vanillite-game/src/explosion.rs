//! Explosions (`ServerLevel.explode` with `ServerExplosion`): the block rays, the damage and
//! knockback of the entities in range, and the `explode` packet for the players nearby.
//! Creepers use it; TNT and other sources call the same function.

use vanillite_base::math::floor;
use vanillite_base::random::RandomSource;
use vanillite_base::{Aabb, BlockPos, Vec3};
use vanillite_data::block::{BlockState, flags};
use vanillite_protocol::SharedPacket;
use vanillite_protocol::packets::play::clientbound::Explode;
use vanillite_protocol::types;
use vanillite_world::entity::{EntityFlags, EntityId};

use crate::level::Level;
use crate::mob::physics::{self, Blocks};
use crate::mob::{MobContext, combat};
use crate::survival::damage::DamageSource;
use crate::survival::player::Env;
use crate::survival::rules::Rule;
use crate::survival::{Effect, sound_event};

/// `Explosion.BlockInteraction`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockInteraction {
    Keep,
    Destroy,
    DestroyWithDecay,
    TriggerBlock,
}

/// `Level.ExplosionInteraction`: who caused the explosion, for the game rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interaction {
    None,
    Block,
    Mob,
    Tnt,
    Trigger,
}

/// One explosion.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Explosion {
    /// The exploding entity (`source`), if any.
    pub source: Option<EntityId>,
    pub center: Vec3,
    pub radius: f32,
    pub fire: bool,
    pub interaction: Interaction,
}

/// What an explosion did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outcome {
    /// Positions of the blocks that were blown up.
    pub blocks: Vec<BlockPos>,
}

/// `getDestroyType(rule)`.
fn destroy_type(env: &Env<'_>, decay: Rule) -> BlockInteraction {
    if env.rules.bool(decay) {
        BlockInteraction::DestroyWithDecay
    } else {
        BlockInteraction::Destroy
    }
}

/// `ServerExplosion.getSeenPercent`: the share of sample points of the box with a free line to
/// the centre.
pub fn seen_percent(blocks: &dyn Blocks, center: Vec3, bb: &Aabb) -> f32 {
    let xs = 1.0 / ((bb.max_x - bb.min_x) * 2.0 + 1.0);
    let ys = 1.0 / ((bb.max_y - bb.min_y) * 2.0 + 1.0);
    let zs = 1.0 / ((bb.max_z - bb.min_z) * 2.0 + 1.0);
    let x_offset = (1.0 - (1.0 / xs).floor() * xs) / 2.0;
    let z_offset = (1.0 - (1.0 / zs).floor() * zs) / 2.0;
    if xs < 0.0 || ys < 0.0 || zs < 0.0 {
        return 0.0;
    }
    let mut hits = 0u32;
    let mut count = 0u32;
    let mut xx = 0.0;
    while xx <= 1.0 {
        let mut yy = 0.0;
        while yy <= 1.0 {
            let mut zz = 0.0;
            while zz <= 1.0 {
                let x = bb.min_x + xx * (bb.max_x - bb.min_x);
                let y = bb.min_y + yy * (bb.max_y - bb.min_y);
                let z = bb.min_z + zz * (bb.max_z - bb.min_z);
                let from = Vec3::new(x + x_offset, y, z + z_offset);
                if physics::clip(blocks, from, center).is_none() {
                    hits += 1;
                }
                count += 1;
                zz += zs;
            }
            yy += ys;
        }
        xx += xs;
    }
    #[allow(clippy::cast_precision_loss)]
    let r = hits as f32 / count.max(1) as f32;
    r
}

/// `ExplosionDamageCalculator.getBlockExplosionResistance`.
fn resistance(state: BlockState) -> Option<f32> {
    let fluid = state.fluid_state();
    if state.is_air() && fluid.is_empty() {
        return None;
    }
    let fluid_resistance = if fluid.is_empty() { 0.0 } else { 100.0 };
    Some(state.block().info().blast_resistance.max(fluid_resistance))
}

/// `ServerExplosion.calculateExplodedPositions`.
fn exploded_positions(level: &mut Level, center: Vec3, radius: f32) -> Vec<BlockPos> {
    let mut set = vanillite_base::hash::FxHashSet::default();
    let mut out = Vec::new();
    for xx in 0..16 {
        for yy in 0..16 {
            for zz in 0..16 {
                if !(xx == 0 || xx == 15 || yy == 0 || yy == 15 || zz == 0 || zz == 15) {
                    continue;
                }
                #[allow(clippy::cast_precision_loss)]
                let f = |v: i32| f64::from(v as f32 / 15.0 * 2.0 - 1.0);
                let (mut xd, mut yd, mut zd) = (f(xx), f(yy), f(zz));
                let d = (xd * xd + yd * yd + zd * zd).sqrt();
                xd /= d;
                yd /= d;
                zd /= d;
                let mut power = radius * (0.7 + level.random().next_float() * 0.6);
                let (mut xp, mut yp, mut zp) = (center.x, center.y, center.z);
                while power > 0.0 {
                    let pos = BlockPos::containing(xp, yp, zp);
                    if !level.in_height(pos.y) {
                        break;
                    }
                    let state = level.block(pos);
                    if let Some(r) = resistance(state) {
                        power -= (r + 0.3) * 0.3;
                    }
                    if power > 0.0 && set.insert(pos) {
                        out.push(pos);
                    }
                    xp += xd * f64::from(0.3_f32);
                    yp += yd * f64::from(0.3_f32);
                    zp += zd * f64::from(0.3_f32);
                    power -= 0.225_000_01;
                }
            }
        }
    }
    out
}

/// `ServerLevel.explode` → `ServerExplosion.explode`. The entities must be in the level.
pub fn explode(level: &mut Level, env: &mut Env<'_>, cx: &mut MobContext, e: Explosion) -> Outcome {
    let interaction = match e.interaction {
        Interaction::None => BlockInteraction::Keep,
        Interaction::Block => destroy_type(env, Rule::BLOCK_EXPLOSION_DROP_DECAY),
        Interaction::Mob => {
            if env.rules.bool(Rule::MOB_GRIEFING) {
                destroy_type(env, Rule::MOB_EXPLOSION_DROP_DECAY)
            } else {
                BlockInteraction::Keep
            }
        }
        Interaction::Tnt => destroy_type(env, Rule::TNT_EXPLOSION_DROP_DECAY),
        Interaction::Trigger => BlockInteraction::TriggerBlock,
    };
    let center = e.center;
    let mut to_blow = exploded_positions(level, center, e.radius);
    // `hurtEntities`.
    let mut hit_players: Vec<(EntityId, Vec3)> = Vec::new();
    if e.radius >= 1.0e-5 {
        let double_radius = f64::from(e.radius * 2.0);
        let area = Aabb::new(
            f64::from(floor(center.x - double_radius - 1.0)),
            f64::from(floor(center.y - double_radius - 1.0)),
            f64::from(floor(center.z - double_radius - 1.0)),
            f64::from(floor(center.x + double_radius + 1.0)),
            f64::from(floor(center.y + double_radius + 1.0)),
            f64::from(floor(center.z + double_radius + 1.0)),
        );
        let mut targets = Vec::new();
        level.entities.store.for_each_in(&area, false, |id| {
            if Some(id) != e.source {
                targets.push(id);
            }
        });
        let mut ents = std::mem::take(&mut level.entities);
        for id in targets {
            let Some(ent) = ents.store.get(id) else {
                continue;
            };
            let is_player = ents.player(id).is_some();
            let is_mob = ents.mob(id).is_some_and(crate::mob::MobData::is_alive);
            if !is_player && !is_mob {
                continue;
            }
            if let Some(p) = ents.player(id)
                && p.game_mode == vanillite_config::GameMode::Spectator
            {
                continue;
            }
            let pos = ent.pos();
            let dist = pos.distance_to_sqr(center).sqrt() / double_radius;
            if dist > 1.0 {
                continue;
            }
            let eye = match ents.mob(id) {
                Some(m) => crate::mob::eye_height(m),
                None => f64::from(ent.kind().info().eye_height),
            };
            let direction = (Vec3::new(pos.x, pos.y + eye, pos.z) - center).normalize();
            let bb = ent.aabb();
            let exposure = seen_percent(&*level, center, &bb);
            let pow = (1.0 - dist) * f64::from(exposure);
            #[allow(clippy::cast_possible_truncation)]
            let damage = ((pow * pow + pow) / 2.0 * 7.0 * double_radius + 1.0) as f32;
            let source = match e.source {
                Some(s) if ents.store.contains(s) => {
                    let kind = if ents.player(s).is_some() {
                        env.types.player_explosion
                    } else {
                        env.types.explosion
                    };
                    combat::entity_source(&ents, kind, s, s)
                }
                _ => {
                    let mut src = DamageSource::of(env.types.explosion);
                    src.position = Some(center);
                    src
                }
            };
            combat::hurt(&*level, &mut ents, env, cx, id, source, damage);
            let resistance = ents
                .mob(id)
                .map_or(0.0, |m| m.attrs.explosion_knockback_resistance);
            let knockback = direction * ((1.0 - dist) * f64::from(exposure) * (1.0 - resistance));
            if ents.mob(id).is_some() {
                if let Some(mut m) = ents.store.get_mut(id) {
                    let v = m.as_ref().velocity();
                    m.set_velocity(v + knockback);
                    m.set_flags(EntityFlags::HAS_IMPULSE, true);
                }
            } else if is_player {
                hit_players.push((id, knockback));
            }
        }
        level.entities = ents;
    }
    // `interactWithBlocks`: shuffled, then each block breaks.
    if interaction != BlockInteraction::Keep && interaction != BlockInteraction::TriggerBlock {
        let n = to_blow.len();
        for i in (1..n).rev() {
            let bound = i32::try_from(i + 1).unwrap_or(i32::MAX);
            let j = usize::try_from(level.random().next_int_bounded(bound)).unwrap_or(0);
            to_blow.swap(i, j);
        }
        for &pos in &to_blow {
            let state = level.block(pos);
            if state.is_air() {
                continue;
            }
            // `onExplosionHit`: the block's loot (with the explosion radius for decay), then
            // air.
            if level.settings.block_drops
                && let Some(data) = crate::player::play::game_data()
            {
                let tool = crate::item::ItemStack::EMPTY;
                let item_in_tag = |item: u16, tag: &str| data.item_in_tag(item, tag);
                let loot = crate::loot::BlockLootContext {
                    state,
                    tool: &tool,
                    enchantment_level: &|_| 0,
                    item_in_tag: &item_in_tag,
                    explosion_radius: (interaction == BlockInteraction::DestroyWithDecay)
                        .then_some(e.radius),
                };
                let mut stacks = Vec::new();
                data.loot.block_drops(&loot, level.random(), &mut stacks);
                for stack in stacks {
                    crate::block::drops::pop_resource(level, pos, stack);
                }
            }
            level.set_block(pos, BlockState::AIR, 3);
        }
    }
    if e.fire {
        for &pos in &to_blow {
            let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
            if level.random().next_int_bounded(3) == 0
                && level.block(pos).is_air()
                && level.block(below).has(flags::SOLID_RENDER)
            {
                level.set_block_and_update(pos, vanillite_data::block::ids::FIRE.default_state());
            }
        }
    }
    // The `explode` packet for the players within 64 blocks.
    let small = e.radius < 2.0 || interaction == BlockInteraction::Keep;
    let particle_name = if small {
        "minecraft:explosion"
    } else {
        "minecraft:explosion_emitter"
    };
    let particle = vanillite_data::registry::PARTICLE_TYPE
        .id(particle_name)
        .map_or(0, i32::from);
    let sound = sound_event("minecraft:entity.generic.explode");
    let block_count = i32::try_from(to_blow.len()).unwrap_or(i32::MAX);
    let wire = |v: Vec3| types::Vec3 {
        x: v.x,
        y: v.y,
        z: v.z,
    };
    for (id, _) in level.entities.players.iter() {
        let Some(p) = level.entities.store.get(id) else {
            continue;
        };
        if p.pos().distance_to_sqr(center) >= 4096.0 {
            continue;
        }
        let knockback = hit_players
            .iter()
            .find(|(h, _)| *h == id)
            .map(|&(_, k)| wire(k));
        env.out.push(Effect::To {
            player: id,
            packet: SharedPacket::of(&Explode {
                center: wire(center),
                radius: e.radius,
                block_count,
                player_knockback: knockback,
                particle,
                sound,
            }),
        });
    }
    Outcome { blocks: to_blow }
}
