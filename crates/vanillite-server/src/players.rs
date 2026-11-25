//! Players in a world: joining, their packets, chunk sending, entity updates and leaving. The
//! network layer hands over connections; the handlers themselves live in
//! `vanillite_game::player`.

use std::sync::{Arc, Mutex};

use vanillite_base::BlockPos;
use vanillite_base::hash::FxHashSet;
use vanillite_base::slot_map::SlotKey;
use vanillite_base::{ChunkPos, Vec3};
use vanillite_config::{GameMode, WorldRules};
use vanillite_game::advancements::PlayerAdvancements;
use vanillite_game::entities::Entities;
use vanillite_game::inventory::{Drop, PlayerInventory};
use vanillite_game::item::ItemStack;
use vanillite_game::level::Level;
use vanillite_game::player::chat::ChatInbox;
use vanillite_game::player::encode::EQUIPMENT_SLOTS;
use vanillite_game::player::encode::encode_update;
use vanillite_game::player::join::{self, InfoEntry, LevelInfo};
use vanillite_game::player::play::{Actor, PlayContext, throw_item};
use vanillite_game::player::{
    Body, ChatContext, ChunkSource, ChunkView, ClientInformation, Connection, GameProfile, Handled,
    MoveContext, PacketSink, PlayerData, PlayerState, effective_view_distance,
};
use vanillite_game::stats::{Stat, StatType, StatsCounter, custom};
use vanillite_game::survival::player::Survival;
use vanillite_game::survival::rules::Rule;
use vanillite_net::{Latency, Outbound};
use vanillite_protocol::SharedPacket;
use vanillite_protocol::compress::Codec;
use vanillite_protocol::frame::encode_frame;
use vanillite_protocol::packets::play::clientbound::{
    BlockChangedAck, SetEquipment, SetHeldSlot, SwingAnimation, TakeItemEntity,
};
use vanillite_protocol::packets::play::serverbound::Hand;
use vanillite_protocol::text::Component;
use vanillite_storage::player_data::{self, JsonFile, PlayerDataStore};
use vanillite_world::chunks::Chunks;
use vanillite_world::entity::{EntityFlags, EntityId, EntityRef, Pose, Rotation};
use vanillite_world::tracking::{EntityTracker, TrackerOutput};

use crate::blocks::{LevelEffects, WorldLevel};

use crate::compute::Compute;
use crate::global::Global;
use crate::player_list::Uuid;

/// Packets handled per player and tick; the rest waits for the next tick (SEC-16).
const PACKETS_PER_TICK: usize = 128;
/// Ticks between latency updates of the tab list (Vanilla's `PlayerList.tick`).
const LATENCY_UPDATE_TICKS: u64 = 600;

/// Viewers per fan-out job at least: fewer are sent on the world thread.
const FAN_OUT_VIEWERS_PER_JOB: usize = 64;
/// Pairs (message, viewer) per fan-out job at least: fewer are copied on the world thread.
const FAN_OUT_PAIRS_PER_JOB: usize = 64 * 1024;

/// One job's batches and their sizes for a range of viewer slots.
type BatchPart<'a> = (&'a mut [Vec<u8>], &'a mut [usize]);

/// Encode once, one batch per viewer: the tracker's messages are encoded once into frames in
/// an arena (adjacent messages with the same viewers merged into one run), then jobs over
/// ranges of runs with about the same number of (run, viewer) pairs append each run to the
/// batches of its viewers, one batch per job and viewer, in parallel on the compute pool.
/// Each viewer's batches are queued in job order, so it gets its packets in message order.
/// The appends only touch the ends of the viewers' batches, which stay in cache.
#[derive(Debug, Default)]
pub(crate) struct FanOut {
    /// Frames of this tick, built once for the viewers' compression threshold.
    arena: Vec<u8>,
    codec: Codec,
    /// Runs: arena range and the index of the message whose viewers receive it.
    runs: Vec<(u32, u32, u32)>,
    /// Viewer slot by entity index (`u32::MAX` = no viewer).
    slot_of: Vec<u32>,
    /// Batches per job, by viewer slot.
    batches: Vec<Vec<Vec<u8>>>,
    /// Size of each batch at the last tick, by job and viewer slot: its capacity this tick.
    last_sizes: Vec<Vec<usize>>,
}

