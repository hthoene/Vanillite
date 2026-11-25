//! Raids (`Raid`, `Raids`): bad omen and raid omen, waves by difficulty, the raid bar,
//! victory, loss and the heroes of the village.
//!
//! - The raids of a dimension live in the level's entities next to the POI (mob ticks join
//!   and leave them). `Raids.tick` runs after the scheduled ticks, before the chunks
//!   (`ServerLevel.tick`).
//! - A raid knows its raiders by entity and UUID per wave (`groupRaiderMap`); a raider knows
//!   its raid by id ([`crate::mob::fam::illager::State::raid`]). Raiders loaded from disk
//!   rejoin through [`rejoin_loaded`] when their chunk becomes live.
//! - The raid's own random (`RandomSource.create()`) is seeded from the entity seed stream, so
//!   it is distribution-equal like Vanilla's.

use vanillite_base::random::{LegacyRandom, RandomSource};
use vanillite_base::{BlockPos, SectionPos, Vec3};
use vanillite_chunk::HeightmapKind;
use vanillite_nbt::{Compound, List, Tag};
use vanillite_protocol::SharedPacket;
use vanillite_protocol::packets::play::clientbound::{
    BossBarColor, BossBarOverlay, Sound, SoundSource,
};
use vanillite_protocol::text::Component;
use vanillite_world::entity::EntityId;

use crate::boss_bar::BossBar;
use crate::effect::{EffectInstance, MobEffect};
use crate::entities::Entities;
use crate::item::EquipmentSlot;
use crate::level::Level;
use crate::mob::fam::SpawnReason;
use crate::mob::fam::illager::{self, raider};
use crate::mob::spawn::{self, SpawnTable};
use crate::mob::{Environment, Kind, MobData};
use crate::survival::Effect;
use crate::survival::rules::Difficulty;

/// `Raid.VALID_RAID_RADIUS_SQR`.
pub const VALID_RAID_RADIUS_SQR: i32 = 9216;
/// `Raid.RAID_REMOVAL_THRESHOLD_SQR`.
pub const RAID_REMOVAL_THRESHOLD_SQR: f64 = 12544.0;
/// `Raid.MAX_NO_ACTION_TIME`.
pub const MAX_NO_ACTION_TIME: i32 = 2400;
/// `Raid.DEFAULT_MAX_RAID_OMEN_LEVEL`.
pub const MAX_RAID_OMEN_LEVEL: i32 = 5;
/// `Raid.MAX_CELEBRATION_TICKS`.
const MAX_CELEBRATION_TICKS: i32 = 600;
/// `Raid.HERO_OF_THE_VILLAGE_DURATION`.
const HERO_DURATION: i32 = 48000;

/// `Raid.RaidStatus`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RaidStatus {
    #[default]
    Ongoing,
    Victory,
    Loss,
    Stopped,
}

impl RaidStatus {
    fn name(self) -> &'static str {
        match self {
            RaidStatus::Ongoing => "ongoing",
            RaidStatus::Victory => "victory",
            RaidStatus::Loss => "loss",
            RaidStatus::Stopped => "stopped",
        }
    }

    fn by_name(name: &str) -> RaidStatus {
        match name {
            "victory" => RaidStatus::Victory,
            "loss" => RaidStatus::Loss,
            "stopped" => RaidStatus::Stopped,
            _ => RaidStatus::Ongoing,
        }
    }
}

/// `Raid.RaiderType`: the kind and its spawns per wave before the bonus wave.
const RAIDER_TYPES: [(Kind, [i32; 8]); 5] = [
    (Kind::Vindicator, [0, 0, 2, 0, 1, 4, 2, 5]),
    (Kind::Evoker, [0, 0, 0, 0, 0, 1, 1, 2]),
    (Kind::Pillager, [0, 4, 3, 3, 4, 4, 4, 2]),
    (Kind::Witch, [0, 0, 0, 0, 3, 0, 0, 1]),
    (Kind::Ravager, [0, 0, 0, 1, 0, 1, 0, 2]),
];

/// A raider of a raid: the live entity and its UUID (`addWaveMob` replaces a copy with the same
/// UUID).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Member {
    pub id: EntityId,
    pub uuid: u128,
}

/// `Raid`.
#[derive(Clone, Debug)]
pub struct Raid {
    pub center: BlockPos,
    pub started: bool,
    pub active: bool,
    pub ticks_active: i64,
    pub raid_omen_level: i32,
    /// `groupsSpawned`.
    pub groups_spawned: i32,
    /// `raidCooldownTicks`.
    pub raid_cooldown_ticks: i32,
    pub post_raid_ticks: i32,
    pub total_health: f32,
    /// `numGroups`.
    pub num_groups: i32,
    pub status: RaidStatus,
    pub celebration_ticks: i32,
    /// `heroesOfTheVillage`.
    pub heroes: Vec<u128>,
    /// `waveSpawnPos`.
    pub wave_spawn_pos: Option<BlockPos>,
    /// `groupRaiderMap`: wave → raiders.
    pub groups: Vec<(i32, Vec<Member>)>,
    /// `groupToLeaderMap`: wave → leader.
    pub leaders: Vec<(i32, EntityId)>,
    /// Raiders alive (`getTotalRaidersAlive`), kept in step with [`Raid::groups`].
    pub raiders_alive: i32,
    /// `raidEvent`.
    pub bar: BossBar,
    random: LegacyRandom,
}

impl Default for Raid {
    fn default() -> Self {
        Raid::new(BlockPos::ZERO, Difficulty::Normal)
    }
}

fn raid_name() -> Component {
    Component::translatable("event.minecraft.raid", Vec::new())
}

impl Raid {
    /// `new Raid(center, difficulty)`.
    pub fn new(center: BlockPos, difficulty: Difficulty) -> Raid {
        let mut random = LegacyRandom::new(crate::mob::new_entity_seed());
        let id = crate::mob::random_uuid(&mut random);
        let mut bar = BossBar::new(
            id,
            raid_name(),
            BossBarColor::Red,
            BossBarOverlay::Notched10,
        );
        bar.set_progress(0.0, &mut Vec::new());
        Raid {
            center,
            started: false,
            active: true,
            ticks_active: 0,
            raid_omen_level: 0,
            groups_spawned: 0,
            raid_cooldown_ticks: 300,
            post_raid_ticks: 0,
            total_health: 0.0,
            num_groups: num_groups(difficulty),
            status: RaidStatus::Ongoing,
            celebration_ticks: 0,
            heroes: Vec::new(),
            wave_spawn_pos: None,
            groups: Vec::new(),
            leaders: Vec::new(),
            raiders_alive: 0,
            bar,
            random,
        }
    }

