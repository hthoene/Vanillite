//! The survival state of a player and its logic: `LivingEntity`/`Player`/`ServerPlayer`'s
//! damage, death, regeneration, air, fire, fall damage, exhaustion and experience.

use vanillite_base::random::{LegacyRandom, RandomSource};
use vanillite_base::{BlockPos, Vec3};
use vanillite_config::GameMode;
use vanillite_nbt::{Compound, Tag};
use vanillite_protocol::SharedPacket;
use vanillite_protocol::packets::play::clientbound::{
    DamageEvent, EntityEvent, PlayerCombatKill, SetExperience, SetHealth, SoundSource,
};
use vanillite_protocol::text::Component;
use vanillite_world::entity::{EntityFlags, EntityMut, Pose};

use super::blocks::{self, BlockAccess, BlockFacts, FluidContact, Landing};
use super::combat::{self, CombatEntry, CombatTracker, FallLocation};
use super::damage::{DamageKind, DamageSource, DamageTypes, tag};
use super::food::{FoodAction, FoodData, exhaustion};
use super::rules::{Difficulty, GameRules, Rule};
use super::{Effect, play_sound};
use crate::player::PlayerData;
use crate::scoreboard::CriteriaKind;
use crate::stats::custom;

/// `Math.round(damage * 10.0F)` of the damage statistics, below `Float.MAX_VALUE / 10`.
#[allow(clippy::cast_possible_truncation, clippy::as_conversions)]
fn damage_stat(damage: f32) -> Option<i32> {
    (damage < 3.402_823_5e37).then(|| (damage * 10.0).round() as i32)
}

/// `Attributes.MAX_HEALTH` of players.
pub const MAX_HEALTH: f32 = 20.0;
/// `Entity.TOTAL_AIR_SUPPLY`.
pub const MAX_AIR: i32 = 300;
/// `Player.getFireImmuneTicks`.
const FIRE_IMMUNE_TICKS: i32 = 20;
/// `Attributes.SAFE_FALL_DISTANCE`.
const SAFE_FALL_DISTANCE: f64 = 3.0;
/// Player eye height when standing (`Player.STANDING_EYE_HEIGHT`) and the other poses.
pub(crate) fn eye_height(height: f32) -> f64 {
    // `EntityDimensions.eyeHeight` of the player's poses: 1.62 standing, 1.27 crouching,
    // 0.4 swimming/gliding, 0.2 sleeping/dying.
    let eye: f32 = if height >= 1.8 {
        1.62
    } else if height >= 1.5 {
        1.27
    } else if height >= 0.6 {
        0.4
    } else {
        0.2
    };
    f64::from(eye)
}

/// Entity event ids (`EntityEvent`).
pub mod entity_event {
    pub const DEATH: i8 = 3;
    pub const POOF: i8 = 60;
    pub const DROWN_PARTICLES: i8 = 67;
    pub const HONEY_SLIDE: i8 = 54;
}

/// A player's abilities as its game mode sets them (`GameType.updatePlayerAbilities`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Abilities {
    pub invulnerable: bool,
    pub mayfly: bool,
    pub instabuild: bool,
}

impl Abilities {
    /// The abilities of a game mode.
    pub fn of(mode: GameMode) -> Abilities {
        match mode {
            GameMode::Creative => Abilities {
                invulnerable: true,
                mayfly: true,
                instabuild: true,
            },
            GameMode::Spectator => Abilities {
                invulnerable: true,
                mayfly: true,
                instabuild: false,
            },
            GameMode::Survival | GameMode::Adventure => Abilities {
                invulnerable: false,
                mayfly: false,
                instabuild: false,
            },
        }
    }
}

/// Experience (`experienceLevel`, `experienceProgress`, `totalExperience`, `enchantmentSeed`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Experience {
    pub level: i32,
    pub progress: f32,
    pub total: i32,
    pub seed: i32,
}

impl Experience {
    /// `getXpNeededForNextLevel`.
    pub fn needed_for_next_level(&self) -> i32 {
        if self.level >= 30 {
            112 + (self.level - 30) * 9
        } else if self.level >= 15 {
            37 + (self.level - 15) * 5
        } else {
            7 + self.level * 2
        }
    }

    /// `giveExperienceLevels` (without the level-up sound).
    pub fn give_levels(&mut self, amount: i32) {
        self.level = self.level.saturating_add(amount);
        if self.level < 0 {
            self.level = 0;
            self.progress = 0.0;
            self.total = 0;
        }
    }

    /// `giveExperiencePoints`.
    pub fn give_points(&mut self, points: i32) {
        self.give_points_with(points, &mut |_| {});
    }

    /// `giveExperiencePoints` calling `level_up` with the new level of every level gained.
    #[allow(clippy::cast_precision_loss, clippy::as_conversions)]
    pub fn give_points_with(&mut self, points: i32, level_up: &mut dyn FnMut(i32)) {
        self.progress += points as f32 / self.needed_for_next_level() as f32;
        self.total = self.total.saturating_add(points).max(0);
        while self.progress < 0.0 {
            let remaining = self.progress * self.needed_for_next_level() as f32;
            if self.level > 0 {
                self.give_levels(-1);
                self.progress = 1.0 + remaining / self.needed_for_next_level() as f32;
            } else {
                self.give_levels(-1);
                self.progress = 0.0;
            }
        }
        while self.progress >= 1.0 {
            self.progress = (self.progress - 1.0) * self.needed_for_next_level() as f32;
            self.give_levels(1);
            level_up(self.level);
            self.progress /= self.needed_for_next_level() as f32;
        }
    }
}

/// The values last sent to the client (`lastSentHealth`, …).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Sent {
    health: f32,
    food: i32,
    saturation_zero: bool,
    exp: i32,
}

impl Default for Sent {
    fn default() -> Self {
        Sent {
            health: -1.0e8,
            food: -99_999_999,
            saturation_zero: false,
            exp: -99_999_999,
        }
    }
}

/// The survival state of a player.
#[derive(Clone, Debug, PartialEq)]
pub struct Survival {
    /// `DATA_HEALTH_ID`.
    pub health: f32,
    /// `DATA_PLAYER_ABSORPTION_ID`.
    pub absorption: f32,
    pub food: FoodData,
    /// `DATA_AIR_SUPPLY_ID`.
    pub air: i32,
    /// `remainingFireTicks`.
    pub fire_ticks: i32,
    pub fall_distance: f64,
    pub hurt_time: i32,
    /// `damageCooldownTime`.
    pub damage_cooldown: i32,
    /// `lastHurt`.
    pub last_hurt: f32,
    pub death_time: i32,
    pub xp: Experience,
    /// `DATA_SCORE_ID`.
    pub score: i32,
    /// `LastDeathLocation`: dimension and block position.
    pub last_death: Option<(String, BlockPos)>,
    /// `respawn` (`ServerPlayer.RespawnConfig`): where the player respawns.
    pub respawn: Option<RespawnConfig>,
    pub combat: CombatTracker,
    /// `tickCount`.
    pub tick_count: i32,
    /// `lastClimbablePos`'s fall location.
    pub last_climbable: Option<FallLocation>,
    /// The fluids around the player after the last update.
    pub fluids: FluidContact,
    /// Portal cooldown, the portal the player stands in and the end credits.
    pub portal: crate::portal::PortalState,
    /// The player's box at the end of the last tick, for portals touched on the way.
    last_box: Option<vanillite_base::Aabb>,
    /// `attackStrengthTicker`.
    pub attack_ticker: i32,
    /// `takeXpDelay`: ticks until the next experience orb can be taken.
    pub take_xp_delay: i32,
    /// `lastLevelUpTime`: the tick of the last level-up sound.
    pub last_level_up_time: i32,
    /// `lastHurtMob` with the tick it was hit (`lastHurtMobTimestamp`).
    pub last_hurt_mob: Option<(vanillite_world::entity::EntityId, i32)>,
    /// `lastHurtByMob` with the tick it hit (`lastHurtByMobTimestamp`).
    pub last_hurt_by_mob: Option<(vanillite_world::entity::EntityId, i32)>,
    /// The item in use (`useItem`): its hand and the ticks it has been used.
    pub using_item: Option<crate::player::using::UsingItem>,
    /// The tick until which the shield is on cool-down (an axe hit it).
    pub shield_disabled_until: i32,
    /// The item in the main hand at the last tick (`lastItemInMainHand`).
    pub last_main_hand_item: u16,
    first_tick: bool,
    sent: Sent,
    recorded: Recorded,
}

/// `ServerPlayer.lastRecorded*`: the values the built-in score criteria got last.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Recorded {
    health: f32,
    food: i32,
    air: i32,
    armor: i32,
    experience: i32,
    level: i32,
}

