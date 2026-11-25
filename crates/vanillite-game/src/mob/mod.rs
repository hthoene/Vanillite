//! Mobs and animals (`Mob`, `PathfinderMob`, `Monster`, `Animal`): their kind data, the
//! living entity tick in Vanilla's order (`Entity.baseTick`, `LivingEntity.aiStep`,
//! `Mob.serverAiStep`), movement physics, path finding, goals, combat and natural spawning.
//!
//! A mob tick works on the world's [`Entities`] taken out of the [`Level`] (the level stays
//! readable for blocks and light); what changes blocks (explosions, a sheep eating grass) is
//! queued as a [`WorldAction`] and applied by the world right after that mob's tick.

pub mod animal;
pub mod attack;
pub mod boss;
pub mod brain;
pub mod combat;
pub mod common;
pub mod equipment;
pub mod fam;
pub mod goals;
pub mod kinds;
pub mod living;
pub mod missile;
pub mod movement;
pub mod nav;
pub mod nbt;
pub mod path;
pub mod physics;
pub mod projectile;
pub mod randompos;
pub mod spawn;
pub mod structures;
pub mod tame;

use vanillite_base::random::{LegacyRandom, RandomSource};
use vanillite_base::{Aabb, BlockPos, ChunkPos, Vec3};
use vanillite_data::block::BlockState;
use vanillite_data::entity::{ATTRIBUTES, EntityTypeId};
use vanillite_world::entity::{
    EntityFlags, EntityId, EntityMut, EntityRef, NetworkIds, NewEntity, Rotation,
};

use crate::entities::Entities;
use crate::level::Level;
use crate::survival::blocks::FluidContact;
use crate::survival::damage::DamageKind;
use crate::survival::player::Env;

pub use fam::Family;
pub use kinds::Kind;
pub use vanillite_world::entity::Dimensions;

/// The attribute values a mob uses (`AttributeMap` with the base values and the spawn
/// modifiers folded in).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Attrs {
    pub max_health: f64,
    pub movement_speed: f64,
    pub flying_speed: f64,
    pub follow_range: f64,
    pub attack_damage: f64,
    pub attack_knockback: f64,
    pub armor: f64,
    pub armor_toughness: f64,
    pub knockback_resistance: f64,
    pub step_height: f64,
    pub jump_strength: f64,
    pub gravity: f64,
    pub safe_fall_distance: f64,
    pub fall_damage_multiplier: f64,
    pub water_movement_efficiency: f64,
    pub movement_efficiency: f64,
    pub friction_modifier: f64,
    pub air_drag_modifier: f64,
    pub explosion_knockback_resistance: f64,
}

impl Attrs {
    /// The default attribute values of an entity type.
    pub fn of(kind: EntityTypeId) -> Attrs {
        let info = kind.info();
        let value = |name: &str| -> f64 {
            let Some(id) = ATTRIBUTES.iter().position(|a| a.name == name) else {
                return 0.0;
            };
            info.attributes
                .iter()
                .find(|(a, _)| usize::from(*a) == id)
                .map_or(ATTRIBUTES[id].default, |&(_, v)| v)
        };
        Attrs {
            max_health: value("minecraft:max_health"),
            movement_speed: value("minecraft:movement_speed"),
            flying_speed: value("minecraft:flying_speed"),
            follow_range: value("minecraft:follow_range"),
            attack_damage: value("minecraft:attack_damage"),
            attack_knockback: value("minecraft:attack_knockback"),
            armor: value("minecraft:armor"),
            armor_toughness: value("minecraft:armor_toughness"),
            knockback_resistance: value("minecraft:knockback_resistance"),
            step_height: value("minecraft:step_height"),
            jump_strength: value("minecraft:jump_strength"),
            gravity: value("minecraft:gravity"),
            safe_fall_distance: value("minecraft:safe_fall_distance"),
            fall_damage_multiplier: value("minecraft:fall_damage_multiplier"),
            water_movement_efficiency: value("minecraft:water_movement_efficiency"),
            movement_efficiency: value("minecraft:movement_efficiency"),
            friction_modifier: value("minecraft:friction_modifier"),
            air_drag_modifier: value("minecraft:air_drag_modifier"),
            explosion_knockback_resistance: value("minecraft:explosion_knockback_resistance"),
        }
    }
}

