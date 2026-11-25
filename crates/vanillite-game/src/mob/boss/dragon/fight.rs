//! The End's dragon fight (`EnderDragonFight`, saved as `minecraft:ender_dragon_fight`): the
//! boss bar, finding or spawning the dragon, counting the pillars' crystals, the exit portal,
//! the egg, the gateways after each kill and the respawn with four crystals
//! (`DragonRespawnStage`).

use vanillite_base::random::{LegacyRandom, RandomSource};
use vanillite_base::{Aabb, BlockPos, ChunkPos, Vec3};
use vanillite_chunk::HeightmapKind;
use vanillite_data::block::ids;
use vanillite_nbt::{Compound, List, Tag};
use vanillite_protocol::packets::play::clientbound::{BossBarColor, BossBarFlags, BossBarOverlay};
use vanillite_protocol::text::Component;
use vanillite_world::chunks::TicketId;
use vanillite_world::entity::{EntityId, Rotation};
use vanillite_worldgen::feature::kinds::end::{EndSpike, spikes_for_seed};

use super::super::special::SpecialData;
use super::{height, live};
use crate::boss_bar::BossBar;
use crate::level::Level;
use crate::level::explosion::BlockInteraction;
use crate::mob::{Kind, MobContext, random_uuid};
use crate::survival::damage::DamageSource;
use crate::survival::player::Env;
use crate::survival::rules::Rule;

/// What the dragon reads of its fight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FightView {
    pub dragon_uuid: Option<u128>,
    pub alive_crystals: i32,
    pub previously_killed: bool,
}

/// What the dragon and the crystals tell the fight (handled right after their tick).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DragonEvent {
    /// `updateDragon`: the bar's progress, the dragon was seen.
    Update { uuid: u128, progress: f32 },
    /// `setDragonKilled`.
    Killed { uuid: u128 },
    /// `onCrystalDestroyed`.
    CrystalDestroyed {
        crystal: EntityId,
        uuid: u128,
        pos: BlockPos,
        source: DamageSource,
    },
}

/// `DragonRespawnStage`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RespawnStage {
    Start,
    PreparingToSummonPillars,
    SummoningPillars,
    SummoningDragon,
    End,
}

impl RespawnStage {
    fn name(self) -> &'static str {
        match self {
            RespawnStage::Start => "start",
            RespawnStage::PreparingToSummonPillars => "preparing_to_summon_pillars",
            RespawnStage::SummoningPillars => "summoning_pillars",
            RespawnStage::SummoningDragon => "summoning_dragon",
            RespawnStage::End => "end",
        }
    }

    fn by_name(name: &str) -> Option<RespawnStage> {
        [
            RespawnStage::Start,
            RespawnStage::PreparingToSummonPillars,
            RespawnStage::SummoningPillars,
            RespawnStage::SummoningDragon,
            RespawnStage::End,
        ]
        .into_iter()
        .find(|s| s.name() == name)
    }
}

/// The dragon's spawn and the respawn beams' target.
const SPAWN: BlockPos = BlockPos::new(0, 128, 0);

/// The fight of an End world.
#[derive(Debug)]
pub struct DragonFight {
    // Saved.
    pub needs_state_scanning: bool,
    pub dragon_killed: bool,
    pub previously_killed: bool,
    pub respawn_stage: Option<RespawnStage>,
    pub respawn_time: i32,
    pub dragon_uuid: Option<u128>,
    pub exit_portal: Option<BlockPos>,
    pub gateways: Vec<i32>,
    pub respawn_crystals: Vec<u128>,
    // Runtime.
    origin: BlockPos,
    seed: i64,
    bar: BossBar,
    ticks_since_dragon_seen: i32,
    alive_crystals: i32,
    ticks_since_crystals_scanned: i32,
    ticks_since_last_player_scan: i32,
    ticket: Option<TicketId>,
    dirty: bool,
}

/// Vanilla's `Util.shuffle(list, random)`.
fn shuffle(list: &mut [i32], random: &mut LegacyRandom) {
    for i in (2..=list.len()).rev() {
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        let swap = random.next_int_bounded(i as i32) as usize;
        list.swap(i - 1, swap);
    }
}

