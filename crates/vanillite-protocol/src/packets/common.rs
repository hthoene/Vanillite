//! Types and packets shared by the configuration and play states. The packets exist once per
//! state with their own ids; [`common_serverbound!`](crate::common_serverbound) and
//! [`common_clientbound!`](crate::common_clientbound) define them in each state's module.

use crate::codec::{Decode, DecodeLimited, Encode, Reader, Result};
use crate::types::{Identifier, VarIntList};
use crate::varint_enum;

varint_enum! {
    /// Chat visibility setting of the client.
    pub enum ChatVisibility { Full, System, Hidden }
}

varint_enum! {
    /// Main hand.
    pub enum MainHand { Left, Right }
}

varint_enum! {
    /// Particle setting of the client.
    pub enum ParticleStatus { All, Decreased, Minimal }
}

varint_enum! {
    /// Result of a resource pack request.
    pub enum ResourcePackAction {
        SuccessfullyLoaded,
        Declined,
        FailedDownload,
        Accepted,
        Downloaded,
        InvalidUrl,
        FailedReload,
        Discarded,
    }
}

/// Client settings (`ClientInformation`). The view distance is raw client input; the server
/// clamps it (SEC-19).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientInfo {
    pub language: String,
    pub view_distance: i8,
    pub chat_visibility: ChatVisibility,
    pub chat_colors: bool,
    pub model_customisation: u8,
    pub main_hand: MainHand,
    pub text_filtering: bool,
    pub allows_listing: bool,
    pub particle_status: ParticleStatus,
}

impl Default for ClientInfo {
    /// Vanilla's `ClientInformation.createDefault()`.
    fn default() -> Self {
        Self {
            language: "en_us".into(),
            view_distance: 2,
            chat_visibility: ChatVisibility::Full,
            chat_colors: true,
            model_customisation: 0,
            main_hand: MainHand::Right,
            text_filtering: false,
            allows_listing: false,
            particle_status: ParticleStatus::All,
        }
    }
}

impl Encode for ClientInfo {
    fn encode(&self, out: &mut Vec<u8>) {
        self.language.encode(out);
        self.view_distance.encode(out);
        self.chat_visibility.encode(out);
        self.chat_colors.encode(out);
        self.model_customisation.encode(out);
        self.main_hand.encode(out);
        self.text_filtering.encode(out);
        self.allows_listing.encode(out);
        self.particle_status.encode(out);
    }
}

impl Decode for ClientInfo {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        Ok(Self {
            language: String::decode_limited(r, 16)?,
            view_distance: r.i8()?,
            chat_visibility: ChatVisibility::decode(r)?,
            chat_colors: bool::decode(r)?,
            model_customisation: r.u8()?,
            main_hand: MainHand::decode(r)?,
            text_filtering: bool::decode(r)?,
            allows_listing: bool::decode(r)?,
            particle_status: ParticleStatus::decode(r)?,
        })
    }
}

/// The tags of one registry: tag name to entry protocol ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryTags {
    pub registry: Identifier,
    pub tags: Vec<TagEntries>,
}

/// One tag and its entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagEntries {
    pub name: Identifier,
    pub entries: VarIntList,
}

impl Encode for TagEntries {
    fn encode(&self, out: &mut Vec<u8>) {
        self.name.encode(out);
        self.entries.encode(out);
    }
}

impl Decode for TagEntries {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        Ok(Self {
            name: Identifier::decode(r)?,
            entries: VarIntList::decode_limited(r, 1 << 20)?,
        })
    }
}

impl Encode for RegistryTags {
    fn encode(&self, out: &mut Vec<u8>) {
        self.registry.encode(out);
        self.tags.encode(out);
    }
}

impl Decode for RegistryTags {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        Ok(Self {
            registry: Identifier::decode(r)?,
            tags: Vec::decode_limited(r, 1 << 16)?,
        })
    }
}