impl Default for Recorded {
    fn default() -> Self {
        Recorded {
            // Java's `Float.MIN_VALUE`.
            health: f32::from_bits(1),
            food: i32::MIN,
            air: i32::MIN,
            armor: i32::MIN,
            experience: i32::MIN,
            level: i32::MIN,
        }
    }
}

impl Default for Survival {
    fn default() -> Self {
        Survival {
            health: MAX_HEALTH,
            absorption: 0.0,
            food: FoodData::default(),
            air: MAX_AIR,
            fire_ticks: 0,
            fall_distance: 0.0,
            hurt_time: 0,
            damage_cooldown: 0,
            last_hurt: 0.0,
            death_time: 0,
            xp: Experience::default(),
            score: 0,
            last_death: None,
            respawn: None,
            combat: CombatTracker::default(),
            tick_count: 0,
            last_climbable: None,
            fluids: FluidContact::default(),
            portal: crate::portal::PortalState::default(),
            last_box: None,
            attack_ticker: 0,
            take_xp_delay: 0,
            last_level_up_time: 0,
            last_hurt_mob: None,
            last_hurt_by_mob: None,
            using_item: None,
            shield_disabled_until: 0,
            last_main_hand_item: 0,
            first_tick: true,
            sent: Sent::default(),
            recorded: Recorded::default(),
        }
    }
}

/// `ServerPlayer.RespawnConfig`: a respawn position (`LevelData.RespawnData`) and whether it
/// is kept without a bed or anchor there.
#[derive(Clone, Debug, PartialEq)]
pub struct RespawnConfig {
    /// `dimension`.
    pub dimension: String,
    /// `pos`.
    pub pos: BlockPos,
    /// `yaw`, wrapped to -180..180.
    pub yaw: f32,
    /// `pitch`, -90..90.
    pub pitch: f32,
    /// `forced`.
    pub forced: bool,
}

impl RespawnConfig {
    /// `RespawnConfig.CODEC`; `None` when a field is missing or out of range.
    pub fn load(data: &Compound) -> Option<RespawnConfig> {
        let (Some(Tag::String(dimension)), Some(Tag::IntArray(pos))) =
            (data.get("dimension"), data.get("pos"))
        else {
            return None;
        };
        let [x, y, z] = pos.as_slice() else {
            return None;
        };
        let yaw = float(data, "yaw").filter(|v| (-180.0..=180.0).contains(v))?;
        let pitch = float(data, "pitch").filter(|v| (-90.0..=90.0).contains(v))?;
        Some(RespawnConfig {
            dimension: dimension.clone(),
            pos: BlockPos::new(*x, *y, *z),
            yaw,
            pitch,
            forced: matches!(data.get("forced"), Some(Tag::Byte(b)) if *b != 0),
        })
    }

    /// The saved compound.
    pub fn save(&self) -> Compound {
        let mut out = Compound::with_capacity(5);
        out.insert("dimension", Tag::String(self.dimension.clone()));
        out.insert(
            "pos",
            Tag::IntArray(vec![self.pos.x, self.pos.y, self.pos.z]),
        );
        out.insert("yaw", Tag::Float(self.yaw));
        out.insert("pitch", Tag::Float(self.pitch));
        if self.forced {
            out.insert("forced", Tag::Byte(1));
        }
        out
    }
}

/// What the survival logic reads and writes besides the player.
pub struct Env<'a> {
    pub types: &'a DamageTypes,
    pub facts: &'a BlockFacts,
    pub blocks: &'a dyn BlockAccess,
    pub rules: &'a GameRules,
    pub difficulty: Difficulty,
    pub game_time: i64,
    /// The world's lowest block.
    pub min_y: i32,
    /// The world's dimension, e.g. `minecraft:overworld`.
    pub dimension: &'a str,
    /// `Level.isRainingAt`.
    pub raining_at: &'a dyn Fn(BlockPos) -> bool,
    /// The world border, where players take `outsideBorder` damage.
    pub border: Option<&'a crate::level::border::WorldBorder>,
    /// An End gateway at the position takes a player: `false` while it cools down, else it
    /// starts its cooldown (`TheEndGatewayBlockEntity.triggerCooldown`).
    pub enter_gateway: &'a mut dyn FnMut(BlockPos) -> bool,
    pub random: &'a mut LegacyRandom,
    pub out: &'a mut Vec<Effect>,
    /// Plugin hooks (damage).
    pub hooks: &'a dyn crate::hooks::GameHooks,
    /// `Player.getDisplayName` by name and UUID: the name in its team's format.
    pub display_name: &'a dyn Fn(&str, u128) -> Component,
}

impl std::fmt::Debug for Env<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Env")
            .field("difficulty", &self.difficulty)
            .field("game_time", &self.game_time)
            .field("dimension", &self.dimension)
            .finish_non_exhaustive()
    }
}

/// One accepted client move (`handlePlayerPositionChange`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MoveRecord {
    /// Position before the move.
    pub from: Vec3,
    /// Position after the move.
    pub to: Vec3,
    /// On ground before the move.
    pub was_on_ground: bool,
    /// On ground as the client reports it now.
    pub on_ground: bool,
    /// The target is above the last good position.
    pub moved_upwards: bool,
}

fn block_pos(v: Vec3) -> BlockPos {
    BlockPos::new(
        vanillite_base::math::floor(v.x),
        vanillite_base::math::floor(v.y),
        vanillite_base::math::floor(v.z),
    )
}

fn float(data: &Compound, key: &str) -> Option<f32> {
    match data.get(key) {
        Some(Tag::Float(v)) if v.is_finite() => Some(*v),
        _ => None,
    }
}

fn int(data: &Compound, key: &str) -> Option<i32> {
    data.get(key)
        .and_then(Tag::as_i64)
        .and_then(|v| i32::try_from(v).ok())
}

impl Survival {
    /// `isDeadOrDying`.
    pub fn is_dead_or_dying(&self) -> bool {
        self.health <= 0.0
    }

    /// `isHurt`.
    pub fn is_hurt(&self) -> bool {
        self.health > 0.0 && self.health < MAX_HEALTH
    }

    /// Reads the survival fields of a player data file.
    pub fn load(data: &Compound) -> Survival {
        let mut s = Survival {
            food: FoodData::load(data),
            ..Survival::default()
        };
        s.health = float(data, "Health")
            .unwrap_or(MAX_HEALTH)
            .clamp(0.0, MAX_HEALTH);
        s.absorption = float(data, "AbsorptionAmount").unwrap_or(0.0).max(0.0);
        s.air = int(data, "Air").unwrap_or(MAX_AIR);
        s.fire_ticks = int(data, "Fire").unwrap_or(0);
        s.fall_distance = match data.get("fall_distance") {
            Some(Tag::Double(v)) if v.is_finite() => *v,
            _ => 0.0,
        };
        s.hurt_time = int(data, "HurtTime").unwrap_or(0);
        s.death_time = int(data, "DeathTime").unwrap_or(0);
        s.xp = Experience {
            level: int(data, "XpLevel").unwrap_or(0),
            progress: float(data, "XpP").unwrap_or(0.0),
            total: int(data, "XpTotal").unwrap_or(0),
            seed: int(data, "XpSeed").unwrap_or(0),
        };
        s.score = int(data, "Score").unwrap_or(0);
        s.portal = crate::portal::PortalState::load(data);
        if let Some(Tag::Compound(loc)) = data.get("LastDeathLocation")
            && let (Some(Tag::String(dim)), Some(Tag::IntArray(pos))) =
                (loc.get("dimension"), loc.get("pos"))
            && let [x, y, z] = pos.as_slice()
        {
            s.last_death = Some((dim.clone(), BlockPos::new(*x, *y, *z)));
        }
        if let Some(Tag::Compound(respawn)) = data.get("respawn") {
            s.respawn = RespawnConfig::load(respawn);
        }
        s
    }

    /// Writes the survival fields into a player data file (Vanilla's names and types).
    pub fn save(&self, out: &mut Compound) {
        out.insert("Health", Tag::Float(self.health));
        out.insert("AbsorptionAmount", Tag::Float(self.absorption));
        out.insert(
            "Air",
            Tag::Short(i16::try_from(self.air).unwrap_or(i16::MAX)),
        );
        out.insert(
            "Fire",
            Tag::Short(i16::try_from(self.fire_ticks).unwrap_or(i16::MAX)),
        );
        out.insert("fall_distance", Tag::Double(self.fall_distance));
        out.insert(
            "HurtTime",
            Tag::Short(i16::try_from(self.hurt_time).unwrap_or(0)),
        );
        out.insert(
            "DeathTime",
            Tag::Short(i16::try_from(self.death_time).unwrap_or(0)),
        );
        out.insert("XpLevel", Tag::Int(self.xp.level));
        out.insert("XpP", Tag::Float(self.xp.progress));
        out.insert("XpTotal", Tag::Int(self.xp.total));
        out.insert("XpSeed", Tag::Int(self.xp.seed));
        out.insert("Score", Tag::Int(self.score));
        self.portal.save(out);
        self.food.save(out);
        out.remove("LastDeathLocation");
        if let Some((dim, pos)) = &self.last_death {
            let mut loc = Compound::with_capacity(2);
            loc.insert("dimension", Tag::String(dim.clone()));
            loc.insert("pos", Tag::IntArray(vec![pos.x, pos.y, pos.z]));
            out.insert("LastDeathLocation", Tag::Compound(loc));
        }
        out.remove("respawn");
        if let Some(respawn) = &self.respawn {
            out.insert("respawn", Tag::Compound(respawn.save()));
        }
    }

