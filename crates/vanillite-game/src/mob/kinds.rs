//! What differs between the mob kinds: goals, sounds, experience, spawn setup and the kind
//! specific parts of their tick (creeper fuse, spider climbing, sheep grazing, enderman
//! teleports, animal ageing).

use vanillite_base::random::{LegacyRandom, RandomSource};
use vanillite_base::{BlockPos, Vec3};
use vanillite_data::block::{BlockState, ids};
use vanillite_data::entity::{EntityTypeId, ids as entity_ids};
use vanillite_protocol::packets::play::clientbound::SoundSource;
use vanillite_world::entity::{Dimensions, EntityFlags, EntityId};

use super::goals::{Goal, MeleeKind};
use super::path::{MalusTable, PathType};
use super::physics::{self, Blocks};
use super::{
    Environment, ExplosionRequest, MobContext, MobData, Other, Tick, WorldAction, atan2_deg,
    combat, light_magic_value, nav,
};
use crate::entities::Entities;
use crate::level::Level;
use crate::survival::damage::DamageSource;
use crate::survival::player::Env;
use crate::survival::rules::{Difficulty, Rule};

/// The mob kinds with behaviour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Zombie,
    Skeleton,
    Creeper,
    Spider,
    Enderman,
    Cow,
    Pig,
    Sheep,
    Chicken,
}

/// Farm animal climate of a biome (`spawns_warm/cold_variant_farm_animals`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FarmClimate {
    #[default]
    Temperate,
    Warm,
    Cold,
}

impl Kind {
    /// All kinds.
    pub const ALL: [Kind; 9] = [
        Kind::Zombie,
        Kind::Skeleton,
        Kind::Creeper,
        Kind::Spider,
        Kind::Enderman,
        Kind::Cow,
        Kind::Pig,
        Kind::Sheep,
        Kind::Chicken,
    ];

