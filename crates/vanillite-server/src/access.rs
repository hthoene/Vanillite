//! Operators and the whitelist: Vanilla's `ops.json` and `whitelist.json`, keyed by UUID
//! (SEC-13), and the `white-list` switch of `server.properties`.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::player_list::Uuid;

/// File of the operators.
pub const OPS_FILE: &str = "ops.json";
/// File of the whitelist.
pub const WHITELIST_FILE: &str = "whitelist.json";

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
        };
        Access {
            dir: dir.to_owned(),
            lists: Mutex::new(lists),
            op_permission_level: u8::try_from(op_permission_level.clamp(1, 4)).unwrap_or(4),
            enforce_whitelist,
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
        let path = self.dir.join(vanillite_config::PROPERTIES_FILE);
        let mut map = std::fs::read_to_string(&path)
            .map(|t| vanillite_config::properties::parse(&t))
            .unwrap_or_default();
        map.insert("white-list".to_owned(), on.to_string());
        if let Err(e) = std::fs::write(&path, vanillite_config::properties::render(&map)) {
            log::error!("Failed to write {}: {e}", path.display());
        }
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
    fn parses_uuids() {
        assert_eq!(
            parse_uuid("10920508-d5d8-3eed-93d2-92f193afe7d7"),
            Some(0x10920508_d5d8_3eed_93d2_92f193afe7d7)
        );
        assert_eq!(parse_uuid("nope"), None);
    }
}
