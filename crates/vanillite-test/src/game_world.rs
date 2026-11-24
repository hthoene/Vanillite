//! The scenario world on the real simulation: a headless [`Level`] over flat chunks.
//!
//! The world matches the Vanilla recorder (`ParityServer` and `ScenarioRecorder`): flat layers
//! from the bottom of the overworld, the scenario's game rules on top of the recorder's
//! defaults, and the region's chunks plus one chunk around them forced (entity ticking). A
//! step runs the simulation phases of `ServerLevel.tick` in Vanilla's order.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use vanillite_base::{BlockPos, ChunkPos};
use vanillite_chunk::ProtoChunk;
use vanillite_chunk::slots::{
    FlatGenerator, LightEngine, Neighbours, NullCodec, NullStore, WorldInfo,
};
use vanillite_data::block::{BlockId, BlockState};
use vanillite_game::block::Behaviours;
use vanillite_game::level::{Level, LevelOutput, LevelSettings, update};
use vanillite_world::chunks::{ChunkSettings, ChunkSlots, Chunks, InlineExecutor};
use vanillite_world::entity::NetworkIds;
use vanillite_world::ticks::LevelTicks;

use crate::scenario::WorldSpec;
use crate::trace::{EntityObs, TickObs};
use crate::world::{Execution, ScenarioWorld};

/// Overworld height range.
const MIN_Y: i32 = -64;
const HEIGHT: u32 = 384;
/// `ChunkLevel.byStatus(ENTITY_TICKING)`: the level of a forced chunk.
const FORCED_LEVEL: u8 = 31;
/// Chunks around the origin that are forced: the recorder forces the scenario region's chunks
/// and one more around them; every scenario region fits in this square.
const FORCED_RADIUS: i32 = 3;

struct NoLight;

