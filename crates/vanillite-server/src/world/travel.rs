//! Players changing places through portals and respawns (`Entity.teleport` with a
//! `TeleportTransition`, `PlayerList.respawn`).
//!
//! A change of world is an asynchronous move between two owners: the old world removes the
//! player entity and hands its data to the new world ([`TickContext::in_world`]). The new
//! world loads the chunks its destination needs (the exit portal search, the End platform),
//! then finds or builds the exit and places the player with the packets of a dimension
//! change. A player that cannot arrive (a spectator without an exit portal) goes back.
//! End gateways move players within their world.

use std::sync::Arc;

use crate::player_list::Uuid;
use vanillite_base::{Aabb, BlockPos, ChunkPos, Vec3};
use vanillite_config::GameMode;
use vanillite_game::block::portal as portal_blocks;
use vanillite_game::level::forced::SavedTickets;
use vanillite_game::player::{Body, ChunkView, PlayerData, effective_view_distance, join};
use vanillite_game::portal::{self, EntryPortal, PortalKind};
use vanillite_game::survival::rules::Rule;
use vanillite_protocol::packets::play::clientbound::{
    Respawn, SetExperience, SetHeldSlot, relative,
};
use vanillite_world::chunks::{FULL_LEVEL, TicketId};
use vanillite_world::entity::{EntityFlags, EntityId, NetworkId, Rotation};
use vanillite_worldgen::behaviour::HorizontalAxis;

use super::{ServerWorld, TickContext, WorldId};
use crate::global::Global;

/// The overworld: where players respawn and where the End's exit portal leads.
const OVERWORLD: WorldId = WorldId(0);
/// The Nether.
const NETHER: WorldId = WorldId(1);
/// The End.
const END: WorldId = WorldId(2);
/// The End's dimension key (`Level.END`).
const END_DIMENSION: &str = "minecraft:the_end";

/// `TicketType.PORTAL`: ticks a portal ticket keeps the chunks around an arrival loaded.
const PORTAL_TICKET_TICKS: u64 = 300;

/// A portal ticket (load and simulation) with the tick it expires (`TicketType.PORTAL`).
#[derive(Debug)]
pub(crate) struct PortalTicket {
    until: u64,
    pos: ChunkPos,
    level: u8,
    load: TicketId,
    simulation: TicketId,
}

/// `ClientboundRespawnPacket.KEEP_ALL_DATA`: what the client keeps on a dimension change.
const KEEP_ALL_DATA: u8 = 3;

/// `LevelEvent.SOUND_PORTAL_TRAVEL`.
const PORTAL_TRAVEL_SOUND: i32 = 1032;

/// A player on its way into a world.
pub(crate) struct Traveller {
    uuid: u128,
    network_id: NetworkId,
    data: PlayerData,
    /// The world it left.
    from: WorldId,
    /// Where it was when it left.
    pos: Vec3,
    velocity: Vec3,
    y_rot: f32,
    x_rot: f32,
    /// Its box size when it left (`getDimensions(getPose())`).
    width: f32,
    height: f32,
    route: Route,
    /// Its entity tags (`entityTags`), kept like `PlayerList.respawn` and `teleport` do.
    tags: Vec<String>,
}

/// Where a traveller goes in its new world.
enum Route {
    /// Through a nether portal (`NetherPortalBlock.getPortalDestination`): the closest exit
    /// portal to `approx` within the search radius, or a new one.
    NetherPortal {
        approx: BlockPos,
        to_nether: bool,
        entry: EntryPortal,
    },
    /// Through an End portal into the End: onto the obsidian platform.
    EndPlatform,
    /// Through the End's exit portal (`findRespawnPositionAndUseSpawnBlock`): the world spawn.
    Spawn,
    /// `PlayerList.respawn` after death, or after the end credits (`keep_all`).
    Respawn { keep_all: bool },
    /// Back to where it came from: its destination had no place for it.
    Back,
}