/// The kind data of a mob (`Dense<MobData>`).
#[derive(Clone, Debug)]
pub struct MobData {
    pub kind: Kind,
    /// The entity's own random (`Entity.random`).
    pub random: LegacyRandom,
    pub attrs: Attrs,
    pub health: f32,
    pub tick_count: i32,
    pub hurt_time: i32,
    /// `invulnerableTime` / `damageCooldownTime`.
    pub damage_cooldown: i32,
    pub last_hurt: f32,
    pub death_time: i32,
    pub dead: bool,
    /// Removed during its own tick (`isRemoved`); the world despawns it after the tick.
    pub removed: bool,
    pub no_action_time: i32,
    pub fall_distance: f64,
    pub fire_ticks: i32,
    pub air: i32,
    pub y_body_rot: f32,
    pub speed: f32,
    pub xxa: f32,
    pub yya: f32,
    pub zza: f32,
    pub jumping: bool,
    pub no_jump_delay: i32,
    pub horizontal_collision: bool,
    pub vertical_collision: bool,
    pub fluids: FluidContact,
    pub stuck_speed: Vec3,
    /// The last damage and the game time it was taken (`lastDamageSource`, `lastDamageStamp`).
    pub last_damage: Option<(DamageKind, i64)>,
    pub last_hurt_by_player: Option<EntityId>,
    pub last_hurt_by_player_time: i32,
    pub last_hurt_by_mob: Option<EntityId>,
    pub last_hurt_by_mob_time: i32,
    pub last_hurt_mob: Option<EntityId>,
    pub persistent: bool,
    pub no_ai: bool,
    pub silent: bool,
    pub ambient_sound_time: i32,
    pub target: Option<EntityId>,
    pub aggressive: bool,
    pub goals: goals::GoalSelector,
    pub targets: goals::GoalSelector,
    pub nav: nav::Navigation,
    pub move_ctl: nav::MoveControl,
    pub look: nav::LookControl,
    pub jump: bool,
    pub body: nav::BodyRotation,
    /// `Sensing`: line of sight results of this tick.
    pub seen: Vec<EntityId>,
    pub unseen: Vec<EntityId>,
    /// `AgeableMob.age`: negative for babies.
    pub age: i32,
    pub in_love: i32,
    pub baby: bool,
    // Kind specific state.
    pub swell: i32,
    pub old_swell: i32,
    pub swell_dir: i32,
    pub max_swell: i32,
    pub explosion_radius: i32,
    pub powered: bool,
    pub ignited: bool,
    /// Ticks the mob draws its bow (`useItemRemaining` counted up), `-1` when not using it.
    pub using_item: i32,
    pub sheep_color: u8,
    pub sheared: bool,
    pub eat_animation: i32,
    pub egg_time: i32,
    pub climbing: bool,
    pub carried: Option<BlockState>,
    pub creepy: bool,
    pub stared_at: bool,
    pub target_change_time: i32,
    /// The path a melee goal computed in `canUse` for its `start`.
    pub goal_path: Option<path::Path>,
    /// Index into the `minecraft:*_variant` registry (pigs, cows, chickens); 0 = default.
    pub variant: i32,
    pub xp_reward: i32,
    /// The last block position (`lastPos`).
    pub last_pos: BlockPos,
    /// `activeEffects` with their attribute modifiers.
    pub effects: crate::effect::ActiveEffects,
    /// `getAbsorptionAmount`.
    pub absorption: f32,
    /// The attribute values without effect modifiers while modifiers are active; `attrs`
    /// holds the modified values then.
    pub base_attrs: Option<Box<Attrs>>,
    /// Wears a saddle (`EquipmentSlot.SADDLE`).
    pub saddled: bool,
    /// `ItemBasedSteering`: boosting, the ticks boosted and the boost length
    /// (`DATA_BOOST_TIME`).
    pub boosting: bool,
    pub boost_time: i32,
    pub boost_total: i32,
    /// `getCustomName`, encoded as the `custom_name` component (network NBT).
    pub custom_name: Option<Box<[u8]>>,
    /// `TamableAnimal`: the owner's UUID, tame, ordered to sit, sitting pose.
    pub owner: Option<u128>,
    pub tame: bool,
    pub order_to_sit: bool,
    pub in_sitting_pose: bool,
    /// `loveCause`: the player who fed it.
    pub love_cause: Option<u128>,
    /// `forcedAge` and `forcedAgeTimer` (babies fed to grow).
    pub forced_age: i32,
    pub forced_age_timer: i32,
    /// A leash read from NBT, attached once its holder is there.
    pub pending_leash: Option<LeashRef>,
    /// The equipment slots and their drop chances.
    pub equipment: equipment::Equipment,
    /// `canPickUpLoot`.
    pub can_pick_up_loot: bool,
    /// `isLeftHanded`.
    pub left_handed: bool,
    /// `PathfinderMob.restrictTo`: the home position and radius (`-1` = none).
    pub restrict_center: BlockPos,
    pub restrict_radius: i32,
    /// The brain of brain AI mobs (`Brain`), `None` for goal AI mobs.
    pub brain: Option<Box<brain::Brain>>,
    /// The state of a family kind.
    pub ext: fam::Ext,
    /// `Zombie.canBreakDoors` (and the other door breakers).
    pub can_break_doors: bool,
    /// `Chicken.isChickenJockey`.
    pub chicken_jockey: bool,
    /// `NeutralMob`: the game time the anger ends (`-1` = none) and whom it is angry at.
    pub anger_end: i64,
    pub anger_target: Option<u128>,
    /// `discardFriction` (long jumps of brain mobs).
    pub discard_friction: bool,
    /// `moveDist` and `nextStep`: when the next step makes a vibration.
    pub move_dist: f32,
    pub next_step: f32,
    /// The state of bosses and the other kinds with their own machinery.
    pub boss: boss::Ext,
}

