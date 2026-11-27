//! Players changing places through portals and respawns (`Entity.teleport` with a
//! `TeleportTransition`, `PlayerList.respawn`).
//!
//! A change of world is an asynchronous move between two owners: the old world removes the
//! player entity and hands its data to the new world ([`TickContext::in_world`]). The new
//! world loads the chunks its destination needs (the exit portal search, the End platform),
//! then finds or builds the exit and places the player with the packets of a dimension
//! change. A player that cannot arrive (a spectator without an exit portal) goes back.
//! End gateways move players and other entities within their world. Ender pearls bring their
//! owners over from other worlds.

use std::sync::Arc;

use crate::player_list::Uuid;
use vanillite_base::{Aabb, BlockPos, ChunkPos, Vec3};
use vanillite_config::GameMode;
use vanillite_game::block::portal as portal_blocks;
use vanillite_game::level::forced::SavedTickets;
use vanillite_game::mob::fam::wildlife::ender_pearl;
use vanillite_game::player::{Body, ChunkView, PlayerData, effective_view_distance, join};
use vanillite_game::portal::{self, EntryPortal, PearlArrival, PortalKind, TravelRequest};
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
    /// The plugins' persistent data.
    persistent: Option<vanillite_nbt::Compound>,
}

/// Where a traveller goes in its new world.
enum Route {
    /// Through a nether portal (`NetherPortalBlock.getPortalDestination`): the closest exit
    /// portal to `approx` within the search radius, or a new one.
    NetherPortal {
        /// The entity's position scaled to this world (`getTeleportationScale`).
        scaled: Vec3,
        /// `scaled` clamped to this world's border, set on arrival.
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
    /// To where its ender pearl landed (`ThrownEnderpearl.onHit` across worlds).
    Pearl(PearlArrival),
    /// `/tp` and `/teleport` into this world (`TeleportCommand.performTeleport`): to an exact
    /// place, without motion.
    Command { pos: Vec3, y_rot: f32, x_rot: f32 },
}

/// An entity besides players on its way into a world through a portal
/// (`teleportCrossDimension` without passengers).
pub(crate) struct EntityTraveller {
    /// Its stored data (`restoreFrom`), without passengers.
    nbt: vanillite_nbt::Compound,
    /// Its box size when it left.
    width: f32,
    height: f32,
    /// `portalCooldown`, which `restoreFrom` takes over.
    cooldown: i32,
    route: Route,
}

/// An entity waiting for the chunks around its destination.
pub(crate) struct EntityWaiting {
    traveller: EntityTraveller,
    area: Option<(ChunkPos, i32, TicketId)>,
}

/// A traveller waiting for the chunks around its destination.
pub(crate) struct Waiting {
    traveller: Traveller,
    /// The chunk radius that must be full, with the ticket that loads it.
    area: Option<(ChunkPos, i32, TicketId)>,
}

/// An entity in an End gateway waiting for the chunks the gateway's exit needs (Vanilla loads
/// them on the spot).
pub(crate) struct GatewayWait {
    entity: EntityId,
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
    /// `missingRespawnBlock`: the bed or anchor is gone (the respawn point is cleared).
    missing: bool,
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
        if kind == PortalKind::Gateway {
            self.enter_gateway(id, entry);
            return;
        }
        if let Some((target, route)) = self.portal_route(ctx.global, id, kind, entry) {
            // Vanilla handles a player's portal in its connection tick (`ServerPlayer.doTick`),
            // after every level ticked: it arrives after the world phase.
            self.send_away(ctx, id, target, route, true);
        }
    }

    /// Where the entity `id` goes through the nether or end portal `kind` at `entry`
    /// (`getPortalDestination` up to the new world): `None` if it may not.
    fn portal_route(
        &self,
        global: &Global,
        id: EntityId,
        kind: PortalKind,
        entry: BlockPos,
    ) -> Option<(WorldId, Route)> {
        match kind {
            PortalKind::Nether => {
                let target = if self.id == NETHER { OVERWORLD } else { NETHER };
                // `isAllowedToEnterPortal`.
                if target == NETHER
                    && !global
                        .level
                        .rules()
                        .bool(Rule::ALLOW_ENTERING_NETHER_USING_PORTALS)
                {
                    return None;
                }
                let entity = self.level.entities.store.get(id)?;
                let (pos, (width, height)) = (entity.pos(), size(&entity.aabb()));
                let types = &global.level.dimension_types;
                let (from, to) = (types.get(self.id.index())?, types.get(target.index())?);
                // `DimensionType.getTeleportationScale`.
                let scale = from.coordinate_scale / to.coordinate_scale;
                let route = Route::NetherPortal {
                    scaled: Vec3::new(pos.x * scale, pos.y, pos.z * scale),
                    approx: BlockPos::new(0, 0, 0),
                    to_nether: target == NETHER,
                    entry: portal::entry_portal(&self.level, entry, pos, width, height),
                };
                Some((target, route))
            }
            // `EndPortalBlock.getPortalDestination`: out of the End to the respawn dimension,
            // else into the End.
            PortalKind::End if self.id == END => Some((OVERWORLD, Route::Spawn)),
            PortalKind::End => Some((END, Route::EndPlatform)),
            PortalKind::Gateway => None,
        }
    }

