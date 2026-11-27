//! Shared movement goals (`CommonGoal::Move`): random strolling, swimming and flying
//! (`RandomStrollGoal`, `RandomSwimmingGoal`, `WaterAvoidingRandomFlyingGoal`), looking for
//! a liquid (`TryFindLiquidGoal`), surfacing for air (`BreathAirGoal`), climbing out of powder
//! snow, doors (`OpenDoorGoal`, `BreakDoorGoal`) and breaking blocks (`RemoveBlockGoal`, the
//! zombies' turtle egg trampling).

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Vec3};
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_protocol::SharedPacket;
use vanillite_protocol::packets::play::clientbound::{BlockDestruction, SoundSource};

use super::common::MoveToBlock;
use super::goals::{Goal, flag, reduced};
use super::{MobData, Tick, WorldAction, living, nav, randompos};
use crate::survival::Effect;
use crate::survival::rules::{Difficulty, Rule};

/// Which positions a wandering goal picks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WanderMode {
    /// `RandomStrollGoal`: `DefaultRandomPos.getPos(mob, 10, 7)`.
    Stroll,
    /// `RandomSwimmingGoal`: `BehaviorUtils.getRandomSwimmablePos(mob, 10, 7)`.
    Swim,
    /// `WaterAvoidingRandomFlyingGoal`: hovering above the ground ahead, else air or water.
    Fly,
}

/// The state of a door goal (`DoorInteractGoal`).
#[derive(Clone, Copy, Debug)]
pub struct DoorState {
    pub pos: BlockPos,
    pub has_door: bool,
    pub passed: bool,
    pub open_dir_x: f32,
    pub open_dir_z: f32,
}

impl DoorState {
    fn new() -> DoorState {
        DoorState {
            pos: BlockPos::ZERO,
            has_door: false,
            passed: false,
            open_dir_x: 0.0,
            open_dir_z: 0.0,
        }
    }
}

/// The sounds of a block removing goal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoveSounds {
    /// No sounds (`RemoveBlockGoal`).
    None,
    /// `ZombieAttackTurtleEggGoal`: `zombie.destroy_egg` while trampling,
    /// `turtle_egg.break` at the end.
    ZombieEgg,
}

/// A shared movement goal with its state.
#[derive(Clone, Copy, Debug)]
pub enum MoveGoal {
    /// `RandomStrollGoal(mob, speed, interval, checkNoActionTime)` and its subclasses.
    Wander {
        mode: WanderMode,
        speed: f64,
        interval: i32,
        check_no_action_time: bool,
        force: bool,
        wanted: Vec3,
    },
    /// `TryFindLiquidGoal(mob, WATER or LAVA)`.
    TryFindLiquid { lava: bool },
    /// `BreathAirGoal` (not interruptable).
    BreathAir,
    /// `ClimbOnTopOfPowderSnowGoal`.
    ClimbOnTopOfPowderSnow,
    /// `OpenDoorGoal(mob, closeDoorAfter)`.
    OpenDoor {
        door: DoorState,
        close: bool,
        forget_time: i32,
    },
    /// `BreakDoorGoal(mob, seconds, validDifficulties)`.
    BreakDoor {
        door: DoorState,
        break_time: i32,
        last_progress: i32,
        door_break_time: i32,
        valid: fn(Difficulty) -> bool,
    },
    /// `RemoveBlockGoal(block, mob, speed, verticalSearchRange)`.
    RemoveBlock {
        mtb: MoveToBlock,
        block: BlockId,
        ticks_since_reached: i32,
        sounds: RemoveSounds,
        accepted_distance: f64,
    },
}

impl MoveGoal {
    /// `RandomStrollGoal(mob, speed, interval)`.
    pub fn random_stroll(speed: f64, interval: i32) -> Goal {
        Self::wander(WanderMode::Stroll, speed, interval)
    }