/// What holds a leash in entity NBT (`leash`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeashRef {
    Entity(u128),
    Knot(BlockPos),
}

impl MobData {
    /// A fresh mob of `kind` (`EntityType.create` without `finalizeSpawn`).
    pub fn new(kind: Kind, seed: i64) -> MobData {
        let attrs = Attrs::of(kind.type_id());
        #[allow(clippy::cast_possible_truncation)]
        let health = attrs.max_health as f32;
        let mut mob = MobData {
            kind,
            random: LegacyRandom::new(seed),
            attrs,
            health,
            tick_count: 0,
            hurt_time: 0,
            damage_cooldown: 0,
            last_hurt: 0.0,
            death_time: 0,
            dead: false,
            removed: false,
            no_action_time: 0,
            fall_distance: 0.0,
            fire_ticks: -1,
            air: 300,
            y_body_rot: 0.0,
            speed: 0.0,
            xxa: 0.0,
            yya: 0.0,
            zza: 0.0,
            jumping: false,
            no_jump_delay: 0,
            horizontal_collision: false,
            vertical_collision: false,
            fluids: FluidContact::default(),
            stuck_speed: Vec3::ZERO,
            last_damage: None,
            last_hurt_by_player: None,
            last_hurt_by_player_time: 0,
            last_hurt_by_mob: None,
            last_hurt_by_mob_time: 0,
            last_hurt_mob: None,
            persistent: false,
            no_ai: false,
            silent: false,
            ambient_sound_time: 0,
            target: None,
            aggressive: false,
            goals: goals::GoalSelector::default(),
            targets: goals::GoalSelector::default(),
            nav: nav::Navigation::new(attrs.follow_range),
            move_ctl: nav::MoveControl::default(),
            look: nav::LookControl::default(),
            jump: false,
            body: nav::BodyRotation::default(),
            seen: Vec::new(),
            unseen: Vec::new(),
            age: 0,
            in_love: 0,
            baby: false,
            swell: 0,
            old_swell: 0,
            swell_dir: -1,
            max_swell: 30,
            explosion_radius: 3,
            powered: false,
            ignited: false,
            using_item: -1,
            sheep_color: 0,
            sheared: false,
            eat_animation: 0,
            egg_time: 0,
            climbing: false,
            carried: None,
            creepy: false,
            stared_at: false,
            target_change_time: 0,
            goal_path: None,
            variant: 0,
            xp_reward: 0,
            effects: crate::effect::ActiveEffects::default(),
            absorption: 0.0,
            base_attrs: None,
            last_pos: BlockPos::new(i32::MIN, i32::MIN, i32::MIN),
            saddled: false,
            boosting: false,
            boost_time: 0,
            boost_total: 0,
            custom_name: None,
            owner: None,
            tame: false,
            order_to_sit: false,
            in_sitting_pose: false,
            love_cause: None,
            forced_age: 0,
            forced_age_timer: 0,
            pending_leash: None,
            equipment: equipment::Equipment::default(),
            can_pick_up_loot: false,
            left_handed: false,
            restrict_center: BlockPos::new(0, 0, 0),
            restrict_radius: -1,
            brain: None,
            ext: fam::Ext::None,
            can_break_doors: false,
            chicken_jockey: false,
            anger_end: -1,
            anger_target: None,
            discard_friction: false,
            move_dist: 0.0,
            next_step: 1.0,
            boss: boss::new_ext(kind),
        };
        // `Mob.ambientSoundTime` starts at 0; only playing a sound resets it to `-interval`.
        // `LivingEntity`'s constructor turns the entity to a random yaw (`setYRot(random.nextFloat()
        // * 2 PI)`); spawning sets the real rotation afterwards, but the draw stays.
        let _ = mob.random.next_float();
        if kind == Kind::Chicken {
            mob.egg_time = mob.random.next_int_bounded(6000) + 6000;
        }
        kinds::register_goals(&mut mob);
        mob
    }

    /// `getMaxHealth`.
    pub fn max_health(&self) -> f32 {
        #[allow(clippy::cast_possible_truncation)]
        let m = self.attrs.max_health as f32;
        m
    }

    /// `isDeadOrDying`.
    pub fn is_dead_or_dying(&self) -> bool {
        self.health <= 0.0
    }

    /// `hasRestriction`.
    pub fn has_restriction(&self) -> bool {
        self.restrict_radius != -1
    }

    /// `isWithinRestriction(pos)`.
    pub fn is_within_restriction(&self, pos: BlockPos) -> bool {
        if self.restrict_radius == -1 {
            return true;
        }
        let r = f64::from(self.restrict_radius);
        self.restrict_center.dist_sqr(pos) < r * r
    }

    /// `isAlive`.
    pub fn is_alive(&self) -> bool {
        !self.removed && self.health > 0.0
    }

    /// `isOnFire` on the server: burning ticks left.
    pub fn is_on_fire(&self) -> bool {
        self.fire_ticks > 0
    }