/// The gateway order of a world seed (`init`: 0..20 shuffled with the seed).
pub fn gateway_order(seed: i64) -> Vec<i32> {
    let mut g: Vec<i32> = (0..20).collect();
    shuffle(&mut g, &mut LegacyRandom::new(seed));
    g
}

/// Where gateway `index` goes (`spawnNewGateway`): on a circle of 96 at y 75.
pub fn gateway_pos(index: i32) -> BlockPos {
    let a = 2.0 * (-std::f64::consts::PI + std::f64::consts::PI / 20.0 * f64::from(index));
    BlockPos::new(
        vanillite_base::math::floor(96.0 * a.cos()),
        75,
        vanillite_base::math::floor(96.0 * a.sin()),
    )
}

fn uuid_tag(uuid: u128) -> Tag {
    #[allow(clippy::cast_possible_truncation)]
    Tag::IntArray(vec![
        (uuid >> 96) as i32,
        (uuid >> 64) as i32,
        (uuid >> 32) as i32,
        uuid as i32,
    ])
}

fn uuid_of(tag: &Tag) -> Option<u128> {
    match tag {
        Tag::IntArray(a) if a.len() == 4 => Some(
            a.iter()
                .fold(0_u128, |acc, &v| (acc << 32) | u128::from(v as u32)),
        ),
        _ => None,
    }
}

impl DragonFight {
    /// The saved data id.
    pub const ID: &'static str = "minecraft:ender_dragon_fight";

    /// The fight from its saved data (`CODEC`, defaults for a new world) and `init`.
    pub fn new(saved: Option<&Compound>, seed: i64, bar_random: &mut LegacyRandom) -> DragonFight {
        let flag = |k: &str, d: bool| {
            saved
                .and_then(|c| c.get(k))
                .and_then(Tag::as_i64)
                .map_or(d, |v| v != 0)
        };
        let get = |k: &str| saved.and_then(|c| c.get(k));
        let pos = |t: Option<&Tag>| match t {
            Some(Tag::IntArray(a)) if a.len() == 3 => Some(BlockPos::new(a[0], a[1], a[2])),
            _ => None,
        };
        let mut gateways: Vec<i32> = match get("gateways") {
            Some(Tag::List(l)) => {
                l.0.iter()
                    .filter_map(|t| t.as_i64().and_then(|v| i32::try_from(v).ok()))
                    .collect()
            }
            Some(Tag::IntArray(a)) => a.clone(),
            _ => Vec::new(),
        };
        let mut dirty = false;
        if gateways.is_empty() {
            gateways = gateway_order(seed);
            dirty = true;
        }
        let respawn_crystals = match get("respawn_crystals") {
            Some(Tag::List(l)) => l.0.iter().filter_map(uuid_of).collect(),
            _ => Vec::new(),
        };
        let bar_uuid = random_uuid(bar_random);
        let bar = BossBar::new(
            bar_uuid,
            Component::translatable("entity.minecraft.ender_dragon", Vec::new()),
            BossBarColor::Pink,
            BossBarOverlay::Progress,
        )
        .with_flags(BossBarFlags {
            darken_screen: false,
            play_music: true,
            create_fog: true,
        });
        DragonFight {
            needs_state_scanning: flag("needs_state_scanning", true),
            dragon_killed: flag("dragon_killed", false),
            previously_killed: flag("previously_killed", false),
            respawn_stage: match get("respawn_stage") {
                Some(Tag::String(s)) => RespawnStage::by_name(s),
                _ => None,
            },
            respawn_time: get("respawn_time")
                .and_then(Tag::as_i64)
                .and_then(|v| i32::try_from(v).ok())
                .unwrap_or(0),
            dragon_uuid: get("dragon_uuid").and_then(uuid_of),
            exit_portal: pos(get("exit_portal_location")),
            gateways,
            respawn_crystals,
            origin: BlockPos::new(0, 0, 0),
            seed,
            bar,
            ticks_since_dragon_seen: 0,
            alive_crystals: 0,
            ticks_since_crystals_scanned: 0,
            ticks_since_last_player_scan: 21,
            ticket: None,
            dirty,
        }
    }

