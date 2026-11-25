//! Thrown tridents (`ThrownTrident`, `AbstractArrow` flight) as the drowned throw them.
//!
//! The missile fields: `ints[0]` the ticks in the ground (despawn), `ints[1]` the block state
//! it sticks in plus one, `ints[2]` bit 0 dealt damage, bit 1 returning (no physics), the
//! bits above the ticks in the ground of `inGroundTime`, `ints[3]` the loyalty level.

use vanillite_base::random::RandomSource;
use vanillite_base::{Aabb, BlockPos, Vec3};
use vanillite_data::block::BlockState;
use vanillite_nbt::{Compound, Tag};
use vanillite_protocol::packets::play::clientbound::SoundSource;
use vanillite_world::entity::{EntityId, Rotation};

use crate::item::ItemStack;
use crate::mob::missile::{MissileData, MissileKind, MissileTick};
use crate::mob::physics::{self, Blocks};
use crate::mob::projectile::{lerp_rotation, segment_box, shoot};
use crate::mob::{Tick, atan2_deg, combat};
use crate::player::encode::DataWriter;
use crate::survival::play_sound;

const DEALT: i32 = 1;
const RETURNING: i32 = 2;

/// Throws a trident from `start` along `dir` (`spawnProjectileUsingShoot` with power 1.6).
pub fn throw(t: &mut Tick<'_, '_>, start: Vec3, dir: Vec3, item: ItemStack, uncertainty: f32) {
    let (velocity, rot) = shoot(&mut t.mob().random, dir, 1.6, uncertainty);
    let mut d = MissileData::new(MissileKind::Trident);
    d.ints[3] = loyalty(&item);
    d.item = item;
    crate::mob::missile::spawn_from(t, start, velocity, rot, d);
}

/// The Loyalty level of a trident (`getTridentReturnToOwnerAcceleration`).
fn loyalty(item: &ItemStack) -> i32 {
    crate::player::play::game_data()
        .map_or(0, |d| d.enchantment_level(item, "minecraft:loyalty"))
        .clamp(0, 127)
}

fn in_ground_time(d: &MissileData) -> i32 {
    d.ints[2] >> 2
}

fn set_in_ground_time(d: &mut MissileData, v: i32) {
    d.ints[2] = (d.ints[2] & 3) | (v.max(0) << 2);
}

fn sound(t: &mut MissileTick<'_, '_>, pos: Vec3, s: &str, volume: f32, pitch: f32) {
    let seed = t.env.random.next_long();
    play_sound(
        t.env.out,
        None,
        pos,
        s,
        SoundSource::Neutral,
        volume,
        pitch,
        seed,
    );
}

