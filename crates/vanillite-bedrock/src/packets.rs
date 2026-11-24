//! Bedrock packet ids and the encoders and decoders the frontend needs (protocol 2225,
//! Bedrock 1.26.60). Field layouts follow the protocol as documented by the gophertunnel project
//! (MIT); the code is written for Vanillite.

use crate::io::{self, Reader, Result};

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
    pub const CURSOR: u8 = 59;
    pub const CREATED_OUTPUT: u8 = 60;
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
    pub const START_JUMPING: u32 = 31;
    pub const HANDLED_TELEPORT: u32 = 37;
    pub const START_FLYING: u32 = 42;
    pub const STOP_FLYING: u32 = 43;
    pub const HORIZONTAL_COLLISION: u32 = 49;
    pub const VERTICAL_COLLISION: u32 = 50;
    /// Number of flags of this protocol version.
    pub const COUNT: u32 = 66;
}

/// Entity metadata keys and flags used here.
pub mod meta {
    pub const FLAGS: u32 = 0;
    pub const NAME: u32 = 4;
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
pub fn start_game(p: &StartGame<'_>) -> Vec<u8> {
    let mut out = Vec::with_capacity(crate::data::BLOCK_PROPERTIES.len().saturating_add(512));
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
    out.push(0); // editor level migration version
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
    out.extend_from_slice(crate::data::BLOCK_PROPERTIES);
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
pub fn add_player(p: &AddPlayer<'_>) -> Vec<u8> {
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
    out.push(0); // not a passenger of a block
    io::write_str(&mut out, ""); // device id
    out.extend_from_slice(&0i32.to_le_bytes()); // build platform
    out
}

/// `add_actor` for a non-player entity with its metadata.
#[allow(clippy::too_many_arguments)]
pub fn add_actor(
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
    out.push(0); // not a passenger of a block
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
pub fn player_list_add(entries: &[ListEntry<'_>]) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_len(&mut out, entries.len());
    for e in entries {
        io::write_varu32(&mut out, 1);
        out.push(0);
        io::write_uuid(&mut out, e.uuid);
        io::write_vari64(&mut out, e.entity_id);
        io::write_str(&mut out, e.name);
        io::write_str(&mut out, e.xuid);
        io::write_str(&mut out, ""); // PlayFab id
        io::write_str(&mut out, ""); // platform chat id
        out.extend_from_slice(&0i32.to_le_bytes()); // build platform
        e.skin.write(&mut out);
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
pub fn read_auth_input(body: &[u8]) -> Result<AuthInput> {
    let mut r = Reader::new(body);
    let pitch = r.f32()?;
    let yaw = r.f32()?;
    let position = r.vec3()?;
    let _move = [r.f32()?, r.f32()?];
    let head_yaw = r.f32()?;
    let n = r.varu32()?;
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
    for v in position.iter().chain(&delta).chain(&[pitch, yaw, head_yaw]) {
        if !v.is_finite() {
            return Err(io::Error::Invalid("non-finite movement"));
        }
    }
    Ok(AuthInput {
        pitch,
        yaw,
        position,
        head_yaw,
        flags,
        tick,
        delta,
    })
}

/// Writes the leading fields of `player_auth_input` (test client) with empty optional parts.
pub fn write_auth_input(i: &AuthInput) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&i.pitch.to_le_bytes());
    out.extend_from_slice(&i.yaw.to_le_bytes());
    io::write_vec3(&mut out, i.position);
    out.extend_from_slice(&[0; 8]);
    out.extend_from_slice(&i.head_yaw.to_le_bytes());
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
    out.extend_from_slice(&[0, 0, 0, 0, 0]);
    out.extend_from_slice(&[0; 8 + 12 + 8]);
    out
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
    /// Tool durability prediction of block breaking (nothing to do).
    MineBlock,
    /// Anything the frontend does not translate (lab table, beacon, loom, ...).
    Unsupported,
}

/// One item stack request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackRequest {
    pub id: i32,
    pub actions: Vec<StackAction>,
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

fn read_stack_action(r: &mut Reader<'_>) -> Result<StackAction> {
    let variant = r.varu32()?;
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
        8 => {
            r.vari32()?;
            r.vari32()?;
            StackAction::Unsupported
        }
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
            StackAction::CraftRecipe { net_id, times }
        }
        12 => StackAction::CraftCreative {
            net_id: r.varu32()?,
            times: r.u8()?,
        },
        13 => {
            r.varu32()?;
            r.array::<4>()?;
            StackAction::Unsupported
        }
        14 => {
            r.array::<4>()?;
            r.u8()?;
            r.vari32()?;
            StackAction::Unsupported
        }
        15 => {
            r.string("loom pattern", 256)?;
            r.u8()?;
            StackAction::Unsupported
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
            StackAction::Unsupported
        }
        _ => return Err(io::Error::Invalid("stack request action")),
    })
}

/// Reads one `ItemStackRequest` (also embedded in `player_auth_input`).
pub fn read_stack_request(r: &mut Reader<'_>) -> Result<StackRequest> {
    let id = r.vari32()?;
    let n = r.len("stack actions", MAX_STACK_ACTIONS)?;
    let mut actions = Vec::with_capacity(n);
    for _ in 0..n {
        actions.push(read_stack_action(r)?);
    }
    let strings = r.len("filter strings", 64)?;
    for _ in 0..strings {
        r.string("filter string", 1024)?;
    }
    r.array::<4>()?;
    Ok(StackRequest { id, actions })
}

/// Reads `item_stack_request`.
pub fn read_stack_requests(body: &[u8]) -> Result<Vec<StackRequest>> {
    let mut r = Reader::new(body);
    let n = r.len("stack requests", 64)?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(read_stack_request(&mut r)?);
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
pub fn item_stack_response(responses: &[StackResponse]) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_len(&mut out, responses.len());
    for resp in responses {
        out.push(if resp.containers.is_some() { 0 } else { 1 });
        io::write_vari32(&mut out, resp.id);
        match &resp.containers {
            Some(containers) if !containers.is_empty() => {
                out.push(1);
                io::write_len(&mut out, containers.len());
                for (container, slots) in containers {
                    out.push(*container);
                    out.push(0); // not dynamic
                    io::write_len(&mut out, slots.len());
                    for s in slots {
                        out.push(s.slot);
                        out.push(s.slot);
                        out.push(s.count);
                        if s.net_id > 0 {
                            out.push(1);
                            io::write_vari32(&mut out, s.net_id);
                        } else {
                            out.push(0);
                        }
                        io::write_str(&mut out, "");
                        out.push(0);
                        io::write_vari32(&mut out, s.durability);
                    }
                }
            }
            _ => out.push(0),
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

fn read_inventory_actions(r: &mut Reader<'_>) -> Result<()> {
    let n = r.len("inventory actions", 64)?;
    for _ in 0..n {
        r.varu32()?;
        if r.bool()? {
            r.u8()?;
        }
        if r.bool()? {
            r.varu32()?;
        }
        r.varu32()?;
        crate::items::read_instance(r)?;
        crate::items::read_instance(r)?;
    }
    Ok(())
}

fn read_use_item(r: &mut Reader<'_>) -> Result<Transaction> {
    let action = u32::try_from(r.vari32()?).map_err(|_| io::Error::Invalid("use item action"))?;
    let _trigger = r.u8()?;
    let pos = read_block_pos(r)?;
    let face = i32::from(r.u8()?);
    let hotbar = r.vari32()?;
    let _hand = r.u8()?;
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
pub fn read_inventory_transaction(body: &[u8]) -> Result<Transaction> {
    let mut r = Reader::new(body);
    read_legacy_slots(&mut r)?;
    let kind = r.varu32()?;
    read_inventory_actions(&mut r)?;
    Ok(match kind {
        2 => read_use_item(&mut r)?,
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
}

/// Reads the optional parts of `player_auth_input` after the movement fields.
pub fn read_auth_input_extra(body: &[u8]) -> Result<AuthInputExtra> {
    let mut r = Reader::new(body);
    // Skip the fixed fields read by `read_auth_input`.
    r.take(4 * 2 + 12 + 8 + 4)?;
    let n = r.len("input flags", usize::try_from(input::COUNT).unwrap_or(0))?;
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
    if r.bool()? {
        read_legacy_slots(&mut r)?;
        read_inventory_actions(&mut r)?;
        extra.item_interaction = Some(read_use_item(&mut r)?);
    }
    if r.bool()? {
        extra.stack_request = Some(read_stack_request(&mut r)?);
    }
    if r.bool()? {
        let n = r.len("block actions", 64)?;
        for _ in 0..n {
            let action = r.vari32()?;
            let pos = read_block_pos(&mut r)?;
            let face = r.vari32()?;
            extra.block_actions.push(BlockAction { action, pos, face });
        }
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

/// Reads `interact`: action and target runtime id.
pub fn read_interact(body: &[u8]) -> Result<(u8, u64)> {
    let mut r = Reader::new(body);
    Ok((r.u8()?, r.varu64()?))
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
pub fn animate(action: u8, runtime_id: u64) -> Vec<u8> {
    let mut out = vec![action];
    io::write_varu64(&mut out, runtime_id);
    out.extend_from_slice(&0f32.to_le_bytes());
    out.push(0); // no swing source
    out.push(0); // main hand
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
pub fn play_sound(name: &str, pos: [f32; 3], volume: f32, pitch: f32) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_str(&mut out, name);
    let eighths = pos.map(|v| crate::num::floor_i32(f64::from(v) * 8.0));
    io::write_block_pos(&mut out, eighths);
    out.extend_from_slice(&volume.to_le_bytes());
    out.extend_from_slice(&pitch.to_le_bytes());
    io::write_vari32(&mut out, 0);
    out.push(0);
    out.push(0);
    out.push(0);
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
pub fn boss_event(entity_id: i64, event: u8, title: &str, health: f32, colour: u8) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_vari64(&mut out, entity_id);
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
pub fn player_skin(uuid: u128, skin: &crate::skin::Skin) -> Vec<u8> {
    let mut out = Vec::new();
    io::write_uuid(&mut out, uuid);
    skin.write(&mut out);
    io::write_str(&mut out, "");
    io::write_str(&mut out, "");
    out
}
