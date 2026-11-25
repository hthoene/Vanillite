//! Brain AI mobs: piglins, piglin brutes, hoglins, zoglins, goats, axolotls, frogs, tadpoles,
//! armadillos and sniffers.
//!
//! Each kind builds its brain in `init` (Vanilla's `Brain.Provider` and `XAi.getActivities`)
//! and ticks it in `custom_server_ai_step` (`brain.tick`, then `XAi.updateActivity`). The
//! kind specific behaviours and sensors are the [`Behavior`] and [`Sensor`] values here.

pub mod armadillo;
pub mod axolotl;
pub mod frog;
pub mod goat;
pub mod hoglin;
pub mod piglin;
pub mod sniffer;

use vanillite_base::random::LegacyRandom;
use vanillite_base::{BlockPos, Vec3};
use vanillite_nbt::Compound;
use vanillite_world::entity::{Dimensions, EntityId};

use super::{FamilyHooks, Hurt, Interact, SpawnContext, SpawnReason};
use crate::item::ItemStack;
use crate::level::Level;
use crate::mob::animal::Interaction;
use crate::mob::brain::behavior::Shape;
use crate::mob::brain::{self, MemoryStatus, MemoryType};
use crate::mob::kinds::DefaultSound;
use crate::mob::{Environment, Kind, MobData, Tick};
use crate::player::encode::DataWriter;
use crate::survival::damage::DamageSource;

/// The family's hooks.
#[derive(Debug)]
pub struct Hooks;

/// The kind state of the family's mobs (in [`super::Ext::Brainy`]); each kind uses its
/// fields.
#[derive(Clone, Debug, Default)]
pub struct State {
    /// `attackAnimationRemainingTicks` (hoglins, zoglins).
    pub attack_anim: i32,
    /// `timeInOverworld` (piglins, brutes, hoglins).
    pub time_in_overworld: i32,
    /// `DATA_IMMUNE_TO_ZOMBIFICATION`.
    pub immune_to_zombification: bool,
    /// `cannotBeHunted` (hoglins), `cannotHunt` (piglins).
    pub cannot_hunt: bool,
    /// The piglin inventory (`SimpleContainer(8)`).
    pub inventory: Vec<ItemStack>,
    /// `DATA_IS_CHARGING_CROSSBOW`.
    pub charging_crossbow: bool,
    /// `DATA_IS_DANCING`.
    pub dancing: bool,
    /// `DATA_IS_SCREAMING_GOAT`, `DATA_HAS_LEFT_HORN`, `DATA_HAS_RIGHT_HORN`.
    pub screaming: bool,
    pub left_horn: bool,
    pub right_horn: bool,
    /// `lowerHeadTick` of goats.
    pub lower_head_tick: i32,
    /// Variant index (axolotl variant, frog variant registry id).
    pub variant: i32,
    /// `DATA_PLAYING_DEAD`.
    pub playing_dead: bool,
    /// `FROM_BUCKET`.
    pub from_bucket: bool,
    /// The frog's tongue target (`DATA_TONGUE_TARGET`).
    pub tongue_target: Option<EntityId>,
    /// The tadpole's own age (`Tadpole.age`).
    pub tadpole_age: i32,
    /// Armadillo state (0 idle, 1 rolling, 2 scared, 3 unrolling) and `inStateTicks`.
    pub armadillo_state: u8,
    pub in_state_ticks: i64,
    /// `scuteTime` (armadillos), `dropSeedAtTick` (sniffers).
    pub scute_time: i32,
    /// Sniffer state (0 idling, 1 feeling happy, 2 scenting, 3 sniffing, 4 searching, 5 digging,
    /// 6 rising).
    pub sniffer_state: u8,
    pub drop_seed_at_tick: i32,
    /// `LongJumpToRandomPos.jumpCandidates` and the not preferred ones of frogs.
    pub jump_candidates: Vec<(BlockPos, i32)>,
    pub not_preferred: Vec<(BlockPos, i32)>,
    /// The frog's tongue target as wire value (network id + 1, 0 for none).
    pub tongue_net_id: i32,
    /// The air supply before `baseTick` (axolotls drying out).
    pub pre_air: i32,
}

/// The ticking mob's family state.
pub fn state(m: &MobData) -> &State {
    static EMPTY: std::sync::OnceLock<State> = std::sync::OnceLock::new();
    match &m.ext {
        super::Ext::Brainy(s) => s,
        _ => EMPTY.get_or_init(State::default),
    }
}

/// The family state, mutably (created on first use).
pub fn state_mut(m: &mut MobData) -> &mut State {
    if !matches!(m.ext, super::Ext::Brainy(_)) {
        m.ext = super::Ext::Brainy(State::default());
    }
    match &mut m.ext {
        super::Ext::Brainy(s) => s,
        _ => unreachable!("set above"),
    }
}

/// The family's goals (brain mobs have none).
#[derive(Clone, Copy, Debug)]
pub enum Goal {
    /// Never runs.
    Idle,
}

/// Whether an item is in an item tag of the data packs.
pub fn item_in_tag(item: u16, tag: &str) -> bool {
    crate::player::play::game_data().is_some_and(|d| d.item_in_tag(item, tag))
}

/// Whether a stack is of the named item.
pub fn is_item(stack: &ItemStack, name: &str) -> bool {
    !stack.is_empty() && crate::item::item_id(stack).name() == name
}

