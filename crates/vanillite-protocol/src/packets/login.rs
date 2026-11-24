//! Login state.

use crate::Packet;
use crate::text::JsonComponent;
use crate::types::{GameProfile, Identifier, RawBytes, Uuid};

/// `hello` (serverbound): the name the player logs in with and their profile id.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Login, dir = Serverbound, name = "hello")]
pub struct Hello {
    #[field(max_len = 16)]
    pub name: String,
    pub profile_id: Uuid,
}

/// `key`: the RSA-encrypted shared secret and challenge (128 bytes each for a 1024-bit key).
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Login, dir = Serverbound, name = "key")]
pub struct Key {
    #[field(max_len = 512)]
    pub shared_secret: RawBytes,
    #[field(max_len = 512)]
    pub challenge: RawBytes,
}

/// `custom_query_answer`: answer to a login plugin request (proxy forwarding).
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Login, dir = Serverbound, name = "custom_query_answer")]
pub struct CustomQueryAnswer {
    #[field(varint)]
    pub transaction_id: i32,
    #[field(rest, max_len = 1_048_576)]
    pub payload: Option<RawBytes>,
}

/// `login_acknowledged`: the client switches to configuration.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Login, dir = Serverbound, name = "login_acknowledged")]
pub struct LoginAcknowledged;

/// `cookie_response` in login.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Login, dir = Serverbound, name = "cookie_response")]
pub struct CookieResponse {
    pub key: Identifier,
    #[field(max_len = 5120)]
    pub payload: Option<RawBytes>,
}

/// `login_disconnect`: reason as a JSON component.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Login, dir = Clientbound, name = "login_disconnect")]
pub struct LoginDisconnect {
    #[field(max_len = 262_144)]
    pub reason: JsonComponent,
}

/// `hello` (clientbound): encryption request.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Login, dir = Clientbound, name = "hello")]
pub struct EncryptionRequest {
    #[field(max_len = 20)]
    pub server_id: String,
    #[field(max_len = 512)]
    pub public_key: RawBytes,
    #[field(max_len = 512)]
    pub challenge: RawBytes,
    pub should_authenticate: bool,
}

/// `login_finished`: the authenticated profile and the server's session id.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Login, dir = Clientbound, name = "login_finished")]
pub struct LoginFinished {
    pub profile: GameProfile,
    pub session_id: Uuid,
}

/// `login_compression`: compression threshold for everything after this packet.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Login, dir = Clientbound, name = "login_compression")]
pub struct LoginCompression {
    #[field(varint)]
    pub threshold: i32,
}

/// `custom_query`: login plugin request.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Login, dir = Clientbound, name = "custom_query")]
pub struct CustomQuery {
    #[field(varint)]
    pub transaction_id: i32,
    pub channel: Identifier,
    #[field(rest, max_len = 1_048_576)]
    pub payload: RawBytes,
}

/// `cookie_request` in login.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Login, dir = Clientbound, name = "cookie_request")]
pub struct CookieRequest {
    pub key: Identifier,
}
