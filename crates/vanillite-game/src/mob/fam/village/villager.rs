//! Villagers (`Villager`): professions and levels, gossip and reputation, food and
//! breeding, sleeping, golem summoning and the brain set up of [`super::ai`].

use super::super::FamilyHooks;

/// The family's hooks for villagers.
#[derive(Debug)]
pub struct Hooks;

/// The villager state (`Villager` fields besides the merchant ones).
#[derive(Clone, Debug, Default)]
pub struct Villager {
    /// `VillagerData`: type and profession registry ids, level (1 to 5).
    pub villager_type: u16,
    pub profession: u16,
    pub level: i32,
    /// `DATA_VILLAGER_DATA_FINALIZED`.
    pub data_finalized: bool,
}

/// Villagers have no goals.
#[derive(Clone, Copy, Debug)]
pub enum NoGoal {}

impl FamilyHooks for Hooks {
    type Goal = NoGoal;
}
