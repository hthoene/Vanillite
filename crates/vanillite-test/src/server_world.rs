//! The scenario world across dimensions: the real server with its three worlds ticked one after
//! another in Vanilla's order (`performance.tick.parallel-dimensions = false`, the `vanilla`
//! profile).
//!
//! The server runs without network, its compute jobs inline, its chunks in memory and its
//! chunk tickets blocking ([`vanillite_server::ServerBuilder::harness`]); every dimension is
//! flat like the recorder's (`ParityServer` with `flatOthers`). A step is one iteration of the
//! server's tick loop without its clock; players join through the server's join code on a
//! connection nobody reads, and a `use_item` action is their `use_item` packet. Everything
//! between the worlds (portals, ender pearls, `execute in`, `/tp`) is the server's own code.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::json;
use vanillite_base::random::LegacyRandom;
use vanillite_base::{BlockPos, Vec3};
use vanillite_chunk::slots::{
    ChunkGenerator, ChunkGeneratorProvider, ChunkStore, ChunkStoreProvider, FlatGenerator,
    MemoryStore, WorldInfo,
};
use vanillite_data::block::{BlockId, BlockState};
use vanillite_data::entity::ids;
use vanillite_game::level::{Level, update};
use vanillite_game::mob::Environment;
use vanillite_game::survival::rules::Difficulty;
use vanillite_server::{Server, ServerBuilder, WorldId};
use vanillite_world::chunks::Visibility;
use vanillite_world::entity::EntityId;

use crate::game_world::{
    FLAT_SEA_LEVEL, PARITY_SHUFFLE_SEED, entity_detail, inside, install, observe_ticks, spawn_mob,
};
use crate::scenario::{EntitySpec, OVERWORLD, WorldSpec};
use crate::trace::{EntityObs, TickObs};
use crate::world::{Execution, ScenarioWorld};

/// The dimensions in world order (`vanillite_server::DIMENSIONS`).
const NETHER: &str = "minecraft:the_nether";
const END: &str = "minecraft:the_end";
/// Server ticks the chunks of the regions get to become ticking.
const SETTLE_LIMIT: usize = 200;
/// View and simulation distance (`ScenarioRecorder.VIEW_DISTANCE`).
const VIEW_DISTANCE: u32 = 2;

/// The flat layers of each dimension: the scenario's overworld, the recorder's Nether and End
/// (`ParityServer.NETHER_LAYERS`, `END_LAYERS`).
#[derive(Debug)]
struct ScenarioGenerators {
    overworld: Vec<(String, u32)>,
}

impl ChunkGeneratorProvider for ScenarioGenerators {
    fn create(&self, world: &WorldInfo) -> Result<Arc<dyn ChunkGenerator>, String> {
        let nether = [("minecraft:bedrock", 1), ("minecraft:netherrack", 3)];
        let end = [("minecraft:bedrock", 1), ("minecraft:end_stone", 3)];
        let layers: Vec<(&str, u32)> = match world.dimension.as_str() {
            NETHER => nether.to_vec(),
            END => end.to_vec(),
            _ => self
                .overworld
                .iter()
                .map(|(b, h)| (b.as_str(), *h))
                .collect(),
        };
        let mut states = Vec::new();
        for (block, height) in layers {
            let state = BlockId::by_name(block)
                .ok_or_else(|| format!("unknown layer block `{block}`"))?
                .default_state();
            states.extend(std::iter::repeat_n(state.0, height as usize));
        }
        Ok(Arc::new(FlatGenerator::new(world, &states)))
    }
}

/// Chunks in memory.
#[derive(Debug)]
struct MemoryStores;

impl ChunkStoreProvider for MemoryStores {
    fn create(
        &self,
        _: &WorldInfo,
        _: vanillite_chunk::store::DimensionId,
    ) -> std::io::Result<Arc<dyn ChunkStore>> {
        Ok(Arc::new(MemoryStore::default()))
    }
}

/// The server of a scenario across dimensions.
pub struct ServerWorld {
    server: Server,
    spec: WorldSpec,
    dir: PathBuf,
    /// The world block ops and observations work in.
    selected: WorldId,
    /// Scenario names by UUID: an entity that changes worlds is a new entity there.
    names: Vec<(u128, String)>,
    /// The packet queues of the scenario players.
    players: Vec<(String, vanillite_protocol::InboundSender)>,
    /// The world of the player [`ServerWorld::player`] found last.
    selected_player: WorldId,
}

impl std::fmt::Debug for ServerWorld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerWorld").finish_non_exhaustive()
    }
}

fn world_id(dimension: &str) -> Result<WorldId, String> {
    vanillite_server::DIMENSIONS
        .iter()
        .position(|d| *d == dimension)
        .and_then(|i| u16::try_from(i).ok())
        .map(WorldId)
        .ok_or_else(|| format!("unknown dimension {dimension}"))
}

