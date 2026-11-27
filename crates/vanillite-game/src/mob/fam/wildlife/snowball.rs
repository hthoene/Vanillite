//! Snowballs (`Snowball`, a `ThrowableItemProjectile`) and the flight all throwables share
//! (`ThrowableProjectile.tick`): they fall with gravity 0.03, slow down by 0.99 in air and 0.8
//! in water, go through End gateways, ride bubble columns and stop at the first block or entity they hit. A
//! snowball breaks there; blazes take 3 damage, everything else a damage-free hit.

use vanillite_base::random::{LegacyRandom, RandomSource};
use vanillite_base::{Aabb, Vec3};
use vanillite_config::GameMode;
use vanillite_protocol::packets::play::clientbound::SoundSource;
use vanillite_world::entity::{EntityFlags, EntityId, NetworkIds, Rotation};

use crate::entities::Entities;
use crate::item::ItemStack;
use crate::mob::WorldAction;
use crate::mob::missile::MissileData;
use crate::survival::play_sound;
use crate::survival::player::Env;

use crate::mob::kinds::Kind;
use crate::mob::missile::{MissileKind, MissileTick};
use crate::mob::physics::{self, Blocks};
use crate::mob::projectile::{BubbleDrag, bubble_columns, lerp_rotation, segment_box};
use crate::survival::damage::DamageSource;

/// `Projectile.shoot(x, y, z, power, uncertainty)`.
pub(crate) fn shoot(
    random: &mut LegacyRandom,
    dir: Vec3,
    power: f32,
    uncertainty: f32,
) -> (Vec3, Rotation) {
    crate::mob::projectile::shoot(random, dir, power, uncertainty)
}

/// `canHitEntity`: living, not a spectator, and not the owner until the projectile left it.
fn can_hit(t: &MissileTick<'_, '_>, id: EntityId, owner: Option<EntityId>, left: bool) -> bool {
    if id == t.id || (!left && Some(id) == owner) {
        return false;
    }
    if let Some(p) = t.ents.player(id) {
        return p.game_mode != GameMode::Spectator && !p.survival.is_dead_or_dying();
    }
    t.ents.mob(id).is_some_and(crate::mob::MobData::is_alive) || t.ents.boat(id).is_some()
}

/// The end of a throwable's flight this tick.
pub(super) enum Flight {
    /// It flew on.
    Miss,
    /// It hit an entity (`Some`) or a block (`None`) and stands where it hit; with the
    /// `PROJECTILE_LAND` place: the block that was hit, if any, and the hit location.
    Hit(Option<EntityId>, Option<vanillite_base::BlockPos>, Vec3),
    /// It is gone (below the world).
    Gone,
    /// It went through a nether or end portal into another world: its tick ends here.
    Away,
}

