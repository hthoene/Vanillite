//! Nautiluses and zombie nautiluses (`AbstractNautilus`, `Nautilus`, `ZombieNautilus`):
//! water mounts tamed with their food, dashing under water with a saddle, drying out on land
//! (nautiluses), zombie variants by biome; their brain is in [`super::ai`].

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Vec3};
use vanillite_nbt::Compound;
use vanillite_world::entity::EntityId;

use super::{SpawnContext, State, encode_ageable, load_ageable, save_ageable};
use crate::entities::Entities;
use crate::item::EquipmentSlot;
use crate::level::Level;
use crate::mob::animal::{self, Interaction};
use crate::mob::fam::{Hurt, Interact};
use crate::mob::nav::{LookKind, MoveKind, NavKind};
use crate::mob::{Environment, Kind, MobData, Tick};
use crate::player::encode::{DataWriter, serializer};
use crate::survival::damage::DamageSource;
use crate::survival::player::Env;

/// `DASH_COOLDOWN_TICKS`.
const DASH_COOLDOWN_TICKS: i32 = 40;

/// The nautilus part of a mob.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Nautilus {
    /// `DASH`.
    pub dash: bool,
    pub dash_cooldown: i32,
    /// The zombie nautilus variant (`minecraft:zombie_nautilus_variant` id: 0 temperate,
    /// 1 warm).
    pub variant: i32,
}

fn nautilus(m: &MobData) -> Nautilus {
    match super::state(m) {
        State::Nautilus(n) => n.clone(),
        _ => Nautilus::default(),
    }
}

fn nautilus_mut(m: &mut MobData) -> &mut Nautilus {
    let s = super::state_mut(m);
    if !matches!(s, State::Nautilus(_)) {
        *s = State::Nautilus(Nautilus::default());
    }
    match s {
        State::Nautilus(n) => n,
        _ => unreachable!("set above"),
    }
}

pub fn init(m: &mut MobData) {
    nautilus_mut(m);
    // `SmoothSwimmingMoveControl(this, 85, 10, 0.011, 0, true)`,
    // `SmoothSwimmingLookControl(this, 10)`, `WaterBoundPathNavigation`.
    m.move_ctl.kind = MoveKind::SmoothSwimming {
        max_turn_x: 85,
        max_turn_y: 10,
        in_water_speed: 0.011,
        outside_water_speed: 0.0,
        apply_gravity: true,
    };
    m.look.kind = LookKind::SmoothSwimming { max_y_rot: 10 };
    m.nav.kind = NavKind::Water { breaching: false };
    super::ai::nautilus_brain(m);
}

/// The sound of a nautilus by state (`ambient`, `hurt`, `death`, `eat`, `dash`,
/// `dash_ready`, `swim`): babies have their own, on land the `_land` ones.
pub fn sound(m: &MobData, what: &str) -> &'static str {
    let under = m.fluids.eye_in_water;
    match (m.kind, m.baby, what, under) {
        (Kind::ZombieNautilus, _, "ambient", true) => "minecraft:entity.zombie_nautilus.ambient",
        (Kind::ZombieNautilus, _, "ambient", false) => {
            "minecraft:entity.zombie_nautilus.ambient_land"
        }
        (Kind::ZombieNautilus, _, "hurt", true) => "minecraft:entity.zombie_nautilus.hurt",
        (Kind::ZombieNautilus, _, "hurt", false) => "minecraft:entity.zombie_nautilus.hurt_land",
        (Kind::ZombieNautilus, _, "death", true) => "minecraft:entity.zombie_nautilus.death",
        (Kind::ZombieNautilus, _, "death", false) => {
            "minecraft:entity.zombie_nautilus.death_land"
        }
        (Kind::ZombieNautilus, _, "dash", true) => "minecraft:entity.zombie_nautilus.dash",
        (Kind::ZombieNautilus, _, "dash", false) => "minecraft:entity.zombie_nautilus.dash_land",
        (Kind::ZombieNautilus, _, "dash_ready", true) => {
            "minecraft:entity.zombie_nautilus.dash_ready"
        }
        (Kind::ZombieNautilus, _, "dash_ready", false) => {
            "minecraft:entity.zombie_nautilus.dash_ready_land"
        }
        (Kind::ZombieNautilus, _, _, _) => "minecraft:entity.zombie_nautilus.eat",
        (_, true, "ambient", true) => "minecraft:entity.baby_nautilus.ambient",
        (_, true, "ambient", false) => "minecraft:entity.baby_nautilus.ambient_land",
        (_, true, "hurt", true) => "minecraft:entity.baby_nautilus.hurt",
        (_, true, "hurt", false) => "minecraft:entity.baby_nautilus.hurt_land",
        (_, true, "death", true) => "minecraft:entity.baby_nautilus.death",
        (_, true, "death", false) => "minecraft:entity.baby_nautilus.death_land",
        (_, true, "eat", _) => "minecraft:entity.baby_nautilus.eat",
        (_, _, "ambient", true) => "minecraft:entity.nautilus.ambient",
        (_, _, "ambient", false) => "minecraft:entity.nautilus.ambient_land",
        (_, _, "hurt", true) => "minecraft:entity.nautilus.hurt",
        (_, _, "hurt", false) => "minecraft:entity.nautilus.hurt_land",
        (_, _, "death", true) => "minecraft:entity.nautilus.death",
        (_, _, "death", false) => "minecraft:entity.nautilus.death_land",
        (_, _, "dash", true) => "minecraft:entity.nautilus.dash",
        (_, _, "dash", false) => "minecraft:entity.nautilus.dash_land",
        (_, _, "dash_ready", true) => "minecraft:entity.nautilus.dash_ready",
        (_, _, "dash_ready", false) => "minecraft:entity.nautilus.dash_ready_land",
        _ => "minecraft:entity.nautilus.eat",
    }
}