/// `makeSound(sound)`: the mob's volume and voice pitch.
pub fn make_sound(t: &mut Tick<'_, '_>, sound: &str) {
    let id = t.id;
    crate::mob::combat::mob_sound(t.ents, t.env, id, sound);
}

/// `level.playSound(null, body, sound, source, volume, pitch)`.
pub fn sound_at(
    t: &mut Tick<'_, '_>,
    sound: &str,
    source: vanillite_protocol::packets::play::clientbound::SoundSource,
    volume: f32,
    pitch: f32,
) {
    if t.mob_ref().silent {
        return;
    }
    let pos = t.pos();
    let seed = crate::survival::sound_seed();
    crate::survival::play_sound(t.env.out, None, pos, sound, source, volume, pitch, seed);
}

/// `getVoicePitch`.
pub fn voice_pitch(m: &mut MobData) -> f32 {
    use vanillite_base::random::RandomSource;
    let a = m.random.next_float();
    let b = m.random.next_float();
    if m.baby {
        (a - b) * 0.2 + 1.5
    } else {
        (a - b) * 0.2 + 1.0
    }
}

/// `level.broadcastEntityEvent(body, event)`.
pub fn entity_event(t: &mut Tick<'_, '_>, event: i8) {
    let id = t.id;
    crate::mob::living::entity_event(t, id, event);
}

/// A tick context for a hit (`hurtServer` overrides use the brain).
pub fn hurt_tick<'a, 'e>(h: &'a mut Hurt<'_, 'e>) -> Tick<'a, 'e> {
    Tick {
        level: h.level,
        ents: &mut *h.ents,
        env: &mut *h.env,
        cx: &mut *h.cx,
        id: h.target,
    }
}

/// The living entity that caused a damage source.
pub fn living_cause(t: &Tick<'_, '_>, source: &DamageSource) -> Option<EntityId> {
    source
        .cause
        .filter(|&c| t.ents.player(c).is_some() || t.ents.mob(c).is_some())
}

/// `PIGLINS_ZOMBIFY`: outside the Nether.
pub fn zombifies(t: &Tick<'_, '_>) -> bool {
    t.env.dimension != "minecraft:the_nether"
}

/// `Mob.convertTo(type, ConversionParams.single(this, keepEquipment, preserveLoot))`: the new
/// mob takes the place of this one, which is discarded.
pub fn convert_to(
    t: &mut Tick<'_, '_>,
    kind: Kind,
    keep_equipment: bool,
    preserve_loot: bool,
) -> Option<EntityId> {
    let seed = t.mob().random.next_long_seed();
    let mut new = MobData::new(kind, seed);
    let old = t.mob_ref().clone();
    if keep_equipment {
        new.equipment = old.equipment.clone();
    }
    new.fall_distance = old.fall_distance;
    new.last_hurt_by_player_time = old.last_hurt_by_player_time;
    new.hurt_time = old.hurt_time;
    new.y_body_rot = old.y_body_rot;
    new.effects = old.effects.clone();
    if old.baby {
        new.baby = true;
    }
    if old.kind.ageable() && kind.ageable() {
        new.age = old.age;
        new.forced_age = old.forced_age;
        new.forced_age_timer = old.forced_age_timer;
    }
    if let Some(angry) = old
        .brain
        .as_deref()
        .and_then(|b| b.uuid(MemoryType::AngryAt))
        && let Some(b) = new.brain.as_deref_mut()
    {
        b.set(MemoryType::AngryAt, brain::MemValue::Uuid(angry));
    }
    if preserve_loot {
        new.can_pick_up_loot = old.can_pick_up_loot;
    }
    new.left_handed = old.left_handed;
    new.no_ai = old.no_ai;
    new.persistent = old.persistent;
    new.fire_ticks = old.fire_ticks;
    new.silent = old.silent;
    new.custom_name = old.custom_name.clone();
    new.damage_cooldown = old.damage_cooldown;
    let (pos, velocity, rot, on_ground) = {
        let c = t.core();
        (
            c.pos(),
            c.velocity(),
            c.rot(),
            c.flags()
                .contains(vanillite_world::entity::EntityFlags::ON_GROUND),
        )
    };
    let uuid = crate::mob::new_uuid();
    let id = crate::mob::spawn(t.ents, t.level.network_ids(), uuid, pos, rot, new);
    if let Some(mut e) = t.ents.store.get_mut(id) {
        e.set_velocity(velocity);
        e.set_flags(vanillite_world::entity::EntityFlags::ON_GROUND, on_ground);
    }
    if let Some(v) = t.ents.riding.vehicle_of(t.id) {
        t.ents.riding.remove(t.id);
        t.ents.riding.add(id, v, false);
    }
    let m = t.mob();
    m.removed = true;
    m.dead = true;
    Some(id)
}

/// Seeds for converted and offspring mobs.
trait NextSeed {
    fn next_long_seed(&mut self) -> i64;
}

impl NextSeed for LegacyRandom {
    fn next_long_seed(&mut self) -> i64 {
        use vanillite_base::random::RandomSource;
        self.next_long()
    }
}