/// A traveller waiting for the chunks around its destination.
pub(crate) struct Waiting {
    traveller: Traveller,
    /// The chunk radius that must be full, with the ticket that loads it.
    area: Option<(ChunkPos, i32, TicketId)>,
}

/// A player in an End gateway waiting for the chunks the gateway's exit needs (Vanilla loads
/// them on the spot).
pub(crate) struct GatewayWait {
    player: EntityId,
    /// The gateway it entered.
    entry: BlockPos,
    /// The chunks requested so far, each with the ticket that loads it.
    tickets: Vec<(ChunkPos, TicketId)>,
}

/// Where a player lands and what the teleport keeps (`TeleportTransition`).
struct Landing {
    pos: Vec3,
    y_rot: f32,
    x_rot: f32,
    /// [`relative`] bits of the teleport.
    relatives: i32,
    /// `PLAY_PORTAL_SOUND`.
    sound: bool,
    /// `placePortalTicket` at this position.
    ticket: Option<BlockPos>,
}

/// `Relative.union(Relative.DELTA, Relative.ROTATION)`: velocity and rotation are kept.
const KEEP_MOTION_AND_ROTATION: i32 =
    relative::DELTA_X | relative::DELTA_Y | relative::DELTA_Z | relative::Y_ROT | relative::X_ROT;

impl ServerWorld {
    /// `handlePortal`'s teleport: the player `id` went through `kind` at `entry`.
    pub(super) fn enter_portal(
        &mut self,
        ctx: &mut TickContext<'_>,
        id: EntityId,
        kind: PortalKind,
        entry: BlockPos,
    ) {
        match kind {
            PortalKind::Nether => {
                let target = if self.id == NETHER { OVERWORLD } else { NETHER };
                // `isAllowedToEnterPortal`.
                if target == NETHER
                    && !ctx
                        .global
                        .level
                        .rules()
                        .bool(Rule::ALLOW_ENTERING_NETHER_USING_PORTALS)
                {
                    return;
                }
                let Some(entity) = self.level.entities.store.get(id) else {
                    return;
                };
                let (pos, (width, height)) = (entity.pos(), size(&entity.aabb()));
                let types = &ctx.global.level.dimension_types;
                let (Some(from), Some(to)) =
                    (types.get(self.id.index()), types.get(target.index()))
                else {
                    return;
                };
                // `DimensionType.getTeleportationScale`.
                let scale = from.coordinate_scale / to.coordinate_scale;
                let route = Route::NetherPortal {
                    approx: portal::scaled_exit(pos, scale),
                    to_nether: target == NETHER,
                    entry: portal::entry_portal(&self.level, entry, pos, width, height),
                };
                self.send_away(ctx, id, target, route);
            }
            PortalKind::End => {
                // `EndPortalBlock.getPortalDestination`: out of the End to the respawn
                // dimension, else into the End.
                let (target, route) = if self.id == END {
                    (OVERWORLD, Route::Spawn)
                } else {
                    (END, Route::EndPlatform)
                };
                self.send_away(ctx, id, target, route);
            }
            PortalKind::Gateway => self.enter_gateway(id, entry),
        }
    }

    /// `PlayerList.respawn` for a player that asked for it: dead, or back from the end
    /// credits. Players respawn in the overworld.
    pub(super) fn respawn(&mut self, ctx: &mut TickContext<'_>, id: EntityId) {
        let Some(player) = self.level.entities.player(id) else {
            return;
        };
        let keep_all = player.survival.portal.won_game;
        if !keep_all && !player.survival.is_dead_or_dying() {
            return;
        }
        // `findRespawnPositionAndUseSpawnBlock`: the world of the player's respawn position.
        let target = player
            .survival
            .respawn
            .as_ref()
            .and_then(|r| {
                crate::server::DIMENSIONS
                    .iter()
                    .position(|d| *d == r.dimension)
            })
            .and_then(|i| u16::try_from(i).ok())
            .map_or(OVERWORLD, WorldId);
        self.send_away(ctx, id, target, Route::Respawn { keep_all });
    }