/// `AbstractNautilus.checkNautilusSpawnRules`: between 25 and 5 blocks below the sea, water
/// below and above.
pub fn check_spawn_rules(level: &Level, env: &Environment, pos: BlockPos) -> bool {
    let sea = env.sea_level;
    let below = level.block(BlockPos::new(pos.x, pos.y - 1, pos.z));
    let above = level.block(BlockPos::new(pos.x, pos.y + 1, pos.z));
    pos.y >= sea - 25
        && pos.y <= sea - 5
        && crate::mob::is_water(below)
        && above.block() == vanillite_data::block::ids::WATER
}

/// `finalizeSpawn`: the attack cooldown memory and the zombie variant by biome.
pub fn finalize(m: &mut MobData, ctx: &SpawnContext<'_>) {
    if m.kind == Kind::ZombieNautilus {
        let warm = ctx.biome_is("minecraft:spawns_coral_variant_zombie_nautilus");
        nautilus_mut(m).variant = i32::from(warm);
    }
    super::ai::nautilus_init_memories(m);
}

/// Wears a saddle.
pub fn is_saddled(m: &MobData) -> bool {
    !m.equipment.get(EquipmentSlot::Saddle).is_empty()
}

/// `AbstractNautilus.travelInWater` (the speed, then 90 % drag; the nav fork's fluid travel
/// handles land and lava).
pub fn travel(t: &mut Tick<'_, '_>, input: Vec3) -> bool {
    if !t.mob_ref().fluids.in_water() {
        return false;
    }
    let speed = t.mob_ref().speed;
    super::ghast::move_relative(t, speed, input);
    let v = t.velocity();
    crate::mob::living::move_self(t, v);
    if t.exists() {
        let v = t.velocity();
        t.set_velocity(v * 0.9);
    }
    true
}

/// `AbstractNautilus.tick`: the dash ends, its cooldown runs out.
pub fn after_tick(t: &mut Tick<'_, '_>) {
    let n = nautilus_mut(t.mob());
    let mut dirty = false;
    if n.dash && n.dash_cooldown < 35 {
        n.dash = false;
        dirty = true;
    }
    if n.dash_cooldown > 0 {
        n.dash_cooldown -= 1;
        if n.dash_cooldown == 0 {
            let s = sound(t.mob_ref(), "dash_ready");
            super::make_sound(t, s);
        }
    }
    if dirty {
        t.core_mut().mark_data_dirty();
    }
}

/// `Nautilus.baseTick`'s air: out of water it dries out (2 damage every second once the air is
/// gone).
pub fn ai_step(t: &mut Tick<'_, '_>) {
    if t.mob_ref().no_ai {
        return;
    }
    if t.mob_ref().is_alive() && !t.mob_ref().fluids.in_water() {
        t.mob().air -= 1;
        if t.mob_ref().air <= -20 {
            t.mob().air = 0;
            let kind = t.env.types.named("minecraft:dry_out");
            let id = t.id;
            crate::mob::combat::hurt(t.level, t.ents, t.env, t.cx, id, DamageSource::of(kind), 2.0);
        }
    } else {
        t.mob().air = 300;
    }
}

