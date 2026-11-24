//! Survival in a world: weather and game time, the players' survival tick, their moves, death
//! and respawn, lightning, and the delivery of what the survival logic sends to other players.

use std::sync::Arc;

use vanillite_base::hash::FxHashMap;
use vanillite_base::random::{LegacyRandom, RandomSource};
use vanillite_base::{Aabb, BlockPos, ChunkPos, Vec3};
use vanillite_chunk::HeightmapKind;
use vanillite_data::block::BlockState;
use vanillite_data::entity::ids;
use vanillite_game::block::weather::Climate;
use vanillite_game::entities::Entities;
use vanillite_game::player::{ChunkView, effective_view_distance, join};
use vanillite_game::survival::Effect;
use vanillite_game::survival::blocks::BlockAccess;
use vanillite_game::survival::player::{self, Env, TickResult};
use vanillite_game::survival::rules::{Difficulty, GameRules, Rule};
use vanillite_game::survival::weather::{LevelWeather, WeatherPacket};
use vanillite_protocol::SharedPacket;
use vanillite_protocol::packets::play::clientbound::{
    ChangeDifficulty, ClockUpdate, GameEvent, RemoveEntities, Respawn, SetExperience, SetTime,
};
use vanillite_protocol::types::VarIntList;
use vanillite_world::chunks::Chunks;
use vanillite_world::entity::{EntityFlags, EntityId, NewEntity, Rotation};

use crate::global::Global;
use crate::level::LevelState;

/// `ServerLevel.tickThunder`: the chance per ticking chunk and tick.
const THUNDER_CHANCE: f64 = 1.0e-5;

/// Block access of a world's loaded chunks.
pub(crate) struct WorldBlocks<'a>(pub(crate) &'a Chunks);

impl BlockAccess for WorldBlocks<'_> {
    #[inline]
    fn block_state(&self, pos: BlockPos) -> Option<BlockState> {
        self.0.block(pos).map(BlockState)
    }
}

/// A lightning bolt (`LightningBolt`): its remaining life, flashes and the entities it hit.
#[derive(Debug)]
struct Bolt {
    id: EntityId,
    pos: Vec3,
    life: i32,
    flashes: i32,
}

/// The survival state of one world.
#[derive(Debug)]
pub(crate) struct WorldSurvival {
    random: LegacyRandom,
    pub(crate) weather: LevelWeather,
    /// `canHaveWeather`: sky light and no ceiling (the overworld).
    can_have_weather: bool,
    /// The overworld advances the game time.
    ticks_time: bool,
    effects: Vec<Effect>,
    weather_out: Vec<WeatherPacket>,
    /// The world's biomes for `isRainingAt`, shared with the level.
    climate: Option<Arc<Climate>>,
    min_y: i32,
    max_y: i32,
    /// Tracking range of players in blocks.
    player_range: i32,
    simulation_distance: i32,
    bolts: Vec<Bolt>,
    /// Candidate chunks left until the next lightning strike (geometric skip sampling).
    thunder_skip: f64,
    /// Players by chunk, rebuilt when effects are delivered.
    grid: FxHashMap<ChunkPos, Vec<EntityId>>,
    scratch: Vec<EntityId>,
}

/// `Level.isRainingAt` without the sky light test (the heightmap test covers it outside of
/// caves and under transparent blocks).
fn raining_at(
    chunks: &Chunks,
    climate: Option<&Climate>,
    weather: &LevelWeather,
    pos: BlockPos,
) -> bool {
    if !weather.is_raining() {
        return false;
    }
    let Some(chunk) = chunks.chunk(pos.chunk()) else {
        return false;
    };
    let Some(heights) = chunk.chunk.heightmaps().get(HeightmapKind::MotionBlocking) else {
        return false;
    };
    #[allow(clippy::cast_sign_loss, clippy::as_conversions)]
    let top = heights.get((pos.x & 15) as u32, (pos.z & 15) as u32) as i32 + chunk.chunk.min_y();
    if top > pos.y {
        return false;
    }
    let Some(climate) = climate else {
        return true;
    };
    let biome = climate
        .manager
        .get_biome(pos.x, pos.y, pos.z, &mut |qx: i32, qy: i32, qz: i32| {
            noise_biome(chunks, qx, qy, qz)
        });
    let data = climate.biomes.get(biome);
    data.has_precipitation && data.warm_enough_to_rain(pos.x, pos.y, pos.z, climate.sea_level)
}

