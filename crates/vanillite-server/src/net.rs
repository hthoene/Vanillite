//! The network as the server sees it: data packs for the configuration phase, the built-in
//! network pipeline, and joined connections entering the game.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use vanillite_config::Config;
use vanillite_datapack::LoadedPacks;
use vanillite_game::flat::{self, FlatWorld};
use vanillite_game::player::join::LevelInfo;
use vanillite_game::player::{ClientInformation, GameProfile, PacketSink, ProfileProperty};
use vanillite_net::{ConfigurationData, Joined, NetConfig, Network, StatusInfo};
use vanillite_protocol::text::Component;
use vanillite_protocol::types::Identifier;
use vanillite_storage::player_data::PlayerDataStore;

use crate::players::{Arrival, NetSink, WorldPlayers};
use crate::server::{DIMENSIONS, StartError};

/// Where the Vanilla data pack comes from.
#[derive(Clone, Debug, Default)]
pub enum VanillaPack {
    /// Downloaded once from Mojang into `<server dir>/cache` (default).
    #[default]
    Download,
    /// Extracted from this local copy of the pinned server jar.
    Jar(PathBuf),
    /// No data packs: empty registries. Only for tests whose clients do not need them.
    None,
}

/// Data packs and what the worlds derive from them.
pub(crate) struct Prepared {
    pub(crate) configuration: ConfigurationData,
    pub(crate) worlds: Vec<WorldPlayers>,
}

fn registry_index(packs: Option<&LoadedPacks>, registry: &str, entry: &str) -> usize {
    packs
        .and_then(|p| p.registry(registry))
        .and_then(|r| r.id_of(entry))
        .unwrap_or(0)
}

/// Loads the data packs and prepares each world's players' side.
pub(crate) fn prepare(
    config: &Config,
    dir: &Path,
    level_dir: &Path,
    pack: &VanillaPack,
) -> Result<Prepared, StartError> {
    let packs = match pack {
        VanillaPack::None => None,
        VanillaPack::Download | VanillaPack::Jar(_) => {
            let jar = match pack {
                VanillaPack::Jar(jar) => Some(jar.as_path()),
                _ => None,
            };
            let cache = dir.join("cache");
            let root = vanillite_datapack::vanilla::ensure_vanilla_pack(&cache, jar)
                .map_err(|e| StartError::Io(format!("Vanilla data pack: {e}")))?;
            let packs = vanillite_datapack::load(&[root.as_path()])
                .map_err(|e| StartError::Io(format!("data packs: {e}")))?;
            Some(packs)
        }
    };
    let configuration = match &packs {
        Some(packs) => ConfigurationData::from_packs(packs, &["minecraft:vanilla"]),
        None => ConfigurationData::new(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vanillite_protocol::packets::configuration::clientbound::UpdateTags {
                registries: Vec::new(),
            },
        ),
    };
    let biomes = packs
        .as_ref()
        .and_then(|p| p.registry("minecraft:worldgen/biome"))
        .map_or(1, |r| r.entries().len());
    flat::install_chunk_facts(u32::try_from(biomes).unwrap_or(1).max(1));
    let plains = registry_index(
        packs.as_ref(),
        "minecraft:worldgen/biome",
        "minecraft:plains",
    );
    let flat_world = Arc::new(FlatWorld::new(u16::try_from(plains).unwrap_or(0)));

    let properties = &config.properties;
    let levels: Vec<Identifier> = DIMENSIONS
        .iter()
        .map(|d| Identifier::vanilla(d.trim_start_matches("minecraft:")))
        .collect();
    let worlds = DIMENSIONS
        .iter()
        .zip(&levels)
        .map(|(dimension, id)| {
            let rules = config.world_rules(dimension);
            let view_distance = rules
                .distances
                .view_distance
                .or(u8::try_from(properties.view_distance.clamp(2, 32)).unwrap_or(10));
            let simulation_distance = rules
                .distances
                .simulation_distance
                .or(u8::try_from(properties.simulation_distance.clamp(2, 32)).unwrap_or(10));
            let level = LevelInfo {
                levels: levels.clone(),
                dimension: id.clone(),
                dimension_type: i32::try_from(registry_index(
                    packs.as_ref(),
                    "minecraft:dimension_type",
                    dimension,
                ))
                .unwrap_or(0),
                // The seed belongs to the world runtime; the flat world has none.
                hashed_seed: 0,
                is_flat: true,
                sea_level: 63,
                spawn: (flat_world.spawn(), 0.0),
                max_players: properties.max_players,
                view_distance: i32::from(view_distance),
                simulation_distance: i32::from(simulation_distance),
                hardcore: properties.hardcore,
                online_mode: properties.online_mode,
            };
            WorldPlayers::new(
                level,
                Box::new(SharedFlat(Arc::clone(&flat_world))),
                PlayerDataStore::new(level_dir),
            )
        })
        .collect();
    Ok(Prepared {
        configuration,
        worlds,
    })
}