    /// The saved data (`CODEC`).
    pub fn save(&self) -> Compound {
        let mut c = Compound::new();
        c.insert(
            "needs_state_scanning",
            Tag::Byte(i8::from(self.needs_state_scanning)),
        );
        c.insert("dragon_killed", Tag::Byte(i8::from(self.dragon_killed)));
        c.insert(
            "previously_killed",
            Tag::Byte(i8::from(self.previously_killed)),
        );
        if let Some(s) = self.respawn_stage {
            c.insert("respawn_stage", Tag::String(s.name().into()));
        }
        c.insert("respawn_time", Tag::Int(self.respawn_time));
        if let Some(u) = self.dragon_uuid {
            c.insert("dragon_uuid", uuid_tag(u));
        }
        if let Some(p) = self.exit_portal {
            c.insert("exit_portal_location", Tag::IntArray(vec![p.x, p.y, p.z]));
        }
        c.insert(
            "gateways",
            Tag::List(List(self.gateways.iter().map(|&g| Tag::Int(g)).collect())),
        );
        if !self.respawn_crystals.is_empty() {
            c.insert(
                "respawn_crystals",
                Tag::List(List(
                    self.respawn_crystals.iter().map(|&u| uuid_tag(u)).collect(),
                )),
            );
        }
        c
    }

    /// Whether the saved data changed since the last [`take_dirty`](Self::take_dirty).
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// What the dragon reads.
    pub fn view(&self) -> FightView {
        FightView {
            dragon_uuid: self.dragon_uuid,
            alive_crystals: self.alive_crystals,
            previously_killed: self.previously_killed,
        }
    }

    /// The boss bar.
    pub fn bar(&self) -> &BossBar {
        &self.bar
    }

    /// `EnderDragonFight.tick`.
    pub fn tick(
        &mut self,
        level: &mut Level,
        env: &mut Env<'_>,
        cx: &mut MobContext,
    ) {
        self.bar.set_visible(!self.dragon_killed, env.out);
        self.ticks_since_last_player_scan += 1;
        if self.ticks_since_last_player_scan >= 20 {
            self.update_players(level, env);
            self.ticks_since_last_player_scan = 0;
        }
        if self.bar.players().is_empty() {
            if let Some(t) = self.ticket.take() {
                level.chunks.remove_ticket(t);
            }
            return;
        }
        if self.ticket.is_none() {
            // `addTicketWithRadius(DRAGON, (0, 0), 9)`.
            self.ticket = Some(level.chunks.add_ticket(ChunkPos::new(0, 0), 33 - 9));
        }
        if !self.arena_loaded(level) {
            return;
        }
        if self.needs_state_scanning {
            self.scan_state(level);
            self.needs_state_scanning = false;
            self.dirty = true;
        }
        if self.respawn_stage.is_some() && !self.tick_respawn(level, env, cx) {
            return;
        }
        if !self.dragon_killed {
            let find = if self.dragon_uuid.is_none() {
                true
            } else {
                self.ticks_since_dragon_seen += 1;
                self.ticks_since_dragon_seen >= 1200
            };
            if find {
                self.find_or_create_dragon(level, env);
                self.ticks_since_dragon_seen = 0;
            }
            self.ticks_since_crystals_scanned += 1;
            if self.ticks_since_crystals_scanned >= 100 {
                self.update_crystal_count(level);
                self.ticks_since_crystals_scanned = 0;
            }
        }
    }

    /// One tick of the respawn sequence; `false` when it was aborted (no crystal left).
    pub(crate) fn tick_respawn(
        &mut self,
        level: &mut Level,
        env: &mut Env<'_>,
        cx: &mut MobContext,
    ) -> bool {
        let Some(stage) = self.respawn_stage else {
            return true;
        };
        let crystals = self.resolve_respawn_crystals(level);
        if crystals.is_empty() {
            self.abort_respawn(level);
            return false;
        }
        let time = self.respawn_time;
        self.respawn_time += 1;
        self.stage_tick(stage, level, env, cx, &crystals, time);
        self.dirty = true;
        true
    }

