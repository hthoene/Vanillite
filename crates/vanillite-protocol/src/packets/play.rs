//! Play state: the subset milestone 1 needs. Packets not listed here are decoded as
//! [`Unhandled`](crate::sets::ServerboundPlay::Unhandled) on the server side.

/// Serverbound play packets.
mod world;

pub mod serverbound {
    pub use super::progress::{SeenAdvancements, SeenAdvancementsAction, SeenAdvancementsPacket};
    pub use super::world::serverbound::*;
    use crate::Packet;
    use crate::codec::{Decode, Encode, Reader, Result};
    use crate::varint_enum;

    crate::common_serverbound!(Play);

    pub use super::mobs::{Attack, Interact, MoveVehicle, PaddleBoat};

    /// `configuration_acknowledged`: the client entered configuration again.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Serverbound, name = "configuration_acknowledged")]
    pub struct ConfigurationAcknowledged;

    /// `accept_teleportation`: confirms a `player_position` with the position the client reached.
    #[derive(Debug, Clone, PartialEq, Packet)]
    #[packet(state = Play, dir = Serverbound, name = "accept_teleportation", bucket = "movement")]
    pub struct AcceptTeleportation {
        #[field(varint)]
        pub id: i32,
        #[field(finite)]
        pub x: f64,
        #[field(finite)]
        pub y: f64,
        #[field(finite)]
        pub z: f64,
        #[field(finite)]
        pub y_rot: f32,
        #[field(finite)]
        pub x_rot: f32,
    }

