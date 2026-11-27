//! Arrows (`AbstractArrow`, `Arrow` with its potion, `SpectralArrow`) shot by bows,
//! skeletons, strays and bogged.

use vanillite_base::math::ceil;
use vanillite_base::random::{LegacyRandom, RandomSource};
use vanillite_base::{Aabb, BlockPos, Vec3};
use vanillite_data::block::BlockState;
use vanillite_data::entity::ids as entity_ids;
use vanillite_protocol::packets::play::clientbound::SoundSource;
use vanillite_world::entity::{EntityFlags, EntityId, NewEntity, Rotation};

use super::physics::{self, Blocks};
use super::{MobContext, Tick, atan2_deg, combat};
use crate::entities::Entities;
use crate::level::Level;
use crate::survival::play_sound;
use crate::survival::player::Env;
use crate::survival::rules::Difficulty;

/// Whether `kind` is an arrow (`Arrow` or `SpectralArrow`).
pub fn is_arrow_kind(kind: vanillite_data::entity::EntityTypeId) -> bool {
    kind == entity_ids::ARROW || kind == entity_ids::SPECTRAL_ARROW
}

/// The kind data of an arrow or spectral arrow.
#[derive(Clone, Debug, PartialEq)]
pub struct ArrowData {
    pub owner: Option<EntityId>,
    pub base_damage: f64,
    pub in_ground: bool,
    pub life: i32,
    pub shake: i32,
    pub crit: bool,
    /// The arrow left its owner's box (`leftOwner`).
    pub left_owner: bool,
    pub last_state: Option<BlockState>,
    pub tick_count: i32,
    /// `pickup`: 0 disallowed, 1 allowed, 2 creative only.
    pub pickup: u8,
    /// The state of its own random ([`LegacyRandom::state`]).
    pub random_state: i64,
    /// The knockback of the weapon's enchantments (Punch).
    pub knockback: f32,
    /// Burning (Flame): sets what it hits on fire for 5 seconds.
    pub fire: bool,
    /// The item it is picked up as (`pickupItemStack`): an arrow, a tipped arrow with its
    /// `potion_contents`, or a spectral arrow.
    pub item: crate::item::ItemStack,
    /// `inGroundTime`.
    pub in_ground_time: i32,
    /// The glowing ticks of a spectral arrow (`duration`).
    pub spectral_duration: i32,
    /// `hasBeenShot`: the `PROJECTILE_SHOOT` game event was posted.
    pub has_been_shot: bool,
    /// `PIERCE_LEVEL`: how many more entities it passes through (Piercing).
    pub pierce_level: u8,
    /// `piercingIgnoreEntityIds`: the entities it pierced (hits them no more).
    pub pierced: Vec<EntityId>,
    /// `soundEvent` is `item.crossbow.hit` (shot from a crossbow) instead of
    /// `entity.arrow.hit`.
    pub crossbow_sound: bool,
    /// The bow or crossbow it was shot from (`firedFromWeapon`).
    pub weapon: Option<crate::item::ItemStack>,
    /// `Projectile.lastDeflectedBy`.
    pub last_deflected: Option<EntityId>,
}

impl ArrowData {
    /// An arrow that was just shot.
    pub fn shot(
        owner: Option<EntityId>,
        base_damage: f64,
        pickup: u8,
        random_state: i64,
        item: crate::item::ItemStack,
    ) -> ArrowData {
        ArrowData {
            owner,
            base_damage,
            in_ground: false,
            life: 0,
            shake: 0,
            crit: false,
            left_owner: false,
            last_state: None,
            tick_count: 0,
            pickup,
            random_state,
            knockback: 0.0,
            fire: false,
            item,
            in_ground_time: 0,
            spectral_duration: 200,
            has_been_shot: false,
            pierce_level: 0,
            pierced: Vec::new(),
            crossbow_sound: false,
            weapon: None,
            last_deflected: None,
        }
    }

    /// The arrow's `potion_contents` (`Arrow.getPotionContents`).
    pub fn potion(&self) -> crate::effect::potion::PotionContents {
        crate::effect::potion::PotionContents::of_stack(&self.item)
    }