    /// `handlePortal`'s teleport of an entity besides players through a nether or end portal
    /// (`teleportCrossDimension`): it leaves this world with its stored data, without its
    /// passengers, and arrives in the other one.
    fn send_entity(
        &mut self,
        ctx: &mut TickContext<'_>,
        id: EntityId,
        kind: PortalKind,
        entry: BlockPos,
    ) {
        let Some((target, route)) = self.portal_route(ctx.global, id, kind, entry) else {
            return;
        };
        let Some(entity) = self.level.entities.store.get(id) else {
            return;
        };
        let (width, height) = size(&entity.aabb());
        let Some(mut nbt) = self.level.save_entity(id) else {
            return;
        };
        nbt.remove("Passengers");
        let cooldown = self
            .level
            .entities
            .portals
            .get(&id)
            .map_or(0, |p| p.cooldown);
        self.level.entities.despawn(id);
        let traveller = EntityTraveller {
            nbt,
            width,
            height,
            cooldown,
            route,
        };
        let Some(global) = ctx.global.arc() else {
            return;
        };
        if target == self.id {
            self.arrive_entity(&global, traveller);
            return;
        }
        ctx.in_world(target, move |world: &mut ServerWorld| {
            world.arrive_entity(&global, traveller);
        });
    }

    /// An entity reached this world through a portal: it waits here until the chunks its
    /// destination needs are loaded.
    pub(crate) fn arrive_entity(&mut self, global: &Global, mut traveller: EntityTraveller) {
        if let Route::NetherPortal { scaled, approx, .. } = &mut traveller.route {
            *approx = self
                .level
                .border
                .clamp_to_bounds(scaled.x, scaled.y, scaled.z);
        }
        let area = arrival_area(&traveller.route).map(|(center, radius)| {
            let level = FULL_LEVEL.saturating_sub(u8::try_from(radius).unwrap_or(u8::MAX));
            (center, radius, self.level.chunks.add_ticket(center, level))
        });
        self.entity_arrivals.push(EntityWaiting { traveller, area });
        self.place_arrivals(global);
    }

