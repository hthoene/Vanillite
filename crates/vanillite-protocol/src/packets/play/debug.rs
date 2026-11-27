//! Play packets of the debug subscriptions (operators' F3 charts and the developer debug
//! renderers), chat completions and deletion, projectile power, the experimental minecart
//! steps and the low disk space warning.

use crate::Packet;
use crate::chat::PackedSignature;
use crate::codec::{Decode, Encode, Reader, Result, decode_list_limited};
use crate::types::{BlockPos, RawBytes, VarIntList, Vec3};
use crate::varint_enum;

/// `ByteBufCodecs.STRING_UTF8`.
const MAX_STRING: usize = 32767;
/// Entries of a chat completion packet (`ByteBufCodecs.list()` has no limit of its own).
const MAX_COMPLETIONS: usize = 1 << 16;
/// The raw value of a debug value packet (decoded only by the test client).
const MAX_DEBUG_VALUE: usize = 2_097_152;

/// `debug_subscription` registry ids (`DebugSubscriptions`, in registration order).
pub mod subscription {
    /// `dedicated_server_tick_time`: the samples of the F3 tick time chart.
    pub const DEDICATED_SERVER_TICK_TIME: i32 = 0;
    /// Number of registered subscriptions.
    pub const COUNT: i32 = 16;
}

/// `ServerboundDebugSubscriptionRequestPacket`: the `debug_subscription` registry ids the client
/// wants (at most 32). Only operators get what they asked for.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Serverbound, name = "debug_subscription_request")]
pub struct DebugSubscriptionRequest {
    #[field(max_len = 32)]
    pub subscriptions: VarIntList,
}

/// `debug/block_value`: a `DebugSubscription.Update` of a block. `value` is the raw
/// `Optional<T>` (presence flag and value) of the subscription's value codec.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "debug/block_value")]
pub struct DebugBlockValue {
    pub pos: BlockPos,
    #[field(varint)]
    pub subscription: i32,
    #[field(rest, max_len = MAX_DEBUG_VALUE)]
    pub value: RawBytes,
}

/// `debug/chunk_value`: a `DebugSubscription.Update` of a chunk (`ChunkPos.asLong`).
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "debug/chunk_value")]
pub struct DebugChunkValue {
    pub chunk: i64,
    #[field(varint)]
    pub subscription: i32,
    #[field(rest, max_len = MAX_DEBUG_VALUE)]
    pub value: RawBytes,
}

/// `debug/entity_value`: a `DebugSubscription.Update` of an entity.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "debug/entity_value")]
pub struct DebugEntityValue {
    #[field(varint)]
    pub entity_id: i32,
    #[field(varint)]
    pub subscription: i32,
    #[field(rest, max_len = MAX_DEBUG_VALUE)]
    pub value: RawBytes,
}

/// `debug/event`: a `DebugSubscription.Event` (the value without a presence flag).
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "debug/event")]
pub struct DebugEvent {
    #[field(varint)]
    pub subscription: i32,
    #[field(rest, max_len = MAX_DEBUG_VALUE)]
    pub value: RawBytes,
}

varint_enum! {
    /// `RemoteDebugSampleType`.
    pub enum DebugSampleType { TickTime }
}

/// `debug_sample`: one sample of a remote debug chart. For [`DebugSampleType::TickTime`] the
/// `TpsDebugDimensions` in nanoseconds: full tick, `tickServer`, scheduled tasks, idle.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "debug_sample")]
pub struct DebugSample {
    #[field(max_len = 64)]
    pub sample: Vec<i64>,
    pub kind: DebugSampleType,
}

varint_enum! {
    /// `ClientboundCustomChatCompletionsPacket.Action`.
    pub enum ChatCompletionsAction { Add, Remove, Set }
}

/// `custom_chat_completions`: extra chat suggestions (plugins; Vanilla never sends it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomChatCompletions {
    pub action: ChatCompletionsAction,
    pub entries: Vec<String>,
}

impl Encode for CustomChatCompletions {
    fn encode(&self, out: &mut Vec<u8>) {
        self.action.encode(out);
        self.entries.encode(out);
    }
}