    /// `isArenaLoaded`: Vanilla checks the chunks `x -8..8` of the row `z = 8` only.
    fn arena_loaded(&self, level: &Level) -> bool {
        let c = self.origin.chunk();
        for x in c.x - 8..=c.x + 8 {
            let chunk = ChunkPos::new(x, c.z + 8);
            if !level.is_block_ticking(chunk) {
                return false;
            }
        }
        // `areEntitiesLoaded`: the chunk's stored entities are live.
        let entities_loaded = |p: ChunkPos| {
            level.chunks.visibility(p) != vanillite_world::chunks::Visibility::Hidden
        };
        (-1..=1).all(|dx| (-1..=1).all(|dz| entities_loaded(ChunkPos::new(c.x + dx, c.z + dz))))
    }

    /// `updatePlayers`: the players alive within 192 blocks of the dragon's spawn see the bar.
    fn update_players(&mut self, level: &Level, env: &mut Env<'_>) {
        let center = Vec3::new(
            f64::from(self.origin.x),
            f64::from(128 + self.origin.y),
            f64::from(self.origin.z),
        );
        let mut wanted = Vec::new();
        for (id, p) in level.entities.players.iter() {
            let Some(e) = level.entities.store.get(id) else {
                continue;
            };
            if !p.survival.is_dead_or_dying() && e.pos().distance_to_sqr(center) <= 192.0 * 192.0 {
                wanted.push(id);
            }
        }
        self.bar.sync_players(&wanted, env.out);
    }

    /// The exit portal's centre (`findExitPortal`): the frame below the podium column, found
    /// from the top down.
    fn find_exit_portal(&mut self, level: &Level) -> Option<BlockPos> {
        let top = height(
            level,
            HeightmapKind::MotionBlocking,
            self.origin.x,
            self.origin.z,
        );
        let mut y = top;
        while y >= level.min_y() {
            let c = BlockPos::new(self.origin.x, y - 3, self.origin.z);
            if live::portal_frame_at(level, c) {
                if self.exit_portal.is_none() {
                    self.exit_portal = Some(c);
                    self.dirty = true;
                }
                return Some(c);
            }
            y -= 1;
        }
        None
    }

    /// `hasActiveExitPortal`: the frame holds end portal blocks.
    fn has_active_exit_portal(&mut self, level: &Level) -> bool {
        self.find_exit_portal(level)
            .is_some_and(|c| level.block(c.offset(1, 0, 0)).block() == ids::END_PORTAL)
    }

    /// The alive dragons of the world (`getDragons`).
    fn dragons(level: &Level) -> Vec<EntityId> {
        level
            .entities
            .mobs
            .iter()
            .filter(|(_, m)| m.kind == Kind::EnderDragon && m.is_alive())
            .map(|(id, _)| id)
            .collect()
    }

    /// `scanState`: a world's first fight learns from the blocks and entities what happened.
    fn scan_state(&mut self, level: &mut Level) {
        let active = self.has_active_exit_portal(level);
        if active {
            self.previously_killed = true;
        } else {
            self.previously_killed = false;
            if self.find_exit_portal(level).is_none() {
                self.spawn_exit_portal(level, false);
            }
        }
        let dragons = Self::dragons(level);
        if let Some(&dragon) = dragons.first() {
            self.dragon_uuid = level.entities.store.get(dragon).map(|e| e.uuid());
            self.dragon_killed = false;
            if !active {
                level.entities.despawn(dragon);
                self.dragon_uuid = None;
            }
        } else {
            self.dragon_killed = true;
        }
        if !self.previously_killed && self.dragon_killed {
            self.dragon_killed = false;
        }
        self.dirty = true;
    }

    fn find_or_create_dragon(&mut self, level: &mut Level, env: &mut Env<'_>) {
        let dragons = Self::dragons(level);
        if let Some(&d) = dragons.first() {
            self.dragon_uuid = level.entities.store.get(d).map(|e| e.uuid());
            self.dirty = true;
        } else {
            self.create_new_dragon(level, env);
        }
    }

