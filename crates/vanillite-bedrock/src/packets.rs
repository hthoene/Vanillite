//! Bedrock packet ids and the encoders and decoders the frontend needs, for every supported
//! protocol ([`crate::version`]: functions taking a [`Version`] differ between releases). Field
//! layouts follow the protocol as documented by the gophertunnel project (MIT); the code is
//! written for Vanillite.

use crate::io::{self, Reader, Result};
use crate::version::Version;

pub mod id {
    pub const LOGIN: u32 = 1;
    pub const PLAY_STATUS: u32 = 2;
    pub const SERVER_TO_CLIENT_HANDSHAKE: u32 = 3;
    pub const CLIENT_TO_SERVER_HANDSHAKE: u32 = 4;
    pub const DISCONNECT: u32 = 5;
    pub const RESOURCE_PACKS_INFO: u32 = 6;
    pub const RESOURCE_PACK_STACK: u32 = 7;
    pub const RESOURCE_PACK_CLIENT_RESPONSE: u32 = 8;
    pub const TEXT: u32 = 9;
    pub const START_GAME: u32 = 11;
    pub const ADD_PLAYER: u32 = 12;
    pub const ADD_ACTOR: u32 = 13;
    pub const REMOVE_ACTOR: u32 = 14;
    pub const MOVE_ACTOR_ABSOLUTE: u32 = 18;
    pub const MOVE_PLAYER: u32 = 19;
    pub const UPDATE_BLOCK: u32 = 21;
    pub const UPDATE_ATTRIBUTES: u32 = 29;
    pub const SET_ACTOR_DATA: u32 = 39;
    pub const SET_ACTOR_MOTION: u32 = 40;
    pub const SET_ACTOR_LINK: u32 = 41;
    pub const ANIMATE: u32 = 44;
    pub const LEVEL_CHUNK: u32 = 58;
    pub const SET_COMMANDS_ENABLED: u32 = 59;
    pub const SET_PLAYER_GAME_TYPE: u32 = 62;
    pub const PLAYER_LIST: u32 = 63;
    pub const REQUEST_CHUNK_RADIUS: u32 = 69;
    pub const CHUNK_RADIUS_UPDATED: u32 = 70;
    pub const GAME_RULES_CHANGED: u32 = 72;
    pub const AVAILABLE_COMMANDS: u32 = 76;
    pub const COMMAND_REQUEST: u32 = 77;
    pub const SET_LOCAL_PLAYER_AS_INITIALIZED: u32 = 113;
    pub const AVAILABLE_ACTOR_IDENTIFIERS: u32 = 119;
    pub const NETWORK_CHUNK_PUBLISHER_UPDATE: u32 = 121;
    pub const BIOME_DEFINITION_LIST: u32 = 122;
    pub const NETWORK_SETTINGS: u32 = 143;
    pub const PLAYER_AUTH_INPUT: u32 = 144;
    pub const CREATIVE_CONTENT: u32 = 145;
    pub const ITEM_REGISTRY: u32 = 162;
    pub const UPDATE_ABILITIES: u32 = 187;
    pub const REQUEST_NETWORK_SETTINGS: u32 = 193;
    pub const SET_TIME: u32 = 10;
    pub const ADD_ITEM_ACTOR: u32 = 15;
    pub const TAKE_ITEM_ACTOR: u32 = 17;
    pub const LEVEL_EVENT: u32 = 25;
    pub const ACTOR_EVENT: u32 = 27;
    pub const MOB_EFFECT: u32 = 28;
    pub const INVENTORY_TRANSACTION: u32 = 30;
    pub const MOB_EQUIPMENT: u32 = 31;
    pub const MOB_ARMOUR_EQUIPMENT: u32 = 32;
    pub const INTERACT: u32 = 33;
    pub const BLOCK_PICK_REQUEST: u32 = 34;
    pub const PLAYER_ACTION: u32 = 36;
    pub const SET_SPAWN_POSITION: u32 = 43;
    pub const RESPAWN: u32 = 45;
    pub const CONTAINER_OPEN: u32 = 46;
    pub const CONTAINER_CLOSE: u32 = 47;
    pub const PLAYER_HOTBAR: u32 = 48;
    pub const INVENTORY_CONTENT: u32 = 49;
    pub const INVENTORY_SLOT: u32 = 50;
    pub const SET_DIFFICULTY: u32 = 60;
    pub const CHANGE_DIMENSION: u32 = 61;
    pub const BOSS_EVENT: u32 = 74;
    pub const PLAY_SOUND: u32 = 86;
    pub const SET_TITLE: u32 = 88;
    pub const PLAYER_SKIN: u32 = 93;
    pub const MODAL_FORM_REQUEST: u32 = 100;
    pub const MODAL_FORM_RESPONSE: u32 = 101;
    pub const SPAWN_PARTICLE_EFFECT: u32 = 118;
    pub const ITEM_STACK_REQUEST: u32 = 147;
    pub const ITEM_STACK_RESPONSE: u32 = 148;
    pub const DEATH_INFO: u32 = 189;
    pub const LEVEL_SOUND_EVENT: u32 = 123;
    pub const CLIENTBOUND_MAP_ITEM_DATA: u32 = 67;
    pub const MAP_INFO_REQUEST: u32 = 68;
    pub const BOOK_EDIT: u32 = 97;
    pub const PLAYER_START_ITEM_COOLDOWN: u32 = 176;
    pub const ADD_PAINTING: u32 = 22;
    pub const BLOCK_EVENT: u32 = 26;
    pub const CONTAINER_SET_DATA: u32 = 51;
    pub const CRAFTING_DATA: u32 = 52;
    pub const BLOCK_ACTOR_DATA: u32 = 56;
    pub const REMOVE_OBJECTIVE: u32 = 106;
    pub const SET_DISPLAY_OBJECTIVE: u32 = 107;
    pub const SET_SCORE: u32 = 108;
    pub const OPEN_SIGN: u32 = 202;
    pub const PLAYER_ENCHANT_OPTIONS: u32 = 146;
    pub const LECTERN_UPDATE: u32 = 125;
    pub const PLAYER_TOGGLE_CRAFTER_SLOT_REQUEST: u32 = 306;
}

/// Inventory window ids.
pub mod window {
    pub const INVENTORY: u32 = 0;
    pub const OFFHAND: u32 = 119;
    pub const ARMOUR: u32 = 120;
    pub const UI: u32 = 124;
}

/// `FullContainerName` container ids used in item stack requests and responses.
pub mod container {
    pub const ARMOUR: u8 = 6;
    pub const COMBINED_INVENTORY: u8 = 12;
    pub const CRAFTING_INPUT: u8 = 13;
    pub const HOTBAR: u8 = 28;
    pub const INVENTORY: u8 = 29;
    pub const OFFHAND: u8 = 34;
    pub const LEVEL_ENTITY: u8 = 7;
    pub const BREWING_INPUT: u8 = 9;
    pub const BREWING_RESULT: u8 = 10;
    pub const BREWING_FUEL: u8 = 11;
    pub const FURNACE_FUEL: u8 = 24;
    pub const FURNACE_INGREDIENT: u8 = 25;
    pub const FURNACE_RESULT: u8 = 26;
    pub const SHULKER_BOX: u8 = 30;
    pub const BLAST_FURNACE_INGREDIENT: u8 = 45;
    pub const SMOKER_INGREDIENT: u8 = 46;
    pub const BARREL: u8 = 58;
    pub const CURSOR: u8 = 59;
    pub const CREATED_OUTPUT: u8 = 60;
    pub const ANVIL_INPUT: u8 = 0;
    pub const ANVIL_MATERIAL: u8 = 1;
    pub const SMITHING_INPUT: u8 = 3;
    pub const SMITHING_MATERIAL: u8 = 4;
    pub const ENCHANTING_INPUT: u8 = 22;
    pub const ENCHANTING_MATERIAL: u8 = 23;
    pub const GRINDSTONE_INPUT: u8 = 50;
    pub const GRINDSTONE_ADDITIONAL: u8 = 51;
    pub const STONECUTTER_INPUT: u8 = 53;
    pub const CARTOGRAPHY_INPUT: u8 = 55;
    pub const CARTOGRAPHY_ADDITIONAL: u8 = 56;
    pub const SMITHING_TEMPLATE: u8 = 61;
    pub const CRAFTER: u8 = 62;
    pub const BEACON_PAYMENT: u8 = 8;
    pub const HORSE_EQUIP: u8 = 27;
    pub const LOOM_INPUT: u8 = 41;
    pub const LOOM_DYE: u8 = 42;
    pub const LOOM_MATERIAL: u8 = 43;
}

/// `ContainerType` of `container_open`.
pub mod container_type {
    pub const CONTAINER: i8 = 0;
    pub const WORKBENCH: i8 = 1;
    pub const FURNACE: i8 = 2;
    pub const BREWING_STAND: i8 = 4;
    pub const DISPENSER: i8 = 6;
    pub const HOPPER: i8 = 8;
    pub const CART_CHEST: i8 = 10;
    pub const CART_HOPPER: i8 = 11;
    pub const BLAST_FURNACE: i8 = 27;
    pub const SMOKER: i8 = 28;
    pub const CHEST_BOAT: i8 = 34;
    pub const ENCHANTMENT: i8 = 3;
    pub const ANVIL: i8 = 5;
    pub const GRINDSTONE: i8 = 26;
    pub const STONECUTTER: i8 = 29;
    pub const CARTOGRAPHY: i8 = 30;
    pub const SMITHING_TABLE: i8 = 33;
    pub const CRAFTER: i8 = 36;
    pub const HORSE: i8 = 12;
    pub const BEACON: i8 = 13;
    pub const LOOM: i8 = 24;
    pub const LECTERN: i8 = 25;
}