/// `getNoiseBiome` of the loaded chunks; the quart height is clamped to the chunk.
fn noise_biome(chunks: &Chunks, qx: i32, qy: i32, qz: i32) -> u16 {
    let Some(chunk) = chunks.chunk(ChunkPos::new(qx >> 2, qz >> 2)) else {
        return 0;
    };
    let sections = chunk.chunk.sections();
    let min_quart = chunk.chunk.min_section() * 4;
    let count = i32::try_from(sections.len()).unwrap_or(0);
    let qy = qy.clamp(min_quart, min_quart + count * 4 - 1);
    let index = usize::try_from((qy - min_quart) >> 2).unwrap_or(0);
    #[allow(clippy::cast_sign_loss, clippy::as_conversions)]
    sections.get(index).map_or(0, |s| {
        s.biome((qx & 3) as u32, (qy & 3) as u32, (qz & 3) as u32)
    })
}

fn chunk_of(pos: Vec3) -> ChunkPos {
    ChunkPos::new(
        vanillite_base::math::floor(pos.x) >> 4,
        vanillite_base::math::floor(pos.z) >> 4,
    )
}

/// `Difficulty` of `server.properties`.
pub(crate) fn difficulty(d: vanillite_config::Difficulty) -> Difficulty {
    match d {
        vanillite_config::Difficulty::Peaceful => Difficulty::Peaceful,
        vanillite_config::Difficulty::Easy => Difficulty::Easy,
        vanillite_config::Difficulty::Normal => Difficulty::Normal,
        vanillite_config::Difficulty::Hard => Difficulty::Hard,
    }
}

/// `ClientboundChangeDifficultyPacket`.
pub(crate) fn change_difficulty(level: &LevelState) -> ChangeDifficulty {
    ChangeDifficulty {
        difficulty: level.difficulty.id(),
        locked: false,
    }
}

/// The current `set_time` with all clocks (`createFullSyncPacket`).
pub(crate) fn full_time_sync(level: &LevelState) -> SetTime {
    let advance = level.rules().bool(Rule::ADVANCE_TIME);
    level.clocks().full_sync(level.game_time(), advance)
}

/// The `set_time` after `clock` changed.
pub(crate) fn clock_update(level: &LevelState, clock: usize) -> SetTime {
    let advance = level.rules().bool(Rule::ADVANCE_TIME);
    level
        .clocks()
        .update_packet(clock, level.game_time(), advance)
}

/// `forceGameTimeSynchronization`: the game time without clocks.
pub(crate) fn game_time_sync(level: &LevelState) -> SetTime {
    SetTime {
        game_time: level.game_time(),
        clocks: Vec::<ClockUpdate>::new(),
    }
}

impl WorldSurvival {
    pub(crate) fn new(
        dimension: &str,
        level: &LevelState,
        info: &join::LevelInfo,
        (min_y, height): (i32, u32),
        player_range: i32,
        climate: Option<Climate>,
    ) -> WorldSurvival {
        let (hashed_seed, sea_level) = (info.hashed_seed, info.sea_level);
        let simulation_distance = info.simulation_distance;
        let overworld = dimension == "minecraft:overworld";
        let mut random = LegacyRandom::new(hashed_seed ^ i64::from(sea_level));
        let thunder_skip = geometric(&mut random);
        WorldSurvival {
            random,
            weather: if overworld {
                LevelWeather::prepare(&level.weather())
            } else {
                LevelWeather::default()
            },
            can_have_weather: overworld,
            ticks_time: overworld,
            effects: Vec::new(),
            weather_out: Vec::new(),
            climate: climate.map(Arc::new),
            min_y,
            max_y: min_y + i32::try_from(height).unwrap_or(0) - 1,
            player_range: player_range.max(0),
            simulation_distance: simulation_distance.max(1),
            bolts: Vec::new(),
            thunder_skip,
            grid: FxHashMap::default(),
            scratch: Vec::new(),
        }
    }

    /// The world's biomes, shared with the level.
    pub(crate) fn climate(&self) -> Option<&Arc<Climate>> {
        self.climate.as_ref()
    }