/// `Animal.finalizeSpawnChildFromBreeding` without the child: both parents rest, the love
/// ends and experience drops.
pub fn finalize_child_from_breeding(t: &mut Tick<'_, '_>, partner: EntityId) {
    use vanillite_base::random::RandomSource;
    for id in [t.id, partner] {
        if let Some(m) = t.ents.mob_mut(id) {
            m.age = 6000;
            m.in_love = 0;
            m.love_cause = None;
        }
    }
    entity_event(t, 18);
    if t.env.rules.bool(crate::survival::rules::Rule::MOB_DROPS) {
        let xp = t.mob().random.next_int_bounded(7) + 1;
        let pos = t.pos();
        crate::mob::combat::award_experience(t.env, pos, xp);
    }
}

/// The family's `spawnChildFromBreeding` overrides (frogs get pregnant, sniffers lay an
/// egg); `false` for the default.
pub fn custom_breed(t: &mut Tick<'_, '_>, partner: EntityId) -> bool {
    match t.mob_ref().kind {
        Kind::Frog => frog::spawn_child_from_breeding(t, partner),
        Kind::Sniffer => sniffer::spawn_child_from_breeding(t, partner),
        _ => return false,
    }
    true
}

/// The family's `canMate` additions (sniffers only mate while idle).
pub fn can_mate(a: &MobData, b: &MobData) -> bool {
    if a.kind == Kind::Sniffer {
        return sniffer::can_mate(a) && sniffer::can_mate(b);
    }
    true
}

/// The items of a gift or interaction loot table with the mob as `THIS_ENTITY`.
pub fn gift_loot(t: &mut Tick<'_, '_>, table: &str) -> Vec<ItemStack> {
    let Some(data) = crate::player::play::game_data() else {
        return Vec::new();
    };
    let Some(this) = crate::mob::attack::facts(t.ents, t.id) else {
        return Vec::new();
    };
    let none_level = |_: &str| 0;
    let entity = crate::loot::EntityLootContext {
        this,
        attacker: None,
        direct_attacker: None,
        last_damage_player: None,
        source: None,
        types: t.env.types,
        tags: &*data,
        attacker_enchantment_level: &none_level,
    };
    let none = |_: &str| 0;
    let in_tag = |item: u16, tag: &str| data.item_in_tag(item, tag);
    let empty = ItemStack::default();
    let ctx = crate::loot::BlockLootContext {
        state: vanillite_data::block::BlockState::AIR,
        tool: &empty,
        enchantment_level: &none,
        item_in_tag: &in_tag,
        explosion_radius: None,
        this_present: true,
        entity: Some(&entity),
        block_entity: None,
    };
    let mut out = Vec::new();
    // The table's random sequence, else the level random.
    data.loot
        .roll_table(t.level, table, &ctx, t.env.random, &mut out);
    out
}

/// `Mob.getTargetFromBrain`: keeps `MobData.target` in sync with `ATTACK_TARGET`.
pub fn sync_target(t: &mut Tick<'_, '_>) {
    let target = brain::brain_ref(t).entity(MemoryType::AttackTarget);
    t.mob().target = target;
}

/// `setAggressive`.
pub fn set_aggressive(t: &mut Tick<'_, '_>, on: bool) {
    if t.mob_ref().aggressive != on {
        t.mob().aggressive = on;
        t.core_mut().mark_data_dirty();
    }
}

impl FamilyHooks for Hooks {
    type Goal = Goal;

    fn ageable(kind: Kind) -> bool {
        matches!(
            kind,
            Kind::Hoglin
                | Kind::Goat
                | Kind::Axolotl
                | Kind::Frog
                | Kind::Armadillo
                | Kind::Sniffer
        )
    }

    fn remove_when_far_away(kind: Kind) -> bool {
        // Hoglins (`removeWhenFarAway` true) and the monsters; animals stay.
        matches!(
            kind,
            Kind::Hoglin | Kind::Zoglin | Kind::Piglin | Kind::PiglinBrute | Kind::Tadpole
        )
    }

    fn requires_custom_persistence(m: &MobData) -> bool {
        match m.kind {
            Kind::Axolotl | Kind::Tadpole => state(m).from_bucket,
            _ => false,
        }
    }

    fn breathes_underwater(kind: Kind) -> bool {
        matches!(
            kind,
            Kind::Axolotl | Kind::Frog | Kind::Tadpole | Kind::Zoglin
        )
    }

    fn fall_damage_immune(kind: Kind) -> bool {
        // `Goat`, `Frog`: the safe fall distance and fall damage multiplier come from their
        // attributes; none of the family is immune.
        let _ = kind;
        false
    }

