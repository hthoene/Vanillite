//! Operators, the whitelist and the ban lists: Vanilla's `ops.json`, `whitelist.json`,
//! `banned-players.json` (keyed by UUID, SEC-13) and `banned-ips.json`, and the `white-list`
//! and `player-idle-timeout` settings of `server.properties`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::player_list::Uuid;

/// File of the operators.
pub const OPS_FILE: &str = "ops.json";
/// File of the whitelist.
pub const WHITELIST_FILE: &str = "whitelist.json";
/// File of the banned players (`PlayerList.USERBANLIST_FILE`).
pub const BANNED_PLAYERS_FILE: &str = "banned-players.json";
/// File of the banned IP addresses (`PlayerList.IPBANLIST_FILE`).
pub const BANNED_IPS_FILE: &str = "banned-ips.json";

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// What every ban has (`BanListEntry`): dates as written (`yyyy-MM-dd HH:mm:ss Z`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ban {
    /// `created`.
    pub created: String,
    /// `source`: who banned (`(Unknown)` when missing).
    pub source: String,
    /// `expires`; `None` is `forever`.
    pub expires: Option<String>,
    /// `reason`.
    pub reason: Option<String>,
}

impl Ban {
    /// A ban from now on, forever.
    pub fn now(source: &str, reason: Option<String>) -> Ban {
        let (y, mo, d, h, mi, s) = utc(unix_now());
        Ban {
            created: format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02} +0000"),
            source: source.to_owned(),
            expires: None,
            reason,
        }
    }

    /// `hasExpired`.
    fn expired(&self) -> bool {
        self.expires
            .as_deref()
            .and_then(parse_date)
            .is_some_and(|t| t < unix_now())
    }

    /// The expiry in `PlayerList.BAN_DATE_FORMAT` (`yyyy-MM-dd 'at' HH:mm:ss z`), if any.
    pub fn expiry_text(&self) -> Option<String> {
        let (y, mo, d, h, mi, s) = utc(self.expires.as_deref().and_then(parse_date)?);
        Some(format!(
            "{y:04}-{mo:02}-{d:02} at {h:02}:{mi:02}:{s:02} UTC"
        ))
    }

    fn to_json(&self, out: &mut serde_json::Map<String, serde_json::Value>) {
        out.insert("created".into(), self.created.clone().into());
        out.insert("source".into(), self.source.clone().into());
        out.insert(
            "expires".into(),
            self.expires
                .clone()
                .unwrap_or_else(|| "forever".into())
                .into(),
        );
        if let Some(reason) = &self.reason {
            out.insert("reason".into(), reason.clone().into());
        }
    }

    fn from_json(v: &serde_json::Value) -> Ban {
        let text = |key: &str| v.get(key).and_then(serde_json::Value::as_str);
        let created = text("created")
            .filter(|t| parse_date(t).is_some())
            .map_or_else(|| Ban::now("", None).created, str::to_owned);
        Ban {
            created,
            source: text("source").unwrap_or("(Unknown)").to_owned(),
            expires: text("expires")
                .filter(|t| parse_date(t).is_some())
                .map(str::to_owned),
            reason: text("reason").map(str::to_owned),
        }
    }
}

/// One banned player (`UserBanListEntry`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserBan {
    /// UUID.
    pub uuid: Uuid,
    /// Name when banned.
    pub name: String,
    /// The ban.
    pub ban: Ban,
}

/// One banned IP address (`IpBanListEntry`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IpBan {
    /// The address.
    pub ip: String,
    /// The ban.
    pub ban: Ban,
}

/// One operator (`ServerOpListEntry`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpEntry {
    /// UUID.
    pub uuid: Uuid,
    /// Last known name.
    pub name: String,
    /// Permission level 1..=4.
    pub level: u8,
    /// May join a full server.
    pub bypasses_player_limit: bool,
}

/// One whitelisted player (`UserWhiteListEntry`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WhitelistEntry {
    /// UUID.
    pub uuid: Uuid,
    /// Last known name.
    pub name: String,
}

#[derive(Debug, Default)]
struct Lists {
    ops: Vec<OpEntry>,
    whitelist: Vec<WhitelistEntry>,
    whitelist_enabled: bool,
    bans: Vec<UserBan>,
    ip_bans: Vec<IpBan>,
}