    /// `getLastDamageSource`: forgotten 40 ticks after the hit.
    pub fn last_damage_source(&self, game_time: i64) -> Option<DamageKind> {
        self.last_damage
            .filter(|&(_, at)| game_time - at <= 40)
            .map(|(k, _)| k)
    }
}

/// The parts of the time of day and the weather mobs react to, for one world tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Environment {
    /// `Level.getSkyDarken()`.
    pub sky_darken: i32,
    /// `EnvironmentAttributes.MONSTERS_BURN`.
    pub monsters_burn: bool,
    /// `isThundering`.
    pub thundering: bool,
    /// `isRaining`.
    pub raining: bool,
    /// The dimension has sky light.
    pub has_sky_light: bool,
    /// `monster_spawn_block_light_limit` of the dimension type.
    pub monster_block_light_limit: i32,
    /// `monster_spawn_light_level` of the dimension type as an inclusive range.
    pub monster_light_test: (i32, i32),
    /// `DimensionType.ambientLight`.
    pub ambient_light: f32,
    /// `DimensionType.hasCeiling`.
    pub has_ceiling: bool,
    /// `EnvironmentAttributes.SURFACE_SLIME_SPAWN_CHANCE` (the moon's brightness / 2 in the
    /// overworld).
    pub surface_slime_chance: f32,
    /// The world seed (slime chunks).
    pub world_seed: i64,
    /// The days of the overworld clock (`Timelines.OVERWORLD_DAY` period count).
    pub day_count: i64,
    /// `getSeaLevel`.
    pub sea_level: i32,
    /// The time of the overworld day (ticks into the `minecraft:overworld` clock's day): the
    /// universal timelines (the villager schedules) follow it in every dimension.
    pub day_time: i64,
    /// `EnvironmentAttributes.BEES_STAY_IN_HIVE`: the night of the `minecraft:day` timeline or
    /// rain (any rain level).
    pub bees_stay_in_hive: bool,
}

impl Default for Environment {
    fn default() -> Self {
        Environment {
            sky_darken: 0,
            monsters_burn: true,
            thundering: false,
            raining: false,
            has_sky_light: true,
            monster_block_light_limit: 0,
            monster_light_test: (0, 7),
            ambient_light: 0.0,
            has_ceiling: false,
            surface_slime_chance: 0.0,
            day_count: 0,
            world_seed: 0,
            sea_level: 63,
            day_time: 6000,
            bees_stay_in_hive: false,
        }
    }
}

impl Environment {
    /// The overworld at `day` ticks of the day (the `minecraft:day` timeline) with the rain and
    /// thunder levels of the weather: sky darkness, whether monsters burn.
    pub fn overworld(day: i64, rain: f32, thunder: f32) -> Environment {
        let mut sky = 15.0 * day_sky_factor(day);
        sky += (4.0 - sky) * 0.3125 * rain;
        sky += (4.0 - sky) * 0.527_343_75 * thunder;
        #[allow(clippy::cast_possible_truncation)]
        let sky_darken = (15.0 - sky) as i32;
        let night = (12542..23460).contains(&day.rem_euclid(24000));
        Environment {
            sky_darken,
            monsters_burn: !night,
            bees_stay_in_hive: night || rain > 0.0,
            thundering: thunder > 0.9,
            raining: rain > 0.2,
            has_sky_light: true,
            monster_block_light_limit: 0,
            monster_light_test: (0, 7),
            ambient_light: 0.0,
            ..Environment::default()
        }
    }

    /// `Level.isBrightOutside`.
    pub fn is_bright_outside(&self) -> bool {
        self.has_sky_light && self.sky_darken < 4
    }
}

/// The overworld's sky light factor at `ticks` of the day (the `day` timeline's
/// `gameplay/sky_light_level` track).
pub fn day_sky_factor(ticks: i64) -> f32 {
    const KEYS: [(i64, f32); 4] = [
        (133, 1.0),
        (11867, 1.0),
        (13670, 0.266_666_68),
        (22330, 0.266_666_68),
    ];
    let t = ticks.rem_euclid(24000);
    for w in KEYS.windows(2) {
        let (a, b) = (w[0], w[1]);
        if t >= a.0 && t < b.0 {
            #[allow(clippy::cast_precision_loss)]
            let f = (t - a.0) as f32 / (b.0 - a.0) as f32;
            return a.1 + (b.1 - a.1) * f;
        }
    }
    // Wraps from the last key to the first of the next day.
    let (last, first) = (KEYS[3], KEYS[0]);
    let span = first.0 + 24000 - last.0;
    let since = if t >= last.0 {
        t - last.0
    } else {
        t + 24000 - last.0
    };
    #[allow(clippy::cast_precision_loss)]
    let f = since as f32 / span as f32;
    last.1 + (first.1 - last.1) * f
}

/// Performance knobs of the mob tick (`[mobs]` in the configuration).
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct MobSettings {
    /// Path finding nodes all mobs of a world may visit per tick; `0` = unlimited (Vanilla).
    pub path_nodes_per_tick: u32,
    /// Mobs farther than this from every player run their AI only every 20th tick; `0` =
    /// off (Vanilla).
    pub activation_range: u32,
}

