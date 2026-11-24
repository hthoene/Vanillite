//! Players in a world: joining, their packets, chunk sending, entity updates and leaving. The
//! network layer hands over connections; the handlers themselves live in
//! `vanillite_game::player`.

use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use vanillite_base::{ChunkPos, Vec3};
use vanillite_config::{GameMode, WorldRules};
use vanillite_game::entities::Entities;
use vanillite_game::player::chat::ChatInbox;
use vanillite_game::player::encode::encode_update;
use vanillite_game::player::join::{self, InfoEntry, LevelInfo};
use vanillite_game::player::{
    Body, ChatContext, ChunkSource, ChunkView, ClientInformation, Connection, GameProfile, Handled,
    MoveContext, PacketSink, PlayerData, PlayerState, effective_view_distance,
};
use vanillite_game::survival::player::Survival;
use vanillite_game::survival::rules::Rule;
use vanillite_net::{Latency, Outbound};
use vanillite_protocol::text::Component;
use vanillite_protocol::{Inbound, SharedPacket};
use vanillite_storage::player_data::{self, PlayerDataStore};
use vanillite_world::entity::{EntityFlags, EntityId, Pose, Rotation};
use vanillite_world::tracking::TrackerOutput;

use crate::global::Global;
use crate::player_list::Uuid;

/// Packets handled per player and tick; the rest waits for the next tick (SEC-16).
const PACKETS_PER_TICK: usize = 128;
/// Ticks between latency updates of the tab list (Vanilla's `PlayerList.tick`).
const LATENCY_UPDATE_TICKS: u64 = 600;

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
    pub(crate) inbound: Receiver<Inbound>,
    pub(crate) memory_connection: bool,
    pub(crate) saved: Option<player_data::PlayerData>,
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
        let data = PlayerData {
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
                &global.chat,
                Arc::clone(&arrival.inbox),
            ),
            survival,
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
        let source = crate::commands::CommandSource {
            global: global_arc,
            kind: crate::commands::SourceKind::Player {
                uuid: arrival.uuid,
                name: name.clone(),
                sink: Arc::clone(&sink),
            },
            level: crate::commands::permission_level(global, arrival.uuid),
            world,
            pos,
            rot,
        };
        crate::commands::send_permissions(global, sink.as_ref(), network_id, &source);
        global
            .players
            .broadcast_system(join::joined_message(&name, arrival.uuid), false, None);
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
            &crate::survival::full_time_sync(&global.level),
            weather,
        );
        connection
            .chunks
            .update_view(center, view_distance, sink.as_ref());
    }

    /// Handles the queued packets of all players (Vanilla handles them between ticks), applies
    /// their effects to the entities and moves chunk views.
    pub(crate) fn handle_packets(
        &mut self,
        entities: &mut Entities,
        rules: &WorldRules,
        runs_normally: bool,
        global: &Global,
        world: crate::world::WorldId,
    ) {
        let ctx = MoveContext {
            runs_normally,
            rules: &rules.movement,
        };
        let Entities { store, players, .. } = entities;
        for (id, player) in players.iter_mut() {
            let Some(mut entity) = store.get_mut(id) else {
                continue;
            };
            let chat = ChatContext {
                uuid: entity.as_ref().uuid(),
                name: &player.profile.name,
                settings: &global.chat,
            };
            let flags = entity.as_ref().flags();
            let rot = entity.as_ref().rot();
            let before = PlayerState {
                body: Body {
                    pos: entity.as_ref().pos(),
                    velocity: entity.as_ref().velocity(),
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
            match player
                .connection
                .handle_packets(&mut state, ctx, chat, PACKETS_PER_TICK)
            {
                Handled::Ok => {}
                Handled::Disconnected => self.leaving.push((id, None)),
                Handled::Kick(reason) => self.leaving.push((id, Some(reason))),
            }
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
        for id in std::mem::take(&mut self.chatting) {
            crate::chat::handle_requests(entities, global, id, world, &mut self.leaving);
        }
        for (id, reason) in std::mem::take(&mut self.leaving) {
            self.leave(entities, global, id, reason);
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

    /// Sends the tracker's updates: each encoded once, then queued for all its viewers.
    pub(crate) fn send_entity_updates(entities: &Entities, updates: &TrackerOutput) {
        updates.fan_out(
            |message| encode_update(message, entities),
            |viewer, packets| {
                if let Some(player) = entities.player(viewer) {
                    for packet in packets {
                        player.connection.sink.send_shared(packet);
                    }
                }
            },
        );
    }

    /// The connection tick (`ServerGamePacketListenerImpl.tick`) after the world tick.
    pub(crate) fn tick_connections(
        &mut self,
        entities: &mut Entities,
        tick: u64,
        global: &Global,
        world: crate::world::WorldId,
    ) {
        crate::commands::answer_suggestions(entities, global, world);
        for uuid in std::mem::take(&mut self.left) {
            if global.players.remove(Uuid(uuid)).is_some() {
                global
                    .players
                    .broadcast(&join::player_info_remove(uuid), None);
            }
        }
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

    /// The player leaves: kicked with `reason`, or the connection is gone.
    fn leave(
        &mut self,
        entities: &mut Entities,
        global: &Global,
        id: EntityId,
        reason: Option<Box<Component>>,
    ) {
        let Some(entity) = entities.store.get(id) else {
            return;
        };
        let uuid = entity.uuid();
        let Some(player) = entities.player(id) else {
            return;
        };
        let name = player.profile.name.clone();
        match reason {
            Some(reason) => {
                log::info!("{name} lost connection: {}", reason.plain());
                player.connection.sink.kick(*reason);
            }
            None => log::info!("{name} lost connection: Disconnected"),
        }
        self.save_player(entities, id);
        entities.despawn(id);
        // Vanilla's order: the message, the entity removal (sent by the tracker this tick),
        // then the tab list entry.
        global
            .players
            .broadcast_system(join::left_message(&name, uuid), false, Some(Uuid(uuid)));
        self.left.push(uuid);
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
        player.survival.save(&mut survival);
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