    fn malus(kind: Kind) -> &'static crate::mob::path::MalusTable {
        match kind {
            Kind::Piglin | Kind::PiglinBrute => piglin::MALUS,
            Kind::Goat => goat::MALUS,
            Kind::Axolotl => axolotl::MALUS,
            Kind::Frog => frog::MALUS,
            Kind::Sniffer => sniffer::MALUS,
            _ => &[],
        }
    }

    fn sound_source(kind: Kind) -> vanillite_protocol::packets::play::clientbound::SoundSource {
        use vanillite_protocol::packets::play::clientbound::SoundSource;
        match kind {
            Kind::Piglin | Kind::PiglinBrute | Kind::Hoglin | Kind::Zoglin => SoundSource::Hostile,
            _ => SoundSource::Neutral,
        }
    }

    fn sound_volume(kind: Kind) -> f32 {
        match kind {
            Kind::Sniffer => 2.0,
            _ => 1.0,
        }
    }

    fn ambient_sound(m: &MobData) -> Option<&'static str> {
        match m.kind {
            Kind::Piglin => piglin::ambient_sound(m),
            Kind::PiglinBrute => piglin::brute_ambient_sound(m),
            Kind::Hoglin => hoglin::ambient_sound(m),
            Kind::Zoglin => Some(hoglin::zoglin_ambient_sound(m)),
            Kind::Goat => Some(goat::sound(m, "ambient")),
            Kind::Axolotl => Some(axolotl::ambient_sound(m)),
            Kind::Frog => Some("minecraft:entity.frog.ambient"),
            Kind::Tadpole => None,
            Kind::Armadillo => armadillo::ambient_sound(m),
            Kind::Sniffer => sniffer::ambient_sound(m),
            _ => Some(m.kind.default_sound(DefaultSound::Ambient)),
        }
    }

    fn hurt_sound(m: &MobData) -> &'static str {
        match m.kind {
            Kind::Goat => goat::sound(m, "hurt"),
            Kind::Tadpole => "minecraft:entity.tadpole.hurt",
            Kind::Armadillo => armadillo::hurt_sound(m),
            _ => m.kind.default_sound(DefaultSound::Hurt),
        }
    }

    fn death_sound(m: &MobData) -> &'static str {
        match m.kind {
            Kind::Goat => goat::sound(m, "death"),
            _ => m.kind.default_sound(DefaultSound::Death),
        }
    }

    fn experience_reward(m: &mut MobData) -> i32 {
        use vanillite_base::random::RandomSource;
        match m.kind {
            // `xpReward` with the equipment bonus of `Mob.getBaseExperienceReward`.
            Kind::Piglin | Kind::PiglinBrute => {
                let base = if m.kind == Kind::Piglin { 5 } else { 20 };
                base + equipment_xp(m)
            }
            Kind::Hoglin => {
                if m.baby {
                    3
                } else {
                    5
                }
            }
            Kind::Zoglin => 5,
            Kind::Tadpole => 0,
            _ => 1 + m.random.next_int_bounded(3),
        }
    }

    fn dimensions(m: &MobData) -> Option<Dimensions> {
        if !m.baby {
            return None;
        }
        match m.kind {
            Kind::Hoglin | Kind::Zoglin => Some(Dimensions {
                width: 0.75,
                height: 0.85,
            }),
            Kind::Piglin => Some(Dimensions {
                width: 0.49,
                height: 0.98,
            }),
            Kind::Goat => Some(Dimensions {
                width: 0.9 * 0.5,
                height: 1.3 * 0.5,
            }),
            Kind::Armadillo => Some(Dimensions {
                width: 0.7 * 0.6,
                height: 0.65 * 0.6,
            }),
            Kind::Sniffer => Some(Dimensions {
                width: 1.9 * 0.5,
                height: 1.75 * 0.5,
            }),
            _ => None,
        }
    }

    fn walk_target_value(kind: Kind, level: &Level, env: &Environment, pos: BlockPos) -> f64 {
        match kind {
            Kind::Hoglin
            | Kind::Zoglin
            | Kind::Goat
            | Kind::Frog
            | Kind::Armadillo
            | Kind::Sniffer => {
                // Brain mobs walk by memories; their `getWalkTargetValue` only matters for
                // random positions.
                match kind {
                    Kind::Hoglin => hoglin::walk_target_value(level, pos),
                    Kind::Armadillo => armadillo::walk_target_value(level, env, pos),
                    _ => crate::mob::kinds::base_walk_target_value(
                        kind.is_monster(),
                        level,
                        env,
                        pos,
                    ),
                }
            }
            Kind::Axolotl => 0.0,
            _ => crate::mob::kinds::base_walk_target_value(kind.is_monster(), level, env, pos),
        }
    }

    fn check_spawn_rules(
        kind: Kind,
        level: &Level,
        env: &Environment,
        pos: BlockPos,
        random: &mut LegacyRandom,
        reason: SpawnReason,
    ) -> bool {
        let below = level.block(BlockPos::new(pos.x, pos.y - 1, pos.z));
        let tags = crate::block::block_tags();
        match kind {
            // `checkPiglinSpawnRules`, `checkHoglinSpawnRules`.
            Kind::Piglin | Kind::Hoglin => below.block().name() != "minecraft:nether_wart_block",
            Kind::PiglinBrute | Kind::Zoglin => {
                crate::mob::spawn::default_spawn_rules(kind, level, env, pos, random, reason)
            }
            // `Goat.checkGoatSpawnRules`.
            Kind::Goat => {
                tags.goats_spawnable_on.contains(below) && animal_bright(level, pos, reason)
            }
            // `Axolotl.checkAxolotlSpawnRules`.
            Kind::Axolotl => tags.axolotls_spawnable_on.contains(below),
            // `Frog.checkFrogSpawnRules`.
            Kind::Frog => {
                tags.frogs_spawnable_on.contains(below) && animal_bright(level, pos, reason)
            }
            // `Armadillo.checkArmadilloSpawnRules`.
            Kind::Armadillo => {
                tags.armadillo_spawnable_on.contains(below) && animal_bright(level, pos, reason)
            }
            Kind::Tadpole => true,
            _ => crate::mob::spawn::default_spawn_rules(kind, level, env, pos, random, reason),
        }
    }

    fn is_food(kind: Kind, item: u16) -> bool {
        let tag = match kind {
            Kind::Hoglin => "minecraft:hoglin_food",
            Kind::Goat => "minecraft:goat_food",
            Kind::Axolotl => "minecraft:axolotl_food",
            Kind::Frog => "minecraft:frog_food",
            Kind::Armadillo => "minecraft:armadillo_food",
            Kind::Sniffer => "minecraft:sniffer_food",
            _ => return false,
        };
        item_in_tag(item, tag)
    }

    fn init(m: &mut MobData) {
        state_mut(m);
        match m.kind {
            Kind::Piglin | Kind::PiglinBrute => piglin::init(m),
            Kind::Hoglin | Kind::Zoglin => hoglin::init(m),
            Kind::Goat => goat::init(m),
            Kind::Axolotl => axolotl::init(m),
            Kind::Frog | Kind::Tadpole => frog::init(m),
            Kind::Armadillo => armadillo::init(m),
            Kind::Sniffer => sniffer::init(m),
            _ => {}
        }
    }

    fn finalize_spawn(m: &mut MobData, ctx: &mut SpawnContext<'_>) {
        match m.kind {
            Kind::Piglin | Kind::PiglinBrute => piglin::finalize_spawn(m, ctx),
            Kind::Hoglin | Kind::Zoglin => hoglin::finalize_spawn(m, ctx),
            Kind::Goat => goat::finalize_spawn(m, ctx),
            Kind::Axolotl => axolotl::finalize_spawn(m, ctx),
            Kind::Frog => frog::finalize_spawn(m, ctx),
            Kind::Armadillo | Kind::Sniffer => animal_group_baby(m, ctx),
            _ => {}
        }
    }

    fn ai_step(t: &mut Tick<'_, '_>) {
        match t.mob_ref().kind {
            Kind::Hoglin | Kind::Zoglin => hoglin::ai_step(t),
            Kind::Goat => goat::ai_step(t),
            Kind::Tadpole => frog::tadpole_ai_step(t),
            Kind::Armadillo => armadillo::ai_step(t),
            _ => {}
        }
    }

    fn before_tick(t: &mut Tick<'_, '_>) {
        match t.mob_ref().kind {
            Kind::Axolotl => axolotl::before_tick(t),
            Kind::Armadillo => armadillo::before_tick(t),
            Kind::Sniffer => sniffer::before_tick(t),
            Kind::Piglin => piglin::before_tick(t),
            _ => {}
        }
    }

    fn register_goals(m: &mut MobData) {
        // A tadpole is an `AbstractFish`: the fish goals besides its brain.
        if m.kind == Kind::Tadpole {
            super::wildlife::fish::register_goals(m);
        }
    }

    fn travel(t: &mut Tick<'_, '_>, input: Vec3) -> bool {
        // `travelInWater` of axolotls and frogs (their own speed, no sinking) and of tadpoles
        // (`AbstractFish`: slow, sinking without a target).
        let kind = t.mob_ref().kind;
        if !t.mob_ref().fluids.in_water() {
            return false;
        }
        match kind {
            Kind::Axolotl | Kind::Frog => {
                let speed = t.mob_ref().speed;
                super::wildlife::water_travel(t, input, speed, false);
                true
            }
            Kind::Tadpole => {
                super::wildlife::water_travel(t, input, 0.01, true);
                true
            }
            _ => false,
        }
    }

    fn custom_server_ai_step(t: &mut Tick<'_, '_>) {
        brain::tick(t);
        if !t.exists() || t.mob_ref().removed {
            return;
        }
        match t.mob_ref().kind {
            Kind::Piglin | Kind::PiglinBrute => piglin::update_activity(t),
            Kind::Hoglin | Kind::Zoglin => hoglin::update_activity(t),
            Kind::Goat => goat::update_activity(t),
            Kind::Axolotl => axolotl::update_activity(t),
            Kind::Frog | Kind::Tadpole => frog::update_activity(t),
            Kind::Armadillo => armadillo::update_activity(t),
            Kind::Sniffer => sniffer::update_activity(t),
            _ => {}
        }
        if !t.exists() || t.mob_ref().removed {
            return;
        }
        sync_target(t);
    }

    fn do_hurt_target(t: &mut Tick<'_, '_>, target: EntityId) -> Option<bool> {
        match t.mob_ref().kind {
            Kind::Hoglin | Kind::Zoglin => Some(hoglin::do_hurt_target(t, target)),
            Kind::PiglinBrute | Kind::Piglin => None,
            _ => None,
        }
    }

    fn after_attack(t: &mut Tick<'_, '_>, target: EntityId) {
        if t.mob_ref().kind == Kind::Axolotl {
            axolotl::after_attack(t, target);
        }
    }

    fn perform_ranged_attack(t: &mut Tick<'_, '_>, target: EntityId, power: f32) {
        if t.mob_ref().kind == Kind::Piglin {
            piglin::shoot_crossbow(t, target, power);
        }
    }

    fn hurt_filter(h: &mut Hurt<'_, '_>, amount: f32) -> Option<f32> {
        let kind = h.ents.mob(h.target).map(|m| m.kind)?;
        match kind {
            Kind::Axolotl => axolotl::hurt_filter(h, amount),
            Kind::Armadillo => armadillo::hurt_filter(h, amount),
            Kind::Goat => Some(amount),
            _ => Some(amount),
        }
    }

    fn after_hurt(h: &mut Hurt<'_, '_>, amount: f32) {
        let Some(kind) = h.ents.mob(h.target).map(|m| m.kind) else {
            return;
        };
        let source = h.source;
        let mut t = hurt_tick(h);
        let Some(cause) = living_cause(&t, &source) else {
            if kind == Kind::Axolotl {
                axolotl::after_hurt(&mut t, None, amount);
            }
            if kind == Kind::Armadillo {
                armadillo::after_hurt(&mut t);
            }
            return;
        };
        match kind {
            Kind::Piglin => piglin::was_hurt_by(&mut t, cause),
            Kind::PiglinBrute => piglin::brute_was_hurt_by(&mut t, cause),
            Kind::Hoglin => hoglin::was_hurt_by(&mut t, cause),
            Kind::Zoglin => hoglin::zoglin_was_hurt_by(&mut t, cause),
            Kind::Axolotl => axolotl::after_hurt(&mut t, Some(cause), amount),
            Kind::Armadillo => armadillo::after_hurt(&mut t),
            Kind::Sniffer | Kind::Goat | Kind::Frog => {}
            _ => {}
        }
    }

    fn on_die(h: &mut Hurt<'_, '_>) {
        let Some(kind) = h.ents.mob(h.target).map(|m| m.kind) else {
            return;
        };
        if kind == Kind::Piglin {
            let mut t = hurt_tick(h);
            piglin::drop_inventory(&mut t);
        }
    }

    fn interact(ix: &mut Interact<'_, '_>) -> Interaction {
        let Some(kind) = ix.ents.mob(ix.target).map(|m| m.kind) else {
            return Interaction::Pass;
        };
        match kind {
            Kind::Piglin => piglin::interact(ix),
            Kind::Hoglin => hoglin::interact(ix),
            Kind::Goat => goat::interact(ix),
            Kind::Axolotl | Kind::Tadpole => axolotl::bucket_or_feed(ix),
            Kind::Frog => crate::mob::animal::animal_interact_pub(ix),
            Kind::Armadillo => armadillo::interact(ix),
            Kind::Sniffer => sniffer::interact(ix),
            _ => Interaction::Pass,
        }
    }

    fn breed_offspring(
        parent: &MobData,
        partner: &MobData,
        child: &mut MobData,
        random: &mut LegacyRandom,
    ) {
        match parent.kind {
            Kind::Hoglin => child.persistent = true,
            Kind::Goat => goat::breed_offspring(parent, partner, child, random),
            Kind::Axolotl => axolotl::breed_offspring(parent, partner, child, random),
            _ => {}
        }
    }

    fn encode_data(w: &mut DataWriter<'_>, m: &MobData) {
        match m.kind {
            Kind::Piglin | Kind::PiglinBrute => piglin::encode(w, m),
            Kind::Hoglin | Kind::Zoglin => hoglin::encode(w, m),
            Kind::Goat => goat::encode(w, m),
            Kind::Axolotl => axolotl::encode(w, m),
            Kind::Frog | Kind::Tadpole => frog::encode(w, m),
            Kind::Armadillo => armadillo::encode(w, m),
            Kind::Sniffer => sniffer::encode(w, m),
            _ => {}
        }
    }

    fn living_flags(m: &MobData) -> u8 {
        u8::from(m.using_item >= 0)
    }

    fn wants_to_pick_up(t: &Tick<'_, '_>, stack: &ItemStack) -> bool {
        match t.mob_ref().kind {
            Kind::Piglin => piglin::wants_to_pick_up(t, stack),
            _ => false,
        }
    }

    fn pick_up_items(t: &mut Tick<'_, '_>) {
        match t.mob_ref().kind {
            Kind::Piglin => piglin::pick_up_items(t),
            // `PiglinBrute.wantsToPickUp`: golden axes only.
            Kind::PiglinBrute => {
                let rules = crate::mob::equipment::PickupRules {
                    wants: |_, s| is_item(s, "minecraft:golden_axe"),
                    preferred_weapon: None,
                };
                crate::mob::equipment::pick_up_items(t, &rules, None, (1.0, 0.0, 1.0));
            }
            // The other brain mobs do not pick up loot.
            _ => {}
        }
    }

    fn save(m: &MobData, out: &mut Compound) {
        if let Some(b) = m.brain.as_deref() {
            brain::save(b, out);
        }
        if Self::ageable(m.kind) {
            save_ageable(m, out);
        }
        match m.kind {
            Kind::Piglin | Kind::PiglinBrute => piglin::save(m, out),
            Kind::Hoglin | Kind::Zoglin => hoglin::save(m, out),
            Kind::Goat => goat::save(m, out),
            Kind::Axolotl => axolotl::save(m, out),
            Kind::Frog | Kind::Tadpole => frog::save(m, out),
            Kind::Armadillo => armadillo::save(m, out),
            Kind::Sniffer => {}
            _ => {}
        }
    }

    fn load(m: &mut MobData, tag: &Compound) {
        if Self::ageable(m.kind) {
            load_ageable(m, tag);
        }
        match m.kind {
            Kind::Piglin | Kind::PiglinBrute => piglin::load(m, tag),
            Kind::Hoglin | Kind::Zoglin => hoglin::load(m, tag),
            Kind::Goat => goat::load(m, tag),
            Kind::Axolotl => axolotl::load(m, tag),
            Kind::Frog | Kind::Tadpole => frog::load(m, tag),
            Kind::Armadillo => armadillo::load(m, tag),
            _ => {}
        }
        if let Some(b) = m.brain.as_deref_mut() {
            brain::load(b, tag);
        }
    }
}