    /// Removes the player `id` from this world and hands it to `target` (this world too).
    fn send_away(
        &mut self,
        ctx: &mut TickContext<'_>,
        id: EntityId,
        target: WorldId,
        route: Route,
    ) {
        let Some(traveller) = self.take_player(id, route) else {
            return;
        };
        self.send(ctx, target, traveller);
    }

    fn send(&mut self, ctx: &mut TickContext<'_>, target: WorldId, traveller: Traveller) {
        if target == self.id {
            self.arrive(ctx.global, traveller);
            return;
        }
        ctx.global.players.set_world(Uuid(traveller.uuid), target);
        let Some(global) = ctx.global.arc() else {
            return;
        };
        ctx.in_world(target, move |world: &mut ServerWorld| {
            world.arrive(&global, traveller);
        });
    }

    /// `removePlayerImmediately`: the player entity leaves; its data travels.
    fn take_player(&mut self, id: EntityId, route: Route) -> Option<Traveller> {
        let entity = self.level.entities.store.get(id)?;
        let (uuid, pos, velocity, rot) =
            (entity.uuid(), entity.pos(), entity.velocity(), entity.rot());
        let (width, height) = size(&entity.aabb());
        let tags = self.level.entities.tags_of(id).to_vec();
        let (removed, data) = self.level.entities.despawn_player(id)?;
        Some(Traveller {
            uuid,
            network_id: removed.network_id,
            data,
            from: self.id,
            pos,
            velocity,
            y_rot: rot.y_rot,
            x_rot: rot.x_rot,
            width,
            height,
            route,
            tags,
        })
    }

    /// A traveller reached this world: it waits here until the chunks its destination needs
    /// are loaded.
    pub(crate) fn arrive(&mut self, global: &Global, traveller: Traveller) {
        let area = match &traveller.route {
            Route::NetherPortal {
                approx, to_nether, ..
            } => Some((approx.chunk(), portal::search_chunk_radius(*to_nether))),
            Route::EndPlatform => Some((
                portal::END_SPAWN_POINT.chunk(),
                portal::PORTAL_TICKET_RADIUS,
            )),
            Route::Spawn | Route::Respawn { .. } | Route::Back => None,
        };
        let area = area.map(|(center, radius)| {
            let level = FULL_LEVEL.saturating_sub(u8::try_from(radius).unwrap_or(u8::MAX));
            (center, radius, self.level.chunks.add_ticket(center, level))
        });
        self.arrivals.push(Waiting { traveller, area });
        self.place_arrivals(global);
    }

    /// Places the travellers whose destination is loaded, moves players through gateways
    /// whose exit is loaded and lets portal tickets expire.
    pub(super) fn tick_travel(&mut self, global: &Global) {
        self.place_arrivals(global);
        let mut waits = std::mem::take(&mut self.gateway_waits);
        waits.retain_mut(|wait| {
            let done = self.level.entities.player(wait.player).is_none() || self.use_gateway(wait);
            if done {
                for (_, ticket) in wait.tickets.drain(..) {
                    self.level.chunks.remove_ticket(ticket);
                }
            }
            !done
        });
        self.gateway_waits = waits;
        let now = self.ticks;
        let chunks = &mut self.level.chunks;
        let before = self.portal_tickets.len();
        self.portal_tickets.retain(|t| {
            let keep = now < t.until;
            if !keep {
                chunks.remove_ticket(t.load);
                chunks.remove_simulation_ticket(t.simulation);
            }
            keep
        });
        self.tickets_dirty |= self.portal_tickets.len() != before;
    }