/// `ThrowableProjectile.tick` up to the hit (`hitTargetOrDeflectSelf` is the caller's): the
/// first tick's bubble columns, gravity and inertia, the hit on the way
/// (`getHitResultOnMoveVector`), the move, the End gateways and bubble columns on the way
/// (`applyEffectsFromBlocks`), `checkLeftOwner`, then `baseTick`'s `handlePortal` and the void.
pub(super) fn fly(t: &mut MissileTick<'_, '_>) -> Flight {
    let Some(d) = t.ents.missile(t.id) else {
        return Flight::Gone;
    };
    let (owner, mut left, life) = (d.owner, d.left_owner, d.life);
    let kind = d.kind;
    let Some(core) = t.core() else {
        return Flight::Gone;
    };
    let no_gravity = core.flags().contains(EntityFlags::NO_GRAVITY);
    let drag = if kind == MissileKind::EnderPearl {
        BubbleDrag::Entity
    } else {
        BubbleDrag::Projectile
    };
    // `handleFirstTickBubbleColumn`.
    if life == 1 {
        bubble_columns(t.level, t.ents, t.env.random, t.cx, t.id, None, drag);
    }
    let Some(core) = t.core() else {
        return Flight::Gone;
    };
    let (pos, mut v, aabb, rot) = (core.pos(), core.velocity(), core.aabb(), core.rot());
    // `applyGravity` (`getDefaultGravity`: 0.07 for experience bottles) and `applyInertia`.
    if !no_gravity {
        v.y -= if kind == MissileKind::ExperienceBottle {
            0.07
        } else {
            0.03
        };
    }
    // `isInWater()`: water reaches into the box (where the last tick's `baseTick` saw it).
    let in_water = crate::experience_orb::touching_water(t.level, &aabb);
    let inertia = f64::from(if in_water { 0.8_f32 } else { 0.99_f32 });
    v = v * inertia;
    // `getHitResultOnMoveVector`: blocks first, then entities up to that point.
    let mut to = pos + v;
    let block_hit = physics::clip(t.level as &dyn Blocks, pos, to);
    if let Some((_, at)) = block_hit {
        to = at;
    }
    let search = expand_towards(aabb, v).inflate(1.0, 1.0, 1.0);
    let m = crate::mob::missile::margin(life);
    let mut found = std::mem::take(&mut t.cx.ids);
    found.clear();
    t.ents
        .store
        .for_each_in(&search, false, |id| found.push(id));
    let mut hit: Option<(EntityId, f64)> = None;
    for &id in &found {
        if !can_hit(t, id, owner, left) {
            continue;
        }
        let Some(e) = t.ents.store.get(id) else {
            continue;
        };
        // `AABB.clip`: nothing when the segment starts inside the box.
        let b = e.aabb().inflate(m, m, m);
        if b.contains(pos.x, pos.y, pos.z) {
            continue;
        }
        if let Some(f) = segment_box(pos, to, &b)
            && hit.is_none_or(|(_, best)| f < best)
        {
            hit = Some((id, f));
        }
    }
    t.cx.ids = found;
    // Bukkit's `ProjectileHitEvent`: a cancelled hit is ignored and the throwable flies on.
    let mut block_hit = block_hit;
    if let Some((target, _)) = hit
        && !crate::mob::missile::hit_event(
            t.level,
            t.ents,
            t.id,
            owner,
            crate::mob::missile::HitWhat::Entity(target),
        )
    {
        hit = None;
    }
    if hit.is_none()
        && let Some((block, at)) = block_hit
        && !crate::mob::missile::hit_event(
            t.level,
            t.ents,
            t.id,
            owner,
            crate::mob::missile::HitWhat::Block(block, at),
        )
    {
        block_hit = None;
        to = pos + v;
    }
    let new_pos = match hit {
        Some((_, f)) => pos + (to - pos) * f,
        None => to,
    };
    // `updateRotation`.
    let sd = (v.x * v.x + v.z * v.z).sqrt();
    #[allow(clippy::cast_possible_truncation)]
    let new_rot = Rotation {
        y_rot: lerp_rotation(rot.y_rot, crate::mob::atan2_deg(v.x, v.z) as f32),
        x_rot: lerp_rotation(rot.x_rot, crate::mob::atan2_deg(v.y, sd) as f32),
        y_head_rot: 0.0,
    };
    if let Some(mut e) = t.ents.store.get_mut(t.id) {
        e.set_velocity(v);
        e.set_pos(new_pos);
        e.set_rot(new_rot);
    }
    // `applyEffectsFromBlocks`: End gateways and bubble columns on the way.
    crate::portal::enter_portals(t.level, t.ents, t.id, &mut t.cx.gateways);
    bubble_columns(t.level, t.ents, t.env.random, t.cx, t.id, Some(pos), drag);
    // `Projectile.checkLeftOwner`, after the move.
    if !left {
        let Some(e) = t.ents.store.get(t.id) else {
            return Flight::Gone;
        };
        let area = expand_towards(e.aabb(), e.velocity()).inflate(1.0, 1.0, 1.0);
        left = owner
            .and_then(|o| t.ents.store.get(o))
            .is_none_or(|o| !o.aabb().intersects(&area));
        if let Some(d) = t.data() {
            d.left_owner = left;
        }
    }
    // `baseTick`: `handlePortal`, then `checkBelowWorld`.
    let in_end = t.env.dimension == "minecraft:the_end";
    if crate::portal::tick_portal(t.level, t.ents, t.id, in_end, &mut t.cx.travel) {
        return Flight::Away;
    }
    if t.core()
        .is_some_and(|e| e.pos().y < f64::from(t.level.min_y() - 64))
    {
        if let Some(d) = t.data() {
            d.removed = true;
        }
        return Flight::Gone;
    }
    match (hit, block_hit) {
        (Some((target, _)), _) => Flight::Hit(Some(target), None, new_pos),
        (None, Some((block, at))) => Flight::Hit(None, Some(block), at),
        (None, None) => Flight::Miss,
    }
}