    /// Movement flags of the `move_player_*` packets.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct MoveFlags {
        pub on_ground: bool,
        pub horizontal_collision: bool,
    }

    impl Encode for MoveFlags {
        fn encode(&self, out: &mut Vec<u8>) {
            out.push(u8::from(self.on_ground) | (u8::from(self.horizontal_collision) << 1));
        }
    }

    impl Decode for MoveFlags {
        fn decode(r: &mut Reader<'_>) -> Result<Self> {
            let f = r.u8()?;
            Ok(Self {
                on_ground: f & 1 != 0,
                horizontal_collision: f & 2 != 0,
            })
        }
    }

    /// `move_player_pos`. Coordinates are checked for finiteness here; the movement handler
    /// applies Vanilla's range and speed checks.
    #[derive(Debug, Clone, PartialEq, Packet)]
    #[packet(state = Play, dir = Serverbound, name = "move_player_pos", bucket = "movement")]
    pub struct MovePlayerPos {
        #[field(finite)]
        pub x: f64,
        #[field(finite)]
        pub y: f64,
        #[field(finite)]
        pub z: f64,
        pub flags: MoveFlags,
    }

    /// `move_player_pos_rot`.
    #[derive(Debug, Clone, PartialEq, Packet)]
    #[packet(state = Play, dir = Serverbound, name = "move_player_pos_rot", bucket = "movement")]
    pub struct MovePlayerPosRot {
        #[field(finite)]
        pub x: f64,
        #[field(finite)]
        pub y: f64,
        #[field(finite)]
        pub z: f64,
        #[field(finite)]
        pub y_rot: f32,
        #[field(finite)]
        pub x_rot: f32,
        pub flags: MoveFlags,
    }

    /// `move_player_rot`.
    #[derive(Debug, Clone, PartialEq, Packet)]
    #[packet(state = Play, dir = Serverbound, name = "move_player_rot", bucket = "movement")]
    pub struct MovePlayerRot {
        #[field(finite)]
        pub y_rot: f32,
        #[field(finite)]
        pub x_rot: f32,
        pub flags: MoveFlags,
    }

    /// `move_player_status_only`.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Serverbound, name = "move_player_status_only", bucket = "movement")]
    pub struct MovePlayerStatusOnly {
        pub flags: MoveFlags,
    }

    /// `chunk_batch_received`: the client's desired chunks per tick.
    #[derive(Debug, Clone, PartialEq, Packet)]
    #[packet(state = Play, dir = Serverbound, name = "chunk_batch_received")]
    pub struct ChunkBatchReceived {
        pub desired_chunks_per_tick: f32,
    }

    /// `client_tick_end`.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Serverbound, name = "client_tick_end", bucket = "movement")]
    pub struct ClientTickEnd;

    /// `player_loaded`: the client finished loading the world after joining or respawning.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Serverbound, name = "player_loaded")]
    pub struct PlayerLoaded;

    /// Pressed movement keys (`Input`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Input {
        pub forward: bool,
        pub backward: bool,
        pub left: bool,
        pub right: bool,
        pub jump: bool,
        pub shift: bool,
        pub sprint: bool,
    }

    impl Encode for Input {
        fn encode(&self, out: &mut Vec<u8>) {
            let bits = [
                self.forward,
                self.backward,
                self.left,
                self.right,
                self.jump,
                self.shift,
                self.sprint,
            ];
            out.push(
                bits.iter()
                    .rev()
                    .fold(0u8, |acc, b| (acc << 1) | u8::from(*b)),
            );
        }
    }

    impl Decode for Input {
        fn decode(r: &mut Reader<'_>) -> Result<Self> {
            let f = r.u8()?;
            Ok(Self {
                forward: f & 1 != 0,
                backward: f & 2 != 0,
                left: f & 4 != 0,
                right: f & 8 != 0,
                jump: f & 16 != 0,
                shift: f & 32 != 0,
                sprint: f & 64 != 0,
            })
        }
    }

    /// `player_input`.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Serverbound, name = "player_input", bucket = "movement")]
    pub struct PlayerInput(pub Input);

    varint_enum! {
        /// Action of `player_command`.
        pub enum PlayerAction {
            StopSleeping,
            StartSprinting,
            StopSprinting,
            StartRidingJump,
            StopRidingJump,
            OpenInventory,
            StartFallFlying,
        }
    }

    /// `player_command`.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Serverbound, name = "player_command", bucket = "interact")]
    pub struct PlayerCommand {
        #[field(varint)]
        pub entity_id: i32,
        pub action: PlayerAction,
        #[field(varint)]
        pub data: i32,
    }

    /// `chat_command`: an unsigned command (max 32767 characters).
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Serverbound, name = "chat_command", bucket = "command")]
    pub struct ChatCommand {
        #[field(max_len = 32767)]
        pub command: String,
    }

    /// `chat_command_signed`: a command with signatures of its message arguments.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Serverbound, name = "chat_command_signed", bucket = "command")]
    pub struct ChatCommandSigned {
        #[field(max_len = 32767)]
        pub command: String,
        /// Epoch milliseconds.
        pub timestamp: i64,
        pub salt: i64,
        #[field(max_len = 8)]
        pub argument_signatures: Vec<crate::chat::ArgumentSignature>,
        pub last_seen: crate::chat::LastSeenUpdate,
    }

    /// `chat`: a chat message (max 256 characters), signed when the player has a chat session.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Serverbound, name = "chat", bucket = "chat")]
    pub struct Chat {
        #[field(max_len = 256)]
        pub message: String,
        /// Epoch milliseconds.
        pub timestamp: i64,
        pub salt: i64,
        pub signature: Option<crate::chat::MessageSignature>,
        pub last_seen: crate::chat::LastSeenUpdate,
    }

    /// `chat_ack`: the client advanced its last-seen window.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Serverbound, name = "chat_ack", bucket = "chat")]
    pub struct ChatAck {
        #[field(varint)]
        pub offset: i32,
    }

    /// `chat_session_update`: the client's chat session with its profile key.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Serverbound, name = "chat_session_update", bucket = "chat")]
    pub struct ChatSessionUpdate(pub crate::chat::ChatSessionData);

    varint_enum! {
        /// Action of `client_command`.
        pub enum ClientCommandAction {
            PerformRespawn,
            RequestStats,
            RequestGameRuleValues,
        }
    }

    /// `client_command`: respawn after death, or a statistics request.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Serverbound, name = "client_command")]
    pub struct ClientCommand {
        pub action: ClientCommandAction,
    }

    /// `command_suggestion`: tab completion of a command line (max 32500 characters).
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Serverbound, name = "command_suggestion", bucket = "command")]
    pub struct CommandSuggestion {
        #[field(varint)]
        pub id: i32,
        #[field(max_len = 32500)]
        pub command: String,
    }

    /// `ping_request`: the client's debug ping (F3 network chart), answered with
    /// `pong_response`.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Serverbound, name = "ping_request")]
    pub struct PingRequest {
        pub time: i64,
    }
}

/// Clientbound play packets.
pub mod clientbound {
    pub use super::world::clientbound::*;
    use crate::Packet;
    use crate::text::Component;
    use crate::types::VarIntList;

    crate::common_clientbound!(Play);

    /// `start_configuration`: sends the client back to configuration.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "start_configuration")]
    pub struct StartConfiguration;

