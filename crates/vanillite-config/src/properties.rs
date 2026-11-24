//! `server.properties`: Vanilla's keys with Vanilla's meaning and defaults, in Java's
//! `.properties` format.
//!
//! Like Vanilla, the file is written back on start with every known key, so admins see all
//! options. Unknown keys are kept.

use std::collections::BTreeMap;
use std::fmt::Write as _;

/// The type of a property's value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropertyKind {
    /// `true` or `false`.
    Bool,
    /// An integer.
    Integer,
    /// Free text.
    Text,
    /// One of these names.
    Enum(&'static [&'static str]),
}

/// Static description of one property, for documentation and generated configuration panels.
#[derive(Clone, Copy, Debug)]
pub struct PropertyInfo {
    /// The key in `server.properties`.
    pub key: &'static str,
    /// The documentation comment, one line per doc line.
    pub doc: &'static str,
    /// The type of the value.
    pub kind: PropertyKind,
    /// The values Vanilla accepts or clamps to (inclusive), if it limits them.
    pub range: Option<(i64, i64)>,
    /// Shown in generated configuration panels (the Pterodactyl egg). `false` for keys a panel
    /// sets itself (address and port) and keys without effect in Vanillite.
    pub panel: bool,
}

/// A type that can be the value of a property.
pub trait PropertyValue: Sized {
    /// The type of the value.
    const KIND: PropertyKind;
    /// Parses the raw value. `None` if it is invalid.
    fn parse(raw: &str) -> Option<Self>;
    /// Renders the value as written to the file.
    fn render(&self) -> String;
}

impl PropertyValue for bool {
    const KIND: PropertyKind = PropertyKind::Bool;
    fn parse(raw: &str) -> Option<Self> {
        // Java's `Boolean.parseBoolean`: everything but "true" is false.
        Some(raw.trim().eq_ignore_ascii_case("true"))
    }
    fn render(&self) -> String {
        self.to_string()
    }
}

macro_rules! number_property_value {
    ($($ty:ty),*) => {$(
        impl PropertyValue for $ty {
            const KIND: PropertyKind = PropertyKind::Integer;
            fn parse(raw: &str) -> Option<Self> {
                raw.trim().parse().ok()
            }
            fn render(&self) -> String {
                self.to_string()
            }
        }
    )*};
}
number_property_value!(u16, i32, i64);

impl PropertyValue for String {
    const KIND: PropertyKind = PropertyKind::Text;
    fn parse(raw: &str) -> Option<Self> {
        Some(raw.to_owned())
    }
    fn render(&self) -> String {
        self.clone()
    }
}