    /// `isOver`.
    pub fn is_over(&self) -> bool {
        self.is_victory() || self.is_loss()
    }

    /// `isBetweenWaves`.
    pub fn is_between_waves(&self) -> bool {
        self.has_first_wave_spawned() && self.raiders_alive == 0 && self.raid_cooldown_ticks > 0
    }

    /// `hasFirstWaveSpawned`.
    pub fn has_first_wave_spawned(&self) -> bool {
        self.groups_spawned > 0
    }

    /// `isStopped`.
    pub fn is_stopped(&self) -> bool {
        self.status == RaidStatus::Stopped
    }

    /// `isVictory`.
    pub fn is_victory(&self) -> bool {
        self.status == RaidStatus::Victory
    }

    /// `isLoss`.
    pub fn is_loss(&self) -> bool {
        self.status == RaidStatus::Loss
    }

    /// `isActive`.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// `isStarted`.
    pub fn is_started(&self) -> bool {
        self.started
    }

    /// `getEnchantOdds`.
    pub fn enchant_odds(&self) -> f32 {
        match self.raid_omen_level {
            2 => 0.1,
            3 => 0.25,
            4 => 0.5,
            5 => 0.75,
            _ => 0.0,
        }
    }

    /// `getLeader(wave)`.
    pub fn leader(&self, wave: i32) -> Option<EntityId> {
        self.leaders.iter().find(|l| l.0 == wave).map(|l| l.1)
    }

    /// `removeLeader(wave)`.
    pub fn remove_leader(&mut self, wave: i32) {
        self.leaders.retain(|l| l.0 != wave);
    }

    fn put_leader(&mut self, wave: i32, id: EntityId) {
        self.remove_leader(wave);
        self.leaders.push((wave, id));
    }

    /// `addHeroOfTheVillage(killer)`.
    pub fn add_hero(&mut self, uuid: u128) {
        if !self.heroes.contains(&uuid) {
            self.heroes.push(uuid);
        }
    }

    fn recount(&mut self) {
        let n: usize = self.groups.iter().map(|g| g.1.len()).sum();
        self.raiders_alive = i32::try_from(n).unwrap_or(i32::MAX);
    }

