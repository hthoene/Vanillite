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
    PositionPath, RemoveEntities, RotateHead, SetEntityData, SetEntityMotion, SetEquipment,
    VecDelta,
};
use vanillite_protocol::types::{self, LpVec3, RawBytes, Uuid, VarIntList};
use vanillite_world::entity::{EntityRef, Pose};
use vanillite_world::tracking::{EntityUpdate, Message};

use super::MainHand;
use crate::entities::Entities;
use crate::item::{EquipmentSlot, ItemStack};
use vanillite_protocol::codec::Encode;

/// The packets of one update, shared by all its viewers.
pub type Encoded = Vec<Arc<SharedPacket>>;

/// Entity data ids (`EntityDataAccessor` ids of 26.3).
mod data_id {
    pub const SHARED_FLAGS: u8 = 0;
    pub const CUSTOM_NAME: u8 = 2;
    pub const AIR_SUPPLY: u8 = 1;
    pub const POSE: u8 = 6;
    pub const ITEM_ENTITY_ITEM: u8 = 8;
    pub const HEALTH: u8 = 9;
    pub const PLAYER_MAIN_HAND: u8 = 15;
    pub const TNT_FUSE: u8 = 8;
    pub const TNT_BLOCK_STATE: u8 = 9;
    pub const PLAYER_MODE_CUSTOMISATION: u8 = 16;
    pub const PLAYER_ABSORPTION: u8 = 17;
    pub const PLAYER_SCORE: u8 = 18;
    pub const LIVING_FLAGS: u8 = 8;
    pub const MOB_FLAGS: u8 = 15;
    pub const KIND_16: u8 = 16;
    pub const KIND_17: u8 = 17;
    pub const KIND_18: u8 = 18;
    pub const KIND_19: u8 = 19;
    pub const ORB_VALUE: u8 = 8;
    pub const ARROW_FLAGS: u8 = 8;
    pub const ARROW_IN_GROUND: u8 = 10;
}

