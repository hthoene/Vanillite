//! Deterministic simulation tests on a classic superflat world (grass at Y -61).

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use vanillite_base::{BlockPos, ChunkPos};
use vanillite_chunk::ProtoChunk;
use vanillite_chunk::slots::{ChunkStorage, FlatGenerator, LightEngine, Neighbours, WorldInfo};
use vanillite_data::block::{BlockState, ids};
use vanillite_data::entity::ids as entity_ids;
use vanillite_world::chunks::{ChunkSettings, ChunkSlots, Chunks, InlineExecutor};
use vanillite_world::entity::NetworkIds;
use vanillite_worldgen::behaviour::StateExt;

use super::{Level, LevelSettings, update};
use crate::block::{Behaviours, falling};

struct NoStorage;

impl ChunkStorage for NoStorage {
    fn load(&self, positions: &[ChunkPos], out: &mut Vec<io::Result<Option<ProtoChunk>>>) {
        out.extend(positions.iter().map(|_| Ok(None)));
    }
    fn save(&self, chunks: &[Arc<ProtoChunk>], out: &mut Vec<io::Result<()>>) {
        out.extend(chunks.iter().map(|_| Ok(())));
    }
    fn flush(&self) -> io::Result<()> {
        Ok(())
    }
}

struct NoLight;

