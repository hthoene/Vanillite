//! Pure Minecraft Java protocol library without I/O: primitives, framing, codecs, packet sets
//! and text components, plus a blocking test client.
//!
//! - [`codec`]: bounded reader, VarInt, strings, lists; [`types`]: identifiers, UUIDs, NBT,
//!   profiles.
//! - [`frame`]: length prefix, compression and encryption for both directions.
//! - [`packets`]: packet structs per state, defined with `#[derive(Packet)]`; [`sets`]: the
//!   per-state decoders that dispatch on packet ids.
//! - [`text`]: text components; [`client`]: a test client over any `Read + Write`.

// Lets `#[derive(Packet)]` refer to this crate by name from inside it.
extern crate self as vanillite_protocol;

pub mod chat;
pub mod cipher;
pub mod client;
pub mod codec;
pub mod commands;
pub mod components;
pub mod components_nbt;
pub mod compress;
pub mod frame;
pub mod ids;
pub mod item;
pub mod packets;
pub mod recipe_display;
pub mod sets;
pub mod shared;
pub mod text;
pub mod types;

pub use codec::{Decode, DecodeLimited, Encode, Error, Reader, Result};
pub use shared::{Inbound, SharedPacket};
pub use vanillite_data::version::{PROTOCOL_VERSION, VERSION_ID};
pub use vanillite_macros::Packet;

/// Connection state; each has its own packet set and limits (SEC-05).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum State {
    Handshake,
    Status,
    Login,
    Configuration,
    Play,
}

impl State {
    /// Frame limits of this state.
    pub const fn limits(self) -> frame::Limits {
        match self {
            Self::Handshake => frame::Limits::HANDSHAKE,
            Self::Status => frame::Limits::STATUS,
            Self::Login => frame::Limits::LOGIN,
            Self::Configuration => frame::Limits::CONFIGURATION,
            Self::Play => frame::Limits::PLAY,
        }
    }
}

/// Packet direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    Serverbound,
    Clientbound,
}

/// Rate-limit bucket of a serverbound packet type (SEC-16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Bucket {
    Default,
    KeepAlive,
    Movement,
    Chat,
    Command,
    Interact,
    Inventory,
    Payload,
}

impl Bucket {
    /// Number of buckets.
    pub const COUNT: usize = 8;

    /// Dense index for per-connection bucket arrays.
    pub const fn index(self) -> usize {
        match self {
            Self::Default => 0,
            Self::KeepAlive => 1,
            Self::Movement => 2,
            Self::Chat => 3,
            Self::Command => 4,
            Self::Interact => 5,
            Self::Inventory => 6,
            Self::Payload => 7,
        }
    }
}

/// A packet type with its generated metadata.
pub trait Packet: Encode + Decode {
    /// Packet id in its state and direction (from the generated table).
    const ID: i32;
    /// Vanilla name without namespace.
    const NAME: &'static str;
    /// State the packet belongs to.
    const STATE: State;
    /// Direction.
    const DIRECTION: Direction;
    /// Rate-limit cost.
    const COST: u16;
    /// Rate-limit bucket.
    const BUCKET: Bucket;
}