/// `player_action` / block action types used here.
pub mod action {
    pub const START_BREAK: i32 = 0;
    pub const ABORT_BREAK: i32 = 1;
    pub const RESPAWN: i32 = 7;
    pub const CRACK_BREAK: i32 = 18;
    pub const PREDICT_DESTROY_BLOCK: i32 = 26;
    pub const CONTINUE_DESTROY_BLOCK: i32 = 27;
}

/// `level_event` ids used here.
pub mod level_event {
    pub const PARTICLES_DESTROY_BLOCK: i32 = 2001;
    pub const START_RAIN: i32 = 3001;
    pub const START_THUNDER: i32 = 3002;
    pub const STOP_RAIN: i32 = 3003;
    pub const STOP_THUNDER: i32 = 3004;
    pub const START_BLOCK_CRACKING: i32 = 3600;
    pub const STOP_BLOCK_CRACKING: i32 = 3601;
}

/// `actor_event` ids used here.
pub mod actor_event {
    pub const HURT: u8 = 2;
    pub const DEATH: u8 = 3;
}

/// `play_status` values.
pub mod status {
    pub const LOGIN_SUCCESS: i32 = 0;
    pub const FAILED_CLIENT: i32 = 1;
    pub const FAILED_SERVER: i32 = 2;
    pub const PLAYER_SPAWN: i32 = 3;
}

/// `move_player` modes.
pub mod move_mode {
    pub const NORMAL: u8 = 0;
    pub const TELEPORT: u8 = 2;
}

/// Bits of `player_auth_input`'s input flags used here.
pub mod input {
    pub const JUMP_DOWN: u32 = 3;
    pub const SNEAKING: u32 = 8;
    pub const START_SPRINTING: u32 = 25;
    pub const STOP_SPRINTING: u32 = 26;
    pub const START_SNEAKING: u32 = 27;
    pub const STOP_SNEAKING: u32 = 28;
    pub const UP: u32 = 10;
    pub const DOWN: u32 = 11;
    pub const LEFT: u32 = 12;
    pub const RIGHT: u32 = 13;
    pub const UP_LEFT: u32 = 14;
    pub const UP_RIGHT: u32 = 15;
    pub const START_JUMPING: u32 = 31;
    pub const HANDLED_TELEPORT: u32 = 37;
    pub const START_FLYING: u32 = 42;
    pub const STOP_FLYING: u32 = 43;
    pub const CLIENT_PREDICTED_VEHICLE: u32 = 45;
    pub const PADDLING_LEFT: u32 = 46;
    pub const PADDLING_RIGHT: u32 = 47;
    pub const HORIZONTAL_COLLISION: u32 = 49;
    pub const VERTICAL_COLLISION: u32 = 50;
    /// Number of flags of this protocol version.
    pub const COUNT: u32 = 66;
}

/// Entity metadata keys and flags used here.
pub mod meta {
    pub const FLAGS: u32 = 0;
    pub const COLOR: u32 = 3;
    pub const NAME: u32 = 4;
    pub const EFFECT_COLOR: u32 = 8;
    pub const MARK_VARIANT: u32 = 43;
    pub const FLAG_RIDING: u32 = 2;
    pub const FLAG_SADDLED: u32 = 8;
    pub const FLAG_SHEARED: u32 = 31;
    pub const SCALE: u32 = 38;
    pub const WIDTH: u32 = 53;
    pub const HEIGHT: u32 = 54;
    pub const ALWAYS_SHOW_NAME: u32 = 81;
    pub const FLAG_SNEAKING: u32 = 1;
    pub const FLAG_SPRINTING: u32 = 3;
    pub const FLAG_SHOW_NAME: u32 = 14;
    pub const FLAG_ALWAYS_SHOW_NAME: u32 = 15;
    pub const FLAG_BREATHING: u32 = 35;
    pub const FLAG_HAS_COLLISION: u32 = 48;
    pub const FLAG_HAS_GRAVITY: u32 = 49;
    pub const FLAG_ON_FIRE: u32 = 0;
    pub const FLAG_INVISIBLE: u32 = 5;
    pub const FLAG_BABY: u32 = 11;
    pub const FLAG_SWIMMING: u32 = 57;
    pub const FLAG_GLIDING: u32 = 32;
    pub const FLAG_IGNITED: u32 = 10;
    pub const VARIANT: u32 = 2;
    pub const FUSE: u32 = 55;
    pub const LEASH_HOLDER: u32 = 37;
    pub const CONTAINER_TYPE: u32 = 44;
    pub const CONTAINER_SIZE: u32 = 45;
    pub const CONTAINER_STRENGTH_MODIFIER: u32 = 46;
    pub const STRENGTH: u32 = 75;
    pub const STRENGTH_MAX: u32 = 76;
    pub const FLAG_TAMED: u32 = 28;
    pub const FLAG_LEASHED: u32 = 30;
    pub const FLAG_CHESTED: u32 = 36;
}

/// Entity metadata, written in key order.
#[derive(Debug, Default)]
pub struct Metadata {
    count: u32,
    data: Vec<u8>,
}

impl Metadata {
    fn entry(&mut self, key: u32, kind: u8) {
        self.count = self.count.saturating_add(1);
        io::write_varu32(&mut self.data, key);
        io::write_varu32(&mut self.data, u32::from(kind));
        self.data.push(kind);
    }

    /// An `int64` entry (flags).
    pub fn long(mut self, key: u32, v: i64) -> Self {
        self.entry(key, 7);
        io::write_vari64(&mut self.data, v);
        self
    }

    /// A string entry.
    pub fn string(mut self, key: u32, v: &str) -> Self {
        self.entry(key, 4);
        io::write_str(&mut self.data, v);
        self
    }

    /// A float entry.
    pub fn float(mut self, key: u32, v: f32) -> Self {
        self.entry(key, 3);
        self.data.extend_from_slice(&v.to_le_bytes());
        self
    }

    /// An `int32` entry.
    pub fn int(mut self, key: u32, v: i32) -> Self {
        self.entry(key, 2);
        io::write_vari32(&mut self.data, v);
        self
    }

    /// A byte entry.
    pub fn byte(mut self, key: u32, v: u8) -> Self {
        self.entry(key, 0);
        self.data.push(v);
        self
    }

    /// Writes the entries.
    pub fn write(&self, out: &mut Vec<u8>) {
        io::write_varu32(out, self.count);
        out.extend_from_slice(&self.data);
    }
}

/// The flags word of an entity.
pub fn entity_flags(sneaking: bool, sprinting: bool) -> i64 {
    let mut flags = 0i64;
    for bit in [
        meta::FLAG_SHOW_NAME,
        meta::FLAG_ALWAYS_SHOW_NAME,
        meta::FLAG_BREATHING,
        meta::FLAG_HAS_COLLISION,
        meta::FLAG_HAS_GRAVITY,
    ] {
        flags |= 1i64.wrapping_shl(bit);
    }
    if sneaking {
        flags |= 1i64.wrapping_shl(meta::FLAG_SNEAKING);
    }
    if sprinting {
        flags |= 1i64.wrapping_shl(meta::FLAG_SPRINTING);
    }
    flags
}

/// `network_settings`: deflate above `threshold` bytes.
pub fn network_settings(threshold: u16) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&threshold.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.push(0);
    out.push(0);
    out.extend_from_slice(&0f32.to_le_bytes());
    out
}

/// `play_status`.
pub fn play_status(status: i32) -> Vec<u8> {
    status.to_be_bytes().to_vec()
}

/// `server_to_client_handshake`.
pub fn server_to_client_handshake(jwt: &str) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_str(&mut out, jwt);
    out
}

/// `disconnect` with a message.
pub fn disconnect(message: &str) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_vari32(&mut out, 0);
    io::write_varu32(&mut out, 0);
    io::write_str(&mut out, message);
    io::write_str(&mut out, message);
    out
}

/// `resource_packs_info` without packs.
pub fn resource_packs_info() -> Vec<u8> {
    let mut out = vec![0, 0, 0, 0];
    io::write_uuid(&mut out, 0);
    io::write_str(&mut out, "");
    io::write_varu32(&mut out, 0);
    out
}

/// `resource_pack_stack` without packs.
pub fn resource_pack_stack() -> Vec<u8> {
    let mut out = vec![0];
    io::write_varu32(&mut out, 0);
    io::write_str(&mut out, "*");
    out.extend_from_slice(&0u32.to_le_bytes());
    out.push(0);
    out.push(0);
    out
}

/// Reads a `resource_pack_client_response`: the response code.
pub fn read_pack_response(body: &[u8]) -> Result<u32> {
    Reader::new(body).varu32()
}

/// What `start_game` needs.
#[derive(Debug, Clone)]
pub struct StartGame<'a> {
    pub entity_id: i64,
    pub game_mode: i32,
    pub position: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub dimension: i32,
    pub difficulty: i32,
    pub spawn: [i32; 3],
    pub hardcore: bool,
    pub world_name: &'a str,
    pub permission: u8,
}