/// `Animal.checkAnimalSpawnRules`' light part: bright enough (natural spawns).
fn animal_bright(level: &Level, pos: BlockPos, reason: SpawnReason) -> bool {
    if reason == SpawnReason::ChunkGeneration {
        return true;
    }
    let (sky, block) = level.light(pos);
    sky.max(block) > 8
}

/// `AgeableMob.finalizeSpawn`: later group members may be babies (5 %).
fn animal_group_baby(m: &mut MobData, ctx: &mut SpawnContext<'_>) {
    use vanillite_base::random::RandomSource;
    if ctx.group_index > 0 && m.random.next_float() <= 0.05 {
        m.age = -24000;
        m.baby = true;
    }
}

/// The experience bonus of the equipment (`Mob.getBaseExperienceReward`): 1–3 per item.
fn equipment_xp(m: &mut MobData) -> i32 {
    use vanillite_base::random::RandomSource;
    let mut xp = 0;
    for slot in crate::mob::equipment::SLOTS {
        if !m.equipment.get(slot).is_empty() && m.equipment.drop_chance(slot) <= 1.0 {
            xp += 1 + m.random.next_int_bounded(3);
        }
    }
    xp
}

/// `AgeableMob`/`Animal` NBT: `Age`, `ForcedAge`, `InLove`, `LoveCause`.
pub fn save_ageable(m: &MobData, out: &mut Compound) {
    out.insert("Age", m.age);
    out.insert("ForcedAge", m.forced_age);
    out.insert("AgeLocked", false);
    out.insert("InLove", m.in_love);
}