    fn place_arrivals(&mut self, global: &Global) {
        if self.arrivals.is_empty() {
            return;
        }
        let waiting = std::mem::take(&mut self.arrivals);
        let mut back = Vec::new();
        for wait in waiting {
            if let Some((center, radius, ticket)) = wait.area {
                if !self.area_loaded(center, radius) {
                    self.arrivals.push(wait);
                    continue;
                }
                self.level.chunks.remove_ticket(ticket);
            }
            if let Some(traveller) = self.place(global, wait.traveller) {
                back.push(traveller);
            }
        }
        // Travellers without a place return to their world (this one for a failed respawn,
        // which cannot happen).
        for mut traveller in back {
            traveller.route = Route::Back;
            let target = traveller.from;
            if target == self.id {
                self.place(global, traveller);
                continue;
            }
            global.players.set_world(Uuid(traveller.uuid), target);
            if let Some(global_arc) = global.arc() {
                global.run_in_world(target, move |world: &mut ServerWorld| {
                    world.arrive(&global_arc, traveller);
                });
            }
        }
    }

    /// Whether every chunk within `radius` of `center` is full.
    fn area_loaded(&self, center: ChunkPos, radius: i32) -> bool {
        (center.x - radius..=center.x + radius).all(|x| {
            (center.z - radius..=center.z + radius)
                .all(|z| self.level.chunks.chunk(ChunkPos::new(x, z)).is_some())
        })
    }