/// Operators and whitelist. Cold data behind a mutex; every change is written at once like
/// Vanilla.
#[derive(Debug)]
pub struct Access {
    dir: PathBuf,
    lists: Mutex<Lists>,
    /// `op-permission-level`: the level of new operators.
    pub op_permission_level: u8,
    /// `enforce-whitelist`: kick players who are not whitelisted when the list changes.
    pub enforce_whitelist: bool,
    /// `player-idle-timeout` in minutes (0 = off).
    idle_timeout: AtomicI32,
}

impl Access {
    /// Loads both files from `dir` (missing files are empty lists).
    pub fn load(
        dir: &Path,
        whitelist_enabled: bool,
        op_permission_level: i32,
        enforce_whitelist: bool,
    ) -> Access {
        let lists = Lists {
            ops: read_list(&dir.join(OPS_FILE), parse_op),
            whitelist: read_list(&dir.join(WHITELIST_FILE), parse_whitelisted),
            whitelist_enabled,
            bans: read_list(&dir.join(BANNED_PLAYERS_FILE), parse_user_ban),
            ip_bans: read_list(&dir.join(BANNED_IPS_FILE), parse_ip_ban),
        };
        let idle_timeout = std::fs::read_to_string(dir.join(vanillite_config::PROPERTIES_FILE))
            .ok()
            .and_then(|t| {
                vanillite_config::properties::parse(&t)
                    .get("player-idle-timeout")
                    .and_then(|v| v.trim().parse().ok())
            })
            .unwrap_or(0);
        Access {
            dir: dir.to_owned(),
            lists: Mutex::new(lists),
            op_permission_level: u8::try_from(op_permission_level.clamp(1, 4)).unwrap_or(4),
            enforce_whitelist,
            idle_timeout: AtomicI32::new(idle_timeout),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Lists> {
        self.lists.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The operator level of a player, if it is an operator.
    pub fn op_level(&self, uuid: u128) -> Option<u8> {
        self.lock()
            .ops
            .iter()
            .find(|o| o.uuid.0 == uuid)
            .map(|o| o.level)
    }

    /// `canBypassPlayerLimit`.
    pub fn bypasses_player_limit(&self, uuid: u128) -> bool {
        self.lock()
            .ops
            .iter()
            .any(|o| o.uuid.0 == uuid && o.bypasses_player_limit)
    }

    /// All operators.
    pub fn ops(&self) -> Vec<OpEntry> {
        self.lock().ops.clone()
    }

    /// Makes a player an operator with `op-permission-level`. `false` if it already is one.
    pub fn op(&self, uuid: u128, name: &str) -> bool {
        let mut lists = self.lock();
        if lists.ops.iter().any(|o| o.uuid.0 == uuid) {
            return false;
        }
        lists.ops.push(OpEntry {
            uuid: Uuid(uuid),
            name: name.to_owned(),
            level: self.op_permission_level,
            bypasses_player_limit: false,
        });
        self.save_ops(&lists);
        true
    }

    /// Removes an operator. `false` if it was none.
    pub fn deop(&self, uuid: u128) -> bool {
        let mut lists = self.lock();
        let before = lists.ops.len();
        lists.ops.retain(|o| o.uuid.0 != uuid);
        let removed = lists.ops.len() != before;
        if removed {
            self.save_ops(&lists);
        }
        removed
    }

    /// Whether the whitelist is on.
    pub fn whitelist_enabled(&self) -> bool {
        self.lock().whitelist_enabled
    }

    /// Turns the whitelist on or off and writes `white-list` into `server.properties`.
    pub fn set_whitelist_enabled(&self, on: bool) {
        self.lock().whitelist_enabled = on;
        self.write_property("white-list", &on.to_string());
    }

    /// Writes one setting into `server.properties`.
    fn write_property(&self, key: &str, value: &str) {
        let path = self.dir.join(vanillite_config::PROPERTIES_FILE);
        let mut map = std::fs::read_to_string(&path)
            .map(|t| vanillite_config::properties::parse(&t))
            .unwrap_or_default();
        map.insert(key.to_owned(), value.to_owned());
        if let Err(e) = std::fs::write(&path, vanillite_config::properties::render(&map)) {
            log::error!("Failed to write {}: {e}", path.display());
        }
    }

    /// `player-idle-timeout` in minutes.
    pub fn idle_timeout(&self) -> i32 {
        self.idle_timeout.load(Ordering::Relaxed)
    }

    /// `DedicatedServer.setPlayerIdleTimeout`: keeps and writes `player-idle-timeout`.
    pub fn set_idle_timeout(&self, minutes: i32) {
        self.idle_timeout.store(minutes, Ordering::Relaxed);
        self.write_property("player-idle-timeout", &minutes.to_string());
    }

    /// The ban of a player, unless it expired (`UserBanList.get`).
    pub fn ban_of(&self, uuid: u128) -> Option<UserBan> {
        let mut lists = self.lock();
        lists.bans.retain(|b| !b.ban.expired());
        lists.bans.iter().find(|b| b.uuid.0 == uuid).cloned()
    }

    /// The ban of an IP address, unless it expired (`IpBanList.get`).
    pub fn ip_ban_of(&self, ip: &str) -> Option<IpBan> {
        let mut lists = self.lock();
        lists.ip_bans.retain(|b| !b.ban.expired());
        lists.ip_bans.iter().find(|b| b.ip == ip).cloned()
    }

    /// `UserBanList.isBanned`.
    pub fn is_banned(&self, uuid: u128) -> bool {
        self.lock().bans.iter().any(|b| b.uuid.0 == uuid)
    }

    /// `IpBanList.isBanned`.
    pub fn is_ip_banned(&self, ip: &str) -> bool {
        self.lock().ip_bans.iter().any(|b| b.ip == ip)
    }

    /// The banned players in list order.
    pub fn bans(&self) -> Vec<UserBan> {
        self.lock().bans.clone()
    }

    /// The banned IP addresses in list order.
    pub fn ip_bans(&self) -> Vec<IpBan> {
        self.lock().ip_bans.clone()
    }

    /// `UserBanList.add`: replaces an existing ban of the player.
    pub fn ban(&self, entry: UserBan) {
        let mut lists = self.lock();
        match lists.bans.iter_mut().find(|b| b.uuid == entry.uuid) {
            Some(existing) => *existing = entry,
            None => lists.bans.push(entry),
        }
        self.save_bans(&lists);
    }

    /// `UserBanList.remove`. `false` if the player was not banned.
    pub fn pardon(&self, uuid: u128) -> bool {
        let mut lists = self.lock();
        let before = lists.bans.len();
        lists.bans.retain(|b| b.uuid.0 != uuid);
        let removed = lists.bans.len() != before;
        if removed {
            self.save_bans(&lists);
        }
        removed
    }

    /// `IpBanList.add`: replaces an existing ban of the address.
    pub fn ban_ip(&self, entry: IpBan) {
        let mut lists = self.lock();
        match lists.ip_bans.iter_mut().find(|b| b.ip == entry.ip) {
            Some(existing) => *existing = entry,
            None => lists.ip_bans.push(entry),
        }
        self.save_ip_bans(&lists);
    }

    /// `IpBanList.remove`. `false` if the address was not banned.
    pub fn pardon_ip(&self, ip: &str) -> bool {
        let mut lists = self.lock();
        let before = lists.ip_bans.len();
        lists.ip_bans.retain(|b| b.ip != ip);
        let removed = lists.ip_bans.len() != before;
        if removed {
            self.save_ip_bans(&lists);
        }
        removed
    }

    /// `PlayerList.isWhiteListed`: the whitelist is off, or the player is listed or an operator.
    pub fn is_whitelisted(&self, uuid: u128) -> bool {
        let lists = self.lock();
        !lists.whitelist_enabled
            || lists.whitelist.iter().any(|w| w.uuid.0 == uuid)
            || lists.ops.iter().any(|o| o.uuid.0 == uuid)
    }

    /// Names on the whitelist, in list order.
    pub fn whitelisted_names(&self) -> Vec<String> {
        self.lock()
            .whitelist
            .iter()
            .map(|w| w.name.clone())
            .collect()
    }

    /// Adds a player to the whitelist. `false` if it is already listed.
    pub fn whitelist_add(&self, uuid: u128, name: &str) -> bool {
        let mut lists = self.lock();
        if lists.whitelist.iter().any(|w| w.uuid.0 == uuid) {
            return false;
        }
        lists.whitelist.push(WhitelistEntry {
            uuid: Uuid(uuid),
            name: name.to_owned(),
        });
        self.save_whitelist(&lists);
        true
    }

    /// Removes a player from the whitelist. `false` if it was not listed.
    pub fn whitelist_remove(&self, uuid: u128) -> bool {
        let mut lists = self.lock();
        let before = lists.whitelist.len();
        lists.whitelist.retain(|w| w.uuid.0 != uuid);
        let removed = lists.whitelist.len() != before;
        if removed {
            self.save_whitelist(&lists);
        }
        removed
    }

    /// `whitelist reload`: reads `whitelist.json` (and `ops.json`) again.
    pub fn reload(&self) {
        let mut lists = self.lock();
        lists.ops = read_list(&self.dir.join(OPS_FILE), parse_op);
        lists.whitelist = read_list(&self.dir.join(WHITELIST_FILE), parse_whitelisted);
    }

    fn save_ops(&self, lists: &Lists) {
        let json: Vec<serde_json::Value> = lists
            .ops
            .iter()
            .map(|o| {
                serde_json::json!({
                    "uuid": o.uuid.to_string(),
                    "name": o.name,
                    "level": o.level,
                    "bypassesPlayerLimit": o.bypasses_player_limit,
                })
            })
            .collect();
        write_list(&self.dir.join(OPS_FILE), &json);
    }

    fn save_bans(&self, lists: &Lists) {
        let json: Vec<serde_json::Value> = lists
            .bans
            .iter()
            .map(|b| {
                let mut out = serde_json::Map::new();
                out.insert("uuid".into(), b.uuid.to_string().into());
                out.insert("name".into(), b.name.clone().into());
                b.ban.to_json(&mut out);
                serde_json::Value::Object(out)
            })
            .collect();
        write_list(&self.dir.join(BANNED_PLAYERS_FILE), &json);
    }

    fn save_ip_bans(&self, lists: &Lists) {
        let json: Vec<serde_json::Value> = lists
            .ip_bans
            .iter()
            .map(|b| {
                let mut out = serde_json::Map::new();
                out.insert("ip".into(), b.ip.clone().into());
                b.ban.to_json(&mut out);
                serde_json::Value::Object(out)
            })
            .collect();
        write_list(&self.dir.join(BANNED_IPS_FILE), &json);
    }

    fn save_whitelist(&self, lists: &Lists) {
        let json: Vec<serde_json::Value> = lists
            .whitelist
            .iter()
            .map(|w| serde_json::json!({ "uuid": w.uuid.to_string(), "name": w.name }))
            .collect();
        write_list(&self.dir.join(WHITELIST_FILE), &json);
    }
}

/// Parses a dashed UUID.
pub fn parse_uuid(text: &str) -> Option<u128> {
    let hex: String = text.chars().filter(|c| *c != '-').collect();
    (hex.len() == 32)
        .then(|| u128::from_str_radix(&hex, 16).ok())
        .flatten()
}

fn parse_op(v: &serde_json::Value) -> Option<OpEntry> {
    Some(OpEntry {
        uuid: Uuid(parse_uuid(v.get("uuid")?.as_str()?)?),
        name: v.get("name")?.as_str()?.to_owned(),
        level: v
            .get("level")
            .and_then(serde_json::Value::as_u64)
            .and_then(|l| u8::try_from(l.clamp(1, 4)).ok())
            .unwrap_or(4),
        bypasses_player_limit: v
            .get("bypassesPlayerLimit")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    })
}

fn parse_whitelisted(v: &serde_json::Value) -> Option<WhitelistEntry> {
    Some(WhitelistEntry {
        uuid: Uuid(parse_uuid(v.get("uuid")?.as_str()?)?),
        name: v.get("name")?.as_str()?.to_owned(),
    })
}

fn parse_user_ban(v: &serde_json::Value) -> Option<UserBan> {
    Some(UserBan {
        uuid: Uuid(parse_uuid(v.get("uuid")?.as_str()?)?),
        name: v.get("name")?.as_str()?.to_owned(),
        ban: Ban::from_json(v),
    })
}

fn parse_ip_ban(v: &serde_json::Value) -> Option<IpBan> {
    Some(IpBan {
        ip: v.get("ip")?.as_str()?.to_owned(),
        ban: Ban::from_json(v),
    })
}

/// Year, month, day, hour, minute, second (UTC) of Unix time `secs` (`civil_from_days`).
pub(crate) fn utc(secs: u64) -> (i64, u8, u8, u8, u8, u8) {
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rest = secs % 86_400;
    let z = days.saturating_add(719_468);
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe.saturating_add(era.saturating_mul(400)) + i64::from(month <= 2);
    let small = |v: i64| u8::try_from(v).unwrap_or(0);
    let part = |v: u64| u8::try_from(v).unwrap_or(0);
    (
        year,
        small(month),
        small(day),
        part(rest / 3600),
        part(rest / 60 % 60),
        part(rest % 60),
    )
}

/// Unix seconds of `yyyy-MM-dd HH:mm:ss Z` (`BanListEntry.DATE_FORMAT`); `None` for other
/// text (`forever`) and dates before 1970.
fn parse_date(text: &str) -> Option<u64> {
    let (date, rest) = text.trim().split_once(' ')?;
    let (time, zone) = rest.split_once(' ')?;
    let mut d = date.splitn(3, '-').map(|p| p.parse::<i64>().ok());
    let (y, mo, day) = (d.next()??, d.next()??, d.next()??);
    let mut t = time.splitn(3, ':').map(|p| p.parse::<i64>().ok());
    let (h, mi, s) = (t.next()??, t.next()??, t.next()??);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&day) || !(0..=9999).contains(&y) {
        return None;
    }
    let sign = if zone.starts_with('-') { -1 } else { 1 };
    let digits = zone.trim_start_matches(['+', '-']);
    let offset = match (digits.get(..2), digits.get(2..)) {
        (Some(hh), Some(mm)) if digits.len() == 4 => {
            sign * (hh.parse::<i64>().ok()? * 3600 + mm.parse::<i64>().ok()? * 60)
        }
        _ => 0,
    };
    // `days_from_civil`.
    let yy = if mo <= 2 { y - 1 } else { y };
    let era = yy.div_euclid(400);
    let yoe = yy.rem_euclid(400);
    let mp = if mo > 2 { mo - 3 } else { mo + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let clock = h.clamp(0, 23) * 3600 + mi.clamp(0, 59) * 60 + s.clamp(0, 59);
    u64::try_from(days * 86_400 + clock - offset).ok()
}

fn read_list<T>(path: &Path, parse: fn(&serde_json::Value) -> Option<T>) -> Vec<T> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            write_list(path, &[]);
            return Vec::new();
        }
        Err(e) => {
            log::error!("Failed to read {}: {e}", path.display());
            return Vec::new();
        }
    };
    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(serde_json::Value::Array(items)) => items.iter().filter_map(parse).collect(),
        _ => {
            log::warn!("{} is not a JSON list; it is ignored", path.display());
            Vec::new()
        }
    }
}

