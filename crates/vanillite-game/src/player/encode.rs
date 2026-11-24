//! Encoding of the entity tracker's updates into play packets: each update is encoded once and
//! the same packets go to all its viewers ([`TrackerOutput::fan_out`]).
//!
//! [`TrackerOutput::fan_out`]: vanillite_world::tracking::TrackerOutput::fan_out

use std::sync::Arc;

use vanillite_base::Vec3;
use vanillite_data::entity::ids;
use vanillite_protocol::SharedPacket;
use vanillite_protocol::codec::write_varint;
use vanillite_protocol::packets::play::clientbound::{
    AddEntity, BundleDelimiter, EntityPositionSync, MoveEntityPos, MoveEntityPosRot, MoveEntityRot,
    PositionPath, RemoveEntities, RotateHead, SetEntityData, SetEntityMotion, VecDelta,
};
use vanillite_protocol::types::{self, LpVec3, RawBytes, Uuid, VarIntList};
use vanillite_world::entity::{EntityRef, Pose};
use vanillite_world::tracking::{EntityUpdate, Message};

use super::MainHand;
use crate::entities::Entities;

/// The packets of one update, shared by all its viewers.
pub type Encoded = Vec<Arc<SharedPacket>>;

/// Entity data ids (`EntityDataAccessor` ids of 26.3).
mod data_id {
    pub const SHARED_FLAGS: u8 = 0;
    pub const AIR_SUPPLY: u8 = 1;
    pub const POSE: u8 = 6;
    pub const HEALTH: u8 = 9;
    pub const PLAYER_MAIN_HAND: u8 = 15;
    pub const PLAYER_MODE_CUSTOMISATION: u8 = 16;
    pub const PLAYER_ABSORPTION: u8 = 17;
    pub const PLAYER_SCORE: u8 = 18;
}

/// Entity data serializer ids (`EntityDataSerializers` registration order of 26.3).
mod serializer {
    pub const BYTE: i32 = 0;
    pub const INT: i32 = 1;
    pub const FLOAT: i32 = 3;
    pub const POSE: i32 = 20;
    pub const HUMANOID_ARM: i32 = 42;
}

fn wire_vec(v: Vec3) -> types::Vec3 {
    types::Vec3 {
        x: v.x,
        y: v.y,
        z: v.z,
    }
}

fn pose_id(pose: Pose) -> i32 {
    // `Pose` is `repr(u8)` in protocol order.
    #[allow(clippy::as_conversions)]
    let id = pose as u8;
    i32::from(id)
}

/// Appends one data item.
fn item(out: &mut Vec<u8>, id: u8, serializer: i32, value: impl FnOnce(&mut Vec<u8>)) {
    out.push(id);
    write_varint(out, serializer);
    value(out);
}

/// The synched data of an entity: all values (`all`) or only those that differ from their
/// defaults (`getNonDefaultValues`, for pairing). Ends with the `0xFF` terminator; `None` when
/// nothing is to be sent.
fn entity_data(entity: &EntityRef<'_>, entities: &Entities, all: bool) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(16);
    let flags = entity.flags().shared_byte();
    if all || flags != 0 {
        item(&mut out, data_id::SHARED_FLAGS, serializer::BYTE, |o| {
            o.push(flags);
        });
    }
    let player = if entity.kind() == ids::PLAYER {
        entities.player(entity.id())
    } else {
        None
    };
    if let Some(player) = player {
        let air = player.survival.air;
        if all || air != crate::survival::player::MAX_AIR {
            item(&mut out, data_id::AIR_SUPPLY, serializer::INT, |o| {
                write_varint(o, air);
            });
        }
    }
    let pose = entity.pose();
    if all || pose != Pose::Standing {
        item(&mut out, data_id::POSE, serializer::POSE, |o| {
            write_varint(o, pose_id(pose));
        });
    }
    if let Some(player) = player {
        // `LivingEntity` defines the health as 1.0 and sets it to the maximum afterwards.
        let health = player.survival.health;
        if all || health != 1.0 {
            item(&mut out, data_id::HEALTH, serializer::FLOAT, |o| {
                o.extend_from_slice(&health.to_be_bytes());
            });
        }
        let hand = player.client.main_hand;
        if all || hand != MainHand::Right {
            item(
                &mut out,
                data_id::PLAYER_MAIN_HAND,
                serializer::HUMANOID_ARM,
                |o| {
                    write_varint(o, i32::from(hand == MainHand::Right));
                },
            );
        }
        let parts = player.client.model_customisation;
        if all || parts != 0 {
            item(
                &mut out,
                data_id::PLAYER_MODE_CUSTOMISATION,
                serializer::BYTE,
                |o| o.push(parts),
            );
        }
        let absorption = player.survival.absorption;
        if all || absorption != 0.0 {
            item(
                &mut out,
                data_id::PLAYER_ABSORPTION,
                serializer::FLOAT,
                |o| o.extend_from_slice(&absorption.to_be_bytes()),
            );
        }
        let score = player.survival.score;
        if all || score != 0 {
            item(&mut out, data_id::PLAYER_SCORE, serializer::INT, |o| {
                write_varint(o, score);
            });
        }
    }
    if out.is_empty() {
        return None;
    }
    out.push(0xFF);
    Some(out)
}