impl ServerWorld {
    /// The server of `spec`, with the chunks of the region and the views ticking.
    pub fn new(spec: &WorldSpec) -> Result<Self, String> {
        install();
        vanillite_game::mob::set_parity_shuffle_seed(Some(PARITY_SHUFFLE_SEED));
        if spec.observe_light {
            return Err("scenarios across dimensions observe no light".to_owned());
        }
        if spec.weather != "clear" {
            return Err("scenarios across dimensions have clear weather".to_owned());
        }
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vanillite-dimensions-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let io = |e: std::io::Error| e.to_string();
        std::fs::write(dir.join("eula.txt"), "eula=true\n").map_err(io)?;
        std::fs::write(
            dir.join("server.properties"),
            format!(
                "level-seed={}\nview-distance={VIEW_DISTANCE}\nsimulation-distance={VIEW_DISTANCE}\n\
                 online-mode=false\nmax-tick-time=-1\npause-when-empty-seconds=0\n\
                 spawn-protection=0\ngamemode=survival\n",
                spec.seed
            ),
        )
        .map_err(io)?;
        let cache = std::env::var_os("VANILLITE_CACHE").map_or_else(
            || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/vanilla-cache"),
            PathBuf::from,
        );
        let cache = std::fs::canonicalize(&cache).unwrap_or(cache);
        #[cfg(unix)]
        std::os::unix::fs::symlink(&cache, dir.join("cache")).map_err(io)?;
        let mut builder = ServerBuilder::new(&dir);
        builder
            .vanilla_pack(vanillite_server::VanillaPack::Download)
            .network(false)
            .harness();
        builder
            .scheduler(Box::new(vanillite_server::InlineScheduler))
            .and_then(|b| b.chunk_storage(Box::new(MemoryStores)))
            .and_then(|b| {
                b.chunk_generator(Box::new(ScenarioGenerators {
                    overworld: spec.layers.clone(),
                }))
            })
            .map_err(|e| e.to_string())?;
        let server = builder.start().map_err(|e| e.to_string())?;
        let mut world = Self {
            server,
            spec: spec.clone(),
            dir,
            selected: world_id(&spec.dimension)?,
            names: Vec::new(),
            players: Vec::new(),
            selected_player: WorldId(0),
        };
        // The recorder's rules (`ScenarioRecorder.rules`).
        let mut rules: Vec<(String, String)> = [
            ("random_tick_speed", "0"),
            ("spawn_mobs", "false"),
            ("advance_time", "false"),
            ("advance_weather", "false"),
        ]
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
        for (k, v) in &spec.rules {
            rules.retain(|(name, _)| name != k);
            rules.push((k.clone(), v.clone()));
        }
        for (k, v) in rules {
            world.server.command(&format!("gamerule {k} {v}"));
        }
        // The regions' chunks, forced like `/forceload`, until they tick with their entities.
        let mut areas = vec![(world.selected, spec.forced.clone())];
        for view in &spec.views {
            areas.push((world_id(&view.dimension)?, view.forced.clone()));
        }
        for (id, chunks) in &areas {
            let level = world.server.world(*id).level_mut();
            for &pos in chunks {
                let Level {
                    chunks: runtime,
                    forced,
                    ..
                } = level;
                forced.set(runtime, pos, true);
            }
        }
        let mut settled = false;
        for _ in 0..SETTLE_LIMIT {
            world.server.step();
            settled = areas.iter().all(|(id, chunks)| {
                let level = world.server.world_ref(*id).level();
                chunks.iter().all(|&pos| {
                    level.chunks.visibility(pos) == Visibility::Ticking
                        && level.is_block_ticking(pos)
                        && level.chunks.entities_loaded(pos)
                })
            });
            if settled {
                break;
            }
        }
        if !settled {
            let mut why = String::new();
            for (id, chunks) in &areas {
                let level = world.server.world_ref(*id).level();
                for &pos in chunks {
                    let state = (
                        level.chunks.visibility(pos),
                        level.is_block_ticking(pos),
                        level.chunks.entities_loaded(pos),
                    );
                    if state != (Visibility::Ticking, true, true) {
                        why = format!("world {} chunk {pos:?}: {state:?}", id.0);
                        break;
                    }
                }
            }
            return Err(format!("the scenario regions do not become ticking: {why}"));
        }
        // The scenario starts at game time 0 like the recorder's.
        world.server.set_game_time(0);
        world.seed_levels(false);
        Ok(world)
    }

    /// [`crate::world::WorldFactory`] of scenarios across dimensions.
    pub fn factory(spec: &WorldSpec, _execution: Execution) -> Box<dyn ScenarioWorld> {
        Box::new(Self::new(spec).unwrap_or_else(|e| panic!("{e}")))
    }