    /// The kind of an entity type, `None` for types without mob behaviour yet.
    pub fn of(t: EntityTypeId) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.type_id() == t)
    }

    /// The entity type.
    pub fn type_id(self) -> EntityTypeId {
        match self {
            Kind::Zombie => entity_ids::ZOMBIE,
            Kind::Skeleton => entity_ids::SKELETON,
            Kind::Creeper => entity_ids::CREEPER,
            Kind::Spider => entity_ids::SPIDER,
            Kind::Enderman => entity_ids::ENDERMAN,
            Kind::Cow => entity_ids::COW,
            Kind::Pig => entity_ids::PIG,
            Kind::Sheep => entity_ids::SHEEP,
            Kind::Chicken => entity_ids::CHICKEN,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Kind::Zombie => "zombie",
            Kind::Skeleton => "skeleton",
            Kind::Creeper => "creeper",
            Kind::Spider => "spider",
            Kind::Enderman => "enderman",
            Kind::Cow => "cow",
            Kind::Pig => "pig",
            Kind::Sheep => "sheep",
            Kind::Chicken => "chicken",
        }
    }

    /// `entity.minecraft.<name>`.
    pub fn translation_key(self) -> &'static str {
        match self {
            Kind::Zombie => "entity.minecraft.zombie",
            Kind::Skeleton => "entity.minecraft.skeleton",
            Kind::Creeper => "entity.minecraft.creeper",
            Kind::Spider => "entity.minecraft.spider",
            Kind::Enderman => "entity.minecraft.enderman",
            Kind::Cow => "entity.minecraft.cow",
            Kind::Pig => "entity.minecraft.pig",
            Kind::Sheep => "entity.minecraft.sheep",
            Kind::Chicken => "entity.minecraft.chicken",
        }
    }

    /// A monster (`Monster`), else an animal.
    pub fn is_monster(self) -> bool {
        matches!(
            self,
            Kind::Zombie | Kind::Skeleton | Kind::Creeper | Kind::Spider | Kind::Enderman
        )
    }

    /// `getAmbientSoundInterval`.
    pub fn ambient_sound_interval(self) -> i32 {
        if self.is_monster() { 80 } else { 120 }
    }

    /// `#burn_in_daylight`.
    pub fn burns_in_daylight(self) -> bool {
        matches!(self, Kind::Zombie | Kind::Skeleton)
    }

    /// `#can_breathe_under_water` (the undead).
    pub fn breathes_underwater(self) -> bool {
        matches!(self, Kind::Zombie | Kind::Skeleton)
    }

    /// `isSensitiveToWater`.
    pub fn sensitive_to_water(self) -> bool {
        self == Kind::Enderman
    }

    /// `#fall_damage_immune` and `Chicken.causeFallDamage`.
    pub fn fall_damage_immune(self) -> bool {
        self == Kind::Chicken
    }

    /// `EntityType.isAllowedInPeaceful`.
    pub fn allowed_in_peaceful(self) -> bool {
        !self.is_monster()
    }

    /// `removeWhenFarAway`: animals stay.
    pub fn remove_when_far_away(self) -> bool {
        self.is_monster()
    }

    /// `setPathfindingMalus` of the kind.
    pub fn malus(self) -> &'static MalusTable {
        const ANIMAL: &MalusTable = &[(PathType::FireInNeighbor, 16.0), (PathType::Fire, -1.0)];
        const CHICKEN: &MalusTable = &[
            (PathType::FireInNeighbor, 16.0),
            (PathType::Fire, -1.0),
            (PathType::Water, 0.0),
        ];
        const ENDERMAN: &MalusTable = &[(PathType::Water, -1.0)];
        match self {
            Kind::Cow | Kind::Pig | Kind::Sheep => ANIMAL,
            Kind::Chicken => CHICKEN,
            Kind::Enderman => ENDERMAN,
            _ => &[],
        }
    }

    /// `getSoundSource`.
    pub fn sound_source(self) -> SoundSource {
        if self.is_monster() {
            SoundSource::Hostile
        } else {
            SoundSource::Neutral
        }
    }

    /// `getSoundVolume`.
    pub fn sound_volume(self) -> f32 {
        if self == Kind::Cow { 0.4 } else { 1.0 }
    }

    /// `getAmbientSound`, `None` for silent kinds.
    pub fn ambient_sound(self) -> Option<&'static str> {
        Some(match self {
            Kind::Zombie => "minecraft:entity.zombie.ambient",
            Kind::Skeleton => "minecraft:entity.skeleton.ambient",
            Kind::Creeper => return None,
            Kind::Spider => "minecraft:entity.spider.ambient",
            Kind::Enderman => "minecraft:entity.enderman.ambient",
            Kind::Cow => "minecraft:entity.cow.ambient",
            Kind::Pig => "minecraft:entity.pig.ambient",
            Kind::Sheep => "minecraft:entity.sheep.ambient",
            Kind::Chicken => "minecraft:entity.chicken.ambient",
        })
    }

    /// `getHurtSound`.
    pub fn hurt_sound(self) -> &'static str {
        match self {
            Kind::Zombie => "minecraft:entity.zombie.hurt",
            Kind::Skeleton => "minecraft:entity.skeleton.hurt",
            Kind::Creeper => "minecraft:entity.creeper.hurt",
            Kind::Spider => "minecraft:entity.spider.hurt",
            Kind::Enderman => "minecraft:entity.enderman.hurt",
            Kind::Cow => "minecraft:entity.cow.hurt",
            Kind::Pig => "minecraft:entity.pig.hurt",
            Kind::Sheep => "minecraft:entity.sheep.hurt",
            Kind::Chicken => "minecraft:entity.chicken.hurt",
        }
    }

    /// `getDeathSound`.
    pub fn death_sound(self) -> &'static str {
        match self {
            Kind::Zombie => "minecraft:entity.zombie.death",
            Kind::Skeleton => "minecraft:entity.skeleton.death",
            Kind::Creeper => "minecraft:entity.creeper.death",
            Kind::Spider => "minecraft:entity.spider.death",
            Kind::Enderman => "minecraft:entity.enderman.death",
            Kind::Cow => "minecraft:entity.cow.death",
            Kind::Pig => "minecraft:entity.pig.death",
            Kind::Sheep => "minecraft:entity.sheep.death",
            Kind::Chicken => "minecraft:entity.chicken.death",
        }
    }

    /// `getFallSounds().small()`.
    pub fn fall_sound_small(self) -> &'static str {
        if self.is_monster() {
            "minecraft:entity.hostile.small_fall"
        } else {
            "minecraft:entity.generic.small_fall"
        }
    }

    /// `getFallSounds().big()`.
    pub fn fall_sound_big(self) -> &'static str {
        if self.is_monster() {
            "minecraft:entity.hostile.big_fall"
        } else {
            "minecraft:entity.generic.big_fall"
        }
    }

    /// `getBaseExperienceReward`: 5 for monsters (skeletons with their bow 1–3 more), 1–3 for
    /// animals.
    pub fn base_experience_reward(self, random: &mut LegacyRandom) -> i32 {
        match self {
            Kind::Skeleton => 5 + 1 + random.next_int_bounded(3),
            _ if self.is_monster() => 5,
            _ => 1 + random.next_int_bounded(3),
        }
    }

    /// `Kind` by its id (`minecraft:zombie`).
    pub fn by_name(name: &str) -> Option<Kind> {
        let name = name.strip_prefix("minecraft:").unwrap_or(name);
        Kind::ALL.into_iter().find(|k| k.name() == name)
    }
}