    /// `ID_EFFECT_COLOR`: the potion colour, -1 without a potion.
    pub fn color(&self) -> i32 {
        let contents = self.potion();
        if contents == crate::effect::potion::PotionContents::default() {
            -1
        } else {
            contents.color()
        }
    }
}

/// The item an arrow item is shot as (`ArrowItem.createArrow`): its entity type and the
/// pickup stack (count 1).
pub fn arrow_of(
    ammo: &crate::item::ItemStack,
) -> (vanillite_data::entity::EntityTypeId, crate::item::ItemStack) {
    let mut item = ammo.clone();
    item.count = 1;
    let kind = if crate::item::item_id(ammo).name() == "minecraft:spectral_arrow" {
        entity_ids::SPECTRAL_ARROW
    } else {
        entity_ids::ARROW
    };
    (kind, item)
}

/// `startUsingItem` with the bow: the "using item" living flag is synched.
pub fn start_using_item(t: &mut Tick<'_, '_>) {
    let m = t.mob();
    m.using_item = 0;
    m.stabbed.clear();
    t.core_mut().mark_data_dirty();
}

/// `stopUsingItem`.
pub fn stop_using_item(t: &mut Tick<'_, '_>) {
    if t.mob_ref().using_item >= 0 {
        let m = t.mob();
        m.using_item = -1;
        m.stabbed = Vec::new();
        t.core_mut().mark_data_dirty();
    }
}

/// `Projectile.getMovementToShoot` and `shoot`: the velocity and rotation of a shot.
pub fn shoot(
    random: &mut LegacyRandom,
    dir: Vec3,
    power: f32,
    uncertainty: f32,
) -> (Vec3, Rotation) {
    let spread = 0.017_227_5 * f64::from(uncertainty);
    let n = dir.normalize();
    let v = Vec3::new(
        n.x + random.triangle(0.0, spread),
        n.y + random.triangle(0.0, spread),
        n.z + random.triangle(0.0, spread),
    ) * f64::from(power);
    let sd = (v.x * v.x + v.z * v.z).sqrt();
    #[allow(clippy::cast_possible_truncation)]
    let rot = Rotation {
        y_rot: atan2_deg(v.x, v.z) as f32,
        x_rot: atan2_deg(v.y, sd) as f32,
        y_head_rot: 0.0,
    };
    (v, rot)
}

/// `BowItem.releaseUsing` / `ProjectileWeaponItem.shoot` of one arrow: spawns it at `start`
/// flying along `dir` with `power` and `uncertainty`, with the weapon's enchantments (Power
/// adds damage, Punch knockback, Flame fire). Dispensers and other shooters use it too.
#[allow(clippy::too_many_arguments)]
pub fn shoot_arrow(
    ents: &mut Entities,
    network_ids: &vanillite_world::entity::NetworkIds,
    random: &mut LegacyRandom,
    start: Vec3,
    dir: Vec3,
    power: f32,
    uncertainty: f32,
    owner: Option<EntityId>,
    weapon: Option<&crate::item::ItemStack>,
    ammo: &crate::item::ItemStack,
    crit: bool,
    pickup: u8,
) -> EntityId {
    // The arrow's own random (its constructor), then `shoot` draws the spread from it.
    let mut own = LegacyRandom::new(crate::mob::new_entity_seed());
    let uuid = crate::mob::new_uuid();
    let (velocity, rot) = shoot(&mut own, dir, power, uncertainty);
    let (kind, item) = arrow_of(ammo);
    let mut data = ArrowData::shot(owner, 2.0, pickup, own.state(), item);
    data.crit = crit;
    data.weapon = weapon.cloned();
    if let Some(w) = weapon {
        let combat = super::attack::combat_data();
        let (enchants, tags): (
            &crate::enchant::Enchantments,
            &dyn crate::predicate::EntityTags,
        ) = (&combat.enchants, &*combat);
        let arrow = crate::predicate::EntityFacts {
            kind: Some(kind),
            ..crate::predicate::EntityFacts::default()
        };
        let types = crate::survival::damage::DamageTypes::built_in();
        let cx = crate::enchant::EnchantContext {
            types: &types,
            tags,
            this: None,
            attacker: owner.and_then(|o| super::attack::facts(ents, o)),
            direct_attacker: Some(arrow),
            attacking_player: None,
            source: None,
            raining: false,
            thundering: false,
            location: None,
        };
        #[allow(clippy::cast_possible_truncation)]
        let dmg = enchants.modify_damage(w, &cx, random, data.base_damage as f32);
        data.base_damage = f64::from(dmg);
        data.knockback = enchants.modify_knockback(w, &cx, random, 0.0);
        data.fire = enchants.projectile_ignite(w).is_some();
    }
    let mut new = NewEntity::new(kind, network_ids.next(), uuid, start);
    new.velocity = velocity;
    new.rot = rot;
    new.flags = EntityFlags::HAS_IMPULSE;
    if data.fire {
        new.flags.set(EntityFlags::ON_FIRE, true);
    }
    let id = ents.spawn_arrow(new, data);
    ents.launched(id, owner);
    id
}