    /// Finds or builds the destination of an entity and adds it there; it is lost when there
    /// is no place for it (Vanilla's `teleport` returns `null`).
    fn place_entity(&mut self, global: &Global, traveller: EntityTraveller) {
        let EntityTraveller {
            mut nbt,
            width,
            height,
            cooldown,
            route,
        } = traveller;
        let landing = match &route {
            Route::NetherPortal {
                approx,
                to_nether,
                entry,
                ..
            } => self.nether_landing(global, *approx, *to_nether, entry, width, height, false),
            Route::EndPlatform => {
                let spawn = portal::END_SPAWN_POINT;
                portal::create_end_platform(&mut self.level, spawn.offset(0, -1, 0));
                let pos = Vec3::new(
                    f64::from(spawn.x) + 0.5,
                    f64::from(spawn.y),
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
                    missing: false,
                })
            }
            // `adjustSpawnLocation`: on top of the spawn column, turned by the spawn angle.
            Route::Spawn => {
                let (spawn, yaw) = self.players.level.spawn;
                let block = containing(spawn);
                let y = portal::height_at(
                    &self.level,
                    vanillite_chunk::HeightmapKind::MotionBlockingNoLeaves,
                    block.x,
                    block.z,
                );
                Some(Landing {
                    pos: Vec3::new(
                        f64::from(block.x) + 0.5,
                        f64::from(y),
                        f64::from(block.z) + 0.5,
                    ),
                    y_rot: yaw,
                    x_rot: 0.0,
                    relatives: KEEP_MOTION_AND_ROTATION,
                    sound: true,
                    ticket: Some(containing(spawn)),
                    missing: false,
                })
            }
            _ => None,
        };
        let Some(landing) = landing else {
            return;
        };
        use vanillite_nbt::{List, Tag};
        let doubles = |list: Option<&Tag>| match list {
            Some(Tag::List(List(v))) => v
                .iter()
                .map(|t| match t {
                    Tag::Double(d) => *d,
                    _ => 0.0,
                })
                .collect(),
            _ => Vec::new(),
        };
        let floats = |list: Option<&Tag>| match list {
            Some(Tag::List(List(v))) => v
                .iter()
                .map(|t| match t {
                    Tag::Float(f) => *f,
                    _ => 0.0,
                })
                .collect(),
            _ => Vec::new(),
        };
        // `teleportSetPosition` (`PositionMoveRotation.calculateAbsolute`).
        let motion: Vec<f64> = doubles(nbt.get("Motion"));
        let rotation: Vec<f32> = floats(nbt.get("Rotation"));
        let keep = |flag: i32, i: usize| {
            if landing.relatives & flag != 0 {
                motion.get(i).copied().unwrap_or(0.0)
            } else {
                0.0
            }
        };
        let motion = [
            keep(relative::DELTA_X, 0),
            keep(relative::DELTA_Y, 1),
            keep(relative::DELTA_Z, 2),
        ];
        let turn = |flag: i32, i: usize, value: f32| {
            if landing.relatives & flag != 0 {
                rotation.get(i).copied().unwrap_or(0.0) + value
            } else {
                value
            }
        };
        let (y_rot, x_rot) = (
            turn(relative::Y_ROT, 0, landing.y_rot),
            turn(relative::X_ROT, 1, landing.x_rot).clamp(-90.0, 90.0),
        );
        nbt.insert(
            "Pos",
            Tag::List(List(vec![
                Tag::Double(landing.pos.x),
                Tag::Double(landing.pos.y),
                Tag::Double(landing.pos.z),
            ])),
        );
        nbt.insert(
            "Motion",
            Tag::List(List(motion.iter().map(|&m| Tag::Double(m)).collect())),
        );
        nbt.insert(
            "Rotation",
            Tag::List(List(vec![Tag::Float(y_rot), Tag::Float(x_rot)])),
        );
        let Some(id) = self.level.load_travelled_entity(nbt) else {
            return;
        };
        ender_pearl::arrived(&mut self.level.entities, id, |uuid| {
            global.players.contains(Uuid(uuid))
        });
        if cooldown > 0 {
            self.level.entities.portals.entry(id).or_default().cooldown = cooldown;
        }
        if let Some(pos) = landing.ticket {
            self.place_portal_ticket(pos);
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
        // `remove(KILLED)` → `triggerOnDeathMobEffects`: a dead player's Wind Charged bursts.
        let wind = !keep_all
            && player
                .survival
                .effects
                .has(vanillite_game::effect::MobEffect::WIND_CHARGED);
        if wind && let Some(e) = self.level.entities.store.get(id) {
            let (pos, height) = (e.pos(), e.dims().height);
            let (center, radius) = vanillite_game::experience_orb::with_entity_random(|r| {
                vanillite_game::effect::living::wind_burst(pos, height, r)
            });
            self.level.explode_with(
                Some(id),
                None,
                center,
                radius,
                vanillite_game::level::explosion::BlockInteraction::TriggerBlock,
                vanillite_game::level::explosion::ExplosionStyle {
                    damages_entities: false,
                    knockback: vanillite_game::effect::living::WIND_CHARGED_KNOCKBACK,
                    wind_sound: Some(vanillite_game::effect::living::WIND_CHARGED_SOUND),
                    resistance: vanillite_game::level::explosion::BlockResistance::WindCharge,
                    fire: false,
                    bad_respawn_point: false,
                },
            );
        }
        self.send_away(ctx, id, target, Route::Respawn { keep_all }, false);
    }

    /// `Entity.teleportTo(level, ...)` of `/tp` and `/teleport` into another world: a player
    /// travels like through a portal, another entity moves as its saved data (without its
    /// passengers) and is added there. `rotation` `None` keeps the entity's own. The motion is
    /// dropped, like `TeleportCommand.performTeleport` does.
    pub(crate) fn teleport_to_world(
        &mut self,
        global: &Global,
        uuid: u128,
        target: WorldId,
        pos: Vec3,
        rotation: Option<(f32, f32)>,
    ) {
        let entities = &mut self.level.entities;
        let Some(id) = crate::commands::find_uuid(entities, uuid) else {
            return;
        };
        let Some(entity) = entities.store.get(id) else {
            return;
        };
        let rot = entity.rot();
        let (y_rot, x_rot) = rotation.unwrap_or((rot.y_rot, rot.x_rot));
        let (y_rot, x_rot) = (
            vanillite_base::math::wrap_degrees_f(y_rot),
            vanillite_base::math::wrap_degrees_f(x_rot),
        );
        if entity.kind() == vanillite_data::entity::ids::PLAYER {
            let Some(mut traveller) = self.take_player(id, Route::Command { pos, y_rot, x_rot })
            else {
                return;
            };
            traveller.velocity = Vec3::new(0.0, 0.0, 0.0);
            if target == self.id {
                self.arrive(global, traveller);
                return;
            }
            global.players.set_world(Uuid(traveller.uuid), target);
            if let Some(global_arc) = global.arc() {
                global.run_in_world(target, move |world: &mut ServerWorld| {
                    world.arrive(&global_arc, traveller);
                });
            }
            return;
        }
        let Some(nbt) = Self::take_entity(&mut self.level, id, pos, y_rot, x_rot) else {
            return;
        };
        global.run_in_world(target, move |world: &mut ServerWorld| {
            world.level.load_entity(nbt);
        });
    }