impl FanOut {
    fn run(&mut self, entities: &Entities, updates: &TrackerOutput, compute: &Compute) {
        let messages = updates.messages();
        if messages.is_empty() {
            return;
        }
        let threshold = entities
            .players
            .iter()
            .next()
            .and_then(|(_, player)| player.connection.sink.frame_threshold());

        // Viewer slots: the players of this world with their sinks.
        let sinks: Vec<&Arc<dyn PacketSink>> = entities
            .players
            .iter()
            .map(|(_, player)| &player.connection.sink)
            .collect();
        let viewers_n = sinks.len();
        self.slot_of.fill(u32::MAX);
        for (slot, (id, _)) in entities.players.iter().enumerate() {
            let index = id.raw().index() as usize;
            if self.slot_of.len() <= index {
                self.slot_of.resize(index + 1, u32::MAX);
            }
            self.slot_of[index] = u32::try_from(slot).unwrap_or(u32::MAX);
        }
        let ids: Vec<EntityId> = entities.players.iter().map(|(id, _)| id).collect();
        let slot_of = &self.slot_of;
        let slot = |viewer: &EntityId| -> usize {
            match slot_of.get(viewer.raw().index() as usize) {
                Some(&s) if ids.get(s as usize) == Some(viewer) => s as usize,
                _ => usize::MAX,
            }
        };

        // Encode once into runs.
        self.arena.clear();
        self.runs.clear();
        let mut pairs = 0usize;
        let mut last: Option<&[EntityId]> = None;
        for (index, message) in messages.iter().enumerate() {
            let Some(packets) = encode_update(message, entities) else {
                continue;
            };
            let start = self.arena.len();
            for packet in &packets {
                let before = self.arena.len();
                if encode_frame(packet.body(), threshold, &mut self.codec, &mut self.arena).is_err()
                {
                    self.arena.truncate(before);
                }
            }
            let end = self.arena.len();
            let (Ok(start32), Ok(end32), Ok(index32)) = (
                u32::try_from(start),
                u32::try_from(end),
                u32::try_from(index),
            ) else {
                break;
            };
            let viewers = updates.viewers_of(message);
            // `PosRot` + `HeadRot` and the like share one viewer list: one copy per viewer.
            if let (Some(prev), Some(run)) = (last, self.runs.last_mut())
                && std::ptr::eq(prev, viewers)
                && run.1 == start32
            {
                run.1 = end32;
                continue;
            }
            self.runs.push((start32, end32, index32));
            pairs += viewers.len();
            last = Some(viewers);
        }
        if pairs == 0 || viewers_n == 0 {
            return;
        }
        let runs = &self.runs;
        let arena = &self.arena;
        let viewers_of = |run: &(u32, u32, u32)| {
            messages
                .get(run.2 as usize)
                .map_or(&[][..], |message| updates.viewers_of(message))
        };

        // Jobs over ranges of runs with about the same number of pairs.
        let workers = compute.scheduler().workers() + 1;
        let jobs = workers.min(pairs.div_ceil(FAN_OUT_PAIRS_PER_JOB)).max(1);
        let per_job = pairs.div_ceil(jobs);
        let mut ranges = Vec::with_capacity(jobs);
        let (mut first, mut acc) = (0, 0);
        for (i, run) in runs.iter().enumerate() {
            acc += viewers_of(run).len();
            if acc >= per_job {
                ranges.push(first..i + 1);
                first = i + 1;
                acc = 0;
            }
        }
        if first < runs.len() {
            ranges.push(first..runs.len());
        }
        let jobs = ranges.len();
        self.batches
            .resize_with(jobs.max(self.batches.len()), Vec::new);
        self.last_sizes
            .resize_with(jobs.max(self.last_sizes.len()), Vec::new);
        for (batches, sizes) in self.batches.iter_mut().zip(&mut self.last_sizes) {
            batches.resize_with(viewers_n, Vec::new);
            sizes.resize(viewers_n, 0);
        }
        let fill = |range: std::ops::Range<usize>, batches: &mut [Vec<u8>], sizes: &[usize]| {
            for run in &runs[range] {
                let bytes = &arena[run.0 as usize..run.1 as usize];
                for viewer in viewers_of(run) {
                    let s = slot(viewer);
                    if let Some(batch) = batches.get_mut(s) {
                        if batch.capacity() == 0 {
                            let hint = sizes.get(s).copied().unwrap_or(0);
                            batch.reserve(hint + hint / 4);
                        }
                        batch.extend_from_slice(bytes);
                    }
                }
            }
        };
        let per_job = self
            .batches
            .iter_mut()
            .zip(&self.last_sizes)
            .zip(ranges.iter().cloned());
        if jobs == 1 {
            for ((batches, sizes), range) in per_job {
                fill(range, batches, sizes);
            }
        } else {
            compute.scope(|scope| {
                let fill = &fill;
                for ((batches, sizes), range) in per_job {
                    scope.spawn(move || fill(range, batches, sizes));
                }
            });
        }

        // Queue each viewer's batches in job order, in parallel over ranges of viewers.
        let send_jobs = workers
            .min(viewers_n.div_ceil(FAN_OUT_VIEWERS_PER_JOB))
            .max(1);
        let per_range = viewers_n.div_ceil(send_jobs);
        let mut parts: Vec<Vec<BatchPart<'_>>> =
            (0..send_jobs).map(|_| Vec::with_capacity(jobs)).collect();
        for (batches, sizes) in self
            .batches
            .iter_mut()
            .zip(self.last_sizes.iter_mut())
            .take(jobs)
        {
            let chunks = batches[..viewers_n]
                .chunks_mut(per_range)
                .zip(sizes[..viewers_n].chunks_mut(per_range));
            for (part, chunk) in parts.iter_mut().zip(chunks) {
                part.push(chunk);
            }
        }
        let send = |first: usize, mut part: Vec<BatchPart<'_>>| {
            let len = part.first().map_or(0, |(batches, _)| batches.len());
            for i in 0..len {
                let Some(sink) = sinks.get(first + i) else {
                    continue;
                };
                for (batches, sizes) in &mut part {
                    let batch = std::mem::take(&mut batches[i]);
                    sizes[i] = batch.len();
                    if !batch.is_empty() {
                        sink.send_batch(batch, threshold);
                    }
                }
            }
        };
        if send_jobs == 1 {
            for part in parts {
                send(0, part);
            }
            return;
        }
        compute.scope(|scope| {
            let send = &send;
            for (index, part) in parts.into_iter().enumerate() {
                scope.spawn(move || send(index * per_range, part));
            }
        });
    }
}

/// A network connection as the game's packet sink.
pub(crate) struct NetSink {
    out: Outbound,
    latency: Latency,
}

impl NetSink {
    pub(crate) fn new(out: Outbound, latency: Latency) -> NetSink {
        NetSink { out, latency }
    }
}

impl PacketSink for NetSink {
    fn send_body(&self, body: Vec<u8>) -> bool {
        self.out.send_body(body)
    }

    fn send_shared(&self, packet: &Arc<SharedPacket>) -> bool {
        self.out.send_shared(packet)
    }

    fn send_batch(&self, frames: Vec<u8>, threshold: Option<usize>) -> bool {
        self.out.send_batch(frames, threshold)
    }

    fn frame_threshold(&self) -> Option<usize> {
        self.out.threshold()
    }

    fn close(&self) {
        self.out.close();
    }

    fn is_closed(&self) -> bool {
        self.out.is_closed()
    }

    fn latency_ms(&self) -> u32 {
        self.latency.millis()
    }
}

/// A player on the way into a world.
pub(crate) struct Arrival {
    pub(crate) uuid: u128,
    pub(crate) profile: GameProfile,
    pub(crate) client: ClientInformation,
    pub(crate) game_mode: GameMode,
    pub(crate) sink: Arc<dyn PacketSink>,
    pub(crate) inbound: vanillite_protocol::InboundReceiver,
    pub(crate) memory_connection: bool,
    /// A virtual connection (test bot, Bedrock player): no chat session, chat is unsigned.
    pub(crate) virtual_connection: bool,
    pub(crate) saved: Option<player_data::PlayerData>,
    /// The statistics file.
    pub(crate) stats: Option<String>,
    /// The advancements file.
    pub(crate) advancements: Option<String>,
    pub(crate) address: String,
    pub(crate) inbox: Arc<Mutex<ChatInbox>>,
}