    /// The weather packets for a player entering this world.
    pub(crate) fn join_weather(&self) -> Vec<GameEvent> {
        self.weather.join_packets()
    }

    /// `advanceWeatherCycle` and `tickTime`, at the start of the world tick when it runs.
    pub(crate) fn tick_level(&mut self, entities: &Entities, global: &Global) {
        let level = &global.level;
        let advance = level.rules().bool(Rule::ADVANCE_WEATHER);
        self.weather_out.clear();
        if self.can_have_weather {
            let mut data = level.weather();
            self.weather.advance(
                Some(&mut data),
                advance,
                &mut self.random,
                &mut self.weather_out,
            );
        } else {
            self.weather
                .advance(None, advance, &mut self.random, &mut self.weather_out);
        }
        for packet in self.weather_out.drain(..) {
            match packet {
                WeatherPacket::World(event) => {
                    let shared = SharedPacket::of(&event);
                    for (_, p) in entities.players.iter() {
                        p.connection.sink.send_shared(&shared);
                    }
                }
                WeatherPacket::All(event) => global.players.broadcast(&event, None),
            }
        }
        if self.ticks_time {
            level
                .game_time
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// The survival `Env` over this world's state.
    fn env<'a>(
        &'a mut self,
        level: &'a LevelState,
        rules: &'a GameRules,
        blocks: &'a WorldBlocks<'a>,
        raining_at: &'a dyn Fn(BlockPos) -> bool,
        dimension: &'a str,
    ) -> Env<'a> {
        Env {
            types: &level.damage_types,
            facts: &level.block_facts,
            blocks,
            rules,
            difficulty: level.difficulty,
            game_time: level.game_time(),
            min_y: self.min_y,
            dimension,
            raining_at,
            random: &mut self.random,
            out: &mut self.effects,
        }
    }

    /// The accepted moves of every player: jump exhaustion, fall damage and movement
    /// exhaustion (`handleMovePlayer`).
    pub(crate) fn moves(
        &mut self,
        entities: &mut Entities,
        chunks: &Chunks,
        global: &Global,
        dimension: &str,
    ) {
        let level = &global.level;
        let rules = level.rules().clone();
        let blocks = WorldBlocks(chunks);
        let (weather, climate) = (self.weather, self.climate.clone());
        let raining = |pos| raining_at(chunks, climate.as_deref(), &weather, pos);
        let mut env = self.env(level, &rules, &blocks, &raining, dimension);
        let Entities { store, players, .. } = entities;
        for (id, data) in players.iter_mut() {
            if data.connection.movement.moves.is_empty() {
                continue;
            }
            let Some(mut entity) = store.get_mut(id) else {
                data.connection.movement.moves.clear();
                continue;
            };
            let moves = std::mem::take(&mut data.connection.movement.moves);
            for mv in &moves {
                player::on_move(&mut entity, data, &mut env, *mv);
            }
            let mut moves = moves;
            moves.clear();
            data.connection.movement.moves = moves;
        }
        self.deliver(entities, global);
    }

    /// The entity tick of players (`ServerPlayer.tick`) and of lightning bolts.
    pub(crate) fn tick_entities(
        &mut self,
        entities: &mut Entities,
        chunks: &Chunks,
        global: &Global,
        dimension: &str,
    ) {
        for (_, data) in entities.players.iter_mut() {
            player::tick_entity(data);
        }
        self.tick_bolts(entities, chunks, global, dimension);
    }

