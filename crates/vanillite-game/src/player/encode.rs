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
    pub const EFFECT_PARTICLES: u8 = 10;
    pub const EFFECT_AMBIENCE: u8 = 11;
    pub const PLAYER_MAIN_HAND: u8 = 15;
    pub const TNT_FUSE: u8 = 8;
    pub const TNT_BLOCK_STATE: u8 = 9;
    pub const EXPERIENCE_ORB_VALUE: u8 = 8;
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
mod serializer {
    pub const BYTE: i32 = 0;
    pub const ITEM_STACK: i32 = 7;
    pub const INT: i32 = 1;
    pub const STRING: i32 = 4;
    pub const COMPONENT: i32 = 5;
    pub const OPTIONAL_COMPONENT: i32 = 6;
    pub const FLOAT: i32 = 3;
    pub const BOOLEAN: i32 = 8;
    pub const PARTICLE: i32 = 16;
    pub const PARTICLES: i32 = 17;
    pub const BLOCK_STATE: i32 = 14;
    pub const OPTIONAL_BLOCK_STATE: i32 = 15;
    pub const POSE: i32 = 20;
    pub const COW_VARIANT: i32 = 23;
    pub const PIG_VARIANT: i32 = 28;
    pub const CHICKEN_VARIANT: i32 = 30;
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

/// The synched data of a minecart (`VehicleEntity`, `AbstractMinecart` and the kinds).
fn minecart_data(out: &mut Vec<u8>, m: &crate::minecart::MinecartData, all: bool) {
    use crate::minecart::MinecartKind;
    if all || m.hurt_time != 0 {
        item(out, 8, serializer::INT, |o| write_varint(o, m.hurt_time));
    }
    if all || m.hurt_dir != 1 {
        item(out, 9, serializer::INT, |o| write_varint(o, m.hurt_dir));
    }
    if all || m.damage != 0.0 {
        item(out, 10, serializer::FLOAT, |o| {
            o.extend_from_slice(&m.damage.to_be_bytes());
        });
    }
    if all || m.display_state.is_some() {
        item(out, 11, serializer::OPTIONAL_BLOCK_STATE, |o| {
            write_varint(o, m.display_state.map_or(0, |s| i32::from(s.0)));
        });
    }
    if all || m.display_offset != m.kind.default_display_offset() {
        item(out, 12, serializer::INT, |o| {
            write_varint(o, m.display_offset)
        });
    }
    match m.kind {
        MinecartKind::Furnace => {
            if all || m.has_fuel {
                item(out, 13, serializer::BOOLEAN, |o| {
                    o.push(u8::from(m.has_fuel))
                });
            }
        }
        MinecartKind::CommandBlock => {
            if all || !m.command.is_empty() {
                item(out, 13, serializer::STRING, |o| {
                    write_varint(o, i32::try_from(m.command.len()).unwrap_or(0));
                    o.extend_from_slice(m.command.as_bytes());
                });
            }
            let output = m
                .extra
                .get("LastOutput")
                .and_then(crate::mob::nbt::tag_component);
            if all || output.is_some() {
                item(out, 14, serializer::COMPONENT, |o| match &output {
                    Some(c) => o.extend_from_slice(c),
                    // `CommonComponents.EMPTY`: an empty literal.
                    None => {
                        let empty = vanillite_nbt::Tag::String(String::new());
                        let _ = vanillite_nbt::write::write_network(&empty, o);
                    }
                });
            }
        }
        _ => {}
    }
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
        })
        .or_else(|| {
            entities
                .minecart(entity.id())
                .and_then(|m| m.custom_name.as_deref())
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
        effect_data(&mut out, &player.survival.effects, all);
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
    if let Some(special) = entities.special(entity.id()) {
        crate::mob::boss::special::entity_data(&mut out, special, all);
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
    if let Some(m) = entities.minecart(entity.id()) {
        minecart_data(&mut out, m, all);
    }
    if let Some(orb) = entities.experience_orb(entity.id())
        && (all || orb.value != 0)
    {
        item(&mut out, data_id::ORB_VALUE, serializer::INT, |o| {
            write_varint(o, orb.value);
        });
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
        // `Arrow.ID_EFFECT_COLOR` (11); after 600 ticks in the ground it goes back to -1.
        if entity.kind() == ids::ARROW {
            let color = arrow.color();
            if all || color != -1 || arrow.in_ground_time >= 600 {
                item(&mut out, 11, serializer::INT, |o| write_varint(o, color));
            }
        }
    }
    if let Some(cloud) = entities.cloud(entity.id()) {
        if all || cloud.radius != 3.0 {
            item(&mut out, 8, serializer::FLOAT, |o| {
                o.extend_from_slice(&cloud.radius.to_be_bytes());
            });
        }
        if all || cloud.waiting {
            item(&mut out, 9, serializer::BOOLEAN, |o| {
                o.push(u8::from(cloud.waiting))
            });
        }
        let particle_id = |name: &str| {
            vanillite_data::registry::PARTICLE_TYPE
                .id(name)
                .map_or(0, i32::from)
        };
        // `PowerParticleOption` of `dragon_breath`, else `ColorParticleOption` of
        // `entity_effect`; the default is white.
        let color = cloud.particle_color();
        if cloud.dragon_breath {
            item(&mut out, 10, serializer::PARTICLE, |o| {
                write_varint(o, particle_id("minecraft:dragon_breath"));
                o.extend_from_slice(&1.0_f32.to_be_bytes());
            });
        } else if all || color != -1 {
            item(&mut out, 10, serializer::PARTICLE, |o| {
                write_varint(o, particle_id("minecraft:entity_effect"));
                o.extend_from_slice(&color.to_be_bytes());
            });
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
    if entity.kind() == ids::EXPERIENCE_ORB
        && let Some(orb) = entities.experience_orb(entity.id())
        && (all || orb.value != 0)
    {
        item(
            &mut out,
            data_id::EXPERIENCE_ORB_VALUE,
            serializer::INT,
            |o| {
                write_varint(o, orb.value);
            },
        );
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

/// `DATA_EFFECT_PARTICLES` and `DATA_EFFECT_AMBIENCE_ID` of a living entity.
fn effect_data(out: &mut Vec<u8>, fx: &crate::effect::ActiveEffects, all: bool) {
    let any_visible = fx.iter().any(|e| e.visible);
    if all || any_visible {
        item(out, data_id::EFFECT_PARTICLES, serializer::PARTICLES, |o| {
            fx.encode_particles(o);
        });
    }
    // `areAllEffectsAmbient` of no effects is true, but the data starts false and is only set
    // while effects are active.
    let ambient = !fx.is_empty() && fx.all_ambient();
    if all || ambient {
        item(out, data_id::EFFECT_AMBIENCE, serializer::BOOLEAN, |o| {
            o.push(u8::from(ambient));
        });
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
    effect_data(out, &mob.effects, all);
    let flags = u8::from(mob.no_ai) | (u8::from(mob.aggressive) << 2);
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
        Kind::Guardian | Kind::ElderGuardian | Kind::Wither | Kind::EnderDragon | Kind::Warden => {
            crate::mob::boss::entity_data(out, mob, all);
        }
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
    }
}

/// `set_equipment` with every non-empty equipment slot of a player (pairing data).
fn equipment(entity: &EntityRef<'_>, entities: &Entities) -> Option<SetEquipment> {
    if let Some(mob) = entities.mob(entity.id()) {
        let mut slots = Vec::new();
        // Skeletons hold their bow (`populateDefaultEquipmentSlots`).
        if mob.kind == crate::mob::Kind::Skeleton {
            let bow = vanillite_data::registry::ITEM.id("minecraft:bow")?;
            slots.push((EquipmentSlot::MainHand.ordinal(), ItemStack::new(bow, 1)));
        }
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
                        let special = entities.special(message.entity)?;
                        let owner = crate::mob::boss::special::spawn_owner(special)?;
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