    /// Finds or builds the destination and places the traveller; returns it when there is no
    /// place for it here.
    fn place(&mut self, global: &Global, mut traveller: Traveller) -> Option<Traveller> {
        let keep_all = match traveller.route {
            Route::Respawn { keep_all } => Some(keep_all),
            _ => None,
        };
        let landing = match self.landing(global, &traveller) {
            Some(landing) => landing,
            None => return Some(traveller),
        };
        if traveller.route_is_back() {
            self.place_back(traveller);
            return None;
        }
        let rules = global.level.rules();
        let data = &mut traveller.data;
        // `triggerDimensionChangeTriggers`: `changed_dimension` from the world it left.
        if traveller.from != self.id
            && let (Some(from), Some(to)) = (
                crate::server::DIMENSIONS.get(traveller.from.index()),
                crate::server::DIMENSIONS.get(self.id.index()),
            )
        {
            data.advancements.trigger(
                vanillite_game::advancements::trigger::Event::ChangedDimension(
                    (*from).to_owned(),
                    (*to).to_owned(),
                ),
            );
        }
        if let Some(keep_all) = keep_all {
            // `restoreFrom`: everything stays after the credits, else what death keeps.
            if keep_all {
                data.survival.portal.won_game = false;
            } else {
                let keep_inventory =
                    rules.bool(Rule::KEEP_INVENTORY) || data.game_mode == GameMode::Spectator;
                data.survival = data.survival.respawned(keep_inventory);
            }
        }
        drop(rules);
        // A respawn position that is gone (`NO_RESPAWN_BLOCK_AVAILABLE`): cleared, the world
        // spawn stands in.
        let respawn_missing = keep_all.is_some()
            && data
                .survival
                .respawn
                .as_ref()
                .is_some_and(|r| r.dimension == self.dimension)
            && data
                .survival
                .respawn
                .as_ref()
                .and_then(|r| crate::survival::forced_respawn(&self.level.chunks, r))
                .is_none();
        if respawn_missing {
            data.survival.respawn = None;
        }
        data.survival.reset_sent();
        data.survival.teleported();
        data.connection.chunks.reset();
        let sink = Arc::clone(&data.connection.sink);
        let info = &self.players.level;
        let mut body = Body {
            pos: traveller.pos,
            velocity: traveller.velocity,
            y_rot: traveller.y_rot,
            x_rot: traveller.x_rot,
            on_ground: false,
            horizontal_collision: false,
            fall_flying: false,
        };
        let respawn = Respawn {
            spawn: join::spawn_info(info, data),
            data_to_keep: keep_all.map_or(KEEP_ALL_DATA, u8::from),
        };
        let difficulty = crate::survival::change_difficulty(&global.level);
        if keep_all.is_some() {
            // `PlayerList.respawn`'s order.
            sink.send(&respawn);
            data.connection.movement.teleport_relative(
                &mut body,
                landing.pos,
                landing.y_rot,
                landing.x_rot,
                landing.relatives,
                sink.as_ref(),
            );
            if respawn_missing {
                sink.send(&vanillite_protocol::packets::play::clientbound::GameEvent {
                    event: vanillite_protocol::packets::play::clientbound::GameEvent::NO_RESPAWN_BLOCK_AVAILABLE,
                    param: 0.0,
                });
            }
            join::send_spawn_position(sink.as_ref(), info);
            sink.send(&difficulty);
            sink.send(&SetExperience {
                progress: data.survival.xp.progress,
                level: data.survival.xp.level,
                total: data.survival.xp.total,
            });
            join::send_level_info(
                sink.as_ref(),
                info,
                &crate::border::level_packets(&self.level.border, global),
                &crate::survival::full_time_sync(&global.level),
                &self.survival.join_weather(),
            );
            data.connection.movement.restart_client_load_timer();
        } else {
            // `ServerPlayer.teleport` to another dimension.
            sink.send(&respawn);
            sink.send(&difficulty);
            data.connection.movement.teleport_relative(
                &mut body,
                landing.pos,
                landing.y_rot,
                landing.x_rot,
                landing.relatives,
                sink.as_ref(),
            );
        }
        data.connection.movement.reset_position(body.pos);
        join::send_abilities(sink.as_ref(), data.game_mode);
        let view = ChunkView {
            center: ChunkPos::new(
                vanillite_base::math::floor(body.pos.x) >> 4,
                vanillite_base::math::floor(body.pos.z) >> 4,
            ),
            view_distance: effective_view_distance(&data.client, self.players.view_distance()),
        };
        data.chunk_view = view;
        let Traveller {
            uuid,
            network_id,
            data,
            tags,
            ..
        } = traveller;
        let id = self.level.entities.spawn_player_as(
            network_id,
            uuid,
            body.pos,
            Rotation {
                y_rot: body.y_rot,
                x_rot: body.x_rot,
                y_head_rot: body.y_rot,
            },
            data,
        );
        self.level.entities.set_tags(id, tags);
        if let Some(mut entity) = self.level.entities.store.get_mut(id) {
            entity.set_velocity(body.velocity);
            entity.set_flags(EntityFlags::ON_GROUND, false);
            entity.mark_data_dirty();
        }
        // `sendPlayerPermissionLevel`: the operator level and the command tree.
        if let Some(source) =
            crate::commands::player_source(&self.level.entities, global, id, self.id)
        {
            crate::commands::send_permissions(global, sink.as_ref(), network_id.0, &source);
        }
        let player = self.level.entities.player_mut(id)?;
        if keep_all.is_none() {
            // `sendLevelInfo`, `sendAllPlayerInfo`, then the transition's own effects.
            join::send_level_info(
                sink.as_ref(),
                &self.players.level,
                &crate::border::level_packets(&self.level.border, global),
                &crate::survival::full_time_sync(&global.level),
                &self.survival.join_weather(),
            );
        }
        player.inventory.send_all(sink.as_ref());
        sink.send(&SetHeldSlot {
            slot: i32::try_from(player.inventory.selected).unwrap_or(0),
        });
        if landing.sound {
            sink.send_shared(&Arc::new(crate::world_updates::level_event_packet(
                PORTAL_TRAVEL_SOUND,
                BlockPos::new(0, 0, 0),
                0,
                false,
            )));
        }
        player
            .connection
            .chunks
            .update_view(view.center, view.view_distance, sink.as_ref());
        if let Some(pos) = landing.ticket {
            self.place_portal_ticket(pos);
        }
        None
    }

    /// Puts a traveller back where it left, unchanged (its client never saw it go).
    fn place_back(&mut self, traveller: Traveller) {
        let Traveller {
            uuid,
            network_id,
            data,
            pos,
            velocity,
            y_rot,
            x_rot,
            ..
        } = traveller;
        let id = self.level.entities.spawn_player_as(
            network_id,
            uuid,
            pos,
            Rotation {
                y_rot,
                x_rot,
                y_head_rot: y_rot,
            },
            data,
        );
        if let Some(mut entity) = self.level.entities.store.get_mut(id) {
            entity.set_velocity(velocity);
        }
    }

