//! Server administration from `server.properties`: the resource pack, server links, codes of
//! conduct, the server icon, the server list's player sample, RCON and Query.
//!
//! Keys of `server.properties` that Vanillite handles differently on purpose:
//! - `enable-jmx-monitoring`, `use-native-transport`: no effect (no JVM; Vanillite picks its
//!   own I/O).
//! - `text-filtering-config`, `text-filtering-version`: no text filtering service; chat is not
//!   filtered (like Vanilla with an empty config).
//! - `status-heartbeat-interval`: belongs to the management server, which Vanillite does not
//!   have.
//! - `enable-rcon`, `enable-query`: listen on `admin.rcon-bind` / `admin.query-bind` of
//!   `vanillite.toml` (loopback by default, SEC-24) and refuse `rcon.password`s shorter than
//!   `admin.rcon-min-password-length`.

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::Arc;

use vanillite_config::{Config, ServerProperties};
use vanillite_net::ResourcePackInfo;
use vanillite_net::query::{QueryConfig, QueryPlayers};
use vanillite_net::rcon::{RconConfig, RconRunner};
use vanillite_protocol::packets::common::{ServerLink, ServerLinkLabel};
use vanillite_protocol::text::Component;
use vanillite_protocol::types::Uuid;

use crate::global::Global;
use crate::world::WorldId;

/// `DedicatedServerProperties.getServerPackInfo`: the pack of `resource-pack`, if set.
pub(crate) fn resource_pack(p: &ServerProperties) -> Option<ResourcePackInfo> {
    let url = p.resource_pack.clone();
    if url.is_empty() {
        return None;
    }
    let mut hash = p.resource_pack_sha1.clone();
    if hash.is_empty() {
        log::warn!(
            "You specified a resource pack without providing a sha1 hash. Pack will be updated on the client only if you change the name of the pack."
        );
    } else if hash.len() != 40 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        log::warn!("Invalid sha1 for resource-pack-sha1");
        if hash.len() > 40 {
            // The packet holds 40 characters; Vanilla fails every join with a longer one.
            hash.clear();
        }
    }
    let prompt = parse_prompt(&p.resource_pack_prompt);
    let id = if p.resource_pack_id.is_empty() {
        let id = Uuid::from_name_bytes(url.as_bytes());
        log::warn!("resource-pack-id missing, using default of {id}");
        id
    } else {
        match Uuid::parse(&p.resource_pack_id) {
            Some(id) => id,
            None => {
                log::warn!("Failed to parse '{}' into UUID", p.resource_pack_id);
                return None;
            }
        }
    };
    Some(ResourcePackInfo {
        id,
        url,
        hash,
        required: p.require_resource_pack,
        prompt,
    })
}

/// `parseResourcePackPrompt`: a JSON text component, `None` when empty or invalid.
fn parse_prompt(prompt: &str) -> Option<Component> {
    if prompt.is_empty() {
        return None;
    }
    match serde_json::from_str::<serde_json::Value>(prompt) {
        Ok(v) => Some(Component::from_json(&v)),
        Err(e) => {
            log::warn!("Failed to parse resource pack prompt '{prompt}': {e}");
            None
        }
    }
}

/// `DedicatedServer.createServerLinks`: the bug report link, if it is an http(s) URI.
pub(crate) fn server_links(p: &ServerProperties) -> Vec<ServerLink> {
    let link = &p.bug_report_link;
    if link.is_empty() {
        return Vec::new();
    }
    // `Util.parseAndValidateUntrustedUri`.
    let scheme = link.split_once(':').map(|(s, _)| s.to_ascii_lowercase());
    let valid = matches!(scheme.as_deref(), Some("http" | "https"))
        && !link.chars().any(|c| c.is_whitespace() || c.is_control());
    if !valid {
        log::warn!("Failed to parse bug link {link}");
        return Vec::new();
    }
    vec![ServerLink {
        label: ServerLinkLabel::Known(ServerLink::BUG_REPORT),
        url: link.clone(),
    }]
}

/// `DedicatedServer.readCodeOfConducts`: `codeofconduct/<language>.txt`; the folder must exist.
pub(crate) fn codes_of_conduct(dir: &Path) -> Result<BTreeMap<String, String>, String> {
    let folder = dir.join("codeofconduct");
    let entries = std::fs::read_dir(&folder).map_err(|_| {
        format!(
            "Code of Conduct folder does not exist: {}",
            folder.display()
        )
    })?;
    let mut codes = BTreeMap::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(language) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".txt"))
        else {
            continue;
        };
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
        codes.insert(language.to_lowercase(), text);
    }
    Ok(codes)
}