    /// `createNewDragon`: a dragon in its holding pattern 128 above the origin.
    fn create_new_dragon(&mut self, level: &mut Level, env: &mut Env<'_>) -> Option<EntityId> {
        let yaw = level.random().next_float() * 360.0;
        let seed = env.random.next_long();
        let uuid = random_uuid(env.random);
        let mob = super::new_dragon(seed, self.origin);
        let pos = Vec3::new(
            f64::from(self.origin.x),
            f64::from(128 + self.origin.y),
            f64::from(self.origin.z),
        );
        let ids = std::sync::Arc::clone(level.network_ids_arc());
        let id = crate::mob::spawn(
            &mut level.entities,
            &ids,
            uuid,
            pos,
            Rotation {
                y_rot: yaw,
                x_rot: 0.0,
                y_head_rot: yaw,
            },
            mob,
        );
        self.dragon_uuid = Some(uuid);
        self.dirty = true;
        Some(id)
    }

    /// `updateCrystalCount`: the crystals on the pillars.
    fn update_crystal_count(&mut self, level: &Level) {
        self.ticks_since_crystals_scanned = 0;
        self.alive_crystals = 0;
        for spike in spikes_for_seed(self.seed) {
            self.alive_crystals += crystals_in(level, spike_box(level, &spike)).len() as i32;
        }
    }

    /// Handles what the dragon and the crystals reported.
    pub fn handle(
        &mut self,
        level: &mut Level,
        env: &mut Env<'_>,
        cx: &mut MobContext,
        event: DragonEvent,
    ) {
        match event {
            DragonEvent::Update { uuid, progress } => {
                if Some(uuid) == self.dragon_uuid {
                    self.bar.set_progress(progress, env.out);
                    self.ticks_since_dragon_seen = 0;
                }
            }
            DragonEvent::Killed { uuid } => self.set_dragon_killed(level, uuid, env),
            DragonEvent::CrystalDestroyed {
                crystal,
                uuid,
                source,
                ..
            } => {
                if self.respawn_stage.is_some() && self.respawn_crystals.contains(&uuid) {
                    self.abort_respawn(level);
                } else {
                    self.update_crystal_count(level);
                    let dragon = self.dragon_uuid.and_then(|u| {
                        level
                            .entities
                            .mobs
                            .iter()
                            .find(|(id, m)| {
                                m.kind == Kind::EnderDragon
                                    && level.entities.store.get(*id).is_some_and(|e| e.uuid() == u)
                            })
                            .map(|(id, _)| id)
                    });
                    if let Some(dragon) = dragon {
                        let mut ents = std::mem::take(&mut level.entities);
                        super::on_crystal_destroyed(
                            level, &mut ents, env, cx, dragon, crystal, &source,
                        );
                        level.entities = ents;
                    }
                }
            }
        }
    }

    /// `setDragonKilled`: the bar hides, the portal opens, a gateway appears and the first
    /// kill leaves the egg.
    fn set_dragon_killed(&mut self, level: &mut Level, uuid: u128, env: &mut Env<'_>) {
        if Some(uuid) != self.dragon_uuid {
            return;
        }
        self.bar.set_progress(0.0, env.out);
        self.bar.set_visible(false, env.out);
        self.spawn_exit_portal(level, true);
        self.spawn_new_gateway(level);
        if !self.previously_killed {
            let y = height(
                level,
                HeightmapKind::MotionBlocking,
                self.origin.x,
                self.origin.z,
            );
            level.set_block_and_update(
                BlockPos::new(self.origin.x, y, self.origin.z),
                ids::DRAGON_EGG.default_state(),
            );
        }
        self.previously_killed = true;
        self.dragon_killed = true;
        self.dirty = true;
    }

    fn spawn_new_gateway(&mut self, level: &mut Level) {
        if let Some(index) = self.gateways.pop() {
            let pos = gateway_pos(index);
            level.level_event(None, 3000, pos, 0);
            live::place_gateway(level, pos);
            self.dirty = true;
        }
    }