/// `AbstractSkeleton.performRangedAttack`.
pub fn skeleton_shoot(t: &mut Tick<'_, '_>, target: EntityId, power: f32) {
    let Some(o) = t.other(target) else {
        return;
    };
    let p = t.pos();
    // `getEyeY() - 0.1F`.
    let start = Vec3::new(p.x, t.eye_y() - f64::from(0.1_f32), p.z);
    let difficulty = t.env.difficulty;
    let id = t.id;
    let m = t.mob();
    // The arrow's own random (its constructor); `setBaseDamageFromMob` and `shoot` draw from
    // it, not from the skeleton's.
    let mut own = LegacyRandom::new(super::new_entity_seed());
    #[allow(clippy::cast_precision_loss)]
    let base_damage =
        f64::from(power) * 2.0 + own.triangle(f64::from(difficulty as i32) * 0.11, 0.574_25);
    let xd = o.pos.x - p.x;
    // `target.getY(0.3333333333333333)`.
    let target_y = o.pos.y + f64::from(o.height) * 0.333_333_333_333_333_3;
    let yd = target_y - start.y;
    let zd = o.pos.z - p.z;
    let dist = (xd * xd + zd * zd).sqrt();
    #[allow(clippy::cast_precision_loss)]
    let uncertainty = (14 - difficulty as i32 * 4) as f32;
    let (velocity, rot) = shoot(
        &mut own,
        Vec3::new(xd, yd + dist * f64::from(0.2_f32), zd),
        1.6,
        uncertainty,
    );
    let uuid = crate::mob::new_uuid();
    let crit = false;
    // `getArrow` of the skeleton kinds: tipped arrows and the wither skeleton's fire.
    let (effect, fire) = super::fam::undead::skeleton_arrow(m.kind);
    let mut new = NewEntity::new(entity_ids::ARROW, t.level.network_ids().next(), uuid, start);
    new.velocity = velocity;
    new.rot = rot;
    new.flags = EntityFlags::HAS_IMPULSE;
    if fire {
        new.flags.set(EntityFlags::ON_FIRE, true);
    }
    let mut data = ArrowData::shot(Some(id), base_damage, 0, own.state(), mob_arrow(effect));
    data.crit = crit;
    data.fire = fire;
    let arrow = t.ents.spawn_arrow(new, data);
    t.ents.launched(arrow, Some(id));
    let m = t.mob();
    let pitch = 1.0 / (m.random.next_float() * 0.4 + 0.8);
    super::kinds::play_sound(t, "minecraft:entity.skeleton.shoot", 1.0, pitch);
    let _ = Difficulty::Peaceful;
}

/// The arrow a skeleton type shoots (`AbstractSkeleton.getArrow`): `Arrow.addEffect` puts the
/// effect of strays, bogged and parched into a plain arrow's potion.
pub(crate) fn mob_arrow(effect: Option<crate::effect::EffectInstance>) -> crate::item::ItemStack {
    use crate::effect::potion::PotionContents;
    let mut item = crate::item::ItemStack::new(
        vanillite_data::registry::ITEM
            .id("minecraft:arrow")
            .unwrap_or(0),
        1,
    );
    if let Some(e) = effect {
        let contents = PotionContents {
            custom_effects: vec![e],
            ..PotionContents::default()
        };
        item.patch_mut().set(
            vanillite_protocol::components::ids::POTION_CONTENTS,
            contents.encode(),
        );
    }
    item
}