/// Entity data serializer ids (`EntityDataSerializers` registration order of 26.3).
#[allow(dead_code)]
pub mod serializer {
    pub const BYTE: i32 = 0;
    pub const INT: i32 = 1;
    pub const LONG: i32 = 2;
    pub const FLOAT: i32 = 3;
    pub const STRING: i32 = 4;
    pub const COMPONENT: i32 = 5;
    pub const OPTIONAL_COMPONENT: i32 = 6;
    pub const ITEM_STACK: i32 = 7;
    pub const BOOLEAN: i32 = 8;
    pub const ROTATIONS: i32 = 9;
    pub const BLOCK_POS: i32 = 10;
    pub const OPTIONAL_BLOCK_POS: i32 = 11;
    pub const DIRECTION: i32 = 12;
    pub const OPTIONAL_LIVING_ENTITY_REFERENCE: i32 = 13;
    pub const BLOCK_STATE: i32 = 14;
    pub const OPTIONAL_BLOCK_STATE: i32 = 15;
    pub const PARTICLE: i32 = 16;
    pub const PARTICLES: i32 = 17;
    pub const VILLAGER_DATA: i32 = 18;
    pub const OPTIONAL_UNSIGNED_INT: i32 = 19;
    pub const POSE: i32 = 20;
    pub const CAT_VARIANT: i32 = 21;
    pub const CAT_SOUND_VARIANT: i32 = 22;
    pub const COW_VARIANT: i32 = 23;
    pub const COW_SOUND_VARIANT: i32 = 24;
    pub const WOLF_VARIANT: i32 = 25;
    pub const WOLF_SOUND_VARIANT: i32 = 26;
    pub const FROG_VARIANT: i32 = 27;
    pub const PIG_VARIANT: i32 = 28;
    pub const PIG_SOUND_VARIANT: i32 = 29;
    pub const CHICKEN_VARIANT: i32 = 30;
    pub const CHICKEN_SOUND_VARIANT: i32 = 31;
    pub const ZOMBIE_NAUTILUS_VARIANT: i32 = 32;
    pub const OPTIONAL_GLOBAL_POS: i32 = 33;
    pub const PAINTING_VARIANT: i32 = 34;
    pub const SNIFFER_STATE: i32 = 35;
    pub const ARMADILLO_STATE: i32 = 36;
    pub const COPPER_GOLEM_STATE: i32 = 37;
    pub const WEATHERING_COPPER_STATE: i32 = 38;
    pub const VECTOR3: i32 = 39;
    pub const QUATERNION: i32 = 40;
    pub const RESOLVABLE_PROFILE: i32 = 41;
    pub const HUMANOID_ARM: i32 = 42;
    pub const DYE_COLOR: i32 = 43;
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
    let name = entities
        .mob(entity.id())
        .and_then(|m| m.custom_name.as_deref())
        .or_else(|| {
            entities
                .boat(entity.id())
                .and_then(|b| b.custom_name.as_deref())
        });
    if all || name.is_some() {
        item(
            &mut out,
            data_id::CUSTOM_NAME,
            serializer::OPTIONAL_COMPONENT,
            |o| {
                o.push(u8::from(name.is_some()));
                if let Some(n) = name {
                    o.extend_from_slice(n);
                }
            },
        );
    }
    let pose = entity.pose();
    if all || pose != Pose::Standing {
        item(&mut out, data_id::POSE, serializer::POSE, |o| {
            write_varint(o, pose_id(pose));
        });
    }
    if let Some(player) = player {
        let living = crate::player::using::living_flags(player.survival.using_item);
        if all || living != 0 {
            item(&mut out, data_id::LIVING_FLAGS, serializer::BYTE, |o| {
                o.push(living);
            });
        }
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
    if let Some(mob) = entities.mob(entity.id()) {
        mob_data(&mut out, mob, all);
    }
    if let Some(b) = entities.boat(entity.id()) {
        if all || b.hurt_time != 0 {
            item(&mut out, 8, serializer::INT, |o| {
                write_varint(o, b.hurt_time)
            });
        }
        if all || b.hurt_dir != 1 {
            item(&mut out, 9, serializer::INT, |o| {
                write_varint(o, b.hurt_dir)
            });
        }
        if all || b.damage != 0.0 {
            item(&mut out, 10, serializer::FLOAT, |o| {
                o.extend_from_slice(&b.damage.to_be_bytes());
            });
        }
        if all || b.paddles.0 {
            item(&mut out, 11, serializer::BOOLEAN, |o| {
                o.push(u8::from(b.paddles.0))
            });
        }
        if all || b.paddles.1 {
            item(&mut out, 12, serializer::BOOLEAN, |o| {
                o.push(u8::from(b.paddles.1))
            });
        }
    }
    if let Some(orb) = entities.orb(entity.id())
        && (all || orb.value != 0)
    {
        item(&mut out, data_id::ORB_VALUE, serializer::INT, |o| {
            write_varint(o, orb.value);
        });
    }
    if let Some(d) = entities.missile(entity.id()) {
        crate::mob::fam::missile_encode(&mut DataWriter { out: &mut out, all }, d);
    }
    if let Some(arrow) = entities.arrow(entity.id()) {
        if all || arrow.crit {
            item(&mut out, data_id::ARROW_FLAGS, serializer::BYTE, |o| {
                o.push(u8::from(arrow.crit));
            });
        }
        if all || arrow.in_ground {
            item(
                &mut out,
                data_id::ARROW_IN_GROUND,
                serializer::BOOLEAN,
                |o| {
                    o.push(u8::from(arrow.in_ground));
                },
            );
        }
    }
    if entity.kind() == ids::TNT
        && let Some(tnt) = entities.primed_tnt(entity.id())
    {
        if all || tnt.fuse != 80 {
            item(&mut out, data_id::TNT_FUSE, serializer::INT, |o| {
                write_varint(o, tnt.fuse);
            });
        }
        let default = vanillite_data::block::ids::TNT.default_state();
        if all || tnt.state != default {
            item(
                &mut out,
                data_id::TNT_BLOCK_STATE,
                serializer::BLOCK_STATE,
                |o| {
                    write_varint(o, i32::from(tnt.state.0));
                },
            );
        }
    }
    if entity.kind() == ids::ITEM
        && let Some(data) = entities.item(entity.id())
        && (all || !data.stack.is_empty())
    {
        item(
            &mut out,
            data_id::ITEM_ENTITY_ITEM,
            serializer::ITEM_STACK,
            |o| {
                data.stack.encode(o);
            },
        );
    }
    if out.is_empty() {
        return None;
    }
    out.push(0xFF);
    Some(out)
}

/// Writes synched data items of a mob kind: every value (`all`, updates) or only those that
/// differ from their defaults (pairing).
#[derive(Debug)]
pub struct DataWriter<'a> {
    out: &'a mut Vec<u8>,
    all: bool,
}

