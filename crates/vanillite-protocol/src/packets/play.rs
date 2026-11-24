//! Play state: the subset milestone 1 needs. Packets not listed here are decoded as
//! [`Unhandled`](crate::sets::ServerboundPlay::Unhandled) on the server side.

/// Serverbound play packets.
pub mod serverbound {
    use crate::Packet;
    use crate::codec::{Decode, Encode, Reader, Result};
    use crate::varint_enum;

    crate::common_serverbound!(Play);

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
}

/// Clientbound play packets.
pub mod clientbound {
    use crate::Packet;
    use crate::text::Component;
    use crate::types::VarIntList;

    crate::common_clientbound!(Play);

    /// `start_configuration`: sends the client back to configuration.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "start_configuration")]
    pub struct StartConfiguration;

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
        /// Event id: change game mode.
        pub const CHANGE_GAME_MODE: u8 = 3;
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
}
