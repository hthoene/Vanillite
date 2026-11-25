//! Worlds as the server sees them, their tick context and cross-world hand-offs.

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use vanillite_base::ChunkPos;
use vanillite_base::hash::FxHashMap;
use vanillite_config::{TrackingRules, WorldRules};
use vanillite_game::entities::Entities;
use vanillite_game::level::Level;
use vanillite_world::chunks::{Chunks, TicketId};
use vanillite_world::entity::EntityId;
use vanillite_world::tracking::{EntityTracker, TrackerInput, TrackerOutput, Viewer};

use vanillite_base::random::XoroshiroRandom;

use crate::blocks::LevelEffects;
use crate::compute::Compute;
use crate::global::Global;
use crate::players::{FanOut, WorldBlocks, WorldPlayers};
use crate::world_setup::ChunkPackets;
use crate::world_updates::WorldUpdates;

mod travel;

/// A world (dimension) of this server: its index in Vanilla's tick order (overworld, nether,
/// end, then others).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WorldId(pub u16);

impl WorldId {
    /// The index into the server's world list.
    pub fn index(self) -> usize {
        usize::from(self.0)
    }
}

/// Work that runs on a world's owner with exclusive access to it.
pub type WorldTask = Box<dyn FnOnce(&mut ServerWorld) + Send + 'static>;

/// One loaded world, owned by exactly one thread at a time: its world thread in parallel mode,
/// the server thread in sequential mode.
///
/// The game state is the [`Level`] (chunks, entities, simulation); the world adds the
/// players' connections, entity tracking and what is sent to clients.
pub struct ServerWorld {
    id: WorldId,
    dimension: String,
    rules: Arc<WorldRules>,
    ticks: u64,
    level: Level,
    tracker: Box<dyn EntityTracker>,
    /// The tracker's result of the last tick.
    entity_updates: TrackerOutput,
    /// Buffers of the batched sending of `entity_updates`.
    fan_out: FanOut,
    /// Overload protection of the tracker.
    overload: Overload,
    viewers: Vec<Viewer>,
    /// Chunks sent to players this tick, for the tracker.
    chunks_sent: Vec<(EntityId, ChunkPos)>,
    pub(crate) players: WorldPlayers,
    packets: ChunkPackets,
    /// Sounds and crack progress of the player handlers.
    effects: LevelEffects,
    /// The random of the item entities (`Entity.random`).
    item_random: XoroshiroRandom,
    updates: WorldUpdates,
    /// The centre chunk of every player with tickets.
    player_tickets: FxHashMap<EntityId, ChunkPos>,
    /// The players' chunk ticket and simulation ticket at each centre chunk with the number
    /// of players there: equal tickets at one chunk give the same levels as one, so dense
    /// players crossing chunks do not recompute the levels of their whole view.
    center_tickets: FxHashMap<ChunkPos, (u32, TicketId, TicketId)>,
    scratch: Vec<ChunkPos>,
    /// Weather, lightning and the players' survival.
    pub(crate) survival: crate::survival::WorldSurvival,
    /// Mobs: their tick, the players' attacks and spawning.
    pub(crate) mobs: crate::mobs::WorldMobs,
    /// Loading and saving the entities of chunks.
    /// Player ids of the tick (block effects).
    player_scratch: Vec<vanillite_world::entity::EntityId>,
    /// The dimension's saved data (`<dimension>/data/`: world border, chunk tickets, raids,
    /// ...); systems register here when they come.
    pub(crate) data: vanillite_storage::saved_data::SavedDataStore,
    /// The saved chunk tickets of types Vanillite does not know, written back unchanged.
    kept_tickets: Vec<vanillite_nbt::Tag>,
    /// The portal tickets changed since the last save.
    tickets_dirty: bool,
    /// Players arriving from other worlds or respawning, waiting for their destination.
    arrivals: Vec<travel::Waiting>,
    /// Players in End gateways waiting for the exit's chunks.
    gateway_waits: Vec<travel::GatewayWait>,
    /// Portal tickets with the tick they expire.
    portal_tickets: Vec<travel::PortalTicket>,
    /// What the players' tick asked of portals.
    portal_requests: crate::survival::PortalRequests,
}