/// The dimensions of a mob type (babies at half size).
pub fn dimensions(kind: EntityTypeId, baby: bool) -> Dimensions {
    let d = Dimensions::of(kind);
    if baby {
        Dimensions {
            width: d.width * 0.5,
            height: d.height * 0.5,
        }
    } else {
        d
    }
}

/// `registerGoals` of each kind.
pub fn register_goals(m: &mut MobData) {
    let g = &mut m.goals;
    let tg = &mut m.targets;
    match m.kind {
        Kind::Zombie => {
            g.add(8, Goal::look_at_player(8.0));
            g.add(8, Goal::random_look());
            g.add(3, Goal::melee(MeleeKind::Zombie, 1.0, false));
            g.add(7, Goal::stroll(1.0));
            tg.add(1, Goal::hurt_by(true));
            tg.add(2, Goal::nearest_player(true));
        }
        Kind::Skeleton => {
            g.add(2, Goal::RestrictSun);
            g.add(
                3,
                Goal::FleeSun {
                    speed: 1.0,
                    wanted: Vec3::ZERO,
                },
            );
            g.add(5, Goal::stroll(1.0));
            g.add(6, Goal::look_at_player(8.0));
            g.add(6, Goal::random_look());
            tg.add(1, Goal::hurt_by(false));
            tg.add(2, Goal::nearest_player(true));
            // `reassessWeaponGoal`: skeletons hold a bow.
            g.add(4, Goal::bow(1.0, 40, 15.0));
        }
        Kind::Creeper => {
            g.add(1, Goal::Float);
            g.add(2, Goal::Swell);
            g.add(4, Goal::melee(MeleeKind::Plain, 1.0, false));
            g.add(5, Goal::stroll(0.8));
            g.add(6, Goal::look_at_player(8.0));
            g.add(6, Goal::random_look());
            tg.add(1, Goal::nearest_player(true));
            tg.add(2, Goal::hurt_by(false));
            m.nav.can_float = true;
        }
        Kind::Spider => {
            g.add(1, Goal::Float);
            g.add(
                3,
                Goal::Leap {
                    yd: 0.4,
                    target: None,
                },
            );
            g.add(4, Goal::melee(MeleeKind::Spider, 1.0, true));
            g.add(5, Goal::stroll(0.8));
            g.add(6, Goal::look_at_player(8.0));
            g.add(6, Goal::random_look());
            tg.add(1, Goal::hurt_by(false));
            tg.add(2, Goal::spider_target());
            m.nav.can_float = true;
        }
        Kind::Enderman => {
            g.add(0, Goal::Float);
            g.add(1, Goal::EndermanFreeze { target: None });
            g.add(2, Goal::melee(MeleeKind::Plain, 1.0, false));
            g.add(7, Goal::stroll_with(1.0, 0.0));
            g.add(8, Goal::look_at_player(8.0));
            g.add(8, Goal::random_look());
            tg.add(
                1,
                Goal::EndermanStare {
                    state: super::goals::TargetState {
                        must_see: false,
                        unseen_ticks: 0,
                        unseen_memory: 60,
                        target_mob: None,
                    },
                    pending: None,
                    aggro_time: 0,
                    teleport_time: 0,
                },
            );
            tg.add(2, Goal::hurt_by(false));
            m.nav.can_float = true;
        }
        Kind::Cow | Kind::Pig | Kind::Sheep | Kind::Chicken => {
            let (panic, follow) = match m.kind {
                Kind::Cow => (2.0, 1.25),
                Kind::Pig => (1.25, 1.1),
                Kind::Sheep => (1.25, 1.1),
                _ => (1.4, 1.1),
            };
            g.add(0, Goal::Float);
            g.add(1, Goal::panic(panic));
            let follow_prio = if m.kind == Kind::Pig { 5 } else { 4 };
            g.add(
                follow_prio,
                Goal::FollowParent {
                    speed: follow,
                    parent: None,
                    recalc: 0,
                },
            );
            if m.kind == Kind::Sheep {
                g.add(5, Goal::EatBlock { tick: 0 });
            }
            let stroll_prio = match m.kind {
                Kind::Pig | Kind::Sheep => 6,
                _ => 5,
            };
            g.add(stroll_prio, Goal::stroll(1.0));
            g.add(stroll_prio + 1, Goal::look_at_player(6.0));
            g.add(stroll_prio + 2, Goal::random_look());
            m.nav.can_float = true;
        }
    }
    // Goals are kept sorted by insertion like Vanilla's linked set; nothing to do here.
}

