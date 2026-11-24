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

    /// A byte entry.
    pub fn byte(mut self, key: u32, v: u8) -> Self {
        self.entry(key, 0);
        self.data.push(v);
        self
    }

    fn write(&self, out: &mut Vec<u8>) {
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
    out.push(0); // server authoritative inventory
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

/// `creative_content` without groups and items.
pub fn creative_content() -> Vec<u8> {
    vec![0, 0]
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

/// `add_actor` for a non-player entity.
pub fn add_actor(
    entity_id: i64,
    kind: &str,
    pos: [f32; 3],
    vel: [f32; 3],
    pitch: f32,
    yaw: f32,
    head_yaw: f32,
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
    let mut flags = entity_flags(false, false);
    flags &=
        !(1i64.wrapping_shl(meta::FLAG_ALWAYS_SHOW_NAME) | 1i64.wrapping_shl(meta::FLAG_SHOW_NAME));
    Metadata::default()
        .long(meta::FLAGS, flags)
        .float(meta::SCALE, 1.0)
        .write(&mut out);
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

/// `available_commands`: each command takes free text.
pub fn available_commands(names: &[String]) -> Vec<u8> {
    let mut out = Vec::new();
    for _ in 0..3 {
        io::write_varu32(&mut out, 0); // enum values, chained values, suffixes
    }
    io::write_varu32(&mut out, 0); // enums
    io::write_varu32(&mut out, 0); // chained subcommands
    io::write_len(&mut out, names.len());
    for name in names {
        io::write_str(&mut out, name);
        io::write_str(&mut out, "");
        out.extend_from_slice(&0u16.to_le_bytes());
        io::write_str(&mut out, "any");
        out.extend_from_slice(&u32::MAX.to_le_bytes()); // no aliases
        io::write_varu32(&mut out, 0); // chained subcommand offsets
        io::write_varu32(&mut out, 1); // overloads
        out.push(0);
        io::write_varu32(&mut out, 1);
        io::write_str(&mut out, "args");
        out.extend_from_slice(&(0x10_0000u32 | RAW_TEXT).to_le_bytes());
        out.push(1); // optional
        out.push(0);
    }
    io::write_varu32(&mut out, 0); // dynamic enums
    io::write_varu32(&mut out, 0); // constraints
    out
}

/// Parameter type "raw text" (rest of the line).
const RAW_TEXT: u32 = 70;

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