fn write_list(path: &Path, items: &[serde_json::Value]) {
    let text = serde_json::to_string_pretty(items).unwrap_or_else(|_| "[]".to_owned());
    if let Err(e) = std::fs::write(path, text) {
        log::error!("Failed to write {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ops_and_whitelist_round_trip() {
        let dir = std::env::temp_dir().join(format!("vl-access-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        let access = Access::load(&dir, false, 4, false);
        let alice = 0x10920508_d5d8_3eed_93d2_92f193afe7d7;
        assert!(access.op(alice, "Alice"));
        assert!(!access.op(alice, "Alice"));
        assert!(access.whitelist_add(alice, "Alice"));
        let reloaded = Access::load(&dir, true, 4, false);
        assert_eq!(reloaded.op_level(alice), Some(4));
        assert_eq!(reloaded.whitelisted_names(), vec!["Alice".to_owned()]);
        assert!(reloaded.is_whitelisted(alice));
        assert!(!reloaded.is_whitelisted(1));
        assert!(reloaded.deop(alice));
        assert!(reloaded.whitelist_remove(alice));
        assert!(!reloaded.is_whitelisted(alice));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn bans_round_trip() {
        let dir = std::env::temp_dir().join(format!("vl-bans-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        let access = Access::load(&dir, false, 4, false);
        access.ban(UserBan {
            uuid: Uuid(7),
            name: "Bob".into(),
            ban: Ban::now("Server", Some("griefing".into())),
        });
        access.ban_ip(IpBan {
            ip: "10.0.0.1".into(),
            ban: Ban::now("Server", None),
        });
        let reloaded = Access::load(&dir, false, 4, false);
        assert_eq!(
            reloaded.ban_of(7).and_then(|b| b.ban.reason),
            Some("griefing".into())
        );
        assert!(reloaded.is_ip_banned("10.0.0.1"));
        assert!(reloaded.pardon(7));
        assert!(!reloaded.pardon(7));
        assert!(reloaded.pardon_ip("10.0.0.1"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ban_dates() {
        assert_eq!(parse_date("1970-01-02 00:00:00 +0000"), Some(86_400));
        assert_eq!(parse_date("1970-01-01 01:00:00 +0100"), Some(0));
        assert_eq!(parse_date("forever"), None);
        let t = parse_date("2026-11-24 13:05:09 +0000").unwrap_or(0);
        assert_eq!(utc(t), (2026, 11, 24, 13, 5, 9));
    }

    #[test]
    fn parses_uuids() {
        assert_eq!(
            parse_uuid("10920508-d5d8-3eed-93d2-92f193afe7d7"),
            Some(0x10920508_d5d8_3eed_93d2_92f193afe7d7)
        );
        assert_eq!(parse_uuid("nope"), None);
    }
}