    /// Every raider of the raid (`getAllRaiders`).
    pub fn all_raiders(&self) -> impl Iterator<Item = EntityId> + '_ {
        self.groups.iter().flat_map(|g| g.1.iter().map(|m| m.id))
    }

    /// `stop`.
    pub fn stop(&mut self, out: &mut Vec<Effect>) {
        self.active = false;
        self.bar.remove_all_players(out);
        self.status = RaidStatus::Stopped;
    }

    /// `absorbRaidOmen(player)` with the amplifier of the player's Raid Omen: whether it had one.
    pub fn absorb_raid_omen(&mut self, amplifier: Option<i32>) -> bool {
        let Some(amplifier) = amplifier else {
            return false;
        };
        self.raid_omen_level = (self.raid_omen_level + amplifier + 1).clamp(0, MAX_RAID_OMEN_LEVEL);
        true
    }

    fn has_bonus_wave(&self) -> bool {
        self.raid_omen_level > 1
    }

    fn is_final_wave(&self) -> bool {
        self.groups_spawned == self.num_groups
    }

    fn has_spawned_bonus_wave(&self) -> bool {
        self.groups_spawned > self.num_groups
    }

    fn has_more_waves(&self) -> bool {
        if self.has_bonus_wave() {
            !self.has_spawned_bonus_wave()
        } else {
            !self.is_final_wave()
        }
    }

    fn should_spawn_bonus_group(&self) -> bool {
        self.is_final_wave() && self.raiders_alive == 0 && self.has_bonus_wave()
    }

    fn should_spawn_group(&self) -> bool {
        self.raid_cooldown_ticks == 0
            && (self.groups_spawned < self.num_groups || self.should_spawn_bonus_group())
            && self.raiders_alive == 0
    }

    /// `getHealthOfLivingRaiders`.
    pub fn health_of_living_raiders(&self, ents: &Entities) -> f32 {
        self.all_raiders()
            .filter_map(|id| ents.mob(id))
            .map(|m| m.health)
            .sum()
    }

    /// `updateBossbar`.
    pub fn update_bossbar(&mut self, ents: &Entities, out: &mut Vec<Effect>) {
        let health = self.health_of_living_raiders(ents);
        let progress = (health / self.total_health).clamp(0.0, 1.0);
        // `Mth.clamp` of NaN (no health yet) gives the lower bound in Java.
        let progress = if progress.is_nan() { 0.0 } else { progress };
        self.bar.set_progress(progress, out);
    }

    /// `addWaveMob(level, wave, raider, updateHealth)`.
    pub fn add_wave_mob(
        &mut self,
        ents: &Entities,
        wave: i32,
        member: Member,
        update_health: bool,
        out: &mut Vec<Effect>,
    ) {
        let i = match self.groups.iter().position(|g| g.0 == wave) {
            Some(i) => i,
            None => {
                self.groups.push((wave, Vec::new()));
                self.groups.len() - 1
            }
        };
        let group = &mut self.groups[i].1;
        group.retain(|m| m.uuid != member.uuid && m.id != member.id);
        group.push(member);
        self.recount();
        if update_health {
            self.total_health += ents.mob(member.id).map_or(0.0, |m| m.health);
        }
        self.update_bossbar(ents, out);
    }

    /// `removeFromRaid(level, raider, removeFromTotalHealth)`: whether it was a member. The
    /// caller clears the raider's raid.
    pub fn remove_from_raid(
        &mut self,
        ents: &Entities,
        wave: i32,
        id: EntityId,
        health: f32,
        remove_from_total_health: bool,
        out: &mut Vec<Effect>,
    ) -> bool {
        let Some(group) = self.groups.iter_mut().find(|g| g.0 == wave) else {
            return false;
        };
        let before = group.1.len();
        group.1.retain(|m| m.id != id);
        if group.1.len() == before {
            return false;
        }
        self.recount();
        if remove_from_total_health {
            self.total_health -= health;
        }
        self.update_bossbar(ents, out);
        true
    }

    /// `moveRaidCenterToNearbyVillageSection`.
    fn move_center_to_village(&mut self, level: &Level) {
        let poi = &level.entities.poi;
        let c = self.center.section();
        let mut best: Option<(f64, BlockPos)> = None;
        for_each_section_in_cube(c, 2, |s| {
            if !poi.is_village_section(s) {
                return;
            }
            let center = section_center(s);
            let d = center.dist_sqr(self.center);
            if best.is_none_or(|(b, _)| d < b) {
                best = Some((d, center));
            }
        });
        if let Some((_, p)) = best {
            self.center = p;
        }
    }

    /// `findRandomSpawnPos(level, maxTries)`.
    fn find_random_spawn_pos(&mut self, level: &Level, max_tries: i32) -> Option<BlockPos> {
        let seconds = self.raid_cooldown_ticks / 20;
        #[allow(clippy::cast_precision_loss)]
        let how_far = 0.22_f32 * seconds as f32 - 0.24;
        let start_angle = self.random.next_float() * std::f32::consts::TAU;
        let far_floor = vanillite_base::math::floor(f64::from(how_far));
        for i in 0..max_tries {
            #[allow(clippy::cast_precision_loss)]
            let angle = start_angle + std::f32::consts::PI * i as f32 / 8.0;
            let cos = vanillite_base::math::cos(f64::from(angle));
            let sin = vanillite_base::math::sin(f64::from(angle));
            let x = self.center.x
                + vanillite_base::math::floor(f64::from(cos * 32.0 * how_far))
                + self.random.next_int_bounded(3) * far_floor;
            let z = self.center.z
                + vanillite_base::math::floor(f64::from(sin * 32.0 * how_far))
                + self.random.next_int_bounded(3) * far_floor;
            let y = spawn::surface_y(level, HeightmapKind::WorldSurface, x, z);
            if (y - self.center.y).abs() > 96 {
                continue;
            }
            let pos = BlockPos::new(x, y, z);
            if level.entities.poi.is_village(pos) && seconds > 7 {
                continue;
            }
            let loaded = (((x - 10) >> 4)..=((x + 10) >> 4)).all(|cx| {
                (((z - 10) >> 4)..=((z + 10) >> 4)).all(|cz| {
                    level
                        .chunks
                        .chunk(vanillite_base::ChunkPos::new(cx, cz))
                        .is_some()
                })
            });
            if loaded
                && level.chunks.in_entity_ticking_range(pos.chunk())
                && (spawn::spawn_position_ok(level, Kind::Ravager.type_id(), pos)
                    || (level.block(BlockPos::new(x, y - 1, z)).block()
                        == vanillite_data::block::ids::SNOW
                        && level.block(pos).is_air()))
            {
                return Some(pos);
            }
        }
        None
    }

    /// `getPotentialBonusSpawns`.
    fn potential_bonus_spawns(
        &mut self,
        kind: Kind,
        wave: i32,
        difficulty: Difficulty,
        bonus_wave: bool,
    ) -> i32 {
        let easy = difficulty == Difficulty::Easy;
        let normal = difficulty == Difficulty::Normal;
        let bonus = match kind {
            Kind::Vindicator | Kind::Pillager => {
                if easy {
                    self.random.next_int_bounded(2)
                } else if normal {
                    1
                } else {
                    2
                }
            }
            Kind::Witch => {
                if easy || wave <= 2 || wave == 4 {
                    return 0;
                }
                1
            }
            Kind::Ravager => i32::from(!easy && bonus_wave),
            _ => return 0,
        };
        if bonus > 0 {
            self.random.next_int_bounded(bonus + 1)
        } else {
            0
        }
    }

    /// `playSound(level, origin)`: the raid horn for the raid's players and those within 64.
    fn play_horn(&mut self, level: &Level, origin: BlockPos, out: &mut Vec<Effect>) {
        let seed = self.random.next_long();
        let raid = Vec3::new(
            f64::from(origin.x) + 0.5,
            f64::from(origin.y) + 0.5,
            f64::from(origin.z) + 0.5,
        );
        for (id, _) in level.entities.players.iter() {
            let Some(e) = level.entities.store.get(id) else {
                continue;
            };
            let p = e.pos();
            let dist = ((raid.x - p.x) * (raid.x - p.x) + (raid.z - p.z) * (raid.z - p.z)).sqrt();
            let x3 = p.x + 13.0 / dist * (raid.x - p.x);
            let z3 = p.z + 13.0 / dist * (raid.z - p.z);
            if dist <= 64.0 || self.bar.players().contains(&id) {
                let packet = SharedPacket::of(&Sound::at(
                    crate::survival::sound_event("minecraft:event.raid.horn"),
                    SoundSource::Neutral,
                    x3,
                    p.y,
                    z3,
                    64.0,
                    1.0,
                    seed,
                ));
                out.push(Effect::To { player: id, packet });
            }
        }
    }

    /// `updatePlayers`: the bar shows to the living players whose closest raid is this one.
    fn update_players(&mut self, valid: &[EntityId], out: &mut Vec<Effect>) {
        self.bar.sync_players(valid, out);
    }

    /// `getDefaultNumSpawns`.
    fn default_num_spawns(&self, spawns: &[i32; 8], wave: i32, bonus: bool) -> i32 {
        let i = if bonus { self.num_groups } else { wave };
        usize::try_from(i)
            .ok()
            .and_then(|i| spawns.get(i))
            .copied()
            .unwrap_or(0)
    }
}

/// `getNumGroups(difficulty)`.
pub fn num_groups(difficulty: Difficulty) -> i32 {
    match difficulty {
        Difficulty::Peaceful => 0,
        Difficulty::Easy => 3,
        Difficulty::Normal => 5,
        Difficulty::Hard => 7,
    }
}

/// `SectionPos.center()`.
pub fn section_center(s: SectionPos) -> BlockPos {
    BlockPos::new(s.x * 16 + 8, s.y * 16 + 8, s.z * 16 + 8)
}

/// `SectionPos.cube(center, radius)` in Vanilla's order (x fastest, then y, then z).
pub fn for_each_section_in_cube(center: SectionPos, radius: i32, mut f: impl FnMut(SectionPos)) {
    for z in center.z - radius..=center.z + radius {
        for y in center.y - radius..=center.y + radius {
            for x in center.x - radius..=center.x + radius {
                f(SectionPos::new(x, y, z));
            }
        }
    }
}

/// What a raid tick needs of the world besides the level.
pub struct RaidTick<'a> {
    pub difficulty: Difficulty,
    /// The `raids` game rule.
    pub raids_rule: bool,
    pub table: Option<&'a SpawnTable>,
    pub env: Environment,
    pub out: &'a mut Vec<Effect>,
}

