//! `MonsterRoomFeature`: the dungeon with a spawner and up to two loot chests.

use serde_json::Value;
use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockState, ids};
use vanillite_nbt::{Compound, List, Tag};

use super::{HORIZONTAL, direction_name, is, safe_set_block};
use crate::Error;
use crate::behaviour::{is_solid, is_solid_render};
use crate::blocks::BlockSet;
use crate::feature::region::{WorldGenRegion, block_entity_type, flags};
use crate::feature::{FeatureContext, FeatureParser, FeatureType};

/// `MOBS`: the spawner's entity, picked uniformly (zombie twice).
const MOBS: [&str; 4] = [
    "minecraft:skeleton",
    "minecraft:zombie",
    "minecraft:zombie",
    "minecraft:spider",
];

/// `BuiltInLootTables.SIMPLE_DUNGEON`.
const SIMPLE_DUNGEON: &str = "minecraft:chests/simple_dungeon";

/// `monster_room`.
#[derive(Debug)]
pub struct MonsterRoomFeature {
    /// `#minecraft:features_cannot_replace`.
    cannot_replace: BlockSet,
}

impl FeatureType for MonsterRoomFeature {
    fn parse(_json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        Ok(Self {
            cannot_replace: parser.block_tag("minecraft:features_cannot_replace"),
        })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let level = &mut *ctx.level;
        let random = &mut *ctx.random;
        let replaceable = |s: BlockState| !self.cannot_replace.contains(s);
        let xr = random.next_int_bounded(2) + 2;
        let (min_x, max_x) = (-xr - 1, xr + 1);
        let zr = random.next_int_bounded(2) + 2;
        let (min_z, max_z) = (-zr - 1, zr + 1);

        // Vanilla reads every block of the box but only uses the floor and ceiling (all solid)
        // and the openings of the wall ring at `dy == 0` (one to five), and every failure
        // returns `false`: the same answer from those blocks alone, openings first.
        let mut hole_count = 0;
        for dx in min_x..=max_x {
            for dz in min_z..=max_z {
                if (dx == min_x || dx == max_x || dz == min_z || dz == max_z)
                    && level.is_empty_block(origin.offset(dx, 0, dz))
                    && level.is_empty_block(origin.offset(dx, 1, dz))
                {
                    hole_count += 1;
                }
            }
        }
        if !(1..=5).contains(&hole_count) {
            return false;
        }
        for dx in min_x..=max_x {
            for dz in min_z..=max_z {
                if !is_solid(level.block(origin.offset(dx, -1, dz)))
                    || !is_solid(level.block(origin.offset(dx, 4, dz)))
                {
                    return false;
                }
            }
        }

        let cave_air = ids::CAVE_AIR.default_state();
        for dx in min_x..=max_x {
            for dy in (-1..=3).rev() {
                for dz in min_z..=max_z {
                    let pos = origin.offset(dx, dy, dz);
                    let state = level.block(pos);
                    if dx == min_x || dy == -1 || dz == min_z || dx == max_x || dz == max_z {
                        if pos.y >= level.min_y()
                            && !is_solid(level.block(pos.relative(Direction::Down)))
                        {
                            level.set_block(pos, cave_air, flags::CLIENTS);
                        } else if is_solid(state) && !is(state, ids::CHEST) {
                            let wall = if dy == -1 && random.next_int_bounded(4) != 0 {
                                ids::MOSSY_COBBLESTONE
                            } else {
                                ids::COBBLESTONE
                            };
                            safe_set_block(level, pos, wall.default_state(), replaceable);
                        }
                    } else if !is(state, ids::CHEST) && !is(state, ids::SPAWNER) {
                        safe_set_block(level, pos, cave_air, replaceable);
                    }
                }
            }
        }

        for _ in 0..2 {
            for _ in 0..3 {
                let x = origin.x + random.next_int_bounded(xr * 2 + 1) - xr;
                let z = origin.z + random.next_int_bounded(zr * 2 + 1) - zr;
                let chest_pos = BlockPos::new(x, origin.y, z);
                if !level.is_empty_block(chest_pos) {
                    continue;
                }
                let walls = HORIZONTAL
                    .iter()
                    .filter(|&&d| is_solid(level.block(chest_pos.relative(d))))
                    .count();
                if walls == 1 {
                    let chest = reorient(level, chest_pos, ids::CHEST.default_state());
                    safe_set_block(level, chest_pos, chest, replaceable);
                    set_block_entity_loot_table(level, random, chest_pos, SIMPLE_DUNGEON);
                    break;
                }
            }
        }

        safe_set_block(level, origin, ids::SPAWNER.default_state(), replaceable);
        if is(level.block(origin), ids::SPAWNER)
            && let Some(spawner) = level.block_entity_mut(origin)
        {
            let mob = MOBS[random.next_int_bounded(MOBS.len() as i32) as usize];
            set_spawner_entity_id(spawner, mob);
        }
        true
    }
}

