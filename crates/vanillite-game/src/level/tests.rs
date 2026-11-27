//! Deterministic simulation tests on a classic superflat world (grass at Y -61).

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use vanillite_base::{BlockPos, ChunkPos};
use vanillite_chunk::ProtoChunk;
use vanillite_chunk::light::ChunkLight;
use vanillite_chunk::slots::{
    ChunkGenerator, ChunkStatus, FlatGenerator, LightEngine, Neighbours, NullCodec, NullStore,
    WorldInfo,
};
use vanillite_data::block::{BlockState, ids};
use vanillite_world::chunks::{ChunkSettings, ChunkSlots, Chunks, InlineExecutor};
use vanillite_world::entity::NetworkIds;
use vanillite_worldgen::behaviour::StateExt;

use super::{Level, LevelSettings, update};
use crate::block::Behaviours;

struct NoLight;

impl LightEngine for NoLight {
    fn initialize_light(&self, _: &mut [ProtoChunk]) {}
    fn light_chunks(&self, _: &mut [ProtoChunk], _: &mut Neighbours<'_>) {}
    fn relight(&self, _: &[BlockPos], _: &Neighbours<'_>) -> Vec<(ChunkPos, ChunkLight)> {
        Vec::new()
    }
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
    level_with(behaviours, |info| Arc::new(FlatGenerator::classic(info)))
}

/// [`flat_level_with`] with another chunk generator.
fn level_with(
    behaviours: Behaviours,
    generator: impl FnOnce(&WorldInfo) -> Arc<dyn ChunkGenerator>,
) -> Level {
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
        store: Arc::new(NullStore),
        codec: Arc::new(NullCodec),
        generator: generator(&info),
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
    let mut level = Level::new(
        chunks,
        LevelSettings::default(),
        Arc::new(behaviours),
        Arc::new(NetworkIds::new()),
        0,
    );
    level.tick_entity_manager();
    assert!(level.is_block_ticking(center));
    level
}

/// One tick of the simulation phases and falling block entities.
pub(crate) fn tick(level: &mut Level) {
    level.tick_time();
    level.run_scheduled_ticks();
    level.tick_chunk_runtime();
    level.run_block_events();
    let mut cursor = level.entities.store.tick_cursor();
    while let Some(id) = level.entities.store.next_to_tick(&mut cursor) {
        if !crate::entities::in_ticking_range(level, id) {
            continue;
        }
        if let Some(kind) = level.entities.store.get(id).map(|e| e.kind()) {
            crate::entities::tick_entity(level, id, kind);
        }
    }
    level.tick_block_entities();
    level.tick_entity_manager();
}

/// A flat generator whose chunk (8, 0) gets an end crystal from generation, kept in its stored
/// tags without a UUID like the `end_spike` feature leaves it.
struct CrystalGenerator(FlatGenerator);

impl ChunkGenerator for CrystalGenerator {
    fn new_chunk(&self, pos: ChunkPos) -> ProtoChunk {
        let mut chunk = self.0.new_chunk(pos);
        if pos == ChunkPos::new(8, 0) {
            let crystal = vanillite_nbt::Compound::new()
                .with("id", "minecraft:end_crystal")
                .with(
                    "Pos",
                    vanillite_nbt::List(vec![
                        vanillite_nbt::Tag::Double(136.5),
                        vanillite_nbt::Tag::Double(-60.0),
                        vanillite_nbt::Tag::Double(8.5),
                    ]),
                );
            let tags = vanillite_nbt::Compound::new().with(
                "entities",
                vanillite_nbt::Tag::List(vanillite_nbt::List(vec![vanillite_nbt::Tag::Compound(
                    crystal,
                )])),
            );
            let mut bytes = Vec::new();
            vanillite_nbt::write::write_network_compound(&tags, &mut bytes).unwrap();
            chunk.extra = Some(bytes.into());
        }
        chunk
    }