/// `finalizeSpawn`: the random spawn bonus, babies, colours and variants.
pub fn finalize_spawn(
    m: &mut MobData,
    difficulty: Difficulty,
    climate: FarmClimate,
    group_index: u32,
) {
    // `Mob.finalizeSpawn`: follow range bonus and left-handedness.
    let bonus = m.random.triangle(0.0, 0.114_850_000_000_000_01);
    m.attrs.follow_range *= 1.0 + bonus;
    let _left_handed = m.random.next_float() < 0.05;
    match m.kind {
        Kind::Zombie => {
            let _pick_up = m.random.next_float() < 0.55 * special_multiplier(difficulty);
            if m.random.next_float() < 0.05 {
                m.baby = true;
            }
            m.attrs.knockback_resistance += m.random.next_double() * f64::from(0.05_f32);
            let range_bonus = m.random.next_double() * 0.2 * 1.0;
            if range_bonus > 1.0 {
                m.attrs.follow_range *= 1.0 + range_bonus;
            }
        }
        Kind::Skeleton => {
            let _pick_up = m.random.next_float() < 0.55 * special_multiplier(difficulty);
            if difficulty == Difficulty::Hard {
                for slot in &mut m.goals.slots {
                    if let Goal::Bow { interval_min, .. } = &mut slot.goal {
                        *interval_min = 20;
                    }
                }
            }
        }
        Kind::Cow | Kind::Pig | Kind::Sheep | Kind::Chicken => {
            if group_index > 0 && m.random.next_float() <= 0.05 {
                m.age = -24000;
                m.baby = true;
            }
            m.variant = match climate {
                FarmClimate::Cold => 0,
                FarmClimate::Temperate => 1,
                FarmClimate::Warm => 2,
            };
            if m.kind == Kind::Sheep {
                m.sheep_color = sheep_color(climate, &mut m.random);
            }
        }
        _ => {}
    }
}

fn special_multiplier(difficulty: Difficulty) -> f32 {
    // `DifficultyInstance.getSpecialMultiplier` with the clamped effective difficulty of a
    // fresh world.
    match difficulty {
        Difficulty::Hard => 0.25,
        _ => 0.0,
    }
}

/// `SheepColorSpawnRules.getSheepColor`.
fn sheep_color(climate: FarmClimate, random: &mut LegacyRandom) -> u8 {
    const WHITE: u8 = 0;
    const LIGHT_GRAY: u8 = 8;
    const GRAY: u8 = 7;
    const BLACK: u8 = 15;
    const BROWN: u8 = 12;
    const PINK: u8 = 6;
    let (singles, common): ([(u8, i32); 4], u8) = match climate {
        FarmClimate::Temperate => ([(BLACK, 5), (GRAY, 5), (LIGHT_GRAY, 5), (BROWN, 3)], WHITE),
        FarmClimate::Warm => ([(GRAY, 5), (LIGHT_GRAY, 5), (WHITE, 5), (BLACK, 3)], BROWN),
        FarmClimate::Cold => ([(LIGHT_GRAY, 5), (GRAY, 5), (WHITE, 5), (BROWN, 3)], BLACK),
    };
    let mut roll = random.next_int_bounded(100);
    for (color, weight) in singles {
        if roll < weight {
            return color;
        }
        roll -= weight;
    }
    if random.next_int_bounded(500) < 499 {
        common
    } else {
        PINK
    }
}

/// `getWalkTargetValue(pos)` of a kind.
pub fn walk_target_value(kind: Kind, level: &Level, env: &Environment, pos: BlockPos) -> f64 {
    let cost = f64::from(light_magic_value(level, env, pos) - 0.5);
    if kind.is_monster() {
        -cost
    } else if level.block(BlockPos::new(pos.x, pos.y - 1, pos.z)).block() == ids::GRASS_BLOCK {
        10.0
    } else {
        cost
    }
}

/// Plays a sound of the ticking mob (`playSound(sound, volume, pitch)`).
pub fn play_sound(t: &mut Tick<'_, '_>, sound: &str, volume: f32, pitch: f32) {
    if t.mob_ref().silent {
        return;
    }
    let pos = t.pos();
    let source = t.mob_ref().kind.sound_source();
    let seed = t.mob().random.next_long();
    crate::survival::play_sound(t.env.out, None, pos, sound, source, volume, pitch, seed);
}