impl fmt::Debug for ServerWorld {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerWorld")
            .field("id", &self.id)
            .field("dimension", &self.dimension)
            .field("ticks", &self.ticks)
            .field("entities", &self.level.entities.store.len())
            .finish_non_exhaustive()
    }
}

impl ServerWorld {
    /// Posts the game events of the players' moves (steps, swimming, landing).
    fn post_player_game_events(&mut self) {
        let mut pending = Vec::new();
        for (id, p) in self.level.entities.players.iter_mut() {
            pending.extend(
                p.survival
                    .game_events
                    .drain(..)
                    .map(|(event, pos, affected)| (id, event, pos, affected)),
            );
        }
        for (id, event, pos, affected) in pending {
            let context = vanillite_game::level::game_event::GameEventContext {
                source: Some(id),
                affected,
            };
            self.level.game_event(event, pos, context);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        id: WorldId,
        dimension: String,
        rules: Arc<WorldRules>,
        tracker: Box<dyn EntityTracker>,
        players: WorldPlayers,
        mut level: Level,
        survival: crate::survival::WorldSurvival,
        mut mobs: crate::mobs::WorldMobs,
        data: vanillite_storage::saved_data::SavedDataStore,
    ) -> ServerWorld {
        // `dimensionType().hasEnderDragonFight()`.
        if dimension == "minecraft:the_end" {
            mobs.init_dragon_fight(&data, &mut level);
        }
        // Entity randoms are `RandomSource.create()`: seeded from the clock, not the world seed.
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as i64)
            ^ i64::from(id.0);
        if let Some(climate) = survival.climate() {
            level.set_climate(Arc::clone(climate));
        }
        let mut packets = ChunkPackets::default();
        packets.set_anti_xray(&rules.anti_xray, &dimension);
        ServerWorld {
            id,
            dimension,
            rules,
            ticks: 0,
            level,
            tracker,
            entity_updates: TrackerOutput::default(),
            fan_out: FanOut::default(),
            overload: Overload::default(),
            viewers: Vec::new(),
            chunks_sent: Vec::new(),
            players,
            packets,
            updates: WorldUpdates::default(),
            player_tickets: FxHashMap::default(),
            center_tickets: FxHashMap::default(),
            effects: LevelEffects::default(),
            item_random: XoroshiroRandom::new(seed),
            scratch: Vec::new(),
            survival,
            mobs,
            player_scratch: Vec::new(),
            data,
            kept_tickets: Vec::new(),
            tickets_dirty: false,
            arrivals: Vec::new(),
            gateway_waits: Vec::new(),
            portal_tickets: Vec::new(),
            portal_requests: crate::survival::PortalRequests::default(),
        }
    }

    /// The chunks of this world.
    pub fn chunks(&self) -> &Chunks {
        &self.level.chunks
    }

    /// The chunks of this world, mutably (tickets, blocks).
    pub fn chunks_mut(&mut self) -> &mut Chunks {
        &mut self.level.chunks
    }

    /// The game state of this world.
    pub fn level(&self) -> &Level {
        &self.level
    }

    /// The game state of this world, mutably (block changes go through
    /// [`Level::set_block`]).
    pub fn level_mut(&mut self) -> &mut Level {
        &mut self.level
    }

