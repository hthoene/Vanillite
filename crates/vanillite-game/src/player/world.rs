//! What the player handlers need from a world: block access with Vanilla's update flags, level
//! events, sounds, crack progress and item drops. The server implements [`PlayLevel`] over the
//! world's [`crate::level::Level`]; sounds, crack progress and new items are collected and sent
//! after the packets of the tick.

use vanillite_base::{BlockPos, Vec3};
use vanillite_data::block::BlockState;

use crate::item::ItemStack;

pub use vanillite_protocol::packets::play::clientbound::SoundSource;

/// `LevelEvent` ids the server sends.
pub mod events {
    /// Block break particles and sound; data = block state id.
    pub const PARTICLES_DESTROY_BLOCK: i32 = 2001;
    /// Digging particles on the hit face (every 4th tick); data = face ordinal.
    pub const PARTICLES_AND_SOUND_BLOCK_DESTROY_PROGRESS: i32 = 2020;
    /// Digging particles on the hit face; data = face ordinal.
    pub const PARTICLES_BLOCK_DESTROY_PROGRESS: i32 = 2019;
}

/// A world as gameplay code sees it.
pub trait PlayLevel {
    /// The block state at `pos`; air outside loaded chunks and the build height.
    fn block(&self, pos: BlockPos) -> BlockState;
    /// Whether the chunk of `pos` is loaded (full).
    fn is_loaded(&self, pos: BlockPos) -> bool;
    /// Lowest block y.
    fn min_y(&self) -> i32;
    /// Highest block y (inclusive, `getMaxY`).
    fn max_y(&self) -> i32;
    /// `Level.setBlock` with the flags of [`crate::level::update`]. Returns whether the block
    /// changed.
    fn set_block(&mut self, pos: BlockPos, state: BlockState, flags: u32) -> bool;
    /// `Level.levelEvent`: to every player near `pos` except `except` (an entity network id).
    fn level_event(&mut self, except: Option<i32>, event: i32, pos: BlockPos, data: i32);
    /// `Level.destroyBlockProgress`: crack progress of `breaker` (network id) at `pos`; a
    /// value outside 0–9 removes it.
    fn destroy_progress(&mut self, breaker: i32, pos: BlockPos, progress: i32);
    /// `Level.playSound` of a registered sound event at `pos`, to every player in range
    /// except `except`.
    fn play_sound(
        &mut self,
        except: Option<i32>,
        pos: Vec3,
        sound: u16,
        source: SoundSource,
        volume: f32,
        pitch: f32,
    );
    /// Spawns an item entity (`new ItemEntity`): at `pos` with `velocity`, `pickup_delay`
    /// ticks before it can be picked up, and the UUID of its thrower.
    fn spawn_item(
        &mut self,
        pos: Vec3,
        stack: ItemStack,
        velocity: Vec3,
        pickup_delay: i16,
        thrower: Option<u128>,
    );
    /// `Level.isUnobstructed`: whether no entity that blocks building intersects the
    /// collision shape of `state` at `pos`.
    fn is_unobstructed(&self, state: BlockState, pos: BlockPos) -> bool;
    /// A uniform random float in `[0, 1)` from the world's random source.
    fn random_float(&mut self) -> f32;
    /// A uniform random double in `[0, 1)` from the world's random source.
    fn random_double(&mut self) -> f64;
    /// A uniform random boolean from the world's random source.
    fn random_boolean(&mut self) -> bool;
    /// `nextInt(bound)` of the world's random source; `bound` is at least 1.
    fn random_int(&mut self, bound: i32) -> i32;
    /// `EndCrystalItem.useOn` on the block at `on`: places an end crystal; whether it did.
    fn use_end_crystal(&mut self, on: BlockPos) -> bool {
        let _ = on;
        false
    }
}