/// `playAmbientSound`.
pub fn play_ambient_sound(t: &mut Tick<'_, '_>) {
    let kind = t.mob_ref().kind;
    let sound = if kind == Kind::Enderman && t.mob_ref().creepy {
        Some("minecraft:entity.enderman.scream")
    } else {
        kind.ambient_sound()
    };
    if let Some(sound) = sound {
        let id = t.id;
        combat::mob_sound(t.ents, t.env, id, sound);
    }
}

/// Kind specific work before `LivingEntity.tick` (`Creeper.tick`'s fuse, using a bow).
pub fn before_tick(t: &mut Tick<'_, '_>) {
    match t.mob_ref().kind {
        Kind::Creeper if t.mob_ref().is_alive() => {
            let m = t.mob();
            m.old_swell = m.swell;
            if m.ignited {
                m.swell_dir = 1;
            }
            let dir = m.swell_dir;
            if dir > 0 && m.swell == 0 {
                play_sound(t, "minecraft:entity.creeper.primed", 1.0, 0.5);
            }
            let m = t.mob();
            m.swell = (m.swell + dir).max(0);
            if m.swell >= m.max_swell {
                m.swell = m.max_swell;
                explode_creeper(t);
            }
        }
        Kind::Skeleton => {
            let m = t.mob();
            if m.using_item >= 0 {
                m.using_item += 1;
            }
        }
        _ => {}
    }
}

/// `Creeper.explodeCreeper`.
fn explode_creeper(t: &mut Tick<'_, '_>) {
    let m = t.mob();
    let multiplier = if m.powered { 2.0 } else { 1.0 };
    #[allow(clippy::cast_precision_loss)]
    let radius = m.explosion_radius as f32 * multiplier;
    m.dead = true;
    m.removed = true;
    let pos = t.pos();
    let id = t.id;
    t.cx.actions.push(WorldAction::Explode(ExplosionRequest {
        source: Some(id),
        pos,
        radius,
        fire: false,
        mob: true,
    }));
}

/// Kind specific work after `LivingEntity.tick` (`Spider.tick`'s climbing flag).
pub fn after_tick(t: &mut Tick<'_, '_>) {
    if t.mob_ref().kind == Kind::Spider {
        let climbing = t.mob_ref().horizontal_collision;
        if climbing != t.mob_ref().climbing {
            t.mob().climbing = climbing;
            t.core_mut().mark_data_dirty();
        }
    }
}

/// Kind specific parts of `aiStep` (chicken flapping, animal ageing).
pub fn ai_step(t: &mut Tick<'_, '_>) {
    let kind = t.mob_ref().kind;
    if kind == Kind::Chicken {
        let v = t.velocity();
        if !t.on_ground() && v.y < 0.0 {
            t.set_velocity(v.multiply(1.0, 0.6, 1.0));
        }
    }
    if !kind.is_monster() && t.mob_ref().is_alive() {
        // `AgeableMob.aiStep` and `Animal.aiStep`.
        let m = t.mob();
        if m.age < 0 {
            m.age += 1;
        } else if m.age > 0 {
            m.age -= 1;
        }
        if m.age != 0 {
            m.in_love = 0;
        }
        if m.in_love > 0 {
            m.in_love -= 1;
        }
        let baby = m.age < 0;
        if baby != m.baby {
            m.baby = baby;
            let kind_type = t.core().kind();
            t.core_mut().set_dims(dimensions(kind_type, baby));
            t.core_mut().mark_data_dirty();
        }
    }
}

/// `customServerAiStep` (enderman teleports in daylight).
pub fn custom_server_ai_step(t: &mut Tick<'_, '_>) {
    if t.mob_ref().kind != Kind::Enderman {
        return;
    }
    if t.cx.env.is_bright_outside()
        && t.mob_ref().tick_count >= t.mob_ref().target_change_time + 600
    {
        let br = t.light_magic_value();
        let sky = super::can_see_sky(t.level, t.block_pos());
        let roll = t.mob().random.next_float() * 30.0;
        if br > 0.5 && sky && roll < (br - 0.4) * 2.0 {
            t.mob().target = None;
            let id = t.id;
            teleport_randomly(t.level, t.ents, t.env, id);
        }
    }
}