    /// `pong_response`: echoes the payload of a `ping_request`.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "pong_response")]
    pub struct PongResponse {
        pub time: i64,
    }

    /// `bundle_delimiter`: packets between two delimiters are applied in one client tick.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "bundle_delimiter")]
    pub struct BundleDelimiter;

    /// `system_chat`.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "system_chat")]
    pub struct SystemChat {
        #[field(max_len = 2_097_152)]
        pub content: Component,
        pub overlay: bool,
    }

    /// `chunk_batch_start`.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "chunk_batch_start")]
    pub struct ChunkBatchStart;

    /// `player_chat`: a player's (possibly signed) chat message.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "player_chat")]
    pub struct PlayerChat {
        /// Messages this receiver got before.
        #[field(varint)]
        pub global_index: i32,
        pub sender: crate::types::Uuid,
        /// Index in the sender's message chain.
        #[field(varint)]
        pub index: i32,
        pub signature: Option<crate::chat::MessageSignature>,
        #[field(max_len = 256)]
        pub content: String,
        /// Epoch milliseconds.
        pub timestamp: i64,
        pub salt: i64,
        #[field(max_len = 20)]
        pub last_seen: Vec<crate::chat::PackedSignature>,
        #[field(max_len = 2_097_152)]
        pub unsigned_content: Option<Component>,
        pub filter_mask: crate::chat::FilterMask,
        pub chat_type: crate::chat::ChatTypeBound,
    }

    /// `disguised_chat`: an unsigned message shown with a chat type.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "disguised_chat")]
    pub struct DisguisedChat {
        #[field(max_len = 2_097_152)]
        pub message: Component,
        pub chat_type: crate::chat::ChatTypeBound,
    }

    /// `commands`: the command tree the player may use.
    #[derive(Debug, Clone, PartialEq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "commands")]
    pub struct Commands(pub crate::commands::CommandTree);

    /// One suggestion of `command_suggestions`.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Suggestion {
        pub text: String,
        pub tooltip: Option<Component>,
    }

    impl crate::codec::Encode for Suggestion {
        fn encode(&self, out: &mut Vec<u8>) {
            self.text.encode(out);
            self.tooltip.encode(out);
        }
    }

    impl crate::codec::Decode for Suggestion {
        fn decode(r: &mut crate::codec::Reader<'_>) -> crate::codec::Result<Self> {
            use crate::codec::DecodeLimited;
            Ok(Self {
                text: String::decode_limited(r, 32767)?,
                tooltip: Option::<Component>::decode_limited(r, 2_097_152)?,
            })
        }
    }

    /// `command_suggestions`: the answer to `command_suggestion`.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "command_suggestions")]
    pub struct CommandSuggestions {
        #[field(varint)]
        pub id: i32,
        /// Start of the replaced range (characters).
        #[field(varint)]
        pub start: i32,
        /// Length of the replaced range.
        #[field(varint)]
        pub length: i32,
        #[field(max_len = 1 << 16)]
        pub suggestions: Vec<Suggestion>,
    }

    /// `player_look_at`: turns the client's player towards a point or an entity.
    #[derive(Debug, Clone, PartialEq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "player_look_at")]
    pub struct PlayerLookAt {
        /// Anchor of the player (0 feet, 1 eyes).
        #[field(varint)]
        pub from_anchor: i32,
        pub x: f64,
        pub y: f64,
        pub z: f64,
        /// The entity to look at and its anchor, instead of the point.
        pub entity: Option<LookAtEntity>,
    }

    /// The target entity of `player_look_at`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct LookAtEntity {
        pub entity_id: i32,
        pub anchor: i32,
    }

    impl crate::codec::Encode for LookAtEntity {
        fn encode(&self, out: &mut Vec<u8>) {
            crate::codec::write_varint(out, self.entity_id);
            crate::codec::write_varint(out, self.anchor);
        }
    }

    impl crate::codec::Decode for LookAtEntity {
        fn decode(r: &mut crate::codec::Reader<'_>) -> crate::codec::Result<Self> {
            Ok(Self {
                entity_id: r.varint()?,
                anchor: r.varint()?,
            })
        }
    }

    /// `entity_event`: a one-byte event of an entity (here: permission levels 24..=28).
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "entity_event")]
    pub struct EntityEvent {
        pub entity_id: i32,
        pub event: i8,
    }

    /// `chunk_batch_finished`.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "chunk_batch_finished")]
    pub struct ChunkBatchFinished {
        #[field(varint)]
        pub batch_size: i32,
    }

    /// `set_chunk_cache_center`.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "set_chunk_cache_center")]
    pub struct SetChunkCacheCenter {
        #[field(varint)]
        pub x: i32,
        #[field(varint)]
        pub z: i32,
    }

    /// `set_chunk_cache_radius`.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "set_chunk_cache_radius")]
    pub struct SetChunkCacheRadius {
        #[field(varint)]
        pub radius: i32,
    }

    /// `set_simulation_distance`.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "set_simulation_distance")]
    pub struct SetSimulationDistance {
        #[field(varint)]
        pub distance: i32,
    }

    /// `forget_level_chunk`: chunk position packed with [`chunk_pos`].
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "forget_level_chunk")]
    pub struct ForgetLevelChunk {
        pub pos: i64,
    }

    /// Vanilla's `ChunkPos.asLong`: x in the low 32 bits, z in the high 32 bits.
    pub fn chunk_pos(x: i32, z: i32) -> i64 {
        (i64::from(z) << 32) | (i64::from(x) & 0xFFFF_FFFF)
    }

    /// `game_event`.
    #[derive(Debug, Clone, PartialEq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "game_event")]
    pub struct GameEvent {
        pub event: u8,
        pub param: f32,
    }

    impl GameEvent {
        /// Event id: the respawn block (bed, anchor) is missing or blocked.
        pub const NO_RESPAWN_BLOCK_AVAILABLE: u8 = 0;
        /// Event id: change game mode.
        pub const CHANGE_GAME_MODE: u8 = 3;
        /// Event id: the end credits (`WIN_GAME`).
        pub const WIN_GAME: u8 = 4;
        /// Event id: the client should wait for level chunks.
        pub const LEVEL_CHUNKS_LOAD_START: u8 = 13;
    }

    /// `remove_entities`.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "remove_entities")]
    pub struct RemoveEntities {
        #[field(max_len = 1 << 20)]
        pub ids: VarIntList,
    }

    /// `rotate_head`.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "rotate_head")]
    pub struct RotateHead {
        #[field(varint)]
        pub entity_id: i32,
        pub head_y_rot: i8,
    }

    pub use super::boss::*;
    pub use super::entities::*;
    pub use super::hud::*;
    pub use super::level::*;
    pub use super::mobs::{Animate, Cooldown, MoveVehicleBack, SetEntityLink, SetPassengers};
    pub use super::progress::{
        AdvancementDisplay, AdvancementEntry, AdvancementProgress, AwardStats,
        SelectAdvancementsTab, StatValue, UpdateAdvancements,
    };
    pub use super::survival::*;
}