    /// Keeps every player's view loaded and the chunks in its simulation distance ticking:
    /// one load ticket and one simulation ticket at each centre chunk, shared by the players
    /// there, moved with a player that was alone, removed when the last one is gone.
    fn update_player_tickets(&mut self) {
        let chunks = &mut self.level.chunks;
        let level = chunks.player_ticket_level();
        let simulation = chunks.player_simulation_level();
        let centers = &mut self.center_tickets;
        for (id, player) in self.level.entities.players.iter() {
            let center = player.chunk_view.center;
            let old = self.player_tickets.get(&id).copied();
            if old == Some(center) {
                continue;
            }
            self.player_tickets.insert(id, center);
            let alone = old.and_then(|old| {
                centers
                    .get(&old)
                    .is_some_and(|entry| entry.0 == 1)
                    .then_some(old)
            });
            if let Some(entry) = centers.get_mut(&center) {
                entry.0 += 1;
            } else if let Some(old) = alone {
                // The only player at its old centre: move those tickets, touching only the
                // chunks whose level changes.
                if let Some(entry) = centers.remove(&old) {
                    chunks.move_ticket(entry.1, center);
                    chunks.move_simulation_ticket(entry.2, center);
                    centers.insert(center, entry);
                }
                continue;
            } else {
                let ticket = chunks.add_ticket(center, level);
                let sim = chunks.add_simulation_ticket(center, simulation);
                centers.insert(center, (1, ticket, sim));
            }
            // The new tickets first, then the old ones: chunks in both views stay loaded.
            if let Some(old) = old {
                release_center(chunks, centers, old);
            }
        }
        let entities = &self.level.entities;
        self.player_tickets.retain(|&id, &mut center| {
            let alive = entities.player(id).is_some();
            if !alive {
                release_center(chunks, centers, center);
            }
            alive
        });
    }

    /// The entities of this world.
    pub fn entities(&self) -> &Entities {
        &self.level.entities
    }

    /// The entities of this world, mutably.
    pub fn entities_mut(&mut self) -> &mut Entities {
        &mut self.level.entities
    }

    /// What the entity tracker produced at the end of the last tick: encode each message
    /// once and send it to its viewers ([`TrackerOutput::fan_out`]).
    pub fn entity_updates(&self) -> &TrackerOutput {
        &self.entity_updates
    }

    /// Places a player that joined the server into this world.
    pub(crate) fn join_player(&mut self, global: &Global, arrival: crate::players::Arrival) {
        let weather = self.survival.join_weather();
        let (uuid, name) = (arrival.uuid, arrival.profile.name.clone());
        let state = crate::border::level_packets(&self.level.border, global);
        self.players.join(
            &mut self.level.entities,
            global,
            self.id,
            arrival,
            &weather,
            &state,
        );
        let plugins = global.plugins.clone();
        plugins.player_join(&mut crate::plugins::PluginWorld(self), uuid, &name);
    }

    /// The UUIDs and positions of this world's players.
    fn player_positions(&self) -> Vec<(u128, [f64; 3])> {
        let entities = &self.level.entities;
        entities
            .players
            .iter()
            .filter_map(|(id, _)| {
                let e = entities.store.get(id)?;
                let pos = e.pos();
                Some((e.uuid(), [pos.x, pos.y, pos.z]))
            })
            .collect()
    }

    /// This world's id.
    pub fn id(&self) -> WorldId {
        self.id
    }

    /// Lets finished chunk jobs wake this world between ticks: `wake` must queue a
    /// [`ServerWorld::pump_chunks`] task for the world's owner.
    pub(crate) fn set_chunk_waker(&mut self, wake: vanillite_world::chunks::Waker) {
        self.level.chunks.set_waker(wake);
    }

    /// Applies finished chunk jobs and starts the next generation steps between ticks
    /// ([`vanillite_world::chunks::Chunks::pump`]).
    pub(crate) fn pump_chunks(&mut self) {
        self.level.chunks.pump();
    }

    /// The dimension key, e.g. `minecraft:overworld`.
    pub fn dimension(&self) -> &str {
        &self.dimension
    }

    /// The rules of this world. Hot paths read plain fields.
    pub fn rules(&self) -> &WorldRules {
        &self.rules
    }

    /// Ticks this world has run.
    pub fn ticks(&self) -> u64 {
        self.ticks
    }

    /// Swaps the rules; called between ticks after a config reload.
    pub(crate) fn set_rules(&mut self, rules: Arc<WorldRules>, max_chained_property: i32) {
        let mut settings = self.level.settings.clone();
        crate::world_setup::apply_simulation_rules(&mut settings, &rules, max_chained_property);
        self.level.set_settings(settings);
        self.level.chunks.set_max_relights(
            usize::try_from(rules.simulation.max_relights_per_tick).unwrap_or(usize::MAX),
        );
        self.mobs.set_rules(&rules);
        self.packets
            .set_anti_xray(&rules.anti_xray, &self.dimension);
        self.rules = rules;
    }