/// `AbstractArrow.tick` (with `Projectile.tick`'s owner check) and `Arrow.tick`.
pub fn tick_arrow(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    id: EntityId,
) {
    let Some(mut a) = ents.arrow(id).cloned() else {
        return;
    };
    let Some(e) = ents.store.get(id) else {
        return;
    };
    // The synched values (`ID_FLAGS`, `PIERCE_LEVEL`, `IN_GROUND`): only a change of one marks
    // the data dirty. The client restarts its shake on every `IN_GROUND` it receives.
    let synched = (a.crit, a.pierce_level, a.in_ground);
    let mut color_changed = false;
    let mut pos = e.pos();
    let start = pos;
    let start_box = e.aabb();
    let mut velocity = e.velocity();
    let mut rot = e.rot();
    let (kind, network_id) = (e.kind(), e.network_id().0);
    let mut random = LegacyRandom::from_state(a.random_state);
    // `Projectile.tick`: the first tick posts `PROJECTILE_SHOOT` from the shooter.
    if !std::mem::replace(&mut a.has_been_shot, true) {
        super::missile::shoot_event(env.out, pos, a.owner);
    }
    a.tick_count += 1;
    let block_pos = BlockPos::containing(pos.x, pos.y, pos.z);
    let state = level.block(block_pos);
    if !state.is_air() {
        for b in state.collision_shape() {
            let shape = Aabb::new(
                f64::from(block_pos.x) + b[0],
                f64::from(block_pos.y) + b[1],
                f64::from(block_pos.z) + b[2],
                f64::from(block_pos.x) + b[3],
                f64::from(block_pos.y) + b[4],
                f64::from(block_pos.z) + b[5],
            );
            if shape.contains(pos.x, pos.y, pos.z) {
                velocity = Vec3::ZERO;
                a.in_ground = true;
                break;
            }
        }
    }
    if a.shake > 0 {
        a.shake -= 1;
    }
    let mut discard = false;
    if a.in_ground {
        let falls = a.last_state != Some(state) && {
            let probe =
                Aabb::new(pos.x, pos.y, pos.z, pos.x, pos.y, pos.z).inflate(0.06, 0.06, 0.06);
            physics::no_collision(&level.view(), &probe, &mut cx.boxes)
        };
        if falls {
            a.in_ground = false;
            velocity = velocity.multiply(
                f64::from(random.next_float() * 0.2),
                f64::from(random.next_float() * 0.2),
                f64::from(random.next_float() * 0.2),
            );
            a.life = 0;
        } else {
            a.life += 1;
            if a.life >= cx.settings.arrow_despawn_ticks {
                discard = true;
            }
        }
        a.in_ground_time += 1;
        if !discard {
            inside_blocks(level, cx, id, start_box, start, pos);
        }
    } else {
        a.in_ground_time = 0;
        // The step and the rotation use the motion from the start of the tick.
        let movement = velocity;
        let in_water = super::is_water(state);
        if in_water {
            velocity = velocity * f64::from(0.6_f32);
        }
        #[allow(clippy::cast_possible_truncation)]
        let y_rot = atan2_deg(movement.x, movement.z) as f32;
        let hd = (movement.x * movement.x + movement.z * movement.z).sqrt();
        #[allow(clippy::cast_possible_truncation)]
        let x_rot = atan2_deg(movement.y, hd) as f32;
        rot.x_rot = lerp_rotation(rot.x_rot, x_rot);
        rot.y_rot = lerp_rotation(rot.y_rot, y_rot);
        // `checkLeftOwner`.
        if !a.left_owner {
            let aabb = e
                .aabb()
                .expand_towards(velocity.x, velocity.y, velocity.z)
                .inflate(1.0, 1.0, 1.0);
            let still_inside = a.owner.is_some_and(|o| {
                ents.store
                    .get(o)
                    .is_some_and(|oe| oe.aabb().intersects(&aabb))
            });
            a.left_owner = !still_inside;
        }
        let to = pos + movement;
        let block_hit = physics::clip(level as &dyn Blocks, pos, to);
        let end = block_hit.map_or(to, |(_, p)| p);
        // `findHitEntities`: the hit entity nearest to the arrow (`computeMargin`).
        #[allow(clippy::cast_precision_loss)]
        let margin = ((a.tick_count - 2) as f32 / 20.0).clamp(0.0, 0.3);
        let area = e
            .aabb()
            .expand_towards(velocity.x, velocity.y, velocity.z)
            .inflate(1.0, 1.0, 1.0);
        let mut hits: Vec<(EntityId, f64, Vec3)> = Vec::new();
        ents.store.for_each_in(&area, false, |other| {
            if other == id || a.pierced.contains(&other) {
                return;
            }
            if !a.left_owner && a.owner == Some(other) {
                return;
            }
            // `canHitEntity`: anything a projectile can hit (`canBeHitByProjectile`).
            if !crate::effect::thrown::can_be_hit(ents, other) {
                return;
            }
            let Some(oe) = ents.store.get(other) else {
                return;
            };
            // `ProjectileUtil.getManyEntityHitResult`: the exact box, else the box grown by
            // the margin when the way from there to its centre is free.
            let bb = oe.aabb();
            let hit = aabb_clip(&bb, pos, end).or_else(|| {
                if margin <= 0.0 {
                    return None;
                }
                let m = f64::from(margin);
                let outside = aabb_clip(&bb.inflate(m, m, m), pos, end)?;
                let towards = physics::clip(level as &dyn Blocks, outside, bb.center())
                    .map_or(bb.center(), |(_, p)| p);
                aabb_clip(&bb, outside, towards).map(|_| outside)
            });
            if let Some(hit) = hit {
                hits.push((other, pos.distance_to_sqr(oe.pos()), hit));
            }
        });
        // `stepMoveAndHit`: the entities on the way, nearest first, are hit one after the
        // other until one stops the arrow; a piercing arrow then flies on.
        hits.sort_by(|x, y| x.1.total_cmp(&y.1));
        let mut deflected = false;
        if let Some(&(_, _, first)) = hits.first() {
            pos = first;
        }
        // Whether an entity was hit: Bukkit's `ProjectileHitEvent` may make the arrow ignore
        // every one on its way.
        let mut hit_any = false;
        for &(target, _, hit) in &hits {
            if !super::missile::hit_event(
                level,
                ents,
                id,
                a.owner,
                super::missile::HitWhat::Entity(target),
            ) {
                continue;
            }
            hit_any = true;
            // `hitTargetOrDeflectSelf`: a breeze turns the arrow back, once in a row.
            if super::missile::deflects(ents, target, kind) {
                if a.last_deflected != Some(target) {
                    (velocity, rot) =
                        super::missile::reverse(ents, env, target, velocity, rot, &mut random);
                    a.last_deflected = Some(target);
                }
                deflected = true;
                break;
            }
            // `onHit` posts `PROJECTILE_LAND` at the hit location after `onHitEntity`.
            super::missile::land_event(env.out, level, id, a.owner, None, hit);
            // `onHitEntity`.
            let pow = velocity.length_sqr().sqrt();
            #[allow(clippy::cast_possible_truncation)]
            let mut damage = ceil((pow * a.base_damage).clamp(0.0, 2.147_483_647e9));
            if a.pierce_level > 0 {
                if a.pierced.len() > usize::from(a.pierce_level) {
                    discard = true;
                    break;
                }
                a.pierced.push(target);
            }
            if a.crit {
                let inc = random.next_int_bounded(damage / 2 + 2);
                damage = damage.saturating_add(inc);
            }
            let owner = a.owner.filter(|o| ents.store.contains(*o)).unwrap_or(id);
            let mut source = combat::entity_source(ents, env.types.arrow, owner, id);
            source.position = Some(pos);
            source.projectile_motion = Some(velocity);
            #[allow(clippy::cast_precision_loss)]
            let hurt = combat::hurt(level, ents, env, cx, target, source, damage as f32);
            let living = crate::effect::thrown::is_living(ents, target);
            if hurt && !living {
                // A vehicle, crystal or decoration: only the hit sound (`onHitEntity`).
                let pitch = 1.2 / (random.next_float() * 0.2 + 0.9);
                let seed = crate::survival::sound_seed();
                play_sound(
                    env.out,
                    None,
                    pos,
                    hit_sound(&a),
                    SoundSource::Neutral,
                    1.0,
                    pitch,
                    seed,
                );
                if a.pierce_level == 0 {
                    discard = true;
                    break;
                }
            } else if hurt {
                if a.fire {
                    super::attack::ignite(ents, target, 100);
                }
                // `doKnockback`: Punch pushes along the arrow's flight.
                if a.knockback > 0.0 {
                    let resist = ents
                        .mob(target)
                        .map_or(0.0, |m| m.attrs.knockback_resistance);
                    let flat = Vec3::new(velocity.x, 0.0, velocity.z);
                    if flat.length_sqr() > 0.0 {
                        let push = flat.normalize()
                            * (f64::from(a.knockback) * 0.6 * (1.0 - resist).max(0.0));
                        if let Some(mut e) = ents.store.get_mut(target) {
                            let v = e.as_ref().velocity();
                            e.set_velocity(v + Vec3::new(push.x, 0.1, push.z));
                            e.set_flags(EntityFlags::HAS_IMPULSE, true);
                            e.set_flags(EntityFlags::HURT_MARKED, true);
                        }
                    }
                }
                // `doPostAttackEffectsWithItemSource` (the victim's equipment), then
                // `doPostHurtEffects`: the potion of an arrow, the glowing of a spectral one.
                super::attack::post_attack(level, ents, env, cx, target, &source, None);
                if kind == entity_ids::SPECTRAL_ARROW {
                    let glowing = crate::effect::EffectInstance::new(
                        crate::effect::MobEffect::GLOWING,
                        a.spectral_duration,
                        0,
                    );
                    crate::effect::living::add_to_entity(level, ents, env, cx, target, glowing);
                } else {
                    crate::effect::cloud::arrow_hit(level, ents, env, cx, target, &a.item);
                }
                // `KILLED_BY_ARROW` of a player's arrow that killed what it hit (no piercing).
                if let Some(player) = a.owner.filter(|&o| ents.player(o).is_some()) {
                    let dead = ents.mob(target).map_or_else(
                        || {
                            ents.player(target)
                                .is_some_and(|p| p.survival.is_dead_or_dying())
                        },
                        |m| !m.is_alive(),
                    );
                    if dead {
                        use crate::advancements::{trigger, view};
                        view::fire(
                            ents,
                            player,
                            trigger::TriggerKind::KILLED_BY_ARROW,
                            |ents| {
                                trigger::Event::KilledByArrow(
                                    view::snapshots(ents, &[target]),
                                    1,
                                    a.weapon.clone(),
                                )
                            },
                        );
                    }
                }
                // `owner.setLastHurtMob(entity)`: mobs and players (their wolves join in).
                if let Some(owner) = a.owner {
                    if let Some(m) = ents.mob_mut(owner) {
                        m.last_hurt_mob = Some(target);
                    } else if let Some(p) = ents.player_mut(owner) {
                        p.survival.last_hurt_mob = Some((target, p.survival.tick_count));
                    }
                }
                let pitch = 1.2 / (random.next_float() * 0.2 + 0.9);
                let seed = crate::survival::sound_seed();
                play_sound(
                    env.out,
                    None,
                    pos,
                    hit_sound(&a),
                    SoundSource::Neutral,
                    1.0,
                    pitch,
                    seed,
                );
                if a.pierce_level == 0 {
                    discard = true;
                    break;
                }
            } else {
                // Deflected (`deflect(ProjectileDeflection.REVERSE, ..., 0.2)`): back at half
                // the power, turned by 170° to 190°.
                let turn = 170.0 + random.next_float() * 20.0;
                velocity = velocity * -0.1;
                rot.y_rot += turn;
                if velocity.length_sqr() < 1.0e-7 {
                    discard = true;
                }
                deflected = true;
                break;
            }
        }
        // A piercing arrow that went through everything on its way: no entity it has not
        // pierced is left, so it goes on to the block or the end of its step.
        let flies_on = !hit_any || (!discard && !deflected && a.pierce_level > 0);
        let block_hit = block_hit.filter(|&(hit_pos, at)| {
            !flies_on
                || super::missile::hit_event(
                    level,
                    ents,
                    id,
                    a.owner,
                    super::missile::HitWhat::Block(hit_pos, at),
                )
        });
        if flies_on && let Some((hit_pos, at)) = block_hit {
            // `onHitBlock`.
            let sign = |v: f64| {
                if v > 0.0 {
                    1.0
                } else if v < 0.0 {
                    -1.0
                } else {
                    0.0
                }
            };
            let offset = Vec3::new(sign(velocity.x), sign(velocity.y), sign(velocity.z))
                * f64::from(0.05_f32);
            pos = at - offset;
            velocity = Vec3::ZERO;
            a.last_state = Some(level.block(hit_pos));
            super::missile::hit_block(level, cx, id, hit_pos, at);
            let pitch = 1.2 / (random.next_float() * 0.2 + 0.9);
            let seed = crate::survival::sound_seed();
            play_sound(
                env.out,
                None,
                pos,
                hit_sound(&a),
                SoundSource::Neutral,
                1.0,
                pitch,
                seed,
            );
            super::missile::land_event(env.out, level, id, a.owner, Some(hit_pos), at);
            a.in_ground = true;
            a.shake = 7;
            a.crit = false;
            a.pierce_level = 0;
            a.crossbow_sound = false;
            a.pierced.clear();
        } else if flies_on {
            pos = to;
        }
        inside_blocks(level, cx, id, start_box, start, pos);
        if !super::is_water(level.block(BlockPos::containing(pos.x, pos.y, pos.z))) {
            // `applyInertia(0.99F)`.
            velocity = velocity * f64::from(0.99_f32);
        }
        if !a.in_ground {
            // `applyGravity`: `movement.add(0, -gravity, 0)` (turns -0.0 into 0.0).
            velocity = velocity + Vec3::new(0.0, -0.05, 0.0);
        }
    }
    a.random_state = random.state();
    // `Arrow.tick`: a tipped arrow in the ground for 600 ticks loses its potion.
    if kind == entity_ids::ARROW
        && a.in_ground
        && a.in_ground_time >= 600
        && a.potion() != crate::effect::potion::PotionContents::default()
    {
        env.out.push(crate::survival::Effect::Tracking {
            entity: id,
            with_self: false,
            packet: vanillite_protocol::SharedPacket::of(
                &vanillite_protocol::packets::play::clientbound::EntityEvent {
                    entity_id: network_id,
                    event: 0,
                },
            ),
        });
        a.item = crate::item::ItemStack::new(
            vanillite_data::registry::ITEM
                .id("minecraft:arrow")
                .unwrap_or(0),
            1,
        );
        color_changed = true;
    }
    if discard || !ents.store.contains(id) {
        cx.discarded.push(id);
        return;
    }
    if let Some(mut e) = ents.store.get_mut(id) {
        e.set_pos(pos);
        e.set_velocity(velocity);
        e.set_rot(rot);
        if color_changed || synched != (a.crit, a.pierce_level, a.in_ground) {
            e.mark_data_dirty();
        }
    }
    if let Some(d) = ents.arrow_mut(id) {
        *d = a;
    }
}