    /// `restoreFrom` for a respawn after death: health, food and air start over; experience,
    /// score and the death location survive only as Vanilla keeps them.
    /// `keep_inventory` is the game rule or a spectator dying.
    pub fn respawned(&self, keep_inventory: bool) -> Survival {
        // `restoreFrom`: the portal state and the credits carry over.
        let mut s = Survival {
            last_death: self.last_death.clone(),
            respawn: self.respawn.clone(),
            portal: self.portal.clone(),
            ..Survival::default()
        };
        if keep_inventory {
            s.xp = self.xp;
            s.score = self.score;
        }
        s.xp.seed = self.xp.seed;
        s
    }

    /// `Player.giveExperiencePoints`: the score grows by the points too. Returns the volume of
    /// the level-up sound (`entity.player.levelup`) to play at the player, if one plays: every
    /// fifth level, at most once per 100 ticks.
    #[allow(clippy::cast_precision_loss, clippy::as_conversions)]
    pub fn give_experience_points(&mut self, points: i32) -> Option<f32> {
        self.score = self.score.wrapping_add(points);
        let tick = self.tick_count;
        let last = &mut self.last_level_up_time;
        let mut volume = None;
        self.xp.give_points_with(points, &mut |level| {
            if level % 5 == 0 && (*last as f32) < tick as f32 - 100.0 {
                let v = if level > 30 { 1.0 } else { level as f32 / 30.0 };
                volume = Some(v * 0.75);
                *last = tick;
            }
        });
        volume
    }

    /// The player was moved without walking (teleport, portal): the blocks it touches next
    /// tick are only those at its new place.
    pub fn teleported(&mut self) {
        self.last_box = None;
    }

    /// Forces the next [`sync`](Self::sync) to send health, food and experience
    /// (`resetSentInfo`).
    pub fn reset_sent(&mut self) {
        self.sent = Sent::default();
    }

    /// Forces the next [`sync`](Self::sync) to send the experience (`lastSentExp = -1`).
    pub fn resend_experience(&mut self) {
        self.sent.exp = Sent::default().exp;
    }

    /// `doTick`'s `updateScoreForCriteria`: the built-in criteria whose values changed since
    /// the last call, with the new values (`Mth.ceil` of health with absorption, food, air,
    /// armour, total experience and level).
    pub fn changed_criteria(&mut self) -> Vec<(CriteriaKind, i32)> {
        let mut out = Vec::new();
        let r = &mut self.recorded;
        let health = self.health + self.absorption;
        if health != r.health {
            r.health = health;
            // `Mth.ceil`: the float's ceiling as an int, saturating like Java's cast.
            #[allow(clippy::cast_possible_truncation, clippy::as_conversions)]
            out.push((CriteriaKind::Health, health.ceil() as i32));
        }
        // TODO(#27): the armour value of the equipment; it is 0 until armour exists.
        let armor = 0;
        for (kind, value, last) in [
            (CriteriaKind::Food, self.food.food_level, &mut r.food),
            (CriteriaKind::Air, self.air, &mut r.air),
            (CriteriaKind::Armor, armor, &mut r.armor),
            (CriteriaKind::Experience, self.xp.total, &mut r.experience),
            (CriteriaKind::Level, self.xp.level, &mut r.level),
        ] {
            if value != *last {
                *last = value;
                out.push((kind, value));
            }
        }
        out
    }

    /// `doTick`'s packets: `set_health` and `set_experience` when they changed.
    pub fn sync(&mut self, sink: &dyn crate::player::PacketSink) {
        let saturation_zero = self.food.saturation == 0.0;
        if self.health != self.sent.health
            || self.sent.food != self.food.food_level
            || saturation_zero != self.sent.saturation_zero
        {
            sink.send(&SetHealth {
                health: self.health,
                food: self.food.food_level,
                saturation: self.food.saturation,
            });
            self.sent.health = self.health;
            self.sent.food = self.food.food_level;
            self.sent.saturation_zero = saturation_zero;
        }
        if self.xp.total != self.sent.exp {
            self.sent.exp = self.xp.total;
            sink.send(&SetExperience {
                progress: self.xp.progress,
                level: self.xp.level,
                total: self.xp.total,
            });
        }
    }
}

/// `setHealth`: clamped to `[0, max]`; marks the entity data dirty when it changed.
fn set_health(entity: &mut EntityMut<'_>, s: &mut Survival, health: f32) {
    let health = health.clamp(0.0, MAX_HEALTH);
    if health != s.health {
        s.health = health;
        entity.mark_data_dirty();
    }
}

fn set_air(entity: &mut EntityMut<'_>, s: &mut Survival, air: i32) {
    if air != s.air {
        s.air = air;
        entity.mark_data_dirty();
    }
}

/// `heal`.
pub fn heal(entity: &mut EntityMut<'_>, player: &mut PlayerData, amount: f32) {
    let s = &mut player.survival;
    if s.health > 0.0 {
        let health = s.health + amount;
        set_health(entity, s, health);
    }
}

/// `causeFoodExhaustion`.
pub fn cause_food_exhaustion(player: &mut PlayerData, amount: f32) {
    if !Abilities::of(player.game_mode).invulnerable {
        player.survival.food.add_exhaustion(amount);
    }
}

/// `Player.isInvulnerableTo` with `ServerPlayer`'s additions (a client that has not loaded the
/// world takes no damage).
fn is_invulnerable_to(player: &PlayerData, env: &Env<'_>, kind: DamageKind) -> bool {
    if !player.connection.movement.has_client_loaded() {
        return true;
    }
    let t = env.types;
    let rules = env.rules;
    if t.is(kind, tag::IS_DROWNING) {
        !rules.bool(Rule::DROWNING_DAMAGE)
    } else if t.is(kind, tag::IS_FALL) {
        !rules.bool(Rule::FALL_DAMAGE)
    } else if t.is(kind, tag::IS_FIRE) {
        !rules.bool(Rule::FIRE_DAMAGE)
    } else if t.is(kind, tag::IS_FREEZING) {
        !rules.bool(Rule::FREEZE_DAMAGE)
    } else {
        false
    }
}