/// `Creeper.causeFallDamage`: a fall shortens the fuse.
pub fn on_fall_damage(t: &mut Tick<'_, '_>, distance: f64) {
    if t.mob_ref().kind == Kind::Creeper {
        let m = t.mob();
        #[allow(clippy::cast_possible_truncation)]
        let add = (distance * 1.5) as i32;
        m.swell += add;
        if m.swell > m.max_swell - 5 {
            m.swell = m.max_swell - 5;
        }
    }
}

/// `Zombie.doHurtTarget`: a burning zombie sets its target on fire.
pub fn after_attack(t: &mut Tick<'_, '_>, target: EntityId) {
    if t.mob_ref().kind != Kind::Zombie || !t.mob_ref().is_on_fire() {
        return;
    }
    #[allow(clippy::cast_precision_loss)]
    let difficulty = t.env.difficulty as i32 as f32;
    if t.mob().random.next_float() < difficulty * 0.3 {
        #[allow(clippy::cast_possible_truncation)]
        let ticks = 2 * difficulty as i32 * 20;
        if let Some(p) = t.ents.player_mut(target) {
            p.survival.fire_ticks = p.survival.fire_ticks.max(ticks);
        } else if let Some(m) = t.ents.mob_mut(target) {
            m.fire_ticks = m.fire_ticks.max(ticks);
        }
    }
}

/// `#edible_for_sheep`.
pub fn edible_for_sheep(state: BlockState) -> bool {
    matches!(
        state.block().name(),
        "minecraft:short_grass"
            | "minecraft:short_dry_grass"
            | "minecraft:tall_dry_grass"
            | "minecraft:fern"
    )
}

/// `EatBlockGoal.tick` at the bite: grass becomes dirt, tall grass disappears.
pub fn eat_block(t: &mut Tick<'_, '_>) {
    let pos = t.block_pos();
    let griefing = t.env.rules.bool(Rule::MOB_GRIEFING);
    if edible_for_sheep(t.level.block(pos)) {
        if griefing {
            t.cx.actions.push(WorldAction::DestroyBlock(pos, false));
        }
    } else {
        let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
        if t.level.block(below).block() != ids::GRASS_BLOCK {
            return;
        }
        if griefing {
            let grass = i32::from(ids::GRASS_BLOCK.default_state().0);
            t.cx.actions
                .push(WorldAction::LevelEvent(2001, below, grass));
            t.cx.actions
                .push(WorldAction::SetBlock(below, ids::DIRT.default_state(), 2));
        }
    }
    // `Sheep.ate`: the wool grows back, babies grow up.
    let m = t.mob();
    m.sheared = false;
    if m.baby {
        m.age = (m.age + 60 * 20).min(0);
    }
    t.core_mut().mark_data_dirty();
}

/// `Mob.lookAt(entity, yMax, xMax)`: turns the body.
pub fn look_at_entity(t: &mut Tick<'_, '_>, o: Other, y_max: f32, x_max: f32) {
    let p = t.pos();
    let xd = o.pos.x - p.x;
    let zd = o.pos.z - p.z;
    let yd = o.eye_y - t.eye_y();
    let sd = (xd * xd + zd * zd).sqrt();
    #[allow(clippy::cast_possible_truncation)]
    let y_rot_d = atan2_deg(zd, xd) as f32 - 90.0;
    #[allow(clippy::cast_possible_truncation)]
    let x_rot_d = -(atan2_deg(yd, sd) as f32);
    let mut rot = t.core().rot();
    let lerp =
        |a: f32, b: f32, max: f32| a + vanillite_base::math::wrap_degrees_f(b - a).clamp(-max, max);
    rot.x_rot = lerp(rot.x_rot, x_rot_d, x_max);
    rot.y_rot = lerp(rot.y_rot, y_rot_d, y_max);
    t.core_mut().set_rot(rot);
}

/// `Creeper.setSwellDir`.
pub fn set_swell_dir(t: &mut Tick<'_, '_>, dir: i32) {
    if t.mob_ref().swell_dir != dir {
        t.mob().swell_dir = dir;
        t.core_mut().mark_data_dirty();
    }
}

/// `EnderMan.setBeingStaredAt` / the creepy flag (open mouth).
pub fn set_creepy(t: &mut Tick<'_, '_>, on: bool) {
    if t.mob_ref().creepy != on {
        t.mob().creepy = on;
        t.core_mut().mark_data_dirty();
    }
}