/// Reads `Age`, `ForcedAge`, `InLove`.
pub fn load_ageable(m: &mut MobData, tag: &Compound) {
    use vanillite_nbt::Tag;
    let int = |k: &str| match tag.get(k) {
        Some(Tag::Int(v)) => Some(*v),
        _ => None,
    };
    m.age = int("Age").unwrap_or(0);
    m.baby = m.age < 0;
    m.forced_age = int("ForcedAge").unwrap_or(0);
    m.in_love = int("InLove").unwrap_or(0);
}

/// A boolean NBT key.
pub fn nbt_bool(tag: &Compound, key: &str, default: bool) -> bool {
    match tag.get(key) {
        Some(vanillite_nbt::Tag::Byte(b)) => *b != 0,
        _ => default,
    }
}

/// An int NBT key.
pub fn nbt_int(tag: &Compound, key: &str, default: i32) -> i32 {
    match tag.get(key) {
        Some(vanillite_nbt::Tag::Int(v)) => *v,
        _ => default,
    }
}

/// The bottom centre of a block.
pub fn bottom_center(p: BlockPos) -> Vec3 {
    crate::mob::randompos::bottom_center(p)
}

// --- Brain plug-ins (`brain::behavior::FamBehavior`, `brain::sensor::FamSensor`) ---------