/// `start_game`.
pub fn start_game(p: &StartGame<'_>, v: &Version) -> Vec<u8> {
    let properties = v.palette.block_properties;
    let mut out = Vec::with_capacity(properties.len().saturating_add(512));
    io::write_vari64(&mut out, p.entity_id);
    io::write_varu64(&mut out, u64::try_from(p.entity_id).unwrap_or(0));
    io::write_vari32(&mut out, p.game_mode);
    io::write_vec3(&mut out, p.position);
    out.extend_from_slice(&p.pitch.to_le_bytes());
    out.extend_from_slice(&p.yaw.to_le_bytes());
    out.extend_from_slice(&0i64.to_le_bytes()); // seed
    out.extend_from_slice(&0i16.to_le_bytes()); // spawn biome type
    io::write_str(&mut out, ""); // user defined biome
    io::write_vari32(&mut out, p.dimension);
    io::write_vari32(&mut out, 1); // generator: infinite
    io::write_vari32(&mut out, p.game_mode);
    out.push(u8::from(p.hardcore));
    io::write_vari32(&mut out, p.difficulty);
    io::write_block_pos(&mut out, p.spawn);
    out.push(1); // achievements disabled
    io::write_vari32(&mut out, 0); // editor world type
    out.push(0); // created in editor
    out.push(0); // exported from editor
    if !v.before_26_60() {
        out.push(0); // editor level migration version
    }
    io::write_vari32(&mut out, 0); // day cycle lock time
    io::write_varu32(&mut out, 0); // education offer
    out.push(0); // education features
    io::write_str(&mut out, ""); // education product id
    out.extend_from_slice(&0f32.to_le_bytes()); // rain
    out.extend_from_slice(&0f32.to_le_bytes()); // lightning
    out.push(0); // confirmed platform locked content
    out.push(1); // multiplayer
    out.push(0); // LAN broadcast
    io::write_vari32(&mut out, 4); // Xbox Live broadcast: public
    io::write_vari32(&mut out, 4); // platform broadcast: public
    out.push(1); // commands enabled
    out.push(0); // texture packs required
    // Game rules: the server decides; the client must not simulate these.
    io::write_varu32(&mut out, 3);
    game_rule_bool(&mut out, "naturalregeneration", false);
    game_rule_bool(&mut out, "dodaylightcycle", false);
    game_rule_bool(&mut out, "doweathercycle", false);
    // Experiments needed for the data-driven block properties.
    let experiments = [
        "data_driven_items",
        "upcoming_creator_features",
        "experimental_molang_features",
    ];
    out.extend_from_slice(&u32::try_from(experiments.len()).unwrap_or(0).to_le_bytes());
    for e in experiments {
        io::write_str(&mut out, e);
        out.push(1);
    }
    out.push(0); // experiments previously toggled
    out.push(0); // bonus chest
    out.push(0); // start with map
    out.push(p.permission);
    out.extend_from_slice(&4i32.to_le_bytes()); // server chunk tick radius
    out.extend_from_slice(&[0; 10]); // locked packs, templates, villagers, persona/skin/emote
    io::write_str(&mut out, "*"); // base game version
    out.extend_from_slice(&0i32.to_le_bytes()); // limited world width
    out.extend_from_slice(&0i32.to_le_bytes()); // limited world depth
    out.push(1); // new nether
    io::write_str(&mut out, ""); // education button name
    io::write_str(&mut out, ""); // education link
    out.push(0); // no forced experimental gameplay
    out.push(0); // chat restriction level
    out.push(0); // player interactions enabled
    io::write_vari32(&mut out, 0); // editor connection policy
    out.push(0); // anonymous block drops in editor worlds
    io::write_str(&mut out, p.world_name); // level id
    io::write_str(&mut out, p.world_name); // world name
    io::write_str(&mut out, ""); // template content identity
    out.push(0); // trial
    io::write_vari32(&mut out, 0); // rewind history size
    out.push(1); // server authoritative block breaking
    out.extend_from_slice(&0i64.to_le_bytes()); // time
    io::write_vari32(&mut out, 0); // enchantment seed
    out.extend_from_slice(properties);
    io::write_str(&mut out, ""); // multiplayer correlation id
    out.push(1); // server authoritative inventory (item stack requests)
    io::write_str(&mut out, "*"); // game version
    out.extend_from_slice(&[0x0a, 0, 0]); // player property data: empty compound
    out.extend_from_slice(&0u64.to_le_bytes()); // block state checksum: unchecked
    io::write_uuid(&mut out, 0); // world template id
    out.push(0); // client side generation
    out.push(0); // block network ids are not hashes
    out.push(0); // server authoritative sound
    out.push(0); // no server join information
    for _ in 0..4 {
        io::write_str(&mut out, ""); // server, scenario, world and owner id
    }
    out
}

fn game_rule_bool(out: &mut Vec<u8>, name: &str, value: bool) {
    io::write_str(out, name);
    out.push(0); // not editable by the player
    io::write_varu32(out, 1);
    out.push(u8::from(value));
}

/// `chunk_radius_updated`.
pub fn chunk_radius_updated(radius: i32) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_vari32(&mut out, radius);
    out
}

/// Reads `request_chunk_radius`: the requested radius.
pub fn read_chunk_radius(body: &[u8]) -> Result<i32> {
    Reader::new(body).vari32()
}

/// `network_chunk_publisher_update`: the client keeps chunks within `radius` blocks of `pos`.
pub fn chunk_publisher_update(pos: [i32; 3], radius: u32) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_block_pos(&mut out, pos);
    io::write_varu32(&mut out, radius);
    out.extend_from_slice(&0u32.to_le_bytes());
    out
}

/// `move_player`.
#[allow(clippy::too_many_arguments)]
pub fn move_player(
    runtime_id: u64,
    pos: [f32; 3],
    pitch: f32,
    yaw: f32,
    head_yaw: f32,
    mode: u8,
    on_ground: bool,
    tick: u64,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(40);
    io::write_varu64(&mut out, runtime_id);
    io::write_vec3(&mut out, pos);
    for v in [pitch, yaw, head_yaw] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.push(mode);
    out.push(u8::from(on_ground));
    io::write_varu64(&mut out, 0);
    if mode == move_mode::TELEPORT {
        out.push(1);
        out.extend_from_slice(&0i32.to_le_bytes()); // cause: unknown
        out.extend_from_slice(&0i32.to_le_bytes()); // source entity type
    } else {
        out.push(0);
    }
    io::write_varu64(&mut out, tick);
    out
}

/// `move_actor_absolute`.
pub fn move_actor_absolute(
    runtime_id: u64,
    on_ground: bool,
    teleport: bool,
    pos: [f32; 3],
    rot: [f32; 3],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(24);
    io::write_varu64(&mut out, runtime_id);
    out.push(u8::from(on_ground) | if teleport { 2 } else { 0 });
    io::write_vec3(&mut out, pos);
    for r in rot {
        out.push(angle_byte(r));
    }
    out
}

/// An angle in degrees as a 1/256 turn byte.
pub fn angle_byte(degrees: f32) -> u8 {
    let turns = (degrees / 360.0).rem_euclid(1.0) * 256.0;
    crate::num::f32_to_u8(turns)
}

/// `remove_actor`.
pub fn remove_actor(unique_id: i64) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_vari64(&mut out, unique_id);
    out
}

/// What `add_player` needs.
#[derive(Debug)]
pub struct AddPlayer<'a> {
    pub uuid: u128,
    pub name: &'a str,
    pub entity_id: i64,
    pub pos: [f32; 3],
    pub pitch: f32,
    pub yaw: f32,
    pub head_yaw: f32,
    pub game_mode: i32,
    pub sneaking: bool,
}

/// `add_player`.
pub fn add_player(p: &AddPlayer<'_>, v: &Version) -> Vec<u8> {
    let mut out = Vec::with_capacity(256);
    io::write_uuid(&mut out, p.uuid);
    io::write_str(&mut out, p.name);
    io::write_varu64(&mut out, u64::try_from(p.entity_id).unwrap_or(0));
    io::write_str(&mut out, ""); // platform chat id
    io::write_vec3(&mut out, p.pos);
    io::write_vec3(&mut out, [0.0; 3]);
    for v in [p.pitch, p.yaw, p.head_yaw] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    write_empty_item(&mut out);
    io::write_vari32(&mut out, p.game_mode);
    Metadata::default()
        .long(meta::FLAGS, entity_flags(p.sneaking, false))
        .string(meta::NAME, p.name)
        .float(meta::SCALE, 1.0)
        .float(meta::WIDTH, 0.6)
        .float(meta::HEIGHT, 1.8)
        .byte(meta::ALWAYS_SHOW_NAME, 1)
        .write(&mut out);
    out.extend_from_slice(&[0, 0]); // no entity properties
    // Ability data: unique id, permissions, no layers.
    out.extend_from_slice(&p.entity_id.to_le_bytes());
    out.push(1);
    out.push(0);
    io::write_varu32(&mut out, 0);
    io::write_varu32(&mut out, 0); // entity links
    if !v.before_26_60() {
        out.push(0); // not a passenger of a block
    }
    io::write_str(&mut out, ""); // device id
    out.extend_from_slice(&0i32.to_le_bytes()); // build platform
    out
}

/// `add_actor` for a non-player entity with its metadata.
#[allow(clippy::too_many_arguments)]
pub fn add_actor(
    v: &Version,
    entity_id: i64,
    kind: &str,
    pos: [f32; 3],
    vel: [f32; 3],
    pitch: f32,
    yaw: f32,
    head_yaw: f32,
    metadata: &Metadata,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(96);
    io::write_vari64(&mut out, entity_id);
    io::write_varu64(&mut out, u64::try_from(entity_id).unwrap_or(0));
    io::write_str(&mut out, kind);
    io::write_vec3(&mut out, pos);
    io::write_vec3(&mut out, vel);
    for v in [pitch, yaw, head_yaw, yaw] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    io::write_varu32(&mut out, 0); // attributes
    metadata.write(&mut out);
    out.extend_from_slice(&[0, 0]); // no entity properties
    io::write_varu32(&mut out, 0); // links
    if !v.before_26_60() {
        out.push(0); // not a passenger of a block
    }
    out
}