impl<'a> DataWriter<'a> {
    /// A writer appending to `out` (`all`: every value, else only non-default ones).
    pub fn new(out: &'a mut Vec<u8>, all: bool) -> DataWriter<'a> {
        DataWriter { out, all }
    }

    /// An item with a custom encoding; `is_default` skips it when pairing.
    pub fn raw(
        &mut self,
        id: u8,
        serializer: i32,
        is_default: bool,
        value: impl FnOnce(&mut Vec<u8>),
    ) {
        if self.all || !is_default {
            item(self.out, id, serializer, value);
        }
    }

    pub fn byte(&mut self, id: u8, v: u8, default: u8) {
        self.raw(id, serializer::BYTE, v == default, |o| o.push(v));
    }

    pub fn boolean(&mut self, id: u8, v: bool, default: bool) {
        self.raw(id, serializer::BOOLEAN, v == default, |o| {
            o.push(u8::from(v))
        });
    }

    /// An `INT` (var int) item.
    pub fn int(&mut self, id: u8, v: i32, default: i32) {
        self.varint(id, serializer::INT, v, default);
    }

    /// A var int item of any serializer (registry holders, enums, `INT`).
    pub fn varint(&mut self, id: u8, serializer: i32, v: i32, default: i32) {
        self.raw(id, serializer, v == default, |o| write_varint(o, v));
    }

    pub fn float(&mut self, id: u8, v: f32, default: f32) {
        #[allow(clippy::float_cmp)]
        let is_default = v == default;
        self.raw(id, serializer::FLOAT, is_default, |o| {
            o.extend_from_slice(&v.to_be_bytes());
        });
    }

    /// An `ITEM_STACK` item.
    pub fn stack(&mut self, id: u8, v: &ItemStack) {
        self.raw(id, serializer::ITEM_STACK, v.is_empty(), |o| v.encode(o));
    }

    /// An `OPTIONAL_BLOCK_POS` item.
    pub fn optional_block_pos(&mut self, id: u8, v: Option<vanillite_base::BlockPos>) {
        self.raw(id, serializer::OPTIONAL_BLOCK_POS, v.is_none(), |o| {
            o.push(u8::from(v.is_some()));
            if let Some(p) = v {
                o.extend_from_slice(&p.as_long().to_be_bytes());
            }
        });
    }

    /// An `OPTIONAL_LIVING_ENTITY_REFERENCE` item (a UUID).
    pub fn optional_uuid(&mut self, id: u8, v: Option<u128>) {
        self.raw(
            id,
            serializer::OPTIONAL_LIVING_ENTITY_REFERENCE,
            v.is_none(),
            |o| {
                o.push(u8::from(v.is_some()));
                if let Some(u) = v {
                    o.extend_from_slice(&u.to_be_bytes());
                }
            },
        );
    }
}

/// The synched data of a mob after the entity's own: health, mob flags and the kind's values.
fn mob_data(out: &mut Vec<u8>, mob: &crate::mob::MobData, all: bool) {
    use crate::mob::Kind;
    let living = crate::mob::kinds::living_flags(mob);
    if all || living != 0 {
        item(out, data_id::LIVING_FLAGS, serializer::BYTE, |o| {
            o.push(living)
        });
    }
    if all || mob.health != 1.0 {
        item(out, data_id::HEALTH, serializer::FLOAT, |o| {
            o.extend_from_slice(&mob.health.to_be_bytes());
        });
    }
    let flags =
        u8::from(mob.no_ai) | (u8::from(mob.left_handed) << 1) | (u8::from(mob.aggressive) << 2);
    if all || flags != 0 {
        item(out, data_id::MOB_FLAGS, serializer::BYTE, |o| o.push(flags));
    }
    let boolean = |out: &mut Vec<u8>, id: u8, v: bool| {
        if all || v {
            item(out, id, serializer::BOOLEAN, |o| o.push(u8::from(v)));
        }
    };
    let variant = |out: &mut Vec<u8>, id: u8, serializer: i32, v: i32| {
        // The default variant is `temperate` (id 1 of `cold`, `temperate`, `warm`).
        if all || v != 1 {
            item(out, id, serializer, |o| write_varint(o, v));
        }
    };
    if mob.kind.family() != crate::mob::Family::Classic {
        crate::mob::fam::encode_data(&mut DataWriter { out, all }, mob);
        return;
    }
    match mob.kind {
        Kind::Zombie => boolean(out, data_id::KIND_16, mob.baby),
        Kind::Creeper => {
            if all || mob.swell_dir != -1 {
                item(out, data_id::KIND_16, serializer::INT, |o| {
                    write_varint(o, mob.swell_dir);
                });
            }
            boolean(out, data_id::KIND_17, mob.powered);
            boolean(out, data_id::KIND_18, mob.ignited);
        }
        Kind::Spider => {
            let f = u8::from(mob.climbing);
            if all || f != 0 {
                item(out, data_id::KIND_16, serializer::BYTE, |o| o.push(f));
            }
        }
        Kind::Enderman => {
            if all || mob.carried.is_some() {
                item(
                    out,
                    data_id::KIND_16,
                    serializer::OPTIONAL_BLOCK_STATE,
                    |o| {
                        write_varint(o, mob.carried.map_or(0, |s| i32::from(s.0)));
                    },
                );
            }
            boolean(out, data_id::KIND_17, mob.creepy);
        }
        Kind::Skeleton => {}
        Kind::Cow | Kind::Pig | Kind::Sheep | Kind::Chicken => {
            boolean(out, data_id::KIND_16, mob.baby);
            match mob.kind {
                Kind::Sheep => {
                    let wool = (mob.sheep_color & 0x0F) | if mob.sheared { 0x10 } else { 0 };
                    if all || wool != 0 {
                        item(out, data_id::KIND_18, serializer::BYTE, |o| o.push(wool));
                    }
                }
                Kind::Pig => {
                    if all || mob.boost_total != 0 {
                        item(out, data_id::KIND_18, serializer::INT, |o| {
                            write_varint(o, mob.boost_total);
                        });
                    }
                    variant(out, data_id::KIND_19, serializer::PIG_VARIANT, mob.variant);
                }
                Kind::Cow => variant(out, data_id::KIND_18, serializer::COW_VARIANT, mob.variant),
                _ => variant(
                    out,
                    data_id::KIND_18,
                    serializer::CHICKEN_VARIANT,
                    mob.variant,
                ),
            }
        }
        _ => {}
    }
}

/// `set_equipment` with every non-empty equipment slot of a player (pairing data).
fn equipment(entity: &EntityRef<'_>, entities: &Entities) -> Option<SetEquipment> {
    if let Some(mob) = entities.mob(entity.id()) {
        let mut slots = crate::mob::equipment::visible(mob);
        if mob.saddled {
            let saddle = vanillite_data::registry::ITEM.id("minecraft:saddle")?;
            slots.push((EquipmentSlot::Saddle.ordinal(), ItemStack::new(saddle, 1)));
        }
        return (!slots.is_empty()).then(|| SetEquipment {
            entity_id: entity.network_id().0,
            slots,
        });
    }
    let player = entities.player(entity.id())?;
    let slots: Vec<(u8, ItemStack)> = EQUIPMENT_SLOTS
        .iter()
        .map(|&slot| (slot.ordinal(), player.inventory.equipment(slot).clone()))
        .filter(|(_, stack)| !stack.is_empty())
        .collect();
    (!slots.is_empty()).then(|| SetEquipment {
        entity_id: entity.network_id().0,
        slots,
    })
}

/// The equipment slots of a player in Vanilla order.
pub const EQUIPMENT_SLOTS: [EquipmentSlot; 6] = [
    EquipmentSlot::MainHand,
    EquipmentSlot::OffHand,
    EquipmentSlot::Feet,
    EquipmentSlot::Legs,
    EquipmentSlot::Chest,
    EquipmentSlot::Head,
];

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
                // `getAddEntityPacket` data: the block state of a falling block, the owner
                // of a projectile.
                data: entities
                    .falling_block(message.entity)
                    .map(|f| i32::from(f.state.0))
                    .or_else(|| {
                        let owner = entities.arrow(message.entity)?.owner?;
                        Some(entities.store.get(owner)?.network_id().0)
                    })
                    .or_else(|| {
                        let owner = entities.missile(message.entity)?.owner?;
                        Some(entities.store.get(owner)?.network_id().0)
                    })
                    .unwrap_or(0),
            });
            let entity = entities.store.get(message.entity);
            let data = entity
                .as_ref()
                .and_then(|e| entity_data(e, entities, false));
            let gear = entity.as_ref().and_then(|e| equipment(e, entities));
            // Passengers and the lead (`sendPairingData`).
            let passengers = crate::riding::passengers_packet(entities, message.entity);
            let riding = entities
                .vehicle_of(message.entity)
                .and_then(|v| crate::riding::passengers_packet(entities, v));
            let link = crate::mob::tame::link_pairing_packet(entities, message.entity);
            if data.is_none()
                && gear.is_none()
                && passengers.is_none()
                && riding.is_none()
                && link.is_none()
            {
                return Some(vec![add]);
            }
            // `sendPairingData` bundles the spawn with its data and equipment.
            let mut packets = vec![SharedPacket::of(&BundleDelimiter), add];
            if let Some(items) = data {
                packets.push(SharedPacket::of(&SetEntityData {
                    entity_id: id,
                    items: RawBytes(items),
                }));
            }
            if let Some(gear) = gear {
                packets.push(SharedPacket::of(&gear));
            }
            for p in [passengers, riding].into_iter().flatten() {
                packets.push(SharedPacket::of(&p));
            }
            if let Some(link) = link {
                packets.push(SharedPacket::of(&link));
            }
            packets.push(SharedPacket::of(&BundleDelimiter));
            Some(packets)
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
