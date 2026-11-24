//! Gameplay: the Vanilla level tick, block behaviours, entities, physics, items, inventories, players and commands.
//!
//! - [`commands`]: the syntax of the commands and the keys they are executed by.
//! - [`entities`]: the entities of a world, the core store plus one kind store per kind.
//! - [`player`]: the player kind, its connection and packet handlers.

pub mod commands;
pub mod entities;
pub mod player;
