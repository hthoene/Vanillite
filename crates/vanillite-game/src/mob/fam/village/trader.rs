//! Wandering traders (`WanderingTrader`) and their spawner (`WanderingTraderSpawner`).

use super::super::FamilyHooks;

/// The family's hooks for wandering traders.
#[derive(Debug)]
pub struct Hooks;

/// The wandering trader state.
#[derive(Clone, Debug, Default)]
pub struct Trader {
    /// `despawnDelay`.
    pub despawn_delay: i32,
}

/// The wandering trader goals.
#[derive(Clone, Copy, Debug)]
pub enum Goal {
    /// Never runs.
    Idle,
}

impl FamilyHooks for Hooks {
    type Goal = Goal;
}