/// `ThrownTrident.tick` with `AbstractArrow.tick`.
pub fn tick(t: &mut MissileTick<'_, '_>) {
    let id = t.id;
    let Some(mut d) = t.ents.missile(id).cloned() else {
        return;
    };
    if d.kind != MissileKind::Trident {
        return;
    }
    let Some(e) = t.ents.store.get(id) else {
        return;
    };
    let mut pos = e.pos();
    let mut velocity = e.velocity();
    let mut rot = e.rot();
    let aabb = e.aabb();
    if in_ground_time(&d) > 4 {
        d.ints[2] |= DEALT;
    }
    // Loyalty: return to the owner once it hit something.
    let loyalty = d.ints[3];
    let owner = d.owner.filter(|&o| t.ents.store.contains(o));
    if loyalty > 0
        && d.ints[2] & (DEALT | RETURNING) != 0
        && let Some(o) = owner
    {
        let alive = t.ents.mob(o).is_some_and(crate::mob::MobData::is_alive)
            || t.ents
                .player(o)
                .is_some_and(|p| !p.survival.is_dead_or_dying());
        if !alive {
            d.removed = true;
        } else if let Some(oe) = t.ents.store.get(o) {
            let eye = Vec3::new(
                oe.pos().x,
                oe.pos().y + f64::from(oe.kind().info().eye_height),
                oe.pos().z,
            );
            let width = f64::from(oe.dims().width);
            if t.ents.player(o).is_none() && pos.distance_to_sqr(eye).sqrt() < width + 1.0 {
                d.removed = true;
            } else {
                if d.ints[2] & RETURNING == 0 {
                    sound(t, pos, "minecraft:item.trident.return", 10.0, 1.0);
                }
                d.ints[2] |= RETURNING;
                let v = eye - pos;
                pos.y += v.y * 0.015 * f64::from(loyalty);
                velocity = velocity * 0.95 + v.normalize() * (0.05 * f64::from(loyalty));
            }
        }
        if d.removed {
            if let Some(m) = t.ents.missile_mut(id) {
                *m = d;
            }
            return;
        }
    }
    let no_physics = d.ints[2] & RETURNING != 0;
    let block_pos = BlockPos::containing(pos.x, pos.y, pos.z);
    let state = t.level.block(block_pos);
    if !no_physics && !state.is_air() && inside_shape(state, block_pos, pos) {
        velocity = Vec3::ZERO;
        d.in_ground = true;
    }
    if d.in_ground && !no_physics {
        let last = u16::try_from(d.ints[1] - 1).ok().map(BlockState);
        let probe = Aabb::new(pos.x, pos.y, pos.z, pos.x, pos.y, pos.z).inflate(0.06, 0.06, 0.06);
        if last != Some(state) && physics::no_collision(t.level, &probe, &mut t.cx.boxes) {
            d.in_ground = false;
            let r = &mut *t.env.random;
            velocity = velocity.multiply(
                f64::from(r.next_float() * 0.2),
                f64::from(r.next_float() * 0.2),
                f64::from(r.next_float() * 0.2),
            );
            d.ints[0] = 0;
        } else {
            let ticks = in_ground_time(&d) + 1;
            set_in_ground_time(&mut d, ticks);
            d.ints[0] += 1;
            if d.ints[0] >= 1200 {
                d.removed = true;
            }
        }
    } else {
        set_in_ground_time(&mut d, 0);
        #[allow(clippy::cast_possible_truncation)]
        let y_rot = atan2_deg(velocity.x, velocity.z) as f32;
        let hd = (velocity.x * velocity.x + velocity.z * velocity.z).sqrt();
        #[allow(clippy::cast_possible_truncation)]
        let x_rot = atan2_deg(velocity.y, hd) as f32;
        rot.x_rot = lerp_rotation(rot.x_rot, x_rot);
        rot.y_rot = lerp_rotation(rot.y_rot, y_rot);
        if !d.left_owner {
            let grown = aabb
                .expand_towards(velocity.x, velocity.y, velocity.z)
                .inflate(1.0, 1.0, 1.0);
            let inside = owner.is_some_and(|o| {
                t.ents
                    .store
                    .get(o)
                    .is_some_and(|oe| oe.aabb().intersects(&grown))
            });
            d.left_owner = !inside;
        }
        let to = pos + velocity;
        if no_physics {
            pos = to;
        } else {
            let block_hit = physics::clip(t.level as &dyn Blocks, pos, to);
            let end = block_hit.map_or(to, |(_, p)| p);
            let entity_hit = if d.ints[2] & DEALT == 0 {
                find_hit(t, &d, aabb, pos, end, velocity)
            } else {
                None
            };
            if let Some((target, hit)) = entity_hit {
                pos = hit;
                hit_entity(t, &mut d, target, pos);
                velocity = velocity.multiply(-0.01, -0.1, -0.01);
                let turn = 170.0 + t.env.random.next_float() * 20.0;
                rot.y_rot += turn;
            } else if let Some((hit_pos, at)) = block_hit {
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
                d.ints[1] = i32::from(t.level.block(hit_pos).0) + 1;
                d.in_ground = true;
                let pitch = 1.2 / (t.env.random.next_float() * 0.2 + 0.9);
                sound(t, pos, "minecraft:item.trident.hit_ground", 1.0, pitch);
            } else {
                pos = to;
            }
        }
        // `applyInertia`: tridents keep their speed in water (0.99) like in the air.
        velocity = velocity * f64::from(0.99_f32);
        if !no_physics && !d.in_ground {
            velocity.y -= 0.05;
        }
    }
    if let Some(mut e) = t.ents.store.get_mut(id) {
        e.set_pos(pos);
        e.set_velocity(velocity);
        e.set_rot(Rotation { ..rot });
        e.mark_data_dirty();
    }
    if let Some(m) = t.ents.missile_mut(id) {
        *m = d;
    }
}

/// Whether `pos` is inside the collision shape of the block at `bp`.
fn inside_shape(state: BlockState, bp: BlockPos, pos: Vec3) -> bool {
    state.collision_shape().iter().any(|b| {
        Aabb::new(
            f64::from(bp.x) + b[0],
            f64::from(bp.y) + b[1],
            f64::from(bp.z) + b[2],
            f64::from(bp.x) + b[3],
            f64::from(bp.y) + b[4],
            f64::from(bp.z) + b[5],
        )
        .contains(pos.x, pos.y, pos.z)
    })
}

