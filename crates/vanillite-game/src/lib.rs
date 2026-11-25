//! Gameplay: the Vanilla level tick, block behaviours, entities, physics, items, inventories, players and commands.
//!
//! - [`advancements`] and [`stats`]: advancements and statistics of the players.
//! - [`block`]: block behaviours: fluids, falling blocks, growth and spreading; placement rules.
//! - [`level`]: the game state of a world and Vanilla's level operations (`setBlock`, neighbour
//!   and shape updates, scheduled and random ticks).
//! - [`commands`]: the syntax of the commands and the keys they are executed by.
//! - [`entities`]: the entities of a world, the core store plus one kind store per kind.
//! - [`player`]: the player kind, its connection and packet handlers.
//! - [`survival`]: health, damage, hunger, death, clocks, weather and game rules.
//! - [`mob`]: mobs and animals: physics, path finding, goals, combat and spawning.

pub mod advancements;
pub mod block;
pub mod commands;
pub mod enchant;
pub mod entities;
pub mod hooks;
pub mod inventory;
pub mod item;
pub mod item_entity;
pub mod level;
pub mod loot;
pub mod menu;
pub mod minecart;
pub mod mob;
pub mod player;
pub mod portal;
pub mod predicate;
pub mod recipe;
pub mod riding;
pub mod scoreboard;
pub mod stats;
pub mod survival;
pub mod vehicle;
