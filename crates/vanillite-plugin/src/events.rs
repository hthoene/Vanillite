//! Event views: borrowed for the handler call, with accessors over the host's records.

use vanillite_abi as abi;

use crate::ffi::str;
use crate::types::{BlockPos, Uuid, Vec3, World};

/// A player joined (after the join message) or left.
#[derive(Debug)]
pub struct PlayerJoin<'a> {
    pub(crate) raw: &'a abi::PlayerEvent,
}

/// A player left. Same view as [`PlayerJoin`].
pub type PlayerLeave<'a> = PlayerJoin<'a>;

impl PlayerJoin<'_> {
    /// The player.
    pub fn player(&self) -> Uuid {
        Uuid::from_abi(self.raw.player)
    }

    /// Its name.
    pub fn name(&self) -> &str {
        // SAFETY: the host's strings are valid for the call.
        unsafe { str(self.raw.name) }
    }

    /// Its world.
    pub fn world(&self) -> World {
        World(self.raw.world)
    }
}

/// A chat message or a command line of a player; cancellable.
#[derive(Debug)]
pub struct PlayerText<'a> {
    pub(crate) raw: &'a mut abi::PlayerTextEvent,
}

impl PlayerText<'_> {
    /// The player.
    pub fn player(&self) -> Uuid {
        Uuid::from_abi(self.raw.player)
    }

    /// Its name.
    pub fn name(&self) -> &str {
        // SAFETY: the host's strings are valid for the call.
        unsafe { str(self.raw.name) }
    }

    /// The message, or the command line without `/`.
    pub fn text(&self) -> &str {
        // SAFETY: the host's strings are valid for the call.
        unsafe { str(self.raw.text) }
    }

    /// Whether an earlier handler cancelled it.
    pub fn is_cancelled(&self) -> bool {
        self.raw.header.cancelled != 0
    }

    /// Cancels (or un-cancels) it: the message is not sent, the command not run.
    pub fn set_cancelled(&mut self, cancelled: bool) {
        self.raw.header.cancelled = u32::from(cancelled);
    }
}

/// All players of a world that moved in one tick (batched: one call per world and tick).
#[derive(Debug)]
pub struct PlayersMoved<'a> {
    pub(crate) raw: &'a abi::PlayersMovedEvent,
}

impl PlayersMoved<'_> {
    /// The world.
    pub fn world(&self) -> World {
        World(self.raw.world)
    }

    /// The moves: player, position at the start of the tick, position now.
    pub fn moves(&self) -> impl Iterator<Item = (Uuid, Vec3, Vec3)> + '_ {
        let raw = self.raw;
        (0..raw.count).map(move |i| {
            // SAFETY: the host passes `count` records `stride` bytes apart, valid for the call;
            // the SDK's record is a prefix of the host's.
            let m = unsafe {
                &*raw
                    .moves
                    .cast::<u8>()
                    .add(i * raw.stride)
                    .cast::<abi::PlayerMove>()
            };
            (
                Uuid::from_abi(m.player),
                Vec3::from_abi(m.from),
                Vec3::from_abi(m.to),
            )
        })
    }
}

/// An entity is about to take damage; cancellable, the amount can be changed.
#[derive(Debug)]
pub struct EntityDamage<'a> {
    pub(crate) raw: &'a mut abi::EntityDamageEvent,
}

impl EntityDamage<'_> {
    /// The world.
    pub fn world(&self) -> World {
        World(self.raw.world)
    }

    /// The entity.
    pub fn entity(&self) -> Uuid {
        Uuid::from_abi(self.raw.entity)
    }

    /// Its type, e.g. `minecraft:player`.
    pub fn entity_type(&self) -> &str {
        // SAFETY: the host's strings are valid for the call.
        unsafe { str(self.raw.entity_type) }
    }

    /// The damage type, e.g. `minecraft:fall`.
    pub fn damage_type(&self) -> &str {
        // SAFETY: the host's strings are valid for the call.
        unsafe { str(self.raw.damage_type) }
    }

    /// The damage.
    pub fn amount(&self) -> f32 {
        self.raw.amount
    }

    /// Changes the damage.
    pub fn set_amount(&mut self, amount: f32) {
        self.raw.amount = amount;
    }

    /// Whether an earlier handler cancelled it.
    pub fn is_cancelled(&self) -> bool {
        self.raw.header.cancelled != 0
    }

    /// Cancels (or un-cancels) the damage.
    pub fn set_cancelled(&mut self, cancelled: bool) {
        self.raw.header.cancelled = u32::from(cancelled);
    }
}

/// A player and a block: breaking or placing it, entering or leaving a bed, a new respawn
/// point; cancellable except leaving a bed.
#[derive(Debug)]
pub struct BlockChange<'a> {
    pub(crate) raw: &'a mut abi::BlockEvent,
}

impl BlockChange<'_> {
    /// The world.
    pub fn world(&self) -> World {
        World(self.raw.world)
    }

    /// The player.
    pub fn player(&self) -> Uuid {
        Uuid::from_abi(self.raw.player)
    }

    /// The block.
    pub fn pos(&self) -> BlockPos {
        BlockPos::from_abi(self.raw.pos)
    }

    /// The state broken or to be placed (the bed or anchor), e.g. `minecraft:oak_log[axis=y]`.
    pub fn state(&self) -> &str {
        // SAFETY: the host's strings are valid for the call.
        unsafe { str(self.raw.state) }
    }

    /// Whether an earlier handler cancelled it.
    pub fn is_cancelled(&self) -> bool {
        self.raw.header.cancelled != 0
    }

    /// Cancels (or un-cancels) it.
    pub fn set_cancelled(&mut self, cancelled: bool) {
        self.raw.header.cancelled = u32::from(cancelled);
    }
}
