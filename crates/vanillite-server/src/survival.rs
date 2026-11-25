//! Survival in a world: weather and game time, the players' survival tick, their moves, death
//! and respawn, lightning, and the delivery of what the survival logic sends to other players.

use std::sync::Arc;

use vanillite_base::hash::FxHashMap;
use vanillite_base::random::{LegacyRandom, RandomSource};
use vanillite_base::{BlockPos, ChunkPos, Vec3};
use vanillite_chunk::HeightmapKind;
use vanillite_data::block::BlockState;
use vanillite_game::block::weather::Climate;
use vanillite_game::entities::Entities;
use vanillite_game::level::block_entity::{BlockEntities, BlockEntity};
use vanillite_game::player::join;
use vanillite_game::portal::PortalKind;
use vanillite_game::survival::Effect;
use vanillite_game::survival::blocks::BlockAccess;
use vanillite_game::survival::player::{self, Env, TickResult};
use vanillite_game::survival::rules::{Difficulty, GameRules, Rule};
use vanillite_game::survival::weather::{LevelWeather, WeatherPacket};
use vanillite_protocol::SharedPacket;
use vanillite_protocol::packets::play::clientbound::{
    ChangeDifficulty, ClockUpdate, GameEvent, RemoveEntities, SetTime,
};
use vanillite_protocol::text::Component;
use vanillite_protocol::types::VarIntList;
use vanillite_world::chunks::Chunks;
use vanillite_world::entity::EntityId;

use crate::global::Global;
use crate::level::LevelState;

/// Block access of a world's loaded chunks.
pub(crate) struct WorldBlocks<'a>(pub(crate) &'a Chunks);

impl BlockAccess for WorldBlocks<'_> {
    #[inline]
    fn block_state(&self, pos: BlockPos) -> Option<BlockState> {
        self.0.block(pos).map(BlockState)
    }
}

/// What the players' tick asks of the world's portals.
#[derive(Debug, Default)]
pub(crate) struct PortalRequests {
    /// Players that go through a portal now: the portal and the block it entered.
    pub(crate) travels: Vec<(EntityId, PortalKind, BlockPos)>,
    /// Gateways that took a player this tick and start their cooldown.
    pub(crate) gateways: Vec<BlockPos>,
}

/// The survival state of one world.
#[derive(Debug)]
pub(crate) struct WorldSurvival {
    pub(crate) random: LegacyRandom,
    pub(crate) weather: LevelWeather,
    /// `canHaveWeather`: sky light and no ceiling (the overworld).
    can_have_weather: bool,
    /// The overworld advances the game time.
    ticks_time: bool,
    pub(crate) effects: Vec<Effect>,
    weather_out: Vec<WeatherPacket>,
    /// The world's biomes for `isRainingAt`, shared with the level.
    climate: Option<Arc<Climate>>,
    pub(crate) min_y: i32,
    /// Tracking range of players in blocks.
    player_range: i32,
    /// Players by chunk, rebuilt when effects are delivered.
    grid: FxHashMap<ChunkPos, Vec<EntityId>>,
    scratch: Vec<EntityId>,
}