/// `damageSources().thrown(projectile, owner)` of the ticking projectile.
pub(super) fn thrown_source(t: &MissileTick<'_, '_>, owner: Option<EntityId>) -> DamageSource {
    let kind = t.env.types.named("minecraft:thrown");
    let mut source = DamageSource::of(kind);
    source.direct = Some(t.id);
    source.cause = owner.filter(|o| t.ents.store.get(*o).is_some());
    source.position = t.core().map(|e| e.pos());
    if let Some(m) = source.cause.and_then(|c| t.ents.mob(c)) {
        source.cause_is_mob = true;
        source.cause_name = Some(m.kind.translation_key());
    }
    source
}

/// `ThrowableProjectile.tick` of a snowball.
pub(super) fn tick(t: &mut MissileTick<'_, '_>) {
    let Some(d) = t.ents.missile(t.id) else {
        return;
    };
    match d.kind {
        MissileKind::Snowball => {}
        MissileKind::EnderPearl => return super::ender_pearl::tick(t),
        MissileKind::Egg | MissileKind::ExperienceBottle => return tick_egg_or_bottle(t),
        MissileKind::EyeOfEnder => return super::eye_of_ender::tick(t),
        MissileKind::FireworkRocket => return crate::firework::tick(t),
        MissileKind::FishingBobber => return crate::fishing::tick(t),
        _ => return,
    }
    let owner = d.owner;
    let Flight::Hit(target, block, at) = fly(t) else {
        return;
    };
    if let Some(target) = target {
        let source = thrown_source(t, owner);
        let damage = if t.ents.mob(target).is_some_and(|m| m.kind == Kind::Blaze) {
            3.0
        } else {
            0.0
        };
        crate::mob::combat::hurt(t.level, t.ents, t.env, t.cx, target, source, damage);
    }
    if let Some(block) = block {
        crate::mob::missile::hit_block(t.level, t.cx, t.id, block, at);
    }
    crate::mob::missile::land_event(t.env.out, t.level, t.id, owner, block, at);
    break_event(t);
    if let Some(d) = t.data() {
        d.removed = true;
    }
}

/// The break particles of a thrown item (entity event 3).
fn break_event(t: &mut MissileTick<'_, '_>) {
    if let Some(e) = t.ents.store.get(t.id) {
        let network_id = e.network_id().0;
        let id = t.id;
        t.env.out.push(crate::survival::Effect::Tracking {
            entity: id,
            with_self: false,
            packet: vanillite_protocol::SharedPacket::of(
                &vanillite_protocol::packets::play::clientbound::EntityEvent {
                    entity_id: network_id,
                    event: 3,
                },
            ),
        });
    }
}

/// `AABB.expandTowards(movement)`.
fn expand_towards(b: Aabb, v: Vec3) -> Aabb {
    Aabb::new(
        if v.x < 0.0 { b.min_x + v.x } else { b.min_x },
        if v.y < 0.0 { b.min_y + v.y } else { b.min_y },
        if v.z < 0.0 { b.min_z + v.z } else { b.min_z },
        if v.x > 0.0 { b.max_x + v.x } else { b.max_x },
        if v.y > 0.0 { b.max_y + v.y } else { b.max_y },
        if v.z > 0.0 { b.max_z + v.z } else { b.max_z },
    )
}

/// The kind a thrown item flies as: snowballs, eggs and experience bottles.
pub fn thrown_kind(stack: &ItemStack) -> Option<MissileKind> {
    if stack.is_empty() {
        return None;
    }
    match crate::item::item_id(stack).name() {
        "minecraft:snowball" => Some(MissileKind::Snowball),
        "minecraft:egg" | "minecraft:blue_egg" | "minecraft:brown_egg" => Some(MissileKind::Egg),
        "minecraft:experience_bottle" => Some(MissileKind::ExperienceBottle),
        _ => None,
    }
}