/// Explosion parameters (`Level.explode`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExplosionRequest {
    pub source: Option<EntityId>,
    /// The entity responsible (`getIndirectSourceEntity`); `None`: the source itself.
    pub indirect: Option<EntityId>,
    pub pos: Vec3,
    pub radius: f32,
    /// `ExplosionInteraction.MOB`: blocks only break with `mobGriefing`.
    pub mob: bool,
    /// The source's `getBlockExplosionResistance` (the wither's skulls, primed TNT minecarts).
    pub resistance: crate::level::explosion::BlockResistance,
}

/// A wind charge burst (`AbstractWindCharge.explode`): a knockback-only explosion that
/// triggers blocks instead of breaking them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindBurst {
    /// The exploding charge.
    pub source: Option<EntityId>,
    pub pos: Vec3,
    pub radius: f32,
    /// The knockback multiplier of the damage calculator.
    pub knockback: f64,
    /// The burst sound.
    pub sound: &'static str,
}

impl ExplosionRequest {
    /// `Level.explode` with the block interaction of the rules (`ExplosionInteraction.MOB` or
    /// `TNT`).
    pub fn run(&self, level: &mut crate::level::Level, rules: &crate::survival::rules::GameRules) {
        use crate::level::explosion::BlockInteraction;
        use crate::survival::rules::Rule;
        let decay = if self.mob {
            Rule::MOB_EXPLOSION_DROP_DECAY
        } else {
            Rule::TNT_EXPLOSION_DROP_DECAY
        };
        let interaction = if self.mob && !rules.bool(Rule::MOB_GRIEFING) {
            BlockInteraction::Keep
        } else if rules.bool(decay) {
            BlockInteraction::DestroyWithDecay
        } else {
            BlockInteraction::Destroy
        };
        // `getIndirectSourceEntity`: a living source caused it itself.
        level.explode_with(
            self.source,
            self.indirect.or(self.source),
            self.pos,
            self.radius,
            interaction,
            crate::level::explosion::ExplosionStyle {
                resistance: self.resistance,
                ..Default::default()
            },
        );
    }
}

/// A change of the world a mob tick causes; applied right after that tick.
#[derive(Clone, Debug, PartialEq)]
pub enum WorldAction {
    Explode(ExplosionRequest),
    /// `setBlock(pos, state, flags)`.
    SetBlock(BlockPos, BlockState, u32),
    /// `destroyBlock(pos, drop)`.
    DestroyBlock(BlockPos, bool),
    /// `levelEvent(event, pos, data)`.
    LevelEvent(i32, BlockPos, i32),
    /// `DoorBlock.setOpen(mob, level, state, pos, open)`.
    SetDoorOpen(BlockPos, bool, Option<EntityId>),
    /// A wind charge burst.
    WindBurst(WindBurst),
    /// `explode(source, damageSource(source, indirect), pos, radius, interaction)` with
    /// `ExplosionInteraction.BLOCK` (`blocks`) or `NONE`.
    ExplodeWith {
        source: Option<EntityId>,
        indirect: Option<EntityId>,
        pos: Vec3,
        radius: f32,
        blocks: bool,
    },
    /// Game events, listener moves and sculk catalyst charges.
    Level(boss::warden::LevelAction),
    /// `entityInside` of the block at the position for the entity (`applyEffectsFromBlocks`).
    EntityInside(BlockPos, EntityId),
    /// A hopper minecart takes in an item (`MinecartHopper.tryConsumeItems`).
    HopperMinecart(EntityId),
    /// `BellBlock.attemptToRing(entity, level, pos, null)`.
    RingBell(BlockPos, Option<EntityId>),
    /// `BoneMealItem.growCrop` on the block (a villager's bone meal).
    BoneMeal(BlockPos),
    /// `BeehiveBlockEntity.addOccupant`: the bee, saved as `data` when it entered (its tick
    /// discards it), moves into the hive at `hive`; `flower` is its saved flower.
    EnterHive {
        hive: BlockPos,
        bee: EntityId,
        data: Box<vanillite_nbt::Compound>,
        flower: Option<BlockPos>,
    },
}

