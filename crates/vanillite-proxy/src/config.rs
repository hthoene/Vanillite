//! `proxy.toml`: typed settings with documented defaults and checked ranges. Unknown keys are
//! errors, so a typo never silently falls back to a default.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use toml::{Table, Value};

/// The file written on first start.
pub const DEFAULT_TOML: &str = r#"# vanillite-proxy configuration.

# Address the proxy listens on.
bind = "0.0.0.0:25577"
# Authenticate players with Mojang's session server and encrypt (strongly recommended).
online-mode = true
# Server list text and the maximum player count shown there.
motd = "A Vanillite proxy"
max-players = 500
# Compression threshold towards clients in bytes (-1 = off) and libdeflate level (1-12).
compression-threshold = 256
compression-level = 4
# Offer Vanillite backends the trusted link: they compress for the client's threshold and the
# proxy passes their packets on without inflating and deflating them. Other backends (Paper)
# ignore the offer. Needs the forwarding secret.
trusted-link = true
# I/O threads (0 = number of cores).
io-threads = 0
# File with the secret shared with the backends (Velocity modern forwarding). Created with a
# random secret if missing. The environment variable VANILLITE_PROXY_SECRET overrides it.
forwarding-secret-file = "forwarding.secret"
# Servers tried in this order on join and as fallback when a server is lost.
try = ["lobby"]
# Clients of older Minecraft versions accepted through the multi-version layer: "all" (every
# version this build translates, 1.20.5 to 26.2), "none", or a comma-separated list of releases
# such as "1.21.4, 26.2". Their packets are translated here, so backends only see the current
# version.
older-versions = "all"

# Backend servers: name = "host:port". Backends must use modern forwarding with the same
# secret (Paper: proxies.velocity; Vanillite: proxy.velocity-secret in vanillite.toml).
[servers]
lobby = "127.0.0.1:25565"

# Host names players connect with, mapped to the servers tried first.
[forced-hosts]
# "lobby.example.com" = ["lobby"]

# Who may use the proxy's commands: "*" = everybody, otherwise player names or UUIDs.
# Commands not listed here are open to everybody.
[permissions]
server = ["*"]
glist = ["*"]

[limits]
# Open connections in total.
max-connections = 4096
# Connections per IP address that are not yet playing.
max-pending-per-ip = 8
# Minimum time between two logins from the same IP address in milliseconds (0 = off).
login-interval-ms = 3000
# Concurrent session-server requests.
max-concurrent-auth = 16
# Seconds a client may take for handshake, status and login.
login-timeout = 30
# Seconds without any packet before a connection is closed.
read-timeout = 30
# Seconds to connect and log in to a backend.
connect-timeout = 5

# Bedrock Edition players (vanillite-bedrock): a RakNet listener on UDP that translates Bedrock
# clients into proxy connections. Backends get the Bedrock identity by forwarding.
[bedrock]
enabled = false
# UDP address. Bedrock's default port is 19132.
bind = "0.0.0.0:19132"
# Require an Xbox Live login. Off accepts any identity a client claims: only for tests.
online-mode = true
# Prefix of Bedrock players' Java names, so they never collide with Java accounts.
name-prefix = "."
# Oldest Bedrock network protocol that may join (0: every version the proxy supports, listed at
# startup). E.g. 2225 admits only Bedrock 1.26.60 and newer.
min-protocol = 0
# Bedrock connections per IP address.
max-connections-per-ip = 8
# Packets per second a Bedrock client may send before it is kicked.
max-packets-per-second = 1000
# Largest chunk radius granted to Bedrock clients.
max-view-distance = 12
# Translated chunk sections kept for all Bedrock players (about 4 KiB each).
chunk-cache-sections = 16384
# Threads of the Bedrock frontend.
threads = 2
"#;

/// Proxy settings.
#[derive(Debug, Clone)]
pub struct ProxyConfig {
    /// Listen address. Default `0.0.0.0:25577`.
    pub bind: SocketAddr,
    /// Authenticate with the session server and encrypt. Default true.
    pub online_mode: bool,
    /// Server list text. Default "A Vanillite proxy".
    pub motd: String,
    /// Maximum player count shown in the server list. Default 500.
    pub max_players: i32,
    /// Compression threshold towards clients (-1 = off). Default 256.
    pub compression_threshold: i32,
    /// libdeflate level (1..=12). Default 4.
    pub compression_level: i32,
    /// Offer Vanillite backends the trusted, pass-through link. Default true.
    pub trusted_link: bool,
    /// I/O threads; 0 = cores. Default 0.
    pub io_threads: usize,
    /// Forwarding secret file. Default `forwarding.secret`.
    pub forwarding_secret_file: String,
    /// The forwarding secret (loaded from the file or the environment).
    pub secret: Option<vanillite_net::velocity::Secret>,
    /// Try order.
    pub try_order: Vec<String>,
    /// Backends by name.
    pub servers: BTreeMap<String, String>,
    /// Forced hosts: lower-case host name to servers.
    pub forced_hosts: BTreeMap<String, Vec<String>>,
    /// Command name to allowed players (`*`, names or UUIDs, lower case).
    pub permissions: BTreeMap<String, Vec<String>>,
    /// Limits.
    pub limits: Limits,
    /// Bedrock frontend.
    pub bedrock: Bedrock,
    /// Older client versions accepted (`older-versions`). Default all supported.
    pub multiversion: vanillite_multiversion::Accepted,
}