    /// Takes the entity `id` (not a player) out of `level` for a teleport to another world:
    /// its data at the destination, for [`Level::load_entity`] there.
    pub(crate) fn take_entity(
        level: &mut vanillite_game::level::Level,
        id: EntityId,
        pos: Vec3,
        y_rot: f32,
        x_rot: f32,
    ) -> Option<vanillite_nbt::Compound> {
        let mut nbt = level.save_entity(id)?;
        level.entities.despawn(id);
        use vanillite_nbt::{List, Tag};
        nbt.insert(
            "Pos",
            Tag::List(List(vec![
                Tag::Double(pos.x),
                Tag::Double(pos.y),
                Tag::Double(pos.z),
            ])),
        );
        nbt.insert("Motion", Tag::List(List(vec![Tag::Double(0.0); 3])));
        nbt.insert(
            "Rotation",
            Tag::List(List(vec![Tag::Float(y_rot), Tag::Float(x_rot)])),
        );
        nbt.insert("OnGround", Tag::Byte(1));
        nbt.remove("Passengers");
        Some(nbt)
    }

    /// Removes the player `id` from this world and hands it to `target` (this world too): at
    /// once, or after the world phase (`after_worlds`).
    fn send_away(
        &mut self,
        ctx: &mut TickContext<'_>,
        id: EntityId,
        target: WorldId,
        route: Route,
        after_worlds: bool,
    ) {
        let Some(traveller) = self.take_player(id, route) else {
            return;
        };
        if target == self.id {
            self.arrive(ctx.global, traveller);
            return;
        }
        ctx.global.players.set_world(Uuid(traveller.uuid), target);
        let Some(global) = ctx.global.arc() else {
            return;
        };
        let arrive = move |world: &mut ServerWorld| world.arrive(&global, traveller);
        if after_worlds {
            ctx.after_world_phase(target, arrive);
        } else {
            ctx.in_world(target, arrive);
        }
    }

