//! Development server for the network pipeline: status, login and configuration with the
//! Vanilla data pack, then it only keeps joined connections open.
//!
//! `cargo run -p vanillite-net --example dev_server -- [port] [offline] [cache-dir]`

use std::time::Duration;
use vanillite_net::{ConfigurationData, NetConfig, Network, StatusInfo};
use vanillite_protocol::packets::play::clientbound::Disconnect;
use vanillite_protocol::text::{Color, Component, NamedColor};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::new()
        .filter_level(log::LevelFilter::Debug)
        .init();
    let args: Vec<String> = std::env::args().collect();
    let port: u16 = args.get(1).and_then(|p| p.parse().ok()).unwrap_or(30001);
    let offline = args.get(2).is_some_and(|a| a == "offline");
    let cache = std::path::PathBuf::from(args.get(3).map_or("vanillite-cache", String::as_str));

    let pack = vanillite_datapack::vanilla::ensure_vanilla_pack(&cache, None)?;
    let packs = vanillite_datapack::load(&[pack.as_path()])?;
    let configuration = ConfigurationData::from_packs(&packs, &["minecraft:vanilla"]);

    let config = NetConfig {
        bind: ([0, 0, 0, 0], port).into(),
        online_mode: !offline,
        ..NetConfig::default()
    };
    let status = StatusInfo {
        description: Component::text("Vanillite").color(Color::Named(NamedColor::Gold)),
        ..StatusInfo::default()
    };
    let (network, joins) = Network::start(config, status, configuration, None)?;
    log::info!("dev server ready on {}", network.local_addr());
    let mut players = Vec::new();
    loop {
        while let Ok(j) = joins.try_recv() {
            log::info!("{} entered play ({:?})", j.profile.name, j.client_info);
            // Play is not implemented here: confirm the finished configuration and disconnect.
            j.outbound.send(&Disconnect {
                reason: Component::text(
                    "Vanillite: login and configuration complete. Play is not implemented yet.",
                ),
            });
            j.outbound.close();
            players.push(j);
        }
        players.retain(|p| {
            loop {
                match p.inbound.try_recv() {
                    Ok(_) => {}
                    Err(std::sync::mpsc::TryRecvError::Empty) => break true,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => break false,
                }
            }
        });
        std::thread::sleep(Duration::from_millis(50));
    }
}