/// The players' side of a world.
pub(crate) struct WorldPlayers {
    pub(crate) level: LevelInfo,
    store: PlayerDataStore,
    view_distance: i32,
    leaving: Vec<(EntityId, Option<Box<Component>>)>,
    /// Players that left this tick; their tab list entries go after the entity removal.
    left: Vec<u128>,
    /// Players with chat or command requests this tick.
    chatting: Vec<EntityId>,
    /// Dead players that asked to respawn this tick.
    respawns: Vec<EntityId>,
}

fn chunk_of(pos: Vec3) -> ChunkPos {
    ChunkPos::new(
        vanillite_base::math::floor(pos.x) >> 4,
        vanillite_base::math::floor(pos.z) >> 4,
    )
}

impl WorldPlayers {
    pub(crate) fn new(level: LevelInfo, store: PlayerDataStore) -> WorldPlayers {
        WorldPlayers {
            view_distance: level.view_distance,
            level,
            store,
            leaving: Vec::new(),
            left: Vec::new(),
            chatting: Vec::new(),
            respawns: Vec::new(),
        }
    }

    /// The players that asked to respawn since the last call.
    pub(crate) fn take_respawns(&mut self) -> Vec<EntityId> {
        std::mem::take(&mut self.respawns)
    }

    /// The world's view distance in chunks.
    pub(crate) fn view_distance(&self) -> i32 {
        self.view_distance
    }

    /// `placeNewPlayer`: spawns the player and sends the join sequence. The player is already
    /// in the global player list.
    pub(crate) fn join(
        &mut self,
        entities: &mut Entities,
        global: &Global,
        world: crate::world::WorldId,
        arrival: Arrival,
        weather: &[vanillite_protocol::packets::play::clientbound::GameEvent],
        state: &join::LevelPackets,
    ) {
        let survival = arrival
            .saved
            .as_ref()
            .map(|saved| Survival::load(&saved.other))
            .unwrap_or_default();
        let (pos, rot, on_ground) = match &arrival.saved {
            Some(saved) => (
                Vec3::new(saved.pos[0], saved.pos[1], saved.pos[2]),
                (saved.rotation[0], saved.rotation[1]),
                saved.on_ground,
            ),
            None => (self.level.spawn.0, (self.level.spawn.1, 0.0), true),
        };
        let center = chunk_of(pos);
        let view_distance = effective_view_distance(&arrival.client, self.view_distance);
        let sink = arrival.sink;
        let name = arrival.profile.name.clone();
        let inventory = arrival
            .saved
            .as_ref()
            .map(|saved| PlayerInventory::load(&saved.other))
            .unwrap_or_default();
        let post_effects = arrival
            .saved
            .as_ref()
            .map(|saved| load_post_effects(&saved.other))
            .unwrap_or_default();
        let mut stats = match arrival.stats.as_deref().map(StatsCounter::from_json) {
            Some(Ok(stats)) => stats,
            Some(Err(e)) => {
                log::error!("Couldn't parse statistics of {name}: {e}");
                StatsCounter::default()
            }
            None => StatsCounter::default(),
        };
        stats.mark_all_dirty();
        let advancements = PlayerAdvancements::load(
            global.systems.advancements(),
            arrival.advancements.as_deref(),
        );
        let recipe_book = match (
            arrival
                .saved
                .as_ref()
                .and_then(|saved| saved.other.get("recipeBook"))
                .and_then(vanillite_nbt::Tag::as_compound),
            vanillite_game::player::play::game_data(),
        ) {
            (Some(c), Some(d)) => {
                vanillite_game::recipe::book::PlayerRecipeBook::load(c, &d.recipes.book)
            }
            _ => vanillite_game::recipe::book::PlayerRecipeBook::default(),
        };
        let data = PlayerData {
            recipe_book,
            stats,
            advancements,
            inventory,
            last_equipment: std::array::from_fn(|_| ItemStack::EMPTY),
            profile: arrival.profile,
            game_mode: arrival.game_mode,
            previous_game_mode: None,
            client: arrival.client,
            chunk_view: ChunkView {
                center,
                view_distance,
            },
            connection: Connection::new(
                Arc::clone(&sink),
                arrival.inbound,
                pos,
                arrival.memory_connection,
                arrival.virtual_connection,
                &global.chat,
                Arc::clone(&arrival.inbox),
            ),
            survival,
            post_effects,
        };
        let id = entities.spawn_player(
            &global.network_ids,
            arrival.uuid,
            pos,
            Rotation {
                y_rot: rot.0,
                x_rot: rot.1,
                y_head_rot: rot.0,
            },
            data,
        );
        let Some(mut entity) = entities.store.get_mut(id) else {
            return;
        };
        entity.set_flags(EntityFlags::ON_GROUND, on_ground);
        let network_id = entity.as_ref().network_id().0;
        let mut body = body_of(entities, id);
        let Some(player) = entities.player_mut(id) else {
            return;
        };
        log::info!(
            "{name}[{}] logged in with entity id {network_id} at ({}, {}, {})",
            arrival.address,
            pos.x,
            pos.y,
            pos.z
        );
        let immediate_respawn = global.level.rules().bool(Rule::IMMEDIATE_RESPAWN);
        join::send_login(
            sink.as_ref(),
            &self.level,
            network_id,
            player,
            global.chat.enforce_secure,
            !immediate_respawn,
        );
        sink.send(&crate::survival::change_difficulty(&global.level));
        join::send_abilities(sink.as_ref(), player.game_mode);
        let Some(global_arc) = global.arc() else {
            return;
        };
        let source = crate::commands::CommandSource::new(
            global_arc,
            crate::commands::SourceKind::Player {
                uuid: arrival.uuid,
                name: name.clone(),
                sink: Arc::clone(&sink),
            },
            crate::commands::permission_level(global, arrival.uuid),
            world,
            pos,
            rot,
            None,
        );
        crate::commands::send_permissions(global, sink.as_ref(), network_id, &source);
        crate::scoreboard::send_all(global, sink.as_ref(), arrival.uuid);
        global.players.broadcast_system(
            join::joined_message(crate::scoreboard::display_name(global, &name, arrival.uuid)),
            false,
            None,
        );
        let connection = &mut player.connection;
        connection
            .movement
            .teleport(&mut body, pos, rot.0, rot.1, sink.as_ref());
        // The tab list: everyone to the new player (itself included), the new player to everyone else.
        let entries = global.players.info_entries();
        sink.send(&join::player_info_add(&entries));
        if let Some(own) = entries.iter().find(|e| e.uuid == arrival.uuid) {
            global
                .players
                .broadcast(&join::player_info_add([own]), Some(Uuid(arrival.uuid)));
        }
        join::send_level_info(
            sink.as_ref(),
            &self.level,
            state,
            &crate::survival::full_time_sync(&global.level),
            weather,
        );
        connection
            .chunks
            .update_view(center, view_distance, sink.as_ref());
        // `placeNewPlayer`: the selected slot, then `initInventoryMenu`.
        sink.send(&SetHeldSlot {
            slot: i32::try_from(player.inventory.selected).unwrap_or(0),
        });
        sink.send_shared(&global.recipes_packet);
        if let Some(d) = vanillite_game::player::play::game_data() {
            let (settings, book) = player.recipe_book.initial(&d.recipes.book);
            sink.send(&settings);
            sink.send(&book);
        }
        player.inventory.send_all(sink.as_ref());
        for (last, slot) in player.last_equipment.iter_mut().zip(EQUIPMENT_SLOTS) {
            *last = player.inventory.equipment(slot).clone();
        }
        // `sendPostEffects` of the first tick (an empty list changes nothing on the client).
        if !player.post_effects.is_empty() {
            crate::commands::send_post_effects(player);
        }
    }