/// The Bedrock frontend (`[bedrock]`).
#[derive(Debug, Clone)]
pub struct Bedrock {
    /// Accept Bedrock players. Default false.
    pub enabled: bool,
    /// UDP listen address. Default `0.0.0.0:19132`.
    pub bind: SocketAddr,
    /// Require an Xbox Live login. Default true.
    pub online_mode: bool,
    /// Prefix of Bedrock players' Java names. Default ".".
    pub name_prefix: String,
    /// Oldest accepted Bedrock protocol (0..=100000, 0 = all supported). Default 0.
    pub min_protocol: i32,
    /// Connections per IP address (1..=1000). Default 8.
    pub max_connections_per_ip: u32,
    /// Packets per second per client (50..=100000). Default 1000.
    pub max_packets_per_second: u32,
    /// Largest chunk radius (2..=32). Default 12.
    pub max_view_distance: u8,
    /// Translated sections cached (0..=1048576). Default 16384.
    pub chunk_cache_sections: usize,
    /// Frontend threads (1..=64). Default 2.
    pub threads: usize,
}

impl Default for Bedrock {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: SocketAddr::from(([0, 0, 0, 0], 19132)),
            online_mode: true,
            name_prefix: ".".to_owned(),
            min_protocol: 0,
            max_connections_per_ip: 8,
            max_packets_per_second: 1000,
            max_view_distance: 12,
            chunk_cache_sections: 16_384,
            threads: 2,
        }
    }
}

impl Bedrock {
    fn parse(t: &mut Table) -> Result<Self, String> {
        let d = Self::default();
        let bind = string(t, "bind", "0.0.0.0:19132")?;
        let bind = bind
            .parse()
            .map_err(|_| format!("bedrock.bind must be ip:port, got {bind:?}"))?;
        let b = Self {
            enabled: boolean(t, "enabled", d.enabled)?,
            bind,
            online_mode: boolean(t, "online-mode", d.online_mode)?,
            name_prefix: string(t, "name-prefix", &d.name_prefix)?,
            min_protocol: i32::try_from(int(t, "min-protocol", 0, 0, 100_000)?).unwrap_or(0),
            max_connections_per_ip: u32::try_from(int(t, "max-connections-per-ip", 8, 1, 1000)?)
                .unwrap_or(8),
            max_packets_per_second: u32::try_from(int(
                t,
                "max-packets-per-second",
                1000,
                50,
                100_000,
            )?)
            .unwrap_or(1000),
            max_view_distance: u8::try_from(int(t, "max-view-distance", 12, 2, 32)?).unwrap_or(12),
            chunk_cache_sections: usize_of(int(t, "chunk-cache-sections", 16_384, 0, 1_048_576)?),
            threads: usize_of(int(t, "threads", 2, 1, 64)?),
        };
        no_unknown(t, "bedrock")?;
        Ok(b)
    }
}

/// Limits against abuse (SEC-17, SEC-18).
#[derive(Debug, Clone)]
pub struct Limits {
    /// Open connections in total. Default 4096.
    pub max_connections: usize,
    /// Pending connections per IP. Default 8.
    pub max_pending_per_ip: u32,
    /// Minimum time between logins from one IP. Default 3 s.
    pub login_interval: Duration,
    /// Concurrent session-server requests. Default 16.
    pub max_concurrent_auth: usize,
    /// Time for handshake, status and login. Default 30 s.
    pub login_timeout: Duration,
    /// Read timeout. Default 30 s.
    pub read_timeout: Duration,
    /// Backend connect and login timeout. Default 5 s.
    pub connect_timeout: Duration,
}

/// The proxy's own commands.
pub const COMMANDS: [&str; 2] = ["server", "glist"];

