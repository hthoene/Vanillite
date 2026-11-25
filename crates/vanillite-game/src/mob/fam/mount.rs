//! Brain AI mounts and helpers: camels, camel husks, happy ghasts, nautiluses, zombie nautiluses, allays, copper golems, breezes and creakings.

use super::FamilyHooks;

/// The family's hooks.
#[derive(Debug)]
pub struct Hooks;

/// The kind state of the family's mobs (in [`super::Ext::Mount`]).
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

// --- Brain plug-ins (`brain::behavior::FamBehavior`, `brain::sensor::FamSensor`) ---------

use crate::mob::Tick;
use crate::mob::brain::behavior::Shape;
use crate::mob::brain::{MemoryStatus, MemoryType};

/// The family's brain behaviours.
#[derive(Clone, Copy, Debug)]
pub enum Behavior {
    /// Never runs (keeps the enum inhabited until the family has behaviours).
    Idle,
}

/// The family's brain sensors.
#[derive(Clone, Copy, Debug)]
pub enum Sensor {
    /// Never ticks.
    Idle,
}

pub fn sensor_scan_rate(_s: Sensor) -> i32 {
    20
}
pub fn sensor_requires(_s: &Sensor) -> &'static [MemoryType] {
    &[]
}
pub fn sensor_tick(_t: &mut Tick<'_, '_>, _s: Sensor) {}
pub fn behavior_duration(_b: &Behavior) -> (i32, i32) {
    (60, 60)
}
pub fn behavior_shape(_b: &Behavior) -> Shape {
    Shape::OneShot
}
pub fn behavior_requirements(_b: &Behavior) -> &'static [(MemoryType, MemoryStatus)] {
    &[]
}
pub fn behavior_no_timeout(_b: &Behavior) -> bool {
    false
}
pub fn behavior_check(_t: &mut Tick<'_, '_>, _b: &mut Behavior) -> bool {
    true
}
pub fn behavior_can_still_use(_t: &mut Tick<'_, '_>, _b: &mut Behavior, _now: i64) -> bool {
    false
}
pub fn behavior_start(_t: &mut Tick<'_, '_>, _b: &mut Behavior, _now: i64) {}
pub fn behavior_tick(_t: &mut Tick<'_, '_>, _b: &mut Behavior, _now: i64) {}
pub fn behavior_stop(_t: &mut Tick<'_, '_>, _b: &mut Behavior, _now: i64) {}
pub fn behavior_trigger(_t: &mut Tick<'_, '_>, _b: &mut Behavior, _now: i64) -> bool {
    false
}