    /// One world tick in Vanilla's order (architecture 3.3, `ServerLevel.tick`): the players'
    /// packets (Vanilla handles them between ticks), game time, scheduled block and fluid
    /// ticks, the chunk source (player tickets, loading, generation, post-processing of new
    /// chunks, random ticks, unloading), block events, the entities in tick order, then what
    /// clients see: light and block changes, events, chunk sending, entity tracking and the
    /// connection tick.
    pub(crate) fn tick(&mut self, ctx: &mut TickContext<'_>) {
        debug_assert_eq!(ctx.world, self.id);
        let plugins = ctx.global.plugins.clone();
        let positions = plugins
            .wants(vanillite_abi::event::PLAYERS_MOVED)
            .then(|| self.player_positions());
        let started = Instant::now();
        self.players.handle_packets(
            &mut self.level,
            &self.rules,
            ctx.runs_normally,
            ctx.global,
            self.id,
            &mut WorldBlocks {
                effects: &mut self.effects,
                tracker: &*self.tracker,
            },
        );
        self.effects.flush(&self.level.entities);
        if let Some(before) = positions {
            let moves: Vec<(u128, [f64; 3], [f64; 3])> = self
                .player_positions()
                .into_iter()
                .filter_map(|(uuid, to)| {
                    let (_, from) = before.iter().find(|(u, _)| *u == uuid)?;
                    (*from != to).then_some((uuid, *from, to))
                })
                .collect();
            plugins.players_moved(&mut crate::plugins::PluginWorld(self), &moves);
        }
        plugins.run_world_tasks(&mut crate::plugins::PluginWorld(self), ctx.tick);
        self.survival.moves(
            &mut self.level.entities,
            &self.level.chunks,
            ctx.global,
            &self.dimension,
        );
        self.post_player_game_events();
        self.mobs.player_actions(
            &mut self.level,
            &mut self.survival,
            ctx.global,
            &self.dimension,
        );
        for id in self.players.take_respawns() {
            self.respawn(ctx, id);
        }
        self.tick_travel(ctx.global);
        if self.id.0 == 0 && ctx.runs_normally {
            // `ServerFunctionManager.tick`: the data pack functions run as the server in the
            // overworld before the clocks and the worlds tick.
            crate::commands::tick_functions(&mut self.level, ctx.global, self.id);
        }
        self.players_inside_blocks();
        // The rules block behaviour reads (sculk shriekers).
        self.level.settings.peaceful =
            ctx.global.level.difficulty() == vanillite_game::survival::rules::Difficulty::Peaceful;
        self.level.settings.spawn_wardens = ctx
            .global
            .level
            .rules()
            .bool(vanillite_game::survival::rules::Rule::SPAWN_WARDENS);
        self.level.set_handling_tick(true);
        if ctx.runs_normally {
            // `advanceWeatherCycle`, then `tickTime`: the overworld advances the shared game
            // time, every world runs on it. The world border moves first.
            self.level.border.tick();
            self.survival.tick_level(&self.level.entities, ctx.global);
            self.level.set_weather(self.survival.weather);
            self.level
                .settings
                .apply_game_rules(&ctx.global.level.rules());
            self.level.settings.difficulty = ctx.global.level.difficulty();
            let level = &mut self.level;
            level.set_game_time(ctx.global.level.game_time());
            if self.id.0 == 0 {
                crate::commands::tick_scheduled(level, ctx.global, self.id);
            }
            let level = &mut self.level;
            level.run_scheduled_ticks();
            self.survival
                .tick_thunder(&mut self.level.entities, &self.level.chunks, ctx.global);
        }
        self.update_player_tickets();
        let level = &mut self.level;
        // `/save-off` (`ServerLevel.noSave`): chunks without tickets stay loaded.
        level.chunks.set_no_save(
            !ctx.global
                .level
                .auto_save
                .load(std::sync::atomic::Ordering::Relaxed),
        );
        level.tick_chunk_runtime();
        level.post_process();
        if ctx.runs_normally {
            let spawn_point = (self.dimension == "minecraft:overworld").then(|| {
                let (pos, _) = self.players.level.spawn;
                vanillite_base::BlockPos::containing(pos.x, pos.y, pos.z)
            });
            self.mobs.spawn(
                level,
                &self.survival,
                ctx.global,
                &self.dimension,
                spawn_point,
            );
            level.tick_chunks(&mut self.scratch);
            level.run_block_events();
        }
        level.set_handling_tick(false);
        if ctx.runs_normally {
            self.ticks += 1;
            self.tick_entities(ctx.global);
            self.level.tick_block_entities();
            self.mobs.apply_magic_hurts(
                &mut self.level,
                &mut self.survival,
                ctx.global,
                &self.dimension,
            );
        }
        self.players.tick_entities(&mut self.level.entities);
        if ctx.runs_normally {
            self.survival.tick_entities(
                &mut self.level.entities,
                &self.level.chunks,
                ctx.global,
                &self.dimension,
            );
        }
        let light = self.updates.light_changes();
        self.level.chunks.take_light_changes(light);
        let mut changed = std::mem::take(&mut self.level.output().changed);
        self.packets
            .invalidate(&mut self.level.chunks, light, &mut changed);
        self.level.output().changed = changed;
        for pos in &self.level.output().block_entity_updates {
            self.packets.invalidate_chunk(pos.chunk());
        }
        self.updates.broadcast(&mut self.level, ctx.global);
        WorldPlayers::send_chunks(
            &mut self.level.entities,
            &self
                .packets
                .source(&self.level.chunks, &self.level.block_entities),
        );
        self.track_entities();
        WorldPlayers::send_entity_updates(
            &self.level.entities,
            &self.entity_updates,
            ctx.compute,
            &mut self.fan_out,
        );
        let mut portals = std::mem::take(&mut self.portal_requests);
        self.survival.do_tick(
            &mut self.level.entities,
            &self.level.chunks,
            &self.level.border,
            &self.level.block_entities,
            ctx.global,
            &self.dimension,
            &mut portals,
        );
        for pos in portals.gateways.drain(..) {
            let state = self.level.block(pos);
            vanillite_game::block::portal::trigger_gateway_cooldown(&mut self.level, pos, state);
        }
        for (id, kind, entry) in portals.travels.drain(..) {
            self.enter_portal(ctx, id, kind, entry);
        }
        self.portal_requests = portals;
        self.players.tick_connections(
            &mut self.level,
            ctx.tick,
            ctx.global,
            self.id,
            &mut WorldBlocks {
                effects: &mut self.effects,
                tracker: &*self.tracker,
            },
        );
        self.effects.flush(&self.level.entities);
        crate::scoreboard::flush(ctx.global);
        self.overload
            .update(started.elapsed(), &self.rules.tracking, &self.dimension);
        // Save rounds take only the time left in the tick (#47).
        let left = ctx.deadline.saturating_sub(ctx.global.clock.now());
        self.level
            .chunks
            .autosave_step(std::time::Duration::from_nanos(left));
    }