impl std::fmt::Debug for RaidTick<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RaidTick")
            .field("difficulty", &self.difficulty)
            .finish_non_exhaustive()
    }
}

/// `Raids`: the raids of a dimension by id.
#[derive(Clone, Debug)]
pub struct Raids {
    /// `(id, raid)` in `raidMap` order.
    pub raids: Vec<(i32, Raid)>,
    /// `nextId`.
    pub next_id: i32,
    /// `tick`.
    pub tick: i32,
    /// Changed since the last save (`setDirty`).
    pub dirty: bool,
}

impl Default for Raids {
    fn default() -> Self {
        Raids {
            raids: Vec::new(),
            next_id: 1,
            tick: 0,
            dirty: true,
        }
    }
}

impl Raids {
    /// `getNearbyRaid(pos, maxDistSqr)`: the closest active raid.
    pub fn nearby_raid(&self, pos: BlockPos, max_dist_sqr: i32) -> Option<(i32, &Raid)> {
        let mut best: Option<(i32, &Raid)> = None;
        let mut closest = f64::from(max_dist_sqr);
        for (id, raid) in &self.raids {
            let d = raid.center.dist_sqr(pos);
            if raid.is_active() && d < closest {
                best = Some((*id, raid));
                closest = d;
            }
        }
        best
    }

    /// `ServerLevel.getRaidAt(pos)`: the closest active raid within 96 blocks.
    pub fn raid_at(&self, pos: BlockPos) -> Option<&Raid> {
        self.nearby_raid(pos, VALID_RAID_RADIUS_SQR).map(|(_, r)| r)
    }

    /// The id of [`Raids::raid_at`].
    pub fn raid_id_at(&self, pos: BlockPos) -> Option<i32> {
        self.nearby_raid(pos, VALID_RAID_RADIUS_SQR).map(|(id, _)| id)
    }

    /// `ServerLevel.isRaided(pos)`.
    pub fn is_raided(&self, pos: BlockPos) -> bool {
        self.raid_at(pos).is_some()
    }

    /// `get(id)`.
    pub fn get(&self, id: i32) -> Option<&Raid> {
        self.raids.iter().find(|r| r.0 == id).map(|r| &r.1)
    }

    /// `get(id)`, mutably.
    pub fn get_mut(&mut self, id: i32) -> Option<&mut Raid> {
        self.raids.iter_mut().find(|r| r.0 == id).map(|r| &mut r.1)
    }

    /// `getUniqueId`.
    fn unique_id(&mut self) -> i32 {
        self.next_id += 1;
        self.next_id
    }

    /// `Raids.tick` (the level's raids taken out of its entities while they tick).
    pub fn tick(level: &mut Level, cx: &mut RaidTick<'_>) {
        let mut raids = std::mem::take(&mut level.entities.raids);
        raids.tick += 1;
        let mut i = 0;
        while i < raids.raids.len() {
            if !cx.raids_rule {
                raids.raids[i].1.stop(cx.out);
            }
            if raids.raids[i].1.is_stopped() {
                raids.raids.remove(i);
                raids.dirty = true;
                continue;
            }
            let id = raids.raids[i].0;
            // The players whose closest raid is this one (`validPlayer`).
            let valid: Vec<EntityId> = level
                .entities
                .players
                .iter()
                .filter(|(pid, p)| !p.survival.is_dead_or_dying() && {
                    level.entities.store.get(*pid).is_some_and(|e| {
                        let pos = e.pos();
                        raids.raid_id_at(BlockPos::containing(pos.x, pos.y, pos.z)) == Some(id)
                    })
                })
                .map(|(pid, _)| pid)
                .collect();
            let mut raid = std::mem::take(&mut raids.raids[i].1);
            raid.tick_raid(level, id, &valid, cx, &mut raids.dirty);
            raids.raids[i].1 = raid;
            i += 1;
        }
        if raids.tick % 200 == 0 {
            raids.dirty = true;
        }
        level.entities.raids = raids;
    }

    /// `createOrExtendRaid(player, raidPosition)`: the raid omen of `player` (its amplifier) at
    /// `pos` starts or grows a raid. `can_start` is `EnvironmentAttributes.CAN_START_RAID`
    /// there. Returns the raid id.
    pub fn create_or_extend_raid(
        &mut self,
        ents: &Entities,
        player: EntityId,
        pos: BlockPos,
        amplifier: Option<i32>,
        raids_rule: bool,
        can_start: bool,
        difficulty: Difficulty,
    ) -> Option<i32> {
        let p = ents.player(player)?;
        if p.game_mode == vanillite_config::GameMode::Spectator || !raids_rule || !can_start {
            return None;
        }
        let posses = ents.poi.in_range(
            crate::poi::PoiType::is_village,
            pos,
            64,
            crate::poi::Occupancy::IsOccupied,
        );
        let mut total = Vec3::ZERO;
        for r in &posses {
            total = total
                + Vec3::new(
                    f64::from(r.pos.x),
                    f64::from(r.pos.y),
                    f64::from(r.pos.z),
                );
        }
        #[allow(clippy::cast_precision_loss)]
        let center = if posses.is_empty() {
            pos
        } else {
            let t = total * (1.0 / posses.len() as f64);
            BlockPos::containing(t.x, t.y, t.z)
        };
        let id = match self.raid_id_at(center) {
            Some(id) => id,
            None => {
                let raid = Raid::new(center, difficulty);
                let id = self.unique_id();
                self.raids.push((id, raid));
                id
            }
        };
        let raid = self.get_mut(id)?;
        if !raid.is_started() || raid.raid_omen_level < MAX_RAID_OMEN_LEVEL {
            raid.absorb_raid_omen(amplifier);
        }
        self.dirty = true;
        Some(id)
    }

    /// `Raids.CODEC` written.
    pub fn save(&self) -> Compound {
        let list: Vec<Tag> = self
            .raids
            .iter()
            .map(|(id, r)| Tag::Compound(save_raid(*id, r)))
            .collect();
        Compound::new()
            .with("raids", List(list))
            .with("next_id", self.next_id)
            .with("tick", self.tick)
    }

    /// `Raids.load(tag)`.
    pub fn load(tag: &Compound) -> Raids {
        let mut out = Raids {
            dirty: false,
            ..Raids::default()
        };
        if let Some(Tag::List(list)) = tag.get("raids") {
            for c in list.0.iter().filter_map(Tag::as_compound) {
                if let Some((id, raid)) = load_raid(c) {
                    out.raids.push((id, raid));
                }
            }
        }
        out.next_id = int(tag, "next_id").unwrap_or(1);
        out.tick = int(tag, "tick").unwrap_or(0);
        out
    }
}