/// The family's brain behaviours.
#[derive(Clone, Copy, Debug)]
pub enum Behavior {
    Piglin(piglin::Behavior),
    Goat(goat::Behavior),
    Axolotl(axolotl::Behavior),
    Frog(frog::Behavior),
    Armadillo(armadillo::Behavior),
    Sniffer(sniffer::Behavior),
}

/// The family's brain sensors.
#[derive(Clone, Copy, Debug)]
pub enum Sensor {
    /// `PIGLIN_SPECIFIC_SENSOR`.
    PiglinSpecific,
    /// `PIGLIN_BRUTE_SPECIFIC_SENSOR`.
    PiglinBruteSpecific,
    /// `HOGLIN_SPECIFIC_SENSOR`.
    HoglinSpecific,
    /// `AXOLOTL_ATTACKABLES`.
    AxolotlAttackables,
    /// `FROG_ATTACKABLES`.
    FrogAttackables,
}

/// A brain behaviour spec of the family.
pub fn spec(b: Behavior) -> brain::Spec {
    brain::Spec::fam(brain::behavior::FamBehavior::Brainy(b))
}

/// A sensor of the family.
pub fn sensor(s: Sensor) -> brain::SensorKind {
    brain::SensorKind::Fam(brain::sensor::FamSensor::Brainy(s))
}

pub fn sensor_scan_rate(_s: Sensor) -> i32 {
    20
}

pub fn sensor_requires(s: &Sensor) -> &'static [MemoryType] {
    match s {
        Sensor::PiglinSpecific => piglin::SPECIFIC_SENSOR_MEMORIES,
        Sensor::PiglinBruteSpecific => piglin::BRUTE_SENSOR_MEMORIES,
        Sensor::HoglinSpecific => hoglin::SENSOR_MEMORIES,
        Sensor::AxolotlAttackables => axolotl::SENSOR_MEMORIES,
        Sensor::FrogAttackables => frog::SENSOR_MEMORIES,
    }
}