/// An empty item stack (`ItemInstance` of air): id, count, metadata, no stack id, block
/// runtime id and empty user data.
fn write_empty_item(out: &mut Vec<u8>) {
    out.extend_from_slice(&[0; 8]);
}

/// One `player_list` entry to add.
#[derive(Debug)]
pub struct ListEntry<'a> {
    pub uuid: u128,
    pub entity_id: i64,
    pub name: &'a str,
    pub xuid: &'a str,
    pub skin: &'a crate::skin::Skin,
}

/// `player_list` adding entries.
pub fn player_list_add(entries: &[ListEntry<'_>], v: &Version) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_len(&mut out, entries.len());
    for e in entries {
        io::write_varu32(&mut out, 1);
        out.push(0);
        io::write_uuid(&mut out, e.uuid);
        io::write_vari64(&mut out, e.entity_id);
        io::write_str(&mut out, e.name);
        io::write_str(&mut out, e.xuid);
        if !v.before_26_60() {
            io::write_str(&mut out, ""); // PlayFab id
        }
        io::write_str(&mut out, ""); // platform chat id
        out.extend_from_slice(&0i32.to_le_bytes()); // build platform
        e.skin.write(&mut out, v);
        out.extend_from_slice(&[0, 0, 0]); // teacher, host, sub-client
        out.extend_from_slice(&0i32.to_be_bytes()); // player colour
    }
    out
}

/// `player_list` removing entries.
pub fn player_list_remove(uuids: &[u128]) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_len(&mut out, uuids.len());
    for &u in uuids {
        io::write_varu32(&mut out, 0);
        out.push(1);
        io::write_uuid(&mut out, u);
    }
    out
}

/// `text` of type raw (a message without author).
pub fn text_raw(message: &str) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(0); // needs translation
    out.push(0); // category: message only
    out.push(0); // type: raw
    io::write_str(&mut out, message);
    io::write_str(&mut out, ""); // xuid
    io::write_str(&mut out, ""); // platform chat id
    out.push(0); // no filtered message
    out
}

/// `text` of type tip (above the hotbar).
pub fn text_tip(message: &str) -> Vec<u8> {
    let mut out = text_raw(message);
    if let Some(t) = out.get_mut(2) {
        *t = 5;
    }
    out
}

/// Reads a chat `text` from the client: the message.
pub fn read_text_message(body: &[u8]) -> Result<String> {
    let mut r = Reader::new(body);
    r.bool()?;
    let _category = r.u8()?;
    let kind = r.u8()?;
    if kind != 1 {
        return Err(io::Error::Invalid("text type"));
    }
    let _source = r.string("source name", 256)?;
    Ok(r.string("message", 512)?.to_owned())
}

/// Reads `command_request`: the command line.
pub fn read_command(body: &[u8]) -> Result<String> {
    Ok(Reader::new(body).string("command", 32_767)?.to_owned())
}

/// `set_player_game_type`.
pub fn set_player_game_type(mode: i32) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_vari32(&mut out, mode);
    out
}

/// `update_attributes` with the movement speed.
pub fn update_movement_speed(runtime_id: u64, speed: f32, tick: u64) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_varu64(&mut out, runtime_id);
    io::write_varu32(&mut out, 1);
    for v in [0.0, f32::MAX, speed, 0.0, f32::MAX, 0.1] {
        out.extend_from_slice(&f32::to_le_bytes(v));
    }
    io::write_str(&mut out, "minecraft:movement");
    io::write_varu32(&mut out, 0);
    io::write_varu64(&mut out, tick);
    out
}

/// Ability bits.
pub mod ability {
    pub const BUILD: u32 = 1;
    pub const MINE: u32 = 1 << 1;
    pub const DOORS: u32 = 1 << 2;
    pub const CONTAINERS: u32 = 1 << 3;
    pub const ATTACK_PLAYERS: u32 = 1 << 4;
    pub const ATTACK_MOBS: u32 = 1 << 5;
    pub const INVULNERABLE: u32 = 1 << 8;
    pub const FLYING: u32 = 1 << 9;
    pub const MAY_FLY: u32 = 1 << 10;
    pub const INSTABUILD: u32 = 1 << 11;
    pub const NO_CLIP: u32 = 1 << 17;
    pub const ALL: u32 = (1 << 20) - 1;
}

/// `update_abilities` with one base layer.
pub fn update_abilities(
    entity_id: i64,
    operator: bool,
    values: u32,
    fly_speed: f32,
    walk_speed: f32,
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&entity_id.to_le_bytes());
    out.push(if operator { 2 } else { 1 });
    out.push(u8::from(operator));
    io::write_varu32(&mut out, 1);
    out.extend_from_slice(&1u16.to_le_bytes()); // base layer
    out.extend_from_slice(&ability::ALL.to_le_bytes());
    out.extend_from_slice(&values.to_le_bytes());
    out.extend_from_slice(&fly_speed.to_le_bytes());
    out.extend_from_slice(&1f32.to_le_bytes());
    out.extend_from_slice(&walk_speed.to_le_bytes());
    out
}

/// `update_block` in layer `layer`.
pub fn update_block(pos: [i32; 3], runtime_id: u32, layer: u32) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_block_pos(&mut out, pos);
    io::write_varu32(&mut out, runtime_id);
    io::write_varu32(&mut out, 2); // network flag
    io::write_varu32(&mut out, layer);
    out
}

/// `add_painting`: a painting by its Bedrock motif name, centred on its wall area.
pub fn add_painting(entity_id: i64, pos: [f32; 3], direction: i32, title: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(48);
    io::write_vari64(&mut out, entity_id);
    io::write_varu64(&mut out, u64::try_from(entity_id).unwrap_or(0));
    io::write_vec3(&mut out, pos);
    io::write_vari32(&mut out, direction);
    io::write_str(&mut out, title);
    out
}

/// `set_commands_enabled`.
pub fn set_commands_enabled(on: bool) -> Vec<u8> {
    vec![u8::from(on)]
}

/// The fields of `player_auth_input` the frontend uses.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AuthInput {
    pub pitch: f32,
    pub yaw: f32,
    /// Eye position.
    pub position: [f32; 3],
    /// Movement keys: x left (+) / right (-), y forward (+) / back (-).
    pub move_vector: [f32; 2],
    pub head_yaw: f32,
    /// Input flag bits (index = flag id).
    pub flags: u128,
    pub tick: u64,
    pub delta: [f32; 3],
}

impl AuthInput {
    /// Whether flag `bit` is set.
    pub fn has(&self, bit: u32) -> bool {
        self.flags.wrapping_shr(bit) & 1 != 0
    }
}

/// Reads the leading fields of `player_auth_input` (the rest is not needed).
pub fn read_auth_input(body: &[u8], v: &Version) -> Result<AuthInput> {
    let mut r = Reader::new(body);
    let pitch = r.f32()?;
    let yaw = r.f32()?;
    let position = r.vec3()?;
    let move_vector = [r.f32()?, r.f32()?];
    let head_yaw = r.f32()?;
    let n = if !v.before_26_50() || r.bool()? {
        r.varu32()?
    } else {
        0
    };
    if n > input::COUNT {
        return Err(io::Error::TooLong("input flags"));
    }
    let mut flags = 0u128;
    for _ in 0..n {
        let bit = r.vari32()?;
        let bit = u32::try_from(bit).map_err(|_| io::Error::Invalid("input flag"))?;
        if bit >= input::COUNT {
            return Err(io::Error::Invalid("input flag"));
        }
        flags |= 1u128.wrapping_shl(bit);
    }
    let _input_mode = r.varu32()?;
    let _play_mode = r.varu32()?;
    let _interaction_model = r.vari32()?;
    let _interact_rot = [r.f32()?, r.f32()?];
    let tick = r.varu64()?;
    let delta = r.vec3()?;
    for v in position
        .iter()
        .chain(&delta)
        .chain(&move_vector)
        .chain(&[pitch, yaw, head_yaw])
    {
        if !v.is_finite() {
            return Err(io::Error::Invalid("non-finite movement"));
        }
    }
    Ok(AuthInput {
        move_vector,
        pitch,
        yaw,
        position,
        head_yaw,
        flags,
        tick,
        delta,
    })
}

/// Writes `player_auth_input` (test client): movement and block actions, no other optional
/// parts.
pub fn write_auth_input(i: &AuthInput, v: &Version, blocks: &[(i32, [i32; 3], i32)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&i.pitch.to_le_bytes());
    out.extend_from_slice(&i.yaw.to_le_bytes());
    io::write_vec3(&mut out, i.position);
    for m in i.move_vector {
        out.extend_from_slice(&m.to_le_bytes());
    }
    out.extend_from_slice(&i.head_yaw.to_le_bytes());
    if v.before_26_50() {
        out.push(1);
    }
    let bits: Vec<u32> = (0..input::COUNT).filter(|&b| i.has(b)).collect();
    io::write_len(&mut out, bits.len());
    for b in bits {
        io::write_vari32(&mut out, i32::try_from(b).unwrap_or(0));
    }
    io::write_varu32(&mut out, 1);
    io::write_varu32(&mut out, 0);
    io::write_vari32(&mut out, 0);
    out.extend_from_slice(&[0; 8]);
    io::write_varu64(&mut out, i.tick);
    io::write_vec3(&mut out, i.delta);
    write_optional_flag(&mut out, false, v); // item interaction
    write_optional_flag(&mut out, false, v); // item stack request
    write_optional_flag(&mut out, !blocks.is_empty(), v);
    if !blocks.is_empty() {
        io::write_len(&mut out, blocks.len());
        for (a, pos, face) in blocks {
            io::write_vari32(&mut out, *a);
            io::write_block_pos(&mut out, *pos);
            io::write_vari32(&mut out, *face);
        }
    }
    write_optional_flag(&mut out, false, v); // vehicle rotation
    write_optional_flag(&mut out, false, v); // predicted vehicle
    out.extend_from_slice(&[0; 8 + 12 + 8]);
    out
}

