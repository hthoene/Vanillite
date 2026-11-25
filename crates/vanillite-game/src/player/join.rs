//! The packets of joining and leaving (Vanilla's `PlayerList.placeNewPlayer`, `sendLevelInfo`
//! and `remove`).

use vanillite_base::Vec3;
use vanillite_config::GameMode as ConfigGameMode;
use vanillite_protocol::chat::ChatSessionData;
use vanillite_protocol::packets::play::clientbound::{
    ChatSession, GameEvent, GameMode, Login, PlayerAbilities, PlayerInfoEntry, PlayerInfoRemove,
    PlayerInfoUpdate, SetDefaultSpawnPosition, SpawnInfo, info_action,
};
use vanillite_protocol::text::{ClickEvent, Color, Component, HoverEvent, NamedColor};
use vanillite_protocol::types::{
    BlockPos, GameProfile, GlobalPos, Identifier, Property, RawBytes, Uuid,
};

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

/// `ServerPlayer.setGameMode`: changes the game mode and sends abilities, the game event and
/// abilities again; `broadcast_tab_list` runs where Vanilla broadcasts the tab list update.
/// Returns `false` when the player already is in that mode.
pub fn set_game_mode(
    player: &mut PlayerData,
    mode: ConfigGameMode,
    broadcast_tab_list: impl FnOnce(),
) -> bool {
    if player.game_mode == mode {
        return false;
    }
    player.previous_game_mode = Some(player.game_mode);
    player.game_mode = mode;
    let sink = player.connection.sink.as_ref();
    sink.send(&abilities(mode));
    broadcast_tab_list();
    sink.send(&GameEvent {
        event: GameEvent::CHANGE_GAME_MODE,
        param: game_mode_id(mode),
    });
    sink.send(&abilities(mode));
    true
}

/// `GameType.getId` as the game event parameter.
fn game_mode_id(mode: ConfigGameMode) -> f32 {
    match mode {
        ConfigGameMode::Survival => 0.0,
        ConfigGameMode::Creative => 1.0,
        ConfigGameMode::Adventure => 2.0,
        ConfigGameMode::Spectator => 3.0,
    }
}

/// `GameType.getName`, for `gameMode.<name>` translations.
pub fn game_mode_name(mode: ConfigGameMode) -> &'static str {
    match mode {
        ConfigGameMode::Survival => "survival",
        ConfigGameMode::Creative => "creative",
        ConfigGameMode::Adventure => "adventure",
        ConfigGameMode::Spectator => "spectator",
    }
}

/// The first packets of a joining player, up to its abilities.
pub fn send_login(
    sink: &dyn PacketSink,
    level: &LevelInfo,
    entity_id: i32,
    player: &PlayerData,
    enforces_secure_chat: bool,
    show_death_screen: bool,
) {
    sink.send(&Login {
        entity_id,
        hardcore: level.hardcore,
        levels: level.levels.clone(),
        max_players: level.max_players,
        chunk_radius: level.view_distance,
        simulation_distance: level.simulation_distance,
        reduced_debug_info: false,
        show_death_screen,
        do_limited_crafting: false,
        spawn: spawn_info(level, player),
        online_mode: level.online_mode,
        enforces_secure_chat,
    });
}

/// `createCommonSpawnInfo`.
pub fn spawn_info(level: &LevelInfo, player: &PlayerData) -> SpawnInfo {
    SpawnInfo {
        dimension_type: level.dimension_type,
        dimension: level.dimension.clone(),
        hashed_seed: level.hashed_seed,
        game_mode: game_mode(player.game_mode),
        previous_game_mode: player.previous_game_mode.map(game_mode),
        is_debug: false,
        is_flat: level.is_flat,
        last_death_location: player.survival.last_death.as_ref().and_then(|(dim, pos)| {
            Some(GlobalPos {
                dimension: Identifier::new(dim).ok()?,
                pos: BlockPos {
                    x: pos.x,
                    y: pos.y,
                    z: pos.z,
                },
            })
        }),
        portal_cooldown: player.survival.portal.cooldown,
        sea_level: level.sea_level,
    }
}