    /// Handles the queued packets of all players (Vanilla handles them between ticks), applies
    /// their effects to the entities and moves chunk views.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn handle_packets(
        &mut self,
        level: &mut Level,
        rules: &WorldRules,
        runs_normally: bool,
        global: &Global,
        world: crate::world::WorldId,
        blocks: &mut WorldBlocks<'_>,
    ) {
        let ctx = MoveContext {
            runs_normally,
            rules: &rules.movement,
        };
        let protection = self.spawn_protection(global);
        // The player kind data leaves the level while the packets are handled, so the block
        // handlers can change the level (`setBlock` with its updates) meanwhile. Nothing in
        // between adds or removes players.
        let mut players = std::mem::take(&mut level.entities.players);
        let spectators: FxHashSet<EntityId> = players
            .iter()
            .filter(|(_, p)| p.game_mode == GameMode::Spectator)
            .map(|(id, _)| id)
            .collect();
        let mut swings: Vec<(EntityId, Hand)> = Vec::new();
        for (id, player) in players.iter_mut() {
            let Some(entity) = level.entities.store.get(id) else {
                continue;
            };
            let chat = ChatContext {
                uuid: entity.uuid(),
                name: &player.profile.name,
                settings: &global.chat,
            };
            let flags = entity.flags();
            let rot = entity.rot();
            let before = PlayerState {
                body: Body {
                    pos: entity.pos(),
                    velocity: entity.velocity(),
                    y_rot: rot.y_rot,
                    x_rot: rot.x_rot,
                    on_ground: flags.contains(EntityFlags::ON_GROUND),
                    horizontal_collision: false,
                    fall_flying: flags.contains(EntityFlags::FALL_FLYING),
                },
                shift_key_down: flags.contains(EntityFlags::CROUCHING),
                sprinting: flags.contains(EntityFlags::SPRINTING),
                client: None,
                respawn: false,
            };
            let mut state = before.clone();
            let mut actor = actor_of(&entity, player.game_mode, global, &level.chunks);
            let op = actor.permission_level > 0;
            let protected = |pos: BlockPos| protection.protects(pos, op);
            let aabb = entity.aabb();
            let mut drops: Vec<Drop> = Vec::new();
            let mut hands: Vec<Hand> = Vec::new();
            let handled = {
                let mut level = WorldLevel {
                    level: &mut *level,
                    effects: &mut *blocks.effects,
                    spectators: &spectators,
                    actor: Some((id, aabb)),
                };
                let hooks =
                    crate::plugins::Hooks::of(&global.plugins, self.level.dimension.as_str());
                let mut play = PlayContext {
                    level: &mut level,
                    inventory: &mut player.inventory,
                    xp: Some(&mut player.survival.xp),
                    recipe_book: Some(&mut player.recipe_book),
                    actor,
                    data: &global.game,
                    spawn_protected: &protected,
                    drops: &mut drops,
                    swings: &mut hands,
                    hooks: &hooks,
                    stats: Some(&mut player.stats),
                    advancements: Some(&mut player.advancements),
                };
                let handled = player.connection.handle_packets(
                    &mut state,
                    ctx,
                    chat,
                    &mut play,
                    PACKETS_PER_TICK,
                );
                actor = play.actor;
                for drop in drops {
                    // `ServerPlayer.drop` of a stack thrown from the hand.
                    if drop.thrown && !drop.stack.is_empty() {
                        let item = drop.stack.item;
                        player
                            .stats
                            .award(Stat::of(StatType::Dropped, item), drop.stack.count);
                        player.stats.award(custom::DROP, 1);
                    }
                    throw_item(&mut level, &actor, drop);
                }
                handled
            };
            swings.extend(hands.into_iter().map(|h| (id, h)));
            match handled {
                Handled::Ok => {}
                Handled::Disconnected => self.leaving.push((id, None)),
                Handled::Kick(reason) => self.leaving.push((id, Some(reason))),
            }
            let Some(mut entity) = level.entities.store.get_mut(id) else {
                continue;
            };
            let body = state.body;
            if body.pos != before.body.pos {
                entity.set_pos(body.pos);
            }
            if body.y_rot != before.body.y_rot || body.x_rot != before.body.x_rot {
                entity.set_rot(Rotation {
                    y_rot: body.y_rot,
                    x_rot: body.x_rot,
                    y_head_rot: body.y_rot,
                });
            }
            entity.set_flags(EntityFlags::ON_GROUND, body.on_ground);
            entity.set_flags(EntityFlags::SPRINTING, state.sprinting);
            if state.shift_key_down != before.shift_key_down {
                // `setShiftKeyDown`, and the pose `Player.updatePlayerPose` picks (without the
                // collision test, which needs blocks).
                entity.set_flags(EntityFlags::CROUCHING, state.shift_key_down);
                let pose = if state.shift_key_down {
                    Pose::Crouching
                } else {
                    Pose::Standing
                };
                entity.set_pose(pose);
                entity.set_dims(vanillite_game::player::dimensions(pose));
            }
            if let Some(client) = state.client {
                if client.model_customisation != player.client.model_customisation
                    || client.main_hand != player.client.main_hand
                {
                    entity.mark_data_dirty();
                }
                let parts = client.model_customisation;
                global
                    .players
                    .update_info(Uuid(entity.as_ref().uuid()), |e| {
                        e.model_customisation = parts;
                    });
                player.client = client;
            }
            if state.respawn {
                self.respawns.push(id);
            }
            if !player.connection.chat.requests.is_empty() {
                self.chatting.push(id);
            }
            let view = ChunkView {
                center: chunk_of(body.pos),
                view_distance: effective_view_distance(&player.client, self.view_distance),
            };
            if view != player.chunk_view {
                player.chunk_view = view;
                player.connection.chunks.update_view(
                    view.center,
                    view.view_distance,
                    player.connection.sink.as_ref(),
                );
            }
        }
        level.entities.players = players;
        let entities = &mut level.entities;
        // Hand swings go to the players that see the swinging one (`sendToTrackingPlayers`).
        for (id, hand) in swings {
            let Some(network_id) = entities.store.get(id).map(|e| e.network_id().0) else {
                continue;
            };
            let packet = SharedPacket::of(&SwingAnimation {
                entity_id: network_id,
                hand,
                kind: 1,
                duration: 6,
            });
            for viewer in blocks.tracker.seen_by(id) {
                if let Some(p) = entities.player(*viewer) {
                    p.connection.sink.send_shared(&packet);
                }
            }
        }
        for id in std::mem::take(&mut self.chatting) {
            crate::chat::handle_requests(level, global, id, world, &mut self.leaving);
        }
        for (id, reason) in std::mem::take(&mut self.leaving) {
            self.leave(level, global, id, reason);
        }
    }

    /// Spawn protection of this world for this tick (`isUnderSpawnProtection`).
    fn spawn_protection(&self, global: &Global) -> SpawnProtection {
        let spawn = self.level.spawn.0;
        SpawnProtection {
            active: self.level.dimension.as_str() == "minecraft:overworld"
                && global.spawn_protection > 0
                && !global.access.ops().is_empty(),
            x: vanillite_base::math::floor(spawn.x),
            z: vanillite_base::math::floor(spawn.z),
            radius: global.spawn_protection,
        }
    }

    /// The player part of the entity tick (`ServerPlayer.tick`).
    pub(crate) fn tick_entities(&mut self, entities: &mut Entities) {
        for (_, player) in entities.players.iter_mut() {
            player.connection.movement.tick_client_load_timeout();
        }
    }

    /// `ChunkMap.tick`: one chunk batch per player.
    pub(crate) fn send_chunks(entities: &mut Entities, source: &dyn ChunkSource) {
        for (_, player) in entities.players.iter_mut() {
            let connection = &mut player.connection;
            connection
                .chunks
                .send_next_chunks(source, connection.sink.as_ref());
        }
    }

    /// Whether `chunk` is in view of `player` but not sent yet.
    pub(crate) fn chunk_pending(entities: &Entities, player: EntityId, chunk: ChunkPos) -> bool {
        entities
            .player(player)
            .is_some_and(|p| p.connection.chunks.is_pending(chunk))
    }

    /// Sends the tracker's updates: each encoded once, then copied into one batch per viewer
    /// and tick, which is queued as a single entry ([`FanOut`]).
    pub(crate) fn send_entity_updates(
        entities: &Entities,
        updates: &TrackerOutput,
        compute: &Compute,
        fan_out: &mut FanOut,
    ) {
        fan_out.run(entities, updates, compute);
    }

    /// The connection tick (`ServerGamePacketListenerImpl.tick`) after the world tick.
    pub(crate) fn tick_connections(
        &mut self,
        level: &mut Level,
        tick: u64,
        global: &Global,
        world: crate::world::WorldId,
        blocks: &mut WorldBlocks<'_>,
    ) {
        crate::commands::answer_suggestions(&mut level.entities, global, world);
        for uuid in std::mem::take(&mut self.left) {
            crate::scoreboard::left(global, uuid);
            if global.players.remove(Uuid(uuid)).is_some() {
                global
                    .players
                    .broadcast(&join::player_info_remove(uuid), None);
            }
        }
        self.tick_players(level, global, blocks);
        crate::progress::tick(level, global, world, self.level.dimension.as_str());
        let entities = &mut level.entities;
        let Entities { store, players, .. } = entities;
        for (id, player) in players.iter_mut() {
            if let Some(entity) = store.get(id) {
                let body = Body {
                    pos: entity.pos(),
                    velocity: entity.velocity(),
                    y_rot: entity.rot().y_rot,
                    x_rot: entity.rot().x_rot,
                    on_ground: entity.flags().contains(EntityFlags::ON_GROUND),
                    horizontal_collision: false,
                    fall_flying: entity.flags().contains(EntityFlags::FALL_FLYING),
                };
                player.connection.movement.tick(&body);
            }
            player.connection.chat.chat_spam.tick();
            player.connection.chat.command_spam.tick();
        }
        // `PlayerList.tick`: latencies for the tab list. The first world sends them for all.
        if tick.is_multiple_of(LATENCY_UPDATE_TICKS)
            && self.level.dimension.as_str() == "minecraft:overworld"
        {
            let entries = global.players.info_entries();
            if !entries.is_empty() {
                let update = vanillite_protocol::packets::play::clientbound::PlayerInfoUpdate {
                    actions:
                        vanillite_protocol::packets::play::clientbound::info_action::UPDATE_LATENCY,
                    ..join::player_info_add(&entries)
                };
                global.players.broadcast(&update, None);
            }
        }
    }

    /// `ServerGamePacketListenerImpl.tick` and `ServerPlayer.doTick` for blocks and items: the
    /// block change acknowledgement, digging progress, item pickup, the inventory menu's
    /// `broadcastChanges` and equipment changes for other players.
    fn tick_players(&mut self, level: &mut Level, global: &Global, blocks: &mut WorldBlocks<'_>) {
        let protection = self.spawn_protection(global);
        // As in `handle_packets`: the players leave the level while it may change.
        let mut players = std::mem::take(&mut level.entities.players);
        let spectators: FxHashSet<EntityId> = players
            .iter()
            .filter(|(_, p)| p.game_mode == GameMode::Spectator)
            .map(|(id, _)| id)
            .collect();
        let mut taken: Vec<(EntityId, EntityId, i32)> = Vec::new();
        let mut equipment: Vec<(EntityId, SetEquipment)> = Vec::new();
        let mut taken_orbs: Vec<(EntityId, EntityId)> = Vec::new();
        for (id, player) in players.iter_mut() {
            let sink = Arc::clone(&player.connection.sink);
            if let Some(sequence) = player.connection.play.take_ack() {
                sink.send(&BlockChangedAck { sequence });
            }
            let Some(entity) = level.entities.store.get(id) else {
                continue;
            };
            let actor = actor_of(&entity, player.game_mode, global, &level.chunks);
            let aabb = entity.aabb();
            let network_id = entity.network_id().0;
            let uuid = entity.uuid();
            let op = actor.permission_level > 0;
            let protected = |pos: BlockPos| protection.protects(pos, op);
            {
                let mut level = WorldLevel {
                    level: &mut *level,
                    effects: &mut *blocks.effects,
                    spectators: &spectators,
                    actor: Some((id, aabb)),
                };
                let mut drops = Vec::new();
                let mut hands = Vec::new();
                let hooks =
                    crate::plugins::Hooks::of(&global.plugins, self.level.dimension.as_str());
                let mut play = PlayContext {
                    level: &mut level,
                    inventory: &mut player.inventory,
                    xp: Some(&mut player.survival.xp),
                    recipe_book: Some(&mut player.recipe_book),
                    actor,
                    data: &global.game,
                    spawn_protected: &protected,
                    drops: &mut drops,
                    swings: &mut hands,
                    hooks: &hooks,
                    stats: Some(&mut player.stats),
                    advancements: Some(&mut player.advancements),
                };
                player.connection.play.tick(&mut play, sink.as_ref());
            }
            // `Player.aiStep` → `touch`: item entities in the box grown by (1, 0.5, 1).
            if player.game_mode != GameMode::Spectator {
                let Entities { store, items, .. } = &mut level.entities;
                let reach = aabb.inflate(1.0, 0.5, 1.0);
                let mut near = Vec::new();
                store.for_each_non_player_in(&reach, |e| {
                    if crate::blocks::is_item(store, e) {
                        near.push(e);
                    }
                });
                let infinite = player.game_mode == GameMode::Creative;
                for item_id in near {
                    let Some(row) = store.get(item_id).map(|e| e.kind_row()) else {
                        continue;
                    };
                    let Some(item) = items.get_mut(row) else {
                        continue;
                    };
                    let target_ok = item.target.is_none_or(|t| t == uuid);
                    if item.pickup_delay != 0 || !target_ok || item.stack.is_empty() {
                        continue;
                    }
                    let count = item.stack.count;
                    let kind = item.stack.item;
                    if player.inventory.add(None, &mut item.stack, infinite) {
                        taken.push((item_id, id, count));
                        player
                            .stats
                            .award(Stat::of(StatType::PickedUp, kind), count);
                    }
                }
            }
            // The rest of `touch`: one random experience orb of those the box touches.
            if player.game_mode != GameMode::Spectator && player.survival.health > 0.0 {
                touch_orb(
                    level,
                    id,
                    &mut player.survival,
                    &mut player.inventory,
                    &aabb.inflate(1.0, 0.5, 1.0),
                    global,
                    &mut taken_orbs,
                );
            }
            let sink = &player.connection.sink;
            if player.inventory.container.is_some() {
                // `containerMenu.broadcastChanges`, then `stillValid`.
                use vanillite_game::menu::container as menus;
                menus::with_view(
                    &mut player.inventory,
                    level,
                    actor.pos,
                    Some(&mut player.survival.xp),
                    |view| {
                        view.flush_result_updates(sink.as_ref());
                        vanillite_game::menu::broadcast_changes(view, sink.as_ref());
                    },
                );
                let in_range = |pos| actor.within_block_range(pos, 4.0);
                let in_entity_range = |b: &vanillite_base::Aabb| actor.within_entity_range(b, 4.0);
                if !menus::still_valid(&player.inventory, level, &in_range, &in_entity_range) {
                    let who = menus::Opener {
                        spectator: player.game_mode == GameMode::Spectator,
                        infinite: player.game_mode == GameMode::Creative,
                        disconnected: false,
                    };
                    let mut drops = Vec::new();
                    menus::close(
                        &mut player.inventory,
                        level,
                        who,
                        &mut drops,
                        Some(sink.as_ref()),
                    );
                    let mut world = WorldLevel {
                        level: &mut *level,
                        effects: &mut *blocks.effects,
                        spectators: &spectators,
                        actor: Some((id, aabb)),
                    };
                    for drop in drops {
                        throw_item(&mut world, &actor, drop);
                    }
                }
            } else {
                player.inventory.broadcast_changes(sink.as_ref());
            }
            // `detectEquipmentUpdates`.
            let mut changed = Vec::new();
            for (last, slot) in player.last_equipment.iter_mut().zip(EQUIPMENT_SLOTS) {
                let now = player.inventory.equipment(slot);
                if !vanillite_game::item::matches(last, now) {
                    *last = now.clone();
                    changed.push((slot.ordinal(), now.clone()));
                }
            }
            if !changed.is_empty() {
                equipment.push((
                    id,
                    SetEquipment {
                        entity_id: network_id,
                        slots: changed,
                    },
                ));
            }
        }
        level.entities.players = players;
        let entities = &mut level.entities;
        for (id, packet) in equipment {
            let packet = SharedPacket::of(&packet);
            for viewer in blocks.tracker.seen_by(id) {
                if let Some(p) = entities.player(*viewer) {
                    p.connection.sink.send_shared(&packet);
                }
            }
        }
        for (orb, taker) in taken_orbs {
            // `Player.take(orb, 1)`; the orb goes when its last merged orb was taken.
            let Some(orb_net) = entities.store.get(orb).map(|e| e.network_id().0) else {
                continue;
            };
            let Some(player_net) = entities.store.get(taker).map(|e| e.network_id().0) else {
                continue;
            };
            let packet = SharedPacket::of(&TakeItemEntity {
                item_id: orb_net,
                player_id: player_net,
                amount: 1,
            });
            for viewer in blocks.tracker.seen_by(orb) {
                if let Some(p) = entities.player(*viewer) {
                    p.connection.sink.send_shared(&packet);
                }
            }
            if entities.experience_orb(orb).is_some_and(|d| d.count <= 0) {
                entities.despawn(orb);
            }
        }
        for (item_id, player_id, count) in taken {
            let (Some(item_net), Some(player_net)) = (
                entities.store.get(item_id).map(|e| e.network_id().0),
                entities.store.get(player_id).map(|e| e.network_id().0),
            ) else {
                continue;
            };
            let packet = SharedPacket::of(&TakeItemEntity {
                item_id: item_net,
                player_id: player_net,
                amount: count,
            });
            for viewer in blocks.tracker.seen_by(item_id) {
                if let Some(p) = entities.player(*viewer) {
                    p.connection.sink.send_shared(&packet);
                }
            }
            let empty = entities.item(item_id).is_none_or(|i| i.stack.is_empty());
            if empty {
                entities.despawn(item_id);
            } else if let Some(mut e) = entities.store.get_mut(item_id) {
                e.mark_data_dirty();
            }
        }
    }

    /// The player leaves: kicked with `reason`, or the connection is gone.
    fn leave(
        &mut self,
        level: &mut Level,
        global: &Global,
        id: EntityId,
        reason: Option<Box<Component>>,
    ) {
        let entities = &mut level.entities;
        let Some(entity) = entities.store.get(id) else {
            return;
        };
        let uuid = entity.uuid();
        let Some(player) = entities.player_mut(id) else {
            return;
        };
        player.stats.award(custom::LEAVE_GAME, 1);
        let name = player.profile.name.clone();
        let changes = player.stats.take_score_changes();
        crate::progress::apply_scores(global, vec![(name.clone(), uuid, changes)]);
        match reason {
            Some(reason) => {
                log::info!("{name} lost connection: {}", reason.plain());
                player.connection.sink.kick(*reason);
            }
            None => log::info!("{name} lost connection: Disconnected"),
        }
        self.save_player(entities, id);
        drop_menus_on_leave(level, id);
        let entities = &mut level.entities;
        entities.despawn(id);
        // Vanilla's order: the message, the entity removal (sent by the tracker this tick),
        // then the tab list entry.
        global.players.broadcast_system(
            join::left_message(crate::scoreboard::display_name(global, &name, uuid)),
            false,
            Some(Uuid(uuid)),
        );
        self.left.push(uuid);
        let world = crate::server::DIMENSIONS
            .iter()
            .position(|d| *d == self.level.dimension.as_str())
            .and_then(|i| u32::try_from(i).ok())
            .unwrap_or(0);
        global.plugins.player_leave(world, uuid, &name);
    }

    /// Queues the write of a player's data file; the fields this server does not handle stay
    /// as they are in the file.
    fn save_player(&self, entities: &Entities, id: EntityId) {
        let (Some(entity), Some(player)) = (entities.store.get(id), entities.player(id)) else {
            return;
        };
        let pos = entity.pos();
        let pos = [pos.x, pos.y, pos.z];
        let rotation = [entity.rot().y_rot, entity.rot().x_rot];
        let on_ground = entity.flags().contains(EntityFlags::ON_GROUND);
        let dimension = self.level.dimension.as_str().to_owned();
        let game_mode = game_mode_id(player.game_mode);
        let mut survival = vanillite_nbt::Compound::new();
        player.inventory.save(&mut survival);
        player.survival.save(&mut survival);
        survival.insert(
            "post_effects",
            vanillite_nbt::Tag::List(vanillite_nbt::List(
                player
                    .post_effects
                    .iter()
                    .map(|e| vanillite_nbt::Tag::String(e.clone()))
                    .collect(),
            )),
        );
        // `PlayerList.save`: the statistics and advancements files with the data.
        let uuid = entity.uuid();
        self.store
            .save_json(JsonFile::Stats, uuid, player.stats.to_json());
        self.store
            .save_json(JsonFile::Advancements, uuid, player.advancements.to_json());
        survival.insert("recipeBook", player.recipe_book.save());
        // `Survival::save` removes the death location when there is none.
        let has_death = survival.get("LastDeathLocation").is_some();
        self.store.update(entity.uuid(), move |data| {
            data.pos = pos;
            data.rotation = rotation;
            data.on_ground = on_ground;
            data.dimension = dimension;
            data.game_mode = game_mode;
            if !has_death {
                data.other.remove("LastDeathLocation");
            }
            for (key, tag) in survival.iter() {
                data.other.insert(key, tag.clone());
            }
        });
    }

    /// Saves all players of this world.
    pub(crate) fn save_all(&self, entities: &Entities) {
        for (id, _) in entities.players.iter() {
            self.save_player(entities, id);
        }
    }
}