/// Encodes one tracker message. `None` skips it (the entity is gone and nothing is left to
/// send).
pub fn encode_update(message: &Message, entities: &Entities) -> Option<Encoded> {
    let id = message.network_id.0;
    let one = |p: Arc<SharedPacket>| Some(vec![p]);
    match message.update {
        EntityUpdate::Spawn {
            kind,
            uuid,
            pos,
            velocity,
            y_rot,
            x_rot,
            y_head_rot,
        } => {
            let add = SharedPacket::of(&AddEntity {
                entity_id: id,
                uuid: Uuid(uuid),
                entity_type: i32::from(kind.0),
                position: wire_vec(pos),
                movement: LpVec3(wire_vec(velocity)),
                x_rot,
                y_rot,
                y_head_rot,
                // `getAddEntityPacket` data: the block state of a falling block.
                data: entities
                    .falling_block(message.entity)
                    .map_or(0, |f| i32::from(f.state.0)),
            });
            let data = entities
                .store
                .get(message.entity)
                .and_then(|e| entity_data(&e, entities, false));
            match data {
                // `sendPairingData` bundles the spawn with its data.
                Some(items) => Some(vec![
                    SharedPacket::of(&BundleDelimiter),
                    add,
                    SharedPacket::of(&SetEntityData {
                        entity_id: id,
                        items: RawBytes(items),
                    }),
                    SharedPacket::of(&BundleDelimiter),
                ]),
                None => Some(vec![add]),
            }
        }
        EntityUpdate::Remove => one(SharedPacket::of(&RemoveEntities {
            ids: VarIntList(vec![id]),
        })),
        EntityUpdate::Sync {
            pos,
            velocity: _,
            y_rot,
            x_rot,
            on_ground,
        } => one(SharedPacket::of(&EntityPositionSync {
            entity_id: id,
            path: PositionPath::Linear(wire_vec(pos)),
            y_rot,
            x_rot,
            on_ground,
        })),
        EntityUpdate::Pos {
            dx,
            dy,
            dz,
            on_ground,
        } => one(SharedPacket::of(&MoveEntityPos {
            entity_id: id,
            delta: VecDelta::Linear([dx, dy, dz]),
            on_ground,
        })),
        EntityUpdate::PosRot {
            dx,
            dy,
            dz,
            y_rot,
            x_rot,
            on_ground,
        } => one(SharedPacket::of(&MoveEntityPosRot {
            entity_id: id,
            delta: VecDelta::Linear([dx, dy, dz]),
            y_rot,
            x_rot,
            on_ground,
        })),
        EntityUpdate::Rot {
            y_rot,
            x_rot,
            on_ground,
        } => one(SharedPacket::of(&MoveEntityRot {
            entity_id: id,
            on_ground,
            y_rot,
            x_rot,
        })),
        EntityUpdate::HeadRot(head) => one(SharedPacket::of(&RotateHead {
            entity_id: id,
            head_y_rot: head,
        })),
        EntityUpdate::Motion(velocity) => one(SharedPacket::of(&SetEntityMotion {
            entity_id: id,
            movement: LpVec3(wire_vec(velocity)),
        })),
        EntityUpdate::Data => {
            let entity = entities.store.get(message.entity)?;
            let items = entity_data(&entity, entities, true)?;
            one(SharedPacket::of(&SetEntityData {
                entity_id: id,
                items: RawBytes(items),
            }))
        }
    }
}