/// `hurtServer` of a player (`ServerPlayer` → `Player` → `LivingEntity`). Returns whether the
/// damage was taken.
pub fn hurt(
    entity: &mut EntityMut<'_>,
    player: &mut PlayerData,
    env: &mut Env<'_>,
    source: DamageSource,
    amount: f32,
) -> bool {
    let types = env.types;
    let kind = source.kind;
    if is_invulnerable_to(player, env, kind) {
        return false;
    }
    if Abilities::of(player.game_mode).invulnerable
        && !types.is(kind, tag::BYPASSES_INVULNERABILITY)
    {
        return false;
    }
    if player.survival.is_dead_or_dying() {
        return false;
    }
    let mut damage = amount;
    if source.scales_with_difficulty(types, source.cause_is_mob) {
        match env.difficulty {
            Difficulty::Peaceful => damage = 0.0,
            Difficulty::Easy => damage = (damage / 2.0 + 1.0).min(damage),
            Difficulty::Normal => {}
            Difficulty::Hard => damage = damage * 3.0 / 2.0,
        }
    }
    let uuid = entity.as_ref().uuid();
    match env
        .hooks
        .entity_damage(uuid, "minecraft:player", &types.get(kind).id, damage)
    {
        Some(changed) => damage = changed,
        None => return false,
    }
    if damage == 0.0 {
        return false;
    }
    // `LivingEntity.hurtServer`.
    if damage < 0.0 {
        damage = 0.0;
    }
    let blocked_amount = apply_item_blocking(entity, player, env, &source, damage);
    damage -= blocked_amount;
    let blocked = blocked_amount > 0.0;
    if !damage.is_finite() {
        damage = f32::MAX;
    }
    let s = &mut player.survival;
    let took_full = if s.damage_cooldown > 10 && !types.is(kind, tag::BYPASSES_COOLDOWN) {
        if damage <= s.last_hurt {
            return false;
        }
        let delta = damage - s.last_hurt;
        actually_hurt(entity, player, env, source, delta);
        player.survival.last_hurt = damage;
        false
    } else {
        s.last_hurt = damage;
        s.damage_cooldown = 20;
        actually_hurt(entity, player, env, source, damage);
        player.survival.hurt_time = 10;
        true
    };
    let id = entity.as_ref().id();
    let pos = entity.as_ref().pos();
    // `setLastHurtByMob`: the living entity that caused it.
    if let Some(cause) = source.cause
        && cause != id
    {
        player.survival.last_hurt_by_mob = Some((cause, player.survival.tick_count));
    }
    if took_full && blocked {
        // `BlocksAttacks.onBlocked`: the shield sound instead of the damage event.
        let pitch = 0.8 + env.random.next_float() * 0.4;
        let seed = env.random.next_long();
        crate::survival::play_sound(
            env.out,
            None,
            pos,
            "minecraft:item.shield.block",
            SoundSource::Players,
            1.0,
            pitch,
            seed,
        );
    } else if took_full {
        env.out.push(Effect::Tracking {
            entity: id,
            with_self: true,
            packet: SharedPacket::of(&DamageEvent {
                entity_id: entity.as_ref().network_id().0,
                source_type: i32::from(kind.0),
                source_cause_id: -1,
                source_direct_id: -1,
                source_position: source.position.map(|p| vanillite_protocol::types::Vec3 {
                    x: p.x,
                    y: p.y,
                    z: p.z,
                }),
            }),
        });
    }
    if took_full {
        if !types.is(kind, tag::NO_IMPACT) && (!blocked || damage > 0.0) {
            entity.set_flags(EntityFlags::HURT_MARKED, true);
        }
        let fully_blocked = blocked && damage <= 0.0;
        if !types.is(kind, tag::NO_KNOCKBACK) && !fully_blocked {
            // `dealDefaultKnockback`: away from the source position.
            let (xd, zd) = source
                .position
                .map_or((0.0, 0.0), |p| (p.x - pos.x, p.z - pos.z));
            let on_ground = entity.as_ref().flags().contains(EntityFlags::ON_GROUND);
            let v = crate::mob::combat::knockback(
                entity.as_ref().velocity(),
                on_ground,
                0.4,
                xd,
                zd,
                0.0,
                env.random,
            );
            if let Some(v) = v {
                entity.set_velocity(v);
            }
        }
    }
    if player.survival.is_dead_or_dying() {
        if took_full {
            player_sound(env, id, pos, "minecraft:entity.player.death");
        }
        die(entity, player, env, source);
    } else if took_full {
        player_sound(env, id, pos, types.get(kind).effects.sound());
    }
    true
}

/// `Player.playSound`: heard by everyone near except the player itself (its client plays it).
fn player_sound(
    env: &mut Env<'_>,
    except: vanillite_world::entity::EntityId,
    pos: Vec3,
    sound: &str,
) {
    let seed = crate::survival::sound_seed();
    play_sound(
        env.out,
        Some(except),
        pos,
        sound,
        SoundSource::Players,
        1.0,
        1.0,
        seed,
    );
}

/// `Player.actuallyHurt` without armour items (the formula runs with zero armour).
fn actually_hurt(
    entity: &mut EntityMut<'_>,
    player: &mut PlayerData,
    env: &mut Env<'_>,
    source: DamageSource,
    damage: f32,
) {
    let types = env.types;
    let kind = source.kind;
    if is_invulnerable_to(player, env, kind) {
        return;
    }
    let mut damage = damage;
    if !types.is(kind, tag::BYPASSES_ARMOR) {
        // `hurtArmor`, then `getDamageAfterArmorAbsorb` with the equipment's armour.
        hurt_armor(player, env, &source, damage);
        let (armor, toughness) = armor_values(player);
        damage = combat::damage_after_absorb(damage, armor, toughness);
    }
    // `getDamageAfterMagicAbsorb`: the protection enchantments.
    if !types.is(kind, tag::BYPASSES_ENCHANTMENTS) && damage > 0.0 {
        let protection = protection(entity, player, env, &source);
        if protection > 0.0 {
            damage = combat::damage_after_magic_absorb(damage, protection);
        }
    }
    let s = &mut player.survival;
    let remaining = (damage - s.absorption).max(0.0);
    s.absorption -= damage - remaining;
    let absorbed = damage - remaining;
    if absorbed > 0.0
        && let Some(v) = damage_stat(absorbed)
    {
        player.stats.award(custom::DAMAGE_ABSORBED, v);
    }
    if remaining != 0.0 {
        let exhaustion = types.get(kind).exhaustion;
        cause_food_exhaustion(player, exhaustion);
        let s = &mut player.survival;
        let entry = CombatEntry {
            source,
            damage: remaining,
            fall_location: fall_location(s),
            // `(float)this.mob.fallDistance`.
            #[allow(clippy::cast_possible_truncation, clippy::as_conversions)]
            fall_distance: s.fall_distance as f32,
        };
        let alive = s.health > 0.0;
        s.combat
            .record_damage(entry, s.tick_count, alive, source.cause.is_some());
        let health = s.health - remaining;
        set_health(entity, s, health);
        if let Some(v) = damage_stat(remaining) {
            player.stats.award(custom::DAMAGE_TAKEN, v);
        }
    }
}

/// The armour and armour toughness of a player's equipment (`getArmorValue`,
/// `ARMOR_TOUGHNESS`).
pub fn armor_values(player: &PlayerData) -> (f32, f32) {
    let mut armor = Vec::new();
    let mut toughness = Vec::new();
    for slot in crate::player::encode::EQUIPMENT_SLOTS {
        let stack = player.inventory.equipment(slot);
        crate::item::attribute_modifiers(stack, slot, "minecraft:armor", &mut armor);
        crate::item::attribute_modifiers(stack, slot, "minecraft:armor_toughness", &mut toughness);
    }
    #[allow(clippy::cast_possible_truncation)]
    (
        crate::item::attribute_value(0.0, &armor, 0.0, 30.0) as f32,
        crate::item::attribute_value(0.0, &toughness, 0.0, 20.0) as f32,
    )
}

/// `hurtArmor` → `doHurtEquipment`: each armour piece loses `max(1, damage / 4)` durability.
fn hurt_armor(player: &mut PlayerData, env: &mut Env<'_>, source: &DamageSource, damage: f32) {
    if damage <= 0.0 || player.game_mode == GameMode::Creative {
        return;
    }
    #[allow(clippy::cast_possible_truncation)]
    let amount = (damage / 4.0).max(1.0) as i32;
    let fire = env.types.is(source.kind, tag::IS_FIRE);
    for index in 36..40 {
        let unbreaking = crate::player::play::game_data().map_or(0, |d| {
            d.enchantment_level(player.inventory.get(index), "minecraft:unbreaking")
        });
        let Some(stack) = player.inventory.get_mut(index) else {
            continue;
        };
        if stack.is_empty() || !crate::item::is_damageable(stack) {
            continue;
        }
        // Netherite (`damage_resistant` against fire) is not hurt by fire.
        if fire
            && crate::item::item_id(stack)
                .name()
                .starts_with("minecraft:netherite_")
        {
            continue;
        }
        let mut n = 0;
        for _ in 0..amount {
            // Unbreaking on armour: a chance of 60 % + 40 % / (level + 1) to take damage.
            #[allow(clippy::cast_precision_loss)]
            let chance = 0.6 + 0.4 / (unbreaking.max(0) as f32 + 1.0);
            if unbreaking <= 0 || env.random.next_float() < chance {
                n += 1;
            }
        }
        let d = crate::item::damage(stack) + n;
        if d >= crate::item::max_damage(stack) {
            stack.shrink(1);
        } else if n > 0 {
            crate::item::set_damage(stack, d);
        }
    }
}

/// `EnchantmentHelper.getDamageProtection` of a player's equipment.
fn protection(
    entity: &EntityMut<'_>,
    player: &PlayerData,
    env: &mut Env<'_>,
    source: &DamageSource,
) -> f32 {
    let Some(data) = crate::player::play::game_data() else {
        return 0.0;
    };
    let equipment: Vec<(crate::item::EquipmentSlot, &crate::item::ItemStack)> =
        crate::player::encode::EQUIPMENT_SLOTS
            .iter()
            .map(|&s| (s, player.inventory.equipment(s)))
            .filter(|(_, s)| !s.is_empty())
            .collect();
    if equipment.is_empty() {
        return 0.0;
    }
    let flags = entity.as_ref().flags();
    let this = crate::predicate::EntityFacts {
        kind: Some(entity.as_ref().kind()),
        on_fire: player.survival.fire_ticks > 0,
        baby: false,
        sneaking: flags.contains(EntityFlags::CROUCHING),
        sprinting: flags.contains(EntityFlags::SPRINTING),
        swimming: flags.contains(EntityFlags::SWIMMING),
        on_ground: flags.contains(EntityFlags::ON_GROUND),
        smelts_loot: false,
        sheep_color: None,
        sheared: false,
    };
    let cx = crate::enchant::EnchantContext {
        types: env.types,
        tags: data,
        this: Some(this),
        attacker: None,
        direct_attacker: None,
        attacking_player: None,
        source: Some(crate::predicate::SourceFacts {
            kind: source.kind,
            is_direct: source.cause.is_some() && source.cause == source.direct,
            direct: None,
            cause: None,
        }),
    };
    data.enchants.damage_protection(&equipment, &cx, env.random)
}

