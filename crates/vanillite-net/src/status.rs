//! The cached server list status (SEC-18: answering a ping never touches the game).

use std::sync::{Arc, RwLock};
use vanillite_protocol::text::Component;
use vanillite_protocol::types::Uuid;

/// Status values the server updates.
#[derive(Debug, Clone)]
pub struct StatusInfo {
    pub description: Component,
    pub max_players: i32,
    pub online_players: i32,
    /// Up to 12 shown names (Vanilla's sample size).
    pub sample: Vec<(String, Uuid)>,
    /// PNG data URL (`data:image/png;base64,...`).
    pub favicon: Option<String>,
    pub enforces_secure_chat: bool,
}

impl Default for StatusInfo {
    fn default() -> Self {
        Self {
            description: Component::text("A Vanillite Server"),
            max_players: 20,
            online_players: 0,
            sample: Vec::new(),
            favicon: None,
            enforces_secure_chat: false,
        }
    }
}

/// The pre-rendered status JSON, shared by all network threads.
#[derive(Debug, Clone)]
pub struct StatusCache {
    json: Arc<RwLock<Arc<str>>>,
}

impl StatusCache {
    /// A cache with initial values.
    pub fn new(info: &StatusInfo) -> Self {
        Self {
            json: Arc::new(RwLock::new(render(info))),
        }
    }

    /// Replaces the status.
    pub fn update(&self, info: &StatusInfo) {
        let json = render(info);
        if let Ok(mut g) = self.json.write() {
            *g = json;
        }
    }

    /// The current JSON.
    pub fn get(&self) -> Arc<str> {
        self.json
            .read()
            .map(|g| Arc::clone(&g))
            .unwrap_or_else(|e| Arc::clone(&e.into_inner()))
    }
}

fn render(info: &StatusInfo) -> Arc<str> {
    use serde_json::{Value, json};
    let sample: Vec<Value> = info
        .sample
        .iter()
        .take(12)
        .map(|(name, id)| json!({ "name": name, "id": id.to_string() }))
        .collect();
    let mut v = json!({
        "version": { "name": vanillite_protocol::VERSION_ID, "protocol": vanillite_protocol::PROTOCOL_VERSION },
        "players": { "max": info.max_players, "online": info.online_players },
        "description": info.description.to_json(),
        "enforcesSecureChat": info.enforces_secure_chat,
    });
    if !sample.is_empty()
        && let Some(players) = v.get_mut("players").and_then(Value::as_object_mut)
    {
        players.insert("sample".into(), Value::Array(sample));
    }
    if let (Some(icon), Some(obj)) = (&info.favicon, v.as_object_mut()) {
        obj.insert("favicon".into(), Value::String(icon.clone()));
    }
    Arc::from(v.to_string())
}