/// The presence flag of an optional field (inside an always-present outer optional before
/// 1.26.50).
pub fn write_optional_flag(out: &mut Vec<u8>, present: bool, v: &Version) {
    if v.before_26_50() {
        out.push(1);
    }
    out.push(u8::from(present));
}

/// Reads the presence flag of an optional field (see [`write_optional_flag`]).
fn read_optional_flag(r: &mut Reader<'_>, v: &Version) -> Result<bool> {
    Ok((!v.before_26_50() || r.bool()?) && r.bool()?)
}

// ---- Gameplay -----------------------------------------------------------------------------------

/// A slot in an item stack request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotRef {
    pub container: u8,
    pub slot: u8,
    pub net_id: i32,
}

fn read_slot_ref(r: &mut Reader<'_>) -> Result<SlotRef> {
    let container = r.u8()?;
    if r.bool()? {
        r.array::<4>()?;
    }
    let slot = r.u8()?;
    let net_id = i32::from_le_bytes(r.array::<4>()?);
    Ok(SlotRef {
        container,
        slot,
        net_id,
    })
}

/// One action of an item stack request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StackAction {
    Take {
        count: u8,
        src: SlotRef,
        dst: SlotRef,
    },
    Place {
        count: u8,
        src: SlotRef,
        dst: SlotRef,
    },
    Swap {
        src: SlotRef,
        dst: SlotRef,
    },
    Drop {
        count: u8,
        src: SlotRef,
    },
    Destroy {
        count: u8,
        src: SlotRef,
    },
    Consume {
        count: u8,
        src: SlotRef,
    },
    Create,
    CraftCreative {
        net_id: u32,
        times: u8,
    },
    CraftRecipe {
        net_id: u32,
        times: u8,
    },
    /// A recipe of the recipe book, placed from the inventory and crafted.
    CraftRecipeAuto {
        net_id: u32,
        times: u8,
    },
    /// An anvil (or cartography table) craft; `filter` indexes the request's filter strings
    /// (the anvil's name field).
    CraftRecipeOptional {
        net_id: u32,
        filter: i32,
    },
    /// A grindstone craft.
    CraftGrindstone,
    /// The beacon's confirm button: Bedrock effect ids (0: none).
    BeaconPayment {
        primary: i32,
        secondary: i32,
    },
    /// A loom craft: the Bedrock banner pattern code (`cre`, `bo`, ...), zero padded.
    CraftLoom {
        pattern: [u8; 4],
    },
    /// Tool durability prediction of block breaking (nothing to do).
    MineBlock,
    /// The deprecated list of crafting results (nothing to do).
    Results,
    /// Anything the frontend does not translate (lab table, beacon, loom, ...).
    Unsupported,
}

/// One item stack request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackRequest {
    pub id: i32,
    pub actions: Vec<StackAction>,
    /// Text the request names (the anvil's name field).
    pub filters: Vec<String>,
}

/// Largest number of actions in one request.
const MAX_STACK_ACTIONS: usize = 64;

fn read_descriptor_count(r: &mut Reader<'_>) -> Result<()> {
    let kind = r.varu32()?;
    r.u8()?;
    match kind {
        0 => {}
        1 => {
            r.string("item name", 256)?;
            r.vari32()?;
        }
        2 => {
            r.string("molang", 4096)?;
            r.array::<2>()?;
        }
        3 => {
            r.string("item tag", 256)?;
        }
        _ => return Err(io::Error::Invalid("item descriptor")),
    }
    r.array::<2>()?;
    Ok(())
}

fn read_stack_action(r: &mut Reader<'_>, v: &Version) -> Result<StackAction> {
    let mut variant = r.varu32()?;
    // 1.26.60 inserted the reserved action 16.
    if v.before_26_60() && variant >= 16 {
        variant = variant.saturating_add(1);
    }
    r.u8()?;
    let transfer = |r: &mut Reader<'_>| -> Result<(u8, SlotRef, SlotRef)> {
        Ok((r.u8()?, read_slot_ref(r)?, read_slot_ref(r)?))
    };
    Ok(match variant {
        0 => {
            let (count, src, dst) = transfer(r)?;
            StackAction::Take { count, src, dst }
        }
        1 => {
            let (count, src, dst) = transfer(r)?;
            StackAction::Place { count, src, dst }
        }
        2 => StackAction::Swap {
            src: read_slot_ref(r)?,
            dst: read_slot_ref(r)?,
        },
        3 => {
            let count = r.u8()?;
            let src = read_slot_ref(r)?;
            r.bool()?;
            StackAction::Drop { count, src }
        }
        4 | 5 => {
            let count = r.u8()?;
            let src = read_slot_ref(r)?;
            if variant == 4 {
                StackAction::Destroy { count, src }
            } else {
                StackAction::Consume { count, src }
            }
        }
        6 => {
            r.u8()?;
            StackAction::Create
        }
        // Lab table combine.
        7 => StackAction::Unsupported,
        8 => StackAction::BeaconPayment {
            primary: r.vari32()?,
            secondary: r.vari32()?,
        },
        9 => {
            r.vari32()?;
            r.vari32()?;
            r.array::<4>()?;
            StackAction::MineBlock
        }
        10 => StackAction::CraftRecipe {
            net_id: r.varu32()?,
            times: r.u8()?,
        },
        11 => {
            let net_id = r.varu32()?;
            let times = r.u8()?;
            let n = r.u8()?;
            for _ in 0..n {
                read_descriptor_count(r)?;
            }
            StackAction::CraftRecipeAuto { net_id, times }
        }
        12 => StackAction::CraftCreative {
            net_id: r.varu32()?,
            times: r.u8()?,
        },
        13 => StackAction::CraftRecipeOptional {
            net_id: r.varu32()?,
            filter: i32::from_le_bytes(r.array::<4>()?),
        },
        14 => {
            r.array::<4>()?;
            r.u8()?;
            r.vari32()?;
            StackAction::CraftGrindstone
        }
        15 => {
            let code = r.string("loom pattern", 256)?;
            r.u8()?;
            let mut pattern = [0u8; 4];
            match code.as_bytes() {
                b if b.len() <= 4 => {
                    for (p, c) in pattern.iter_mut().zip(b) {
                        *p = *c;
                    }
                    StackAction::CraftLoom { pattern }
                }
                _ => StackAction::Unsupported,
            }
        }
        16 => {
            r.string("reserved id", 256)?;
            r.u8()?;
            StackAction::Unsupported
        }
        17 => StackAction::Unsupported,
        18 => {
            let n = r.len("craft results", 64)?;
            for _ in 0..n {
                let kind = r.varu32()?;
                r.u8()?;
                if kind == 1 {
                    r.string("item name", 256)?;
                    r.vari32()?;
                }
                r.array::<2>()?;
                r.varu32()?;
                r.bytes("item user data", 1 << 16)?;
            }
            r.u8()?;
            StackAction::Results
        }
        _ => return Err(io::Error::Invalid("stack request action")),
    })
}

/// Reads one `ItemStackRequest` (also embedded in `player_auth_input`).
pub fn read_stack_request(r: &mut Reader<'_>, v: &Version) -> Result<StackRequest> {
    let id = r.vari32()?;
    let n = r.len("stack actions", MAX_STACK_ACTIONS)?;
    let mut actions = Vec::with_capacity(n);
    for _ in 0..n {
        actions.push(read_stack_action(r, v)?);
    }
    let strings = r.len("filter strings", 64)?;
    let mut filters = Vec::with_capacity(strings);
    for _ in 0..strings {
        filters.push(r.string("filter string", 1024)?.to_owned());
    }
    r.array::<4>()?;
    Ok(StackRequest {
        id,
        actions,
        filters,
    })
}

/// Reads `item_stack_request`.
pub fn read_stack_requests(body: &[u8], v: &Version) -> Result<Vec<StackRequest>> {
    let mut r = Reader::new(body);
    let n = r.len("stack requests", 64)?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(read_stack_request(&mut r, v)?);
    }
    Ok(out)
}

/// One slot of an item stack response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResponseSlot {
    pub slot: u8,
    pub count: u8,
    pub net_id: i32,
    pub durability: i32,
}

/// One item stack response: `None` slots = rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackResponse {
    pub id: i32,
    pub containers: Option<Vec<(u8, Vec<ResponseSlot>)>>,
}