    /// The transition's destination in this world (`getPortalDestination`); `None` if there
    /// is none.
    fn landing(&mut self, global: &Global, traveller: &Traveller) -> Option<Landing> {
        match &traveller.route {
            Route::NetherPortal {
                approx,
                to_nether,
                entry,
            } => {
                let level = &mut self.level;
                // `getExitPortal`.
                let found =
                    portal::find_closest_portal(level, *approx, portal::search_radius(*to_nether));
                let (exit, ticket) = match found {
                    Some(pos) => {
                        let state = level.block(pos);
                        let axis = HorizontalAxis::of(state)?;
                        let rect = portal::largest_rectangle_around(pos, axis, |p| {
                            level.block(p) == state
                        });
                        (rect, Some(pos))
                    }
                    None => {
                        if traveller.data.game_mode == GameMode::Spectator {
                            return None;
                        }
                        let logical_height = global
                            .level
                            .dimension_types
                            .get(self.id.index())
                            .map_or(256, |t| t.logical_height);
                        let created =
                            portal::create_portal(level, *approx, entry.axis, logical_height);
                        let Some(created) = created else {
                            log::error!(
                                "Unable to create a portal, likely target out of worldborder"
                            );
                            return None;
                        };
                        (created, None)
                    }
                };
                // `createDimensionTransition`.
                let exit_axis =
                    HorizontalAxis::of(level.block(exit.min_corner)).unwrap_or(HorizontalAxis::X);
                let (pos, rotation) = portal::exit_position(
                    &exit,
                    exit_axis,
                    entry,
                    traveller.width,
                    traveller.height,
                );
                let pos = free_position(level, pos, traveller.width, traveller.height);
                Some(Landing {
                    pos,
                    y_rot: rotation,
                    x_rot: 0.0,
                    relatives: KEEP_MOTION_AND_ROTATION,
                    sound: true,
                    // A found exit keeps its own chunks loaded, a new one those at the player.
                    ticket: Some(ticket.unwrap_or_else(|| containing(pos))),
                })
            }
            Route::EndPlatform => {
                let spawn = portal::END_SPAWN_POINT;
                portal::create_end_platform(&mut self.level, spawn.offset(0, -1, 0));
                // Players arrive one block lower, on the platform.
                let pos = Vec3::new(
                    f64::from(spawn.x) + 0.5,
                    f64::from(spawn.y) - 1.0,
                    f64::from(spawn.z) + 0.5,
                );
                Some(Landing {
                    pos,
                    // `Direction.WEST.toYRot()`.
                    y_rot: 90.0,
                    x_rot: 0.0,
                    relatives: relative::DELTA_X
                        | relative::DELTA_Y
                        | relative::DELTA_Z
                        | relative::X_ROT,
                    sound: true,
                    ticket: Some(containing(pos)),
                })
            }
            Route::Respawn { .. }
                if let Some((pos, y_rot, x_rot)) = traveller
                    .data
                    .survival
                    .respawn
                    .as_ref()
                    .filter(|r| r.dimension == self.dimension)
                    .and_then(|r| crate::survival::forced_respawn(&self.level.chunks, r)) =>
            {
                Some(Landing {
                    pos,
                    y_rot,
                    x_rot,
                    relatives: 0,
                    sound: false,
                    ticket: None,
                })
            }
            Route::Spawn | Route::Respawn { .. } => {
                let (pos, yaw) = self.players.level.spawn;
                Some(Landing {
                    pos,
                    y_rot: yaw,
                    x_rot: 0.0,
                    relatives: 0,
                    sound: false,
                    ticket: None,
                })
            }
            Route::Back => Some(Landing {
                pos: traveller.pos,
                y_rot: traveller.y_rot,
                x_rot: traveller.x_rot,
                relatives: 0,
                sound: false,
                ticket: None,
            }),
        }
    }