    /// `RandomSwimmingGoal(mob, speed, interval)`.
    pub fn random_swimming(speed: f64, interval: i32) -> Goal {
        Self::wander(WanderMode::Swim, speed, interval)
    }

    /// `WaterAvoidingRandomFlyingGoal(mob, speed)`.
    pub fn water_avoiding_flying(speed: f64) -> Goal {
        Self::wander(WanderMode::Fly, speed, 120)
    }

    fn wander(mode: WanderMode, speed: f64, interval: i32) -> Goal {
        Self::goal(MoveGoal::Wander {
            mode,
            speed,
            interval,
            check_no_action_time: true,
            force: false,
            wanted: Vec3::ZERO,
        })
    }

    /// `TryFindLiquidGoal(mob, FluidTags.WATER)`.
    pub fn try_find_water() -> Goal {
        Self::goal(MoveGoal::TryFindLiquid { lava: false })
    }

    /// `TryFindLiquidGoal(mob, FluidTags.LAVA)`.
    pub fn try_find_lava() -> Goal {
        Self::goal(MoveGoal::TryFindLiquid { lava: true })
    }

    /// `BreathAirGoal(mob)`.
    pub fn breath_air() -> Goal {
        Self::goal(MoveGoal::BreathAir)
    }

    /// `ClimbOnTopOfPowderSnowGoal(mob, level)`.
    pub fn climb_on_top_of_powder_snow() -> Goal {
        Self::goal(MoveGoal::ClimbOnTopOfPowderSnow)
    }

    /// `OpenDoorGoal(mob, closeDoorAfter)`.
    pub fn open_door(close_after: bool) -> Goal {
        Self::goal(MoveGoal::OpenDoor {
            door: DoorState::new(),
            close: close_after,
            forget_time: 0,
        })
    }

    /// `BreakDoorGoal(mob, validDifficulties)` (240 ticks to break).
    pub fn break_door(valid: fn(Difficulty) -> bool) -> Goal {
        Self::break_door_seconds(-1, valid)
    }

    /// `BreakDoorGoal(mob, doorBreakTime, validDifficulties)`.
    pub fn break_door_seconds(door_break_time: i32, valid: fn(Difficulty) -> bool) -> Goal {
        Self::goal(MoveGoal::BreakDoor {
            door: DoorState::new(),
            break_time: 0,
            last_progress: -1,
            door_break_time,
            valid,
        })
    }

    /// `RemoveBlockGoal(block, mob, speed, verticalSearchRange)` (search range 24).
    pub fn remove_block(block: BlockId, speed: f64, vertical: i32, sounds: RemoveSounds) -> Goal {
        Self::goal(MoveGoal::RemoveBlock {
            mtb: MoveToBlock::new(speed, 24, vertical),
            block,
            ticks_since_reached: 0,
            sounds,
            accepted_distance: 1.0,
        })
    }

    /// `Zombie.ZombieAttackTurtleEggGoal(mob, 1.0, 3)` (accepted distance 1.14).
    pub fn zombie_turtle_egg() -> Goal {
        Self::goal(MoveGoal::RemoveBlock {
            mtb: MoveToBlock::new(1.0, 24, 3),
            block: ids::TURTLE_EGG,
            ticks_since_reached: 0,
            sounds: RemoveSounds::ZombieEgg,
            accepted_distance: 1.14,
        })
    }

    fn goal(g: MoveGoal) -> Goal {
        Goal::Common(super::common::CommonGoal::Move(g))
    }

    /// The goal's flags.
    pub fn flags(&self) -> u8 {
        match self {
            MoveGoal::Wander { .. } => flag::MOVE,
            MoveGoal::BreathAir => flag::MOVE | flag::LOOK,
            MoveGoal::ClimbOnTopOfPowderSnow => flag::JUMP,
            MoveGoal::RemoveBlock { .. } => flag::MOVE | flag::JUMP,
            MoveGoal::TryFindLiquid { .. }
            | MoveGoal::OpenDoor { .. }
            | MoveGoal::BreakDoor { .. } => 0,
        }
    }

