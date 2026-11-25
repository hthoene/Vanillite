//! Golem building (`CarvedPumpkinBlock.onPlace` → `trySpawnGolem`): a carved pumpkin or jack
//! o'lantern on two snow blocks makes a snow golem, on a T of iron blocks an iron golem, on a
//! copper block a copper golem (the copper block turns into a copper chest).

use vanillite_base::{BlockPos, Direction, Vec3};
use vanillite_data::block::{BlockId, BlockState, ids as block_ids};
use vanillite_world::entity::{EntityId, Rotation};
use vanillite_worldgen::behaviour::StateExt;

use super::{Kind, MobData};
use crate::level::{Level, update};

/// A cell of a golem pattern.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cell {
    /// `^`: a carved pumpkin or jack o'lantern.
    Pumpkin,
    /// `#`: the body block.
    Body,
    /// `~`: air.
    Air,
}

use Cell::{Air, Body, Pumpkin};

const SNOW: [&[Cell]; 3] = [&[Pumpkin], &[Body], &[Body]];
const IRON: [&[Cell]; 3] = [&[Air, Pumpkin, Air], &[Body, Body, Body], &[Air, Body, Air]];
const COPPER: [&[Cell]; 2] = [&[Pumpkin], &[Body]];

/// A found pattern (`BlockPattern.BlockPatternMatch`).
#[derive(Clone, Copy, Debug)]
struct Match {
    front_top_left: BlockPos,
    forwards: Direction,
    up: Direction,
    width: i32,
    height: i32,
}

impl Match {
    /// `getBlock(right, down, 0)` (`BlockPattern.translateAndRotate`).
    fn block(&self, right: i32, down: i32) -> BlockPos {
        translate(self.front_top_left, self.forwards, self.up, right, down)
    }
}

fn translate(
    origin: BlockPos,
    forwards: Direction,
    up: Direction,
    right: i32,
    down: i32,
) -> BlockPos {
    let [fx, fy, fz] = forwards.normal();
    let [ux, uy, uz] = up.normal();
    let (rx, ry, rz) = (fy * uz - fz * uy, fz * ux - fx * uz, fx * uy - fy * ux);
    BlockPos::new(
        origin.x - ux * down + rx * right,
        origin.y - uy * down + ry * right,
        origin.z - uz * down + rz * right,
    )
}

fn is_pumpkin(state: BlockState) -> bool {
    state.is(block_ids::CARVED_PUMPKIN) || state.is(block_ids::JACK_O_LANTERN)
}

/// `BlockPattern.find(level, pos)`: the first match around `origin`, positions in
/// `betweenClosed` order, then every `forwards` and `up` direction.
fn find(
    level: &Level,
    origin: BlockPos,
    rows: &[&[Cell]],
    body: &dyn Fn(BlockState) -> bool,
) -> Option<Match> {
    let height = i32::try_from(rows.len()).ok()?;
    let width = i32::try_from(rows.first()?.len()).ok()?;
    let size = width.max(height);
    let cell_ok = |cell: Cell, pos: BlockPos| {
        if !level.is_loaded(pos) {
            return false;
        }
        let state = level.block(pos);
        match cell {
            Pumpkin => is_pumpkin(state),
            Body => body(state),
            Air => state.is_air(),
        }
    };
    for dz in 0..size {
        for dy in 0..size {
            for dx in 0..size {
                let test = BlockPos::new(origin.x + dx, origin.y + dy, origin.z + dz);
                for forwards in Direction::ALL {
                    for up in Direction::ALL {
                        if up == forwards || up == forwards.opposite() {
                            continue;
                        }
                        let all = rows.iter().zip(0..).all(|(row, y)| {
                            row.iter().zip(0..).all(|(&cell, x)| {
                                cell_ok(cell, translate(test, forwards, up, x, y))
                            })
                        });
                        if all {
                            return Some(Match {
                                front_top_left: test,
                                forwards,
                                up,
                                width,
                                height,
                            });
                        }
                    }
                }
            }
        }
    }
    None
}

/// `CarvedPumpkinBlock.onPlace`: a pumpkin replacing another block tries to finish a golem.
pub fn pumpkin_on_place(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    old: BlockState,
    _: bool,
) {
    if !old.is(state.block()) {
        try_spawn_golem(level, pos);
    }
}

fn is_copper(state: BlockState) -> bool {
    crate::block::block_tags().copper.contains(state)
}

