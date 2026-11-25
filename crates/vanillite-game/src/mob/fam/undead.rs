//! Zombie and skeleton variants, the small monsters (cave spider, silverfish, endermite), the cubes (slime, magma cube, sulfur cube), giants and phantoms.

use super::FamilyHooks;

/// The family's hooks.
#[derive(Debug)]
pub struct Hooks;

/// The kind state of the family's mobs (in [`super::Ext::Undead`]).
#[derive(Clone, Debug, Default)]
pub struct State {}

/// The family's goals.
#[derive(Clone, Copy, Debug)]
pub enum Goal {
    /// Never runs (keeps the enum inhabited until the family has goals).
    Idle,
}

impl FamilyHooks for Hooks {
    type Goal = Goal;
}