    /// `requiresUpdateEveryTick`.
    pub fn every_tick(&self) -> bool {
        matches!(
            self,
            MoveGoal::ClimbOnTopOfPowderSnow
                | MoveGoal::OpenDoor { .. }
                | MoveGoal::BreakDoor { .. }
                | MoveGoal::RemoveBlock { .. }
        )
    }

    /// `isInterruptable`.
    pub fn interruptable(&self) -> bool {
        !matches!(self, MoveGoal::BreathAir)
    }
}

/// `Zombie.setCanBreakDoors`: adds or removes the door breaking goal (priority 1, Hard only)
/// and lets the navigation open doors; only for mobs that navigate on the ground.
pub fn set_can_break_doors(m: &mut MobData, on: bool) {
    let is_break = |g: &Goal| {
        matches!(
            g,
            Goal::Common(super::common::CommonGoal::Move(MoveGoal::BreakDoor { .. }))
        )
    };
    if !m.nav.can_navigate_ground() {
        if m.can_break_doors {
            m.goals.slots.retain(|s| !is_break(&s.goal));
            m.can_break_doors = false;
        }
        return;
    }
    if m.can_break_doors == on {
        return;
    }
    m.can_break_doors = on;
    m.nav.can_open_doors = on;
    if on {
        m.goals
            .add(1, MoveGoal::break_door(|d| d == Difficulty::Hard));
    } else {
        m.goals.slots.retain(|s| !is_break(&s.goal));
    }
}

/// `hasControllingPassenger`.
fn has_controlling_passenger(t: &Tick<'_, '_>) -> bool {
    t.ents
        .riding
        .passengers_of(t.id)
        .first()
        .is_some_and(|&p| crate::riding::controls(t.ents, p, t.id))
}

/// `DoorBlock.isWoodenDoor`: a door that opens by hand.
fn is_wooden_door(state: BlockState) -> bool {
    state.block().name().ends_with("_door")
        && crate::block::redstone::BlockSet::of(state.block()).can_open_by_hand
}

/// `DoorInteractGoal.isOpen`.
fn door_is_open(t: &Tick<'_, '_>, door: &mut DoorState) -> bool {
    if !door.has_door {
        return false;
    }
    let state = t.level.block(door.pos);
    if !state.block().name().ends_with("_door") {
        door.has_door = false;
        return false;
    }
    state.value("open") == Some("true")
}

/// `DoorInteractGoal.canUse`: a wooden door on the next nodes of the path (or above the
/// mob) while it bumps into something.
fn door_can_use(t: &mut Tick<'_, '_>, door: &mut DoorState) -> bool {
    if !t.mob_ref().nav.is_ground() || !t.mob_ref().horizontal_collision {
        return false;
    }
    let p = t.pos();
    let Some(path) = &t.mob_ref().nav.path else {
        return false;
    };
    if path.is_done() {
        return false;
    }
    let end = (path.next + 2).min(path.nodes.len());
    for node in &path.nodes[..end] {
        let pos = BlockPos::new(node.x, node.y + 1, node.z);
        let (dx, dz) = (f64::from(pos.x) - p.x, f64::from(pos.z) - p.z);
        door.pos = pos;
        if dx * dx + dz * dz <= 2.25 {
            door.has_door = is_wooden_door(t.level.block(pos));
            if door.has_door {
                return true;
            }
        }
    }
    let b = t.block_pos();
    door.pos = BlockPos::new(b.x, b.y + 1, b.z);
    door.has_door = is_wooden_door(t.level.block(door.pos));
    door.has_door
}