impl LightEngine for NoLight {
    fn initialize_light(&self, _: &mut [ProtoChunk]) {}
    fn light_chunks(&self, _: &mut [ProtoChunk], _: &mut Neighbours<'_>) {}
    fn relight(&self, _: &[ChunkPos], _: &mut Neighbours<'_>) {}
}

/// Installs the chunk facts and the block tags behaviour reads from the Vanilla data pack
/// (extracted into `$VANILLITE_CACHE`, default `<workspace>/target/vanilla-cache`), once.
fn install() {
    static DONE: OnceLock<()> = OnceLock::new();
    DONE.get_or_init(|| {
        vanillite_chunk::facts::install_vanilla(&[], &[], 1);
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

/// A headless level over flat chunks.
pub struct GameWorld {
    level: Level,
}

impl std::fmt::Debug for GameWorld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GameWorld").finish_non_exhaustive()
    }
}

impl GameWorld {
    /// The world of `spec`. The simulation runs on the world's thread only, so serial and
    /// parallel execution are the same.
    pub fn new(spec: &WorldSpec) -> Result<Self, String> {
        install();
        let info = WorldInfo {
            dimension: "minecraft:overworld".to_owned(),
            dir: std::env::temp_dir(),
            min_y: MIN_Y,
            height: HEIGHT,
            seed: spec.seed,
            has_sky_light: true,
            biomes: Arc::from([spec.biome.clone()]),
            default_biome: 0,
            air: BlockState::AIR.0,
        };
        let mut layers = Vec::new();
        for (block, height) in &spec.layers {
            let state = BlockId::by_name(block)
                .ok_or_else(|| format!("unknown layer block `{block}`"))?
                .default_state();
            layers.extend(std::iter::repeat_n(state.0, *height as usize));
        }
        let slots = ChunkSlots {
            store: Arc::new(NullStore),
            codec: Arc::new(NullCodec),
            generator: Arc::new(FlatGenerator::new(&info, &layers)),
            light: Arc::new(NoLight),
            executor: Arc::new(InlineExecutor),
        };
        let mut chunks = Chunks::new(info, ChunkSettings::default(), slots);
        let mut forced = Vec::new();
        for cx in -FORCED_RADIUS..=FORCED_RADIUS {
            for cz in -FORCED_RADIUS..=FORCED_RADIUS {
                let pos = ChunkPos::new(cx, cz);
                chunks.add_ticket(pos, FORCED_LEVEL);
                chunks.add_simulation_ticket(pos, FORCED_LEVEL);
                forced.push(pos);
            }
        }
        for i in 0.. {
            if forced.iter().all(|&c| chunks.is_block_ticking(c)) {
                break;
            }
            if i == 1000 {
                return Err("the scenario region does not become ticking".to_owned());
            }
            chunks.tick();
        }
        let mut settings = LevelSettings::default();
        apply_rules(&mut settings, &spec.rules)?;
        let level = Level::new(
            chunks,
            settings,
            Arc::new(Behaviours::vanilla()),
            Arc::new(NetworkIds::new()),
            spec.seed,
        );
        Ok(Self { level })
    }

    /// [`crate::world::WorldFactory`] of the game world.
    pub fn factory(spec: &WorldSpec, _execution: Execution) -> Box<dyn ScenarioWorld> {
        Box::new(Self::new(spec).unwrap_or_else(|e| panic!("{e}")))
    }

    /// The level, for tests that look deeper than the trace.
    pub fn level(&mut self) -> &mut Level {
        &mut self.level
    }

    /// `entityTickList.forEach(tickNonPassenger)` for the kinds the simulation has.
    fn tick_entities(&mut self) {
        let level = &mut self.level;
        let mut cursor = level.entities.store.tick_cursor();
        while let Some(id) = level.entities.store.next_to_tick(&mut cursor) {
            let Some(mut entity) = level.entities.store.get_mut(id) else {
                continue;
            };
            entity.set_old_pos();
            let kind = entity.as_ref().kind();
            vanillite_game::entities::tick_entity(level, id, kind);
        }
    }
}

/// The recorder's default game rules (`ScenarioRecorder.DEFAULT_RULES`: no random ticks, no
/// mobs, no time or weather) with the scenario's rules on top.
fn apply_rules(
    settings: &mut LevelSettings,
    rules: &std::collections::BTreeMap<String, String>,
) -> Result<(), String> {
    settings.random_tick_speed = 0;
    for (name, value) in rules {
        let bad = || format!("game rule {name}: bad value `{value}`");
        match name.as_str() {
            "random_tick_speed" => settings.random_tick_speed = value.parse().map_err(|_| bad())?,
            "water_source_conversion" => {
                settings.water_source_conversion = value.parse().map_err(|_| bad())?;
            }
            "lava_source_conversion" => {
                settings.lava_source_conversion = value.parse().map_err(|_| bad())?;
            }
            "block_drops" => settings.block_drops = value.parse().map_err(|_| bad())?,
            "entity_drops" => settings.entity_drops = value.parse().map_err(|_| bad())?,
            "spawn_mobs" | "advance_time" | "advance_weather" => {}
            _ => {
                return Err(format!(
                    "game rule {name} is not supported by the game world"
                ));
            }
        }
    }
    Ok(())
}

impl ScenarioWorld for GameWorld {
    fn set_block_raw(&mut self, pos: BlockPos, state: BlockState) {
        self.level
            .set_block(pos, state, update::CLIENTS | update::KNOWN_SHAPE);
    }

    fn place_block(&mut self, pos: BlockPos, state: BlockState) {
        self.level.set_block(pos, state, update::ALL);
    }

    fn block(&self, pos: BlockPos) -> BlockState {
        self.level.block(pos)
    }

    /// The simulation phases of `ServerLevel.tick`: game time, scheduled block then fluid
    /// ticks, the chunk source (post-processing, random ticks), block events, entities, block
    /// entities.
    fn step(&mut self) {
        let level = &mut self.level;
        level.tick_time();
        level.run_scheduled_ticks();
        level.tick_chunk_runtime();
        level.post_process();
        let mut scratch = Vec::new();
        level.tick_chunks(&mut scratch);
        level.run_block_events();
        self.tick_entities();
        let level = &mut self.level;
        level.tick_block_entities();
        // Nobody watches: drop what would go to clients.
        *level.output() = LevelOutput::default();
        let mut changes = Vec::new();
        level.chunks.take_block_changes(&mut changes);
        let mut light = Vec::new();
        level.chunks.take_light_changes(&mut light);
    }

    fn block_ticks(&self, min: BlockPos, max: BlockPos, out: &mut Vec<TickObs>) {
        observe_ticks(
            &self.level,
            self.level.chunks.block_ticks(),
            min,
            max,
            out,
            |kind| BlockId(kind).name().to_owned(),
        );
    }

    fn fluid_ticks(&self, min: BlockPos, max: BlockPos, out: &mut Vec<TickObs>) {
        observe_ticks(
            &self.level,
            self.level.chunks.fluid_ticks(),
            min,
            max,
            out,
            |kind| {
                vanillite_data::registry::FLUID
                    .name(kind)
                    .unwrap_or("?")
                    .to_owned()
            },
        );
    }

    fn entities(&self, min: BlockPos, max: BlockPos, out: &mut Vec<EntityObs>) {
        let core = self.level.entities.store.columns();
        for row in 0..core.id.len() {
            let pos = core.pos[row];
            let block = BlockPos::new(
                pos.x.floor() as i32,
                pos.y.floor() as i32,
                pos.z.floor() as i32,
            );
            if !inside(block, min, max) {
                continue;
            }
            let v = core.velocity[row];
            out.push(EntityObs {
                kind: core.kind[row].name().to_owned(),
                pos: [pos.x, pos.y, pos.z],
                motion: [v.x, v.y, v.z],
            });
        }
    }
}

fn inside(p: BlockPos, min: BlockPos, max: BlockPos) -> bool {
    p.x >= min.x && p.y >= min.y && p.z >= min.z && p.x <= max.x && p.y <= max.y && p.z <= max.z
}

/// Pending ticks inside `min..=max` in drain order, delays relative to the game time.
fn observe_ticks(
    level: &Level,
    ticks: &LevelTicks,
    min: BlockPos,
    max: BlockPos,
    out: &mut Vec<TickObs>,
    name: impl Fn(u16) -> String,
) {
    let mut all = Vec::new();
    for cx in min.x >> 4..=max.x >> 4 {
        for cz in min.z >> 4..=max.z >> 4 {
            all.extend(
                ticks
                    .ticks_in(ChunkPos::new(cx, cz))
                    .filter(|t| inside(t.pos, min, max)),
            );
        }
    }
    all.sort_by_key(LevelTicks::drain_order);
    let now = level.game_time();
    out.extend(all.into_iter().map(|t| TickObs {
        pos: t.pos,
        kind: name(t.kind),
        delay: t.trigger - now,
        priority: i32::from(t.priority.0),
    }));
}