    /// `removePlayerImmediately`: the player entity leaves; its data travels.
    fn take_player(&mut self, id: EntityId, route: Route) -> Option<Traveller> {
        // Out of bed before leaving the world.
        vanillite_game::survival::sleep::stop_sleep_in_bed(
            &mut self.level,
            id,
            true,
            true,
            &vanillite_game::hooks::NoHooks,
        );
        let entity = self.level.entities.store.get(id)?;
        let (uuid, pos, velocity, rot) =
            (entity.uuid(), entity.pos(), entity.velocity(), entity.rot());
        let (width, height) = size(&entity.aabb());
        let tags = self.level.entities.tags_of(id).to_vec();
        let persistent = self.level.entities.persistent_data(id).cloned();
        let (removed, mut data) = self.level.entities.despawn_player(id)?;
        // The camera entity stays behind (`setCamera(this)` of the transition).
        data.connection.camera = None;
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
            persistent,
        })
    }

    /// A traveller reached this world: it waits here until the chunks its destination needs
    /// are loaded.
    pub(crate) fn arrive(&mut self, global: &Global, mut traveller: Traveller) {
        if let Route::NetherPortal { scaled, approx, .. } = &mut traveller.route {
            // `newWorldBorder.clampToBounds`.
            *approx = self
                .level
                .border
                .clamp_to_bounds(scaled.x, scaled.y, scaled.z);
        }
        let area = arrival_area(&traveller.route).map(|(center, radius)| {
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
            let done =
                self.level.entities.store.get(wait.entity).is_none() || self.use_gateway(wait);
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
        self.pearl_tickets
            .retain(|_, &mut (until, ticket, simulation)| {
                let keep = now < until;
                if !keep {
                    chunks.remove_ticket(ticket);
                    chunks.remove_simulation_ticket(simulation);
                }
                keep
            });
    }

    /// What the entities' tick asked of this world: portal and ender pearl tickets, gateway
    /// exits to load, and pearl owners in other worlds.
    pub(super) fn entity_travel(&mut self, ctx: &mut TickContext<'_>) {
        for request in self.mobs.take_travel() {
            match request {
                TravelRequest::PortalTicket(pos) => self.place_portal_ticket(pos),
                TravelRequest::PearlTicket(chunk) => self.place_pearl_ticket(chunk),
                TravelRequest::Gateway { entity, entry } => self.enter_gateway(entity, entry),
                TravelRequest::Portal {
                    entity,
                    kind,
                    entry,
                } => self.send_entity(ctx, entity, kind, entry),
                TravelRequest::PearlOwner { owner, arrival } => {
                    let Some(world) = ctx.global.players.world_of(Uuid(owner)) else {
                        continue;
                    };
                    let Some(global) = ctx.global.arc() else {
                        continue;
                    };
                    let target = self.id;
                    if world == target {
                        // The owner came here meanwhile; it travels like from another world.
                        self.follow_pearl(&global, owner, target, arrival, None);
                        continue;
                    }
                    // With the other worlds at hand (sequential), the owner's world hands the
                    // player back at once and it lands in this tick, like Vanilla's; else its
                    // world sends it over.
                    let handed = Arc::new(HandOver::waiting());
                    let back = Arc::clone(&handed);
                    let global_owner = Arc::clone(&global);
                    ctx.in_world(world, move |w: &mut ServerWorld| {
                        w.follow_pearl(&global_owner, owner, target, arrival, Some(&back));
                    });
                    if let Some(traveller) = handed.close() {
                        self.arrive(&global, traveller);
                    }
                }
            }
        }
    }

    /// `loadAndSpawnEnderPearls` of a joining player: each pearl into its world, with an
    /// ender pearl ticket.
    pub(crate) fn load_pearls(&mut self, global: &Global, pearls: Vec<vanillite_nbt::Compound>) {
        for tag in pearls {
            let dimension = match tag.get(ender_pearl::DIMENSION_TAG) {
                Some(vanillite_nbt::Tag::String(d)) => d.clone(),
                _ => continue,
            };
            if dimension == self.dimension {
                self.load_pearl(&tag);
                continue;
            }
            let Some(index) = crate::server::DIMENSIONS
                .iter()
                .position(|d| *d == dimension.as_str())
                .and_then(|i| u16::try_from(i).ok())
            else {
                log::warn!(
                    "Trying to load ender pearl without level ({dimension}) being loaded, skipping"
                );
                continue;
            };
            global.run_in_world(WorldId(index), move |world: &mut ServerWorld| {
                world.load_pearl(&tag);
            });
        }
    }

    /// The ender pearls here of a player that died in another world vanish
    /// (`ender_pearls_vanish_on_death`).
    pub(crate) fn discard_pearls_of(&mut self, uuid: u128) {
        let dead = self.level.entities.player_by_uuid(uuid).is_none_or(|id| {
            self.level
                .entities
                .player(id)
                .is_some_and(|p| p.survival.is_dead_or_dying() && !p.survival.portal.won_game)
        });
        if dead {
            ender_pearl::take_owned(&mut self.level.entities, uuid, "");
        }
    }

    /// The ender pearls here of a player that left from another world.
    pub(crate) fn add_pearls_of(&mut self, uuid: u128) {
        self.players.add_pearls(&mut self.level, uuid);
    }

    fn load_pearl(&mut self, tag: &vanillite_nbt::Compound) {
        let network_ids = Arc::clone(self.level.network_ids_arc());
        match ender_pearl::spawn_saved(&mut self.level.entities, &network_ids, tag) {
            Some(chunk) => self.place_pearl_ticket(chunk),
            None => log::warn!(
                "Failed to spawn player ender pearl in level ({}), skipping",
                self.dimension
            ),
        }
    }

    /// `ServerPlayer.placeEnderPearlTicket`: the chunks within 2 of `chunk` stay loaded and
    /// ticking for 40 ticks (`TicketType.ENDER_PEARL`); placing it again extends it.
    fn place_pearl_ticket(&mut self, chunk: ChunkPos) {
        let until = self.ticks + ender_pearl::TICKET_TICKS;
        if let Some(entry) = self.pearl_tickets.get_mut(&chunk) {
            entry.0 = until;
            return;
        }
        let level = FULL_LEVEL - u8::try_from(ender_pearl::TICKET_RADIUS).unwrap_or(0);
        let chunks = &mut self.level.chunks;
        let ticket = chunks.add_ticket(chunk, level);
        let simulation = chunks.add_simulation_ticket(chunk, level);
        self.pearl_tickets
            .insert(chunk, (until, ticket, simulation));
    }

    fn place_arrivals(&mut self, global: &Global) {
        for wait in std::mem::take(&mut self.entity_arrivals) {
            if let Some((center, radius, ticket)) = wait.area {
                if !self.area_loaded(center, radius) {
                    self.entity_arrivals.push(wait);
                    continue;
                }
                self.level.chunks.remove_ticket(ticket);
            }
            self.place_entity(global, wait.traveller);
        }
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
        let pearl = match traveller.route {
            Route::Pearl(arrival) => Some((arrival, (traveller.pos, traveller.y_rot))),
            _ => None,
        };
        let from_world = traveller.from;
        // Vanilla teleports inside the player's tick (`handlePortal`): the rest of that tick
        // runs here.
        let finish_tick = matches!(
            traveller.route,
            Route::NetherPortal { .. } | Route::EndPlatform | Route::Spawn
        );
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
        // `teleport(TeleportTransition)`: `enteredNetherPosition` and
        // `triggerDimensionChangeTriggers` (`changed_dimension`, `nether_travel`) from the world
        // it left; a respawn (`PlayerList.respawn`) fires none.
        if keep_all.is_none()
            && traveller.from != self.id
            && let (Some(from), Some(to)) = (
                crate::server::DIMENSIONS.get(traveller.from.index()),
                crate::server::DIMENSIONS.get(self.id.index()),
            )
        {
            use vanillite_game::advancements::trigger::{Event, TriggerKind};
            let (from, to) = (*from, *to);
            let tracking = &mut data.survival.tracking;
            if from == "minecraft:overworld" && to == "minecraft:the_nether" {
                tracking.entered_nether = Some(traveller.pos);
            }
            data.advancements
                .trigger(Event::ChangedDimension(from.to_owned(), to.to_owned()));
            if from == "minecraft:the_nether"
                && to == "minecraft:overworld"
                && let Some(entered) = data.survival.tracking.entered_nether
            {
                let kind = TriggerKind::NETHER_TRAVEL;
                data.advancements
                    .trigger_with(kind, || Event::Distance(kind, entered));
            }
            if to != "minecraft:the_nether" {
                data.survival.tracking.entered_nether = None;
            }
        }
        if let Some(keep_all) = keep_all {
            // `restoreFrom`: everything stays after the credits, else what death keeps.
            if keep_all {
                data.survival.portal.won_game = false;
            } else {
                let keep_inventory = data
                    .survival
                    .keep_inventory_override
                    .unwrap_or_else(|| rules.bool(Rule::KEEP_INVENTORY))
                    || data.game_mode == GameMode::Spectator;
                data.survival = data.survival.respawned(keep_inventory);
            }
        }
        drop(rules);
        // A respawn position that is gone (`NO_RESPAWN_BLOCK_AVAILABLE`): cleared, the world
        // spawn stands in.
        let respawn_missing = keep_all.is_some() && landing.missing;
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
            // `ServerPlayer.teleport` to another dimension; `triggerDimensionChangeTriggers`
            // forgets the Nether entry outside the Nether.
            if traveller.from != self.id {
                let portal = &mut data.survival.portal;
                if traveller.from == OVERWORLD && self.id == NETHER {
                    portal.entered_nether_pos = Some(traveller.pos);
                } else if self.id != NETHER {
                    portal.entered_nether_pos = None;
                }
            }
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
        join::send_abilities(sink.as_ref(), data.game_mode, data.survival.flying);
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
            persistent,
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
        self.level.entities.set_persistent_data(id, persistent);
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
        // `handleClientCommand(PERFORM_RESPAWN)`: a hardcore death respawns as a spectator.
        if keep_all == Some(false) && self.players.level.hardcore {
            join::set_game_mode(player, GameMode::Spectator, || {
                crate::commands::exec::broadcast_game_mode(global, uuid, GameMode::Spectator);
            });
        }
        if let Some(pos) = landing.ticket {
            self.place_portal_ticket(pos);
        }
        if let Some((arrival, from)) = pearl {
            self.pearl_landed(global, id, arrival, from);
        }
        if finish_tick {
            self.survival.finish_portal_tick(
                &mut self.level.entities,
                &self.level.chunks,
                &self.level.border,
                &self.level.block_entities,
                global,
                &self.dimension,
                id,
            );
        }
        // Plugins see the player in its place: a respawn, or the arrival in another world.
        let from = u32::from(from_world.0);
        let respawned = keep_all.is_some();
        if respawned || from_world != self.id {
            let plugins = global.plugins.clone();
            plugins.player_world(
                respawned,
                &mut crate::plugins::PluginWorld(self, global),
                uuid,
                from,
                [body.pos.x, body.pos.y, body.pos.z],
            );
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
            tags,
            persistent,
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
        self.level.entities.set_tags(id, tags);
        self.level.entities.set_persistent_data(id, persistent);
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
                ..
            } => self.nether_landing(
                global,
                *approx,
                *to_nether,
                entry,
                traveller.width,
                traveller.height,
                traveller.data.game_mode == GameMode::Spectator,
            ),
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
                    missing: false,
                })
            }
            Route::Spawn | Route::Respawn { .. } => {
                // `findRespawnPositionAndUseSpawnBlock`: a respawn (not the End's exit) uses
                // an anchor charge.
                let consume = matches!(traveller.route, Route::Respawn { .. });
                let config = traveller
                    .data
                    .survival
                    .respawn
                    .as_ref()
                    .filter(|r| r.dimension == self.dimension);
                let found = config.map(|r| {
                    vanillite_game::survival::respawn::find_respawn_and_use_spawn_block(
                        &mut self.level,
                        r,
                        consume,
                    )
                });
                if let Some(Some((pos, y_rot, x_rot))) = found {
                    return Some(Landing {
                        pos,
                        y_rot,
                        x_rot,
                        relatives: 0,
                        sound: false,
                        ticket: None,
                        missing: false,
                    });
                }
                // `TeleportTransition.createDefault` / `missingRespawnBlock`: around the world
                // spawn (`adjustSpawnLocation`).
                let (spawn, yaw) = super::effective_spawn(&self.level, self.players.level.spawn);
                let radius = global
                    .level
                    .rules()
                    .get(vanillite_game::survival::rules::Rule::RESPAWN_RADIUS);
                let pos = vanillite_game::survival::respawn::find_spawn(
                    &mut self.level,
                    BlockPos::containing(spawn.x, spawn.y, spawn.z),
                    radius,
                    crate::players::game_mode_by_id(global.level.default_game_type())
                        == Some(GameMode::Adventure),
                );
                Some(Landing {
                    pos,
                    y_rot: yaw,
                    x_rot: 0.0,
                    relatives: 0,
                    sound: false,
                    ticket: None,
                    missing: found.is_some(),
                })
            }
            Route::Pearl(arrival) => Some(Landing {
                pos: arrival.pos,
                y_rot: 0.0,
                x_rot: 0.0,
                relatives: relative::Y_ROT | relative::X_ROT,
                sound: false,
                ticket: None,
                missing: false,
            }),
            &Route::Command { pos, y_rot, x_rot } => Some(Landing {
                pos,
                y_rot,
                x_rot,
                relatives: 0,
                sound: false,
                ticket: None,
                missing: false,
            }),
            Route::Back => Some(Landing {
                pos: traveller.pos,
                y_rot: traveller.y_rot,
                x_rot: traveller.x_rot,
                relatives: 0,
                sound: false,
                ticket: None,
                missing: false,
            }),
        }
    }

    /// `NetherPortalBlock.getExitPortal` in this world: the closest portal to `approx`, else a
    /// new one (not for spectators); the landing in it (`createDimensionTransition`).
    #[allow(clippy::too_many_arguments)]
    fn nether_landing(
        &mut self,
        global: &Global,
        approx: BlockPos,
        to_nether: bool,
        entry: &EntryPortal,
        width: f32,
        height: f32,
        spectator: bool,
    ) -> Option<Landing> {
        let level = &mut self.level;
        // `getExitPortal`.
        let found = portal::find_closest_portal(level, approx, portal::search_radius(to_nether));
        let (exit, ticket) = match found {
            Some(pos) => {
                let state = level.block(pos);
                let axis = HorizontalAxis::of(state)?;
                let rect = portal::largest_rectangle_around(pos, axis, |p| level.block(p) == state);
                (rect, Some(pos))
            }
            None => {
                if spectator {
                    return None;
                }
                let logical_height = global
                    .level
                    .dimension_types
                    .get(self.id.index())
                    .map_or(256, |t| t.logical_height);
                let created = portal::create_portal(level, approx, entry.axis, logical_height);
                let Some(created) = created else {
                    log::error!("Unable to create a portal, likely target out of worldborder");
                    return None;
                };
                (created, None)
            }
        };
        // `createDimensionTransition`.
        let exit_axis =
            HorizontalAxis::of(level.block(exit.min_corner)).unwrap_or(HorizontalAxis::X);
        let (pos, rotation) = portal::exit_position(&exit, exit_axis, entry, width, height);
        let pos = free_position(level, pos, width, height);
        Some(Landing {
            pos,
            y_rot: rotation,
            x_rot: 0.0,
            relatives: KEEP_MOTION_AND_ROTATION,
            sound: true,
            // A found exit keeps its own chunks loaded, a new one those at the player.
            ticket: Some(ticket.unwrap_or_else(|| containing(pos))),
            missing: false,
        })
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
    pub(super) fn enter_gateway(&mut self, id: EntityId, entry: BlockPos) {
        if self.gateway_waits.iter().any(|w| w.entity == id) {
            return;
        }
        let mut wait = GatewayWait {
            entity: id,
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
            self.teleport_through_gateway(wait.entity, pos);
        }
        true
    }

    /// The gateway's teleport within this world: to the bottom centre of `exit`, players
    /// keeping velocity and rotation, then `PLACE_PORTAL_TICKET`.
    fn teleport_through_gateway(&mut self, id: EntityId, exit: BlockPos) {
        let entities = &mut self.level.entities;
        let pos = if entities.player(id).is_some() {
            let pos = Vec3::new(
                f64::from(exit.x) + 0.5,
                f64::from(exit.y),
                f64::from(exit.z) + 0.5,
            );
            let Some(pos) = vanillite_game::player::movement::teleport_within_world(
                entities,
                id,
                pos,
                0.0,
                0.0,
                KEEP_MOTION_AND_ROTATION,
            ) else {
                return;
            };
            pos
        } else {
            portal::teleport_through_gateway(entities, id, exit)
        };
        self.place_portal_ticket(containing(pos));
    }

    /// The player `uuid` of this world goes to `target`, where its ender pearl landed
    /// (`isAllowedToTeleportOwner`: alive; `isAcceptingMessages`).
    fn follow_pearl(
        &mut self,
        global: &Arc<Global>,
        uuid: u128,
        target: WorldId,
        arrival: PearlArrival,
        handed: Option<&HandOver>,
    ) {
        let Some(id) = self.level.entities.player_by_uuid(uuid) else {
            return;
        };
        let allowed = self.level.entities.player(id).is_some_and(|p| {
            !p.survival.is_dead_or_dying()
                && !p.survival.portal.won_game
                && !p.connection.sink.is_closed()
        });
        if !allowed {
            return;
        }
        let Some(traveller) = self.take_player(id, Route::Pearl(arrival)) else {
            return;
        };
        if target == self.id {
            self.arrive(global, traveller);
            return;
        }
        global.players.set_world(Uuid(traveller.uuid), target);
        let traveller = match handed {
            Some(h) => h.give(traveller),
            None => Some(traveller),
        };
        let Some(traveller) = traveller else {
            return;
        };
        let global_arc = Arc::clone(global);
        global.run_in_world(target, move |world: &mut ServerWorld| {
            world.arrive(&global_arc, traveller);
        });
    }

    /// The end of a player's trip to its ender pearl: the endermite where it was (in this
    /// world, as Vanilla adds it to the pearl's level), the portal cooldown, then
    /// [`ender_pearl::land`].
    fn pearl_landed(
        &mut self,
        global: &Global,
        id: EntityId,
        arrival: PearlArrival,
        from: (Vec3, f32),
    ) {
        let state = &global.level;
        let rules = state.rules().clone();
        let hooks = crate::plugins::Hooks::of(&global.plugins, &self.dimension);
        let chunks = &self.level.chunks;
        let blocks = crate::survival::WorldBlocks(chunks);
        let (weather, climate) = (self.survival.weather, self.survival.climate().cloned());
        let raining =
            move |pos| crate::survival::raining_at(chunks, climate.as_deref(), &weather, pos);
        let mut no_gateway = |_| false;
        let dimension = self.dimension.clone();
        let names = |name: &str, uuid| crate::scoreboard::display_name(global, name, uuid);
        let teams = |uuid| crate::scoreboard::entity_team(global, uuid);
        let mut env = self.survival.env(
            state,
            &rules,
            &blocks,
            &raining,
            &dimension,
            &mut no_gateway,
            &hooks,
            &names,
            &teams,
        );
        let entities = &mut self.level.entities;
        if arrival.endermite {
            env.out.push(vanillite_game::survival::Effect::SpawnMob {
                kind: vanillite_data::entity::ids::ENDERMITE,
                pos: from.0,
                y_rot: from.1,
                velocity: Vec3::ZERO,
            });
        }
        if arrival.cooldown
            && let Some(p) = entities.player_mut(id)
        {
            p.survival.portal.cooldown = portal::PLAYER_PORTAL_COOLDOWN;
        }
        ender_pearl::land(entities, &mut env, id);
        self.survival.deliver(&mut self.level.entities, global);
    }
}

