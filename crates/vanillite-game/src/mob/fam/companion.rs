//! Horses (horse, donkey, mule, skeleton and zombie horse), llamas, trader llamas, wolves, cats, ocelots, parrots, foxes, pandas, polar bears and rabbits.

use super::FamilyHooks;

/// The family's hooks.
#[derive(Debug)]
pub struct Hooks;

/// The kind state of the family's mobs (in [`super::Ext::Companion`]).
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