impl WorldAction {
    /// Applies the actions that need nothing but the level; returns `false` for the others.
    pub fn apply_to_level(&self, level: &mut crate::level::Level) -> bool {
        match *self {
            WorldAction::SetBlock(pos, block, flags) => {
                level.set_block(pos, block, flags);
            }
            WorldAction::DestroyBlock(pos, drop) => {
                level.destroy_block(pos, drop, None);
            }
            WorldAction::LevelEvent(event, pos, data) => {
                level.level_event(None, event, pos, data);
            }
            WorldAction::EntityInside(pos, id) => {
                if level.entities.store.contains(id) {
                    level.entity_inside(pos, id);
                }
            }
            WorldAction::HopperMinecart(id) => crate::minecart::hopper_suck(level, id),
            WorldAction::RingBell(pos, entity) => {
                crate::block::bell::attempt_to_ring(level, entity, pos, None);
            }
            WorldAction::EnterHive {
                hive,
                bee,
                ref data,
                flower,
            } => crate::block::beehive::add_occupant(level, hive, bee, data, flower),
            WorldAction::BoneMeal(pos) => {
                let mut meal =
                    crate::item::ItemStack::new(vanillite_data::item::ids::BONE_MEAL.0, 1);
                crate::block::bonemeal::grow_crop(level, pos, &mut meal);
            }
            WorldAction::SetDoorOpen(pos, open, source) => {
                crate::block::redstone::set_door_open(level, pos, open, source);
            }
            WorldAction::WindBurst(b) => {
                // `ExplosionInteraction.TRIGGER` without damage (`damagesEntities` off).
                level.explode_with(
                    b.source,
                    None,
                    b.pos,
                    b.radius,
                    crate::level::explosion::BlockInteraction::TriggerBlock,
                    crate::level::explosion::ExplosionStyle {
                        damages_entities: false,
                        knockback: b.knockback,
                        wind_sound: Some(b.sound),
                        ..Default::default()
                    },
                );
            }
            WorldAction::Explode(_) | WorldAction::ExplodeWith { .. } | WorldAction::Level(_) => {
                return false;
            }
        }
        true
    }
}

/// The mob state of a world besides the entities: search buffers, queued world changes and
/// this tick's environment.
#[derive(Debug, Default)]
pub struct MobContext {
    pub finder: path::PathFinder,
    pub boxes: Vec<Aabb>,
    pub actions: Vec<WorldAction>,
    pub env: Environment,
    pub settings: MobSettings,
    /// Path nodes left this tick (`u64::MAX` = unlimited).
    pub path_budget: u64,
    pub ids: Vec<EntityId>,
    /// The world has at least one player.
    pub has_players: bool,
    /// Entities to remove after the tick (`discard` of other entities).
    pub discarded: Vec<EntityId>,
    /// The boss bars of entities (the wither's), by entity; see
    /// [`boss::wither::sweep_boss_bars`].
    pub boss_bars: vanillite_base::hash::FxHashMap<EntityId, crate::boss_bar::BossBar>,
    /// The End fight as the dragon and the crystals see it; `None` outside the End.
    pub dragon_fight: Option<boss::dragon::FightView>,
    /// What the dragon and the crystals report to the fight, handled after each tick.
    pub dragon_events: Vec<boss::dragon::DragonEvent>,
    /// Menus players opened at entities (chest boats, container minecarts), opened once the
    /// entities are back in the level.
    pub menus: Vec<crate::menu::container::MenuRequest>,
}

impl MobContext {
    /// Resets the per-tick state.
    pub fn begin_tick(&mut self, env: Environment, settings: MobSettings, has_players: bool) {
        self.env = env;
        self.settings = settings;
        self.has_players = has_players;
        self.path_budget = if settings.path_nodes_per_tick == 0 {
            u64::MAX
        } else {
            u64::from(settings.path_nodes_per_tick)
        };
    }
}

/// `Mth.DEG_TO_RAD` (`(float) (Math.PI / 180.0)`): Vanilla turns float degrees into radians
/// with a float multiplication before `Mth.sin`/`Mth.cos`.
pub const DEG_TO_RAD: f32 = 0.017_453_292;

/// `Entity.calculateViewVector(xRot, yRot)` with Vanilla's table sine.
pub fn view_vector(x_rot: f32, y_rot: f32) -> Vec3 {
    use vanillite_base::math::{cos, sin};
    let f = x_rot * DEG_TO_RAD;
    let f1 = -y_rot * DEG_TO_RAD;
    let (f2, f3) = (cos(f64::from(f1)), sin(f64::from(f1)));
    let (f4, f5) = (cos(f64::from(f)), sin(f64::from(f)));
    Vec3::new(f64::from(f3 * f4), f64::from(-f5), f64::from(f2 * f4))
}

/// `Mth.atan2(y, x) * Mth.RAD_TO_DEG` (the float `57.295776F` widened to double), as every
/// Vanilla entity rotation computes it.
pub fn atan2_deg(y: f64, x: f64) -> f64 {
    vanillite_base::math::atan2(y, x) * f64::from(57.295_776_f32)
}

fn fluid_ids(names: [&str; 2]) -> [u16; 2] {
    names.map(|n| vanillite_data::registry::FLUID.id(n).unwrap_or(u16::MAX))
}

/// Whether a block holds water (`FluidTags.WATER`).
pub fn is_water(state: BlockState) -> bool {
    static IDS: std::sync::OnceLock<[u16; 2]> = std::sync::OnceLock::new();
    let f = state.fluid_state();
    !f.is_empty()
        && IDS
            .get_or_init(|| fluid_ids(["minecraft:water", "minecraft:flowing_water"]))
            .contains(&f.info().fluid)
}

/// Whether a block holds lava (`FluidTags.LAVA`).
pub fn is_lava(state: BlockState) -> bool {
    static IDS: std::sync::OnceLock<[u16; 2]> = std::sync::OnceLock::new();
    let f = state.fluid_state();
    !f.is_empty()
        && IDS
            .get_or_init(|| fluid_ids(["minecraft:lava", "minecraft:flowing_lava"]))
            .contains(&f.info().fluid)
}