fn int(tag: &Compound, key: &str) -> Option<i32> {
    match tag.get(key) {
        Some(Tag::Int(v)) => Some(*v),
        Some(Tag::Short(v)) => Some(i32::from(*v)),
        Some(Tag::Byte(v)) => Some(i32::from(*v)),
        _ => None,
    }
}

fn uuid_ints(uuid: u128) -> Tag {
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    Tag::IntArray(vec![
        (uuid >> 96) as i32,
        (uuid >> 64) as i32,
        (uuid >> 32) as i32,
        uuid as i32,
    ])
}

fn uuid_of(tag: &Tag) -> Option<u128> {
    match tag {
        #[allow(clippy::cast_sign_loss)]
        Tag::IntArray(v) if v.len() == 4 => Some(
            (u128::from(v[0] as u32) << 96)
                | (u128::from(v[1] as u32) << 64)
                | (u128::from(v[2] as u32) << 32)
                | u128::from(v[3] as u32),
        ),
        _ => None,
    }
}

fn save_raid(id: i32, r: &Raid) -> Compound {
    Compound::new()
        .with("id", id)
        .with("started", r.started)
        .with("active", r.active)
        .with("ticks_active", r.ticks_active)
        .with("raid_omen_level", r.raid_omen_level)
        .with("groups_spawned", r.groups_spawned)
        .with("cooldown_ticks", r.raid_cooldown_ticks)
        .with("post_raid_ticks", r.post_raid_ticks)
        .with("total_health", r.total_health)
        .with("group_count", r.num_groups)
        .with("status", r.status.name())
        .with(
            "center",
            Tag::IntArray(vec![r.center.x, r.center.y, r.center.z]),
        )
        .with(
            "heroes_of_the_village",
            List(r.heroes.iter().map(|&u| uuid_ints(u)).collect()),
        )
}

fn load_raid(c: &Compound) -> Option<(i32, Raid)> {
    let id = int(c, "id")?;
    let boolean = |k: &str| matches!(c.get(k), Some(Tag::Byte(b)) if *b != 0);
    let center = match c.get("center") {
        Some(Tag::IntArray(v)) if v.len() == 3 => BlockPos::new(v[0], v[1], v[2]),
        _ => return None,
    };
    let mut raid = Raid::new(center, Difficulty::Normal);
    raid.started = boolean("started");
    raid.active = boolean("active");
    raid.ticks_active = match c.get("ticks_active") {
        Some(Tag::Long(v)) => *v,
        Some(Tag::Int(v)) => i64::from(*v),
        _ => 0,
    };
    raid.raid_omen_level = int(c, "raid_omen_level").unwrap_or(0);
    raid.groups_spawned = int(c, "groups_spawned").unwrap_or(0);
    raid.raid_cooldown_ticks = int(c, "cooldown_ticks").unwrap_or(0);
    raid.post_raid_ticks = int(c, "post_raid_ticks").unwrap_or(0);
    raid.total_health = match c.get("total_health") {
        Some(Tag::Float(v)) => *v,
        Some(Tag::Double(v)) => *v as f32,
        _ => 0.0,
    };
    raid.num_groups = int(c, "group_count").unwrap_or(0);
    raid.status = RaidStatus::by_name(c.get("status").and_then(Tag::as_str).unwrap_or(""));
    if let Some(Tag::List(list)) = c.get("heroes_of_the_village") {
        raid.heroes = list.0.iter().filter_map(uuid_of).collect();
    }
    Some((id, raid))
}

// --- The raid tick -------------------------------------------------------------------------

impl Raid {
    /// `Raid.tick(level)`.
    fn tick_raid(
        &mut self,
        level: &mut Level,
        raid_id: i32,
        valid: &[EntityId],
        cx: &mut RaidTick<'_>,
        dirty: &mut bool,
    ) {
        if self.is_stopped() {
            return;
        }
        if self.status == RaidStatus::Ongoing {
            let old_active = self.active;
            self.active = level.is_loaded(self.center);
            if cx.difficulty == Difficulty::Peaceful {
                self.stop(cx.out);
                return;
            }
            if old_active != self.active {
                self.bar.set_visible(self.active, cx.out);
            }
            if !self.active {
                return;
            }
            if !level.entities.poi.is_village(self.center) {
                self.move_center_to_village(level);
            }
            if !level.entities.poi.is_village(self.center) {
                if self.groups_spawned > 0 {
                    self.status = RaidStatus::Loss;
                } else {
                    self.stop(cx.out);
                }
            }
            self.ticks_active += 1;
            if self.ticks_active >= 48000 {
                self.stop(cx.out);
                return;
            }
            let alive = self.raiders_alive;
            if alive == 0 && self.has_more_waves() {
                if self.raid_cooldown_ticks <= 0 {
                    if self.raid_cooldown_ticks == 0 && self.groups_spawned > 0 {
                        self.raid_cooldown_ticks = 300;
                        self.bar.set_name(raid_name(), cx.out);
                        return;
                    }
                } else {
                    let cached = self.wave_spawn_pos.is_some();
                    let mut find = !cached && self.raid_cooldown_ticks % 5 == 0;
                    if let Some(p) = self.wave_spawn_pos
                        && !level.chunks.in_entity_ticking_range(p.chunk())
                    {
                        find = true;
                    }
                    if find {
                        self.wave_spawn_pos = self.find_random_spawn_pos(level, 8);
                    }
                    if self.raid_cooldown_ticks == 300 || self.raid_cooldown_ticks % 20 == 0 {
                        self.update_players(valid, cx.out);
                    }
                    self.raid_cooldown_ticks -= 1;
                    #[allow(clippy::cast_precision_loss)]
                    let progress =
                        ((300 - self.raid_cooldown_ticks) as f32 / 300.0).clamp(0.0, 1.0);
                    self.bar.set_progress(progress, cx.out);
                }
            }
            if self.ticks_active % 20 == 0 {
                self.update_players(valid, cx.out);
                self.update_raiders(level, raid_id, cx.out);
                let name = if alive > 0 && alive <= 2 {
                    raid_name()
                        .append(Component::text(" - "))
                        .append(Component::translatable_args(
                            "event.minecraft.raid.raiders_remaining",
                            vec![vanillite_protocol::text::Arg::Int(alive)],
                        ))
                } else {
                    raid_name()
                };
                self.bar.set_name(name, cx.out);
            }
            let mut sound_played = false;
            let mut attempt = 0;
            while self.should_spawn_group() {
                let pos = match self.wave_spawn_pos {
                    Some(p) => Some(p),
                    None => self.find_random_spawn_pos(level, 20),
                };
                if let Some(pos) = pos {
                    self.started = true;
                    self.spawn_group(level, raid_id, pos, cx);
                    if !sound_played {
                        self.play_horn(level, pos, cx.out);
                        sound_played = true;
                    }
                } else {
                    attempt += 1;
                }
                if attempt > 5 {
                    self.stop(cx.out);
                    break;
                }
            }
            if self.is_started() && !self.has_more_waves() && alive == 0 {
                if self.post_raid_ticks < 40 {
                    self.post_raid_ticks += 1;
                } else {
                    self.status = RaidStatus::Victory;
                    for &hero in &self.heroes {
                        let Some(&pid) = level.entities.players_by_uuid.get(&hero) else {
                            continue;
                        };
                        let spectator = level.entities.player(pid).is_some_and(|p| {
                            p.game_mode == vanillite_config::GameMode::Spectator
                        });
                        if spectator {
                            continue;
                        }
                        crate::effect::living::add_detached(
                            &mut level.entities,
                            pid,
                            EffectInstance::with_flags(
                                MobEffect::HERO_OF_THE_VILLAGE,
                                HERO_DURATION,
                                self.raid_omen_level - 1,
                                false,
                                false,
                                true,
                            ),
                        );
                    }
                }
            }
            *dirty = true;
        } else if self.is_over() {
            self.celebration_ticks += 1;
            if self.celebration_ticks >= MAX_CELEBRATION_TICKS {
                self.stop(cx.out);
                return;
            }
            if self.celebration_ticks % 20 == 0 {
                self.update_players(valid, cx.out);
                self.bar.set_visible(true, cx.out);
                if self.is_victory() {
                    self.bar.set_progress(0.0, cx.out);
                    self.bar.set_name(
                        Component::translatable("event.minecraft.raid.victory.full", Vec::new()),
                        cx.out,
                    );
                } else {
                    self.bar.set_name(
                        Component::translatable("event.minecraft.raid.defeat.full", Vec::new()),
                        cx.out,
                    );
                }
            }
        }
    }