/// `ServerPlayer.readAdditionalSaveData`'s `post_effects`: the valid identifiers.
fn load_post_effects(data: &vanillite_nbt::Compound) -> Vec<String> {
    let Some(vanillite_nbt::Tag::List(list)) = data.get("post_effects") else {
        return Vec::new();
    };
    list.0
        .iter()
        .filter_map(|t| t.as_str())
        .filter_map(|id| vanillite_protocol::types::Identifier::new(id).ok())
        .map(|id| id.to_string())
        .collect()
}

/// What the block and item handling of a tick needs from the world.
pub(crate) struct WorldBlocks<'a> {
    pub(crate) effects: &'a mut LevelEffects,
    pub(crate) tracker: &'a dyn EntityTracker,
}

/// `isUnderSpawnProtection` of one world for one tick.
#[derive(Clone, Copy, Debug)]
struct SpawnProtection {
    /// Overworld, a radius above 0 and at least one operator.
    active: bool,
    x: i32,
    z: i32,
    radius: i32,
}

impl SpawnProtection {
    /// Whether a non-operator may not change the block at `pos`.
    fn protects(self, pos: BlockPos, op: bool) -> bool {
        if !self.active || op {
            return false;
        }
        let dx = pos.x.saturating_sub(self.x).saturating_abs();
        let dz = pos.z.saturating_sub(self.z).saturating_abs();
        dx.max(dz) <= self.radius
    }
}