impl LightEngine for NoLight {
    fn initialize_light(&self, _: &mut [ProtoChunk]) {}
    fn light_chunks(&self, _: &mut [ProtoChunk], _: &mut Neighbours<'_>) {}
    fn relight(&self, _: &[ChunkPos], _: &mut Neighbours<'_>) {}
}

/// Loads the block tags behaviour reads from the Vanilla data pack, extracted into
/// `$VANILLITE_CACHE` (default `<workspace>/target/vanilla-cache`) like the worldgen goldens.
fn install_tags() {
    static DONE: OnceLock<()> = OnceLock::new();
    DONE.get_or_init(|| {
        let cache = std::env::var_os("VANILLITE_CACHE").map_or_else(
            || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/vanilla-cache"),
            PathBuf::from,
        );
        let jar = std::env::var_os("VANILLITE_SERVER_JAR").map(PathBuf::from);
        let pack = vanillite_datapack::vanilla::ensure_vanilla_pack(&cache, jar.as_deref())
            .expect("Vanilla data pack (set VANILLITE_SERVER_JAR for an offline copy)");
        let packs = vanillite_datapack::load(&[pack.as_path()]).expect("load Vanilla data pack");
        vanillite_worldgen::behaviour::init(&packs);
    });
}

/// A flat world with the chunks around the origin full and block ticking.
pub(crate) fn flat_level() -> Level {
    flat_level_with(Behaviours::vanilla())
}

/// [`flat_level`] with the given block behaviours.
pub(crate) fn flat_level_with(behaviours: Behaviours) -> Level {
    vanillite_chunk::facts::install_vanilla(&[], &[], 1);
    install_tags();
    let info = WorldInfo {
        dimension: "minecraft:overworld".to_owned(),
        dir: std::env::temp_dir(),
        min_y: -64,
        height: 384,
        seed: 0,
        has_sky_light: true,
        biomes: Arc::from(["minecraft:plains".to_owned()]),
        default_biome: 0,
        air: BlockState::AIR.0,
    };
    let slots = ChunkSlots {
        storage: Arc::new(NoStorage),
        generator: Arc::new(FlatGenerator::classic(&info)),
        light: Arc::new(NoLight),
        executor: Arc::new(InlineExecutor),
    };
    let settings = ChunkSettings {
        view_distance: 3,
        simulation_distance: 3,
        ..ChunkSettings::default()
    };
    let mut chunks = Chunks::new(info, settings, slots);
    let center = ChunkPos::new(0, 0);
    let level = chunks.player_ticket_level();
    chunks.add_ticket(center, level);
    let simulation = chunks.player_simulation_level();
    chunks.add_simulation_ticket(center, simulation);
    for _ in 0..4 {
        chunks.tick();
    }
    assert!(chunks.is_block_ticking(center));
    Level::new(
        chunks,
        LevelSettings::default(),
        Arc::new(behaviours),
        Arc::new(NetworkIds::new()),
        0,
    )
}

/// One tick of the simulation phases and falling block entities.
pub(crate) fn tick(level: &mut Level) {
    level.tick_time();
    level.run_scheduled_ticks();
    level.chunks.tick();
    level.run_block_events();
    let mut cursor = level.entities.store.tick_cursor();
    while let Some(id) = level.entities.store.next_to_tick(&mut cursor) {
        let kind = level.entities.store.get(id).map(|e| e.kind());
        if kind == Some(entity_ids::FALLING_BLOCK) {
            falling::tick_entity(level, id);
        }
    }
    level.tick_block_entities();
}

fn level_of(state: BlockState) -> Option<&'static str> {
    state.value("level")
}

#[test]
fn water_spreads_seven_blocks_and_lava_source_turns_to_obsidian() {
    let mut level = flat_level();
    let source = BlockPos::new(0, -60, 0);
    assert!(level.set_block(source, ids::WATER.default_state(), update::ALL));
    for _ in 0..100 {
        tick(&mut level);
    }
    for distance in 1..=7 {
        let state = level.block(BlockPos::new(distance, -60, 0));
        assert!(state.is(ids::WATER), "distance {distance}: {state:?}");
        assert_eq!(level_of(state), Some(distance.to_string().as_str()));
    }
    assert!(level.block(BlockPos::new(8, -60, 0)).is_air());
    // Diagonal neighbours are reached around the corner.
    assert_eq!(level_of(level.block(BlockPos::new(3, -60, 3))), Some("6"));

    // `LiquidBlock.shouldSpreadLiquid`: a lava source next to water becomes obsidian, flowing
    // lava becomes cobblestone.
    let lava = BlockPos::new(2, -60, 0);
    level.set_block(lava, ids::LAVA.default_state(), update::ALL);
    assert!(level.block(lava).is(ids::OBSIDIAN));
    let flowing = BlockPos::new(-2, -60, 0);
    let flowing_lava = ids::LAVA.default_state().with("level", "2").unwrap();
    level.set_block(flowing, flowing_lava, update::ALL);
    assert!(level.block(flowing).is(ids::COBBLESTONE));
}

#[test]
fn sand_falls_as_entity_and_lands_on_the_ground() {
    let mut level = flat_level();
    let start = BlockPos::new(5, -50, 5);
    level.set_block(start, ids::SAND.default_state(), update::ALL);
    // `FallingBlock.onPlace` schedules the fall in 2 ticks.
    tick(&mut level);
    assert!(level.block(start).is(ids::SAND));
    tick(&mut level);
    assert!(level.block(start).is_air());
    assert_eq!(level.entities.store.len(), 1);
    for _ in 0..60 {
        tick(&mut level);
    }
    assert!(level.block(BlockPos::new(5, -60, 5)).is(ids::SAND));
    assert_eq!(level.entities.store.len(), 0);
}

mod pistons {
    use vanillite_base::{BlockPos, Direction};
    use vanillite_data::block::{BlockState, ids};
    use vanillite_worldgen::behaviour::StateExt;

    use super::{flat_level_with, tick};
    use crate::block::{Behaviours, BlockBehaviour};
    use crate::level::{Level, update};

    /// Vanilla's behaviours with a redstone block that powers its neighbours (`PoweredBlock`),
    /// independent of the redstone port.
    fn level() -> Level {
        let mut behaviours = Behaviours::vanilla();
        behaviours.set(
            ids::REDSTONE_BLOCK,
            BlockBehaviour {
                signal: Some(|_, _, _, _| 15),
                ..*behaviours.get(ids::REDSTONE_BLOCK)
            },
        );
        flat_level_with(behaviours)
    }

    fn run(level: &mut Level, ticks: usize) {
        for _ in 0..ticks {
            tick(level);
            level.tick_block_entities();
        }
    }

    fn piston(block: vanillite_data::block::BlockId, facing: Direction) -> BlockState {
        block.default_state().set_direction("facing", facing)
    }

    const Y: i32 = -60;

    fn at(x: i32) -> BlockPos {
        BlockPos::new(x, Y, 0)
    }