    /// `updateRaiders`: raiders that left, unloaded or idled outside the village leave the raid.
    fn update_raiders(&mut self, level: &mut Level, raid_id: i32, out: &mut Vec<Effect>) {
        let mut remove: Vec<(i32, EntityId)> = Vec::new();
        for (wave, members) in &self.groups {
            for m in members {
                let ents = &level.entities;
                let Some(mob) = ents.mob(m.id).filter(|mob| !mob.removed) else {
                    remove.push((*wave, m.id));
                    continue;
                };
                let Some(pos) = ents.store.get(m.id).map(|e| e.pos()) else {
                    remove.push((*wave, m.id));
                    continue;
                };
                let bp = BlockPos::containing(pos.x, pos.y, pos.z);
                if self.center.dist_sqr(bp) >= RAID_REMOVAL_THRESHOLD_SQR {
                    remove.push((*wave, m.id));
                } else if mob.tick_count > 600 {
                    let no_action = mob.no_action_time;
                    let outside = !ents.poi.is_village(bp) && no_action > MAX_NO_ACTION_TIME;
                    if let Some(mob) = level.entities.mob_mut(m.id) {
                        let s = illager::st_mut(mob);
                        if outside {
                            s.ticks_outside_raid += 1;
                        }
                        if s.ticks_outside_raid >= 30 {
                            remove.push((*wave, m.id));
                        }
                    }
                }
            }
        }
        for (wave, id) in remove {
            let health = level.entities.mob(id).map_or(0.0, |m| m.health);
            self.remove_from_raid(&level.entities, wave, id, health, true, out);
            if let Some(mob) = level.entities.mob_mut(id) {
                let s = illager::st_mut(mob);
                if s.raid == Some(raid_id) {
                    s.raid = None;
                }
                if s.patrol_leader {
                    self.remove_leader(s.wave);
                }
            }
        }
    }

