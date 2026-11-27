//! The locator bar of a world (`ServerWaypointManager`, `WaypointTransmitter`): every player
//! transmits a waypoint the other players of the world receive as a block position, a chunk or,
//! when really far, an azimuth. Sneaking, invisibility and heads (the `waypoint_transmit_range`
//! modifiers) hide a player; spectators receive all.
//!
//! Vanilla updates the connections whenever a player moves; here they are updated once per
//! tick for the players that moved or changed, with the same connections and packets.

use std::sync::Arc;

use vanillite_base::hash::{FxHashMap, FxHashSet};
use vanillite_base::{BlockPos, ChunkPos, Vec3};
use vanillite_config::GameMode;
use vanillite_data::entity::ids::PLAYER;
use vanillite_game::effect::attribute::Attribute;
use vanillite_game::entities::Entities;
use vanillite_game::item::EquipmentSlot;
use vanillite_game::player::ChunkView;
use vanillite_game::player::sink::PacketSink;
use vanillite_protocol::packets::play::clientbound::{Waypoint, WaypointOperation, WaypointTarget};
use vanillite_protocol::types::{Identifier, Uuid};
use vanillite_world::entity::{EntityId, Pose};

use crate::global::Global;

/// `WaypointTransmitter.REALLY_FAR_DISTANCE`.
const REALLY_FAR: f64 = 332.0;
/// `AttributeModifier.Operation.ADD_MULTIPLIED_TOTAL`.
const ADD_MULTIPLIED_TOTAL: u8 = 2;
/// `TeamColor.BLACK` on the locator bar (`Waypoint.Icon.cloneAndAssignStyle`).
const BLACK: i32 = -13_619_152;
/// `TeamColor.rgb` by colour id.
const TEAM_RGB: [i32; 16] = [
    0x00_0000, 0x00_00AA, 0x00_AA00, 0x00_AAAA, 0xAA_0000, 0xAA_00AA, 0xFF_AA00, 0xAA_AAAA,
    0x55_5555, 0x55_55FF, 0x55_FF55, 0x55_FFFF, 0xFF_5555, 0xFF_55FF, 0xFF_FF55, 0xFF_FFFF,
];

/// `Waypoint.Icon` after `cloneAndAssignStyle`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Icon {
    style: Option<String>,
    color: Option<i32>,
}

/// What the locator bar needs of a player this tick.
struct Snapshot {
    uuid: u128,
    sink: Arc<dyn PacketSink>,
    pos: Vec3,
    block: BlockPos,
    chunk: ChunkPos,
    view: Option<ChunkView>,
    /// `isTransmittingWaypoint`: `waypoint_transmit_range` > 0.
    transmit_range: f64,
    receive_range: f64,
    spectator: bool,
    /// The vehicles it rides, innermost first (`hasIndirectPassenger`).
    vehicles: Vec<EntityId>,
    icon: Icon,
}

impl Snapshot {
    fn transmitting(&self) -> bool {
        self.transmit_range > 0.0
    }

    /// Whether anything a connection depends on changed since `before`.
    fn changed(&self, before: &Snapshot) -> bool {
        self.pos != before.pos
            || self.view != before.view
            || self.transmit_range.to_bits() != before.transmit_range.to_bits()
            || self.spectator != before.spectator
            || self.vehicles != before.vehicles
            || self.icon != before.icon
    }

    fn distance(&self, other: &Snapshot) -> f64 {
        let (dx, dy, dz) = (
            self.pos.x - other.pos.x,
            self.pos.y - other.pos.y,
            self.pos.z - other.pos.z,
        );
        (dx * dx + dy * dy + dz * dz).sqrt()
    }

    fn sees_chunk(&self, chunk: ChunkPos) -> bool {
        self.view.is_some_and(|v| v.contains(chunk, false))
    }
}

/// One `WaypointTransmitter.Connection`.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Connection {
    Azimuth(f32),
    Block(BlockPos),
    Chunk(ChunkPos),
}

/// The locator bar state of one world.
#[derive(Default)]
pub(crate) struct Waypoints {
    players: FxHashMap<EntityId, Snapshot>,
    /// By (receiver, source), with the icon the receiver got.
    connections: FxHashMap<(EntityId, EntityId), (Connection, Icon)>,
}

/// `WaypointTransmitter.doesSourceIgnoreReceiver`.
fn ignores(source_id: EntityId, source: &Snapshot, receiver: &Snapshot) -> bool {
    if receiver.spectator {
        return false;
    }
    if source.spectator || receiver.vehicles.contains(&source_id) {
        return true;
    }
    source.distance(receiver) >= source.transmit_range.min(receiver.receive_range)
}

/// The azimuth of `source` seen from `receiver` (`EntityAzimuthConnection`).
#[allow(clippy::cast_possible_truncation, clippy::as_conversions)]
fn azimuth(source: &Snapshot, receiver: &Snapshot) -> f32 {
    // `receiver.position().subtract(source.position()).rotateClockwise90()`.
    let (x, z) = (
        -(receiver.pos.z - source.pos.z),
        receiver.pos.x - source.pos.x,
    );
    z.atan2(x) as f32
}