impl Decode for CustomChatCompletions {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        Ok(Self {
            action: ChatCompletionsAction::decode(r)?,
            entries: decode_list_limited(r, MAX_COMPLETIONS, MAX_STRING)?,
        })
    }
}

impl Packet for CustomChatCompletions {
    const ID: i32 = crate::ids::play::clientbound::CUSTOM_CHAT_COMPLETIONS;
    const NAME: &'static str = "custom_chat_completions";
    const STATE: crate::State = crate::State::Play;
    const DIRECTION: crate::Direction = crate::Direction::Clientbound;
    const COST: u16 = 1;
    const BUCKET: crate::Bucket = crate::Bucket::Default;
}

/// `delete_chat`: hides a signed chat message (plugins/moderation; Vanilla never sends it).
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "delete_chat")]
pub struct DeleteChat {
    pub signature: PackedSignature,
}

/// `low_disk_space_warning` (integrated server only).
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "low_disk_space_warning")]
pub struct LowDiskSpaceWarning;

/// `projectile_power`: `AbstractHurtingProjectile.accelerationPower`, bundled with every
/// movement-tracking `set_entity_motion` of fireballs, wither skulls and wind charges.
#[derive(Debug, Clone, PartialEq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "projectile_power")]
pub struct ProjectilePower {
    #[field(varint)]
    pub entity_id: i32,
    pub power: f64,
}

/// `NewMinecartBehavior.MinecartStep`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MinecartStep {
    pub position: Vec3,
    pub movement: Vec3,
    /// Packed degrees (`ByteBufCodecs.ROTATION_BYTE`).
    pub y_rot: i8,
    pub x_rot: i8,
    pub weight: f32,
}

impl Encode for MinecartStep {
    fn encode(&self, out: &mut Vec<u8>) {
        self.position.encode(out);
        self.movement.encode(out);
        self.y_rot.encode(out);
        self.x_rot.encode(out);
        self.weight.encode(out);
    }
}

impl Decode for MinecartStep {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        Ok(Self {
            position: Vec3::decode(r)?,
            movement: Vec3::decode(r)?,
            y_rot: r.i8()?,
            x_rot: r.i8()?,
            weight: r.f32()?,
        })
    }
}

/// `move_minecart_along_track`: the interpolation steps of a minecart with the experimental
/// `minecart_improvements` behaviour.
#[derive(Debug, Clone, PartialEq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "move_minecart_along_track")]
pub struct MoveMinecartAlongTrack {
    #[field(varint)]
    pub entity_id: i32,
    #[field(max_len = 1 << 16)]
    pub steps: Vec<MinecartStep>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn round_trip<P: Encode + Decode + PartialEq + std::fmt::Debug>(p: &P, bytes: &[u8]) {
        let mut out = Vec::new();
        p.encode(&mut out);
        assert_eq!(out, bytes);
        let mut r = Reader::new(bytes);
        assert_eq!(&P::decode(&mut r).unwrap(), p);
        assert_eq!(r.remaining(), 0);
    }

    #[test]
    fn debug_sample_is_a_long_array_and_the_type() {
        let p = DebugSample {
            sample: vec![1, 2, 3, 4],
            kind: DebugSampleType::TickTime,
        };
        let mut bytes = vec![4];
        for v in 1i64..=4 {
            bytes.extend_from_slice(&v.to_be_bytes());
        }
        bytes.push(0);
        round_trip(&p, &bytes);
    }

    #[test]
    fn projectile_power_is_varint_and_double() {
        let p = ProjectilePower {
            entity_id: 300,
            power: 0.1,
        };
        let mut bytes = vec![0xAC, 0x02];
        bytes.extend_from_slice(&0.1f64.to_be_bytes());
        round_trip(&p, &bytes);
    }

    #[test]
    fn chat_completions_and_delete_chat() {
        let p = CustomChatCompletions {
            action: ChatCompletionsAction::Set,
            entries: vec!["a".into(), "bc".into()],
        };
        round_trip(&p, &[2, 2, 1, b'a', 2, b'b', b'c']);
        round_trip(
            &DeleteChat {
                signature: PackedSignature::Id(5),
            },
            &[6],
        );
    }
}
