//! World runtime infrastructure: chunk map, tickets, chunk scheduler, block access and updates, scheduled ticks and the entity store. No gameplay.
//!
//! - [`chunks`]: chunk map, tickets and levels, the chunk scheduler, per-player chunk sending
//!   and block access.
//! - [`entity`]: the entity store (generational ids, core columns, kind stores, tick order,
//!   section index).
//! - [`ticks`]: scheduled block and fluid ticks in Vanilla's order.
//! - [`tracking`]: the `EntityTracker` slot, its built-in implementation and the encode-once
//!   fan-out of its updates.

pub mod chunks;
pub mod entity;
pub mod ticks;
pub mod tracking;