/// `StructurePiece.reorient`: faces a chest away from its single solid neighbour, or away from
/// walls in front of it.
fn reorient(level: &WorldGenRegion<'_>, pos: BlockPos, state: BlockState) -> BlockState {
    let mut solid_neighbour = None;
    for direction in HORIZONTAL {
        let neighbour = level.block(pos.relative(direction));
        if is(neighbour, ids::CHEST) {
            return state;
        }
        if is_solid_render(neighbour) {
            if solid_neighbour.is_some() {
                solid_neighbour = None;
                break;
            }
            solid_neighbour = Some(direction);
        }
    }
    let facing = match solid_neighbour {
        Some(direction) => direction.opposite(),
        None => {
            let solid = |d: Direction| is_solid_render(level.block(pos.relative(d)));
            let mut lock = horizontal_facing(state).unwrap_or(Direction::North);
            if solid(lock) {
                lock = lock.opposite();
            }
            if solid(lock) {
                lock = clockwise(lock);
            }
            if solid(lock) {
                lock = lock.opposite();
            }
            lock
        }
    };
    state
        .with("facing", direction_name(facing))
        .unwrap_or(state)
}

/// The `facing` property of a horizontally facing state.
fn horizontal_facing(state: BlockState) -> Option<Direction> {
    Some(match state.value("facing")? {
        "north" => Direction::North,
        "south" => Direction::South,
        "west" => Direction::West,
        "east" => Direction::East,
        _ => return None,
    })
}

/// `Direction.getClockWise()` for horizontal directions.
fn clockwise(direction: Direction) -> Direction {
    match direction {
        Direction::North => Direction::East,
        Direction::East => Direction::South,
        Direction::South => Direction::West,
        Direction::West => Direction::North,
        other => other,
    }
}

/// `RandomizableContainer.setBlockEntityLootTable(level, random, pos, lootTable)`: for a
/// container block entity, stores the loot table and a seed from `random.nextLong()` as
/// `ChestBlockEntity.saveAdditional` writes them (`LootTable`, `LootTableSeed` unless 0).
fn set_block_entity_loot_table<R: RandomSource>(
    level: &mut WorldGenRegion<'_>,
    random: &mut R,
    pos: BlockPos,
    loot_table: &str,
) {
    if !is_randomizable_container(level.block(pos)) {
        return;
    }
    let Some(tag) = level.block_entity_mut(pos) else {
        return;
    };
    let seed = random.next_long();
    tag.insert("LootTable", Tag::String(loot_table.to_owned()));
    if seed != 0 {
        tag.insert("LootTableSeed", Tag::Long(seed));
    } else {
        tag.remove("LootTableSeed");
    }
}

/// Whether the state's block entity implements `RandomizableContainer`.
fn is_randomizable_container(state: BlockState) -> bool {
    matches!(
        block_entity_type(state),
        Some(
            "minecraft:chest"
                | "minecraft:trapped_chest"
                | "minecraft:barrel"
                | "minecraft:shulker_box"
                | "minecraft:dispenser"
                | "minecraft:dropper"
                | "minecraft:hopper"
                | "minecraft:crafter"
                | "minecraft:decorated_pot"
        )
    )
}

/// `SpawnerBlockEntity.setEntityId(type, random)` on the saved data: a fresh spawner gets
/// `BaseSpawner`'s default fields; `SpawnData.entity.id` is set. The spawner has no spawn
/// potentials, so no random value is used.
fn set_spawner_entity_id(tag: &mut Compound, entity: &str) {
    if tag.get("Delay").is_none() {
        tag.insert("Delay", Tag::Short(20));
        tag.insert("MinSpawnDelay", Tag::Short(200));
        tag.insert("MaxSpawnDelay", Tag::Short(800));
        tag.insert("SpawnCount", Tag::Short(4));
        tag.insert("MaxNearbyEntities", Tag::Short(6));
        tag.insert("RequiredPlayerRange", Tag::Short(16));
        tag.insert("SpawnRange", Tag::Short(4));
        tag.insert("SpawnPotentials", Tag::List(List::new()));
    }
    let mut spawn_data = match tag.remove("SpawnData") {
        Some(Tag::Compound(data)) => data,
        _ => Compound::new(),
    };
    let mut entity_tag = match spawn_data.remove("entity") {
        Some(Tag::Compound(e)) => e,
        _ => Compound::new(),
    };
    entity_tag.insert("id", Tag::String(entity.to_owned()));
    spawn_data.insert("entity", Tag::Compound(entity_tag));
    tag.insert("SpawnData", Tag::Compound(spawn_data));
}