/// `SnowballItem.use`, `EggItem.use` and `ExperienceBottleItem.use`: the throw sound, the
/// projectile from the player's eyes (`spawnProjectileFromRotation` with power 1.5, the bottle
/// 0.7 aimed 20 degrees higher), the use statistic and one item less.
pub fn player_throw(
    ents: &mut Entities,
    env: &mut Env<'_>,
    network_ids: &NetworkIds,
    player: EntityId,
    stack: &mut ItemStack,
    kind: MissileKind,
) {
    let Some(pos) = ents.store.get(player).map(|e| e.pos()) else {
        return;
    };
    let (sound, source) = match kind {
        MissileKind::Egg => ("minecraft:entity.egg.throw", SoundSource::Players),
        MissileKind::ExperienceBottle => (
            "minecraft:entity.experience_bottle.throw",
            SoundSource::Neutral,
        ),
        _ => ("minecraft:entity.snowball.throw", SoundSource::Neutral),
    };
    let pitch = 0.4 / (env.random.next_float() * 0.4 + 0.8);
    play_sound(
        env.out,
        None,
        pos,
        sound,
        source,
        0.5,
        pitch,
        crate::survival::sound_seed(),
    );
    // `ThrowableItemProjectile(type, owner, level, stack)`: at the eyes, 0.1 lower.
    let eye = crate::mob::missile::eye_height(ents, player);
    let start = Vec3::new(pos.x, pos.y + eye - f64::from(0.1_f32), pos.z);
    let mut data = MissileData::new(kind);
    data.item = stack.with_count(1);
    let (y_offset, pow) = if kind == MissileKind::ExperienceBottle {
        (-20.0, 0.7)
    } else {
        (0.0, 1.5)
    };
    crate::mob::missile::spawn_from_rotation(
        ents,
        network_ids,
        player,
        start,
        y_offset,
        pow,
        1.0,
        data,
    );
    let infinite = ents.player_mut(player).is_some_and(|p| {
        p.stats.award(
            crate::stats::Stat::of(crate::stats::StatType::Used, stack.item),
            1,
        );
        p.game_mode == GameMode::Creative
    });
    if !infinite {
        stack.shrink(1);
    }
}

/// `ThrownEgg` and `ThrownExperienceBottle`: the flight, then `onHit`.
fn tick_egg_or_bottle(t: &mut MissileTick<'_, '_>) {
    let Some(d) = t.ents.missile(t.id) else {
        return;
    };
    let (kind, owner) = (d.kind, d.owner);
    let before = t.core().map(|e| (e.pos(), e.velocity()));
    let Flight::Hit(target, block, hit_at) = fly(t) else {
        return;
    };
    let Some((at, velocity)) = t.core().map(|e| (e.pos(), e.velocity())) else {
        return;
    };
    let mut random = vanillite_base::random::LegacyRandom::from_state(
        t.ents.missile(t.id).map_or(0, |d| d.random_state),
    );
    if kind == MissileKind::Egg {
        // `onHitEntity`: a damage-free hit.
        if let Some(target) = target {
            let source = thrown_source(t, owner);
            crate::mob::combat::hurt(t.level, t.ents, t.env, t.cx, target, source, 0.0);
        }
        crate::mob::missile::land_event(t.env.out, t.level, t.id, owner, block, hit_at);
        // `onHit`: one chick in 8, four in 256.
        if random.next_int_bounded(8) == 0 {
            let count = if random.next_int_bounded(32) == 0 {
                4
            } else {
                1
            };
            let y_rot = t.core().map_or(0.0, |e| e.rot().y_rot);
            let variant = egg_variant(t.ents.missile(t.id).map(|d| &d.item));
            for _ in 0..count {
                if !spawn_chick(t, at, y_rot, variant) {
                    break;
                }
            }
        }
        break_event(t);
    } else {
        crate::mob::missile::land_event(t.env.out, t.level, t.id, owner, block, hit_at);
        // `ThrownExperienceBottle.onHit`: the splash particles and sound, 3 to 11 experience.
        let block = vanillite_base::BlockPos::containing(at.x, at.y, at.z);
        t.cx.actions
            .push(WorldAction::LevelEvent(2002, block, -13_083_194));
        t.cx.actions.push(WorldAction::LevelEvent(1053, block, 0));
        let amount = 3 + random.next_int_bounded(5) + random.next_int_bounded(5);
        let direction = match (target, before) {
            (None, Some((from, _))) => {
                let to = from + velocity;
                physics::clip(t.level as &dyn Blocks, from, to)
                    .map_or(Vec3::ZERO, |(b, p)| face_normal(b, p))
            }
            _ => velocity * -1.0,
        };
        let network_ids = std::sync::Arc::clone(t.level.network_ids_arc());
        crate::experience_orb::award_in(t.ents, &network_ids, t.env.random, at, direction, amount);
    }
    if let Some(d) = t.data() {
        d.random_state = random.state();
        d.removed = true;
    }
}