impl Default for ProxyConfig {
    fn default() -> Self {
        let mut permissions = BTreeMap::new();
        for c in COMMANDS {
            permissions.insert(c.to_owned(), vec!["*".to_owned()]);
        }
        Self {
            bind: SocketAddr::from(([0, 0, 0, 0], 25577)),
            online_mode: true,
            motd: "A Vanillite proxy".to_owned(),
            max_players: 500,
            compression_threshold: 256,
            compression_level: 4,
            trusted_link: true,
            io_threads: 0,
            forwarding_secret_file: "forwarding.secret".to_owned(),
            secret: None,
            try_order: vec!["lobby".to_owned()],
            servers: BTreeMap::from([("lobby".to_owned(), "127.0.0.1:25565".to_owned())]),
            forced_hosts: BTreeMap::new(),
            permissions,
            limits: Limits {
                max_connections: 4096,
                max_pending_per_ip: 8,
                login_interval: Duration::from_secs(3),
                max_concurrent_auth: 16,
                login_timeout: Duration::from_secs(30),
                read_timeout: Duration::from_secs(30),
                connect_timeout: Duration::from_secs(5),
            },
            bedrock: Bedrock::default(),
            multiversion: vanillite_multiversion::Accepted::all(),
        }
    }
}

fn int(table: &mut Table, key: &str, default: i64, min: i64, max: i64) -> Result<i64, String> {
    match table.remove(key) {
        None => Ok(default),
        Some(Value::Integer(v)) if (min..=max).contains(&v) => Ok(v),
        Some(v) => Err(format!(
            "{key} must be an integer in {min}..={max}, got {v}"
        )),
    }
}

fn string(table: &mut Table, key: &str, default: &str) -> Result<String, String> {
    match table.remove(key) {
        None => Ok(default.to_owned()),
        Some(Value::String(s)) => Ok(s),
        Some(v) => Err(format!("{key} must be a string, got {v}")),
    }
}

fn boolean(table: &mut Table, key: &str, default: bool) -> Result<bool, String> {
    match table.remove(key) {
        None => Ok(default),
        Some(Value::Boolean(b)) => Ok(b),
        Some(v) => Err(format!("{key} must be true or false, got {v}")),
    }
}

fn strings(value: Value, key: &str) -> Result<Vec<String>, String> {
    let Value::Array(items) = value else {
        return Err(format!("{key} must be a list of strings"));
    };
    items
        .into_iter()
        .map(|v| match v {
            Value::String(s) => Ok(s),
            _ => Err(format!("{key} must be a list of strings")),
        })
        .collect()
}

fn table(table: &mut Table, key: &str) -> Result<Table, String> {
    match table.remove(key) {
        None => Ok(Table::new()),
        Some(Value::Table(t)) => Ok(t),
        Some(_) => Err(format!("[{key}] must be a table")),
    }
}

fn no_unknown(table: &Table, section: &str) -> Result<(), String> {
    match table.keys().next() {
        Some(k) if section.is_empty() => Err(format!("unknown key {k}")),
        Some(k) => Err(format!("unknown key {section}.{k}")),
        None => Ok(()),
    }
}

fn usize_of(v: i64) -> usize {
    usize::try_from(v).unwrap_or(0)
}

fn secs(v: i64) -> Duration {
    Duration::from_secs(u64::try_from(v).unwrap_or(0))
}