/// `DoorInteractGoal.start`.
#[allow(clippy::cast_possible_truncation)]
fn door_start(t: &Tick<'_, '_>, door: &mut DoorState) {
    let p = t.pos();
    door.passed = false;
    door.open_dir_x = (f64::from(door.pos.x) + 0.5 - p.x) as f32;
    door.open_dir_z = (f64::from(door.pos.z) + 0.5 - p.z) as f32;
}

/// `DoorInteractGoal.tick`: passed once the mob is on the other side.
#[allow(clippy::cast_possible_truncation)]
fn door_tick(t: &Tick<'_, '_>, door: &mut DoorState) {
    let p = t.pos();
    let x = (f64::from(door.pos.x) + 0.5 - p.x) as f32;
    let z = (f64::from(door.pos.z) + 0.5 - p.z) as f32;
    if door.open_dir_x * x + door.open_dir_z * z < 0.0 {
        door.passed = true;
    }
}

/// `DoorInteractGoal.setOpen`.
fn door_set_open(t: &mut Tick<'_, '_>, door: &DoorState, open: bool) {
    if door.has_door && t.level.block(door.pos).block().name().ends_with("_door") {
        let id = t.id;
        t.cx.actions
            .push(WorldAction::SetDoorOpen(door.pos, open, Some(id)));
    }
}

/// `level.destroyBlockProgress(mob.getId(), pos, progress)`: the crack overlay for the
/// players within 32 blocks.
fn destroy_progress(t: &mut Tick<'_, '_>, pos: BlockPos, progress: i32) {
    let id = t.core().network_id().0;
    let packet = SharedPacket::of(&BlockDestruction {
        id,
        pos: vanillite_protocol::types::BlockPos {
            x: pos.x,
            y: pos.y,
            z: pos.z,
        },
        progress: u8::try_from(progress).unwrap_or(255),
    });
    t.env.out.push(Effect::Nearby {
        pos: Vec3::new(f64::from(pos.x), f64::from(pos.y), f64::from(pos.z)),
        range: 32.0,
        except: None,
        packet,
    });
}

/// The powder snow a mob stands in (`isInPowderSnow` / `wasInPowderSnow`).
fn in_powder_snow(t: &Tick<'_, '_>) -> bool {
    let b = t.aabb().inflate(-1.0e-5, -1.0e-5, -1.0e-5);
    let floor = vanillite_base::math::floor;
    for x in floor(b.min_x)..=floor(b.max_x) {
        for y in floor(b.min_y)..=floor(b.max_y) {
            for z in floor(b.min_z)..=floor(b.max_z) {
                if t.level.block(BlockPos::new(x, y, z)).block() == ids::POWDER_SNOW {
                    return true;
                }
            }
        }
    }
    false
}

/// `BreathAirGoal.findAirPosition`: the first block of the column (and its four sides) up to
/// 8 above that gives air, else 8 above.
fn find_air_position(t: &mut Tick<'_, '_>) {
    let b = t.block_pos();
    let p = t.pos();
    let gives_air = |state: BlockState| {
        (state.fluid_state().is_empty() || state.block() == ids::BUBBLE_COLUMN)
            && super::path::is_pathfindable_land(state)
    };
    // `BlockPos.neighborColumn`: the column, then north, east, south and west of it.
    const STEPS: [(i32, i32); 5] = [(0, 0), (0, -1), (1, 0), (0, 1), (-1, 0)];
    let mut dest = None;
    'search: for (dx, dz) in STEPS {
        for y in b.y..=b.y + 8 {
            let pos = BlockPos::new(b.x + dx, y, b.z + dz);
            if gives_air(t.level.block(pos)) {
                dest = Some(pos);
                break 'search;
            }
        }
    }
    let dest = dest.unwrap_or_else(|| BlockPos::containing(p.x, p.y + 8.0, p.z));
    let at = Vec3::new(f64::from(dest.x), f64::from(dest.y + 1), f64::from(dest.z));
    nav::move_to_pos(t, at, 1.0);
}

