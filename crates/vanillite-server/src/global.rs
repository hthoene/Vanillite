//! Server-wide state that worlds can read and change through narrow methods (architecture 4.7).

use vanillite_world::entity::NetworkIds;

use crate::clock::Clock;
use crate::player_list::PlayerList;

/// Server-wide state shared with all worlds.
#[derive(Debug)]
pub struct Global {
    /// The monotonic server clock.
    pub clock: Clock,
    /// Online players.
    pub players: PlayerList,
    /// Network entity ids of all worlds.
    pub network_ids: NetworkIds,
}