/// `applyItemBlocking`: a raised shield facing the source blocks the damage and takes
/// durability; an axe-like weapon disables it for a while.
fn apply_item_blocking(
    entity: &mut EntityMut<'_>,
    player: &mut PlayerData,
    env: &mut Env<'_>,
    source: &DamageSource,
    damage: f32,
) -> f32 {
    if damage <= 0.0 {
        return 0.0;
    }
    let Some(using) = player.survival.using_item else {
        return 0.0;
    };
    let shield = crate::item::ItemStack::new(using.item, 1);
    if !crate::item::item_id(&shield).info().blocks_attacks || using.ticks < 5 {
        return 0.0;
    }
    if env.types.is(source.kind, tag::BYPASSES_SHIELD) {
        return 0.0;
    }
    let pos = entity.as_ref().pos();
    let angle = match source.position {
        Some(at) => {
            let head = f64::from(entity.as_ref().rot().y_head_rot) * std::f64::consts::PI / 180.0;
            let view = Vec3::new(-head.sin(), 0.0, head.cos());
            let to = Vec3::new(at.x - pos.x, 0.0, at.z - pos.z).normalize();
            to.dot(view).clamp(-1.0, 1.0).acos()
        }
        None => std::f64::consts::PI,
    };
    // The default damage reduction: everything within 90 degrees.
    if angle > f64::from(std::f32::consts::PI / 180.0 * 90.0) {
        return 0.0;
    }
    let blocked = damage;
    // `item_damage`: threshold 3, base 1, factor 1.
    if blocked >= 3.0 && player.game_mode != GameMode::Creative {
        #[allow(clippy::cast_possible_truncation)]
        let n = (1.0 + blocked).floor() as i32;
        let index = match using.hand {
            vanillite_protocol::packets::play::serverbound::Hand::MainHand => {
                player.inventory.selected
            }
            vanillite_protocol::packets::play::serverbound::Hand::OffHand => {
                crate::inventory::OFFHAND
            }
        };
        if let Some(s) = player.inventory.get_mut(index)
            && crate::item::is_damageable(s)
        {
            let d = crate::item::damage(s) + n;
            if d >= crate::item::max_damage(s) {
                s.shrink(1);
                player.survival.using_item = None;
                entity.mark_data_dirty();
            } else {
                crate::item::set_damage(s, d);
            }
        }
    }
    // `blockUsingItem`: weapons that disable shields put the shield on cool-down.
    if source.disable_shield_seconds > 0.0 && !env.types.is(source.kind, tag::IS_PROJECTILE) {
        #[allow(clippy::cast_possible_truncation)]
        let ticks = (source.disable_shield_seconds * 20.0) as i32;
        player.survival.using_item = None;
        player.survival.shield_disabled_until = player.survival.tick_count + ticks;
        entity.mark_data_dirty();
        let seed = env.random.next_long();
        crate::survival::play_sound(
            env.out,
            None,
            pos,
            "minecraft:item.shield.break",
            SoundSource::Players,
            0.8,
            0.8 + env.random.next_float() * 0.4,
            seed,
        );
        player
            .connection
            .sink
            .send(&vanillite_protocol::packets::play::clientbound::Cooldown {
                group: vanillite_data::registry::ITEM
                    .name(using.item)
                    .unwrap_or("minecraft:shield")
                    .to_owned(),
                duration: ticks,
            });
    }
    blocked
}

/// `FallLocation.getCurrentFallLocation`.
fn fall_location(s: &Survival) -> Option<FallLocation> {
    match s.last_climbable {
        Some(location) => Some(location),
        None if s.fluids.in_water() => Some(FallLocation::Water),
        None => None,
    }
}

/// `ServerPlayer.die`.
pub fn die(
    entity: &mut EntityMut<'_>,
    player: &mut PlayerData,
    env: &mut Env<'_>,
    source: DamageSource,
) {
    let network_id = entity.as_ref().network_id().0;
    let uuid = entity.as_ref().uuid();
    let id = entity.as_ref().id();
    let sink = std::sync::Arc::clone(&player.connection.sink);
    let mut broadcast = None;
    if env.rules.bool(Rule::SHOW_DEATH_MESSAGES) {
        let name = (env.display_name)(&player.profile.name, uuid);
        let message = player.survival.combat.death_message(env.types, name);
        sink.send(&PlayerCombatKill {
            player_id: network_id,
            message: message.clone(),
        });
        broadcast = Some(message);
    } else {
        sink.send(&PlayerCombatKill {
            player_id: network_id,
            message: Component::empty(),
        });
    }
    // `dropAllDeathLoot` → `dropEquipment` and `dropExperience`.
    if !env.rules.bool(Rule::KEEP_INVENTORY) && player.game_mode != GameMode::Spectator {
        let pos = entity.as_ref().pos();
        // `getEyeY()` with the pose's float eye height.
        let eye = match entity.as_ref().pose() {
            Pose::Crouching => 1.27f32,
            Pose::Swimming | Pose::FallFlying | Pose::SpinAttack => 0.4,
            Pose::Sleeping | Pose::Dying => 0.2,
            _ => 1.62,
        };
        let eye_y = pos.y + f64::from(eye);
        drop_all(
            &mut player.inventory,
            Vec3::new(pos.x, eye_y - 0.3, pos.z),
            env,
        );
        // `getBaseExperienceReward`.
        let amount = player.survival.xp.level.saturating_mul(7).min(100);
        if amount > 0 {
            env.out.push(Effect::Experience { pos, amount });
        }
    }
    // The kill credit is the damage's causing entity: nothing else credits kills yet.
    env.out.push(Effect::Died {
        victim: id,
        killer: source.cause,
        message: broadcast,
    });
    env.out.push(Effect::Tracking {
        entity: id,
        with_self: true,
        packet: SharedPacket::of(&EntityEvent {
            entity_id: network_id,
            event: entity_event::DEATH,
        }),
    });
    entity.set_pose(Pose::Dying);
    entity.set_dims(crate::player::dimensions(Pose::Dying));
    let s = &mut player.survival;
    s.fire_ticks = s.fire_ticks.min(0);
    entity.set_flags(EntityFlags::ON_FIRE, false);
    let alive = s.health > 0.0;
    s.combat.recheck_status(s.tick_count, alive);
    s.last_death = Some((env.dimension.to_owned(), block_pos(entity.as_ref().pos())));
    player.stats.award(custom::DEATHS, 1);
    player.stats.reset(custom::TIME_SINCE_DEATH);
    player.stats.reset(custom::TIME_SINCE_REST);
    player.connection.movement.mark_client_unloaded();
}

/// `destroyVanishingCursedItems` + `Inventory.dropAll`: every stack is thrown in a random
/// direction from `from` (`createItemStackToDrop(stack, true, false)`).
fn drop_all(inventory: &mut crate::inventory::PlayerInventory, from: Vec3, env: &mut Env<'_>) {
    let vanishing = |stack: &crate::item::ItemStack| {
        crate::player::play::game_data()
            .is_some_and(|d| d.enchantment_level(stack, "minecraft:vanishing_curse") > 0)
    };
    let inv = inventory;
    // The main inventory, then the equipment in `EquipmentSlot` order.
    let slots = inv
        .items
        .iter_mut()
        .chain(std::iter::once(&mut inv.offhand))
        .chain(inv.armor.iter_mut());
    for slot in slots {
        let stack = std::mem::replace(slot, crate::item::ItemStack::EMPTY);
        if stack.is_empty() || vanishing(&stack) {
            continue;
        }
        let pow = env.random.next_float() * 0.5;
        let dir = env.random.next_float() * std::f32::consts::TAU;
        let velocity = Vec3::new(
            f64::from(-vanillite_base::math::sin(f64::from(dir)) * pow),
            f64::from(0.2f32),
            f64::from(vanillite_base::math::cos(f64::from(dir)) * pow),
        );
        let uuid = (u128::from(env.random.next_long() as u64) << 64)
            | u128::from(env.random.next_long() as u64);
        let y_rot = env.random.next_float() * 360.0;
        env.out.push(Effect::DropItem {
            uuid,
            pos: from,
            velocity,
            y_rot,
            pickup_delay: 40,
            stack,
        });
    }
}

/// `/kill`: `hurtServer(genericKill, Float.MAX_VALUE)`.
pub fn kill(entity: &mut EntityMut<'_>, player: &mut PlayerData, env: &mut Env<'_>) -> bool {
    let source = DamageSource::of(env.types.generic_kill);
    hurt(entity, player, env, source, f32::MAX)
}