    /// `applyEffectsFromBlocks` after the players' moves: pressure plates, tripwire and other
    /// blocks they stand in react (`Entity.checkInsideBlocks`).
    fn players_inside_blocks(&mut self) {
        self.player_scratch.clear();
        self.player_scratch
            .extend(self.level.entities.players.iter().map(|(id, _)| id));
        for &id in &self.player_scratch {
            vanillite_game::block::plate::check_inside_blocks(&mut self.level, id);
        }
    }

    /// `entityTickList.forEach(tickNonPassenger)`: every entity in tick order; players move
    /// through their packets.
    fn tick_entities(&mut self, global: &Global) {
        self.mobs
            .tick_dragon_fight(&mut self.level, &mut self.survival, global, &self.dimension);
        self.mobs
            .tick_entities(&mut self.level, &mut self.survival, global, &self.dimension);
        let level = &mut self.level;
        let min_y = level.min_y();
        crate::blocks::tick_items(
            &mut level.entities,
            &level.chunks,
            &mut self.item_random,
            min_y,
            i16::try_from(self.rules.items.despawn_ticks).unwrap_or(i16::MAX),
        );
    }

    /// Runs the entity tracker over this tick's state and resets the per-tick sync state.
    fn track_entities(&mut self) {
        self.viewers.clear();
        self.viewers.extend(
            self.level
                .entities
                .players
                .iter()
                .map(|(entity, player)| Viewer {
                    entity,
                    view_center: player.chunk_view.center,
                    view_distance: player.chunk_view.view_distance,
                }),
        );
        self.entity_updates.clear();
        self.chunks_sent.clear();
        let mut sent = Vec::new();
        for (entity, player) in self.level.entities.players.iter_mut() {
            player.connection.chunks.take_sent(&mut sent);
            self.chunks_sent
                .extend(sent.drain(..).map(|pos| (entity, pos)));
        }
        let entities = &self.level.entities;
        let input = TrackerInput {
            entities: &entities.store,
            viewers: &self.viewers,
            chunk_pending: &|player, chunk| WorldPlayers::chunk_pending(entities, player, chunk),
            chunks_sent: &self.chunks_sent,
            update_scale: self.overload.scale,
        };
        self.tracker.update(&input, &mut self.entity_updates);
        self.level.entities.store.clear_removed();
        self.level.entities.store.clear_sync_flags();
    }