/// `LivingEntity.makeWaypointConnectionWith`.
fn connect(source_id: EntityId, source: &Snapshot, receiver: &Snapshot) -> Option<Connection> {
    if ignores(source_id, source, receiver) {
        None
    } else if source.distance(receiver) > REALLY_FAR {
        Some(Connection::Azimuth(azimuth(source, receiver)))
    } else if !receiver.sees_chunk(source.chunk) {
        Some(Connection::Chunk(source.chunk))
    } else {
        Some(Connection::Block(source.block))
    }
}

/// `Connection.isBroken`.
fn broken(c: Connection, source_id: EntityId, source: &Snapshot, receiver: &Snapshot) -> bool {
    match c {
        Connection::Azimuth(_) => {
            ignores(source_id, source, receiver)
                || receiver.sees_chunk(source.chunk)
                || source.distance(receiver) <= REALLY_FAR
        }
        Connection::Block(last) => {
            manhattan(last, source.block) > 1 || ignores(source_id, source, receiver)
        }
        Connection::Chunk(last) => {
            chessboard(last, source.chunk) > 1
                || ignores(source_id, source, receiver)
                || receiver.sees_chunk(last)
        }
    }
}

fn manhattan(a: BlockPos, b: BlockPos) -> i64 {
    let d = |a: i32, b: i32| (i64::from(a) - i64::from(b)).abs();
    d(a.x, b.x) + d(a.y, b.y) + d(a.z, b.z)
}

fn chessboard(a: ChunkPos, b: ChunkPos) -> i64 {
    let d = |a: i32, b: i32| (i64::from(a) - i64::from(b)).abs();
    d(a.x, b.x).max(d(a.z, b.z))
}

fn packet(
    op: WaypointOperation,
    uuid: u128,
    icon: Option<&Icon>,
    c: Option<Connection>,
) -> Waypoint {
    let style = icon
        .and_then(|i| i.style.as_deref())
        .and_then(|s| Identifier::new(s).ok());
    Waypoint {
        operation: op,
        id: Ok(Uuid(uuid)),
        style: style.unwrap_or_else(|| Identifier::vanilla("default")),
        color: icon.and_then(|i| i.color),
        target: match c {
            None => WaypointTarget::Empty,
            Some(Connection::Block(p)) => WaypointTarget::Position {
                x: p.x,
                y: p.y,
                z: p.z,
            },
            Some(Connection::Chunk(p)) => WaypointTarget::Chunk { x: p.x, z: p.z },
            Some(Connection::Azimuth(a)) => WaypointTarget::Azimuth(a),
        },
    }
}

fn untrack(sink: &dyn PacketSink, uuid: u128) {
    sink.send(&packet(WaypointOperation::Untrack, uuid, None, None));
}

/// `waypoint_transmit_range` of a player: the default with the modifiers of its equipment
/// (heads, carved pumpkins), sneaking and its effects (invisibility).
fn transmit_range(player: &vanillite_game::player::PlayerData, crouching: bool) -> f64 {
    let attribute = Attribute::WAYPOINT_TRANSMIT_RANGE;
    let mut modifiers = Vec::new();
    for slot in [
        EquipmentSlot::MainHand,
        EquipmentSlot::OffHand,
        EquipmentSlot::Feet,
        EquipmentSlot::Legs,
        EquipmentSlot::Chest,
        EquipmentSlot::Head,
    ] {
        vanillite_game::item::attribute_modifiers(
            player.inventory.equipment(slot),
            slot,
            attribute.name(),
            &mut modifiers,
        );
    }
    if crouching {
        // `ServerPlayer.WAYPOINT_TRANSMIT_RANGE_CROUCH_MODIFIER`.
        modifiers.push((ADD_MULTIPLIED_TOTAL, -1.0));
    }
    let base = attribute.base_of(PLAYER);
    let (min, max) = (0.0, attribute.sanitize(f64::MAX));
    let value = vanillite_game::item::attribute_value(base, &modifiers, min, max);
    player.survival.effects.attribute(attribute, value)
}