    /// `placePortalTicket`: the chunks within 3 of `pos` stay loaded and ticking for 300
    /// ticks.
    fn place_portal_ticket(&mut self, pos: BlockPos) {
        let level = FULL_LEVEL - u8::try_from(portal::PORTAL_TICKET_RADIUS).unwrap_or(0);
        self.add_portal_ticket(pos.chunk(), level, PORTAL_TICKET_TICKS);
    }

    /// Adds a portal ticket that expires after `ticks`.
    fn add_portal_ticket(&mut self, pos: ChunkPos, level: u8, ticks: u64) {
        let chunks = &mut self.level.chunks;
        let load = chunks.add_ticket(pos, level);
        let simulation = chunks.add_simulation_ticket(pos, level);
        self.portal_tickets.push(PortalTicket {
            until: self.ticks + ticks,
            pos,
            level,
            load,
            simulation,
        });
        self.tickets_dirty = true;
    }

    /// Restores the saved chunk tickets besides the forced ones (`activateAllDeactivatedTickets`).
    pub(crate) fn restore_tickets(&mut self, saved: SavedTickets) {
        for t in &saved.portal {
            self.add_portal_ticket(t.pos, t.level, u64::try_from(t.ticks_left).unwrap_or(0));
        }
        self.kept_tickets = saved.kept;
        // Loading is not a change.
        self.tickets_dirty = false;
    }

    /// The saved chunk tickets besides the forced ones, with the portal tickets' remaining
    /// ticks.
    pub(crate) fn saved_tickets(&self) -> SavedTickets {
        let portal = self
            .portal_tickets
            .iter()
            .map(|t| vanillite_game::level::forced::PortalTicket {
                pos: t.pos,
                level: t.level,
                ticks_left: i64::try_from(t.until.saturating_sub(self.ticks)).unwrap_or(i64::MAX),
            })
            .collect();
        SavedTickets {
            portal,
            kept: self.kept_tickets.clone(),
        }
    }

    /// `EndGatewayBlock.getPortalDestination`: the player moves to the gateway's exit once the
    /// chunks it needs are loaded; a gateway in the End without an exit (from the dragon
    /// fight) gets one first.
    fn enter_gateway(&mut self, id: EntityId, entry: BlockPos) {
        if self.gateway_waits.iter().any(|w| w.player == id) {
            return;
        }
        let mut wait = GatewayWait {
            player: id,
            entry,
            tickets: Vec::new(),
        };
        if !self.use_gateway(&mut wait) {
            self.gateway_waits.push(wait);
        }
    }

    /// `getPortalPosition` and the teleport for `wait`, or tickets for the chunks that are
    /// missing; whether the wait is over.
    fn use_gateway(&mut self, wait: &mut GatewayWait) -> bool {
        let Some(gateway) = portal_blocks::gateway(&self.level, wait.entry) else {
            return true;
        };
        let exit = gateway.exit;
        let needed = match (exit, gateway.exact) {
            (None, _) if self.dimension == END_DIMENSION => {
                portal_blocks::gateway_exit_chunks(&self.level, wait.entry)
            }
            (None, _) => return true,
            (Some(_), true) => Vec::new(),
            // `findExitPosition` looks 5 blocks around the exit.
            (Some(exit), false) => {
                let center = exit.chunk();
                (center.x - 1..=center.x + 1)
                    .flat_map(|x| (center.z - 1..=center.z + 1).map(move |z| ChunkPos::new(x, z)))
                    .filter(|&pos| self.level.chunks.chunk(pos).is_none())
                    .collect()
            }
        };
        if !needed.is_empty() {
            for pos in needed {
                if !wait.tickets.iter().any(|&(p, _)| p == pos) {
                    let ticket = self.level.chunks.add_ticket(pos, FULL_LEVEL);
                    wait.tickets.push((pos, ticket));
                }
            }
            return false;
        }
        if exit.is_none() {
            portal_blocks::create_gateway_exit(&mut self.level, wait.entry);
        }
        let destination = portal_blocks::gateway(&self.level, wait.entry)
            .and_then(|gateway| portal_blocks::gateway_destination(&self.level, gateway));
        if let Some(pos) = destination {
            self.teleport_through_gateway(wait.player, pos);
        }
        true
    }

