//! Gameplay: the Vanilla level tick, block behaviours, entities, physics, items, inventories, players and commands.
//!
//! - [`block`]: block behaviours: fluids, falling blocks, growth and spreading; placement rules.
//! - [`level`]: the game state of a world and Vanilla's level operations (`setBlock`, neighbour
//!   and shape updates, scheduled and random ticks).
//! - [`commands`]: the syntax of the commands and the keys they are executed by.
//! - [`entities`]: the entities of a world, the core store plus one kind store per kind.
//! - [`player`]: the player kind, its connection and packet handlers.
//! - [`survival`]: health, damage, hunger, death, clocks, weather and game rules.

pub mod block;
pub mod commands;
pub mod enchant;
pub mod entities;
pub mod experience_orb;
pub mod inventory;
pub mod item;
pub mod item_entity;
pub mod item_nbt;
pub mod level;
pub mod loot;
pub mod map;
pub mod menu;
pub mod player;
pub mod recipe;
pub mod survival;