/// `EnderMan.isBeingStaredBy`: the player looks at the enderman's eyes.
pub fn is_stared_at_by(t: &Tick<'_, '_>, player: EntityId) -> bool {
    let Some(e) = t.ents.store.get(player) else {
        return false;
    };
    let rot = e.rot();
    let eye = e.pos().y + f64::from(e.kind().info().eye_height);
    // `getViewVector`.
    let pitch = f64::from(rot.x_rot).to_radians();
    let yaw = f64::from(-rot.y_rot).to_radians();
    let (yc, ys) = (yaw.cos(), yaw.sin());
    let (pc, ps) = (pitch.cos(), pitch.sin());
    let view = Vec3::new(ys * pc, -ps, yc * pc).normalize();
    let p = t.pos();
    let to = Vec3::new(p.x - e.pos().x, t.eye_y() - eye, p.z - e.pos().z);
    let dist = to.length_sqr().sqrt();
    if dist == 0.0 {
        return false;
    }
    let dot = view.dot(to.normalize());
    if dot <= 1.0 - 0.025 / dist {
        return false;
    }
    physics::clip(
        t.level as &dyn Blocks,
        Vec3::new(e.pos().x, eye, e.pos().z),
        Vec3::new(p.x, t.eye_y(), p.z),
    )
    .is_none()
}

/// `EnderMan.teleport()`: a random spot within 32 blocks.
pub fn teleport_randomly(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    id: EntityId,
) -> bool {
    let Some(pos) = ents.store.get(id).map(|e| e.pos()) else {
        return false;
    };
    let Some(m) = ents.mob_mut(id) else {
        return false;
    };
    let x = pos.x + (m.random.next_double() - 0.5) * 64.0;
    let y = pos.y + f64::from(m.random.next_int_bounded(64) - 32);
    let z = pos.z + (m.random.next_double() - 0.5) * 64.0;
    teleport_to(level, ents, env, id, Vec3::new(x, y, z))
}

/// `EnderMan.teleport(x, y, z)` and `randomTeleport`.
fn teleport_to(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    id: EntityId,
    to: Vec3,
) -> bool {
    let mut pos = BlockPos::containing(to.x, to.y, to.z);
    while pos.y > level.min_y()
        && !blocks_motion(level.block(BlockPos::new(pos.x, pos.y - 1, pos.z)))
    {
        pos = BlockPos::new(pos.x, pos.y - 1, pos.z);
    }
    let below = level.block(BlockPos::new(pos.x, pos.y - 1, pos.z));
    if !blocks_motion(below) || super::is_water(level.block(pos)) || !level.is_loaded(pos) {
        return false;
    }
    let Some(e) = ents.store.get(id) else {
        return false;
    };
    let old = e.pos();
    let dims = e.dims();
    let dest = Vec3::new(to.x, f64::from(pos.y), to.z);
    let aabb = dims.bounding_box(dest);
    let mut scratch = Vec::new();
    if !physics::no_collision(level, &aabb, &mut scratch)
        || physics::contains_any_liquid(level, &aabb)
    {
        return false;
    }
    if let Some(mut e) = ents.store.get_mut(id) {
        e.set_pos(dest);
        e.set_velocity(Vec3::ZERO);
        e.set_flags(EntityFlags::HAS_IMPULSE, true);
    }
    if let Some(m) = ents.mob_mut(id) {
        m.nav.stop();
        m.target_change_time = m.tick_count;
    }
    let seed = env.random.next_long();
    crate::survival::play_sound(
        env.out,
        None,
        old,
        "minecraft:entity.enderman.teleport",
        SoundSource::Hostile,
        1.0,
        1.0,
        seed,
    );
    true
}

/// `BlockState.blocksMotion`: solid and not cobweb or bamboo sapling.
fn blocks_motion(state: BlockState) -> bool {
    state.block() != ids::COBWEB && state.block() != ids::BAMBOO_SAPLING && nav::is_solid(state)
}

/// `EnderMan.hurtServer` for projectiles: up to 64 tries to teleport away.
pub fn enderman_dodge(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    _cx: &mut MobContext,
    id: EntityId,
) {
    for _ in 0..64 {
        if teleport_randomly(level, ents, env, id) {
            return;
        }
    }
}

/// `EnderMan.hurtServer`: other damage than from a living entity makes it teleport (9 in
/// 10).
pub fn after_hurt(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    id: EntityId,
    source: DamageSource,
) {
    let Some(m) = ents.mob_mut(id) else {
        return;
    };
    if m.kind != Kind::Enderman || !m.is_alive() {
        return;
    }
    let living_cause = source
        .cause
        .is_some_and(|c| ents.mob(c).is_some() || ents.player(c).is_some());
    let Some(m) = ents.mob_mut(id) else {
        return;
    };
    if !living_cause && m.random.next_int_bounded(10) != 0 {
        teleport_randomly(level, ents, env, id);
    }
}

