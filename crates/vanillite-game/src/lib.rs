//! Gameplay: the Vanilla level tick, block behaviours, entities, physics, items, inventories, players and commands.
//!
//! - [`block`]: block behaviours: fluids, falling blocks, growth and spreading.
//! - [`level`]: the game state of a world and Vanilla's level operations (`setBlock`, neighbour
//!   and shape updates, scheduled and random ticks).
//! - [`commands`]: the syntax of the commands and the keys they are executed by.
//! - [`entities`]: the entities of a world, the core store plus one kind store per kind.
//! - [`player`]: the player kind, its connection and packet handlers.
//! - [`survival`]: health, damage, hunger, death, clocks, weather and game rules.

pub mod block;
pub mod commands;
pub mod entities;
pub mod level;
pub mod player;
pub mod survival;