    /// `ServerPlayer.doTick` of every player, in the connection tick. Players whose death
    /// animation ended disappear for the others.
    pub(crate) fn do_tick(
        &mut self,
        entities: &mut Entities,
        chunks: &Chunks,
        global: &Global,
        dimension: &str,
    ) {
        let level = &global.level;
        let rules = level.rules().clone();
        let blocks = WorldBlocks(chunks);
        let (weather, climate) = (self.weather, self.climate.clone());
        let raining = |pos| raining_at(chunks, climate.as_deref(), &weather, pos);
        let mut removed = std::mem::take(&mut self.scratch);
        {
            let mut env = self.env(level, &rules, &blocks, &raining, dimension);
            let Entities { store, players, .. } = entities;
            for (id, data) in players.iter_mut() {
                let Some(mut entity) = store.get_mut(id) else {
                    continue;
                };
                if player::do_tick(&mut entity, data, &mut env) == TickResult::Remove {
                    removed.push(id);
                }
            }
        }
        for id in removed.drain(..) {
            // `remove(KILLED)`: the entity leaves the others' view; the player itself stays
            // until it respawns.
            if let Some(entity) = entities.store.get(id) {
                self.effects.push(Effect::Tracking {
                    entity: id,
                    with_self: false,
                    packet: SharedPacket::of(&RemoveEntities {
                        ids: VarIntList(vec![entity.network_id().0]),
                    }),
                });
            }
        }
        self.scratch = removed;
        self.deliver(entities, global);
    }

    /// Kills a player (`/kill`) and delivers the effects at once.
    pub(crate) fn kill(
        entities: &mut Entities,
        chunks: Option<&Chunks>,
        global: &Global,
        dimension: &str,
        id: EntityId,
    ) -> bool {
        Self::on_player(entities, chunks, global, dimension, id, player::kill)
    }

    /// Hurts a player (`/damage`, `hurtServer`) and delivers the effects at once.
    pub(crate) fn hurt(
        entities: &mut Entities,
        chunks: Option<&Chunks>,
        global: &Global,
        dimension: &str,
        id: EntityId,
        source: vanillite_game::survival::damage::DamageSource,
        amount: f32,
    ) -> bool {
        Self::on_player(
            entities,
            chunks,
            global,
            dimension,
            id,
            |entity, data, env| player::hurt(entity, data, env, source, amount),
        )
    }

    /// Runs `f` on a player with a survival environment outside the tick (commands) and
    /// delivers its effects at once.
    fn on_player(
        entities: &mut Entities,
        chunks: Option<&Chunks>,
        global: &Global,
        dimension: &str,
        id: EntityId,
        f: impl FnOnce(
            &mut vanillite_world::entity::EntityMut<'_>,
            &mut vanillite_game::player::PlayerData,
            &mut Env<'_>,
        ) -> bool,
    ) -> bool {
        struct NoBlocks;
        impl BlockAccess for NoBlocks {
            fn block_state(&self, _: BlockPos) -> Option<BlockState> {
                None
            }
        }
        let level = &global.level;
        let rules = level.rules().clone();
        let mut random = LegacyRandom::new(level.game_time());
        let mut out = Vec::new();
        let no_blocks = NoBlocks;
        let world_blocks = chunks.map(WorldBlocks);
        let blocks: &dyn BlockAccess = match &world_blocks {
            Some(b) => b,
            None => &no_blocks,
        };
        let raining = |_| false;
        let mut env = Env {
            types: &level.damage_types,
            facts: &level.block_facts,
            blocks,
            rules: &rules,
            difficulty: level.difficulty,
            game_time: level.game_time(),
            min_y: 0,
            dimension,
            raining_at: &raining,
            random: &mut random,
            out: &mut out,
        };
        let Entities { store, players, .. } = entities;
        let Some(mut entity) = store.get_mut(id) else {
            return false;
        };
        let row = entity.as_ref().kind_row();
        let Some(data) = players.get_mut(row) else {
            return false;
        };
        let done = f(&mut entity, data, &mut env);
        deliver_simple(&out, entities, global);
        done
    }