/// `getEyeHeight` of a mob: the type's eye height, scaled for babies.
pub fn eye_height(mob: &MobData) -> f64 {
    let info = mob.kind.type_id().info();
    let scale = if mob.baby { 0.5 } else { 1.0 };
    f64::from(info.eye_height * scale)
}

/// Adds a mob to the entities (`addFreshEntity` without `finalizeSpawn`).
pub fn spawn(
    entities: &mut Entities,
    network_ids: &NetworkIds,
    uuid: u128,
    pos: Vec3,
    rot: Rotation,
    mob: MobData,
) -> EntityId {
    // The dragon's eight parts take the ids after its own.
    let network_id = if mob.kind == Kind::EnderDragon {
        network_ids.next_block(9)
    } else {
        network_ids.next()
    };
    let mut new = NewEntity::new(mob.kind.type_id(), network_id, uuid, pos);
    new.rot = rot;
    let dims = kinds::mob_dimensions(&mob);
    let id = entities.spawn_mob(new, mob);
    if let Some(mut e) = entities.store.get_mut(id)
        && e.as_ref().dims() != dims
    {
        e.set_dims(dims);
    }
    id
}

thread_local! {
    /// The seeds of new entities: Vanilla gives every entity a fresh random
    /// (`RandomSource.create()`), so creating one draws nothing from the level's or another
    /// entity's random. The parity scenarios replace it with a stream (see
    /// [`set_entity_seed_stream`]).
    static ENTITY_SEEDS: std::cell::RefCell<(Option<LegacyRandom>, LegacyRandom)> =
        std::cell::RefCell::new((None, LegacyRandom::new(crate::survival::sound_seed())));
}

/// The seed of a newly created entity's own random (the `Entity` constructor's
/// `RandomSource.create()`).
pub fn new_entity_seed() -> i64 {
    ENTITY_SEEDS.with_borrow_mut(|(stream, unique)| match stream {
        Some(s) => s.next_long(),
        None => unique.next_long(),
    })
}

/// From now on (on this thread) new entities take their seeds from a legacy random seeded
/// with `seed` in creation order (`None`: unique seeds again). For tick exact comparisons with
/// Vanilla, whose recorder does the same.
pub fn set_entity_seed_stream(seed: Option<i64>) {
    ENTITY_SEEDS.with_borrow_mut(|(stream, _)| *stream = seed.map(LegacyRandom::new));
}

/// The UUID of a new entity (`Mth.createInsecureUUID` of its own fresh random): draws from no
/// game random.
pub fn new_uuid() -> u128 {
    crate::block::tnt::insecure_uuid()
}

/// The UUID, yaw and motion of a new item entity (`new ItemEntity(level, x, y, z, stack)`):
/// the constructor draws `bobOffs`, the yaw and the motion from the item's own random, which takes its
/// seed from the entity seed stream. Throws replace the motion afterwards.
pub fn new_item_entity() -> (u128, f32, Vec3) {
    let mut own = LegacyRandom::new(new_entity_seed());
    // `bobOffs`, then the yaw.
    let _bob = own.next_float();
    let y_rot = own.next_float() * 360.0;
    let x = own.next_double() * 0.2 - 0.1;
    let z = own.next_double() * 0.2 - 0.1;
    (new_uuid(), y_rot, Vec3::new(x, 0.2, z))
}

/// The UUID of a new entity that keeps no random of its own here (items): it still takes its
/// seed from the entity seed stream, so the entities created after it get Vanilla's seeds.
pub fn new_entity_uuid() -> u128 {
    new_entity_seed();
    new_uuid()
}

/// A random UUID (`Mth.createInsecureUUID`).
pub fn random_uuid(random: &mut impl RandomSource) -> u128 {
    let most = random.next_long() as u64 & !0xF000 | 0x4000;
    let least = random.next_long() as u64 & 0x3FFF_FFFF_FFFF_FFFF | 0x8000_0000_0000_0000;
    (u128::from(most) << 64) | u128::from(least)
}

/// What one mob tick works on.
pub struct Tick<'a, 'e> {
    pub level: &'a Level,
    pub ents: &'a mut Entities,
    pub env: &'a mut Env<'e>,
    pub cx: &'a mut MobContext,
    pub id: EntityId,
}