/// `MinecraftServer.loadStatusIcon`: `server-icon.png`, else the world's `icon.png`, as a data
/// URL; it must be a 64x64 PNG.
pub(crate) fn server_icon(dir: &Path, level_name: &str) -> Option<String> {
    let path = [
        dir.join("server-icon.png"),
        dir.join(level_name).join("icon.png"),
    ]
    .into_iter()
    .find(|p| p.is_file())?;
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) => {
            log::error!("Couldn't load server icon: {e}");
            return None;
        }
    };
    match png_size(&bytes) {
        Some((64, 64)) => Some(format!("data:image/png;base64,{}", base64(&bytes))),
        Some((w, h)) => {
            log::error!(
                "Couldn't load server icon: Invalid world icon size [{w}, {h}], but expected [64, 64]"
            );
            None
        }
        None => {
            log::error!("Couldn't load server icon: not a PNG file");
            None
        }
    }
}

/// Width and height from a PNG's `IHDR` (`PngInfo.fromBytes`).
fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let (signature, rest) = bytes.split_first_chunk::<8>()?;
    if *signature != SIGNATURE || rest.get(4..8)? != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(*rest.get(8..12)?.first_chunk::<4>()?);
    let height = u32::from_be_bytes(*rest.get(12..16)?.first_chunk::<4>()?);
    Some((width, height))
}