/// Defines the serverbound packets that configuration and play share, for one state.
#[macro_export]
#[doc(hidden)]
macro_rules! common_serverbound {
    ($state:ident) => {
        /// `client_information`.
        #[derive(Debug, Clone, PartialEq, Eq, $crate::Packet)]
        #[packet(state = $state, dir = Serverbound, name = "client_information")]
        pub struct ClientInformation(pub $crate::packets::common::ClientInfo);

        /// `cookie_response`.
        #[derive(Debug, Clone, PartialEq, Eq, $crate::Packet)]
        #[packet(state = $state, dir = Serverbound, name = "cookie_response", bucket = "payload")]
        pub struct CookieResponse {
            pub key: $crate::types::Identifier,
            #[field(max_len = 5120)]
            pub payload: Option<$crate::types::RawBytes>,
        }

        /// `custom_payload` (plugin message); Vanilla's limit is 32767 bytes.
        #[derive(Debug, Clone, PartialEq, Eq, $crate::Packet)]
        #[packet(state = $state, dir = Serverbound, name = "custom_payload", bucket = "payload")]
        pub struct CustomPayload {
            pub channel: $crate::types::Identifier,
            #[field(rest, max_len = 32767)]
            pub data: $crate::types::RawBytes,
        }

        /// `keep_alive` answer.
        #[derive(Debug, Clone, PartialEq, Eq, $crate::Packet)]
        #[packet(state = $state, dir = Serverbound, name = "keep_alive", bucket = "keep_alive")]
        pub struct KeepAlive {
            pub id: i64,
        }

        /// `pong` answer to a `ping`.
        #[derive(Debug, Clone, PartialEq, Eq, $crate::Packet)]
        #[packet(state = $state, dir = Serverbound, name = "pong", bucket = "keep_alive")]
        pub struct Pong {
            pub id: i32,
        }

        /// `resource_pack` status.
        #[derive(Debug, Clone, PartialEq, Eq, $crate::Packet)]
        #[packet(state = $state, dir = Serverbound, name = "resource_pack")]
        pub struct ResourcePack {
            pub id: $crate::types::Uuid,
            pub action: $crate::packets::common::ResourcePackAction,
        }

        /// `custom_click_action` from a dialog: kept as the length-prefixed raw payload.
        #[derive(Debug, Clone, PartialEq, Eq, $crate::Packet)]
        #[packet(state = $state, dir = Serverbound, name = "custom_click_action", bucket = "payload")]
        pub struct CustomClickAction {
            pub id: $crate::types::Identifier,
            #[field(max_len = 65536)]
            pub payload: $crate::types::RawBytes,
        }
    };
}

/// Defines the clientbound packets that configuration and play share, for one state.
#[macro_export]
#[doc(hidden)]
macro_rules! common_clientbound {
    ($state:ident) => {
        /// `cookie_request`.
        #[derive(Debug, Clone, PartialEq, Eq, $crate::Packet)]
        #[packet(state = $state, dir = Clientbound, name = "cookie_request")]
        pub struct CookieRequest {
            pub key: $crate::types::Identifier,
        }

        /// `custom_payload` (plugin message, e.g. `minecraft:brand`).
        #[derive(Debug, Clone, PartialEq, Eq, $crate::Packet)]
        #[packet(state = $state, dir = Clientbound, name = "custom_payload")]
        pub struct CustomPayload {
            pub channel: $crate::types::Identifier,
            #[field(rest, max_len = 1_048_576)]
            pub data: $crate::types::RawBytes,
        }

        /// `disconnect` with a reason.
        #[derive(Debug, Clone, PartialEq, Eq, $crate::Packet)]
        #[packet(state = $state, dir = Clientbound, name = "disconnect")]
        pub struct Disconnect {
            #[field(max_len = 2_097_152)]
            pub reason: $crate::text::Component,
        }

        /// `keep_alive`.
        #[derive(Debug, Clone, PartialEq, Eq, $crate::Packet)]
        #[packet(state = $state, dir = Clientbound, name = "keep_alive")]
        pub struct KeepAlive {
            pub id: i64,
        }

        /// `ping`.
        #[derive(Debug, Clone, PartialEq, Eq, $crate::Packet)]
        #[packet(state = $state, dir = Clientbound, name = "ping")]
        pub struct Ping {
            pub id: i32,
        }

        /// `post_effects`: the post-processing shaders of the player (`/posteffect`).
        #[derive(Debug, Clone, PartialEq, Eq, $crate::Packet)]
        #[packet(state = $state, dir = Clientbound, name = "post_effects")]
        pub struct PostEffects {
            #[field(max_len = 1024)]
            pub effects: Vec<$crate::types::Identifier>,
        }

        /// `store_cookie`.
        #[derive(Debug, Clone, PartialEq, Eq, $crate::Packet)]
        #[packet(state = $state, dir = Clientbound, name = "store_cookie")]
        pub struct StoreCookie {
            pub key: $crate::types::Identifier,
            #[field(max_len = 5120)]
            pub payload: $crate::types::RawBytes,
        }

        /// `transfer` to another server.
        #[derive(Debug, Clone, PartialEq, Eq, $crate::Packet)]
        #[packet(state = $state, dir = Clientbound, name = "transfer")]
        pub struct Transfer {
            #[field(max_len = 32767)]
            pub host: String,
            #[field(varint)]
            pub port: i32,
        }

        /// `update_tags`: tags of every networked registry.
        #[derive(Debug, Clone, PartialEq, Eq, $crate::Packet)]
        #[packet(state = $state, dir = Clientbound, name = "update_tags")]
        pub struct UpdateTags {
            #[field(max_len = 1024)]
            pub registries: Vec<$crate::packets::common::RegistryTags>,
        }
    };
}