impl ProxyConfig {
    /// Parses a configuration file's text.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut t: Table = text.parse().map_err(|e| format!("{e}"))?;
        let d = Self::default();
        let bind = string(&mut t, "bind", "0.0.0.0:25577")?;
        let bind = bind
            .parse()
            .map_err(|_| format!("bind must be ip:port, got {bind:?}"))?;
        let try_order = match t.remove("try") {
            None => d.try_order,
            Some(v) => strings(v, "try")?,
        };
        let mut servers = BTreeMap::new();
        for (name, addr) in table(&mut t, "servers")? {
            let Value::String(addr) = addr else {
                return Err(format!("servers.{name} must be \"host:port\""));
            };
            if addr
                .rsplit_once(':')
                .is_none_or(|(_, p)| p.parse::<u16>().is_err())
            {
                return Err(format!(
                    "servers.{name} must be \"host:port\", got {addr:?}"
                ));
            }
            servers.insert(name, addr);
        }
        let mut forced_hosts = BTreeMap::new();
        for (host, list) in table(&mut t, "forced-hosts")? {
            forced_hosts.insert(
                host.to_ascii_lowercase(),
                strings(list, &format!("forced-hosts.{host}"))?,
            );
        }
        // Commands not listed keep their default (everybody).
        let mut permissions = d.permissions;
        for (command, list) in table(&mut t, "permissions")? {
            if !COMMANDS.contains(&command.as_str()) {
                return Err(format!(
                    "unknown command permissions.{command} (known: {})",
                    COMMANDS.join(", ")
                ));
            }
            let list = strings(list, &format!("permissions.{command}"))?;
            permissions.insert(
                command,
                list.iter().map(|s| s.to_ascii_lowercase()).collect(),
            );
        }
        let mut l = table(&mut t, "limits")?;
        let limits = Limits {
            max_connections: usize_of(int(&mut l, "max-connections", 4096, 1, 1_000_000)?),
            max_pending_per_ip: u32::try_from(int(&mut l, "max-pending-per-ip", 8, 1, 100_000)?)
                .unwrap_or(8),
            login_interval: Duration::from_millis(
                u64::try_from(int(&mut l, "login-interval-ms", 3000, 0, 600_000)?).unwrap_or(0),
            ),
            max_concurrent_auth: usize_of(int(&mut l, "max-concurrent-auth", 16, 1, 1024)?),
            login_timeout: secs(int(&mut l, "login-timeout", 30, 1, 600)?),
            read_timeout: secs(int(&mut l, "read-timeout", 30, 5, 600)?),
            connect_timeout: secs(int(&mut l, "connect-timeout", 5, 1, 120)?),
        };
        no_unknown(&l, "limits")?;
        let bedrock = Bedrock::parse(&mut table(&mut t, "bedrock")?)?;
        let multiversion =
            vanillite_multiversion::Accepted::parse(&string(&mut t, "older-versions", "all")?)
                .map_err(|e| format!("older-versions: {e}"))?;
        let config = Self {
            bind,
            online_mode: boolean(&mut t, "online-mode", true)?,
            motd: string(&mut t, "motd", &d.motd)?,
            max_players: i32::try_from(int(&mut t, "max-players", 500, 0, 1_000_000)?)
                .unwrap_or(500),
            compression_threshold: i32::try_from(int(
                &mut t,
                "compression-threshold",
                256,
                -1,
                1_048_576,
            )?)
            .unwrap_or(256),
            compression_level: i32::try_from(int(&mut t, "compression-level", 4, 1, 12)?)
                .unwrap_or(4),
            trusted_link: boolean(&mut t, "trusted-link", true)?,
            io_threads: usize_of(int(&mut t, "io-threads", 0, 0, 1024)?),
            forwarding_secret_file: string(&mut t, "forwarding-secret-file", "forwarding.secret")?,
            secret: None,
            try_order,
            servers,
            forced_hosts,
            permissions,
            limits,
            bedrock,
            multiversion,
        };
        no_unknown(&t, "")?;
        config.check()?;
        Ok(config)
    }

    /// Every server named in the try order and forced hosts exists.
    fn check(&self) -> Result<(), String> {
        let named = self.try_order.iter().map(|s| ("try", s)).chain(
            self.forced_hosts
                .values()
                .flatten()
                .map(|s| ("forced-hosts", s)),
        );
        for (what, name) in named {
            if !self.servers.contains_key(name) {
                return Err(format!("{what} names the unknown server {name:?}"));
            }
        }
        Ok(())
    }

    /// Loads `proxy.toml` from `dir` (writing the default file first if missing) and the
    /// forwarding secret (creating a random one if the file is missing).
    pub fn load(dir: &Path) -> Result<Self, String> {
        let path = dir.join("proxy.toml");
        if !path.exists() {
            std::fs::write(&path, DEFAULT_TOML)
                .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
            log::info!("Wrote the default configuration to {}", path.display());
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let mut config = Self::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let secret = match std::env::var("VANILLITE_PROXY_SECRET") {
            Ok(s) if !s.is_empty() => s,
            _ => {
                let file = dir.join(&config.forwarding_secret_file);
                if !file.exists() {
                    let random: [u8; 12] = vanillite_net::rand_bytes();
                    let s: String = random
                        .iter()
                        .map(|b| {
                            char::from(
                                b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"
                                    .get(usize::from(*b) % 62)
                                    .copied()
                                    .unwrap_or(b'a'),
                            )
                        })
                        .collect();
                    std::fs::write(&file, &s)
                        .map_err(|e| format!("cannot write {}: {e}", file.display()))?;
                    log::info!("Created a new forwarding secret in {}", file.display());
                }
                std::fs::read_to_string(&file)
                    .map_err(|e| format!("cannot read {}: {e}", file.display()))?
                    .trim()
                    .to_owned()
            }
        };
        config.secret = vanillite_net::velocity::Secret::new(secret.as_bytes());
        if config.secret.is_none() {
            return Err("the forwarding secret is empty".to_owned());
        }
        Ok(config)
    }

    /// Whether `name`/`uuid` may use `command`.
    pub fn permitted(&self, command: &str, name: &str, uuid: &str) -> bool {
        self.permissions.get(command).is_some_and(|list| {
            list.iter()
                .any(|p| p == "*" || p.eq_ignore_ascii_case(name) || p.eq_ignore_ascii_case(uuid))
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    #[test]
    fn default_file_parses() {
        let c = super::ProxyConfig::parse(super::DEFAULT_TOML).unwrap();
        assert_eq!(c.try_order, ["lobby"]);
        assert!(c.permitted("server", "x", "y"));
        assert!(super::ProxyConfig::parse("tyop = 1").is_err());
    }
}