    /// `spawnGroup(level, pos)`.
    fn spawn_group(&mut self, level: &mut Level, raid_id: i32, pos: BlockPos, cx: &mut RaidTick<'_>) {
        let mut leader_set = false;
        let wave = self.groups_spawned + 1;
        self.total_health = 0.0;
        let bonus = self.should_spawn_bonus_group();
        for (kind, spawns) in RAIDER_TYPES {
            let n = self.default_num_spawns(&spawns, wave, bonus)
                + self.potential_bonus_spawns(kind, wave, cx.difficulty, bonus);
            let mut ravagers = 0;
            for _ in 0..n {
                let mut mob = MobData::new(kind, crate::mob::new_entity_seed());
                if !leader_set && can_be_leader(kind) {
                    illager::st_mut(&mut mob).patrol_leader = true;
                    set_leader_banner(&mut mob);
                    leader_set = true;
                }
                let leader = illager::st(&mob).patrol_leader;
                let id = self.join_new(level, raid_id, wave, mob, pos, cx);
                if leader && let Some(id) = id {
                    self.put_leader(wave, id);
                }
                if kind == Kind::Ravager {
                    let rider = if wave == num_groups(Difficulty::Normal) {
                        Some(Kind::Pillager)
                    } else if wave >= num_groups(Difficulty::Hard) {
                        Some(if ravagers == 0 {
                            Kind::Evoker
                        } else {
                            Kind::Vindicator
                        })
                    } else {
                        None
                    };
                    ravagers += 1;
                    if let (Some(rk), Some(vehicle)) = (rider, id) {
                        let mob = MobData::new(rk, crate::mob::new_entity_seed());
                        if let Some(rider) = self.join_new(level, raid_id, wave, mob, pos, cx) {
                            let at = Vec3::new(f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
                            if let Some(mut e) = level.entities.store.get_mut(rider) {
                                e.set_pos(at);
                            }
                            level.entities.riding.add(rider, vehicle, false);
                        }
                    }
                }
            }
        }
        self.wave_spawn_pos = None;
        self.groups_spawned += 1;
        self.update_bossbar(&level.entities, cx.out);
    }

    /// `joinRaid(level, wave, raider, pos, exists = false)` for a new raider: placed above
    /// `pos`, finalized with `EVENT`, buffed and added to the world.
    fn join_new(
        &mut self,
        level: &mut Level,
        raid_id: i32,
        wave: i32,
        mut mob: MobData,
        pos: BlockPos,
        cx: &mut RaidTick<'_>,
    ) -> Option<EntityId> {
        {
            let s = illager::st_mut(&mut mob);
            s.raid = Some(raid_id);
            s.wave = wave;
            s.can_join_raid = true;
            s.ticks_outside_raid = 0;
        }
        let at = Vec3::new(
            f64::from(pos.x) + 0.5,
            f64::from(pos.y) + 1.0,
            f64::from(pos.z) + 0.5,
        );
        let extra = spawn::with_level_random(level, |level, random| {
            spawn::finalize(
                level,
                cx.table,
                &cx.env,
                &mut mob,
                pos,
                cx.difficulty,
                0,
                SpawnReason::Event,
                random,
            )
        });
        apply_raid_buffs(&mut mob, self, wave);
        let id = spawn::add_mob_with(level, mob, at, 0.0, extra);
        if let Some(mut e) = level.entities.store.get_mut(id) {
            e.set_flags(vanillite_world::entity::EntityFlags::ON_GROUND, true);
        }
        let uuid = level.entities.store.get(id).map_or(0, |e| e.uuid());
        self.add_wave_mob(&level.entities, wave, Member { id, uuid }, true, cx.out);
        Some(id)
    }
}

/// `canBeLeader`: witches and ravagers never lead.
pub fn can_be_leader(kind: Kind) -> bool {
    !matches!(kind, Kind::Witch | Kind::Ravager)
}

/// `Raid.setLeader`'s banner: the ominous banner on the head, always dropped.
fn set_leader_banner(m: &mut MobData) {
    m.equipment
        .set(EquipmentSlot::Head, raider::ominous_banner());
    m.equipment.set_drop_chance(EquipmentSlot::Head, 2.0);
}

/// `applyRaidBuffs(level, wave, isCaptain)`: pillagers may get a quick charge crossbow,
/// vindicators an iron axe (enchanted with sharpness by chance).
pub fn apply_raid_buffs(m: &mut MobData, raid: &Raid, wave: i32) {
    match m.kind {
        Kind::Pillager => {
            let enchant = m.random.next_float() <= raid.enchant_odds();
            if enchant {
                let level = if wave > num_groups(Difficulty::Normal) {
                    Some(2)
                } else if wave > num_groups(Difficulty::Easy) {
                    Some(1)
                } else {
                    None
                };
                if let Some(level) = level {
                    let mut crossbow = crate::mob::equipment::stack("minecraft:crossbow");
                    enchant_single(&mut crossbow, "minecraft:quick_charge", level);
                    m.equipment.set(EquipmentSlot::MainHand, crossbow);
                }
            }
        }
        Kind::Vindicator => {
            let mut axe = crate::mob::equipment::stack("minecraft:iron_axe");
            let enchant = m.random.next_float() <= raid.enchant_odds();
            if enchant {
                let level = if wave > num_groups(Difficulty::Normal) {
                    2
                } else {
                    1
                };
                enchant_single(&mut axe, "minecraft:sharpness", level);
            }
            m.equipment.set(EquipmentSlot::MainHand, axe);
        }
        _ => {}
    }
}

/// `SingleEnchantment.enchant` (`raid/*` enchantment providers): `upgrade(enchantment, level)`.
fn enchant_single(stack: &mut crate::item::ItemStack, name: &str, level: i32) {
    let Some(data) = crate::player::play::game_data() else {
        return;
    };
    let Some(id) = (0..512u32).find(|&i| data.enchanting.get(i).is_some_and(|e| e.name == name))
    else {
        return;
    };
    let mut list = crate::item::enchantments(stack);
    match list.iter_mut().find(|e| e.0 == id) {
        Some(e) => e.1 = e.1.max(level),
        None => list.push((id, level)),
    }
    stack.patch_mut().set(
        vanillite_protocol::components::ids::ENCHANTMENTS,
        crate::enchant::write(&list),
    );
}

// --- Raiders -----------------------------------------------------------------------------

/// `Raids.canJoinRaid(raider)`.
pub fn can_join_raid(m: &MobData) -> bool {
    m.is_alive() && illager::st(m).can_join_raid && m.no_action_time <= MAX_NO_ACTION_TIME
}

/// `raid.joinRaid(level, wave, raider, null, exists = true)` of a living raider.
pub fn join_existing(ents: &mut Entities, raid_id: i32, wave: i32, id: EntityId, out: &mut Vec<Effect>) {
    let uuid = ents.store.get(id).map_or(0, |e| e.uuid());
    let mut raids = std::mem::take(&mut ents.raids);
    if let Some(raid) = raids.get_mut(raid_id) {
        raid.add_wave_mob(ents, wave, Member { id, uuid }, true, out);
        raids.dirty = true;
        if let Some(m) = ents.mob_mut(id) {
            let s = illager::st_mut(m);
            s.raid = Some(raid_id);
            s.wave = wave;
            s.can_join_raid = true;
            s.ticks_outside_raid = 0;
        }
    }
    ents.raids = raids;
}

/// `Raider.hasActiveRaid`.
pub fn has_active_raid(ents: &Entities, m: &MobData) -> bool {
    illager::st(m)
        .raid
        .and_then(|id| ents.raids.get(id))
        .is_some_and(Raid::is_active)
}

/// The raid of a raider (`getCurrentRaid`).
pub fn current_raid<'a>(ents: &'a Entities, m: &MobData) -> Option<&'a Raid> {
    illager::st(m).raid.and_then(|id| ents.raids.get(id))
}

/// `Raider.die`: a leader leaves its wave, a player killer becomes a hero, the raider leaves
/// the raid.
pub fn on_raider_death(ents: &mut Entities, id: EntityId, killer: Option<EntityId>, out: &mut Vec<Effect>) {
    let Some(m) = ents.mob(id) else {
        return;
    };
    let Some(raid_id) = illager::st(m).raid else {
        return;
    };
    let (wave, leader, health) = (illager::st(m).wave, illager::st(m).patrol_leader, m.health);
    let hero = killer
        .filter(|&k| ents.player(k).is_some())
        .and_then(|k| ents.store.get(k).map(|e| e.uuid()));
    let mut raids = std::mem::take(&mut ents.raids);
    if let Some(raid) = raids.get_mut(raid_id) {
        if leader {
            raid.remove_leader(wave);
        }
        if let Some(h) = hero {
            raid.add_hero(h);
        }
        if raid.remove_from_raid(ents, wave, id, health, false, out) {
            raids.dirty = true;
            if let Some(m) = ents.mob_mut(id) {
                illager::st_mut(m).raid = None;
            }
        }
    }
    ents.raids = raids;
}