/// `item_stack_response`.
pub fn item_stack_response(responses: &[StackResponse], v: &Version) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_len(&mut out, responses.len());
    for resp in responses {
        out.push(if resp.containers.is_some() { 0 } else { 1 });
        io::write_vari32(&mut out, resp.id);
        match &resp.containers {
            Some(containers) if !containers.is_empty() => {
                write_optional_flag(&mut out, true, v);
                io::write_len(&mut out, containers.len());
                for (container, slots) in containers {
                    out.push(*container);
                    out.push(0); // not dynamic
                    io::write_len(&mut out, slots.len());
                    for s in slots {
                        out.push(s.slot);
                        out.push(s.slot);
                        out.push(s.count);
                        write_optional_flag(&mut out, s.net_id > 0, v);
                        if s.net_id > 0 {
                            io::write_vari32(&mut out, s.net_id);
                        }
                        io::write_str(&mut out, "");
                        out.push(0);
                        io::write_vari32(&mut out, s.durability);
                    }
                }
            }
            _ => write_optional_flag(&mut out, false, v),
        }
    }
    out
}

/// `inventory_content` from pre-encoded `ItemInstance`s.
pub fn inventory_content(window: u32, count: usize, items: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(items.len().saturating_add(16));
    io::write_varu32(&mut out, window);
    io::write_len(&mut out, count);
    out.extend_from_slice(items);
    out.extend_from_slice(&[0, 0]); // container name: none, not dynamic
    write_empty_item(&mut out); // storage item
    out
}

/// `inventory_slot` from a pre-encoded `ItemInstance`.
pub fn inventory_slot(window: u32, slot: u32, item: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(item.len().saturating_add(8));
    io::write_varu32(&mut out, window);
    io::write_varu32(&mut out, slot);
    out.push(0); // no container name
    out.push(0); // no storage item
    out.extend_from_slice(item);
    out
}

/// `mob_equipment` (held item) from a pre-encoded `ItemInstance`.
pub fn mob_equipment(runtime_id: u64, item: &[u8], slot: u8, window: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(item.len().saturating_add(12));
    io::write_varu64(&mut out, runtime_id);
    out.extend_from_slice(item);
    out.push(slot);
    out.push(slot);
    out.push(window);
    out
}

/// Reads `mob_equipment` from the client: the selected hotbar slot and window.
pub fn read_mob_equipment(body: &[u8]) -> Result<(u8, u8)> {
    let mut r = Reader::new(body);
    r.varu64()?;
    crate::items::read_instance(&mut r)?;
    let _inventory_slot = r.u8()?;
    let hotbar = r.u8()?;
    let window = r.u8()?;
    Ok((hotbar, window))
}

/// `mob_armour_equipment` from pre-encoded `ItemInstance`s (head, chest, legs, feet, body).
pub fn mob_armour_equipment(runtime_id: u64, items: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(items.len().saturating_add(10));
    io::write_varu64(&mut out, runtime_id);
    out.extend_from_slice(items);
    out
}

/// `player_hotbar`: select a hotbar slot.
pub fn player_hotbar(slot: u32) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_varu32(&mut out, slot);
    out.push(0);
    out.push(1);
    out
}

/// `container_open`.
pub fn container_open(window: u8, kind: i8, pos: [i32; 3], entity: i64) -> Vec<u8> {
    let mut out = vec![window, kind.to_le_bytes()[0]];
    io::write_block_pos(&mut out, pos);
    io::write_vari64(&mut out, entity);
    out
}

/// `container_set_data`.
/// One option of `player_enchant_options`: the shown enchantment and its level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnchantOption {
    pub cost: u8,
    pub enchantment: u8,
    pub level: u8,
    pub net_id: u32,
}

/// `player_enchant_options`: the options of the enchanting table.
pub fn player_enchant_options(options: &[EnchantOption]) -> Vec<u8> {
    const NAMES: [&str; 3] = ["vanillite ", "fire light ", "free elder "];
    let mut out = Vec::new();
    io::write_len(&mut out, options.len());
    for (i, o) in options.iter().enumerate() {
        out.push(o.cost);
        out.extend_from_slice(&0i32.to_le_bytes());
        io::write_len(&mut out, 1);
        out.extend_from_slice(&[o.enchantment, o.level]);
        io::write_len(&mut out, 0);
        io::write_len(&mut out, 0);
        io::write_str(&mut out, NAMES.get(i).copied().unwrap_or(""));
        io::write_varu32(&mut out, o.net_id);
    }
    out
}

pub fn container_set_data(window: u8, key: i32, value: i32) -> Vec<u8> {
    let mut out = vec![window];
    io::write_vari32(&mut out, key);
    io::write_vari32(&mut out, value);
    out
}

/// `block_event` (type 1: chest lid, data 1 open / 0 closed).
pub fn block_event(pos: [i32; 3], kind: i32, data: i32) -> Vec<u8> {
    let mut out = Vec::with_capacity(20);
    io::write_block_pos(&mut out, pos);
    io::write_vari32(&mut out, kind);
    io::write_vari32(&mut out, data);
    out
}

/// `block_actor_data` from a network NBT compound.
pub fn block_actor_data(pos: [i32; 3], nbt: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nbt.len().saturating_add(16));
    io::write_block_pos(&mut out, pos);
    out.extend_from_slice(nbt);
    out
}

/// `open_sign`.
pub fn open_sign(pos: [i32; 3], front: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(16);
    io::write_block_pos(&mut out, pos);
    out.push(u8::from(front));
    out
}

/// `set_display_objective` (`slot`: `sidebar`, `list` or `belowname`; descending order).
pub fn set_display_objective(slot: &str, name: &str, display: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(name.len().saturating_add(display.len()).saturating_add(24));
    io::write_str(&mut out, slot);
    io::write_str(&mut out, name);
    io::write_str(&mut out, display);
    io::write_str(&mut out, "dummy");
    io::write_vari32(&mut out, 1);
    out
}

/// `remove_objective`.
pub fn remove_objective(name: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(name.len().saturating_add(2));
    io::write_str(&mut out, name);
    out
}

/// `set_score` of fake-player entries: entry id, objective, score and shown name.
pub fn set_score<'a>(
    entries: impl ExactSizeIterator<Item = (i64, &'a str, i32, &'a str)>,
) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_len(&mut out, entries.len());
    for (id, objective, score, name) in entries {
        io::write_varu32(&mut out, 3);
        io::write_str(&mut out, "changefakeplayer");
        io::write_vari64(&mut out, id);
        io::write_str(&mut out, objective);
        out.extend_from_slice(&score.to_le_bytes());
        io::write_str(&mut out, name);
    }
    out
}

/// `container_close` (`server_side`: the server closes it, not an answer to the client).
pub fn container_close(window: u8, kind: i8, server_side: bool) -> Vec<u8> {
    vec![window, kind.to_le_bytes()[0], u8::from(server_side)]
}

/// Reads `container_close`: the window id.
pub fn read_container_close(body: &[u8]) -> Result<u8> {
    Reader::new(body).u8()
}

/// What an `inventory_transaction` (or the item interaction of `player_auth_input`) asks for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Transaction {
    /// Click on a block (0), in the air (1) or break a block (2).
    UseItem {
        action: u32,
        pos: [i32; 3],
        face: i32,
        hotbar: i32,
        click: [f32; 3],
    },
    /// Interact (0) with or attack (1) an entity.
    UseItemOnEntity {
        target: u64,
        action: u32,
        hotbar: i32,
    },
    /// Release a used item (bow, food).
    Release { action: u32 },
    /// Legacy inventory actions (dropping from the hotbar outside a screen) and anything else.
    Other,
}

fn read_block_pos(r: &mut Reader<'_>) -> Result<[i32; 3]> {
    Ok([r.vari32()?, r.vari32()?, r.vari32()?])
}

fn read_legacy_slots(r: &mut Reader<'_>) -> Result<()> {
    let _legacy_id = r.vari32()?;
    if r.bool()? {
        let n = r.len("legacy slots", 64)?;
        for _ in 0..n {
            r.u8()?;
            r.bytes("legacy slot list", 256)?;
        }
    }
    Ok(())
}

fn read_inventory_actions(r: &mut Reader<'_>, v: &Version) -> Result<()> {
    let n = r.len("inventory actions", 64)?;
    for _ in 0..n {
        r.varu32()?;
        if read_optional_flag(r, v)? {
            r.u8()?;
        }
        if read_optional_flag(r, v)? {
            r.varu32()?;
        }
        r.varu32()?;
        crate::items::read_instance(r)?;
        crate::items::read_instance(r)?;
    }
    Ok(())
}

fn read_use_item(r: &mut Reader<'_>, v: &Version) -> Result<Transaction> {
    let action = u32::try_from(r.vari32()?).map_err(|_| io::Error::Invalid("use item action"))?;
    let _trigger = r.u8()?;
    let pos = read_block_pos(r)?;
    let face = i32::from(r.u8()?);
    let hotbar = r.vari32()?;
    if !v.before_26_50() {
        let _hand = r.u8()?;
    }
    crate::items::read_instance(r)?;
    let _player = r.vec3()?;
    let click = r.vec3()?;
    let _block = r.varu32()?;
    let _prediction = r.u8()?;
    let _cooldown = r.u8()?;
    if !click.iter().all(|v| v.is_finite()) {
        return Err(io::Error::Invalid("click position"));
    }
    Ok(Transaction::UseItem {
        action,
        pos,
        face,
        hotbar,
        click,
    })
}

/// Reads `inventory_transaction`.
pub fn read_inventory_transaction(body: &[u8], v: &Version) -> Result<Transaction> {
    let mut r = Reader::new(body);
    read_legacy_slots(&mut r)?;
    // Before 1.26.50 the type and the actions carry an (always set) presence flag.
    if v.before_26_50() {
        r.bool()?;
    }
    let kind = r.varu32()?;
    if v.before_26_50() {
        r.bool()?;
    }
    read_inventory_actions(&mut r, v)?;
    Ok(match kind {
        2 => read_use_item(&mut r, v)?,
        3 => {
            let target = r.varu64()?;
            let action = u32::try_from(r.vari32()?).unwrap_or(u32::MAX);
            let hotbar = r.vari32()?;
            Transaction::UseItemOnEntity {
                target,
                action,
                hotbar,
            }
        }
        4 => Transaction::Release {
            action: u32::try_from(r.vari32()?).unwrap_or(u32::MAX),
        },
        _ => Transaction::Other,
    })
}