    /// The level random of every world (and with `rand_value` the LCG of random tick
    /// positions) starts from the world seed (`ScenarioRecorder.seedLevels`).
    fn seed_levels(&mut self, rand_value: bool) {
        let seed = self.spec.seed;
        for i in 0..vanillite_server::DIMENSIONS.len() {
            let Ok(index) = u16::try_from(i) else {
                continue;
            };
            let level = self.server.world(WorldId(index)).level_mut();
            *level.random() = LegacyRandom::new(seed);
            if rand_value {
                level.set_rand_value(seed as i32);
            }
        }
    }

    fn level(&self) -> &Level {
        self.server.world_ref(self.selected).level()
    }

    fn level_mut(&mut self) -> &mut Level {
        self.server.world(self.selected).level_mut()
    }

    fn name_of(&self, uuid: u128) -> Option<String> {
        self.names
            .iter()
            .find(|(u, _)| *u == uuid)
            .map(|(_, n)| n.clone())
    }

    /// The overworld entity of a scenario player.
    fn player(&mut self, name: &str) -> Result<EntityId, String> {
        let uuid = self
            .names
            .iter()
            .find(|(_, n)| n == name)
            .map(|(u, _)| *u)
            .ok_or_else(|| format!("no scenario entity `{name}`"))?;
        for i in 0..vanillite_server::DIMENSIONS.len() {
            let index = u16::try_from(i).map_err(|e| e.to_string())?;
            if let Some(id) = self
                .server
                .world_ref(WorldId(index))
                .level()
                .entities
                .player_by_uuid(uuid)
            {
                self.selected_player = WorldId(index);
                return Ok(id);
            }
        }
        Err(format!("the player `{name}` is in no world"))
    }

    fn add_player(&mut self, spec: &EntitySpec, index: usize) -> Result<(), String> {
        let uuid = spec.uuid(index);
        let tx = self.server.join_without_network(&spec.name, uuid);
        self.players.push((spec.name.clone(), tx));
        self.names.push((uuid, spec.name.clone()));
        let overworld = self.server.world(WorldId(0)).level_mut();
        let id = overworld
            .entities
            .player_by_uuid(uuid)
            .ok_or("the scenario player did not join")?;
        let pos = Vec3::new(spec.pos[0], spec.pos[1], spec.pos[2]);
        // `snapTo` with head and body facing the yaw; the server moves the player (it has no
        // client).
        vanillite_game::player::movement::teleport_within_world(
            &mut overworld.entities,
            id,
            pos,
            spec.rot[0],
            spec.rot[1],
            0,
        );
        if let Some(mut e) = overworld.entities.store.get_mut(id) {
            e.set_rot(vanillite_world::entity::Rotation {
                y_rot: spec.rot[0],
                x_rot: spec.rot[1],
                y_head_rot: spec.rot[0],
            });
            e.set_old_pos();
        }
        if let Some(p) = overworld.entities.player_mut(id) {
            p.survival.simulate_movement = true;
            p.survival.y_body_rot = spec.rot[0];
        }
        Ok(())
    }
}

impl Drop for ServerWorld {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl ScenarioWorld for ServerWorld {
    fn select_dimension(&mut self, dimension: &str) -> Result<(), String> {
        self.selected = world_id(dimension)?;
        Ok(())
    }

    fn is_loaded(&self, pos: BlockPos) -> bool {
        let level = self.level();
        level.is_loaded(pos) && level.chunks.chunk_now(pos.chunk())
    }

    fn set_block_raw(&mut self, pos: BlockPos, state: BlockState) {
        self.level_mut()
            .set_block(pos, state, update::CLIENTS | update::KNOWN_SHAPE);
    }

    fn place_block(&mut self, pos: BlockPos, state: BlockState) {
        self.level_mut().set_block(pos, state, update::ALL);
    }

    fn block(&self, pos: BlockPos) -> BlockState {
        self.level().block(pos)
    }

    fn step(&mut self) {
        self.server.step();
    }

    fn block_ticks(&self, min: BlockPos, max: BlockPos, out: &mut Vec<TickObs>) {
        let level = self.level();
        observe_ticks(level, level.chunks.block_ticks(), min, max, out, |kind| {
            BlockId(kind).name().to_owned()
        });
    }

    fn fluid_ticks(&self, min: BlockPos, max: BlockPos, out: &mut Vec<TickObs>) {
        let level = self.level();
        observe_ticks(level, level.chunks.fluid_ticks(), min, max, out, |kind| {
            vanillite_data::registry::FLUID
                .name(kind)
                .unwrap_or("?")
                .to_owned()
        });
    }