/// `CarvedPumpkinBlock.trySpawnGolem`.
pub fn try_spawn_golem(level: &mut Level, top: BlockPos) -> Option<EntityId> {
    let snow = |s: BlockState| s.is(block_ids::SNOW_BLOCK);
    if let Some(m) = find(level, top, &SNOW, &snow) {
        let spawn = m.block(0, 2);
        return Some(spawn_golem(
            level,
            &m,
            MobData::new(Kind::SnowGolem, super::new_entity_seed()),
            spawn,
        ));
    }
    let iron = |s: BlockState| s.is(block_ids::IRON_BLOCK);
    if let Some(m) = find(level, top, &IRON, &iron) {
        let mut golem = MobData::new(Kind::IronGolem, super::new_entity_seed());
        super::fam::wildlife::st_mut(&mut golem).player_created = true;
        let spawn = m.block(1, 2);
        return Some(spawn_golem(level, &m, golem, spawn));
    }
    if let Some(m) = find(level, top, &COPPER, &is_copper) {
        // The pattern's cached states: the copper block and the pumpkin before clearing.
        let copper = level.block(m.block(0, 1));
        let facing = level
            .block(m.block(0, 0))
            .direction("facing")
            .unwrap_or(Direction::North);
        let mut golem = MobData::new(Kind::CopperGolem, super::new_entity_seed());
        super::fam::mount::copper_golem::golem_mut(&mut golem).weather = weather_of(copper.block());
        let spawn = m.block(0, 0);
        let id = spawn_golem(level, &m, golem, spawn);
        // `replaceCopperBlockWithChest` (a single chest).
        if let Some(chest) = copper_chest_of(copper.block()) {
            let state = chest.default_state().set_direction("facing", facing);
            level.set_block(m.block(0, 1), state, update::CLIENTS);
        }
        // `CopperGolem.spawn`: `playSpawnSound`.
        let at = Vec3::new(
            f64::from(spawn.x) + 0.5,
            f64::from(spawn.y) + 0.05,
            f64::from(spawn.z) + 0.5,
        );
        let sound = "entity.copper_golem.spawn";
        level.play_sound(
            None,
            at.x,
            at.y,
            at.z,
            crate::block::sound_id(sound),
            crate::level::SOUND_NEUTRAL,
            1.0,
            1.0,
        );
        return Some(id);
    }
    None
}

/// `getWeatherStateFromPattern`: the age of the copper block (waxed blocks count as their
/// unwaxed kind).
fn weather_of(block: BlockId) -> super::fam::mount::copper_golem::Weather {
    use super::fam::mount::copper_golem::Weather;
    let name = block
        .name()
        .trim_start_matches("minecraft:")
        .trim_start_matches("waxed_");
    match name {
        "exposed_copper" => Weather::Exposed,
        "weathered_copper" => Weather::Weathered,
        "oxidized_copper" => Weather::Oxidized,
        _ => Weather::Unaffected,
    }
}

/// `COPPER_TO_COPPER_CHEST_MAPPING`.
fn copper_chest_of(block: BlockId) -> Option<BlockId> {
    let name = block.name().trim_start_matches("minecraft:");
    let chest = match name {
        "copper_block" => "copper_chest".to_owned(),
        "waxed_copper_block" => "waxed_copper_chest".to_owned(),
        other => format!("{other}_chest"),
    };
    BlockId::by_name(&format!("minecraft:{chest}"))
        .or_else(|| BlockId::by_name("minecraft:copper_chest"))
}

/// `spawnGolemInWorld`: clears the pattern, adds the golem at `spawn` and updates the
/// neighbours.
fn spawn_golem(level: &mut Level, m: &Match, golem: MobData, spawn: BlockPos) -> EntityId {
    // `clearPatternBlocks`.
    for x in 0..m.width {
        for y in 0..m.height {
            let p = m.block(x, y);
            let state = level.block(p);
            level.set_block(p, BlockState::AIR, update::CLIENTS);
            level.level_event(None, 2001, p, i32::from(state.0));
        }
    }
    let uuid = super::new_uuid();
    let network_ids = std::sync::Arc::clone(level.network_ids_arc());
    let id = super::spawn(
        &mut level.entities,
        &network_ids,
        uuid,
        Vec3::new(
            f64::from(spawn.x) + 0.5,
            f64::from(spawn.y) + 0.05,
            f64::from(spawn.z) + 0.5,
        ),
        Rotation::default(),
        golem,
    );
    // `updatePatternBlocks`.
    for x in 0..m.width {
        for y in 0..m.height {
            level.update_neighbors_at(m.block(x, y), block_ids::AIR);
        }
    }
    id
}