    fn generate(&self, status: ChunkStatus, targets: &mut [ProtoChunk], n: &mut Neighbours<'_>) {
        self.0.generate(status, targets, n);
    }
}

#[test]
fn generation_entities_come_alive_when_their_chunk_is_full() {
    let mut level = level_with(Behaviours::vanilla(), |info| {
        Arc::new(CrystalGenerator(FlatGenerator::classic(info)))
    });
    // The chunk becomes full after the level started, like a chunk a player walks into.
    let far = ChunkPos::new(8, 0);
    let ticket = level.chunks.player_ticket_level();
    level.chunks.add_ticket(far, ticket);
    let simulation = level.chunks.player_simulation_level();
    level.chunks.add_simulation_ticket(far, simulation);
    for _ in 0..8 {
        tick(&mut level);
    }
    assert!(level.chunks.entities_loaded(far));
    let crystals: Vec<_> = level
        .entities
        .store
        .columns()
        .id
        .iter()
        .filter_map(|&id| level.entities.store.get(id))
        .filter(|e| e.kind() == vanillite_data::entity::ids::END_CRYSTAL)
        .map(|e| e.uuid())
        .collect();
    assert_eq!(crystals.len(), 1);
    assert_ne!(crystals[0], 0);
    let extra = level.chunks.chunk(far).unwrap().extra.clone();
    assert!(!extra.unwrap().windows(8).any(|w| w == b"entities"));
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

#[test]
fn powered_rails_carry_power_eight_rails_from_the_source() {
    let mut level = flat_level();
    let y = -60;
    for x in 0..12 {
        level.set_block_and_update(BlockPos::new(x, y, 0), ids::POWERED_RAIL.default_state());
    }
    assert_eq!(
        level.block(BlockPos::new(5, y, 0)).value("shape"),
        Some("east_west")
    );
    // A powered target block as the source (it emits its `power` to every side).
    let source = BlockPos::new(-1, y, 0);
    let target = |power: &str| ids::TARGET.default_state().with("power", power).unwrap();
    level.set_block(source, target("15"), update::ALL | update::SKIP_ON_PLACE);
    for x in 0..12 {
        let state = level.block(BlockPos::new(x, y, 0));
        assert_eq!(state.flag("powered"), x <= 8, "rail {x}: {state:?}");
    }
    // Removing the source turns the whole line off.
    level.set_block(source, target("0"), update::ALL | update::SKIP_ON_PLACE);
    for x in 0..12 {
        assert!(
            !level.block(BlockPos::new(x, y, 0)).flag("powered"),
            "rail {x}"
        );
    }
}

#[test]
fn rail_curves_towards_two_neighbours() {
    let mut level = flat_level();
    let y = -60;
    let center = BlockPos::new(0, y, 0);
    level.set_block_and_update(center, ids::RAIL.default_state());
    level.set_block_and_update(BlockPos::new(1, y, 0), ids::RAIL.default_state());
    assert_eq!(level.block(center).value("shape"), Some("east_west"));
    level.set_block_and_update(BlockPos::new(0, y, 1), ids::RAIL.default_state());
    assert_eq!(level.block(center).value("shape"), Some("south_east"));
}

#[test]
fn tripwire_hooks_attach_over_string() {
    let mut level = flat_level();
    let y = -60;
    level.set_block_and_update(BlockPos::new(-1, y, 0), ids::STONE.default_state());
    level.set_block_and_update(BlockPos::new(5, y, 0), ids::STONE.default_state());
    let hook = |facing: &str| {
        ids::TRIPWIRE_HOOK
            .default_state()
            .with("facing", facing)
            .unwrap()
    };
    let a = BlockPos::new(0, y, 0);
    let b = BlockPos::new(4, y, 0);
    level.set_block_and_update(a, hook("east"));
    let state = level.block(a);
    crate::block::tripwire::hook_set_placed_by(&mut level, a, state);
    for x in 1..4 {
        level.set_block_and_update(BlockPos::new(x, y, 0), ids::TRIPWIRE.default_state());
    }
    level.set_block_and_update(b, hook("west"));
    let state = level.block(b);
    crate::block::tripwire::hook_set_placed_by(&mut level, b, state);
    assert!(level.block(a).flag("attached"), "{:?}", level.block(a));
    assert!(level.block(b).flag("attached"), "{:?}", level.block(b));
    for x in 1..4 {
        assert!(
            level.block(BlockPos::new(x, y, 0)).flag("attached"),
            "string {x}"
        );
    }
    assert!(!level.block(a).flag("powered"));
}

#[test]
fn note_block_plays_on_rising_signal() {
    let mut level = flat_level();
    let note = BlockPos::new(2, -60, 2);
    level.set_block(note, ids::NOTE_BLOCK.default_state(), update::ALL);
    level.output().block_events.clear();
    // A powered lectern next to it gives a signal of 15.
    let lectern = ids::LECTERN.default_state().set_flag("powered", true);
    level.set_block(BlockPos::new(3, -60, 2), lectern, update::ALL);
    assert!(level.block(note).flag("powered"));
    tick(&mut level);
    let events = &level.output().block_events;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].pos, note);
    assert_eq!(events[0].block, ids::NOTE_BLOCK);
    assert_eq!(level.output().sounds.len(), 1);
    // A steady signal does not play again.
    level.output().block_events.clear();
    tick(&mut level);
    assert!(level.output().block_events.is_empty());
}