/// What the world must do with a player after its tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TickResult {
    /// Nothing.
    Alive,
    /// The death animation is over: remove the entity (`remove(KILLED)`); the player waits
    /// for its respawn.
    Remove,
    /// The player goes through a portal now (`handlePortal`): the portal and the block it
    /// entered.
    Portal(crate::portal::PortalKind, BlockPos),
    /// The player entered the End's exit portal for the first time: the credits roll
    /// (`showEndCredits`), the entity leaves the world and the player respawns when they end.
    Credits,
}

/// `ServerPlayer.tick`'s survival part (once per world tick, with the entities).
pub fn tick_entity(player: &mut PlayerData) {
    let s = &mut player.survival;
    if s.damage_cooldown > 0 {
        s.damage_cooldown -= 1;
    }
    s.attack_ticker = s.attack_ticker.saturating_add(1);
    if s.take_xp_delay > 0 {
        s.take_xp_delay -= 1;
    }
    // `resetAttackStrengthTicker` when another item comes into the main hand.
    let held = player.inventory.selected_item().item;
    let s = &mut player.survival;
    if held != s.last_main_hand_item {
        s.attack_ticker = 0;
        s.last_main_hand_item = held;
    }
}

/// `ServerPlayer.doTick`: `Player.tick` (base tick: fluids, fire, void, suffocation, air,
/// death; `aiStep`: regeneration and block effects), the food tick and the health and
/// experience packets.
pub fn do_tick(
    entity: &mut EntityMut<'_>,
    player: &mut PlayerData,
    env: &mut Env<'_>,
) -> TickResult {
    let spectator = player.game_mode == GameMode::Spectator;
    let abilities = Abilities::of(player.game_mode);
    let mut result = TickResult::Alive;
    if player.survival.portal.won_game {
        // Removed from the world while the credits roll.
        return result;
    }
    {
        let s = &mut player.survival;
        s.tick_count = s.tick_count.wrapping_add(1);
    }
    let id = entity.as_ref().id();
    // `Entity.baseTick`: `handlePortal` first.
    let can_use_portal = !player.survival.is_dead_or_dying();
    let portal_delay = |kind| match kind {
        crate::portal::PortalKind::Nether => env
            .rules
            .get(if abilities.invulnerable {
                Rule::PLAYERS_NETHER_PORTAL_CREATIVE_DELAY
            } else {
                Rule::PLAYERS_NETHER_PORTAL_DEFAULT_DELAY
            })
            .max(0),
        _ => 0,
    };
    if let Some((kind, entry)) = player.survival.portal.handle(can_use_portal, portal_delay) {
        return TickResult::Portal(kind, entry);
    }
    update_fluids(entity, player, env);
    update_swimming(entity, player);
    {
        let s = &mut player.survival;
        if s.fire_ticks > 0 {
            let in_lava = !s.first_tick && s.fluids.in_lava();
            if s.fire_ticks % 20 == 0 && !in_lava {
                let source = DamageSource::of(env.types.on_fire);
                hurt(entity, player, env, source, 1.0);
            }
            player.survival.fire_ticks -= 1;
        }
    }
    {
        let s = &mut player.survival;
        if !s.first_tick && s.fluids.in_lava() {
            s.fall_distance *= 0.5;
        }
    }
    if entity.as_ref().pos().y < f64::from(env.min_y.saturating_sub(64)) {
        // `LivingEntity.onBelowWorld`.
        let source = DamageSource::of(env.types.out_of_world);
        hurt(entity, player, env, source, 4.0);
    }
    let on_fire = player.survival.fire_ticks > 0;
    entity.set_flags(EntityFlags::ON_FIRE, on_fire);
    player.survival.first_tick = false;
    // `LivingEntity.baseTick`.
    let alive = !player.survival.is_dead_or_dying();
    if alive {
        let pos = entity.as_ref().pos();
        let dims = entity.as_ref().dims();
        let eye = Vec3::new(pos.x, pos.y + eye_height(dims.height), pos.z);
        if !spectator && blocks::is_in_wall(env.blocks, env.facts, eye, dims.width) {
            let source = DamageSource::of(env.types.in_wall);
            hurt(entity, player, env, source, 1.0);
        } else if let Some(amount) = env
            .border
            .and_then(|b| b.damage(pos.x, pos.z, f64::from(dims.width) / 2.0))
        {
            let source = DamageSource::of(env.types.outside_border);
            hurt(entity, player, env, source, amount);
        }
        let eye_in_water = player.survival.fluids.eye_in_water
            && !env
                .blocks
                .block_state(block_pos(eye))
                .is_some_and(|s| env.facts.is_bubble_column(s));
        if eye_in_water {
            if !abilities.invulnerable {
                let air = player.survival.air - 1;
                set_air(entity, &mut player.survival, air);
                if player.survival.air <= -20 {
                    set_air(entity, &mut player.survival, 0);
                    env.out.push(Effect::Tracking {
                        entity: id,
                        with_self: true,
                        packet: SharedPacket::of(&EntityEvent {
                            entity_id: entity.as_ref().network_id().0,
                            event: entity_event::DROWN_PARTICLES,
                        }),
                    });
                    let source = DamageSource::of(env.types.drown);
                    hurt(entity, player, env, source, 2.0);
                }
            } else if player.survival.air < MAX_AIR {
                let air = (player.survival.air + 4).min(MAX_AIR);
                set_air(entity, &mut player.survival, air);
            }
        } else if player.survival.air < MAX_AIR {
            let air = (player.survival.air + 4).min(MAX_AIR);
            set_air(entity, &mut player.survival, air);
        }
    }
    {
        let s = &mut player.survival;
        if s.hurt_time > 0 {
            s.hurt_time -= 1;
        }
    }
    if player.survival.is_dead_or_dying() {
        // `tickDeath`.
        let s = &mut player.survival;
        s.death_time += 1;
        if s.death_time >= 20 {
            env.out.push(Effect::Tracking {
                entity: id,
                with_self: true,
                packet: SharedPacket::of(&EntityEvent {
                    entity_id: entity.as_ref().network_id().0,
                    event: entity_event::POOF,
                }),
            });
            result = TickResult::Remove;
        }
    }
    {
        let s = &mut player.survival;
        let alive = s.health > 0.0;
        s.combat.recheck_status(s.tick_count, alive);
    }
    // `Player.aiStep`: peaceful regeneration (`ServerPlayer.tickRegeneration`).
    if env.difficulty == Difficulty::Peaceful && env.rules.bool(Rule::NATURAL_HEALTH_REGENERATION) {
        if player.survival.tick_count % 20 == 0 && player.survival.health < MAX_HEALTH {
            heal(entity, player, 1.0);
        }
        let s = &mut player.survival;
        if s.food.needs_food() && s.tick_count % 10 == 0 {
            s.food.food_level += 1;
        }
    }
    if result == TickResult::Alive && !player.survival.is_dead_or_dying() {
        if !spectator {
            apply_block_effects(entity, player, env);
        }
        if enter_portals(entity, player, env) {
            result = TickResult::Credits;
        }
    }
    player.survival.last_box = Some(entity.as_ref().aabb());
    // `FoodData.tick`.
    let s = &mut player.survival;
    let action = s.food.tick(
        env.difficulty,
        env.rules.bool(Rule::NATURAL_HEALTH_REGENERATION),
        s.is_hurt(),
        s.health,
    );
    match action {
        FoodAction::None => {}
        FoodAction::Heal(amount) => heal(entity, player, amount),
        FoodAction::Starve => {
            let source = DamageSource::of(env.types.starve);
            hurt(entity, player, env, source, 1.0);
        }
    }
    // `ServerPlayer.doTick`'s time statistics; players never sleep yet.
    let stats = &mut player.stats;
    stats.award(custom::PLAY_TIME, 1);
    stats.award(custom::TOTAL_WORLD_TIME, 1);
    if player.survival.health > 0.0 {
        stats.award(custom::TIME_SINCE_DEATH, 1);
    }
    // `isDiscrete`: the sneak key is held.
    if entity.as_ref().flags().contains(EntityFlags::CROUCHING) {
        stats.award(custom::CROUCH_TIME, 1);
    }
    stats.award(custom::TIME_SINCE_REST, 1);
    let sink = std::sync::Arc::clone(&player.connection.sink);
    player.survival.sync(sink.as_ref());
    result
}

/// `updateFluidInteraction`: the fluids around the player; touching water resets the fall
/// distance.
fn update_fluids(entity: &EntityMut<'_>, player: &mut PlayerData, env: &Env<'_>) {
    let e = entity.as_ref();
    let pos = e.pos();
    let eye = Vec3::new(pos.x, pos.y + eye_height(e.dims().height), pos.z);
    let contact = blocks::fluid_contact(env.blocks, env.facts, e.aabb(), eye);
    let s = &mut player.survival;
    s.fluids = contact;
    if contact.in_water() {
        s.fall_distance = 0.0;
    }
}