/// `Entity.applyEffectsFromBlocks(from, to)` of a projectile whose box was `start_box` at
/// `from`: `entityInside` (buttons, pressure plates, tripwires) of the blocks the box passes
/// on its way ([`crate::survival::inside`]), as world actions.
pub fn inside_blocks(
    level: &Level,
    cx: &mut MobContext,
    id: EntityId,
    start_box: Aabb,
    from: Vec3,
    to: Vec3,
) {
    #[allow(clippy::cast_possible_truncation)]
    let dims = level.entities.store.get(id).map_or(
        vanillite_world::entity::Dimensions {
            width: (start_box.max_x - start_box.min_x) as f32,
            height: (start_box.max_y - start_box.min_y) as f32,
        },
        |e| e.dims(),
    );
    let way = [crate::survival::inside::Movement::plain(from, to)];
    crate::block::plate::for_each_inside(level, dims, &way, |pos| {
        cx.actions.push(super::WorldAction::EntityInside(pos, id));
    });
}

/// `soundEvent`: what the arrow sounds like when it hits.
fn hit_sound(a: &ArrowData) -> &'static str {
    if a.crossbow_sound {
        "minecraft:item.crossbow.hit"
    } else {
        "minecraft:entity.arrow.hit"
    }
}

/// `Projectile.lerpRotation`.
pub fn lerp_rotation(old: f32, new: f32) -> f32 {
    let mut old = old;
    while new - old < -180.0 {
        old -= 360.0;
    }
    while new - old >= 180.0 {
        old += 360.0;
    }
    old + (new - old) * 0.2
}