/// `RemoveBlockGoal.isValidTarget`: the block with two air blocks above, in a loaded chunk.
fn remove_target_valid(t: &Tick<'_, '_>, pos: BlockPos, block: BlockId) -> bool {
    t.level.is_loaded(pos)
        && t.level.block(pos).block() == block
        && t.level
            .block(BlockPos::new(pos.x, pos.y + 1, pos.z))
            .is_air()
        && t.level
            .block(BlockPos::new(pos.x, pos.y + 2, pos.z))
            .is_air()
}

/// `RemoveBlockGoal.getPosWithBlock`: the block at, below, beside or two below the mob.
fn pos_with_block(t: &Tick<'_, '_>, pos: BlockPos, block: BlockId) -> Option<BlockPos> {
    let candidates = [
        pos,
        BlockPos::new(pos.x, pos.y - 1, pos.z),
        BlockPos::new(pos.x - 1, pos.y, pos.z),
        BlockPos::new(pos.x + 1, pos.y, pos.z),
        BlockPos::new(pos.x, pos.y, pos.z - 1),
        BlockPos::new(pos.x, pos.y, pos.z + 1),
        BlockPos::new(pos.x, pos.y - 2, pos.z),
    ];
    candidates
        .into_iter()
        .find(|&p| t.level.block(p).block() == block)
}

/// The block a running remove goal targets (`MoveToBlockGoal.isValidTarget` needs it as a
/// function pointer; the goals that remove other blocks than turtle eggs pass their own).
fn turtle_egg_target(t: &Tick<'_, '_>, pos: BlockPos) -> bool {
    remove_target_valid(t, pos, ids::TURTLE_EGG)
}

fn remove_filter(block: BlockId) -> super::common::BlockFilter {
    if block == ids::TURTLE_EGG {
        turtle_egg_target
    } else {
        |_, _| false
    }
}

pub fn can_use(t: &mut Tick<'_, '_>, g: &mut MoveGoal) -> bool {
    match g {
        MoveGoal::Wander {
            mode,
            interval,
            check_no_action_time,
            force,
            wanted,
            ..
        } => {
            if has_controlling_passenger(t) {
                return false;
            }
            if !*force {
                if *check_no_action_time && t.mob_ref().no_action_time >= 100 {
                    return false;
                }
                if t.mob().random.next_int_bounded(reduced(*interval).max(1)) != 0 {
                    return false;
                }
            }
            let pos = match mode {
                WanderMode::Stroll => randompos::default_pos(t, 10, 7),
                WanderMode::Swim => randompos::swimmable_pos(t, 10, 7),
                WanderMode::Fly => randompos::water_avoiding_flying_pos(t),
            };
            match pos {
                Some(p) => {
                    *wanted = p;
                    *force = false;
                    true
                }
                None => false,
            }
        }
        MoveGoal::TryFindLiquid { lava } => {
            let state = t.level.block(t.block_pos());
            let in_liquid = if *lava {
                super::is_lava(state)
            } else {
                super::is_water(state)
            };
            t.on_ground() && !in_liquid
        }
        MoveGoal::BreathAir => t.mob_ref().air < 140,
        MoveGoal::ClimbOnTopOfPowderSnow => {
            use super::Kind;
            let walkable = matches!(
                t.mob_ref().kind,
                Kind::Rabbit | Kind::Endermite | Kind::Silverfish | Kind::Fox
            );
            if !walkable || !in_powder_snow(t) {
                return false;
            }
            let b = t.block_pos();
            let above = t.level.block(BlockPos::new(b.x, b.y + 1, b.z));
            above.block() == ids::POWDER_SNOW || above.collision_shape().is_empty()
        }
        MoveGoal::OpenDoor { door, .. } => door_can_use(t, door),
        MoveGoal::BreakDoor { door, valid, .. } => {
            if !door_can_use(t, door) || !t.env.rules.bool(Rule::MOB_GRIEFING) {
                return false;
            }
            valid(t.env.difficulty) && !door_is_open(t, door)
        }
        MoveGoal::RemoveBlock { mtb, block, .. } => {
            if !t.env.rules.bool(Rule::MOB_GRIEFING) {
                return false;
            }
            if mtb.next_start_tick > 0 {
                mtb.next_start_tick -= 1;
                return false;
            }
            if mtb.find_nearest_block(t, remove_filter(*block)) {
                mtb.next_start_tick = reduced(20);
                true
            } else {
                mtb.next_start_tick = MoveToBlock::default_next_start_tick(t);
                false
            }
        }
    }
}