/// `Raider.hurtServer`: the raid bar before the damage.
pub fn on_raider_hurt(ents: &mut Entities, id: EntityId, out: &mut Vec<Effect>) {
    let Some(raid_id) = ents.mob(id).and_then(|m| illager::st(m).raid) else {
        return;
    };
    let mut raids = std::mem::take(&mut ents.raids);
    if let Some(raid) = raids.get_mut(raid_id)
        && raid.is_active()
    {
        raid.update_bossbar(ents, out);
    }
    ents.raids = raids;
}

/// `Raider.readAdditionalSaveData` with a `RaidId`: a raider loaded with its chunk rejoins its
/// raid (without adding to the total health) and leads its wave again.
pub fn rejoin_loaded(ents: &mut Entities, id: EntityId) {
    let Some(m) = ents.mob(id) else {
        return;
    };
    if !illager::is_raider(m.kind) {
        return;
    }
    let Some(raid_id) = illager::st(m).raid else {
        return;
    };
    let (wave, leader) = (illager::st(m).wave, illager::st(m).patrol_leader);
    let uuid = ents.store.get(id).map_or(0, |e| e.uuid());
    let mut raids = std::mem::take(&mut ents.raids);
    match raids.get_mut(raid_id) {
        Some(raid) => {
            raid.add_wave_mob(ents, wave, Member { id, uuid }, false, &mut Vec::new());
            if leader {
                raid.put_leader(wave, id);
            }
        }
        None => {
            if let Some(m) = ents.mob_mut(id) {
                illager::st_mut(m).raid = None;
            }
        }
    }
    ents.raids = raids;
}

// --- Omens -------------------------------------------------------------------------------

/// What the omen effects of a player know of the village and raids at its position, set by
/// the world before the player ticks (the effects tick without the level).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OmenView {
    /// `level.isVillage(player.blockPosition())`.
    pub in_village: bool,
    /// A raid at the position already has the maximum raid omen level.
    pub raid_full: bool,
}

/// The raid omen state of a player (`ServerPlayer.raidOmenPosition`) and the raid it starts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlayerOmen {
    /// `raidOmenPosition` (stored as `raid_omen_position`).
    pub position: Option<BlockPos>,
    /// This tick's view of the village (see [`OmenView`]).
    pub view: OmenView,
    /// A Raid Omen ran out at this position: the world starts or extends the raid.
    pub start_raid: Option<BlockPos>,
    /// The Raid Omen amplifier the raid absorbs (the effect is gone by then).
    pub start_amplifier: i32,
}

/// `ServerLevel.getRaids().createOrExtendRaid` for the players whose Raid Omen ran out this
/// tick (`RaidOmenMobEffect.applyEffectTick`), after their effects ticked.
pub fn start_pending_raids(
    ents: &mut Entities,
    raids_rule: bool,
    can_start: bool,
    difficulty: Difficulty,
) {
    let pending: Vec<(EntityId, BlockPos, i32)> = ents
        .players
        .iter_mut()
        .filter_map(|(id, p)| {
            let pos = p.survival.raid_omen.start_raid.take()?;
            Some((id, pos, p.survival.raid_omen.start_amplifier))
        })
        .collect();
    for (player, pos, amplifier) in pending {
        let mut raids = std::mem::take(&mut ents.raids);
        raids.create_or_extend_raid(
            ents,
            player,
            pos,
            Some(amplifier),
            raids_rule,
            can_start,
            difficulty,
        );
        ents.raids = raids;
    }
}

/// Fills the [`OmenView`] of every player with bad omen or raid omen.
pub fn update_omen_views(ents: &mut Entities) {
    let Entities {
        store,
        players,
        poi,
        raids,
        ..
    } = ents;
    for (id, p) in players.iter_mut() {
        if !p.survival.effects.has(MobEffect::BAD_OMEN) {
            continue;
        }
        let Some(pos) = store.get(id).map(|e| e.pos()) else {
            continue;
        };
        let bp = BlockPos::containing(pos.x, pos.y, pos.z);
        p.survival.raid_omen.view = OmenView {
            in_village: poi.is_village(bp),
            raid_full: raids
                .raid_at(bp)
                .is_some_and(|r| r.raid_omen_level >= MAX_RAID_OMEN_LEVEL),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_by_difficulty_and_bonus_wave() {
        assert_eq!(num_groups(Difficulty::Easy), 3);
        assert_eq!(num_groups(Difficulty::Hard), 7);
        let mut r = Raid::new(BlockPos::ZERO, Difficulty::Easy);
        assert!(r.has_more_waves());
        r.groups_spawned = 3;
        assert!(!r.has_more_waves());
        r.raid_omen_level = 2;
        assert!(r.has_more_waves());
        assert!(r.should_spawn_bonus_group());
        r.groups_spawned = 4;
        assert!(!r.has_more_waves());
    }

    #[test]
    fn raid_omen_absorbs_up_to_five() {
        let mut r = Raid::new(BlockPos::ZERO, Difficulty::Normal);
        assert!(!r.absorb_raid_omen(None));
        assert!(r.absorb_raid_omen(Some(3)));
        assert_eq!(r.raid_omen_level, 4);
        r.absorb_raid_omen(Some(3));
        assert_eq!(r.raid_omen_level, 5);
        assert!((r.enchant_odds() - 0.75).abs() < 1e-6);
    }

    #[test]
    fn saved_data_round_trip() {
        let mut raids = Raids::default();
        let mut r = Raid::new(BlockPos::new(1, 64, -3), Difficulty::Hard);
        r.status = RaidStatus::Victory;
        r.heroes.push(0x1234_5678_9abc_def0_0fed_cba9_8765_4321);
        r.total_health = 12.5;
        raids.raids.push((2, r));
        raids.next_id = 2;
        let saved = raids.save();
        let back = Raids::load(&saved);
        assert_eq!(back.save(), saved);
        assert_eq!(back.get(2).map(|r| r.num_groups), Some(7));
    }

    #[test]
    fn nearby_raid_needs_active_within_range() {
        let mut raids = Raids::default();
        raids.raids.push((2, Raid::new(BlockPos::new(0, 64, 0), Difficulty::Normal)));
        assert!(raids.raid_at(BlockPos::new(50, 64, 0)).is_some());
        assert!(raids.raid_at(BlockPos::new(97, 64, 0)).is_none());
        raids.raids[0].1.active = false;
        assert!(!raids.is_raided(BlockPos::new(0, 64, 0)));
    }
}
