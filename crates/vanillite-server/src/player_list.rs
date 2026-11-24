//! The list of online players.

use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError};

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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayerEntry {
    /// The player's UUID.
    pub uuid: Uuid,
    /// The player's name.
    pub name: String,
    /// The world the player is in.
    pub world: WorldId,
}

/// Online players in join order (Vanilla's order). Cold data behind a mutex: changed on join,
/// quit and world change, read for the server list and the pause check.
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

    /// A copy of the list.
    pub fn snapshot(&self) -> Vec<PlayerEntry> {
        self.lock().clone()
    }
}