    /// `tickThunder` over the ticking chunks around the players, and the bolts' life.
    pub(crate) fn tick_thunder(
        &mut self,
        entities: &mut Entities,
        chunks: &Chunks,
        global: &Global,
    ) {
        if !(self.can_have_weather && self.weather.is_raining() && self.weather.is_thundering()) {
            return;
        }
        let count = entities.players.len();
        if count == 0 {
            return;
        }
        let side = 2 * self.simulation_distance + 1;
        let per_player = f64::from(side * side);
        #[allow(clippy::cast_precision_loss, clippy::as_conversions)]
        let total = per_player * count as f64;
        self.thunder_skip -= total;
        let mut targets = Vec::new();
        while self.thunder_skip < 0.0 {
            // The candidate `total + skip` among all players' squares.
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::as_conversions
            )]
            let index = (total + self.thunder_skip) as usize;
            self.thunder_skip += geometric(&mut self.random);
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::as_conversions
            )]
            let per = per_player as usize;
            let Some((id, _)) = entities.players.iter().nth(index / per.max(1)) else {
                continue;
            };
            let Some(center) = entities.store.get(id).map(|e| chunk_of(e.pos())) else {
                continue;
            };
            let offset = i32::try_from(index % per.max(1)).unwrap_or(0);
            let chunk = ChunkPos::new(
                center.x - self.simulation_distance + offset % side,
                center.z - self.simulation_distance + offset / side,
            );
            targets.push(chunk);
        }
        if targets.is_empty() {
            return;
        }
        self.build_grid(entities);
        for chunk in targets {
            // A chunk inside several players' squares is a candidate several times.
            let mut covering = 0u32;
            let d = self.simulation_distance;
            for x in chunk.x - d..=chunk.x + d {
                for z in chunk.z - d..=chunk.z + d {
                    covering +=
                        u32::try_from(self.grid.get(&ChunkPos::new(x, z)).map_or(0, Vec::len))
                            .unwrap_or(0);
                }
            }
            if covering == 0 || chunks.chunk(chunk).is_none() {
                continue;
            }
            if covering > 1
                && self
                    .random
                    .next_int_bounded(i32::try_from(covering).unwrap_or(i32::MAX))
                    != 0
            {
                continue;
            }
            self.strike(entities, chunks, global, chunk);
        }
    }

    /// One strike in `chunk`: `findLightningTargetAround`, `isRainingAt` and the bolt.
    fn strike(
        &mut self,
        entities: &mut Entities,
        chunks: &Chunks,
        global: &Global,
        chunk: ChunkPos,
    ) {
        let r = self.random.next_int();
        let x = chunk.x * 16 + (r & 15);
        let z = chunk.z * 16 + ((r >> 8) & 15);
        let Some(c) = chunks.chunk(chunk) else {
            return;
        };
        let Some(heights) = c.chunk.heightmaps().get(HeightmapKind::MotionBlocking) else {
            return;
        };
        #[allow(clippy::cast_sign_loss, clippy::as_conversions)]
        let top = heights.get((x & 15) as u32, (z & 15) as u32) as i32 + c.chunk.min_y();
        let mut pos = BlockPos::new(x, top, z);
        // TODO(#30): lightning rods.
        let area = Aabb::new(
            f64::from(x) - 3.0,
            f64::from(top) - 3.0,
            f64::from(z) - 3.0,
            f64::from(x) + 4.0,
            f64::from(self.max_y) + 2.0 + 3.0,
            f64::from(z) + 4.0,
        );
        let mut candidates = Vec::new();
        entities
            .store
            .for_each_in(&area, true, |id| candidates.push(id));
        candidates.retain(|&id| {
            entities.player(id).is_some_and(|p| {
                p.survival.health > 0.0 && p.game_mode != vanillite_config::GameMode::Spectator
            }) && entities.store.get(id).is_some_and(|e| {
                let p = e.pos();
                let feet = BlockPos::new(
                    vanillite_base::math::floor(p.x),
                    vanillite_base::math::floor(p.y),
                    vanillite_base::math::floor(p.z),
                );
                sees_sky(chunks, feet)
            })
        });
        if candidates.is_empty() {
            if pos.y == self.min_y - 1 {
                pos = BlockPos::new(pos.x, pos.y + 2, pos.z);
            }
        } else {
            let pick = self
                .random
                .next_int_bounded(i32::try_from(candidates.len()).unwrap_or(i32::MAX));
            if let Some(e) = usize::try_from(pick)
                .ok()
                .and_then(|i| candidates.get(i))
                .and_then(|&id| entities.store.get(id))
            {
                let p = e.pos();
                pos = BlockPos::new(
                    vanillite_base::math::floor(p.x),
                    vanillite_base::math::floor(p.y),
                    vanillite_base::math::floor(p.z),
                );
            }
        }
        if !raining_at(chunks, self.climate.as_deref(), &self.weather, pos) {
            return;
        }
        // TODO(#30): the skeleton horse trap (`spawn_mobs` and the local difficulty).
        let at = Vec3::new(
            f64::from(pos.x) + 0.5,
            f64::from(pos.y),
            f64::from(pos.z) + 0.5,
        );
        let uuid = (u128::from(self.random.next_long() as u64) << 64)
            | u128::from(self.random.next_long() as u64);
        let id = entities.store.spawn(
            NewEntity::new(ids::LIGHTNING_BOLT, global.network_ids.next(), uuid, at),
            |_| 0,
        );
        let flashes = self.random.next_int_bounded(3) + 1;
        self.bolts.push(Bolt {
            id,
            pos: at,
            life: 2,
            flashes,
        });
    }

    /// `LightningBolt.tick`: hits the entities around it while it flashes.
    fn tick_bolts(
        &mut self,
        entities: &mut Entities,
        chunks: &Chunks,
        global: &Global,
        dimension: &str,
    ) {
        if self.bolts.is_empty() {
            return;
        }
        let level = &global.level;
        let rules = level.rules().clone();
        let blocks = WorldBlocks(chunks);
        let (weather, climate) = (self.weather, self.climate.clone());
        let raining = |pos| raining_at(chunks, climate.as_deref(), &weather, pos);
        let mut bolts = std::mem::take(&mut self.bolts);
        let mut hit = Vec::new();
        {
            let mut env = self.env(level, &rules, &blocks, &raining, dimension);
            bolts.retain_mut(|bolt| {
                // TODO(#30): fire blocks (`spawnFire`) once blocks tick.
                bolt.life -= 1;
                if bolt.life < 0 {
                    if bolt.flashes == 0 {
                        entities.despawn(bolt.id);
                        return false;
                    } else if bolt.life < -env.random.next_int_bounded(10) {
                        bolt.flashes -= 1;
                        bolt.life = 1;
                        env.random.next_long();
                    }
                }
                if bolt.life >= 0 {
                    let p = bolt.pos;
                    let area = Aabb::new(
                        p.x - 3.0,
                        p.y - 3.0,
                        p.z - 3.0,
                        p.x + 3.0,
                        p.y + 9.0,
                        p.z + 3.0,
                    );
                    hit.clear();
                    entities.store.for_each_in(&area, true, |id| hit.push(id));
                    let Entities { store, players, .. } = &mut *entities;
                    for &id in &hit {
                        let Some(mut entity) = store.get_mut(id) else {
                            continue;
                        };
                        if !entity.as_ref().aabb().intersects(&area) {
                            continue;
                        }
                        let row = entity.as_ref().kind_row();
                        if let Some(data) = players.get_mut(row)
                            && data.survival.health > 0.0
                        {
                            player::thunder_hit(&mut entity, data, &mut env);
                        }
                    }
                }
                true
            });
        }
        self.bolts = bolts;
        self.deliver(entities, global);
    }

    /// Players by chunk for the delivery of effects.
    fn build_grid(&mut self, entities: &Entities) {
        for list in self.grid.values_mut() {
            list.clear();
        }
        for (id, _) in entities.players.iter() {
            if let Some(e) = entities.store.get(id) {
                self.grid.entry(chunk_of(e.pos())).or_default().push(id);
            }
        }
        self.grid.retain(|_, list| !list.is_empty());
    }

    /// Sends what the survival logic produced to its receivers. Each receiver lookup visits
    /// the players near the source only: all players when there are few, else the chunks in
    /// range.
    fn deliver(&mut self, entities: &mut Entities, global: &Global) {
        if self.effects.is_empty() {
            return;
        }
        let effects = std::mem::take(&mut self.effects);
        self.build_grid(entities);
        let mut receivers = Vec::new();
        for effect in &effects {
            match effect {
                Effect::Tracking {
                    entity,
                    with_self,
                    packet,
                } => {
                    let Some(source) = entities.store.get(*entity) else {
                        continue;
                    };
                    let pos = source.pos();
                    let chunk = chunk_of(pos);
                    let range = f64::from(self.player_range);
                    self.near(entities, pos, range, &mut receivers);
                    for &viewer in &receivers {
                        if viewer == *entity {
                            continue;
                        }
                        let (Some(v), Some(p)) =
                            (entities.store.get(viewer), entities.player(viewer))
                        else {
                            continue;
                        };
                        let d = v.pos() - pos;
                        let sees = vanillite_world::tracking::Viewer {
                            entity: viewer,
                            view_center: p.chunk_view.center,
                            view_distance: p.chunk_view.view_distance,
                        }
                        .sees_chunk(chunk);
                        if sees && d.x * d.x + d.z * d.z <= range * range {
                            p.connection.sink.send_shared(packet);
                        }
                    }
                    if *with_self && let Some(p) = entities.player(*entity) {
                        p.connection.sink.send_shared(packet);
                    }
                }
                Effect::Nearby {
                    pos,
                    range,
                    except,
                    packet,
                } => {
                    self.near(entities, *pos, *range, &mut receivers);
                    for &id in &receivers {
                        if Some(id) == *except {
                            continue;
                        }
                        let (Some(e), Some(p)) = (entities.store.get(id), entities.player(id))
                        else {
                            continue;
                        };
                        let d = e.pos() - *pos;
                        if d.x * d.x + d.y * d.y + d.z * d.z < range * range {
                            p.connection.sink.send_shared(packet);
                        }
                    }
                }
                Effect::Broadcast(message) => {
                    global
                        .players
                        .broadcast_system(message.clone(), false, None);
                }
                Effect::DropItem { .. } => {}
            }
        }
        spawn_drops(&effects, entities, global);
    }

    /// The players of the chunks within `range` of `pos` (all players when that is fewer).
    fn near(&self, entities: &Entities, pos: Vec3, range: f64, out: &mut Vec<EntityId>) {
        out.clear();
        #[allow(clippy::cast_possible_truncation, clippy::as_conversions)]
        let r = (range / 16.0).ceil() as i32 + 1;
        let side = usize::try_from(2 * r + 1).unwrap_or(usize::MAX);
        if entities.players.len() <= side.saturating_mul(side) {
            out.extend(entities.players.iter().map(|(id, _)| id));
            return;
        }
        let c = chunk_of(pos);
        for x in c.x - r..=c.x + r {
            for z in c.z - r..=c.z + r {
                if let Some(list) = self.grid.get(&ChunkPos::new(x, z)) {
                    out.extend_from_slice(list);
                }
            }
        }
    }

    /// `PlayerList.respawn` for a dead player that asked for it: a new entity with the same
    /// network id at the world spawn, the respawn packets and the world sent again.
    pub(crate) fn respawn(
        &mut self,
        entities: &mut Entities,
        global: &Global,
        level_info: &join::LevelInfo,
        server_view_distance: i32,
        id: EntityId,
    ) -> Option<EntityId> {
        let level = &global.level;
        let dead = entities
            .player(id)
            .is_some_and(|p| p.survival.is_dead_or_dying());
        if !dead {
            return None;
        }
        let keep_all = false;
        let uuid = entities.store.get(id)?.uuid();
        let tags = entities.tags_of(id).to_vec();
        let (removed, mut data) = entities.despawn_player(id)?;
        let keep_inventory = level.rules().bool(Rule::KEEP_INVENTORY)
            || data.game_mode == vanillite_config::GameMode::Spectator;
        data.survival = data.survival.respawned(keep_inventory);
        data.survival.reset_sent();
        data.connection.chunks.reset();
        let sink = Arc::clone(&data.connection.sink);
        let (pos, yaw) = level_info.spawn;
        join::send_abilities(sink.as_ref(), data.game_mode);
        sink.send(&Respawn {
            spawn: join::spawn_info(level_info, &data),
            data_to_keep: u8::from(keep_all),
        });
        let mut body = vanillite_game::player::Body {
            pos,
            velocity: Vec3::ZERO,
            y_rot: yaw,
            x_rot: 0.0,
            on_ground: false,
            horizontal_collision: false,
            fall_flying: false,
        };
        data.connection
            .movement
            .teleport(&mut body, pos, yaw, 0.0, sink.as_ref());
        join::send_spawn_position(sink.as_ref(), level_info);
        sink.send(&change_difficulty(level));
        sink.send(&SetExperience {
            progress: data.survival.xp.progress,
            level: data.survival.xp.level,
            total: data.survival.xp.total,
        });
        join::send_level_info(
            sink.as_ref(),
            level_info,
            &full_time_sync(level),
            &self.join_weather(),
        );
        let view = ChunkView {
            center: chunk_of(pos),
            view_distance: effective_view_distance(&data.client, server_view_distance),
        };
        data.chunk_view = view;
        data.connection
            .chunks
            .update_view(view.center, view.view_distance, sink.as_ref());
        data.connection.movement.restart_client_load_timer();
        let new = entities.spawn_player_as(
            removed.network_id,
            uuid,
            pos,
            Rotation {
                y_rot: yaw,
                x_rot: 0.0,
                y_head_rot: yaw,
            },
            data,
        );
        entities.set_tags(new, tags);
        if let Some(mut entity) = entities.store.get_mut(new) {
            entity.set_flags(EntityFlags::ON_GROUND, false);
            entity.mark_data_dirty();
        }
        Some(new)
    }
}