/// The acting player for the block and item handlers.
fn actor_of(
    entity: &EntityRef<'_>,
    game_mode: GameMode,
    global: &Global,
    chunks: &Chunks,
) -> Actor {
    let pose = entity.pose();
    // `Player.getDefaultDimensions(pose).eyeHeight()`.
    // Eye heights are floats in Vanilla (`EntityDimensions.eyeHeight`).
    let eye_height = f64::from(match pose {
        Pose::Crouching => 1.27f32,
        Pose::Swimming | Pose::FallFlying | Pose::SpinAttack => 0.4,
        Pose::Sleeping | Pose::Dying => 0.2,
        _ => 1.62,
    });
    let pos = entity.pos();
    let eye = BlockPos::new(
        vanillite_base::math::floor(pos.x),
        vanillite_base::math::floor(pos.y + eye_height),
        vanillite_base::math::floor(pos.z),
    );
    let fluid = chunks
        .block(eye)
        .map_or(vanillite_data::fluid::FluidState::EMPTY, |s| {
            vanillite_data::block::BlockState(s).fluid_state()
        });
    let eyes_in_water = !fluid.is_empty() && matches!(fluid.info().fluid, 1 | 2);
    let flags = entity.flags();
    Actor {
        entity: Some(entity.id()),
        network_id: entity.network_id().0,
        uuid: entity.uuid(),
        pos,
        eye_height,
        y_rot: entity.rot().y_rot,
        x_rot: entity.rot().x_rot,
        on_ground: flags.contains(EntityFlags::ON_GROUND),
        sneaking: flags.contains(EntityFlags::CROUCHING),
        game_mode,
        permission_level: crate::commands::permission_level(global, entity.uuid()),
        eyes_in_water,
    }
}