/// `updateSwimming`: sprinting under water swims.
fn update_swimming(entity: &mut EntityMut<'_>, player: &PlayerData) {
    let flags = entity.as_ref().flags();
    let fluids = player.survival.fluids;
    let sprinting = flags.contains(EntityFlags::SPRINTING);
    let swimming = if flags.contains(EntityFlags::SWIMMING) {
        sprinting && fluids.in_water()
    } else {
        sprinting && fluids.eye_in_water && fluids.in_water()
    };
    let swimming = swimming && player.game_mode != GameMode::Spectator;
    if swimming != flags.contains(EntityFlags::SWIMMING) {
        entity.set_flags(EntityFlags::SWIMMING, swimming);
    }
}

/// The portal blocks the player touched on its way this tick (`entityInside` of nether and
/// End portals and gateways). Returns whether the end credits start.
fn enter_portals(entity: &EntityMut<'_>, player: &mut PlayerData, env: &mut Env<'_>) -> bool {
    use crate::portal::PortalKind;
    let now = entity.as_ref().aabb();
    let swept = match player.survival.last_box {
        Some(last) => vanillite_base::Aabb {
            min_x: now.min_x.min(last.min_x),
            min_y: now.min_y.min(last.min_y),
            min_z: now.min_z.min(last.min_z),
            max_x: now.max_x.max(last.max_x),
            max_y: now.max_y.max(last.max_y),
            max_z: now.max_z.max(last.max_z),
        },
        None => now,
    };
    let portals = blocks::inside_portals(env.blocks, swept);
    let portal = &mut player.survival.portal;
    if let Some(pos) = portals.nether {
        portal.set_inside(PortalKind::Nether, pos);
    }
    if let Some(pos) = portals.end {
        if env.dimension == "minecraft:the_end" && !portal.seen_credits {
            // `showEndCredits`.
            portal.seen_credits = true;
            if !portal.won_game {
                portal.won_game = true;
                player.connection.sink.send(
                    &vanillite_protocol::packets::play::clientbound::GameEvent {
                        event: vanillite_protocol::packets::play::clientbound::GameEvent::WIN_GAME,
                        param: 0.0,
                    },
                );
                return true;
            }
        } else {
            portal.set_inside(PortalKind::End, pos);
        }
    }
    if let Some(pos) = portals.gateway
        && (env.enter_gateway)(pos)
    {
        portal.set_inside(PortalKind::Gateway, pos);
    }
    false
}

/// `Entity.applyEffectsFromBlocks` at the player's position: the floor it stands on (magma),
/// fire and lava inside its box, water and rain putting the fire out.
fn apply_block_effects(entity: &mut EntityMut<'_>, player: &mut PlayerData, env: &mut Env<'_>) {
    let e = entity.as_ref();
    let pos = e.pos();
    let id = e.id();
    let flags = e.flags();
    let aabb = e.aabb();
    if flags.contains(EntityFlags::ON_GROUND) {
        let on = BlockPos::new(
            vanillite_base::math::floor(pos.x),
            vanillite_base::math::floor(pos.y - 0.2),
            vanillite_base::math::floor(pos.z),
        );
        // `MagmaBlock.stepOn`: careful (sneaking) players are safe.
        if env
            .blocks
            .block_state(on)
            .is_some_and(|s| env.facts.is_hot_floor(s))
            && !flags.contains(EntityFlags::CROUCHING)
        {
            let source = DamageSource::of(env.types.hot_floor);
            hurt(entity, player, env, source, 1.0);
        }
    }
    let was_on_fire = player.survival.fire_ticks > 0;
    let previous = player.survival.fire_ticks;
    let inside = blocks::inside_blocks(env.blocks, env.facts, aabb);
    if let Some(damage) = inside.fire {
        // `BaseFireBlock.fireIgnite`, then the fire damage.
        let s = &mut player.survival;
        if s.fire_ticks < 0 {
            s.fire_ticks += 1;
        } else {
            s.fire_ticks += env.random.next_int_between(1, 3);
        }
        if s.fire_ticks >= 0 {
            ignite_for_ticks(s, 160);
        }
        let source = DamageSource::of(env.types.in_fire);
        hurt(entity, player, env, source, damage);
    }
    if inside.lava_blocks > 0 && !player.survival.is_dead_or_dying() {
        // `lavaIgnite`, then `lavaHurt`.
        ignite_for_ticks(&mut player.survival, 300);
        let source = DamageSource::of(env.types.lava);
        if hurt(entity, player, env, source, 4.0) {
            let pitch = 2.0 + env.random.next_float() * 0.4;
            let seed = crate::survival::sound_seed();
            play_sound(
                env.out,
                None,
                pos,
                "minecraft:entity.generic.burn",
                SoundSource::Players,
                0.4,
                pitch,
                seed,
            );
        }
    }
    if inside.water {
        let s = &mut player.survival;
        s.fire_ticks = s.fire_ticks.min(0);
    }
    let raining = (env.raining_at)(block_pos(pos))
        || (env.raining_at)(BlockPos::new(
            vanillite_base::math::floor(pos.x),
            vanillite_base::math::floor(aabb.max_y),
            vanillite_base::math::floor(pos.z),
        ));
    if raining {
        let s = &mut player.survival;
        s.fire_ticks = s.fire_ticks.min(0);
    }
    if was_on_fire && player.survival.fire_ticks <= 0 {
        // `playEntityOnFireExtinguishedSound`.
        let pitch = 1.6 + (env.random.next_float() - env.random.next_float()) * 0.4;
        let seed = crate::survival::sound_seed();
        play_sound(
            env.out,
            Some(id),
            pos,
            "minecraft:entity.generic.extinguish_fire",
            SoundSource::Players,
            0.7,
            pitch,
            seed,
        );
    }
    let ignited = player.survival.fire_ticks > previous;
    let s = &mut player.survival;
    if s.fire_ticks <= 0 && !ignited {
        s.fire_ticks = -FIRE_IMMUNE_TICKS;
    }
}

/// `igniteForTicks`.
fn ignite_for_ticks(s: &mut Survival, ticks: i32) {
    if s.fire_ticks < ticks {
        s.fire_ticks = ticks;
    }
}

/// `Entity.thunderHit`: one more fire tick, eight seconds of fire when that reached zero, and
/// the lightning damage.
pub fn thunder_hit(entity: &mut EntityMut<'_>, player: &mut PlayerData, env: &mut Env<'_>) {
    let s = &mut player.survival;
    s.fire_ticks = s.fire_ticks.saturating_add(1);
    if s.fire_ticks == 0 {
        ignite_for_ticks(s, 160);
    }
    let source = DamageSource::of(env.types.lightning_bolt);
    hurt(entity, player, env, source, 5.0);
}

/// The survival effects of one accepted client move, in Vanilla's order: the jump's exhaustion,
/// `doCheckFallDamage`, the fall reset after moving up and `checkMovementStatistics`.
pub fn on_move(
    entity: &mut EntityMut<'_>,
    player: &mut PlayerData,
    env: &mut Env<'_>,
    mv: MoveRecord,
) {
    if player.survival.is_dead_or_dying() {
        return;
    }
    let flags = entity.as_ref().flags();
    let sprinting = flags.contains(EntityFlags::SPRINTING);
    let crouching = flags.contains(EntityFlags::CROUCHING);
    if mv.was_on_ground && !mv.on_ground && mv.moved_upwards {
        // `ServerPlayer.jumpFromGround`.
        let cost = if sprinting {
            exhaustion::SPRINT_JUMP
        } else {
            exhaustion::JUMP
        };
        player.stats.award(custom::JUMP, 1);
        cause_food_exhaustion(player, cost);
    }
    let dy = mv.to.y - mv.from.y;
    check_fall_damage(entity, player, env, dy, mv.on_ground, crouching);
    if mv.moved_upwards {
        player.survival.fall_distance = 0.0;
    }
    update_climbable(entity, player, env);
    movement_statistics(player, entity, env, mv);
}

/// `onClimbable`: remembers the climbable block the player is in for the fall message.
fn update_climbable(entity: &EntityMut<'_>, player: &mut PlayerData, env: &Env<'_>) {
    if player.game_mode == GameMode::Spectator {
        return;
    }
    let pos = block_pos(entity.as_ref().pos());
    if let Some(state) = env.blocks.block_state(pos) {
        let block = state.block();
        if env.facts.is_climbable(block) {
            player.survival.last_climbable = Some(FallLocation::of_block(
                block.name(),
                env.facts.is_trapdoor(block),
            ));
        }
    }
}