/// The unit normal of the face of block `b` the point `at` lies on.
fn face_normal(b: vanillite_base::BlockPos, at: Vec3) -> Vec3 {
    let local = at - Vec3::new(f64::from(b.x), f64::from(b.y), f64::from(b.z));
    let faces = [
        (local.x.abs(), Vec3::new(-1.0, 0.0, 0.0)),
        ((1.0 - local.x).abs(), Vec3::new(1.0, 0.0, 0.0)),
        (local.y.abs(), Vec3::new(0.0, -1.0, 0.0)),
        ((1.0 - local.y).abs(), Vec3::new(0.0, 1.0, 0.0)),
        (local.z.abs(), Vec3::new(0.0, 0.0, -1.0)),
        ((1.0 - local.z).abs(), Vec3::new(0.0, 0.0, 1.0)),
    ];
    faces
        .iter()
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map_or(Vec3::ZERO, |f| f.1)
}

/// The `chicken/variant` of an egg (`DataComponents.CHICKEN_VARIANT`): 0 cold, 1 temperate,
/// 2 warm.
fn egg_variant(item: Option<&ItemStack>) -> i32 {
    let Some(item) = item else {
        return 1;
    };
    if let Some(id) = vanillite_data::registry::DATA_COMPONENT_TYPE.id("minecraft:chicken/variant")
        && let Some(v) = item.patch().and_then(|p| p.get(id))
        && let Ok(v) = vanillite_protocol::codec::Reader::new(v).varint()
    {
        return v;
    }
    match crate::item::item_id(item).name() {
        "minecraft:blue_egg" => 0,
        "minecraft:brown_egg" => 2,
        _ => 1,
    }
}

/// A chick from an egg (`EntityType.CHICKEN.create`, `setAge(-24000)`, `snapTo`, the egg's
/// variant, `fudgePositionAfterSizeChange(ZERO)`): `false` when it finds no room.
fn spawn_chick(t: &mut MissileTick<'_, '_>, at: Vec3, y_rot: f32, variant: i32) -> bool {
    let seed = crate::mob::new_entity_seed();
    let mut chick = crate::mob::MobData::new(Kind::Chicken, seed);
    chick.age = -24000;
    chick.baby = true;
    chick.variant = variant;
    let height = f64::from(Kind::Chicken.type_id().info().height * 0.5);
    let width = f64::from(Kind::Chicken.type_id().info().width * 0.5);
    // The free centre closest to `at` with a box of the chick's size: the centre may move up
    // to half the chick's height, which lifts it out of a floor the egg broke on.
    let mut feet = at.y - height / 2.0;
    let mut scratch = Vec::new();
    let boxed = |y: f64| {
        Aabb::new(
            at.x - width / 2.0,
            y,
            at.z - width / 2.0,
            at.x + width / 2.0,
            y + height,
            at.z + width / 2.0,
        )
    };
    if !physics::no_collision(t.level as &dyn Blocks, &boxed(feet), &mut scratch) {
        let mut boxes = Vec::new();
        physics::block_boxes(t.level as &dyn Blocks, &boxed(feet), &mut boxes);
        let top = boxes.iter().fold(feet, |m, b| m.max(b.max_y));
        if top - feet > height / 2.0 + 5.0e-7
            || !physics::no_collision(t.level as &dyn Blocks, &boxed(top), &mut scratch)
        {
            return false;
        }
        feet = top;
    }
    let pos = Vec3::new(at.x, feet, at.z);
    let network_ids = std::sync::Arc::clone(t.level.network_ids_arc());
    crate::mob::spawn(
        t.ents,
        &network_ids,
        crate::mob::new_uuid(),
        pos,
        Rotation {
            y_rot,
            x_rot: 0.0,
            y_head_rot: y_rot,
        },
        chick,
    );
    true
}