impl Waypoints {
    /// The tick's update: the players that left get their connections removed, then the
    /// connections of the players that moved or changed are made, updated or broken.
    pub(crate) fn tick(&mut self, entities: &Entities, global: &Global) {
        let enabled = global
            .level
            .rules()
            .bool(vanillite_game::survival::rules::Rule::LOCATOR_BAR);
        if !enabled {
            // `breakAllConnections`.
            for ((receiver, source), _) in self.connections.drain() {
                if let (Some(r), Some(s)) = (self.players.get(&receiver), self.players.get(&source))
                {
                    untrack(r.sink.as_ref(), s.uuid);
                }
            }
            self.players.clear();
            return;
        }
        let now = snapshots(entities, global);
        // `removePlayer`: the leaving player's own connections and those to it break.
        let gone: Vec<EntityId> = self
            .players
            .keys()
            .filter(|id| !now.contains_key(id))
            .copied()
            .collect();
        if !gone.is_empty() {
            let players = &self.players;
            self.connections.retain(|(receiver, source), _| {
                if !gone.contains(receiver) && !gone.contains(source) {
                    return true;
                }
                if let (Some(r), Some(s)) = (players.get(receiver), players.get(source)) {
                    untrack(r.sink.as_ref(), s.uuid);
                }
                false
            });
        }
        let changed: Vec<EntityId> = now
            .iter()
            .filter(|(id, snap)| {
                self.players
                    .get(id)
                    .is_none_or(|before| snap.changed(before))
            })
            .map(|(id, _)| *id)
            .collect();
        self.players = now;
        if changed.is_empty() {
            return;
        }
        let ids: Vec<EntityId> = self.players.keys().copied().collect();
        let changed_set: FxHashSet<EntityId> = changed.iter().copied().collect();
        for &moved in &changed {
            for &other in &ids {
                if other == moved {
                    continue;
                }
                // Both directions; a pair of two changed players is handled once.
                if other < moved && changed_set.contains(&other) {
                    continue;
                }
                self.update_pair(moved, other);
                self.update_pair(other, moved);
            }
        }
    }

    /// `ServerWaypointManager.updateConnection` / `createConnection` for one pair.
    fn update_pair(&mut self, receiver_id: EntityId, source_id: EntityId) {
        let (Some(receiver), Some(source)) =
            (self.players.get(&receiver_id), self.players.get(&source_id))
        else {
            return;
        };
        let key = (receiver_id, source_id);
        let sink = receiver.sink.as_ref();
        if !source.transmitting() {
            // `untrackWaypoint`.
            if self.connections.remove(&key).is_some() {
                untrack(sink, source.uuid);
            }
            return;
        }
        let existing = self.connections.get(&key).cloned();
        let remake = match &existing {
            None => true,
            Some((c, icon)) => *icon != source.icon || broken(*c, source_id, source, receiver),
        };
        if !remake {
            if let Some((c, icon)) = existing {
                let next = match c {
                    Connection::Azimuth(last) => {
                        let now = azimuth(source, receiver);
                        ((now - last).abs() > 0.008_726_646).then_some(Connection::Azimuth(now))
                    }
                    Connection::Block(last) => (manhattan(last, source.block) > 0)
                        .then_some(Connection::Block(source.block)),
                    Connection::Chunk(last) => (chessboard(last, source.chunk) > 0)
                        .then_some(Connection::Chunk(source.chunk)),
                };
                if let Some(next) = next {
                    sink.send(&packet(
                        WaypointOperation::Update,
                        source.uuid,
                        Some(&icon),
                        Some(next),
                    ));
                    self.connections.insert(key, (next, icon));
                }
            }
            return;
        }
        match connect(source_id, source, receiver) {
            Some(c) => {
                sink.send(&packet(
                    WaypointOperation::Track,
                    source.uuid,
                    Some(&source.icon),
                    Some(c),
                ));
                self.connections.insert(key, (c, source.icon.clone()));
            }
            None => {
                if self.connections.remove(&key).is_some() {
                    untrack(sink, source.uuid);
                }
            }
        }
    }
}

/// The players of the world as the locator bar sees them.
fn snapshots(entities: &Entities, global: &Global) -> FxHashMap<EntityId, Snapshot> {
    let icons = global.systems.waypoints().clone();
    let board = global.systems.scoreboard();
    let receive = Attribute::WAYPOINT_RECEIVE_RANGE.base_of(PLAYER);
    let mut out = FxHashMap::default();
    for (id, player) in entities.players.iter() {
        let Some(entity) = entities.store.get(id) else {
            continue;
        };
        let pos = entity.pos();
        let block = vanillite_game::player::play::block_pos_of(pos);
        let mut vehicles = Vec::new();
        let mut at = id;
        while let Some(v) = entities.vehicle_of(at) {
            if vehicles.contains(&v) {
                break;
            }
            vehicles.push(v);
            at = v;
        }
        let uuid = entity.uuid();
        let own = icons.get(&uuid);
        // `cloneAndAssignStyle`: the icon's colour, else the team's.
        let color = own.and_then(|i| i.color).or_else(|| {
            let color = board.team_of(&player.profile.name)?.color?;
            Some(if color == 0 {
                BLACK
            } else {
                TEAM_RGB.get(usize::from(color)).copied()?
            })
        });
        out.insert(
            id,
            Snapshot {
                uuid,
                sink: Arc::clone(&player.connection.sink),
                pos,
                block,
                chunk: ChunkPos::new(block.x >> 4, block.z >> 4),
                view: player.connection.chunks.view(),
                transmit_range: transmit_range(player, entity.pose() == Pose::Crouching),
                receive_range: receive,
                spectator: player.game_mode == GameMode::Spectator,
                vehicles,
                icon: Icon {
                    style: own.and_then(|i| i.style.clone()),
                    color,
                },
            },
        );
    }
    out
}
