//! The list of online players.

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use vanillite_game::player::PacketSink;
use vanillite_game::player::join::InfoEntry;
use vanillite_protocol::{Packet, SharedPacket};

use crate::world::WorldId;

/// A player UUID.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Uuid(pub u128);

impl fmt::Display for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let v = self.0;
        write!(
            f,
            "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
            v >> 96,
            (v >> 80) & 0xffff,
            (v >> 64) & 0xffff,
            (v >> 48) & 0xffff,
            v & 0xffff_ffff_ffff
        )
    }
}

/// One online player.
#[derive(Clone)]
pub struct PlayerEntry {
    /// The player's UUID.
    pub uuid: Uuid,
    /// The player's name.
    pub name: String,
    /// The world the player is in.
    pub world: WorldId,
    /// The tab list entry (profile with skin, game mode, latency).
    pub info: InfoEntry,
    /// The player's connection, for messages to all players.
    pub sink: Arc<dyn PacketSink>,
}

impl fmt::Debug for PlayerEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlayerEntry")
            .field("uuid", &self.uuid)
            .field("name", &self.name)
            .field("world", &self.world)
            .finish_non_exhaustive()
    }
}

/// Online players in join order (Vanilla's order). Cold data behind a mutex: changed on join,
/// quit and world change, read for the server list, the tab list and messages to everyone.
#[derive(Debug)]
pub struct PlayerList {
    max_players: usize,
    players: Mutex<Vec<PlayerEntry>>,
}

impl PlayerList {
    /// An empty list for at most `max_players` players.
    pub fn new(max_players: usize) -> PlayerList {
        PlayerList {
            max_players,
            players: Mutex::new(Vec::new()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Vec<PlayerEntry>> {
        self.players.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The configured maximum.
    pub fn max_players(&self) -> usize {
        self.max_players
    }

    /// Number of online players.
    pub fn count(&self) -> usize {
        self.lock().len()
    }

    /// Whether a player with this UUID is online.
    pub fn contains(&self, uuid: Uuid) -> bool {
        self.lock().iter().any(|p| p.uuid == uuid)
    }

    /// Adds a player. Returns `false` if a player with that UUID is already online.
    pub fn add(&self, entry: PlayerEntry) -> bool {
        let mut players = self.lock();
        if players.iter().any(|p| p.uuid == entry.uuid) {
            return false;
        }
        players.push(entry);
        true
    }

    /// Removes a player.
    pub fn remove(&self, uuid: Uuid) -> Option<PlayerEntry> {
        let mut players = self.lock();
        let index = players.iter().position(|p| p.uuid == uuid)?;
        Some(players.remove(index))
    }

    /// Records that a player moved to another world.
    pub fn set_world(&self, uuid: Uuid, world: WorldId) {
        if let Some(player) = self.lock().iter_mut().find(|p| p.uuid == uuid) {
            player.world = world;
        }
    }

    /// Updates a player's tab list entry.
    pub fn update_info(&self, uuid: Uuid, update: impl FnOnce(&mut InfoEntry)) {
        if let Some(player) = self.lock().iter_mut().find(|p| p.uuid == uuid) {
            update(&mut player.info);
        }
    }

    /// A copy of the list.
    pub fn snapshot(&self) -> Vec<PlayerEntry> {
        self.lock().clone()
    }

    /// The tab list entries of all players, with current latencies.
    pub fn info_entries(&self) -> Vec<InfoEntry> {
        self.lock()
            .iter()
            .map(|p| InfoEntry {
                latency: p.sink.latency_ms(),
                ..p.info.clone()
            })
            .collect()
    }

    /// Sends a packet to every player except `except`. The packet is encoded once.
    pub fn broadcast<P: Packet>(&self, packet: &P, except: Option<Uuid>) {
        let shared = SharedPacket::of(packet);
        let sinks: Vec<Arc<dyn PacketSink>> = self
            .lock()
            .iter()
            .filter(|p| Some(p.uuid) != except)
            .map(|p| Arc::clone(&p.sink))
            .collect();
        for sink in sinks {
            sink.send_shared(&shared);
        }
    }
}