/// `canSeeSky` from the heightmap.
fn sees_sky(chunks: &Chunks, pos: BlockPos) -> bool {
    let Some(c) = chunks.chunk(pos.chunk()) else {
        return false;
    };
    let Some(heights) = c.chunk.heightmaps().get(HeightmapKind::MotionBlocking) else {
        return false;
    };
    #[allow(clippy::cast_sign_loss, clippy::as_conversions)]
    let top = heights.get((pos.x & 15) as u32, (pos.z & 15) as u32) as i32 + c.chunk.min_y();
    top <= pos.y
}

/// One draw of the number of candidate chunks until the next strike.
fn geometric(random: &mut LegacyRandom) -> f64 {
    let u = random.next_double().max(f64::MIN_POSITIVE);
    (u.ln() / (1.0 - THUNDER_CHANCE).ln()).floor()
}

/// Effects of a command, delivered by looking at all players of the world.
fn deliver_simple(effects: &[Effect], entities: &mut Entities, global: &Global) {
    for effect in effects {
        match effect {
            Effect::Tracking {
                entity,
                with_self,
                packet,
            } => {
                let Some(source) = entities.store.get(*entity) else {
                    continue;
                };
                let chunk = chunk_of(source.pos());
                for (id, p) in entities.players.iter() {
                    let sees = vanillite_world::tracking::Viewer {
                        entity: id,
                        view_center: p.chunk_view.center,
                        view_distance: p.chunk_view.view_distance,
                    }
                    .sees_chunk(chunk);
                    if (id == *entity && *with_self) || (id != *entity && sees) {
                        p.connection.sink.send_shared(packet);
                    }
                }
            }
            Effect::Nearby {
                pos,
                range,
                except,
                packet,
            } => {
                for (id, p) in entities.players.iter() {
                    if Some(id) == *except {
                        continue;
                    }
                    if let Some(e) = entities.store.get(id) {
                        let d = e.pos() - *pos;
                        if d.x * d.x + d.y * d.y + d.z * d.z < range * range {
                            p.connection.sink.send_shared(packet);
                        }
                    }
                }
            }
            Effect::Broadcast(message) => {
                global
                    .players
                    .broadcast_system(message.clone(), false, None);
            }
            Effect::DropItem { .. } => {}
        }
    }
    spawn_drops(effects, entities, global);
}

/// The item entities of [`Effect::DropItem`]s (`addFreshEntity`).
fn spawn_drops(effects: &[Effect], entities: &mut Entities, global: &Global) {
    for effect in effects {
        if let Effect::DropItem {
            uuid,
            pos,
            velocity,
            y_rot,
            pickup_delay,
            stack,
        } = effect
        {
            entities.spawn_item(
                global.network_ids.next(),
                *uuid,
                *pos,
                *velocity,
                *y_rot,
                vanillite_game::item_entity::ItemData::new(stack.clone(), *pickup_delay, None),
            );
        }
    }
}

/// Sends `packet` to every player of the server (`broadcastAll`).
pub(crate) fn broadcast_all<P: vanillite_protocol::Packet>(global: &Global, packet: &P) {
    global.players.broadcast(packet, None);
}

/// The game event for `immediate_respawn` (`ClientboundGameEventPacket.IMMEDIATE_RESPAWN`).
pub(crate) fn immediate_respawn_event(on: bool) -> GameEvent {
    GameEvent {
        event: 11,
        param: if on { 1.0 } else { 0.0 },
    }
}