    /// Runs `then` once the running save round of this world is committed.
    pub(crate) fn when_saved(&mut self, then: vanillite_world::chunks::SavedCallback) {
        self.level.chunks.when_saved(then);
    }

    /// Whether the world has saved tickets to load before the first tick.
    pub(crate) fn has_saved_tickets(&self) -> bool {
        !self.level.forced.is_empty() || !self.portal_tickets.is_empty()
    }

    /// One step of `prepareLevels` for the saved tickets: runs the chunk system without a game
    /// tick and returns how many ticketed chunks are not full yet.
    pub(crate) fn prepare_step(&mut self) -> usize {
        self.level.chunks.tick();
        self.level.chunks.pending_full()
    }

    /// Saves the world like Vanilla (`saveEverything`): the players always, the chunks and the
    /// dimension's saved data unless `skip_chunks` (`/save-off`). `flush` waits until the
    /// chunks are written and committed (shutdown, `/save-all flush`); otherwise a save round
    /// starts that captures them from the remaining time of the next ticks.
    pub(crate) fn save(&mut self, flush: bool, skip_chunks: bool) {
        log::debug!("Saving {} (flush: {flush})", self.dimension);
        self.players.save_all(&self.level.entities);
        if !skip_chunks {
            if let Some(border) = self.level.border.take_dirty() {
                self.data.write(crate::border::SAVED_DATA, border.to_nbt());
            }
            if self.level.forced.take_dirty() | std::mem::take(&mut self.tickets_dirty) {
                crate::chunk_tickets::save(&self.data, &self.level, &self.saved_tickets());
            }
            self.data.save();
            self.level.save(flush);
        }
    }
}

/// Overload protection of the entity tracker (`performance.entity-tracking.overload-*`):
/// scales the movement update intervals with the smoothed world tick time.
#[derive(Debug)]
struct Overload {
    /// Smoothed world tick time in milliseconds.
    average_millis: f64,
    /// The factor on the update intervals (1 = Vanilla).
    scale: i32,
    /// Ticks since the scale last changed.
    since_change: u32,
}

impl Default for Overload {
    fn default() -> Overload {
        Overload {
            average_millis: 0.0,
            scale: 1,
            since_change: 0,
        }
    }
}

impl Overload {
    /// Ticks between two steps of the scale.
    const STEP_TICKS: u32 = 20;