    /// Entities in the region of the selected world; outside the overworld with their `dim`.
    fn entities(&self, min: BlockPos, max: BlockPos, out: &mut Vec<EntityObs>) {
        let level = self.level();
        let dimension = vanillite_server::DIMENSIONS[self.selected.index()];
        let ents = &level.entities;
        let core = ents.store.columns();
        let name_of = |id: EntityId| ents.store.get(id).and_then(|e| self.name_of(e.uuid()));
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
            // A player moves by its client: its server-side motion is not part of the trace.
            let v = if core.kind[row] == ids::PLAYER {
                Vec3::ZERO
            } else {
                core.velocity[row]
            };
            let mut detail = entity_detail(
                ents,
                row,
                &name_of,
                &|_| false,
                self.spec.player_state,
                self.spec.sleep_state,
            );
            if dimension != OVERWORLD {
                detail
                    .get_or_insert_with(serde_json::Map::new)
                    .insert("dim".into(), json!(dimension));
            }
            out.push(EntityObs {
                kind: core.kind[row].name().to_owned(),
                pos: [pos.x, pos.y, pos.z],
                motion: [v.x, v.y, v.z],
                detail,
            });
        }
    }

    /// Players (through the join), every level random reseeded with the world seed, then the
    /// other entities in the overworld.
    fn add_entities(&mut self, entities: &[EntitySpec], entity_seed: i64) -> Result<(), String> {
        for (i, spec) in entities.iter().enumerate() {
            if spec.is_player() {
                self.add_player(spec, i)?;
            }
        }
        self.seed_levels(true);
        let env = Environment {
            sea_level: FLAT_SEA_LEVEL,
            ..Environment::overworld(self.spec.time.unwrap_or(0), 0.0, 0.0)
        };
        let difficulty = match self.spec.difficulty.as_str() {
            "peaceful" => Difficulty::Peaceful,
            "easy" => Difficulty::Easy,
            "hard" => Difficulty::Hard,
            _ => Difficulty::Normal,
        };
        for (i, spec) in entities.iter().enumerate() {
            if spec.is_player() {
                continue;
            }
            let level = self.server.world(WorldId(0)).level_mut();
            let network_ids = Arc::clone(level.network_ids_arc());
            let id = spawn_mob(level, &network_ids, env, difficulty, spec, i)?;
            let uuid = level.entities.store.get(id).map(|e| e.uuid());
            if let Some(uuid) = uuid {
                self.names.push((uuid, spec.name.clone()));
            }
        }
        self.level_mut().set_entity_seed(entity_seed);
        Ok(())
    }

    fn level_rng(&mut self) -> Option<i64> {
        (self.spec.level_rng || !self.names.is_empty()).then(|| self.level_mut().random().state())
    }

    /// As the server at the origin of the overworld, before the tick.
    fn command(&mut self, line: &str) -> Result<(), String> {
        self.server.command(line);
        Ok(())
    }

    /// The player's `use_item` packet with the stack in its main hand.
    fn use_item(&mut self, op: &crate::scenario::UseItemOp) -> Result<(), String> {
        use vanillite_protocol::packets::play::serverbound::{Hand, UseItem};
        let id = self.player(&op.entity)?;
        // Without `item` the hand stays as it is.
        let item = match &op.item {
            Some(name) => Some(
                vanillite_data::item::ItemId::by_name(name)
                    .ok_or_else(|| format!("unknown item {name}"))?,
            ),
            None => None,
        };
        let level = self.server.world(self.selected_player).level_mut();
        let rot = level
            .entities
            .store
            .get(id)
            .map(|e| e.rot())
            .ok_or("the player is gone")?;
        let p = level.entities.player_mut(id).ok_or("the player is gone")?;
        if let Some(item) = item {
            let slot = p.inventory.selected;
            p.inventory
                .set(slot, vanillite_game::item::ItemStack::new(item.0, op.count));
        }
        let tx = self
            .players
            .iter()
            .find(|(n, _)| *n == op.entity)
            .map(|(_, tx)| tx)
            .ok_or("no packet queue")?;
        let sent = tx.try_send(vanillite_protocol::Inbound {
            seq: 0,
            packet: vanillite_protocol::sets::ServerboundPlay::UseItem(UseItem {
                hand: Hand::MainHand,
                sequence: 0,
                y_rot: rot.y_rot,
                x_rot: rot.x_rot,
            }),
        });
        if sent {
            Ok(())
        } else {
            Err("the use_item packet was dropped".to_owned())
        }
    }
}

/// The scenario world of a scenario: across dimensions on the server, else the headless
/// level.
pub fn factory(spec: &WorldSpec, execution: Execution) -> Box<dyn ScenarioWorld> {
    if spec.dimensions {
        ServerWorld::factory(spec, execution)
    } else {
        crate::game_world::GameWorld::factory(spec, execution)
    }
}