mod boss;
mod hud;
mod level;
mod mobs;
mod progress;
mod survival;

/// Clientbound play packets with hand-written codecs (variable layouts).
mod entities {
    use crate::Packet;
    use crate::codec::{Decode, DecodeLimited, Encode, Error, Reader, Result, write_varint};
    use crate::text::Component;
    use crate::types::{
        GameProfile, GlobalPos, Identifier, LpVec3, Property, RawBytes, Uuid, Vec3,
    };

    /// Game mode (`GameType`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub enum GameMode {
        #[default]
        Survival,
        Creative,
        Adventure,
        Spectator,
    }

    impl GameMode {
        /// Wire id.
        pub const fn id(self) -> i32 {
            match self {
                Self::Survival => 0,
                Self::Creative => 1,
                Self::Adventure => 2,
                Self::Spectator => 3,
            }
        }

        fn from_id(id: i32) -> Result<Self> {
            match id {
                0 => Ok(Self::Survival),
                1 => Ok(Self::Creative),
                2 => Ok(Self::Adventure),
                3 => Ok(Self::Spectator),
                _ => Err(Error::Invalid("game mode")),
            }
        }
    }

    impl Encode for GameMode {
        fn encode(&self, out: &mut Vec<u8>) {
            write_varint(out, self.id());
        }
    }

    impl Decode for GameMode {
        fn decode(r: &mut Reader<'_>) -> Result<Self> {
            Self::from_id(r.varint()?)
        }
    }

    /// Spawn information shared by `login` and `respawn` (`CommonPlayerSpawnInfo`).
    #[derive(Debug, Clone, PartialEq)]
    pub struct SpawnInfo {
        /// Network id of the dimension type in its registry.
        pub dimension_type: i32,
        pub dimension: Identifier,
        pub hashed_seed: i64,
        pub game_mode: GameMode,
        pub previous_game_mode: Option<GameMode>,
        pub is_debug: bool,
        pub is_flat: bool,
        pub last_death_location: Option<GlobalPos>,
        pub portal_cooldown: i32,
        pub sea_level: i32,
    }

    impl Encode for SpawnInfo {
        fn encode(&self, out: &mut Vec<u8>) {
            write_varint(out, self.dimension_type);
            self.dimension.encode(out);
            self.hashed_seed.encode(out);
            self.game_mode.encode(out);
            write_varint(
                out,
                self.previous_game_mode
                    .map_or(0, |g| g.id().saturating_add(1)),
            );
            self.is_debug.encode(out);
            self.is_flat.encode(out);
            self.last_death_location.encode(out);
            write_varint(out, self.portal_cooldown);
            write_varint(out, self.sea_level);
        }
    }

    impl Decode for SpawnInfo {
        fn decode(r: &mut Reader<'_>) -> Result<Self> {
            Ok(Self {
                dimension_type: r.varint()?,
                dimension: Identifier::decode(r)?,
                hashed_seed: r.i64()?,
                game_mode: GameMode::decode(r)?,
                previous_game_mode: match r.varint()? {
                    0 => None,
                    id => Some(GameMode::from_id(id.saturating_sub(1))?),
                },
                is_debug: bool::decode(r)?,
                is_flat: bool::decode(r)?,
                last_death_location: Option::decode(r)?,
                portal_cooldown: r.varint()?,
                sea_level: r.varint()?,
            })
        }
    }

    /// `login` (play): the join packet.
    #[derive(Debug, Clone, PartialEq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "login")]
    pub struct Login {
        pub entity_id: i32,
        pub hardcore: bool,
        #[field(max_len = 1024)]
        pub levels: Vec<Identifier>,
        #[field(varint)]
        pub max_players: i32,
        #[field(varint)]
        pub chunk_radius: i32,
        #[field(varint)]
        pub simulation_distance: i32,
        pub reduced_debug_info: bool,
        pub show_death_screen: bool,
        pub do_limited_crafting: bool,
        pub spawn: SpawnInfo,
        pub online_mode: bool,
        pub enforces_secure_chat: bool,
    }

    /// Position, velocity and rotation (`PositionMoveRotation`).
    #[derive(Debug, Clone, Copy, PartialEq, Default)]
    pub struct PositionMoveRotation {
        pub position: Vec3,
        pub delta: Vec3,
        pub y_rot: f32,
        pub x_rot: f32,
    }

    impl Encode for PositionMoveRotation {
        fn encode(&self, out: &mut Vec<u8>) {
            self.position.encode(out);
            self.delta.encode(out);
            self.y_rot.encode(out);
            self.x_rot.encode(out);
        }
    }

    impl Decode for PositionMoveRotation {
        fn decode(r: &mut Reader<'_>) -> Result<Self> {
            Ok(Self {
                position: Vec3::decode(r)?,
                delta: Vec3::decode(r)?,
                y_rot: r.f32()?,
                x_rot: r.f32()?,
            })
        }
    }

    /// Bits of `Relative`: which components of a teleport are relative.
    pub mod relative {
        pub const X: i32 = 1;
        pub const Y: i32 = 1 << 1;
        pub const Z: i32 = 1 << 2;
        pub const Y_ROT: i32 = 1 << 3;
        pub const X_ROT: i32 = 1 << 4;
        pub const DELTA_X: i32 = 1 << 5;
        pub const DELTA_Y: i32 = 1 << 6;
        pub const DELTA_Z: i32 = 1 << 7;
        pub const ROTATE_DELTA: i32 = 1 << 8;
    }

    /// `player_position`: teleports the client's player; answered by `accept_teleportation`.
    #[derive(Debug, Clone, PartialEq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "player_position")]
    pub struct PlayerPosition {
        #[field(varint)]
        pub teleport_id: i32,
        pub change: PositionMoveRotation,
        /// [`relative`] bits.
        pub relatives: i32,
    }

    /// `teleport_entity`.
    #[derive(Debug, Clone, PartialEq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "teleport_entity")]
    pub struct TeleportEntity {
        #[field(varint)]
        pub entity_id: i32,
        pub change: PositionMoveRotation,
        pub relatives: i32,
        pub on_ground: bool,
    }

    /// `add_entity`.
    #[derive(Debug, Clone, PartialEq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "add_entity")]
    pub struct AddEntity {
        #[field(varint)]
        pub entity_id: i32,
        pub uuid: Uuid,
        /// Network id in the `entity_type` registry.
        #[field(varint)]
        pub entity_type: i32,
        pub position: Vec3,
        pub movement: LpVec3,
        pub x_rot: i8,
        pub y_rot: i8,
        pub y_head_rot: i8,
        #[field(varint)]
        pub data: i32,
    }

    /// Position change of `move_entity_pos(_rot)` (`VecDelta`): one delta in 1/4096 blocks, or
    /// several steps with their tick offsets.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum VecDelta {
        Linear([i16; 3]),
        Stepped(Vec<(i32, [i16; 3])>),
    }

    impl VecDelta {
        fn steps(&self) -> i32 {
            match self {
                Self::Linear(_) => 0,
                Self::Stepped(s) => i32::try_from(s.len()).unwrap_or(i32::MAX),
            }
        }

        fn encode(&self, out: &mut Vec<u8>) {
            match self {
                Self::Linear(d) => d.iter().for_each(|v| v.encode(out)),
                Self::Stepped(steps) => {
                    for (ticks, d) in steps {
                        write_varint(out, *ticks);
                        d.iter().for_each(|v| v.encode(out));
                    }
                }
            }
        }

        fn decode(r: &mut Reader<'_>, steps: i32) -> Result<Self> {
            let delta =
                |r: &mut Reader<'_>| -> Result<[i16; 3]> { Ok([r.i16()?, r.i16()?, r.i16()?]) };
            if steps <= 0 {
                return Ok(Self::Linear(delta(r)?));
            }
            let n = usize::try_from(steps).map_err(|_| Error::NegativeLength)?;
            if n > r.remaining() / 7 {
                return Err(Error::TooLong {
                    what: "VecDelta steps",
                    len: n,
                    max: r.remaining() / 7,
                });
            }
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                let ticks = r.varint()?;
                v.push((ticks, delta(r)?));
            }
            Ok(Self::Stepped(v))
        }

        fn properties(&self, on_ground: bool) -> i32 {
            i32::from(on_ground) | self.steps().wrapping_shl(1)
        }
    }

    fn split_properties(p: i32) -> (bool, i32) {
        (
            p & 1 != 0,
            i32::try_from(u32::from_ne_bytes(p.to_ne_bytes()).wrapping_shr(1)).unwrap_or(0),
        )
    }

    macro_rules! manual_packet {
        ($ty:ident, $id:ident, $name:literal) => {
            impl Packet for $ty {
                const ID: i32 = crate::ids::play::clientbound::$id;
                const NAME: &'static str = $name;
                const STATE: crate::State = crate::State::Play;
                const DIRECTION: crate::Direction = crate::Direction::Clientbound;
                const COST: u16 = 1;
                const BUCKET: crate::Bucket = crate::Bucket::Default;
            }
        };
    }

    /// `move_entity_pos`.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct MoveEntityPos {
        pub entity_id: i32,
        pub delta: VecDelta,
        pub on_ground: bool,
    }

    impl Encode for MoveEntityPos {
        fn encode(&self, out: &mut Vec<u8>) {
            write_varint(out, self.entity_id);
            write_varint(out, self.delta.properties(self.on_ground));
            self.delta.encode(out);
        }
    }

    impl Decode for MoveEntityPos {
        fn decode(r: &mut Reader<'_>) -> Result<Self> {
            let entity_id = r.varint()?;
            let (on_ground, steps) = split_properties(r.varint()?);
            Ok(Self {
                entity_id,
                delta: VecDelta::decode(r, steps)?,
                on_ground,
            })
        }
    }

    /// `move_entity_pos_rot`.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct MoveEntityPosRot {
        pub entity_id: i32,
        pub delta: VecDelta,
        pub y_rot: i8,
        pub x_rot: i8,
        pub on_ground: bool,
    }

    impl Encode for MoveEntityPosRot {
        fn encode(&self, out: &mut Vec<u8>) {
            write_varint(out, self.entity_id);
            write_varint(out, self.delta.properties(self.on_ground));
            self.delta.encode(out);
            self.y_rot.encode(out);
            self.x_rot.encode(out);
        }
    }

    impl Decode for MoveEntityPosRot {
        fn decode(r: &mut Reader<'_>) -> Result<Self> {
            let entity_id = r.varint()?;
            let (on_ground, steps) = split_properties(r.varint()?);
            let delta = VecDelta::decode(r, steps)?;
            Ok(Self {
                entity_id,
                delta,
                y_rot: r.i8()?,
                x_rot: r.i8()?,
                on_ground,
            })
        }
    }

    /// `move_entity_rot`.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "move_entity_rot")]
    pub struct MoveEntityRot {
        #[field(varint)]
        pub entity_id: i32,
        pub on_ground: bool,
        pub y_rot: i8,
        pub x_rot: i8,
    }

    /// Path of `entity_position_sync` (`PositionPath`).
    #[derive(Debug, Clone, PartialEq)]
    pub enum PositionPath {
        Linear(Vec3),
        /// Positions with tick offsets; the last one is the end position.
        Stepped(Vec<(Vec3, i32)>),
    }

    impl Encode for PositionPath {
        fn encode(&self, out: &mut Vec<u8>) {
            match self {
                Self::Linear(p) => {
                    write_varint(out, 0);
                    p.encode(out);
                }
                Self::Stepped(steps) => {
                    write_varint(out, 1);
                    crate::codec::write_len(out, steps.len());
                    for (p, t) in steps {
                        p.encode(out);
                        write_varint(out, *t);
                    }
                }
            }
        }
    }

    impl Decode for PositionPath {
        fn decode(r: &mut Reader<'_>) -> Result<Self> {
            match r.varint()? {
                0 => Ok(Self::Linear(Vec3::decode(r)?)),
                1 => {
                    let n = r.len_prefix("position steps", 1024, 25)?;
                    let mut v = Vec::with_capacity(n);
                    for _ in 0..n {
                        v.push((Vec3::decode(r)?, r.varint()?));
                    }
                    Ok(Self::Stepped(v))
                }
                _ => Err(Error::Invalid("position path type")),
            }
        }
    }

    /// `entity_position_sync`: absolute position of an entity.
    #[derive(Debug, Clone, PartialEq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "entity_position_sync")]
    pub struct EntityPositionSync {
        #[field(varint)]
        pub entity_id: i32,
        pub path: PositionPath,
        pub y_rot: f32,
        pub x_rot: f32,
        pub on_ground: bool,
    }

    /// `set_entity_motion`.
    #[derive(Debug, Clone, PartialEq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "set_entity_motion")]
    pub struct SetEntityMotion {
        #[field(varint)]
        pub entity_id: i32,
        pub movement: LpVec3,
    }

    /// `set_entity_data`: metadata items encoded by the entity code, ending with `0xFF`.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "set_entity_data")]
    pub struct SetEntityData {
        #[field(varint)]
        pub entity_id: i32,
        #[field(rest, max_len = 2_097_152)]
        pub items: RawBytes,
    }

    /// `set_default_spawn_position`.
    #[derive(Debug, Clone, PartialEq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "set_default_spawn_position")]
    pub struct SetDefaultSpawnPosition {
        pub pos: GlobalPos,
        pub yaw: f32,
        pub pitch: f32,
    }

    /// `player_abilities`. Flags: 1 invulnerable, 2 flying, 4 may fly, 8 instabuild.
    #[derive(Debug, Clone, PartialEq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "player_abilities")]
    pub struct PlayerAbilities {
        pub flags: u8,
        pub flying_speed: f32,
        pub walking_speed: f32,
    }

    /// `level_chunk_with_light`: chunk and light data as encoded by `vanillite-chunk`.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "level_chunk_with_light")]
    pub struct LevelChunkWithLight {
        pub x: i32,
        pub z: i32,
        #[field(rest, max_len = 2_097_152)]
        pub data: RawBytes,
    }

    /// `player_info_remove`.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "player_info_remove")]
    pub struct PlayerInfoRemove {
        #[field(max_len = 1 << 16)]
        pub ids: Vec<Uuid>,
    }

    /// Chat session of a player (`RemoteChatSession.Data`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ChatSession {
        pub session_id: Uuid,
        pub expires_at: i64,
        pub public_key: RawBytes,
        pub key_signature: RawBytes,
    }

    /// One entry of `player_info_update`; fields not covered by the packet's actions are ignored.
    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    pub struct PlayerInfoEntry {
        pub id: Uuid,
        /// Name and properties for `ADD_PLAYER`.
        pub profile: Option<GameProfile>,
        pub chat_session: Option<ChatSession>,
        pub game_mode: GameMode,
        pub listed: bool,
        pub latency: i32,
        pub display_name: Option<Component>,
        pub list_order: i32,
        pub show_hat: bool,
    }

    /// Action bits of `player_info_update`.
    pub mod info_action {
        pub const ADD_PLAYER: u8 = 1;
        pub const INITIALIZE_CHAT: u8 = 1 << 1;
        pub const UPDATE_GAME_MODE: u8 = 1 << 2;
        pub const UPDATE_LISTED: u8 = 1 << 3;
        pub const UPDATE_LATENCY: u8 = 1 << 4;
        pub const UPDATE_DISPLAY_NAME: u8 = 1 << 5;
        pub const UPDATE_LIST_ORDER: u8 = 1 << 6;
        pub const UPDATE_HAT: u8 = 1 << 7;
    }

    /// `player_info_update`: tab list and skin data.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct PlayerInfoUpdate {
        /// [`info_action`] bits (Java's 8-element `EnumSet` as one byte).
        pub actions: u8,
        pub entries: Vec<PlayerInfoEntry>,
    }

    impl Encode for PlayerInfoUpdate {
        fn encode(&self, out: &mut Vec<u8>) {
            use info_action::*;
            let a = self.actions;
            out.push(a);
            crate::codec::write_len(out, self.entries.len());
            for e in &self.entries {
                e.id.encode(out);
                if a & ADD_PLAYER != 0 {
                    let p = e.profile.clone().unwrap_or_default();
                    p.name.encode(out);
                    p.properties.encode(out);
                }
                if a & INITIALIZE_CHAT != 0 {
                    match &e.chat_session {
                        Some(s) => {
                            out.push(1);
                            s.session_id.encode(out);
                            s.expires_at.encode(out);
                            s.public_key.encode(out);
                            s.key_signature.encode(out);
                        }
                        None => out.push(0),
                    }
                }
                if a & UPDATE_GAME_MODE != 0 {
                    e.game_mode.encode(out);
                }
                if a & UPDATE_LISTED != 0 {
                    e.listed.encode(out);
                }
                if a & UPDATE_LATENCY != 0 {
                    write_varint(out, e.latency);
                }
                if a & UPDATE_DISPLAY_NAME != 0 {
                    e.display_name.encode(out);
                }
                if a & UPDATE_LIST_ORDER != 0 {
                    write_varint(out, e.list_order);
                }
                if a & UPDATE_HAT != 0 {
                    e.show_hat.encode(out);
                }
            }
        }
    }

    impl Decode for PlayerInfoUpdate {
        fn decode(r: &mut Reader<'_>) -> Result<Self> {
            use info_action::*;
            let a = r.u8()?;
            let n = r.len_prefix("player info entries", 1 << 16, 16)?;
            let mut entries = Vec::with_capacity(n);
            for _ in 0..n {
                let mut e = PlayerInfoEntry {
                    id: Uuid::decode(r)?,
                    ..Default::default()
                };
                if a & ADD_PLAYER != 0 {
                    let name = String::decode_limited(r, 16)?;
                    let properties = Vec::<Property>::decode_limited(r, 16)?;
                    e.profile = Some(GameProfile {
                        id: e.id,
                        name,
                        properties,
                    });
                }
                if a & INITIALIZE_CHAT != 0 && bool::decode(r)? {
                    e.chat_session = Some(ChatSession {
                        session_id: Uuid::decode(r)?,
                        expires_at: r.i64()?,
                        public_key: RawBytes::decode_limited(r, 512)?,
                        key_signature: RawBytes::decode_limited(r, 4096)?,
                    });
                }
                if a & UPDATE_GAME_MODE != 0 {
                    e.game_mode = GameMode::decode(r)?;
                }
                if a & UPDATE_LISTED != 0 {
                    e.listed = bool::decode(r)?;
                }
                if a & UPDATE_LATENCY != 0 {
                    e.latency = r.varint()?;
                }
                if a & UPDATE_DISPLAY_NAME != 0 {
                    e.display_name = Option::<Component>::decode_limited(r, 2_097_152)?;
                }
                if a & UPDATE_LIST_ORDER != 0 {
                    e.list_order = r.varint()?;
                }
                if a & UPDATE_HAT != 0 {
                    e.show_hat = bool::decode(r)?;
                }
                entries.push(e);
            }
            Ok(Self {
                actions: a,
                entries,
            })
        }
    }

    manual_packet!(MoveEntityPos, MOVE_ENTITY_POS, "move_entity_pos");
    manual_packet!(MoveEntityPosRot, MOVE_ENTITY_POS_ROT, "move_entity_pos_rot");
    manual_packet!(PlayerInfoUpdate, PLAYER_INFO_UPDATE, "player_info_update");
}