    /// `spawnExitPortal(activated)`.
    fn spawn_exit_portal(&mut self, level: &mut Level, active: bool) {
        let at = match self.exit_portal {
            Some(p) => p,
            None => {
                let y = height(
                    level,
                    HeightmapKind::MotionBlockingNoLeaves,
                    self.origin.x,
                    self.origin.z,
                );
                let mut p = BlockPos::new(self.origin.x, y - 1, self.origin.z);
                while level.block(p).block() == ids::BEDROCK && p.y > 63 {
                    p = p.offset(0, -1, 0);
                }
                let p = BlockPos::new(p.x, p.y.max(level.min_y() + 1), p.z);
                self.exit_portal = Some(p);
                self.dirty = true;
                p
            }
        };
        live::place_podium(level, at, active);
    }

    /// Ids of the respawn crystals that still exist.
    fn resolve_respawn_crystals(&self, level: &Level) -> Vec<EntityId> {
        level
            .entities
            .specials
            .iter()
            .filter(|(_, d)| matches!(d, SpecialData::EndCrystal(c) if !c.removed))
            .filter(|(id, _)| {
                level
                    .entities
                    .store
                    .get(*id)
                    .is_some_and(|e| self.respawn_crystals.contains(&e.uuid()))
            })
            .map(|(id, _)| id)
            .collect()
    }

    fn abort_respawn(&mut self, level: &mut Level) {
        self.respawn_stage = None;
        self.respawn_time = 0;
        self.reset_spike_crystals(level);
        self.spawn_exit_portal(level, true);
        self.dirty = true;
    }

    /// `resetSpikeCrystals`: the pillars' crystals become vulnerable and stop beaming.
    fn reset_spike_crystals(&mut self, level: &mut Level) {
        for spike in spikes_for_seed(self.seed) {
            for id in crystals_in(level, spike_box(level, &spike)) {
                set_crystal(level, id, |c| {
                    c.invulnerable = false;
                    c.beam_target = None;
                });
            }
        }
    }

    /// `tryRespawn`: four crystals on the portal's sides start the respawn.
    pub fn try_respawn(&mut self, level: &mut Level) {
        if !self.dragon_killed || self.respawn_stage.is_some() {
            return;
        }
        let location = match self.exit_portal {
            Some(p) => p,
            None => {
                if self.find_exit_portal(level).is_none() {
                    self.spawn_exit_portal(level, true);
                }
                match self.exit_portal {
                    Some(p) => p,
                    None => return,
                }
            }
        };
        let center = location.offset(0, 1, 0);
        let mut crystals = Vec::new();
        for dir in live::HORIZONTAL {
            let p = center.relative(dir).relative(dir).relative(dir);
            let found = crystals_in(level, Aabb::of_block(p));
            if found.is_empty() {
                return;
            }
            crystals.extend(found);
        }
        self.respawn_dragon(level, &crystals);
    }

    fn respawn_dragon(&mut self, level: &mut Level, crystals: &[EntityId]) {
        if !self.dragon_killed || self.respawn_stage.is_some() {
            return;
        }
        while let Some(c) = self.find_exit_portal(level) {
            for p in live::portal_box(c) {
                let b = level.block(p).block();
                if b == ids::BEDROCK || b == ids::END_PORTAL {
                    level.set_block_and_update(p, ids::END_STONE.default_state());
                }
            }
        }
        self.respawn_stage = Some(RespawnStage::Start);
        self.respawn_time = 0;
        self.spawn_exit_portal(level, false);
        self.respawn_crystals = crystals
            .iter()
            .filter_map(|&id| level.entities.store.get(id).map(|e| e.uuid()))
            .collect();
        self.dirty = true;
    }

    /// `setRespawnStage`.
    fn set_respawn_stage(&mut self, level: &mut Level, env: &mut Env<'_>, stage: RespawnStage) {
        self.respawn_time = 0;
        if stage == RespawnStage::End {
            self.respawn_stage = None;
            self.dragon_killed = false;
            self.create_new_dragon(level, env);
        } else {
            self.respawn_stage = Some(stage);
        }
        self.dirty = true;
    }