    #[test]
    fn piston_extends_and_retracts() {
        let mut level = level();
        level.set_block_and_update(at(0), piston(ids::PISTON, Direction::East));
        level.set_block_and_update(at(1), ids::STONE.default_state());
        let power = BlockPos::new(0, Y, 1);
        level.set_block_and_update(power, ids::REDSTONE_BLOCK.default_state());
        run(&mut level, 4);
        assert!(level.block(at(0)).flag("extended"));
        assert!(level.block(at(1)).is(ids::PISTON_HEAD));
        assert!(level.block(at(2)).is(ids::STONE));
        assert!(level.block_entities.is_empty());

        level.set_block_and_update(power, BlockState::AIR);
        run(&mut level, 4);
        assert!(level.block(at(0)).is(ids::PISTON));
        assert!(!level.block(at(0)).flag("extended"));
        assert!(level.block(at(1)).is_air());
        assert!(level.block(at(2)).is(ids::STONE));
    }

    #[test]
    fn sticky_piston_pulls_back() {
        let mut level = level();
        level.set_block_and_update(at(0), piston(ids::STICKY_PISTON, Direction::East));
        level.set_block_and_update(at(1), ids::STONE.default_state());
        let power = BlockPos::new(0, Y, 1);
        level.set_block_and_update(power, ids::REDSTONE_BLOCK.default_state());
        run(&mut level, 4);
        assert!(level.block(at(2)).is(ids::STONE));
        level.set_block_and_update(power, BlockState::AIR);
        run(&mut level, 4);
        assert!(level.block(at(1)).is(ids::STONE));
        assert!(level.block(at(2)).is_air());
        assert!(level.block(at(0)).is(ids::STICKY_PISTON));
    }

    #[test]
    fn pushes_twelve_blocks_but_not_thirteen() {
        for (count, moves) in [(12, true), (13, false)] {
            let mut level = level();
            level.set_block_and_update(at(0), piston(ids::PISTON, Direction::East));
            for x in 1..=count {
                level.set_block_and_update(at(x), ids::STONE.default_state());
            }
            level.set_block_and_update(BlockPos::new(0, Y, 1), ids::REDSTONE_BLOCK.default_state());
            run(&mut level, 4);
            assert_eq!(level.block(at(0)).flag("extended"), moves, "{count} blocks");
            assert_eq!(
                level.block(at(count + 1)).is(ids::STONE),
                moves,
                "{count} blocks"
            );
        }
    }

    #[test]
    fn slime_block_carries_its_neighbours() {
        let mut level = level();
        // In the air: a slime block on the ground would try to take the ground along.
        let at = |x: i32| BlockPos::new(x, Y + 5, 0);
        let y = Y + 5;
        level.set_block_and_update(at(0), piston(ids::STICKY_PISTON, Direction::East));
        level.set_block_and_update(at(1), ids::SLIME_BLOCK.default_state());
        let above = BlockPos::new(1, y + 1, 0);
        level.set_block_and_update(above, ids::STONE.default_state());
        let side = BlockPos::new(1, y, -1);
        level.set_block_and_update(side, ids::OAK_PLANKS.default_state());
        let power = BlockPos::new(0, y, 1);
        level.set_block_and_update(power, ids::REDSTONE_BLOCK.default_state());
        run(&mut level, 4);
        assert!(level.block(at(2)).is(ids::SLIME_BLOCK));
        assert!(level.block(BlockPos::new(2, y + 1, 0)).is(ids::STONE));
        assert!(level.block(BlockPos::new(2, y, -1)).is(ids::OAK_PLANKS));
        assert!(level.block(above).is_air());
        // Pulled back together.
        level.set_block_and_update(power, BlockState::AIR);
        run(&mut level, 4);
        assert!(level.block(at(1)).is(ids::SLIME_BLOCK));
        assert!(level.block(above).is(ids::STONE));
        assert!(level.block(side).is(ids::OAK_PLANKS));
    }

    #[test]
    fn obsidian_does_not_move() {
        let mut level = level();
        level.set_block_and_update(at(0), piston(ids::PISTON, Direction::East));
        level.set_block_and_update(at(1), ids::OBSIDIAN.default_state());
        level.set_block_and_update(BlockPos::new(0, Y, 1), ids::REDSTONE_BLOCK.default_state());
        run(&mut level, 4);
        assert!(!level.block(at(0)).flag("extended"));
        assert!(level.block(at(1)).is(ids::OBSIDIAN));
        let _ = update::ALL;
    }
}