/// `customServerAiStep`: the brain and the activity, then `checkRestriction` of tame ones.
pub fn custom_server_ai_step(t: &mut Tick<'_, '_>) {
    super::ai::tick(t);
    let leashed = t.ents.leashes.contains_key(&t.id);
    let ridden = !t.ents.riding.passengers_of(t.id).is_empty();
    if !leashed && !ridden && t.mob_ref().tame {
        let m = t.mob_ref();
        let radius = if !m.baby && !is_saddled(m) { 32 } else { 16 };
        let here = t.block_pos();
        let c = m.restrict_center;
        let r = f64::from(radius + 8);
        let far = f64::from(c.x - here.x).powi(2)
            + f64::from(c.y - here.y).powi(2)
            + f64::from(c.z - here.z).powi(2)
            >= r * r;
        if !m.has_restriction() || far || m.restrict_radius != radius {
            let m = t.mob();
            m.restrict_center = here;
            m.restrict_radius = radius;
        }
    }
}

/// `hurtServer`: anger at the living attacker (`NautilusAi.setAngerTarget`).
pub fn after_hurt(h: &mut Hurt<'_, '_>) {
    let Some(cause) = h.source.cause else {
        return;
    };
    if h.ents.mob(cause).is_none() && h.ents.player(cause).is_none() {
        return;
    }
    let uuid = h.ents.store.get(cause).map(|e| e.uuid());
    if let (Some(uuid), Some(m)) = (uuid, h.ents.mob_mut(h.target)) {
        super::ai::nautilus_set_anger(m, uuid, h.env.game_time);
    }
}

/// `handleStartJump`: the dash sound and flag of a saddled nautilus.
pub fn handle_start_jump(ents: &mut Entities, env: &mut Env<'_>, id: EntityId, strength: i32) {
    let Some(m) = ents.mob(id) else {
        return;
    };
    if strength <= 0 || !is_saddled(m) {
        return;
    }
    let s = sound(m, "dash");
    crate::mob::combat::mob_sound(ents, env, id, s);
    if let Some(m) = ents.mob_mut(id) {
        let n = nautilus_mut(m);
        n.dash = true;
        if n.dash_cooldown == 0 {
            n.dash_cooldown = DASH_COOLDOWN_TICKS;
        }
    }
    if let Some(mut e) = ents.store.get_mut(id) {
        e.mark_data_dirty();
    }
}

/// `isFood`: taming items for wild adults, the food else.
fn is_food(m: &MobData, item: u16) -> bool {
    let tag = if !m.tame && !m.baby {
        "minecraft:nautilus_taming_items"
    } else {
        "minecraft:nautilus_food"
    };
    crate::player::play::game_data().is_some_and(|d| d.item_in_tag(item, tag))
}

/// `AbstractNautilus.mobInteract`.
pub fn interact(ix: &mut Interact<'_, '_>) -> Interaction {
    let Some(m) = ix.ents.mob(ix.target) else {
        return Interaction::Pass;
    };
    if m.baby {
        return animal::animal_interact(ix.ents, ix.env, ix.player, ix.target, ix.hand, ix.stack);
    }
    let tame = m.tame;
    if tame && ix.sneaking {
        // `openCustomInventoryScreen`: the mount inventory menu is not there yet.
        return Interaction::Success;
    }
    let food = is_food(m, ix.stack.item);
    if !ix.stack.is_empty() {
        if !tame && food {
            use_food(ix);
            try_to_tame(ix);
            return Interaction::Success;
        }
        if food && m.health < m.max_health() {
            // `feed(player, hand, stack, 2, 1)`.
            use_food(ix);
            if let Some(m) = ix.ents.mob_mut(ix.target) {
                m.health = (m.health + 2.0).min(m.max_health());
            }
            let s = ix.ents.mob(ix.target).map(|m| sound(m, "eat"));
            if let Some(s) = s {
                crate::mob::combat::mob_sound(ix.ents, ix.env, ix.target, s);
            }
            return Interaction::Success;
        }
        let name = crate::item::item_id(ix.stack).name();
        if name == "minecraft:saddle" && tame && !is_saddled(m) {
            if let Some(m) = ix.ents.mob_mut(ix.target) {
                m.equipment.set(EquipmentSlot::Saddle, ix.stack.with_count(1));
            }
            animal::use_item(ix.ents, ix.player, ix.hand);
            let under = ix.ents.mob(ix.target).is_some_and(|m| m.fluids.eye_in_water);
            let s = if under {
                "minecraft:item.nautilus_saddle_underwater_equip"
            } else {
                "minecraft:item.nautilus_saddle_equip"
            };
            crate::mob::combat::mob_sound(ix.ents, ix.env, ix.target, s);
            return Interaction::Success;
        }
    }
    if tame && !ix.sneaking && !food {
        if crate::riding::start_riding(ix.ents, ix.env, ix.player, ix.target, false)
            && let Some(m) = ix.ents.mob_mut(ix.target)
        {
            m.restrict_radius = -1;
        }
        return Interaction::Success;
    }
    animal::animal_interact(ix.ents, ix.env, ix.player, ix.target, ix.hand, ix.stack)
}