    fn update(&mut self, elapsed: Duration, rules: &TrackingRules, world: &str) {
        let millis = elapsed.as_secs_f64() * 1000.0;
        self.average_millis = self.average_millis * 0.9 + millis * 0.1;
        self.since_change = self.since_change.saturating_add(1);
        let Some(target) = rules.overload_tick_millis.get() else {
            self.scale = 1;
            return;
        };
        let max = i32::try_from(rules.overload_max_interval_scale)
            .unwrap_or(1)
            .max(1);
        let target = f64::from(target);
        let scale = if self.average_millis > target && self.scale < max {
            self.scale + 1
        } else if self.average_millis < target * 0.6 && self.scale > 1 {
            self.scale - 1
        } else {
            self.scale.min(max)
        };
        if scale != self.scale && self.since_change >= Self::STEP_TICKS {
            if scale > self.scale {
                log::warn!(
                    "{world} is overloaded ({:.1} ms per tick): entity movement updates are sent \
                     every {scale}x their interval",
                    self.average_millis
                );
            } else {
                log::info!("{world}: entity movement updates every {scale}x their interval");
            }
            self.scale = scale;
            self.since_change = 0;
        }
    }
}

/// What a world tick gets besides its world.
pub struct TickContext<'a> {
    /// The world being ticked.
    pub world: WorldId,
    /// The server tick count.
    pub tick: u64,
    /// Whether game elements run this tick (`false` while frozen).
    pub runs_normally: bool,
    /// When the tick should end (clock nanoseconds); autosave uses the time left.
    pub deadline: u64,
    /// The compute pool for parallel phases.
    pub compute: &'a Compute,
    /// Server-wide state.
    pub global: &'a Global,
    /// The other worlds, when they are accessible right now (sequential mode).
    others: Option<OtherWorlds<'a>>,
    /// Cross-world work that runs after the world phase.
    outbox: &'a mut Vec<(WorldId, WorldTask)>,
}

struct OtherWorlds<'a> {
    before: &'a mut [ServerWorld],
    after: &'a mut [ServerWorld],
}

impl fmt::Debug for TickContext<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TickContext")
            .field("world", &self.world)
            .field("tick", &self.tick)
            .field("immediate", &self.others.is_some())
            .finish_non_exhaustive()
    }
}

impl<'a> TickContext<'a> {
    pub(crate) fn new(
        world: WorldId,
        tick: u64,
        runs_normally: bool,
        deadline: u64,
        compute: &'a Compute,
        global: &'a Global,
        outbox: &'a mut Vec<(WorldId, WorldTask)>,
    ) -> Self {
        TickContext {
            world,
            tick,
            runs_normally,
            deadline,
            compute,
            global,
            others: None,
            outbox,
        }
    }

    /// Gives the context direct access to the other worlds (sequential mode). `before` holds
    /// the worlds with lower ids, `after` those with higher ids.
    pub(crate) fn with_others(
        mut self,
        before: &'a mut [ServerWorld],
        after: &'a mut [ServerWorld],
    ) -> Self {
        self.others = Some(OtherWorlds { before, after });
        self
    }

    /// Runs `op` on another world: immediately when that world is accessible (sequential
    /// mode, exact Vanilla order), otherwise after the world phase of this tick, before the
    /// target's next tick (parallel mode). This is the one hand-off primitive: entities and
    /// players changing worlds are moved by such an operation into the target's inbox.
    pub fn in_world(
        &mut self,
        target: WorldId,
        op: impl FnOnce(&mut ServerWorld) + Send + 'static,
    ) {
        let current = self.world.index();
        let index = target.index();
        let world = self.others.as_mut().and_then(|others| {
            if index < current {
                others.before.get_mut(index)
            } else {
                index
                    .checked_sub(current + 1)
                    .and_then(|i| others.after.get_mut(i))
            }
        });
        match world {
            Some(world) => op(world),
            None => self.outbox.push((target, Box::new(op))),
        }
    }
}

/// Drops one player from the tickets at `pos`; the last one removes them.
fn release_center(
    chunks: &mut Chunks,
    centers: &mut FxHashMap<ChunkPos, (u32, TicketId, TicketId)>,
    pos: ChunkPos,
) {
    if let Some(entry) = centers.get_mut(&pos) {
        entry.0 = entry.0.saturating_sub(1);
        if entry.0 == 0 {
            let (_, ticket, sim) = *entry;
            centers.remove(&pos);
            chunks.remove_ticket(ticket);
            chunks.remove_simulation_ticket(sim);
        }
    }
}