/// A block action of `player_auth_input`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockAction {
    pub action: i32,
    pub pos: [i32; 3],
    pub face: i32,
}

/// The optional tail of `player_auth_input`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AuthInputExtra {
    pub item_interaction: Option<Transaction>,
    pub stack_request: Option<StackRequest>,
    pub block_actions: Vec<BlockAction>,
    /// Pitch and yaw of the vehicle the client predicts.
    pub vehicle_rotation: Option<[f32; 2]>,
}

/// Reads the optional parts of `player_auth_input` after the movement fields.
pub fn read_auth_input_extra(body: &[u8], v: &Version) -> Result<AuthInputExtra> {
    let mut r = Reader::new(body);
    // Skip the fixed fields read by `read_auth_input`.
    r.take(4 * 2 + 12 + 8 + 4)?;
    let n = if !v.before_26_50() || r.bool()? {
        r.len("input flags", usize::try_from(input::COUNT).unwrap_or(0))?
    } else {
        0
    };
    for _ in 0..n {
        r.vari32()?;
    }
    r.varu32()?;
    r.varu32()?;
    r.vari32()?;
    r.take(8)?;
    r.varu64()?;
    r.vec3()?;
    let mut extra = AuthInputExtra::default();
    if read_optional_flag(&mut r, v)? {
        read_legacy_slots(&mut r)?;
        // Before 1.26.50 the actions are an optional inside an optional.
        if !v.before_26_50() || read_optional_flag(&mut r, v)? {
            read_inventory_actions(&mut r, v)?;
        }
        extra.item_interaction = Some(read_use_item(&mut r, v)?);
    }
    if read_optional_flag(&mut r, v)? {
        extra.stack_request = Some(read_stack_request(&mut r, v)?);
    }
    if read_optional_flag(&mut r, v)? {
        let n = r.len("block actions", 64)?;
        for _ in 0..n {
            let action = r.vari32()?;
            let pos = read_block_pos(&mut r)?;
            let face = r.vari32()?;
            extra.block_actions.push(BlockAction { action, pos, face });
        }
    }
    if read_optional_flag(&mut r, v)? {
        extra.vehicle_rotation = Some([r.f32()?, r.f32()?]);
    }
    Ok(extra)
}

/// Reads `player_action`: action, block position and face.
pub fn read_player_action(body: &[u8]) -> Result<(i32, [i32; 3], i32)> {
    let mut r = Reader::new(body);
    r.varu64()?;
    let action = r.vari32()?;
    let pos = read_block_pos(&mut r)?;
    let _result = read_block_pos(&mut r)?;
    let face = r.vari32()?;
    Ok((action, pos, face))
}

/// `set_actor_link` (kind 0 remove, 1 rider, 2 passenger).
pub fn set_actor_link(ridden: i64, rider: i64, kind: u8, immediate: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(24);
    io::write_vari64(&mut out, ridden);
    io::write_vari64(&mut out, rider);
    out.push(kind);
    out.push(u8::from(immediate));
    out.push(1); // rider initiated
    out.extend_from_slice(&0f32.to_le_bytes()); // vehicle angular velocity
    out
}

/// `spawn_particle_effect` of a vanilla particle effect without MoLang variables.
pub fn spawn_particle_effect(dimension: u8, pos: [f32; 3], name: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(name.len().saturating_add(24));
    out.push(dimension);
    io::write_vari64(&mut out, -1);
    io::write_vec3(&mut out, pos);
    io::write_str(&mut out, name);
    out.push(0); // no MoLang variables
    out
}

/// Reads `interact`: action and target runtime id.
pub fn read_interact(body: &[u8]) -> Result<(u8, u64)> {
    let mut r = Reader::new(body);
    Ok((r.u8()?, r.varu64()?))
}

/// Reads `lectern_update`: the page (spread of two pages) and the lectern's position.
pub fn read_lectern_update(body: &[u8]) -> Result<(u8, [i32; 3])> {
    let mut r = Reader::new(body);
    let page = r.u8()?;
    let _count = r.u8()?;
    Ok((page, read_block_pos(&mut r)?))
}

/// Reads `player_toggle_crafter_slot_request`: the crafter, the slot and whether it is now
/// disabled.
pub fn read_toggle_crafter_slot(body: &[u8]) -> Result<([i32; 3], u8, bool)> {
    let mut r = Reader::new(body);
    let mut pos = [0i32; 3];
    for p in &mut pos {
        *p = i32::from_le_bytes(r.array::<4>()?);
    }
    Ok((pos, r.u8()?, r.bool()?))
}

/// Reads `block_pick_request`: the block position.
pub fn read_block_pick(body: &[u8]) -> Result<[i32; 3]> {
    read_block_pos(&mut Reader::new(body))
}

/// `add_item_actor` from a pre-encoded `ItemInstance`.
pub fn add_item_actor(entity_id: i64, item: &[u8], pos: [f32; 3], vel: [f32; 3]) -> Vec<u8> {
    let mut out = Vec::with_capacity(item.len().saturating_add(48));
    io::write_vari64(&mut out, entity_id);
    io::write_varu64(&mut out, u64::try_from(entity_id).unwrap_or(0));
    out.extend_from_slice(item);
    io::write_vec3(&mut out, pos);
    io::write_vec3(&mut out, vel);
    Metadata::default()
        .long(meta::FLAGS, entity_flags(false, false) & !name_flags())
        .write(&mut out);
    out.push(0); // not from fishing
    out
}

fn name_flags() -> i64 {
    1i64.wrapping_shl(meta::FLAG_ALWAYS_SHOW_NAME) | 1i64.wrapping_shl(meta::FLAG_SHOW_NAME)
}

/// `take_item_actor`.
pub fn take_item_actor(item: u64, taker: u64) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_varu64(&mut out, item);
    io::write_varu64(&mut out, taker);
    out
}

/// `set_actor_data`.
pub fn set_actor_data(runtime_id: u64, metadata: &Metadata) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_varu64(&mut out, runtime_id);
    metadata.write(&mut out);
    out.extend_from_slice(&[0, 0]); // no entity properties
    io::write_varu64(&mut out, 0);
    out
}

/// `set_actor_motion`.
pub fn set_actor_motion(runtime_id: u64, vel: [f32; 3]) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_varu64(&mut out, runtime_id);
    io::write_vec3(&mut out, vel);
    io::write_varu64(&mut out, 0);
    out
}

/// `animate` (1 = swing arm, 4 = critical hit, 3 = wake up).
pub fn animate(action: u8, runtime_id: u64, v: &Version) -> Vec<u8> {
    let mut out = vec![action];
    io::write_varu64(&mut out, runtime_id);
    out.extend_from_slice(&0f32.to_le_bytes());
    out.push(0); // no swing source
    if !v.before_26_60() {
        out.push(0); // main hand
    }
    out
}

/// `actor_event`.
pub fn actor_event(runtime_id: u64, event: u8, data: i32) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_varu64(&mut out, runtime_id);
    out.push(event);
    io::write_vari32(&mut out, data);
    out.push(0);
    out
}

/// `level_event`.
pub fn level_event(event: i32, pos: [f32; 3], data: i32) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_vari32(&mut out, event);
    io::write_vec3(&mut out, pos);
    io::write_vari32(&mut out, data);
    out
}

/// `level_sound_event` without an entity (e.g. `break` with a block runtime id).
pub fn level_sound_event(sound: &str, pos: [f32; 3], data: i32) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_str(&mut out, sound);
    io::write_vec3(&mut out, pos);
    io::write_vari32(&mut out, data);
    io::write_str(&mut out, "");
    out.push(0); // not a baby mob
    out.push(0); // relative volume
    out.extend_from_slice(&(-1i64).to_le_bytes());
    out.push(0); // no fire position
    out
}

/// `play_sound`.
pub fn play_sound(name: &str, pos: [f32; 3], volume: f32, pitch: f32, v: &Version) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_str(&mut out, name);
    let eighths = pos.map(|v| crate::num::floor_i32(f64::from(v) * 8.0));
    io::write_block_pos(&mut out, eighths);
    out.extend_from_slice(&volume.to_le_bytes());
    out.extend_from_slice(&pitch.to_le_bytes());
    io::write_vari32(&mut out, 0); // loop count
    if v.before_26_50() {
        out.push(0); // no handle
    } else {
        out.extend_from_slice(&[0, 0, 0]); // in range only, no handle, no playback position
    }
    out
}

/// `set_time`.
pub fn set_time(time: i32) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_vari32(&mut out, time);
    out
}

/// `game_rules_changed` with one boolean rule.
pub fn game_rule_changed(name: &str, value: bool) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_varu32(&mut out, 1);
    game_rule_bool(&mut out, name, value);
    out
}

/// `set_difficulty`.
pub fn set_difficulty(difficulty: u32) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_varu32(&mut out, difficulty);
    out
}

/// One attribute of `update_attributes`.
#[derive(Debug, Clone, Copy)]
pub struct Attribute<'a> {
    pub name: &'a str,
    pub min: f32,
    pub max: f32,
    pub value: f32,
    pub default: f32,
}