/// `LivingEntity.checkFallDamage` → `Entity.checkFallDamage` → `Block.fallOn`.
fn check_fall_damage(
    entity: &mut EntityMut<'_>,
    player: &mut PlayerData,
    env: &mut Env<'_>,
    dy: f64,
    on_ground: bool,
    crouching: bool,
) {
    if !player.survival.fluids.in_water() {
        update_fluids(entity, player, env);
    }
    let pos = entity.as_ref().pos();
    let on_pos = BlockPos::new(
        vanillite_base::math::floor(pos.x),
        vanillite_base::math::floor(pos.y - 0.2),
        vanillite_base::math::floor(pos.z),
    );
    let Some(on_state) = env.blocks.block_state(on_pos) else {
        // `touchingUnloadedChunk`.
        return;
    };
    if !player.survival.fluids.in_water() && dy < 0.0 {
        player.survival.fall_distance -= dy;
    }
    if on_ground {
        let fall = player.survival.fall_distance;
        if fall > 0.0 {
            match env.facts.landing(on_state) {
                Landing::Fall(multiplier) => {
                    cause_fall_damage(entity, player, env, fall, multiplier, env.types.fall);
                }
                Landing::Slime => {
                    let multiplier = if crouching { 1.0 } else { 0.0 };
                    cause_fall_damage(entity, player, env, fall, multiplier, env.types.fall);
                }
                Landing::Honey => {
                    let id = entity.as_ref().id();
                    player_sound(env, id, pos, "minecraft:block.honey_block.slide");
                    env.out.push(Effect::Tracking {
                        entity: id,
                        with_self: true,
                        packet: SharedPacket::of(&EntityEvent {
                            entity_id: entity.as_ref().network_id().0,
                            event: entity_event::HONEY_SLIDE,
                        }),
                    });
                    cause_fall_damage(entity, player, env, fall, 0.2, env.types.fall);
                }
                Landing::Stalagmite => {
                    let kind = env.types.stalagmite;
                    cause_fall_damage(entity, player, env, fall + 2.5, 2.0, kind);
                }
            }
        }
        player.survival.fall_distance = 0.0;
        player.survival.last_climbable = None;
    }
}

/// `calculateFallDamage`.
pub fn fall_damage(fall_distance: f64, multiplier: f32) -> i32 {
    let power = fall_distance + 1.0e-6 - SAFE_FALL_DISTANCE;
    // `Mth.floor(double)`.
    vanillite_base::math::floor(power * f64::from(multiplier))
}

/// `Player.causeFallDamage` → `LivingEntity.causeFallDamage`.
fn cause_fall_damage(
    entity: &mut EntityMut<'_>,
    player: &mut PlayerData,
    env: &mut Env<'_>,
    fall_distance: f64,
    multiplier: f32,
    kind: DamageKind,
) {
    if Abilities::of(player.game_mode).mayfly {
        return;
    }
    if fall_distance >= 2.0 {
        #[allow(clippy::cast_possible_truncation, clippy::as_conversions)]
        let cm = (fall_distance * 100.0).round() as i32;
        player.stats.award(custom::FALL_ONE_CM, cm);
    }
    let damage = fall_damage(fall_distance, multiplier);
    if damage > 0 {
        let id = entity.as_ref().id();
        let pos = entity.as_ref().pos();
        let sound = if damage > 4 {
            "minecraft:entity.player.big_fall"
        } else {
            "minecraft:entity.player.small_fall"
        };
        player_sound(env, id, pos, sound);
        // TODO(block sounds): `playBlockFallSound` needs the block sound types.
        #[allow(clippy::cast_precision_loss, clippy::as_conversions)]
        let amount = damage as f32;
        hurt(entity, player, env, DamageSource::of(kind), amount);
    }
}

/// `ServerPlayer.checkMovementStatistics`: the distance statistics and the exhaustion of
/// swimming, walking in water and sprinting (players never ride yet).
fn movement_statistics(
    player: &mut PlayerData,
    entity: &EntityMut<'_>,
    env: &Env<'_>,
    mv: MoveRecord,
) {
    let (dx, dy, dz) = (
        mv.to.x - mv.from.x,
        mv.to.y - mv.from.y,
        mv.to.z - mv.from.z,
    );
    // `didNotMove`.
    if dx * dx + dy * dy + dz * dz < 1.0e-5 {
        return;
    }
    // `Math.round((float)Math.sqrt(...) * 100.0F)`.
    #[allow(clippy::cast_possible_truncation, clippy::as_conversions)]
    let cm = |d: f64| ((d.sqrt() as f32) * 100.0).round() as i32;
    #[allow(clippy::cast_precision_loss, clippy::as_conversions)]
    let cost = |per_meter: f32, cm: i32| per_meter * cm as f32 * 0.01;
    let flags = entity.as_ref().flags();
    let fluids = player.survival.fluids;
    // `onClimbable`.
    let climbable = player.game_mode != GameMode::Spectator
        && env
            .blocks
            .block_state(block_pos(entity.as_ref().pos()))
            .is_some_and(|s| env.facts.is_climbable(s.block()));
    let in_fluid = |player: &mut PlayerData, stat, d: i32| {
        if d > 0 {
            player.stats.award(stat, d);
            cause_food_exhaustion(player, cost(exhaustion::SWIM, d));
        }
    };
    if flags.contains(EntityFlags::SWIMMING) {
        in_fluid(player, custom::SWIM_ONE_CM, cm(dx * dx + dy * dy + dz * dz));
    } else if fluids.eye_in_water {
        let d = cm(dx * dx + dy * dy + dz * dz);
        in_fluid(player, custom::WALK_UNDER_WATER_ONE_CM, d);
    } else if fluids.in_water() {
        in_fluid(player, custom::WALK_ON_WATER_ONE_CM, cm(dx * dx + dz * dz));
    } else if climbable {
        if dy > 0.0 {
            #[allow(clippy::cast_possible_truncation, clippy::as_conversions)]
            let up = (dy * 100.0).round() as i32;
            player.stats.award(custom::CLIMB_ONE_CM, up);
        }
    } else if mv.on_ground {
        let d = cm(dx * dx + dz * dz);
        if d > 0 {
            if flags.contains(EntityFlags::SPRINTING) {
                player.stats.award(custom::SPRINT_ONE_CM, d);
                cause_food_exhaustion(player, cost(exhaustion::SPRINT, d));
            } else if entity.as_ref().pose() == Pose::Crouching {
                player.stats.award(custom::CROUCH_ONE_CM, d);
            } else {
                player.stats.award(custom::WALK_ONE_CM, d);
            }
        }
    } else if flags.contains(EntityFlags::FALL_FLYING) {
        player
            .stats
            .award(custom::AVIATE_ONE_CM, cm(dx * dx + dy * dy + dz * dz));
    } else {
        let d = cm(dx * dx + dz * dz);
        if d > 25 {
            player.stats.award(custom::FLY_ONE_CM, d);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `calculateFallDamage` of Vanilla 26.3: `floor(distance + 1e-6 - 3)`.
    #[test]
    fn fall_damage_values() {
        assert_eq!(fall_damage(3.0, 1.0), 0);
        assert_eq!(fall_damage(3.5, 1.0), 0);
        assert_eq!(fall_damage(4.0, 1.0), 1);
        // The captured Vanilla fall: 12 blocks cost 9 health.
        assert_eq!(fall_damage(12.0, 1.0), 9);
        assert_eq!(fall_damage(23.0, 1.0), 20);
        // Hay bale.
        assert_eq!(fall_damage(23.0, 0.2), 4);
        // Slime without sneaking.
        assert_eq!(fall_damage(50.0, 0.0), 0);
    }

    #[test]
    fn experience_levels() {
        let mut xp = Experience::default();
        xp.give_points(7);
        assert_eq!((xp.level, xp.total), (1, 7));
        assert_eq!(xp.progress, 0.0);
        xp.give_points(9 + 11 + 13);
        assert_eq!(xp.level, 4);
        assert_eq!(xp.needed_for_next_level(), 15);
        let mut high = Experience {
            level: 30,
            ..Experience::default()
        };
        assert_eq!(high.needed_for_next_level(), 112);
        high.give_levels(-31);
        assert_eq!((high.level, high.total), (0, 0));
    }

    #[test]
    fn survival_nbt_round_trip() {
        let mut s = Survival {
            health: 7.5,
            ..Survival::default()
        };
        s.food.food_level = 13;
        s.air = 120;
        s.xp.level = 3;
        s.last_death = Some(("minecraft:overworld".to_owned(), BlockPos::new(1, -60, 2)));
        let mut nbt = Compound::new();
        s.save(&mut nbt);
        let back = Survival::load(&nbt);
        assert_eq!(back.health, 7.5);
        assert_eq!(back.food.food_level, 13);
        assert_eq!(back.air, 120);
        assert_eq!(back.xp.level, 3);
        assert_eq!(back.last_death, s.last_death);
    }
}