    /// `DragonRespawnStage.tick`.
    fn stage_tick(
        &mut self,
        stage: RespawnStage,
        level: &mut Level,
        env: &mut Env<'_>,
        cx: &mut MobContext,
        crystals: &[EntityId],
        time: i32,
    ) {
        let _ = cx;
        match stage {
            RespawnStage::Start => {
                for &c in crystals {
                    set_crystal(level, c, |c| c.beam_target = Some(SPAWN));
                }
                self.set_respawn_stage(level, env, RespawnStage::PreparingToSummonPillars);
            }
            RespawnStage::PreparingToSummonPillars => {
                if time < 100 {
                    if time == 0 || time == 50 || time == 51 || time == 52 || time >= 95 {
                        level.level_event(None, 3001, SPAWN, 0);
                    }
                } else {
                    self.set_respawn_stage(level, env, RespawnStage::SummoningPillars);
                }
            }
            RespawnStage::SummoningPillars => {
                let start = time % 40 == 0;
                let end = time % 40 == 39;
                if start || end {
                    let spikes = spikes_for_seed(self.seed);
                    let index = usize::try_from(time / 40).unwrap_or(usize::MAX);
                    if let Some(spike) = spikes.get(index) {
                        if start {
                            let target =
                                BlockPos::new(spike.center_x, spike.height + 1, spike.center_z);
                            for &c in crystals {
                                set_crystal(level, c, |c| c.beam_target = Some(target));
                            }
                        } else {
                            for z in spike.center_z - 10..=spike.center_z + 10 {
                                for y in spike.height - 10..=spike.height + 10 {
                                    for x in spike.center_x - 10..=spike.center_x + 10 {
                                        level.remove_block(BlockPos::new(x, y, z), false);
                                    }
                                }
                            }
                            let decay = if env.rules.bool(Rule::BLOCK_EXPLOSION_DROP_DECAY) {
                                BlockInteraction::DestroyWithDecay
                            } else {
                                BlockInteraction::Destroy
                            };
                            level.explode(
                                None,
                                None,
                                Vec3::new(
                                    f64::from(spike.center_x) + 0.5,
                                    f64::from(spike.height),
                                    f64::from(spike.center_z) + 0.5,
                                ),
                                5.0,
                                decay,
                            );
                            live::place_spike(level, env, spike);
                        }
                    } else if start {
                        self.set_respawn_stage(level, env, RespawnStage::SummoningDragon);
                    }
                }
            }
            RespawnStage::SummoningDragon => {
                if time >= 100 {
                    self.set_respawn_stage(level, env, RespawnStage::End);
                    self.reset_spike_crystals(level);
                    for &c in crystals {
                        set_crystal(level, c, |c| c.beam_target = None);
                        let Some(pos) = level.entities.store.get(c).map(|e| e.pos()) else {
                            continue;
                        };
                        level.explode(Some(c), None, pos, 6.0, BlockInteraction::Keep);
                        level.entities.despawn(c);
                    }
                } else if time >= 80 {
                    level.level_event(None, 3001, SPAWN, 0);
                } else if time == 0 {
                    for &c in crystals {
                        set_crystal(level, c, |c| c.beam_target = Some(SPAWN));
                    }
                } else if time < 5 {
                    level.level_event(None, 3001, SPAWN, 0);
                }
            }
            RespawnStage::End => {}
        }
    }
}

/// `spike.getTopBoundingBox()`: the pillar's column over the whole height.
fn spike_box(level: &Level, spike: &EndSpike) -> Aabb {
    Aabb::new(
        f64::from(spike.center_x - spike.radius),
        f64::from(level.min_y() - 2048),
        f64::from(spike.center_z - spike.radius),
        f64::from(spike.center_x + spike.radius),
        f64::from(level.max_y() + 2048),
        f64::from(spike.center_z + spike.radius),
    )
}