impl std::fmt::Debug for Tick<'_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tick")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl Tick<'_, '_> {
    /// The ticking mob's data. Panics are impossible while the mob is alive in the store;
    /// callers check [`Self::exists`] once at the start.
    pub fn mob(&mut self) -> &mut MobData {
        let row = self
            .ents
            .store
            .get(self.id)
            .map_or(u32::MAX, |e| e.kind_row());
        self.ents
            .mobs
            .get_mut(row)
            .expect("the ticking mob is in the mob store")
    }

    /// The ticking mob's data, read only.
    pub fn mob_ref(&self) -> &MobData {
        let row = self
            .ents
            .store
            .get(self.id)
            .map_or(u32::MAX, |e| e.kind_row());
        self.ents
            .mobs
            .get(row)
            .expect("the ticking mob is in the mob store")
    }

    /// Whether the ticking mob still exists.
    pub fn exists(&self) -> bool {
        self.ents.mob(self.id).is_some()
    }

    /// The ticking mob's core columns.
    pub fn core(&self) -> EntityRef<'_> {
        self.ents
            .store
            .get(self.id)
            .expect("the ticking mob is in the store")
    }

    /// The ticking mob's core columns, mutably.
    pub fn core_mut(&mut self) -> EntityMut<'_> {
        self.ents
            .store
            .get_mut(self.id)
            .expect("the ticking mob is in the store")
    }

    pub fn pos(&self) -> Vec3 {
        self.core().pos()
    }

    pub fn velocity(&self) -> Vec3 {
        self.core().velocity()
    }

    pub fn set_velocity(&mut self, v: Vec3) {
        self.core_mut().set_velocity(v);
    }

    pub fn on_ground(&self) -> bool {
        self.core().flags().contains(EntityFlags::ON_GROUND)
    }

    pub fn aabb(&self) -> Aabb {
        self.core().aabb()
    }

    pub fn block_pos(&self) -> BlockPos {
        let p = self.pos();
        BlockPos::containing(p.x, p.y, p.z)
    }

    /// `getEyeY`.
    pub fn eye_y(&self) -> f64 {
        self.pos().y + eye_height(self.mob_ref())
    }

    pub fn game_time(&self) -> i64 {
        self.env.game_time
    }

    /// Position and eye height of another living entity (player or mob).
    pub fn other(&self, id: EntityId) -> Option<Other> {
        let e = self.ents.store.get(id)?;
        let eye = if let Some(m) = self.ents.mob(id) {
            if !m.is_alive() {
                return None;
            }
            eye_height(m)
        } else {
            let p = self.ents.player(id)?;
            if p.survival.is_dead_or_dying() {
                return None;
            }
            f64::from(e.kind().info().eye_height)
                * f64::from(e.dims().height / e.kind().info().height.max(0.01))
        };
        Some(Other {
            pos: e.pos(),
            eye_y: e.pos().y + eye,
            aabb: e.aabb(),
            height: e.dims().height,
            is_player: self.ents.player(id).is_some(),
        })
    }

    /// The light level dependent value at the mob's eyes (`getLightLevelDependentMagicValue`).
    pub fn light_magic_value(&self) -> f32 {
        let p = self.pos();
        let pos = BlockPos::containing(p.x, self.eye_y(), p.z);
        if !self.level.is_loaded(pos) {
            return 0.0;
        }
        light_magic_value(self.level, &self.cx.env, pos)
    }
}

/// Another living entity as a mob sees it.
#[derive(Clone, Copy, Debug)]
pub struct Other {
    pub pos: Vec3,
    pub eye_y: f64,
    pub aabb: Aabb,
    /// `getBbHeight()`.
    pub height: f32,
    pub is_player: bool,
}

/// `getMaxLocalRawBrightness(pos)` with the sky darkened by the time of day and the weather.
pub fn max_local_raw_brightness(level: &Level, env: &Environment, pos: BlockPos) -> i32 {
    let (sky, block) = level.light(pos);
    (sky - env.sky_darken).max(block)
}

/// `LevelReader.getLightLevelDependentMagicValue(pos)`.
pub fn light_magic_value(level: &Level, env: &Environment, pos: BlockPos) -> f32 {
    #[allow(clippy::cast_precision_loss)]
    let v = max_local_raw_brightness(level, env, pos) as f32 / 15.0;
    let curved = v / (4.0 - 3.0 * v);
    env.ambient_light + (1.0 - env.ambient_light) * curved
}

/// `canSeeSky(pos)`: no light blocking block above (the sky light at full strength).
pub fn can_see_sky(level: &Level, pos: BlockPos) -> bool {
    if pos.y >= level.max_y() {
        return true;
    }
    let Some(c) = level.chunks.chunk(ChunkPos::new(pos.x >> 4, pos.z >> 4)) else {
        return false;
    };
    let Some(heights) = c
        .chunk
        .heightmaps()
        .get(vanillite_chunk::HeightmapKind::MotionBlocking)
    else {
        return false;
    };
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_wrap)]
    let top = heights.get((pos.x & 15) as u32, (pos.z & 15) as u32) as i32 + c.chunk.min_y();
    top <= pos.y
}

/// One mob tick (`ServerLevel.tickNonPassenger`: `tickCount++`, then `Mob.tick`), with the
/// despawn check before it. `level.entities` is empty during the call; the entities are in
/// `ents`.
pub fn tick(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    id: EntityId,
) {
    let mut t = Tick {
        level,
        ents,
        env,
        cx,
        id,
    };
    if !t.exists() {
        return;
    }
    living::check_despawn(&mut t);
    if t.mob_ref().removed {
        return;
    }
    t.mob().tick_count += 1;
    living::tick(&mut t);
}