pub fn sensor_tick(t: &mut Tick<'_, '_>, s: Sensor) {
    match s {
        Sensor::PiglinSpecific => piglin::specific_sensor(t),
        Sensor::PiglinBruteSpecific => piglin::brute_sensor(t),
        Sensor::HoglinSpecific => hoglin::specific_sensor(t),
        Sensor::AxolotlAttackables => axolotl::attackables_sensor(t),
        Sensor::FrogAttackables => frog::attackables_sensor(t),
    }
}

macro_rules! by_behavior {
    ($b:expr, $f:ident($($arg:expr),*)) => {
        match $b {
            Behavior::Piglin(b) => piglin::$f(b $(, $arg)*),
            Behavior::Goat(b) => goat::$f(b $(, $arg)*),
            Behavior::Axolotl(b) => axolotl::$f(b $(, $arg)*),
            Behavior::Frog(b) => frog::$f(b $(, $arg)*),
            Behavior::Armadillo(b) => armadillo::$f(b $(, $arg)*),
            Behavior::Sniffer(b) => sniffer::$f(b $(, $arg)*),
        }
    };
}

pub fn behavior_duration(b: &Behavior) -> (i32, i32) {
    by_behavior!(b, duration())
}
pub fn behavior_shape(b: &Behavior) -> Shape {
    by_behavior!(b, shape())
}
pub fn behavior_requirements(b: &Behavior) -> &'static [(MemoryType, MemoryStatus)] {
    by_behavior!(b, requirements())
}
pub fn behavior_no_timeout(b: &Behavior) -> bool {
    by_behavior!(b, no_timeout())
}
pub fn behavior_check(t: &mut Tick<'_, '_>, b: &mut Behavior) -> bool {
    match b {
        Behavior::Piglin(b) => piglin::check(b, t),
        Behavior::Goat(b) => goat::check(b, t),
        Behavior::Axolotl(b) => axolotl::check(b, t),
        Behavior::Frog(b) => frog::check(b, t),
        Behavior::Armadillo(b) => armadillo::check(b, t),
        Behavior::Sniffer(b) => sniffer::check(b, t),
    }
}
pub fn behavior_can_still_use(t: &mut Tick<'_, '_>, b: &mut Behavior, now: i64) -> bool {
    match b {
        Behavior::Piglin(b) => piglin::can_still_use(b, t, now),
        Behavior::Goat(b) => goat::can_still_use(b, t, now),
        Behavior::Axolotl(b) => axolotl::can_still_use(b, t, now),
        Behavior::Frog(b) => frog::can_still_use(b, t, now),
        Behavior::Armadillo(b) => armadillo::can_still_use(b, t, now),
        Behavior::Sniffer(b) => sniffer::can_still_use(b, t, now),
    }
}
pub fn behavior_start(t: &mut Tick<'_, '_>, b: &mut Behavior, now: i64) {
    match b {
        Behavior::Piglin(b) => piglin::start(b, t, now),
        Behavior::Goat(b) => goat::start(b, t, now),
        Behavior::Axolotl(b) => axolotl::start(b, t, now),
        Behavior::Frog(b) => frog::start(b, t, now),
        Behavior::Armadillo(b) => armadillo::start(b, t, now),
        Behavior::Sniffer(b) => sniffer::start(b, t, now),
    }
}
pub fn behavior_tick(t: &mut Tick<'_, '_>, b: &mut Behavior, now: i64) {
    match b {
        Behavior::Piglin(b) => piglin::tick(b, t, now),
        Behavior::Goat(b) => goat::tick(b, t, now),
        Behavior::Axolotl(b) => axolotl::tick(b, t, now),
        Behavior::Frog(b) => frog::tick(b, t, now),
        Behavior::Armadillo(b) => armadillo::tick(b, t, now),
        Behavior::Sniffer(b) => sniffer::tick(b, t, now),
    }
}
pub fn behavior_stop(t: &mut Tick<'_, '_>, b: &mut Behavior, now: i64) {
    match b {
        Behavior::Piglin(b) => piglin::stop(b, t, now),
        Behavior::Goat(b) => goat::stop(b, t, now),
        Behavior::Axolotl(b) => axolotl::stop(b, t, now),
        Behavior::Frog(b) => frog::stop(b, t, now),
        Behavior::Armadillo(b) => armadillo::stop(b, t, now),
        Behavior::Sniffer(b) => sniffer::stop(b, t, now),
    }
}
pub fn behavior_trigger(t: &mut Tick<'_, '_>, b: &mut Behavior, now: i64) -> bool {
    match b {
        Behavior::Piglin(b) => piglin::trigger(b, t, now),
        Behavior::Goat(b) => goat::trigger(b, t, now),
        Behavior::Axolotl(b) => axolotl::trigger(b, t, now),
        Behavior::Frog(b) => frog::trigger(b, t, now),
        Behavior::Armadillo(b) => armadillo::trigger(b, t, now),
        Behavior::Sniffer(b) => sniffer::trigger(b, t, now),
    }
}

#[cfg(test)]
mod tests;