/// The end crystals whose boxes touch `area`.
fn crystals_in(level: &Level, area: Aabb) -> Vec<EntityId> {
    let mut out = Vec::new();
    let ents = &level.entities;
    ents.store.for_each_in(&area, false, |id| {
        if matches!(ents.special(id), Some(SpecialData::EndCrystal(c)) if !c.removed)
            && ents
                .store
                .get(id)
                .is_some_and(|e| e.aabb().intersects(&area))
        {
            out.push(id);
        }
    });
    out
}

/// Changes a crystal's data and marks its synched data dirty.
fn set_crystal(level: &mut Level, id: EntityId, f: impl FnOnce(&mut super::Crystal)) {
    if let Some(SpecialData::EndCrystal(c)) = level.entities.special_mut(id) {
        f(c);
        if let Some(mut e) = level.entities.store.get_mut(id) {
            e.mark_data_dirty();
        }
    }
}

/// `EndCrystalItem.useOn`: places a crystal (without its bottom) on obsidian or bedrock when
/// the two blocks above are free of entities. Returns whether it was placed; the fight then
/// tries to respawn the dragon.
pub fn use_end_crystal(level: &mut Level, uuid: u128, on: BlockPos) -> bool {
    let b = level.block(on).block();
    if b != ids::OBSIDIAN && b != ids::BEDROCK {
        return false;
    }
    let above = on.offset(0, 1, 0);
    if !level.block(above).is_air() {
        return false;
    }
    let (x, y, z) = (f64::from(above.x), f64::from(above.y), f64::from(above.z));
    let area = Aabb::new(x, y, z, x + 1.0, y + 2.0, z + 1.0);
    let mut any = false;
    let ents = &level.entities;
    ents.store.for_each_in(&area, false, |id| {
        if ents
            .store
            .get(id)
            .is_some_and(|e| e.aabb().intersects(&area))
        {
            any = true;
        }
    });
    if any {
        return false;
    }
    let mut new = vanillite_world::entity::NewEntity::new(
        vanillite_data::entity::ids::END_CRYSTAL,
        level.network_ids().next(),
        uuid,
        Vec3::new(x + 0.5, y, z + 0.5),
    );
    new.rot = Rotation::default();
    level.entities.spawn_special(
        new,
        SpecialData::EndCrystal(Box::new(super::Crystal {
            show_bottom: false,
            ..super::Crystal::default()
        })),
    );
    let place = crate::level::game_event::events().entity_place;
    level.game_event_at(None, place, above);
    level.output().try_dragon_respawn = true;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_order_is_a_seeded_permutation() {
        let order = gateway_order(42);
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..20).collect::<Vec<_>>());
        assert_eq!(order, gateway_order(42));
        assert_ne!(order, gateway_order(43));
    }

    #[test]
    fn gateway_positions_on_the_circle() {
        assert_eq!(gateway_pos(0), BlockPos::new(96, 75, 0));
        for i in 0..20 {
            let p = gateway_pos(i);
            let r = f64::from(p.x * p.x + p.z * p.z).sqrt();
            assert!((94.0..=97.0).contains(&r), "{i}: {p:?}");
            assert_eq!(p.y, 75);
        }
    }

    #[test]
    fn saved_data_round_trip() {
        let mut r = LegacyRandom::new(1);
        let mut f = DragonFight::new(None, 7, &mut r);
        assert!(f.take_dirty(), "new gateways are saved");
        f.dragon_killed = true;
        f.previously_killed = true;
        f.dragon_uuid = Some(0x1234_5678_9abc_def0_1122_3344_5566_7788);
        f.exit_portal = Some(BlockPos::new(0, 60, 0));
        f.respawn_stage = Some(RespawnStage::SummoningPillars);
        let saved = f.save();
        let back = DragonFight::new(Some(&saved), 7, &mut r);
        assert!(back.dragon_killed && back.previously_killed);
        assert_eq!(back.dragon_uuid, f.dragon_uuid);
        assert_eq!(back.exit_portal, f.exit_portal);
        assert_eq!(back.gateways, f.gateways);
        assert_eq!(back.respawn_stage, Some(RespawnStage::SummoningPillars));
    }
}
