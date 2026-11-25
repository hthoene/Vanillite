//! The villager brain (`VillagerGoalPackages`, the villager behaviours and sensors).

use crate::mob::Tick;
use crate::mob::brain::behavior::Shape;
use crate::mob::brain::{MemoryStatus, MemoryType};

/// The villager behaviours.
#[derive(Clone, Copy, Debug)]
pub enum Behavior {
    /// Never runs.
    Idle,
}

/// The villager sensors.
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
    false
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