/// `findHitEntity`: the closest living entity the flight crosses.
fn find_hit(
    t: &MissileTick<'_, '_>,
    d: &MissileData,
    aabb: Aabb,
    from: Vec3,
    to: Vec3,
    velocity: Vec3,
) -> Option<(EntityId, Vec3)> {
    let area = aabb
        .expand_towards(velocity.x, velocity.y, velocity.z)
        .inflate(1.0, 1.0, 1.0);
    let mut best: Option<(EntityId, f64, Vec3)> = None;
    t.ents.store.for_each_in(&area, false, |other| {
        if other == t.id || (!d.left_owner && d.owner == Some(other)) {
            return;
        }
        let alive = t.ents.mob(other).is_some_and(crate::mob::MobData::is_alive)
            || t.ents.player(other).is_some_and(|p| {
                !p.survival.is_dead_or_dying()
                    && p.game_mode != vanillite_config::GameMode::Spectator
            });
        if !alive {
            return;
        }
        let Some(oe) = t.ents.store.get(other) else {
            return;
        };
        let grown = oe.aabb().inflate(0.3, 0.3, 0.3);
        if let Some(f) = segment_box(from, to, &grown) {
            let hit = from + (to - from) * f;
            let dist = from.distance_to_sqr(oe.pos());
            if best.is_none_or(|(_, b, _)| dist < b) {
                best = Some((other, dist, hit));
            }
        }
    });
    best.map(|(id, _, hit)| (id, hit))
}

/// `ThrownTrident.onHitEntity`: 8 damage (with the weapon's enchantments), then it bounces.
fn hit_entity(t: &mut MissileTick<'_, '_>, d: &mut MissileData, target: EntityId, pos: Vec3) {
    let owner = d
        .owner
        .filter(|o| t.ents.store.contains(*o))
        .unwrap_or(t.id);
    let kind = t.env.types.named("minecraft:trident");
    let mut source = combat::entity_source(t.ents, kind, owner, t.id);
    source.position = Some(pos);
    let mut damage = 8.0_f32;
    if !d.item.is_empty() {
        let (enchants, tags) = crate::mob::attack::combat_data();
        let cx = crate::enchant::EnchantContext {
            types: t.env.types,
            tags,
            this: crate::mob::attack::facts(t.ents, target),
            attacker: crate::mob::attack::facts(t.ents, owner),
            direct_attacker: None,
            attacking_player: None,
            source: None,
        };
        damage = enchants.modify_damage(&d.item, &cx, t.env.random, damage);
    }
    d.ints[2] |= DEALT;
    combat::hurt(t.level, t.ents, t.env, t.cx, target, source, damage);
    sound(t, pos, "minecraft:item.trident.hit", 1.0, 1.0);
}

/// `AbstractArrow` and `ThrownTrident` NBT.
pub fn save(d: &MissileData, out: &mut Compound) {
    if d.kind != MissileKind::Trident {
        return;
    }
    out.insert(
        "life",
        i16::try_from(d.ints[0].clamp(0, 32767)).unwrap_or(0),
    );
    out.insert("inGround", d.in_ground);
    out.insert("pickup", 0_i8);
    out.insert("damage", 8.0_f64);
    out.insert("crit", false);
    out.insert("DealtDamage", d.ints[2] & DEALT != 0);
    if !d.item.is_empty() {
        out.insert("item", crate::inventory::stack_to_nbt(&d.item));
    }
}

/// Reads `ThrownTrident` NBT.
pub fn load(d: &mut MissileData, tag: &Compound) {
    if d.kind != MissileKind::Trident {
        return;
    }
    let boolean = |k: &str| matches!(tag.get(k), Some(Tag::Byte(b)) if *b != 0);
    d.in_ground = boolean("inGround");
    d.ints[0] = match tag.get("life") {
        Some(Tag::Short(v)) => i32::from(*v),
        _ => 0,
    };
    if boolean("DealtDamage") {
        d.ints[2] |= DEALT;
    }
    if d.item.is_empty()
        && let Some(Tag::Compound(c)) = tag.get("item")
    {
        d.item = crate::inventory::stack_from_nbt(c);
    }
    d.ints[3] = loyalty(&d.item);
}

/// `AbstractArrow` (flags, pierce level, in ground) and `ThrownTrident` (loyalty, foil) data.
pub fn encode(w: &mut DataWriter<'_>, d: &MissileData) {
    if d.kind != MissileKind::Trident {
        return;
    }
    w.byte(8, 0, 0);
    w.byte(9, 0, 0);
    w.boolean(10, d.in_ground, false);
    w.byte(11, u8::try_from(d.ints[3].clamp(0, 127)).unwrap_or(0), 0);
    w.boolean(12, !crate::item::enchantments(&d.item).is_empty(), false);
}