pub fn can_continue(t: &mut Tick<'_, '_>, g: &mut MoveGoal) -> bool {
    match g {
        MoveGoal::Wander { .. } => !t.mob_ref().nav.is_done() && !has_controlling_passenger(t),
        MoveGoal::TryFindLiquid { .. } | MoveGoal::BreathAir | MoveGoal::ClimbOnTopOfPowderSnow => {
            can_use(t, g)
        }
        MoveGoal::OpenDoor {
            door,
            close,
            forget_time,
        } => *close && *forget_time > 0 && !door.passed,
        MoveGoal::BreakDoor {
            door,
            break_time,
            door_break_time,
            valid,
            ..
        } => {
            let p = t.pos();
            let c = door.pos;
            let center = Vec3::new(
                f64::from(c.x) + 0.5,
                f64::from(c.y) + 0.5,
                f64::from(c.z) + 0.5,
            );
            *break_time <= (*door_break_time).max(240)
                && !door_is_open(t, door)
                && center.distance_to_sqr(p) < 4.0
                && valid(t.env.difficulty)
        }
        MoveGoal::RemoveBlock { mtb, block, .. } => mtb.can_continue(t, remove_filter(*block)),
    }
}

pub fn start(t: &mut Tick<'_, '_>, g: &mut MoveGoal) {
    match g {
        MoveGoal::Wander { speed, wanted, .. } => {
            nav::move_to_pos(t, *wanted, *speed);
        }
        MoveGoal::TryFindLiquid { lava } => {
            let p = t.pos();
            let floor = vanillite_base::math::floor;
            let (x0, y0, z0) = (floor(p.x - 2.0), floor(p.y - 2.0), floor(p.z - 2.0));
            let (x1, y1, z1) = (floor(p.x + 2.0), t.block_pos().y, floor(p.z + 2.0));
            // `BlockPos.betweenClosed`: x fastest, then y, then z.
            let mut found = None;
            'search: for z in z0..=z1 {
                for y in y0..=y1 {
                    for x in x0..=x1 {
                        let state = t.level.block(BlockPos::new(x, y, z));
                        let liquid = if *lava {
                            super::is_lava(state)
                        } else {
                            super::is_water(state)
                        };
                        if liquid {
                            found = Some(BlockPos::new(x, y, z));
                            break 'search;
                        }
                    }
                }
            }
            if let Some(f) = found {
                let at = Vec3::new(f64::from(f.x), f64::from(f.y), f64::from(f.z));
                t.mob().move_ctl.set_wanted_position(at, 1.0);
            }
        }
        MoveGoal::BreathAir => {
            t.mob().nav.stop();
            find_air_position(t);
        }
        MoveGoal::OpenDoor {
            door, forget_time, ..
        } => {
            // `OpenDoorGoal.start` replaces `DoorInteractGoal.start`.
            *forget_time = 20;
            let d = *door;
            door_set_open(t, &d, true);
        }
        MoveGoal::BreakDoor {
            door, break_time, ..
        } => {
            door_start(t, door);
            *break_time = 0;
        }
        MoveGoal::RemoveBlock {
            mtb,
            ticks_since_reached,
            ..
        } => {
            mtb.start(t);
            *ticks_since_reached = 0;
        }
        MoveGoal::ClimbOnTopOfPowderSnow => {}
    }
}