/// Whether the ticking mob has its living flag "using item" (skeletons drawing their bow).
pub fn living_flags(m: &MobData) -> u8 {
    u8::from(m.using_item >= 0)
}

/// One loot entry: an item with a uniform count, smelted when the mob burns.
struct Drop {
    item: &'static str,
    min: i32,
    max: i32,
    smelted: Option<&'static str>,
    player_kill: bool,
}

const fn drop(item: &'static str, min: i32, max: i32) -> Drop {
    Drop {
        item,
        min,
        max,
        smelted: None,
        player_kill: false,
    }
}

const fn meat(item: &'static str, min: i32, max: i32, smelted: &'static str) -> Drop {
    Drop {
        item,
        min,
        max,
        smelted: Some(smelted),
        player_kill: false,
    }
}

/// The items of `entities/<kind>` of the 26.3 loot tables without looting: one pool per
/// entry. `killed_by_player`, `on_fire` and the sheep's wool decide the conditional parts.
pub fn death_loot(m: &mut MobData, killed_by_player: bool) -> Vec<crate::item::ItemStack> {
    const SHEEP_WOOL: [&str; 16] = [
        "minecraft:white_wool",
        "minecraft:orange_wool",
        "minecraft:magenta_wool",
        "minecraft:light_blue_wool",
        "minecraft:yellow_wool",
        "minecraft:lime_wool",
        "minecraft:pink_wool",
        "minecraft:gray_wool",
        "minecraft:light_gray_wool",
        "minecraft:cyan_wool",
        "minecraft:purple_wool",
        "minecraft:blue_wool",
        "minecraft:brown_wool",
        "minecraft:green_wool",
        "minecraft:red_wool",
        "minecraft:black_wool",
    ];
    let pools: &[Drop] = match m.kind {
        Kind::Zombie => &[drop("minecraft:rotten_flesh", 0, 2)],
        Kind::Skeleton => &[drop("minecraft:arrow", 0, 2), drop("minecraft:bone", 0, 2)],
        Kind::Creeper => &[drop("minecraft:gunpowder", 0, 2)],
        Kind::Spider => &[
            drop("minecraft:string", 0, 2),
            Drop {
                item: "minecraft:spider_eye",
                min: -1,
                max: 1,
                smelted: None,
                player_kill: true,
            },
        ],
        Kind::Enderman => &[drop("minecraft:ender_pearl", 0, 1)],
        Kind::Cow => &[
            drop("minecraft:leather", 0, 2),
            meat("minecraft:beef", 1, 3, "minecraft:cooked_beef"),
        ],
        Kind::Pig => &[meat(
            "minecraft:porkchop",
            1,
            3,
            "minecraft:cooked_porkchop",
        )],
        Kind::Chicken => &[
            drop("minecraft:feather", 0, 2),
            meat("minecraft:chicken", 1, 1, "minecraft:cooked_chicken"),
        ],
        Kind::Sheep => &[meat("minecraft:mutton", 1, 2, "minecraft:cooked_mutton")],
    };
    let on_fire = m.is_on_fire();
    let mut out = Vec::new();
    let push = |name: &str, count: i32, out: &mut Vec<crate::item::ItemStack>| {
        if count > 0
            && let Some(id) = vanillite_data::registry::ITEM.id(name)
        {
            out.push(crate::item::ItemStack::new(id, count));
        }
    };
    for d in pools {
        if d.player_kill && !killed_by_player {
            continue;
        }
        let count = m.random.next_int_between_inclusive(d.min, d.max);
        let name = match d.smelted {
            Some(cooked) if on_fire => cooked,
            _ => d.item,
        };
        push(name, count, &mut out);
    }
    match m.kind {
        Kind::Zombie => {
            // The rare drop: `killed_by_player` and a 2.5 % chance.
            if killed_by_player && m.random.next_float() < 0.025 {
                let rare = [
                    "minecraft:iron_ingot",
                    "minecraft:carrot",
                    "minecraft:potato",
                ];
                let i = usize::try_from(m.random.next_int_bounded(3)).unwrap_or(0);
                push(rare[i], 1, &mut out);
            }
        }
        Kind::Sheep if !m.sheared => {
            push(SHEEP_WOOL[usize::from(m.sheep_color & 15)], 1, &mut out);
        }
        _ => {}
    }
    out
}