/// The placeholder flat world, shared by all dimensions.
struct SharedFlat(Arc<FlatWorld>);

impl vanillite_game::player::ChunkSource for SharedFlat {
    fn chunk_packet(
        &self,
        pos: vanillite_base::ChunkPos,
    ) -> Option<Arc<vanillite_protocol::SharedPacket>> {
        self.0.chunk_packet(pos)
    }
}

/// The running network.
pub(crate) struct NetState {
    pub(crate) network: Network,
    joins: Receiver<Joined>,
    status: StatusInfo,
    online: usize,
}

impl NetState {
    /// Starts the network pipeline with the settings of `server.properties`.
    /// Without `listen` it binds to a free port of the loopback interface only.
    pub(crate) fn start(
        config: &Config,
        configuration: ConfigurationData,
        listen: bool,
    ) -> Result<NetState, StartError> {
        let p = &config.properties;
        let ip: std::net::IpAddr = if p.server_ip.is_empty() {
            std::net::Ipv4Addr::UNSPECIFIED.into()
        } else {
            p.server_ip
                .parse()
                .map_err(|_| StartError::Io(format!("invalid server-ip {:?}", p.server_ip)))?
        };
        let bind = if listen {
            (ip, p.server_port).into()
        } else {
            std::net::SocketAddr::from(([127, 0, 0, 1], 0))
        };
        let net = NetConfig {
            bind,
            online_mode: p.online_mode,
            prevent_proxy_connections: p.prevent_proxy_connections,
            compression_threshold: p.network_compression_threshold,
            accepts_transfers: p.accepts_transfers,
            // Room for every allowed player plus logins in progress.
            max_connections: usize::try_from(p.max_players)
                .unwrap_or(0)
                .saturating_add(256)
                .max(NetConfig::default().max_connections),
            ..NetConfig::default()
        };
        let status = StatusInfo {
            description: Component::text(p.motd.clone()),
            max_players: p.max_players,
            ..StatusInfo::default()
        };
        let (network, joins) = Network::start(net, status.clone(), configuration, None)
            .map_err(|e| StartError::Io(format!("cannot listen on {ip}:{}: {e}", p.server_port)))?;
        Ok(NetState {
            network,
            joins,
            status,
            online: 0,
        })
    }

    /// The next joined connection.
    pub(crate) fn next_join(&self) -> Option<Joined> {
        self.joins.try_recv().ok()
    }

    /// Updates the player count of the server list.
    pub(crate) fn set_online(&mut self, online: usize) {
        if online != self.online {
            self.online = online;
            self.status.online_players = i32::try_from(online).unwrap_or(i32::MAX);
            self.network.status().update(&self.status);
        }
    }
}

/// Turns a joined connection into an arrival for a world.
pub(crate) fn arrival(
    joined: Joined,
    saved: Option<vanillite_storage::player_data::PlayerData>,
    default_mode: vanillite_config::GameMode,
    force_game_mode: bool,
) -> (Arrival, Arc<dyn PacketSink>) {
    let sink: Arc<dyn PacketSink> = Arc::new(NetSink::new(joined.outbound, joined.latency));
    let saved_mode = saved.as_ref().and_then(|s| match s.game_mode {
        0 => Some(vanillite_config::GameMode::Survival),
        1 => Some(vanillite_config::GameMode::Creative),
        2 => Some(vanillite_config::GameMode::Adventure),
        3 => Some(vanillite_config::GameMode::Spectator),
        _ => None,
    });
    let game_mode = if force_game_mode {
        default_mode
    } else {
        saved_mode.unwrap_or(default_mode)
    };
    let profile = GameProfile {
        name: joined.profile.name.clone(),
        properties: joined
            .profile
            .properties
            .iter()
            .map(|p| ProfileProperty {
                name: p.name.clone(),
                value: p.value.clone(),
                signature: p.signature.clone(),
            })
            .collect(),
    };
    let arrival = Arrival {
        uuid: joined.profile.id.0,
        profile,
        client: ClientInformation::from(&joined.client_info),
        game_mode,
        sink: Arc::clone(&sink),
        inbound: joined.inbound,
        memory_connection: joined.is_virtual,
        saved,
        address: joined.address.to_string(),
    };
    (arrival, sink)
}