pub fn stop(t: &mut Tick<'_, '_>, g: &mut MoveGoal) {
    match g {
        MoveGoal::Wander { .. } => t.mob().nav.stop(),
        MoveGoal::OpenDoor { door, .. } => {
            let d = *door;
            door_set_open(t, &d, false);
        }
        MoveGoal::BreakDoor { door, .. } => {
            let pos = door.pos;
            destroy_progress(t, pos, -1);
        }
        MoveGoal::RemoveBlock { .. } => t.mob().fall_distance = 1.0,
        _ => {}
    }
}

pub fn tick(t: &mut Tick<'_, '_>, g: &mut MoveGoal) {
    match g {
        MoveGoal::BreathAir => {
            find_air_position(t);
            let m = t.mob_ref();
            let input = Vec3::new(f64::from(m.xxa), f64::from(m.yya), f64::from(m.zza));
            living::move_relative(t, 0.02, input);
            let v = t.velocity();
            living::move_self(t, v);
        }
        MoveGoal::ClimbOnTopOfPowderSnow => t.mob().jump = true,
        MoveGoal::OpenDoor {
            door, forget_time, ..
        } => {
            *forget_time -= 1;
            door_tick(t, door);
        }
        MoveGoal::BreakDoor {
            door,
            break_time,
            last_progress,
            door_break_time,
            valid,
        } => {
            door_tick(t, door);
            if t.mob().random.next_int_bounded(20) == 0 {
                t.cx.actions
                    .push(WorldAction::LevelEvent(1019, door.pos, 0));
                swing(t);
            }
            *break_time += 1;
            let total = (*door_break_time).max(240);
            let progress = break_progress(*break_time, total);
            if progress != *last_progress {
                let pos = door.pos;
                destroy_progress(t, pos, progress);
                *last_progress = progress;
            }
            if *break_time == total && valid(t.env.difficulty) {
                let pos = door.pos;
                t.cx.actions
                    .push(WorldAction::SetBlock(pos, BlockState::AIR, 3));
                t.cx.actions.push(WorldAction::LevelEvent(1021, pos, 0));
                // `Block.getId(getBlockState(pos))` after the removal: air.
                t.cx.actions.push(WorldAction::LevelEvent(2001, pos, 0));
            }
        }
        MoveGoal::RemoveBlock {
            mtb,
            block,
            ticks_since_reached,
            sounds,
            accepted_distance,
        } => {
            let b = mtb.block_pos;
            let target = BlockPos::new(b.x, b.y + 1, b.z);
            mtb.tick(t, target, *accepted_distance);
            let eat = pos_with_block(t, t.block_pos(), *block);
            if !(mtb.reached_target && eat.is_some()) {
                return;
            }
            let Some(eat) = eat else {
                return;
            };
            if *ticks_since_reached > 0 {
                let v = t.velocity();
                t.set_velocity(Vec3::new(v.x, 0.3, v.z));
                // `sendParticles(ITEM egg)`: the crumbs.
                let r = &mut t.mob().random;
                let dist = Vec3::new(
                    f64::from((r.next_float() - 0.5) * 0.08),
                    f64::from((r.next_float() - 0.5) * 0.08),
                    f64::from((r.next_float() - 0.5) * 0.08),
                );
                let at = Vec3::new(
                    f64::from(eat.x) + 0.5,
                    f64::from(eat.y) + 0.7,
                    f64::from(eat.z) + 0.5,
                );
                let egg = crate::level::particles::item(vanillite_data::item::ids::EGG.0);
                t.cx.send_particles(egg, at, 3, dist, f64::from(0.15_f32));
            }
            if *ticks_since_reached % 2 == 0 {
                let v = t.velocity();
                t.set_velocity(Vec3::new(v.x, -0.3, v.z));
                if *ticks_since_reached % 6 == 0 && *sounds == RemoveSounds::ZombieEgg {
                    let pitch = 0.9 + t.mob().random.next_float() * 0.2;
                    block_sound(
                        t,
                        mtb.block_pos,
                        "minecraft:entity.zombie.destroy_egg",
                        SoundSource::Hostile,
                        0.5,
                        pitch,
                    );
                }
            }
            if *ticks_since_reached > 60 {
                t.cx.actions
                    .push(WorldAction::SetBlock(eat, BlockState::AIR, 3));
                let at = Vec3::new(
                    f64::from(eat.x) + 0.5,
                    f64::from(eat.y),
                    f64::from(eat.z) + 0.5,
                );
                for _ in 0..20 {
                    let r = &mut t.mob().random;
                    let dist = Vec3::new(
                        r.next_gaussian() * 0.02,
                        r.next_gaussian() * 0.02,
                        r.next_gaussian() * 0.02,
                    );
                    let poof = crate::level::particles::simple("minecraft:poof");
                    t.cx.send_particles(poof, at, 1, dist, f64::from(0.15_f32));
                }
                if *sounds == RemoveSounds::ZombieEgg {
                    let pitch = 0.9 + t.env.random.next_float() * 0.2;
                    block_sound(
                        t,
                        eat,
                        "minecraft:entity.turtle.egg_break",
                        SoundSource::Blocks,
                        0.7,
                        pitch,
                    );
                }
            }
            *ticks_since_reached += 1;
        }
        _ => {}
    }
}