/// `update_attributes`.
pub fn update_attributes(runtime_id: u64, attributes: &[Attribute<'_>]) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_varu64(&mut out, runtime_id);
    io::write_len(&mut out, attributes.len());
    for a in attributes {
        for v in [a.min, a.max, a.value, a.min, a.max, a.default] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        io::write_str(&mut out, a.name);
        io::write_varu32(&mut out, 0);
    }
    io::write_varu64(&mut out, 0);
    out
}

/// `respawn` (state 0 searching, 1 ready, 2 client ready).
pub fn respawn(pos: [f32; 3], state: u8, runtime_id: u64) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_vec3(&mut out, pos);
    out.push(state);
    io::write_varu64(&mut out, runtime_id);
    out
}

/// Reads `respawn` from the client: the state.
pub fn read_respawn_state(body: &[u8]) -> Result<u8> {
    let mut r = Reader::new(body);
    r.vec3()?;
    r.u8()
}

/// `change_dimension`.
pub fn change_dimension(dimension: i32, pos: [f32; 3], respawn: bool) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_vari32(&mut out, dimension);
    io::write_vec3(&mut out, pos);
    out.push(u8::from(respawn));
    out.push(0); // no loading screen id
    out
}

/// `death_info`.
pub fn death_info(cause: &str) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_str(&mut out, cause);
    io::write_varu32(&mut out, 0);
    out
}

/// `set_spawn_position` (type 1: world spawn).
pub fn set_spawn_position(pos: [i32; 3], dimension: i32) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_vari32(&mut out, 1);
    io::write_block_pos(&mut out, pos);
    io::write_vari32(&mut out, dimension);
    io::write_block_pos(&mut out, pos);
    out
}

/// `mob_effect` (operation 1 add, 2 modify, 3 remove).
pub fn mob_effect(
    runtime_id: u64,
    operation: u8,
    effect: i32,
    amplifier: i32,
    particles: bool,
    duration: i32,
) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_varu64(&mut out, runtime_id);
    out.push(operation);
    io::write_vari32(&mut out, effect);
    io::write_vari32(&mut out, amplifier);
    out.push(u8::from(particles));
    io::write_vari32(&mut out, duration);
    io::write_varu64(&mut out, 0);
    out.push(0);
    out
}

/// `set_title` (0 clear, 1 reset, 2 title, 3 subtitle, 4 action bar, 5 durations).
pub fn set_title(action: i32, text: &str, times: [i32; 3]) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_vari32(&mut out, action);
    io::write_str(&mut out, text);
    for t in times {
        io::write_vari32(&mut out, t);
    }
    for _ in 0..3 {
        io::write_str(&mut out, "");
    }
    out
}

/// `boss_event` show (0), hide (2), health (4) or title (5).
pub fn boss_event(
    entity_id: i64,
    event: u8,
    title: &str,
    health: f32,
    colour: u8,
    v: &Version,
) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_vari64(&mut out, entity_id);
    if v.before_26_50() {
        io::write_vari64(&mut out, 0); // player id
    }
    out.push(event);
    io::write_str(&mut out, title);
    io::write_str(&mut out, title);
    out.extend_from_slice(&health.to_le_bytes());
    out.push(colour);
    out.push(0);
    out
}

/// `modal_form_request`.
pub fn modal_form_request(id: u32, json: &str) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_varu32(&mut out, id);
    io::write_str(&mut out, json);
    out
}

/// Reads `modal_form_response`: form id and the response JSON (`None` when closed).
pub fn read_modal_form_response(body: &[u8]) -> Result<(u32, Option<String>)> {
    let mut r = Reader::new(body);
    let id = r.varu32()?;
    let data = if r.bool()? {
        Some(r.string("form response", 64 << 10)?.to_owned())
    } else {
        None
    };
    Ok((id, data))
}

/// `player_skin`.
pub fn player_skin(uuid: u128, skin: &crate::skin::Skin, v: &Version) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_uuid(&mut out, uuid);
    skin.write(&mut out, v);
    io::write_str(&mut out, "");
    io::write_str(&mut out, "");
    out
}

// ---- Utility items ------------------------------------------------------------------------------

/// `player_start_item_cooldown`: a cool-down category and its duration in ticks.
pub fn start_item_cooldown(category: &str, ticks: i32) -> Vec<u8> {
    let mut out = Vec::with_capacity(category.len().saturating_add(8));
    io::write_str(&mut out, category);
    io::write_vari32(&mut out, ticks);
    out
}

/// One map decoration (`MapDecoration`): image, rotation, position, label and colour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapDecoration {
    pub image: u8,
    pub rotation: u8,
    pub x: u8,
    pub y: u8,
    pub label: String,
    /// ARGB.
    pub color: u32,
}

/// What `clientbound_map_item_data` carries besides the map id.
#[derive(Debug, Clone, Default)]
pub struct MapUpdate<'a> {
    pub dimension: u8,
    pub locked: bool,
    pub scale: u8,
    pub decorations: Option<&'a [MapDecoration]>,
    /// Start x, start y, width, height and the ABGR pixels of the rectangle (row by row).
    pub texture: Option<(u8, u8, u8, u8, &'a [u32])>,
}

/// `clientbound_map_item_data` (the optional-field layout of all supported protocols): the
/// map tracks itself (`MapsIncludedIn`, required since 1.19.50); each decoration gets a
/// tracked object as Bedrock needs one to show it.
pub fn map_item_data(map_id: i64, u: &MapUpdate<'_>) -> Vec<u8> {
    let pixels = u.texture.map_or(0, |t| t.4.len());
    let mut out = Vec::with_capacity(pixels.saturating_mul(4).saturating_add(64));
    io::write_vari64(&mut out, map_id);
    out.push(u.dimension);
    out.push(u8::from(u.locked));
    io::write_block_pos(&mut out, [0, 0, 0]);
    out.push(1);
    io::write_varu32(&mut out, 1);
    io::write_vari64(&mut out, map_id);
    out.push(1);
    out.push(u.scale);
    match u.decorations {
        Some(list) => {
            out.push(1);
            io::write_len(&mut out, list.len());
            for i in 0..list.len() {
                // Type entity, the entity id, no block.
                out.extend_from_slice(&0i32.to_le_bytes());
                out.push(1);
                io::write_vari64(&mut out, i64::try_from(i).unwrap_or(0));
                out.push(0);
            }
            out.push(1);
            io::write_len(&mut out, list.len());
            for d in list {
                out.extend_from_slice(&[d.image, d.rotation, d.x, d.y]);
                io::write_str(&mut out, &d.label);
                out.extend_from_slice(&d.color.to_le_bytes());
            }
        }
        None => out.extend_from_slice(&[0, 0]),
    }
    match u.texture {
        Some((x, y, w, h, colors)) => {
            for v in [w, h, x, y] {
                out.push(1);
                io::write_vari32(&mut out, i32::from(v));
            }
            out.push(1);
            io::write_len(&mut out, colors.len());
            for c in colors {
                out.extend_from_slice(&c.to_le_bytes());
            }
        }
        None => out.extend_from_slice(&[0; 5]),
    }
    out
}

/// Reads the map id of `map_info_request` (the client's pixel hints are not needed).
pub fn read_map_info_request(body: &[u8]) -> Result<i64> {
    Reader::new(body).vari64()
}

/// A `book_edit` action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BookAction {
    ReplacePage { page: i32, text: String },
    AddPage { page: i32, text: String },
    DeletePage { page: i32 },
    SwapPages { a: i32, b: i32 },
    Sign { title: String },
}

/// Most bytes of a page or title string from the client (Java pages hold 1024 characters).
const MAX_BOOK_TEXT: usize = 1024 * 4;

/// Reads `book_edit`: the inventory slot and the action.
pub fn read_book_edit(body: &[u8]) -> Result<(i32, BookAction)> {
    let mut r = Reader::new(body);
    let slot = r.vari32()?;
    let action = r.varu32()?;
    let action = match action {
        0 | 1 => {
            let page = r.vari32()?;
            let text = r.string("book page", MAX_BOOK_TEXT)?.to_owned();
            r.string("book photo", MAX_BOOK_TEXT)?;
            if action == 0 {
                BookAction::ReplacePage { page, text }
            } else {
                BookAction::AddPage { page, text }
            }
        }
        2 => BookAction::DeletePage { page: r.vari32()? },
        3 => BookAction::SwapPages {
            a: r.vari32()?,
            b: r.vari32()?,
        },
        4 => {
            let title = r.string("book title", MAX_BOOK_TEXT)?.to_owned();
            r.string("book author", MAX_BOOK_TEXT)?;
            r.string("book xuid", MAX_BOOK_TEXT)?;
            BookAction::Sign { title }
        }
        _ => return Err(io::Error::Invalid("book edit action")),
    };
    Ok((slot, action))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_beacon_and_loom_actions() {
        // Request 1: beacon payment (speed, none), loom craft `cre`; no filter strings.
        let mut body = vec![1, 2, 2];
        body.extend_from_slice(&[8, 0, 2, 0]);
        body.extend_from_slice(&[15, 0, 3, b'c', b'r', b'e', 1]);
        body.extend_from_slice(&[0, 0, 0, 0, 0]);
        let reqs = read_stack_requests(&body, crate::Version::latest()).unwrap_or_default();
        let actions = reqs.first().map(|r| r.actions.clone()).unwrap_or_default();
        assert_eq!(
            actions,
            vec![
                StackAction::BeaconPayment {
                    primary: 1,
                    secondary: 0
                },
                StackAction::CraftLoom { pattern: *b"cre\0" },
            ]
        );
    }
}