/// The player's abilities (`ClientboundPlayerAbilitiesPacket`).
pub fn send_abilities(sink: &dyn PacketSink, mode: ConfigGameMode) {
    sink.send(&abilities(mode));
}

/// `sendLevelInfo`: world border, clocks, spawn point, the weather and the request to wait for
/// chunks.
pub fn send_level_info(
    sink: &dyn PacketSink,
    level: &LevelInfo,
    time: &vanillite_protocol::packets::play::clientbound::SetTime,
    weather: &[GameEvent],
) {
    // TODO: `initialize_border` comes with the world border.
    sink.send(time);
    send_spawn_position(sink, level);
    for event in weather {
        sink.send(event);
    }
    sink.send(&GameEvent {
        event: GameEvent::LEVEL_CHUNKS_LOAD_START,
        param: 0.0,
    });
}

/// `ClientboundSetDefaultSpawnPositionPacket` with the server's spawn (`getRespawnData`: in
/// the overworld, the first of the levels).
pub fn send_spawn_position(sink: &dyn PacketSink, level: &LevelInfo) {
    let (spawn, yaw) = level.spawn;
    sink.send(&SetDefaultSpawnPosition {
        pos: GlobalPos {
            dimension: level.levels.first().unwrap_or(&level.dimension).clone(),
            pos: BlockPos {
                x: vanillite_base::math::floor(spawn.x),
                y: vanillite_base::math::floor(spawn.y),
                z: vanillite_base::math::floor(spawn.z),
            },
        },
        yaw,
        pitch: 0.0,
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
    /// The player's chat session, once it sent one.
    pub chat_session: Option<ChatSessionData>,
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
                chat_session: p.chat_session.as_ref().map(|s| ChatSession {
                    session_id: s.session_id,
                    expires_at: s.expires_at,
                    public_key: RawBytes(s.public_key.clone()),
                    key_signature: RawBytes(s.key_signature.clone()),
                }),
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

/// `Player.getDisplayName()` of a player without team: the name, a click that suggests
/// `/tell <name> `, the entity as hover and the name as insertion.
pub fn display_name(name: &str, uuid: u128) -> Component {
    Component::text(name)
        .click(ClickEvent::SuggestCommand(format!("/tell {name} ")))
        .hover(HoverEvent::ShowEntity {
            id: "minecraft:player".into(),
            uuid,
            name: Some(Component::text(name)),
        })
        .insertion(name)
}

fn yellow_message(key: &str, name: &str, uuid: u128) -> Component {
    Component::translatable(key, vec![display_name(name, uuid)])
        .color(Color::Named(NamedColor::Yellow))
}

/// `multiplayer.player.joined`.
pub fn joined_message(name: &str, uuid: u128) -> Component {
    yellow_message("multiplayer.player.joined", name, uuid)
}

/// `multiplayer.player.left`.
pub fn left_message(name: &str, uuid: u128) -> Component {
    yellow_message("multiplayer.player.left", name, uuid)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use vanillite_protocol::packets::play::clientbound::SystemChat;

    /// The join and leave messages encode byte-exact like Vanilla 26.3 (captured when Bob
    /// joined and was kicked).
    #[test]
    fn join_and_leave_messages_match_vanilla() {
        let dir = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../vanillite-protocol/tests/vectors/text/"
        );
        let bob = 0xfaa5dca3_c3d4_354b_ae1b_dde9e5a14b3b;
        for (file, message) in [
            ("joined.bin", super::joined_message("Bob", bob)),
            ("left.bin", super::left_message("Bob", bob)),
        ] {
            let vanilla = std::fs::read(format!("{dir}{file}")).unwrap();
            let body = vanillite_protocol::frame::encode_body(&SystemChat {
                content: message,
                overlay: false,
            });
            assert_eq!(body, vanilla, "{file}");
        }
    }
}