/// `BreakDoorGoal`'s crack stage: `(int) ((float) breakTime / doorBreakTime * 10.0F)`.
pub fn break_progress(break_time: i32, total: i32) -> i32 {
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    let progress = (break_time as f32 / total as f32 * 10.0) as i32;
    progress
}

/// `swingForAttack` when not swinging: the arm swing animation.
fn swing(t: &mut Tick<'_, '_>) {
    crate::mob::brain::behavior::swing_for_attack(t);
}

/// `level.playSound(null, pos, sound, source, volume, pitch)` at a block.
fn block_sound(
    t: &mut Tick<'_, '_>,
    pos: BlockPos,
    sound: &str,
    source: SoundSource,
    volume: f32,
    pitch: f32,
) {
    let at = Vec3::new(
        f64::from(pos.x) + 0.5,
        f64::from(pos.y) + 0.5,
        f64::from(pos.z) + 0.5,
    );
    let seed = crate::survival::sound_seed();
    crate::survival::play_sound(t.env.out, None, at, sound, source, volume, pitch, seed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn door_cracks_in_ten_stages_over_240_ticks() {
        assert_eq!(break_progress(1, 240), 0);
        assert_eq!(break_progress(24, 240), 1);
        assert_eq!(break_progress(239, 240), 9);
        assert_eq!(break_progress(240, 240), 10);
        assert_eq!(break_progress(120, 240), 5);
    }

    #[test]
    fn break_door_goal_only_on_ground_navigation() {
        let mut m = MobData::new(super::super::Kind::Zombie, 1);
        let before = m.goals.slots.len();
        set_can_break_doors(&mut m, true);
        assert!(m.can_break_doors && m.nav.can_open_doors);
        assert_eq!(m.goals.slots.len(), before + 1);
        set_can_break_doors(&mut m, false);
        assert_eq!(m.goals.slots.len(), before);
        m.nav.kind = super::super::nav::NavKind::Water { breaching: false };
        set_can_break_doors(&mut m, true);
        assert!(!m.can_break_doors);
    }

    #[test]
    fn zombies_trample_turtle_eggs_first() {
        let m = MobData::new(super::super::Kind::Zombie, 1);
        assert!(matches!(
            m.goals.slots.first().map(|s| s.goal),
            Some(Goal::Common(super::super::common::CommonGoal::Move(
                MoveGoal::RemoveBlock { .. }
            )))
        ));
    }
}