fn body_of(entities: &Entities, id: EntityId) -> Body {
    match entities.store.get(id) {
        Some(e) => Body {
            pos: e.pos(),
            velocity: e.velocity(),
            y_rot: e.rot().y_rot,
            x_rot: e.rot().x_rot,
            on_ground: e.flags().contains(EntityFlags::ON_GROUND),
            horizontal_collision: false,
            fall_flying: false,
        },
        None => Body {
            pos: Vec3::ZERO,
            velocity: Vec3::ZERO,
            y_rot: 0.0,
            x_rot: 0.0,
            on_ground: false,
            horizontal_collision: false,
            fall_flying: false,
        },
    }
}

/// `GameType.byId` without a fallback.
pub(crate) fn game_mode_by_id(id: i32) -> Option<GameMode> {
    match id {
        0 => Some(GameMode::Survival),
        1 => Some(GameMode::Creative),
        2 => Some(GameMode::Adventure),
        3 => Some(GameMode::Spectator),
        _ => None,
    }
}

/// `GameType.getId`.
pub(crate) fn game_mode_id(mode: GameMode) -> i32 {
    match mode {
        GameMode::Survival => 0,
        GameMode::Creative => 1,
        GameMode::Adventure => 2,
        GameMode::Spectator => 3,
    }
}