macro_rules! named_enum {
    (
        $(#[doc = $doc:literal])*
        $name:ident { $($(#[doc = $vdoc:literal])* $variant:ident = $text:literal),* $(,)? }
    ) => {
        $(#[doc = $doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name {
            $($(#[doc = $vdoc])* $variant,)*
        }

        impl $name {
            /// All values, in Vanilla's id order.
            pub const ALL: &'static [$name] = &[$($name::$variant),*];

            /// Vanilla's name.
            pub fn name(self) -> &'static str {
                match self {
                    $($name::$variant => $text,)*
                }
            }
        }

        impl PropertyValue for $name {
            const KIND: PropertyKind = PropertyKind::Enum(&[$($text),*]);
            /// Accepts the name or Vanilla's numeric id, like Vanilla.
            fn parse(raw: &str) -> Option<Self> {
                let raw = raw.trim();
                if let Ok(id) = raw.parse::<usize>() {
                    return $name::ALL.get(id).copied();
                }
                $name::ALL.iter().copied().find(|v| v.name().eq_ignore_ascii_case(raw))
            }
            fn render(&self) -> String {
                self.name().to_owned()
            }
        }
    };
}

named_enum! {
    /// A game mode.
    GameMode { Survival = "survival", Creative = "creative", Adventure = "adventure", Spectator = "spectator" }
}

named_enum! {
    /// A difficulty.
    Difficulty { Peaceful = "peaceful", Easy = "easy", Normal = "normal", Hard = "hard" }
}

named_enum! {
    /// Compression of chunks in region files.
    RegionFileCompression { Deflate = "deflate", Lz4 = "lz4", None = "none" }
}

/// Picks a given value or the default. Internal to `properties!`.
macro_rules! pick {
    (; $default:expr) => {
        $default
    };
    ($value:expr ; $default:expr) => {
        $value
    };
}

/// Declares the properties: `field: Type = "key", default[, range = [min, max]][, panel = false];`
macro_rules! properties {
    ($(
        $(#[doc = $doc:literal])*
        $field:ident: $ty:ty = $key:literal, $default:expr
            $(, range = [$min:expr, $max:expr])?
            $(, panel = $panel:literal)?
        ;
    )*) => {
        /// The typed content of `server.properties`.
        #[derive(Clone, Debug, PartialEq)]
        pub struct ServerProperties {
            $( $(#[doc = $doc])* pub $field: $ty, )*
        }

        impl Default for ServerProperties {
            fn default() -> Self {
                Self { $( $field: $default, )* }
            }
        }

        impl ServerProperties {
            /// Every key Vanillite knows, in declaration order.
            pub const KEYS: &'static [&'static str] = &[$($key),*];

            /// Every key Vanillite knows with its description, in declaration order.
            pub const INFO: &'static [PropertyInfo] = &[$(
                PropertyInfo {
                    key: $key,
                    doc: concat!($($doc, "\n",)*),
                    kind: <$ty as PropertyValue>::KIND,
                    range: pick!($(Some(($min, $max)))? ; None),
                    panel: pick!($($panel)? ; true),
                },
            )*];

            /// Reads the typed values from raw key/value pairs. Invalid values fall back to
            /// the default with a warning; missing keys take the default.
            pub fn from_map(map: &BTreeMap<String, String>) -> Self {
                let mut properties = Self::default();
                $(
                    if let Some(raw) = map.get($key) {
                        match <$ty as PropertyValue>::parse(raw) {
                            Some(value) => properties.$field = value,
                            None => log::warn!(
                                "server.properties: invalid value {raw:?} for {}, using {:?}",
                                $key,
                                properties.$field.render()
                            ),
                        }
                    }
                )*
                properties
            }

            /// Writes every known key into `map`, keeping unknown keys.
            pub fn write_to_map(&self, map: &mut BTreeMap<String, String>) {
                $( map.insert($key.to_owned(), self.$field.render()); )*
            }
        }
    };
}

properties! {
    /// Accept transfers of players from other servers.
    accepts_transfers: bool = "accepts-transfers", false;
    /// Allow flight in survival (otherwise flying players are kicked).
    allow_flight: bool = "allow-flight", false;
    /// Send console command output to operators.
    broadcast_console_to_ops: bool = "broadcast-console-to-ops", true;
    /// Send RCON command output to operators.
    broadcast_rcon_to_ops: bool = "broadcast-rcon-to-ops", true;
    /// Seconds of chat spam (each message counts one second, each tick takes 1/20 away) after
    /// which non-operators are kicked; 0 disables.
    chat_spam_threshold_seconds: i32 = "chat-spam-threshold-seconds", 10;
    /// Like `chat-spam-threshold-seconds`, for commands.
    command_spam_threshold_seconds: i32 = "command-spam-threshold-seconds", 10;
    /// Link shown as "report bug" in the client.
    bug_report_link: String = "bug-report-link", String::new();
    /// Difficulty of new worlds.
    difficulty: Difficulty = "difficulty", Difficulty::Easy;
    /// Show the code of conduct to joining players.
    enable_code_of_conduct: bool = "enable-code-of-conduct", false;
    /// Expose JMX beans (not applicable to Vanillite, kept for compatibility).
    enable_jmx_monitoring: bool = "enable-jmx-monitoring", false, panel = false;
    /// Enable the GameSpy4 query listener.
    enable_query: bool = "enable-query", false;
    /// Enable remote console access.
    enable_rcon: bool = "enable-rcon", false;
    /// Answer server list pings.
    enable_status: bool = "enable-status", true;
    /// Require chat signing keys from Mojang.
    enforce_secure_profile: bool = "enforce-secure-profile", true;
    /// Kick players not on the whitelist when it is reloaded.
    enforce_whitelist: bool = "enforce-whitelist", false;
    /// Entity tracking range in percent of the default.
    entity_broadcast_range_percentage: i32 = "entity-broadcast-range-percentage", 100,
        range = [10, 1000];
    /// Force players into the default game mode on join.
    force_gamemode: bool = "force-gamemode", false;
    /// Permission level of functions.
    function_permission_level: i32 = "function-permission-level", 2, range = [1, 4];
    /// Default game mode.
    gamemode: GameMode = "gamemode", GameMode::Survival;
    /// Generate structures in new chunks.
    generate_structures: bool = "generate-structures", true;
    /// Settings of the flat and customised generators (JSON).
    generator_settings: String = "generator-settings", "{}".to_owned();
    /// Hardcore mode.
    hardcore: bool = "hardcore", false;
    /// Hide the player sample in the server list.
    hide_online_players: bool = "hide-online-players", false;
    /// Data packs disabled when the world is created.
    initial_disabled_packs: String = "initial-disabled-packs", String::new();
    /// Data packs enabled when the world is created.
    initial_enabled_packs: String = "initial-enabled-packs", "vanilla".to_owned();
    /// World folder name.
    level_name: String = "level-name", "world".to_owned();
    /// World seed; empty = random.
    level_seed: String = "level-seed", String::new();
    /// World preset of new worlds.
    level_type: String = "level-type", "minecraft:normal".to_owned();
    /// Log client IP addresses.
    log_ips: bool = "log-ips", true;
    /// Maximum chained neighbour updates before skipping.
    max_chained_neighbor_updates: i32 = "max-chained-neighbor-updates", 1_000_000;
    /// Maximum number of players.
    max_players: i32 = "max-players", 20;
    /// Milliseconds a single tick may take before the watchdog stops the server; -1 disables.
    max_tick_time: i64 = "max-tick-time", 60_000;
    /// Maximum world border radius.
    max_world_size: i32 = "max-world-size", 29_999_984, range = [1, 29_999_984];
    /// Message of the day in the server list.
    motd: String = "motd", "A Minecraft Server".to_owned();
    /// Packets larger than this many bytes are compressed; -1 disables compression.
    network_compression_threshold: i32 = "network-compression-threshold", 256;
    /// Authenticate players with Mojang.
    online_mode: bool = "online-mode", true;
    /// Permission level of operators.
    op_permission_level: i32 = "op-permission-level", 4, range = [0, 4];
    /// Seconds without players after which the server stops ticking; 0 disables.
    pause_when_empty_seconds: i32 = "pause-when-empty-seconds", 60;
    /// Minutes of inactivity before a player is kicked; 0 disables.
    player_idle_timeout: i32 = "player-idle-timeout", 0;
    /// Kick players whose IP differs from the one Mojang saw.
    prevent_proxy_connections: bool = "prevent-proxy-connections", false;
    /// Port of the query listener.
    query_port: i32 = "query.port", 25565, range = [1, 65535];
    /// Maximum packets per second before a player is kicked; 0 disables.
    rate_limit: i32 = "rate-limit", 0;
    /// RCON password.
    rcon_password: String = "rcon.password", String::new();
    /// RCON port.
    rcon_port: i32 = "rcon.port", 25575, range = [1, 65535];
    /// Compression of chunks in region files.
    region_file_compression: RegionFileCompression = "region-file-compression", RegionFileCompression::Deflate;
    /// Kick players who decline the resource pack.
    require_resource_pack: bool = "require-resource-pack", false;
    /// Resource pack URL.
    resource_pack: String = "resource-pack", String::new();
    /// Resource pack UUID.
    resource_pack_id: String = "resource-pack-id", String::new();
    /// Resource pack prompt (text component).
    resource_pack_prompt: String = "resource-pack-prompt", String::new();
    /// Resource pack SHA-1.
    resource_pack_sha1: String = "resource-pack-sha1", String::new();
    /// Address to bind to; empty = all.
    server_ip: String = "server-ip", String::new(), panel = false;
    /// Port to listen on.
    server_port: u16 = "server-port", 25565, range = [1, 65535], panel = false;
    /// Simulation distance in chunks.
    simulation_distance: i32 = "simulation-distance", 10, range = [2, 32];
    /// Radius around the spawn only operators may build in.
    spawn_protection: i32 = "spawn-protection", 16;
    /// Seconds between status heartbeats to the management server; 0 disables.
    status_heartbeat_interval: i32 = "status-heartbeat-interval", 0;
    /// Flush region files synchronously.
    sync_chunk_writes: bool = "sync-chunk-writes", true;
    /// Text filtering configuration.
    text_filtering_config: String = "text-filtering-config", String::new(), panel = false;
    /// Text filtering protocol version.
    text_filtering_version: i32 = "text-filtering-version", 0, panel = false;
    /// Use epoll on Linux (Vanillite chooses its own I/O; kept for compatibility).
    use_native_transport: bool = "use-native-transport", true, panel = false;
    /// View distance in chunks.
    view_distance: i32 = "view-distance", 10, range = [2, 32];
    /// Only whitelisted players may join.
    white_list: bool = "white-list", false;
}

/// Parses Java's `.properties` format: `#`/`!` comments, `=`, `:` or whitespace as separator,
/// backslash escapes including `\uXXXX`, and line continuations.
pub fn parse(text: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let mut logical = line.trim_start().to_owned();
        if logical.is_empty() || logical.starts_with('#') || logical.starts_with('!') {
            continue;
        }
        while ends_with_odd_backslashes(&logical) {
            logical.pop();
            match lines.next() {
                Some(next) => logical.push_str(next.trim_start()),
                None => break,
            }
        }
        let (key, value) = split_key_value(&logical);
        map.insert(unescape(key), unescape(value));
    }
    map
}

/// Renders a map in Java's `.properties` format, sorted by key like Vanilla.
pub fn render(map: &BTreeMap<String, String>) -> String {
    let mut out = String::from("#Minecraft server properties\n");
    for (key, value) in map {
        let _ = writeln!(out, "{}={}", escape(key, true), escape(value, false));
    }
    out
}

fn ends_with_odd_backslashes(s: &str) -> bool {
    s.bytes().rev().take_while(|&b| b == b'\\').count() % 2 == 1
}

fn split_key_value(line: &str) -> (&str, &str) {
    let mut escaped = false;
    for (i, c) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' => escaped = true,
            '=' | ':' | ' ' | '\t' | '\u{c}' => {
                let key = &line[..i];
                let rest = line[i..].trim_start_matches([' ', '\t', '\u{c}']);
                let rest = rest
                    .strip_prefix(['=', ':'])
                    .map_or(rest, |r| r.trim_start_matches([' ', '\t', '\u{c}']));
                return (key, rest);
            }
            _ => {}
        }
    }
    (line, "")
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    // `\uXXXX` escapes are UTF-16 code units; surrogate pairs span two escapes.
    let mut utf16 = Vec::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            let next = chars.next();
            if next == Some('u') {
                let hex: String = chars.by_ref().take(4).collect();
                match u16::from_str_radix(&hex, 16) {
                    Ok(unit) => utf16.push(unit),
                    Err(_) => out.push_str(&hex),
                }
                continue;
            }
            out.push_str(&String::from_utf16_lossy(&utf16));
            utf16.clear();
            match next {
                Some('t') => out.push('\t'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('f') => out.push('\u{c}'),
                Some(other) => out.push(other),
                None => {}
            }
            continue;
        }
        out.push_str(&String::from_utf16_lossy(&utf16));
        utf16.clear();
        out.push(c);
    }
    out.push_str(&String::from_utf16_lossy(&utf16));
    out
}

fn escape(s: &str, is_key: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for (i, c) in s.chars().enumerate() {
        match c {
            ' ' if is_key || i == 0 => out.push_str("\\ "),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\u{c}' => out.push_str("\\f"),
            '=' | ':' | '#' | '!' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    let _ = write!(out, "\\u{unit:04X}");
                }
            }
            c => out.push(c),
        }
    }
    out
}
