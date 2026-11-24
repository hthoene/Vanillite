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
        Arc::new(Behaviours::vanilla()),
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