/// `Level.isRainingAt` without the sky light test (the heightmap test covers it outside of
/// caves and under transparent blocks).
pub(crate) fn raining_at(
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
        difficulty: level.difficulty().id(),
        locked: level.difficulty_locked(),
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
        (min_y, _height): (i32, u32),
        player_range: i32,
        climate: Option<Climate>,
    ) -> WorldSurvival {
        let (hashed_seed, sea_level) = (info.hashed_seed, info.sea_level);
        let overworld = dimension == "minecraft:overworld";
        let random = LegacyRandom::new(hashed_seed ^ i64::from(sea_level));
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
            player_range: player_range.max(0),
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
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn env<'a>(
        &'a mut self,
        level: &'a LevelState,
        rules: &'a GameRules,
        blocks: &'a dyn BlockAccess,
        raining_at: &'a dyn Fn(BlockPos) -> bool,
        dimension: &'a str,
        enter_gateway: &'a mut dyn FnMut(BlockPos) -> bool,
        hooks: &'a dyn vanillite_game::hooks::GameHooks,
        display_name: &'a dyn Fn(&str, u128) -> Component,
    ) -> Env<'a> {
        Env {
            types: &level.damage_types,
            facts: &level.block_facts,
            blocks,
            rules,
            difficulty: level.difficulty(),
            game_time: level.game_time(),
            min_y: self.min_y,
            dimension,
            raining_at,
            border: None,
            enter_gateway,
            random: &mut self.random,
            out: &mut self.effects,
            hooks,
            display_name,
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
        let mut no_gateway = |_| false;
        let hooks = crate::plugins::Hooks::of(&global.plugins, dimension);
        let names = |name: &str, uuid| crate::scoreboard::display_name(global, name, uuid);
        let mut env = self.env(
            level,
            &rules,
            &blocks,
            &raining,
            dimension,
            &mut no_gateway,
            &hooks,
            &names,
        );
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

    /// The entity tick of players (`ServerPlayer.tick`).
    pub(crate) fn tick_entities(&mut self, entities: &mut Entities) {
        for (_, data) in entities.players.iter_mut() {
            player::tick_entity(data);
        }
    }

    /// `ServerPlayer.doTick` of every player, in the connection tick. Players whose death
    /// animation ended or whose end credits started disappear for the others; players going
    /// through portals and the gateways they entered go to `portals`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn do_tick(
        &mut self,
        entities: &mut Entities,
        chunks: &Chunks,
        border: &vanillite_game::level::border::WorldBorder,
        block_entities: &BlockEntities,
        global: &Global,
        dimension: &str,
        portals: &mut PortalRequests,
    ) {
        let level = &global.level;
        let rules = level.rules().clone();
        let blocks = WorldBlocks(chunks);
        let (weather, climate) = (self.weather, self.climate.clone());
        let raining = |pos| raining_at(chunks, climate.as_deref(), &weather, pos);
        let mut removed = std::mem::take(&mut self.scratch);
        {
            let hooks = crate::plugins::Hooks::of(&global.plugins, dimension);
            let names = |name: &str, uuid| crate::scoreboard::display_name(global, name, uuid);
            // `EndGatewayBlock.entityInside`: a gateway takes a player unless it cools down,
            // then starts its cooldown (applied after the tick, seen at once by the others).
            let gateways = &mut portals.gateways;
            let mut enter_gateway = |pos: BlockPos| {
                let ready = matches!(
                    block_entities.get(pos),
                    Some(BlockEntity::EndGateway(gateway)) if !gateway.is_cooling_down()
                ) && !gateways.contains(&pos);
                if ready {
                    gateways.push(pos);
                }
                ready
            };
            let mut env = self.env(
                level,
                &rules,
                &blocks,
                &raining,
                dimension,
                &mut enter_gateway,
                &hooks,
                &names,
            );
            env.border = Some(border);
            let Entities { store, players, .. } = entities;
            for (id, data) in players.iter_mut() {
                let Some(mut entity) = store.get_mut(id) else {
                    continue;
                };
                match player::do_tick(&mut entity, data, &mut env) {
                    TickResult::Remove | TickResult::Credits => removed.push(id),
                    TickResult::Portal(kind, entry) => portals.travels.push((id, kind, entry)),
                    TickResult::Alive => {}
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
        crate::scoreboard::update_criteria(global, entities);
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

    /// Runs `f` on the entities of a level with a survival environment and a mob context
    /// outside the tick (commands acting on any entity); delivers the effects, applies the
    /// world changes that need only the level and removes the discarded entities.
    pub(crate) fn with_entities<R>(
        level: &mut vanillite_game::level::Level,
        global: &Global,
        dimension: &str,
        f: impl FnOnce(
            &vanillite_game::level::Level,
            &mut Entities,
            &mut Env<'_>,
            &mut vanillite_game::mob::MobContext,
        ) -> R,
    ) -> R {
        let state = &global.level;
        let rules = state.rules().clone();
        let mut random = LegacyRandom::new(state.game_time() ^ level.game_time());
        let mut out = Vec::new();
        let mut cx = vanillite_game::mob::MobContext::default();
        let mut ents = std::mem::take(&mut level.entities);
        let r = {
            let blocks = WorldBlocks(&level.chunks);
            let raining = |_| false;
            let mut no_gateway = |_| false;
            let hooks = crate::plugins::Hooks::of(&global.plugins, dimension);
            let names = |name: &str, uuid| crate::scoreboard::display_name(global, name, uuid);
            let mut env = Env {
                hooks: &hooks,
                display_name: &names,
                types: &state.damage_types,
                facts: &state.block_facts,
                blocks: &blocks,
                rules: &rules,
                difficulty: state.difficulty(),
                game_time: state.game_time(),
                min_y: level.min_y(),
                dimension,
                raining_at: &raining,
                border: None,
                enter_gateway: &mut no_gateway,
                random: &mut random,
                out: &mut out,
            };
            f(level, &mut ents, &mut env, &mut cx)
        };
        level.entities = ents;
        for action in std::mem::take(&mut cx.actions) {
            action.apply_to_level(level);
        }
        for id in std::mem::take(&mut cx.discarded) {
            level.entities.despawn(id);
        }
        deliver_simple(&out, &mut level.entities, global);
        r
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
        let mut no_gateway = |_| false;
        let hooks = crate::plugins::Hooks::of(&global.plugins, dimension);
        let names = |name: &str, uuid| crate::scoreboard::display_name(global, name, uuid);
        let mut env = Env {
            hooks: &hooks,
            display_name: &names,
            types: &level.damage_types,
            facts: &level.block_facts,
            blocks,
            rules: &rules,
            difficulty: level.difficulty(),
            game_time: level.game_time(),
            min_y: 0,
            dimension,
            raining_at: &raining,
            border: None,
            enter_gateway: &mut no_gateway,
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
    pub(crate) fn deliver(&mut self, entities: &mut Entities, global: &Global) {
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
                            sent_radius: 0,
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
                Effect::Died {
                    victim,
                    killer,
                    message,
                } => {
                    // `ServerPlayer.die`: `tellNeutralMobsThatIDied` with `forgive_dead_players`.
                    if entities.player(*victim).is_some()
                        && global.level.rules().bool(Rule::FORGIVE_DEAD_PLAYERS)
                    {
                        vanillite_game::mob::common::tell_neutral_mobs_player_died(
                            entities, *victim,
                        );
                    }
                    crate::scoreboard::died(global, entities, *victim, *killer, message.as_ref());
                }
                Effect::To { player, packet } => {
                    if let Some(p) = entities.player(*player) {
                        p.connection.sink.send_shared(packet);
                    }
                }
                Effect::DropItem { .. }
                | Effect::Experience { .. }
                | Effect::SpawnMob { .. }
                | Effect::RespawnShoulder { .. } => {}
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
}

/// `ServerPlayer.findRespawnAndUseSpawnBlock` for a respawn position without a bed or anchor:
/// a forced one where the block and the one above are free (`isPossibleToRespawnInThis`).
pub(crate) fn forced_respawn(
    chunks: &Chunks,
    respawn: &player::RespawnConfig,
) -> Option<(Vec3, f32, f32)> {
    let free = |pos: BlockPos| {
        let state = chunks.block(pos).map_or(BlockState::AIR, BlockState);
        !vanillite_game::block::is_solid(state) && !vanillite_game::block::is_liquid(state)
    };
    let pos = respawn.pos;
    (respawn.forced && free(pos) && free(pos.offset(0, 1, 0))).then(|| {
        (
            Vec3::new(
                f64::from(pos.x) + 0.5,
                f64::from(pos.y) + 0.1,
                f64::from(pos.z) + 0.5,
            ),
            respawn.yaw,
            respawn.pitch,
        )
    })
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
                        sent_radius: 0,
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
            Effect::Died {
                victim,
                killer,
                message,
            } => {
                // `ServerPlayer.die`: `tellNeutralMobsThatIDied` with `forgive_dead_players`.
                if entities.player(*victim).is_some()
                    && global.level.rules().bool(Rule::FORGIVE_DEAD_PLAYERS)
                {
                    vanillite_game::mob::common::tell_neutral_mobs_player_died(entities, *victim);
                }
                crate::scoreboard::died(global, entities, *victim, *killer, message.as_ref());
            }
            Effect::To { player, packet } => {
                if let Some(p) = entities.player(*player) {
                    p.connection.sink.send_shared(packet);
                }
            }
            Effect::DropItem { .. }
            | Effect::Experience { .. }
            | Effect::SpawnMob { .. }
            | Effect::RespawnShoulder { .. } => {}
        }
    }
    spawn_drops(effects, entities, global);
}

/// The item entities of [`Effect::DropItem`]s (`addFreshEntity`) and the experience orbs of
/// [`Effect::Experience`]s.
fn spawn_drops(effects: &[Effect], entities: &mut Entities, global: &Global) {
    for effect in effects {
        if let Effect::Experience { pos, amount } = effect {
            vanillite_game::experience_orb::with_entity_random(|random| {
                vanillite_game::experience_orb::award_in(
                    entities,
                    &global.network_ids,
                    random,
                    *pos,
                    Vec3::default(),
                    *amount,
                );
            });
        }
        if let Effect::SpawnMob {
            kind,
            pos,
            y_rot,
            velocity,
        } = effect
            && let Some(kind) = vanillite_game::mob::Kind::of(*kind)
        {
            let (uuid, seed) = vanillite_game::experience_orb::with_entity_random(|r| {
                (vanillite_game::mob::random_uuid(r), r.next_long())
            });
            let rot = vanillite_world::entity::Rotation {
                y_rot: *y_rot,
                x_rot: 0.0,
                y_head_rot: *y_rot,
            };
            let data = vanillite_game::mob::MobData::new(kind, seed);
            let id =
                vanillite_game::mob::spawn(entities, &global.network_ids, uuid, *pos, rot, data);
            if let Some(mut e) = entities.store.get_mut(id) {
                e.set_velocity(*velocity);
            }
        }
        if let Effect::RespawnShoulder { tag, pos, owner } = effect {
            // `EntityType.create(tag, LOAD)`, `setOwner`, `setPos`, `addWithUUID`.
            let seed = vanillite_game::experience_orb::with_entity_random(|r| r.next_long());
            if let Some(mut loaded) = vanillite_game::mob::nbt::load_mob(tag, seed) {
                loaded.data.owner = Some(*owner);
                let id = vanillite_game::mob::spawn(
                    entities,
                    &global.network_ids,
                    loaded.uuid,
                    *pos,
                    loaded.rot,
                    loaded.data,
                );
                if let Some(mut e) = entities.store.get_mut(id) {
                    e.set_velocity(loaded.velocity);
                }
            }
        }
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
