//! The packets of joining and leaving (Vanilla's `PlayerList.placeNewPlayer`, `sendLevelInfo`
//! and `remove`).

use vanillite_base::Vec3;
use vanillite_config::GameMode as ConfigGameMode;
use vanillite_protocol::packets::play::clientbound::{
    GameEvent, GameMode, Login, PlayerAbilities, PlayerInfoEntry, PlayerInfoRemove,
    PlayerInfoUpdate, SetDefaultSpawnPosition, SpawnInfo, SystemChat, info_action,
};
use vanillite_protocol::text::{Color, Component, NamedColor};
use vanillite_protocol::types::{BlockPos, GameProfile, GlobalPos, Identifier, Property, Uuid};

use super::sink::PacketSink;
use super::{GameProfile as Profile, PlayerData};

/// Skin part bit of the hat layer (`PlayerModelPart.HAT`).
const HAT: u8 = 0x40;

/// What the join packets need to know about the server and the player's world.
#[derive(Clone, Debug)]
pub struct LevelInfo {
    /// All dimensions of the server.
    pub levels: Vec<Identifier>,
    /// The world's dimension.
    pub dimension: Identifier,
    /// Network id of the world's dimension type.
    pub dimension_type: i32,
    /// First 8 bytes of the SHA-256 of the seed (`BiomeManager.obfuscateSeed`).
    pub hashed_seed: i64,
    /// Superflat world.
    pub is_flat: bool,
    /// Sea level.
    pub sea_level: i32,
    /// The world's spawn point and yaw.
    pub spawn: (Vec3, f32),
    /// `max-players`.
    pub max_players: i32,
    /// The world's view distance.
    pub view_distance: i32,
    /// The world's simulation distance.
    pub simulation_distance: i32,
    /// `hardcore`.
    pub hardcore: bool,
    /// Online mode.
    pub online_mode: bool,
}

/// The protocol game mode.
pub fn game_mode(mode: ConfigGameMode) -> GameMode {
    match mode {
        ConfigGameMode::Survival => GameMode::Survival,
        ConfigGameMode::Creative => GameMode::Creative,
        ConfigGameMode::Adventure => GameMode::Adventure,
        ConfigGameMode::Spectator => GameMode::Spectator,
    }
}

/// `ClientboundPlayerAbilitiesPacket` for a game mode's default abilities
/// (`GameType.updatePlayerAbilities`).
fn abilities(mode: ConfigGameMode) -> PlayerAbilities {
    let flags = match mode {
        // invulnerable | may fly | instabuild
        ConfigGameMode::Creative => 1 | 4 | 8,
        // invulnerable | flying | may fly
        ConfigGameMode::Spectator => 1 | 2 | 4,
        ConfigGameMode::Survival | ConfigGameMode::Adventure => 0,
    };
    PlayerAbilities {
        flags,
        flying_speed: 0.05,
        walking_speed: 0.1,
    }
}

/// The first packets of a joining player, up to its abilities.
pub fn send_login(sink: &dyn PacketSink, level: &LevelInfo, entity_id: i32, player: &PlayerData) {
    sink.send(&Login {
        entity_id,
        hardcore: level.hardcore,
        levels: level.levels.clone(),
        max_players: level.max_players,
        chunk_radius: level.view_distance,
        simulation_distance: level.simulation_distance,
        reduced_debug_info: false,
        show_death_screen: true,
        do_limited_crafting: false,
        spawn: SpawnInfo {
            dimension_type: level.dimension_type,
            dimension: level.dimension.clone(),
            hashed_seed: level.hashed_seed,
            game_mode: game_mode(player.game_mode),
            previous_game_mode: player.previous_game_mode.map(game_mode),
            is_debug: false,
            is_flat: level.is_flat,
            last_death_location: None,
            portal_cooldown: 0,
            sea_level: level.sea_level,
        },
        online_mode: level.online_mode,
        enforces_secure_chat: false,
    });
    sink.send(&abilities(player.game_mode));
}

/// `sendLevelInfo`: spawn point and the request to wait for chunks.
pub fn send_level_info(sink: &dyn PacketSink, level: &LevelInfo) {
    let (spawn, yaw) = level.spawn;
    sink.send(&SetDefaultSpawnPosition {
        pos: GlobalPos {
            dimension: level.dimension.clone(),
            pos: BlockPos {
                x: vanillite_base::math::floor(spawn.x),
                y: vanillite_base::math::floor(spawn.y),
                z: vanillite_base::math::floor(spawn.z),
            },
        },
        yaw,
        pitch: 0.0,
    });
    sink.send(&GameEvent {
        event: GameEvent::LEVEL_CHUNKS_LOAD_START,
        param: 0.0,
    });
}

/// One tab list entry of an online player.
#[derive(Clone, Debug)]
pub struct InfoEntry {
    /// UUID.
    pub uuid: u128,
    /// Name and skin.
    pub profile: Profile,
    /// Game mode.
    pub game_mode: ConfigGameMode,
    /// Latency in milliseconds.
    pub latency: u32,
    /// Displayed skin parts.
    pub model_customisation: u8,
}

/// `ClientboundPlayerInfoUpdatePacket.createPlayerInitializing`.
pub fn player_info_add<'a>(players: impl IntoIterator<Item = &'a InfoEntry>) -> PlayerInfoUpdate {
    use info_action::*;
    PlayerInfoUpdate {
        actions: ADD_PLAYER
            | INITIALIZE_CHAT
            | UPDATE_GAME_MODE
            | UPDATE_LISTED
            | UPDATE_LATENCY
            | UPDATE_DISPLAY_NAME
            | UPDATE_LIST_ORDER
            | UPDATE_HAT,
        entries: players
            .into_iter()
            .map(|p| PlayerInfoEntry {
                id: Uuid(p.uuid),
                profile: Some(GameProfile {
                    id: Uuid(p.uuid),
                    name: p.profile.name.clone(),
                    properties: p
                        .profile
                        .properties
                        .iter()
                        .map(|q| Property {
                            name: q.name.clone(),
                            value: q.value.clone(),
                            signature: q.signature.clone(),
                        })
                        .collect(),
                }),
                chat_session: None,
                game_mode: game_mode(p.game_mode),
                listed: true,
                latency: i32::try_from(p.latency).unwrap_or(i32::MAX),
                display_name: None,
                list_order: 0,
                show_hat: p.model_customisation & HAT != 0,
            })
            .collect(),
    }
}

/// `ClientboundPlayerInfoRemovePacket`.
pub fn player_info_remove(uuid: u128) -> PlayerInfoRemove {
    PlayerInfoRemove {
        ids: vec![Uuid(uuid)],
    }
}

fn yellow_message(key: &str, name: &str) -> SystemChat {
    SystemChat {
        content: Component::translatable(key, vec![Component::text(name)])
            .color(Color::Named(NamedColor::Yellow)),
        overlay: false,
    }
}

/// `multiplayer.player.joined`.
pub fn joined_message(name: &str) -> SystemChat {
    yellow_message("multiplayer.player.joined", name)
}

/// `multiplayer.player.left`.
pub fn left_message(name: &str) -> SystemChat {
    yellow_message("multiplayer.player.left", name)
}