/// A player handed from one world to the world that asked for it while that world waits
/// (sequential dimensions): [`TickContext::in_world`] ran the request at once.
struct HandOver {
    /// `None` once the asking world stopped waiting.
    slot: std::sync::Mutex<Option<Option<Traveller>>>,
}

impl HandOver {
    fn waiting() -> Self {
        HandOver {
            slot: std::sync::Mutex::new(Some(None)),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Option<Traveller>>> {
        self.slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Gives the traveller to the waiting world; returns it if nobody waits any more.
    fn give(&self, traveller: Traveller) -> Option<Traveller> {
        let mut slot = self.lock();
        match &mut *slot {
            Some(waiting) => {
                *waiting = Some(traveller);
                None
            }
            None => Some(traveller),
        }
    }

    /// Stops waiting: the traveller handed over meanwhile, if any.
    fn close(&self) -> Option<Traveller> {
        self.lock().take().flatten()
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

/// The chunks a traveller's destination needs before it can be placed: the exit portal search,
/// the End platform, the place of a teleport command.
fn arrival_area(route: &Route) -> Option<(ChunkPos, i32)> {
    match route {
        Route::NetherPortal {
            approx, to_nether, ..
        } => Some((approx.chunk(), portal::search_chunk_radius(*to_nether))),
        Route::EndPlatform => Some((
            portal::END_SPAWN_POINT.chunk(),
            portal::PORTAL_TICKET_RADIUS,
        )),
        Route::Command { pos, .. } => Some((
            ChunkPos::new(
                vanillite_base::math::floor(pos.x) >> 4,
                vanillite_base::math::floor(pos.z) >> 4,
            ),
            1,
        )),
        Route::Spawn | Route::Respawn { .. } | Route::Back | Route::Pearl(_) => None,
    }
}