    /// The gateway's teleport within this world: to the bottom centre of `exit`, keeping
    /// velocity and rotation, then `PLACE_PORTAL_TICKET`.
    fn teleport_through_gateway(&mut self, id: EntityId, exit: BlockPos) {
        let pos = Vec3::new(
            f64::from(exit.x) + 0.5,
            f64::from(exit.y),
            f64::from(exit.z) + 0.5,
        );
        let entities = &mut self.level.entities;
        let Some(entity) = entities.store.get(id) else {
            return;
        };
        let rot = entity.rot();
        let flags = entity.flags();
        let mut body = Body {
            pos: entity.pos(),
            velocity: entity.velocity(),
            y_rot: rot.y_rot,
            x_rot: rot.x_rot,
            on_ground: flags.contains(EntityFlags::ON_GROUND),
            horizontal_collision: false,
            fall_flying: flags.contains(EntityFlags::FALL_FLYING),
        };
        let Some(player) = entities.player_mut(id) else {
            return;
        };
        let sink = Arc::clone(&player.connection.sink);
        player.connection.movement.teleport_relative(
            &mut body,
            pos,
            0.0,
            0.0,
            KEEP_MOTION_AND_ROTATION,
            sink.as_ref(),
        );
        player.connection.movement.reset_position(body.pos);
        player.survival.teleported();
        if let Some(mut entity) = entities.store.get_mut(id) {
            entity.set_pos(body.pos);
            entity.set_velocity(body.velocity);
        }
        self.place_portal_ticket(containing(body.pos));
    }
}

impl Traveller {
    fn route_is_back(&self) -> bool {
        matches!(self.route, Route::Back)
    }
}

/// Width and height of a box.
#[allow(clippy::cast_possible_truncation)]
fn size(aabb: &Aabb) -> (f32, f32) {
    (
        (aabb.max_x - aabb.min_x) as f32,
        (aabb.max_y - aabb.min_y) as f32,
    )
}

/// `PortalShape.findCollisionFreePosition`: the lowest spot at most one block above `pos`
/// where a box of `width` × `height` does not collide with blocks, `pos` if there is none.
/// Vanilla searches the closest free centre in that column of the box's footprint; arrivals
/// in a portal's air are free at once.
fn free_position(level: &vanillite_game::level::Level, pos: Vec3, width: f32, height: f32) -> Vec3 {
    if width > 4.0 || height > 4.0 {
        return pos;
    }
    let (half, height) = (f64::from(width) / 2.0, f64::from(height));
    let block = |p: BlockPos| level.block(p);
    let mut scratch = Vec::new();
    let free = |y: f64, scratch: &mut Vec<Aabb>| {
        let b = Aabb::new(
            pos.x - half,
            y,
            pos.z - half,
            pos.x + half,
            y + height,
            pos.z + half,
        );
        !vanillite_game::item_entity::collides(&b, &block, scratch)
    };
    if free(pos.y, &mut scratch) {
        return pos;
    }
    // Block tops come in sixteenths (slabs, snow, carpets).
    let base = pos.y.floor();
    (1..=32)
        .map(|step| base + f64::from(step) / 16.0)
        .filter(|&y| y > pos.y && y <= pos.y + 1.0)
        .find(|&y| free(y, &mut scratch))
        .map_or(pos, |y| Vec3::new(pos.x, y, pos.z))
}

/// `BlockPos.containing(pos)`.
fn containing(pos: Vec3) -> BlockPos {
    BlockPos::containing(pos.x, pos.y, pos.z)
}