#[test]
fn sculk_sensor_activates_after_the_vibration_travelled() {
    let mut level = flat_level();
    let sensor = BlockPos::new(0, -60, 0);
    level.set_block(sensor, ids::SCULK_SENSOR.default_state(), update::ALL);
    assert!(level.block_entity(sensor).is_some());
    tick(&mut level);
    // A block placed 8 blocks away: the vibration is selected the next tick and arrives 8 block
    // entity ticks after the event (the selecting tick already travels one block).
    let place = super::game_event::events().block_place;
    level.game_event_at(None, place, BlockPos::new(8, -60, 0));
    for i in 0..7 {
        tick(&mut level);
        assert_eq!(
            level.block(sensor).value("sculk_sensor_phase"),
            Some("inactive"),
            "tick {i}"
        );
    }
    tick(&mut level);
    let state = level.block(sensor);
    assert_eq!(state.value("sculk_sensor_phase"), Some("active"));
    // `getRedstoneStrengthForDistance(8, 8)`: 15 - floor(15 / 8 * 8), at least 1.
    assert_eq!(state.int("power"), Some(1));
    assert_eq!(level.signal(sensor, vanillite_base::Direction::West), 1);
    // `block_place` has frequency 13.
    assert_eq!(
        level.analog_output(state, sensor, vanillite_base::Direction::North),
        13
    );
    // Active for 30 ticks, then 10 ticks of cooldown.
    for _ in 0..30 {
        tick(&mut level);
    }
    assert_eq!(
        level.block(sensor).value("sculk_sensor_phase"),
        Some("cooldown")
    );
    assert_eq!(level.block(sensor).int("power"), Some(0));
    for _ in 0..10 {
        tick(&mut level);
    }
    assert_eq!(
        level.block(sensor).value("sculk_sensor_phase"),
        Some("inactive")
    );
}

#[test]
fn wool_occludes_vibrations() {
    let mut level = flat_level();
    let sensor = BlockPos::new(0, -60, 0);
    level.set_block(sensor, ids::SCULK_SENSOR.default_state(), update::ALL);
    // A wool wall between the sensor and the event, high enough for every nudged line.
    for y in -61..=-58 {
        for z in -2..=2 {
            level.set_block(
                BlockPos::new(2, y, z),
                ids::WHITE_WOOL.default_state(),
                update::ALL,
            );
        }
    }
    let place = super::game_event::events().block_place;
    level.game_event_at(None, place, BlockPos::new(4, -60, 0));
    for _ in 0..10 {
        tick(&mut level);
    }
    assert_eq!(
        level.block(sensor).value("sculk_sensor_phase"),
        Some("inactive")
    );
}

/// A flat level where redstone blocks emit 15 to every side.
fn powered_level() -> Level {
    let mut level = flat_level();
    let behaviours = Arc::get_mut(&mut level.behaviours).expect("unshared behaviours");
    let mut redstone = *behaviours.get(ids::REDSTONE_BLOCK);
    redstone.signal = Some(|_, _, _, _| 15);
    behaviours.set(ids::REDSTONE_BLOCK, redstone);
    level
}

#[test]
fn signal_primes_tnt_and_it_explodes_after_80_ticks() {
    let mut level = powered_level();
    let tnt = BlockPos::new(4, -60, 4);
    let other = BlockPos::new(6, -60, 4);
    level.set_block(tnt, ids::TNT.default_state(), update::ALL);
    level.set_block(other, ids::TNT.default_state(), update::ALL);
    assert!(level.block(tnt).is(ids::TNT));
    level.set_block(
        BlockPos::new(3, -60, 4),
        ids::REDSTONE_BLOCK.default_state(),
        update::ALL,
    );
    assert!(level.block(tnt).is_air());
    assert_eq!(level.entities.store.len(), 1);
    for _ in 0..79 {
        tick(&mut level);
    }
    assert_eq!(level.entities.store.len(), 1);
    tick(&mut level);
    // The first TNT exploded; the second one was primed by it with a short fuse.
    assert_eq!(level.output().explosions.len(), 1);
    assert!(level.output().explosions[0].block_count > 0);
    assert!(level.block(other).is_air());
    assert_eq!(level.entities.store.len(), 1);
    let id = level.entities.store.columns().id[0];
    let fuse = level.entities.primed_tnt(id).unwrap().fuse;
    assert!((10..30).contains(&fuse), "short fuse {fuse}");
    // The ground below the TNT is gone.
    assert!(level.block(BlockPos::new(4, -61, 4)).is_air());
}

#[test]
fn dispenser_triggers_and_fails_empty_and_hopper_locks() {
    let mut level = powered_level();
    let dispenser = BlockPos::new(2, -60, 2);
    let hopper = BlockPos::new(2, -60, 4);
    level.set_block(dispenser, ids::DISPENSER.default_state(), update::ALL);
    level.set_block(hopper, ids::HOPPER.default_state(), update::ALL);
    assert!(level.block_entity(hopper).is_some());
    level.set_block(
        BlockPos::new(2, -60, 3),
        ids::REDSTONE_BLOCK.default_state(),
        update::ALL,
    );
    assert_eq!(level.block(dispenser).value("triggered"), Some("true"));
    assert_eq!(level.block(hopper).value("enabled"), Some("false"));
    level.output().level_events.clear();
    for _ in 0..4 {
        tick(&mut level);
    }
    let events = &level.output().level_events;
    assert_eq!(events.len(), 1);
    assert_eq!((events[0].event, events[0].pos), (1001, dispenser));
}