/// The tab list entry of a joining player.
pub(crate) fn info_entry(arrival: &Arrival) -> InfoEntry {
    InfoEntry {
        uuid: arrival.uuid,
        profile: arrival.profile.clone(),
        game_mode: arrival.game_mode,
        latency: arrival.sink.latency_ms(),
        model_customisation: arrival.client.model_customisation,
        chat_session: None,
    }
}

/// `Player.remove` for a player that leaves: open menus close, what they held drops.
fn drop_menus_on_leave(level: &mut Level, id: EntityId) {
    let Some(pos) = level.entities.store.get(id).map(|e| e.pos()) else {
        return;
    };
    let Some(player) = level.entities.player_mut(id) else {
        return;
    };
    let spectator = player.game_mode == GameMode::Spectator;
    let mut inv = std::mem::take(&mut player.inventory);
    let mut drops = Vec::new();
    vanillite_game::menu::container::close_on_leave(&mut inv, level, spectator, &mut drops);
    if let Some(player) = level.entities.player_mut(id) {
        player.inventory = inv;
    }
    let eye = vanillite_base::Vec3::new(pos.x, pos.y + 1.62 - 0.3, pos.z);
    for drop in drops {
        vanillite_game::block::drops::spawn_item(
            level,
            eye,
            drop.stack,
            vanillite_base::Vec3::new(0.0, 0.1, 0.0),
            40,
            None,
        );
    }
}

/// `ExperienceOrb.playerTouch` of the orb `Util.getRandom` picks among those `reach`
/// touches: with `takeXpDelay` over, one orb of its stack repairs with Mending and gives the
/// rest as experience points.
fn touch_orb(
    level: &mut Level,
    id: EntityId,
    survival: &mut vanillite_game::survival::player::Survival,
    inventory: &mut vanillite_game::inventory::PlayerInventory,
    reach: &vanillite_base::Aabb,
    global: &Global,
    taken: &mut Vec<(EntityId, EntityId)>,
) {
    use vanillite_base::random::RandomSource;
    use vanillite_game::experience_orb as orbs;
    let mut near = Vec::new();
    orbs::touching(&level.entities, reach, &mut near);
    if near.is_empty() {
        return;
    }
    let n = i32::try_from(near.len()).unwrap_or(i32::MAX);
    let pick = orbs::with_entity_random(|r| r.next_int_bounded(n));
    let Some(&orb) = usize::try_from(pick).ok().and_then(|i| near.get(i)) else {
        return;
    };
    if survival.take_xp_delay != 0 {
        return;
    }
    survival.take_xp_delay = orbs::TAKE_XP_DELAY;
    let points = orbs::with_entity_random(|r| {
        orbs::take_one(
            &mut level.entities,
            orb,
            inventory,
            &global.game.enchanting,
            &mut |n| r.next_int_bounded(n),
        )
    });
    let Some(points) = points else {
        return;
    };
    taken.push((orb, id));
    if points > 0 {
        let volume = survival.give_experience_points(points);
        let Some(mut entity) = level.entities.store.get_mut(id) else {
            return;
        };
        // The score is synched data.
        entity.mark_data_dirty();
        let pos = entity.as_ref().pos();
        if let Some(volume) = volume {
            level.play_sound(
                None,
                pos.x,
                pos.y,
                pos.z,
                vanillite_game::block::sound_id("entity.player.levelup"),
                vanillite_game::level::SOUND_PLAYERS,
                volume,
                1.0,
            );
        }
    }
}