/// Where the segment enters `b`, as a fraction of the segment (`AABB.clip`: nothing when it
/// starts inside).
pub fn segment_box(from: Vec3, to: Vec3, b: &Aabb) -> Option<f64> {
    aabb_clip_scale(b, from, to)
}

/// `AABB.clip(from, to)`: where the way from `from` to `to` enters the box; nothing when it
/// starts inside.
pub fn aabb_clip(b: &Aabb, from: Vec3, to: Vec3) -> Option<Vec3> {
    let (dx, dy, dz) = (to.x - from.x, to.y - from.y, to.z - from.z);
    aabb_clip_scale(b, from, to).map(|scale| {
        Vec3::new(
            from.x + scale * dx,
            from.y + scale * dy,
            from.z + scale * dz,
        )
    })
}

/// [`aabb_clip`] as the fraction of the way.
fn aabb_clip_scale(b: &Aabb, from: Vec3, to: Vec3) -> Option<f64> {
    let (dx, dy, dz) = (to.x - from.x, to.y - from.y, to.z - from.z);
    let mut scale = 1.0;
    let mut hit = false;
    #[allow(clippy::too_many_arguments)]
    let mut clip_point = |da: f64,
                          db: f64,
                          dc: f64,
                          point: f64,
                          (min_b, max_b): (f64, f64),
                          (min_c, max_c): (f64, f64),
                          (fa, fb, fc): (f64, f64, f64)| {
        let s = (point - fa) / da;
        let pb = fb + s * db;
        let pc = fc + s * dc;
        if 0.0 < s
            && s < scale
            && min_b - 1.0e-7 < pb
            && pb < max_b + 1.0e-7
            && min_c - 1.0e-7 < pc
            && pc < max_c + 1.0e-7
        {
            scale = s;
            hit = true;
        }
    };
    let yz = (from.x, from.y, from.z);
    if dx > 1.0e-7 {
        clip_point(
            dx,
            dy,
            dz,
            b.min_x,
            (b.min_y, b.max_y),
            (b.min_z, b.max_z),
            yz,
        );
    } else if dx < -1.0e-7 {
        clip_point(
            dx,
            dy,
            dz,
            b.max_x,
            (b.min_y, b.max_y),
            (b.min_z, b.max_z),
            yz,
        );
    }
    let zx = (from.y, from.z, from.x);
    if dy > 1.0e-7 {
        clip_point(
            dy,
            dz,
            dx,
            b.min_y,
            (b.min_z, b.max_z),
            (b.min_x, b.max_x),
            zx,
        );
    } else if dy < -1.0e-7 {
        clip_point(
            dy,
            dz,
            dx,
            b.max_y,
            (b.min_z, b.max_z),
            (b.min_x, b.max_x),
            zx,
        );
    }
    let xy = (from.z, from.x, from.y);
    if dz > 1.0e-7 {
        clip_point(
            dz,
            dx,
            dy,
            b.min_z,
            (b.min_x, b.max_x),
            (b.min_y, b.max_y),
            xy,
        );
    } else if dz < -1.0e-7 {
        clip_point(
            dz,
            dx,
            dy,
            b.max_z,
            (b.min_x, b.max_x),
            (b.min_y, b.max_y),
            xy,
        );
    }
    hit.then_some(scale)
}

/// `AbstractArrow.playerTouch` and `tryPickup` of arrows and tridents: a player takes a
/// projectile in the ground (a returning trident too) once it no longer shakes. Returns whether
/// it was taken; the caller sends `take` and removes it.
pub fn try_pickup(
    ents: &mut crate::entities::Entities,
    projectile: EntityId,
    player: &mut crate::player::PlayerData,
    player_id: EntityId,
) -> bool {
    if ents.missile(projectile).is_some() {
        return super::fam::undead::trident::try_pickup(ents, projectile, player, player_id);
    }
    let Some(a) = ents.arrow(projectile) else {
        return false;
    };
    if !a.in_ground || a.shake > 0 {
        return false;
    }
    let infinite = player.game_mode == vanillite_config::GameMode::Creative;
    match a.pickup {
        1 => {
            let mut item = a.item.clone();
            player.inventory.add(None, &mut item, infinite)
        }
        2 => infinite,
        _ => false,
    }
}