/// `usePlayerItem`: bucket food leaves a water bucket.
fn use_food(ix: &mut Interact<'_, '_>) {
    let bucket = crate::player::play::game_data()
        .is_some_and(|d| d.item_in_tag(ix.stack.item, "minecraft:nautilus_bucket_food"));
    if bucket {
        if let Some(water) = vanillite_data::registry::ITEM.id("minecraft:water_bucket") {
            animal::fill_held(
                ix.ents,
                ix.env,
                ix.player,
                ix.hand,
                crate::item::ItemStack::new(water, 1),
            );
        }
    } else {
        animal::use_item(ix.ents, ix.player, ix.hand);
    }
}

/// `tryToTame`: one in three.
fn try_to_tame(ix: &mut Interact<'_, '_>) {
    let uuid = ix.ents.store.get(ix.player).map(|e| e.uuid());
    let Some(m) = ix.ents.mob_mut(ix.target) else {
        return;
    };
    let tamed = m.random.next_int_bounded(3) == 0;
    if tamed && let Some(uuid) = uuid {
        crate::mob::tame::tame(m, uuid);
        m.nav.stop();
    }
    animal::entity_event(ix.ents, ix.env, ix.target, if tamed { 7 } else { 6 });
    let s = ix.ents.mob(ix.target).map(|m| sound(m, "eat"));
    if let Some(s) = s {
        crate::mob::combat::mob_sound(ix.ents, ix.env, ix.target, s);
    }
    if let Some(mut e) = ix.ents.store.get_mut(ix.target) {
        e.mark_data_dirty();
    }
}

/// The synched data: `AgeableMob`, `TamableAnimal` flags 18 and owner 19, dash 20, the zombie
/// variant 21.
pub fn encode(w: &mut DataWriter<'_>, m: &MobData) {
    encode_ageable(w, m);
    let flags = u8::from(m.in_sitting_pose) | (u8::from(m.tame) << 2);
    w.byte(18, flags, 0);
    w.optional_uuid(19, m.owner);
    let n = nautilus(m);
    w.boolean(20, n.dash, false);
    if m.kind == Kind::ZombieNautilus {
        w.varint(21, serializer::ZOMBIE_NAUTILUS_VARIANT, n.variant, 0);
    }
}

pub fn save(m: &MobData, out: &mut Compound) {
    save_ageable(m, out, m.kind == Kind::Nautilus);
    if m.kind == Kind::ZombieNautilus {
        let v = if nautilus(m).variant == 1 {
            "minecraft:warm"
        } else {
            "minecraft:temperate"
        };
        out.insert("variant", v);
    }
    super::ai::save_brain(m, out);
}

pub fn load(m: &mut MobData, tag: &Compound) {
    load_ageable(m, tag, m.kind == Kind::Nautilus);
    if m.kind == Kind::ZombieNautilus {
        let warm = matches!(
            tag.get("variant").and_then(vanillite_nbt::Tag::as_str),
            Some("minecraft:warm" | "warm")
        );
        nautilus_mut(m).variant = i32::from(warm);
    }
    super::ai::load_brain(m, tag);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variant_round_trip() {
        let mut m = MobData::new(Kind::ZombieNautilus, 3);
        nautilus_mut(&mut m).variant = 1;
        let mut out = Compound::new();
        save(&m, &mut out);
        let mut n = MobData::new(Kind::ZombieNautilus, 4);
        load(&mut n, &out);
        assert_eq!(nautilus(&n).variant, 1);
        assert_eq!(n.move_ctl.kind, m.move_ctl.kind);
    }

    #[test]
    fn sounds_by_state() {
        let mut m = MobData::new(Kind::Nautilus, 1);
        assert_eq!(sound(&m, "hurt"), "minecraft:entity.nautilus.hurt_land");
        m.fluids.eye_in_water = true;
        m.baby = true;
        assert_eq!(sound(&m, "ambient"), "minecraft:entity.baby_nautilus.ambient");
    }
}