/// Standard base64 with padding.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3).saturating_mul(4));
    for chunk in bytes.chunks(3) {
        let b = [
            chunk.first().copied().unwrap_or(0),
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for i in 0..4usize {
            if i <= chunk.len() {
                let index = usize::try_from((n >> (18 - 6 * i)) & 63).unwrap_or(0);
                out.push(char::from(ALPHABET.get(index).copied().unwrap_or(b'A')));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// `buildPlayerStatus`: up to 12 players from a random offset, shuffled; players who do not
/// allow listing appear as `Anonymous Player` with the nil UUID.
pub(crate) fn player_sample(
    players: &[(String, u128, bool)],
    random: &mut impl FnMut(usize) -> usize,
) -> Vec<(String, Uuid)> {
    let size = players.len().min(12);
    let offset = random(players.len().saturating_sub(size).saturating_add(1));
    let mut sample: Vec<(String, Uuid)> = players
        .iter()
        .skip(offset)
        .take(size)
        .map(|(name, uuid, listed)| {
            if *listed {
                (name.clone(), Uuid(*uuid))
            } else {
                ("Anonymous Player".to_owned(), Uuid(0))
            }
        })
        .collect();
    // `Util.shuffle`: Fisher-Yates from the end.
    for i in (1..sample.len()).rev() {
        let j = random(i.saturating_add(1));
        sample.swap(i, j);
    }
    sample
}

/// The address an admin listener binds to: `bind`, or `server-ip` like Vanilla when empty.
fn admin_bind(bind: &str, server_ip: &str, port: i32, what: &str) -> Option<SocketAddr> {
    let ip = if bind.is_empty() { server_ip } else { bind };
    let ip: IpAddr = if ip.is_empty() {
        IpAddr::from([0, 0, 0, 0])
    } else {
        match ip.parse() {
            Ok(ip) => ip,
            Err(_) => {
                log::warn!("Invalid address {ip:?} for {what}; it stays off");
                return None;
            }
        }
    };
    match u16::try_from(port) {
        Ok(port) if port > 0 => Some(SocketAddr::new(ip, port)),
        _ => {
            log::warn!("Invalid {what} port {port} found in server.properties ({what} disabled)");
            None
        }
    }
}

/// `RconThread.create`: starts RCON when `enable-rcon` is on and the password is accepted.
pub(crate) fn start_rcon(
    config: &Config,
    network: &vanillite_net::Network,
    handle: crate::server::ServerHandle,
    global: &Arc<Global>,
) {
    let (p, admin) = (&config.properties, &config.server.admin);
    if !p.enable_rcon {
        return;
    }
    let Some(bind) = admin_bind(&admin.rcon_bind, &p.server_ip, p.rcon_port, "rcon") else {
        return;
    };
    if p.rcon_password.is_empty() {
        log::warn!("No rcon password set in server.properties, rcon disabled!");
        return;
    }
    let min = usize::try_from(admin.rcon_min_password_length).unwrap_or(usize::MAX);
    if p.rcon_password.chars().count() < min {
        log::warn!(
            "rcon.password is shorter than admin.rcon-min-password-length ({min}), rcon disabled!"
        );
        return;
    }
    let weak = Arc::downgrade(global);
    let runner: RconRunner = Arc::new(move |line, reply| {
        let weak = weak.clone();
        handle.run_in_world(WorldId(0), move |w| {
            let output = weak
                .upgrade()
                .map(|g| crate::commands::run_rcon_command(w, &g, &line))
                .unwrap_or_default();
            reply(output);
        });
    });
    let rcon = RconConfig {
        bind,
        password: p.rcon_password.clone(),
        max_connections: usize::try_from(admin.rcon_max_connections).unwrap_or(1),
        max_failed_logins: admin.rcon_max_failed_logins,
        log_ips: p.log_ips,
    };
    if let Err(e) = network.start_rcon(rcon, runner) {
        log::warn!("Unable to initialise RCON on {bind}: {e}");
    }
}

/// `QueryThreadGs4.create`: starts Query when `enable-query` is on.
pub(crate) fn start_query(
    config: &Config,
    network: &vanillite_net::Network,
    players: Arc<QueryPlayers>,
) {
    let (p, admin) = (&config.properties, &config.server.admin);
    if !p.enable_query {
        return;
    }
    let Some(bind) = admin_bind(&admin.query_bind, &p.server_ip, p.query_port, "query") else {
        return;
    };
    let host_ip = if p.server_ip.is_empty() {
        local_ip().unwrap_or_else(|| {
            log::warn!(
                "Unable to determine local host IP, please set server-ip in server.properties"
            );
            "0.0.0.0".to_owned()
        })
    } else {
        p.server_ip.clone()
    };
    let query = QueryConfig {
        bind,
        motd: p.motd.clone(),
        level_name: p.level_name.clone(),
        max_players: p.max_players,
        server_port: p.server_port,
        host_ip,
        max_packets_per_second: admin.query_max_packets_per_second,
    };
    if let Err(e) = network.start_query(query, players) {
        log::warn!("Unable to initialise query system on {bind}: {e}");
    }
}

/// The address of this host on its default route (`InetAddress.getLocalHost`); a UDP
/// `connect` only selects the route, nothing is sent.
fn local_ip() -> Option<String> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    Some(socket.local_addr().ok()?.ip().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_pack_like_vanilla() {
        let mut p = ServerProperties::default();
        assert_eq!(resource_pack(&p), None);
        p.resource_pack = "https://example.com/pack.zip".into();
        p.resource_pack_sha1 = "0123456789abcdef0123456789abcdef01234567".into();
        p.require_resource_pack = true;
        p.resource_pack_prompt = r#"{"text":"Please"}"#.into();
        let pack = resource_pack(&p).unwrap_or_else(|| panic!("pack"));
        assert_eq!(
            pack.id,
            Uuid::from_name_bytes(b"https://example.com/pack.zip")
        );
        assert!(pack.required);
        assert_eq!(pack.prompt.map(|c| c.plain()), Some("Please".to_owned()));
        p.resource_pack_id = "not a uuid".into();
        assert_eq!(resource_pack(&p), None);
    }

    #[test]
    fn bug_report_link_needs_http() {
        let mut p = ServerProperties {
            bug_report_link: "https://example.com/bugs".into(),
            ..ServerProperties::default()
        };
        assert_eq!(server_links(&p).len(), 1);
        p.bug_report_link = "javascript:alert(1)".into();
        assert!(server_links(&p).is_empty());
    }

    #[test]
    fn encodes_base64_and_reads_png_size() {
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
        assert_eq!(base64(b"M"), "TQ==");
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13];
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&64u32.to_be_bytes());
        png.extend_from_slice(&64u32.to_be_bytes());
        assert_eq!(png_size(&png), Some((64, 64)));
        assert_eq!(png_size(b"GIF89a"), None);
    }

    #[test]
    fn sample_hides_unlisted_players() {
        let players: Vec<(String, u128, bool)> = (0..20u128)
            .map(|i| (format!("P{i}"), i + 1, i != 3))
            .collect();
        let mut zero = |_: usize| 0;
        let sample = player_sample(&players, &mut zero);
        assert_eq!(sample.len(), 12);
        assert!(sample.contains(&("Anonymous Player".to_owned(), Uuid(0))));
    }
}
