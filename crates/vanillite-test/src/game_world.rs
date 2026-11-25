//! The scenario world on the real simulation: a headless [`Level`] over flat chunks.
//!
//! The world matches the Vanilla recorder (`ParityServer` and `ScenarioRecorder`): flat layers
//! from the bottom of the overworld, the scenario's game rules on top of the recorder's
//! defaults, and the region's chunks plus one chunk around them forced (entity ticking). A
//! step runs the simulation phases of `ServerLevel.tick` in Vanilla's order, then the
//! connection tick of the players (`ServerPlayer.doTick`). Chunks are saved to and loaded from
//! memory in the Anvil format, so unloading and loading them goes through the real chunk and
//! entity records.
//!
//! Entities: the mob, arrow and orb tick and what it queues (explosions, block changes, drops)
//! work like `vanillite-server`'s world (`mobs.rs`, `survival.rs`); keep them in step. The
//! survival random of the server world is the level random here, as in Vanilla (one
//! `Level.random`).

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{Map, Value, json};
use vanillite_base::random::{LegacyRandom, RandomSource};
use vanillite_base::{BlockPos, ChunkPos, Vec3};
use vanillite_chunk::ProtoChunk;
use vanillite_chunk::light::LightNibbles;
use vanillite_chunk::slots::{
    FlatGenerator, LightEngine, LightEngineProvider, MemoryStore, Neighbours, WorldInfo,
};
use vanillite_data::block::{BlockId, BlockState};
use vanillite_data::entity::{EntityTypeId, ids};
use vanillite_game::block::Behaviours;
use vanillite_game::block::weather::Climate;
use vanillite_game::entities::{Entities, is_mob_kind};
use vanillite_game::item_entity::ItemData;
use vanillite_game::level::explosion::BlockInteraction;
use vanillite_game::level::forced::SavedTickets;
use vanillite_game::level::{Level, LevelOutput, LevelSettings, update};
use vanillite_game::lightning;
use vanillite_game::minecart::{self, MinecartKind};
use vanillite_game::mob::common::CommonGoal;
use vanillite_game::mob::fam::FamGoal;
use vanillite_game::mob::goals::{Goal, MeleeKind, Slot};
use vanillite_game::mob::movement::{MoveGoal, RemoveSounds};
use vanillite_game::mob::{
    self, Environment, Kind, MobContext, MobData, MobSettings, WorldAction, combat, kinds,
    projectile,
};
use vanillite_game::player::chat::{ChatInbox, ChatSettings};
use vanillite_game::player::connection::Connection;
use vanillite_game::player::sink::PacketSink;
use vanillite_game::player::{ChunkView, ClientInformation, GameProfile, PlayerData};
use vanillite_game::survival::Effect;
use vanillite_game::survival::blocks::{BlockAccess, BlockFacts};
use vanillite_game::survival::damage::{DamageSource, DamageTypes};
use vanillite_game::survival::player::{self as survival_player, Env};
use vanillite_game::survival::rules::{Difficulty, GameRules, Rule};
use vanillite_game::survival::weather::LevelWeather;
use vanillite_protocol::SharedPacket;
use vanillite_storage::{AnvilCodec, Compression};
use vanillite_world::chunks::{ChunkSettings, ChunkSlots, Chunks, InlineExecutor, Visibility};
use vanillite_world::entity::{EntityId, NetworkIds, Rotation};
use vanillite_world::ticks::LevelTicks;

use crate::scenario::{EntitySpec, Hurt, WorldSpec};
use crate::trace::{EntityObs, TickObs};
use crate::world::{Execution, ScenarioWorld};

/// Overworld height range.
const MIN_Y: i32 = -64;
const HEIGHT: u32 = 384;
/// Chunk runtime ticks [`GameWorld::set_forced`] runs at most to settle.
const SETTLE_LIMIT: usize = 1000;

/// No lighting, for scenarios that do not need it (see [`WorldSpec::light`]).
struct NoLight;

impl LightEngine for NoLight {
    fn initialize_light(&self, _: &mut [ProtoChunk]) {}
    fn light_chunks(&self, _: &mut [ProtoChunk], _: &mut Neighbours<'_>) {}
    fn relight(&self, _: &[ChunkPos], _: &mut Neighbours<'_>) {}
}

/// A connection nobody reads.
struct NoSink;

impl PacketSink for NoSink {
    fn send_body(&self, _: Vec<u8>) -> bool {
        true
    }
    fn send_shared(&self, _: &Arc<SharedPacket>) -> bool {
        true
    }
    fn close(&self) {}
    fn is_closed(&self) -> bool {
        false
    }
    fn latency_ms(&self) -> u32 {
        0
    }
}

/// Blocks of the level's chunks for survival code.
struct ChunkBlocks<'a>(&'a Chunks);

impl BlockAccess for ChunkBlocks<'_> {
    fn block_state(&self, pos: BlockPos) -> Option<BlockState> {
        self.0.block(pos).map(BlockState)
    }
}

/// Damage types and block facts of the Vanilla data pack.
static PACK: OnceLock<(DamageTypes, BlockFacts)> = OnceLock::new();
/// The biomes of the Vanilla data pack (temperatures, precipitation).
static BIOMES: OnceLock<vanillite_worldgen::biome::BiomeRegistry> = OnceLock::new();
/// The biome ids by protocol id.
static BIOME_NAMES: OnceLock<Arc<[String]>> = OnceLock::new();
/// The trees saplings grow into (they do not depend on the seed).
static TREES: OnceLock<Arc<vanillite_game::block::tree::TreeFeatures>> = OnceLock::new();

/// Sea level of Vanilla's flat worlds (`FlatLevelSource.getSeaLevel`).
const FLAT_SEA_LEVEL: i32 = -63;

/// Installs the chunk facts and the block tags behaviour reads from the Vanilla data pack
/// (extracted into `$VANILLITE_CACHE`, default `<workspace>/target/vanilla-cache`), once.
fn install() {
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
        // The heightmap tags and the biome count, as the server installs them.
        let tag = |name: &str| {
            packs
                .tag_ids("minecraft:block", name)
                .expect("Vanilla heightmap block tag")
        };
        let biomes = packs
            .registry("minecraft:worldgen/biome")
            .map_or(1, |r| r.entries().len());
        vanillite_chunk::facts::install_vanilla(
            &tag("minecraft:blocks_motion_in_heightmap"),
            &tag("minecraft:blocks_motion_in_heightmap_no_leaves"),
            u32::try_from(biomes).unwrap_or(u32::MAX),
        );
        vanillite_worldgen::behaviour::init(&packs);
        let _ = PACK.set((
            vanillite_server::load_damage_types(&packs),
            vanillite_server::load_block_facts(&packs),
        ));
        let _ = BIOMES
            .set(vanillite_worldgen::biome::BiomeRegistry::load(&packs).expect("Vanilla biomes"));
        let _ = TREES.set(Arc::new(
            vanillite_game::block::tree::TreeFeatures::load(
                &packs,
                BIOMES.get().expect("biomes first"),
                0,
                MIN_Y,
                HEIGHT as i32,
                FLAT_SEA_LEVEL,
            )
            .expect("Vanilla tree features"),
        ));
        let _ = BIOME_NAMES.set(
            packs
                .registry("minecraft:worldgen/biome")
                .map(|r| r.entries().iter().map(|e| e.id.clone()).collect())
                .expect("Vanilla biome registry"),
        );
        // Loot tables and item tags (block drops), as the server installs them.
        vanillite_game::player::play::install_game_data(Arc::new(vanillite_server::game_data(
            Some(&packs),
        )));
    });
}

/// The server whose command tree, scoreboard and storage `command` actions use: started once
/// without network in a temporary directory, with the Vanilla data pack of [`install`], and
/// never run (the commands run on the scenario's level).
fn command_server() -> &'static vanillite_server::Global {
    static GLOBAL: OnceLock<Arc<vanillite_server::Global>> = OnceLock::new();
    GLOBAL.get_or_init(|| {
        install();
        let dir = std::env::temp_dir().join(format!("vanillite-scenario-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scenario server directory");
        std::fs::write(dir.join("eula.txt"), "eula=true\n").expect("scenario server eula");
        let cache = std::env::var_os("VANILLITE_CACHE").map_or_else(
            || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/vanilla-cache"),
            PathBuf::from,
        );
        let cache = std::fs::canonicalize(&cache).unwrap_or(cache);
        #[cfg(unix)]
        std::os::unix::fs::symlink(&cache, dir.join("cache")).expect("link the pack cache");
        let mut builder = vanillite_server::ServerBuilder::new(&dir);
        builder
            .vanilla_pack(vanillite_server::VanillaPack::Download)
            .network(false);
        let server = builder.start().expect("scenario server starts");
        let global = server.global().arc().expect("server global");
        // Kept for the whole test run: its global outlives every scenario.
        std::mem::forget(server);
        global
    })
}

/// A headless level over flat chunks.
pub struct GameWorld {
    level: Level,
    /// The world's spec, to open it again on a restart.
    spec: WorldSpec,
    /// Where the chunks are saved, kept over restarts.
    store: Arc<MemoryStore>,
    /// Runs the light engine (scenarios that observe light).
    light: bool,
    network_ids: Arc<NetworkIds>,
    mobs: MobContext,
    rules: GameRules,
    difficulty: Difficulty,
    env: Environment,
    effects: Vec<Effect>,
    /// Scenario names of entities.
    names: Vec<(EntityId, String)>,
    seed: i64,
    /// Frames observe the level random.
    level_rng: bool,
    /// The overworld clock (`time`), for the local difficulty of lightning strikes.
    clock_time: i64,
    /// The entities the ticking lightning bolt strikes.
    bolt_hits: Vec<EntityId>,
    /// The own random of item entities (not seeded per entity yet).
    item_random: LegacyRandom,
    item_scratch: vanillite_server::ItemScratch,
}

/// Ticks until an item entity despawns (`ItemEntity.LIFETIME`).
const ITEM_LIFETIME: i16 = 6000;

impl std::fmt::Debug for GameWorld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GameWorld").finish_non_exhaustive()
    }
}

/// The seed of every shuffled behaviour list's random in a scenario
/// (`ParityHooks.SHUFFLE_SEED`).
const PARITY_SHUFFLE_SEED: i64 = 0x5348_5546;

impl GameWorld {
    /// The world of `spec`. The simulation runs on the world's thread only, so serial and
    /// parallel execution are the same.
    pub fn new(spec: &WorldSpec) -> Result<Self, String> {
        install();
        // The recorder seeds every shuffled behaviour list's random alike
        // (`ParityHooks.shuffleRandom`).
        vanillite_game::mob::set_parity_shuffle_seed(Some(PARITY_SHUFFLE_SEED));
        let store = Arc::new(MemoryStore::default());
        let (mut level, network_ids) = open_level(spec, Arc::clone(&store))?;
        let mut rules = GameRules::default();
        apply_rules(&mut LevelSettings::default(), &mut rules, &spec.rules)?;
        let difficulty = match spec.difficulty.as_str() {
            "peaceful" => Difficulty::Peaceful,
            "easy" => Difficulty::Easy,
            "normal" => Difficulty::Normal,
            "hard" => Difficulty::Hard,
            d => return Err(format!("unknown difficulty `{d}`")),
        };
        level.settings.difficulty = difficulty;
        // The world starts at day time 0 unless the scenario sets one; time does not advance.
        let env = Environment::overworld(spec.time.unwrap_or(0), 0.0, 0.0);
        let mut world = Self {
            level,
            spec: spec.clone(),
            store,
            light: spec.observe_light,
            network_ids,
            mobs: MobContext::default(),
            rules,
            difficulty,
            env,
            effects: Vec::new(),
            names: Vec::new(),
            seed: spec.seed,
            level_rng: spec.level_rng,
            clock_time: spec.time.unwrap_or(0),
            bolt_hits: Vec::new(),
            item_random: LegacyRandom::new(spec.seed),
            item_scratch: vanillite_server::ItemScratch::default(),
        };
        world
            .set_forced(&spec.forced, true)
            .map_err(|_| "the scenario region does not become ticking".to_owned())?;
        Ok(world)
    }

    /// Whether the chunk system is settled around the forced chunks: every chunk is visible
    /// as its ticket level says, and nothing is loading, generating or waiting to unload.
    fn settled(&self) -> bool {
        let chunks = &self.level.chunks;
        chunks.is_idle()
            && self.level.forced.sorted().into_iter().all(|pos| {
                chunks.visibility(pos) == Visibility::Ticking && chunks.is_block_ticking(pos)
            })
    }

    /// [`crate::world::WorldFactory`] of the game world.
    pub fn factory(spec: &WorldSpec, _execution: Execution) -> Box<dyn ScenarioWorld> {
        Box::new(Self::new(spec).unwrap_or_else(|e| panic!("{e}")))
    }

    /// The level, for tests that look deeper than the trace.
    pub fn level(&mut self) -> &mut Level {
        &mut self.level
    }

    /// Runs `f` with the survival environment of this tick; the entities are taken out of the
    /// level meanwhile, the level random is the environment's random.
    fn with_env<R>(
        &mut self,
        f: impl FnOnce(&Level, &mut Entities, &mut Env<'_>, &mut MobContext) -> R,
    ) -> R {
        let mut random = std::mem::replace(self.level.random(), LegacyRandom::new(0));
        let mut ents = std::mem::take(&mut self.level.entities);
        let (types, facts) = PACK.get().expect("installed");
        let r = {
            let level = &self.level;
            let blocks = ChunkBlocks(&level.chunks);
            let raining = |pos| level.is_raining_at(pos);
            let mut no_gateway = |_| false;
            let names = |name: &str, _| vanillite_protocol::text::Component::text(name.to_owned());
            let mut env = Env {
                types,
                facts,
                blocks: &blocks,
                rules: &self.rules,
                difficulty: self.difficulty,
                game_time: level.game_time(),
                min_y: MIN_Y,
                dimension: "minecraft:overworld",
                raining_at: &raining,
                border: None,
                enter_gateway: &mut no_gateway,
                random: &mut random,
                out: &mut self.effects,
                hooks: &vanillite_game::hooks::NoHooks,
                display_name: &names,
            };
            f(level, &mut ents, &mut env, &mut self.mobs)
        };
        self.level.entities = ents;
        *self.level.random() = random;
        r
    }

    /// `entityTickList.forEach(tickNonPassenger)`: falling blocks, TNT, players
    /// (`ServerPlayer.tick`), mobs, arrows and orbs.
    fn tick_entities(&mut self) {
        let has_players = !self.level.entities.players.is_empty();
        self.mobs
            .begin_tick(self.env, MobSettings::default(), has_players);
        minecart::players_push(&mut self.level.entities);
        let mut cursor = self.level.entities.store.tick_cursor();
        while let Some(id) = self.level.entities.store.next_to_tick(&mut cursor) {
            if !vanillite_game::entities::in_ticking_range(&self.level, id) {
                continue;
            }
            let Some(mut entity) = self.level.entities.store.get_mut(id) else {
                continue;
            };
            entity.set_old_pos();
            let kind = entity.as_ref().kind();
            let pos = entity.as_ref().pos();
            if kind == ids::LIGHTNING_BOLT {
                let mut hits = std::mem::take(&mut self.bolt_hits);
                lightning::tick(&mut self.level, id, &mut hits);
                if !hits.is_empty() {
                    self.with_env(|level, ents, env, cx| {
                        for &target in &hits {
                            lightning::thunder_hit(level, ents, env, cx, target);
                        }
                    });
                    self.finish_removals();
                }
                self.bolt_hits = hits;
                continue;
            }
            if kind == ids::FALLING_BLOCK || kind == ids::TNT {
                vanillite_game::entities::tick_entity(&mut self.level, id, kind);
                self.explosion_hurts();
                continue;
            }
            if kind == ids::PLAYER {
                if let Some(p) = self.level.entities.player_mut(id) {
                    p.connection.movement.tick_client_load_timeout();
                    survival_player::tick_entity(p);
                }
                continue;
            }
            if kind == ids::ITEM {
                let level = &mut self.level;
                let random = &mut self.item_random;
                vanillite_server::tick_item(
                    &mut level.entities,
                    &level.chunks,
                    id,
                    &mut || random.next_float(),
                    MIN_Y,
                    ITEM_LIFETIME,
                    &mut self.item_scratch,
                );
                continue;
            }
            let is_mob = is_mob_kind(kind);
            let is_special = mob::boss::special::is_special_kind(kind);
            let is_minecart = MinecartKind::of(kind).is_some();
            let is_boat = vanillite_game::vehicle::BoatKind::of(kind).is_some();
            let is_missile = vanillite_game::mob::missile::is_missile_kind(kind);
            if !is_mob
                && !is_special
                && !is_minecart
                && !is_boat
                && !is_missile
                && !projectile::is_arrow_kind(kind)
                && kind != ids::EXPERIENCE_ORB
                && kind != ids::AREA_EFFECT_CLOUD
            {
                continue;
            }
            if !self
                .level
                .is_block_ticking(BlockPos::containing(pos.x, pos.y, pos.z).chunk())
            {
                continue;
            }
            if kind == ids::EXPERIENCE_ORB {
                vanillite_game::entities::tick_entity(&mut self.level, id, kind);
                continue;
            }
            self.with_env(|level, ents, env, cx| {
                if is_mob {
                    mob::tick(level, ents, env, cx, id);
                } else if is_special {
                    mob::boss::special::tick(level, ents, env, cx, id);
                } else if is_minecart {
                    minecart::tick(level, ents, env, cx, id);
                } else if is_boat {
                    vanillite_game::vehicle::tick_boat(level, ents, env, cx, id);
                } else if projectile::is_arrow_kind(kind) {
                    projectile::tick_arrow(level, ents, env, cx, id);
                } else if is_missile {
                    vanillite_game::mob::missile::tick(level, ents, env, cx, id);
                } else {
                    vanillite_game::effect::cloud::tick(level, ents, env, cx, id);
                }
            });
            if !self.mobs.actions.is_empty() {
                self.apply_actions();
            }
            let ents = &mut self.level.entities;
            if ents.mob(id).is_some_and(|m| m.removed) {
                ents.despawn(id);
            }
            self.finish_removals();
        }
        self.spawn_drops();
    }

    /// What the last mob tick queued (`WorldMobs::apply_actions` of the server).
    fn apply_actions(&mut self) {
        for action in std::mem::take(&mut self.mobs.actions) {
            match action {
                WorldAction::Explode(e) => {
                    e.run(&mut self.level, &self.rules);
                    self.explosion_hurts();
                }
                WorldAction::ExplodeWith {
                    source,
                    indirect,
                    pos,
                    radius,
                    blocks,
                } => {
                    let interaction = if !blocks {
                        BlockInteraction::Keep
                    } else if self.rules.bool(Rule::BLOCK_EXPLOSION_DROP_DECAY) {
                        BlockInteraction::DestroyWithDecay
                    } else {
                        BlockInteraction::Destroy
                    };
                    self.level
                        .explode(source, indirect, pos, radius, interaction);
                    self.explosion_hurts();
                }
                WorldAction::Level(action) => action.apply(&mut self.level),
                other => {
                    other.apply_to_level(&mut self.level);
                }
            }
        }
        self.finish_removals();
    }

    /// The explosion damage of the living entities the last explosions hit
    /// (`WorldMobs::apply_explosion_hurts` of the server).
    fn explosion_hurts(&mut self) {
        if self.level.output().explosion_hurts.is_empty() {
            return;
        }
        let hurts = std::mem::take(&mut self.level.output().explosion_hurts);
        self.with_env(|level, ents, env, cx| {
            for hurt in hurts {
                let mut source = match (hurt.source, hurt.indirect) {
                    (Some(direct), Some(cause)) => {
                        combat::entity_source(ents, env.types.player_explosion, cause, direct)
                    }
                    (direct, _) => {
                        let mut source = DamageSource::of(env.types.explosion);
                        source.direct = direct;
                        source.position = Some(
                            direct
                                .and_then(|d| ents.store.get(d))
                                .map_or(hurt.center, |e| e.pos()),
                        );
                        source
                    }
                };
                source.position.get_or_insert(hurt.center);
                if ents.minecart(hurt.target).is_some() {
                    minecart::hurt(ents, env, cx, hurt.target, source, hurt.amount);
                    continue;
                }
                combat::hurt(level, ents, env, cx, hurt.target, source, hurt.amount);
            }
        });
        self.finish_removals();
    }

    fn finish_removals(&mut self) {
        for d in std::mem::take(&mut self.mobs.discarded) {
            self.level.entities.despawn(d);
        }
    }

    /// The item entities of the drops since the last call (`spawn_drops` of the server); the
    /// other effects go to clients nobody has.
    fn spawn_drops(&mut self) {
        for effect in std::mem::take(&mut self.effects) {
            if let Effect::Experience { pos, amount } = effect {
                vanillite_game::experience_orb::with_entity_random(|random| {
                    vanillite_game::experience_orb::award_in(
                        &mut self.level.entities,
                        &self.network_ids,
                        random,
                        pos,
                        vanillite_base::Vec3::default(),
                        amount,
                    );
                });
            }
            if let Effect::DropItem {
                uuid,
                pos,
                velocity,
                y_rot,
                pickup_delay,
                stack,
            } = effect
            {
                self.level.entities.spawn_item(
                    self.network_ids.next(),
                    uuid,
                    pos,
                    velocity,
                    y_rot,
                    ItemData::new(stack, pickup_delay, None),
                );
            }
        }
    }

    /// `ServerPlayer.doTick` of every player (the connection tick).
    fn tick_players(&mut self) {
        let players: Vec<EntityId> = self
            .level
            .entities
            .players
            .iter()
            .map(|(id, _)| id)
            .collect();
        if players.is_empty() {
            return;
        }
        self.with_env(|level, ents, env, cx| {
            for &id in &players {
                let Entities { store, players, .. } = &mut *ents;
                let Some(mut entity) = store.get_mut(id) else {
                    continue;
                };
                let Some(data) = players.get_mut(entity.as_ref().kind_row()) else {
                    continue;
                };
                let std::ops::ControlFlow::Continue(result) =
                    survival_player::do_tick_ai(&mut entity, data, env)
                else {
                    continue;
                };
                // `pushEntities` and `touch` at the end of the player's `aiStep`.
                vanillite_game::mob::living::player_push_entities(ents, id);
                vanillite_game::mob::fam::undead::cubes::player_touch(level, ents, env, cx, id);
                let Entities { store, players, .. } = &mut *ents;
                let Some(mut entity) = store.get_mut(id) else {
                    continue;
                };
                let Some(data) = players.get_mut(entity.as_ref().kind_row()) else {
                    continue;
                };
                survival_player::do_tick_end(&mut entity, data, env, result);
            }
        });
        self.spawn_drops();
    }

    fn named(&self, name: &str) -> Result<EntityId, String> {
        self.names
            .iter()
            .find(|(_, n)| n == name)
            .map(|(id, _)| *id)
            .ok_or_else(|| format!("no scenario entity `{name}`"))
    }

    fn name_of(&self, id: EntityId) -> Option<&str> {
        self.names
            .iter()
            .find(|(i, _)| *i == id)
            .map(|(_, n)| n.as_str())
    }

    fn add_player(&mut self, spec: &EntitySpec, index: usize) {
        let pos = Vec3::new(spec.pos[0], spec.pos[1], spec.pos[2]);
        let (_tx, inbound) = vanillite_protocol::inbound_queue(64);
        let chat = ChatSettings::default();
        let mut data = PlayerData {
            profile: GameProfile {
                name: spec.name.clone(),
                properties: Vec::new(),
            },
            game_mode: vanillite_config::GameMode::Survival,
            previous_game_mode: None,
            client: ClientInformation::default(),
            chunk_view: ChunkView {
                center: BlockPos::containing(pos.x, pos.y, pos.z).chunk(),
                view_distance: 2,
            },
            connection: Connection::new(
                Arc::new(NoSink),
                inbound,
                pos,
                true,
                true,
                &chat,
                Arc::new(Mutex::new(ChatInbox::default())),
            ),
            inventory: vanillite_game::inventory::PlayerInventory::default(),
            last_equipment: std::array::from_fn(|_| vanillite_game::item::ItemStack::EMPTY),
            survival: vanillite_game::survival::player::Survival::default(),
            post_effects: Vec::new(),
            stats: vanillite_game::stats::StatsCounter::default(),
            advancements: vanillite_game::advancements::PlayerAdvancements::load(
                Arc::new(vanillite_game::advancements::Advancements::default()),
                None,
            ),
            recipe_book: vanillite_game::recipe::book::PlayerRecipeBook::default(),
        };
        let rot = Rotation {
            y_rot: spec.rot[0],
            x_rot: spec.rot[1],
            y_head_rot: spec.rot[0],
        };
        // Nobody moves the mock player: the server does, like Vanilla's.
        data.survival.simulate_movement = true;
        data.survival.y_body_rot = spec.rot[0];
        let id =
            self.level
                .entities
                .spawn_player(&self.network_ids, spec.uuid(index), pos, rot, data);
        self.names.push((id, spec.name.clone()));
    }

    fn add_mob(&mut self, spec: &EntitySpec, index: usize) -> Result<(), String> {
        if let Some(kind) = EntityTypeId::by_name(&spec.kind).and_then(MinecartKind::of) {
            let rot = Rotation {
                y_rot: spec.rot[0],
                x_rot: spec.rot[1],
                y_head_rot: spec.rot[0],
            };
            let pos = Vec3::new(spec.pos[0], spec.pos[1], spec.pos[2]);
            let id = minecart::spawn(
                &mut self.level.entities,
                &self.network_ids,
                spec.uuid(index),
                pos,
                rot,
                minecart::MinecartData::new(kind, spec.seed),
            );
            if let (Some(m), Some(mut e)) = (spec.motion, self.level.entities.store.get_mut(id)) {
                e.set_velocity(Vec3::new(m[0], m[1], m[2]));
            }
            self.names.push((id, spec.name.clone()));
            return Ok(());
        }
        let kind = EntityTypeId::by_name(&spec.kind)
            .and_then(Kind::of)
            .ok_or_else(|| format!("{}: no mob behaviour in Vanillite", spec.kind))?;
        let mut m = MobData::new(kind, spec.seed);
        if spec.finalize {
            // `finalizeSpawn(level, difficulty, COMMAND, null)` with the level random.
            let pos = BlockPos::containing(spec.pos[0], spec.pos[1], spec.pos[2]);
            let mut random = std::mem::replace(self.level.random(), LegacyRandom::new(0));
            {
                let mut ctx = vanillite_game::mob::fam::SpawnContext::simple(
                    self.difficulty,
                    vanillite_game::mob::fam::SpawnReason::Command,
                );
                ctx.pos = pos;
                ctx.level = Some(&self.level);
                ctx.env = self.env;
                kinds::finalize_spawn(&mut m, &mut ctx, &mut random);
            }
            *self.level.random() = random;
        }
        if let Some(baby) = spec.baby {
            m.baby = baby;
            m.age = if baby { -24000 } else { 0 };
        }
        m.y_body_rot = spec.rot[0];
        let rot = Rotation {
            y_rot: spec.rot[0],
            x_rot: spec.rot[1],
            y_head_rot: spec.rot[0],
        };
        let pos = Vec3::new(spec.pos[0], spec.pos[1], spec.pos[2]);
        let id = mob::spawn(
            &mut self.level.entities,
            &self.network_ids,
            spec.uuid(index),
            pos,
            rot,
            m,
        );
        self.names.push((id, spec.name.clone()));
        Ok(())
    }

    /// The trace detail of an entity (see [`crate::trace`]).
    fn detail(&self, row: usize) -> Option<Map<String, Value>> {
        let ents = &self.level.entities;
        let core = ents.store.columns();
        let id = core.id[row];
        let mut d = Map::new();
        if let Some(name) = self.name_of(id) {
            d.insert("name".into(), json!(name));
        }
        if let Some(item) = ents.item(id) {
            d.insert(
                "item".into(),
                json!(vanillite_data::item::ItemId(item.stack.item).name()),
            );
            d.insert("count".into(), json!(item.stack.count));
        }
        let bits = |f: f32| json!(f.to_bits() as i32);
        let rot = core.rot[row];
        let living = |d: &mut Map<String, Value>, body: f32, hp: f32, death: i32| {
            d.insert(
                "rot".into(),
                json!([
                    bits(rot.y_rot),
                    bits(rot.x_rot),
                    bits(rot.y_head_rot),
                    bits(body)
                ]),
            );
            d.insert("hp".into(), bits(hp));
            if death > 0 {
                d.insert("death".into(), json!(death));
            }
        };
        if let Some(p) = ents.player(id) {
            living(
                &mut d,
                p.survival.y_body_rot,
                p.survival.health,
                p.survival.death_time,
            );
        }
        if ents.minecart(id).is_some() {
            d.insert("rot".into(), json!([bits(rot.y_rot), bits(rot.x_rot)]));
        }
        if let Some(m) = ents.mob(id) {
            living(&mut d, m.y_body_rot, m.health, m.death_time);
            d.insert("rng".into(), json!(m.random.state()));
            if let Some(t) = m.target {
                let name = self.name_of(t).map_or_else(
                    || {
                        ents.store
                            .get(t)
                            .map_or("?", |e| e.kind().name())
                            .to_owned()
                    },
                    str::to_owned,
                );
                d.insert("target".into(), json!(name));
            }
            d.insert("goals".into(), running(m.kind, &m.goals.slots));
            d.insert("targets".into(), running(m.kind, &m.targets.slots));
            if let Some(path) = m.nav.path.as_ref().filter(|p| !p.is_done()) {
                let nodes: Vec<Value> = path
                    .nodes
                    .iter()
                    .map(|n| json!([n.x, n.y, n.z, path_type_name(n.kind)]))
                    .collect();
                d.insert(
                    "path".into(),
                    json!({
                        "to": [path.target.x, path.target.y, path.target.z],
                        "next": path.next,
                        "reach": path.reached,
                        "nodes": nodes,
                    }),
                );
            }
        }
        (!d.is_empty()).then_some(d)
    }
}

/// The running goals by Vanilla class name, sorted.
fn running(kind: Kind, slots: &[Slot]) -> Value {
    let mut names: Vec<&str> = slots
        .iter()
        .filter(|s| s.running)
        .map(|s| goal_name(kind, &s.goal))
        .collect();
    names.sort_unstable();
    json!(names)
}

/// The Vanilla class of a goal of a mob of `kind`.
fn goal_name(kind: Kind, goal: &Goal) -> &'static str {
    match goal {
        Goal::Float => "FloatGoal",
        Goal::Panic { .. } => "PanicGoal",
        Goal::Stroll { .. } => "WaterAvoidingRandomStrollGoal",
        Goal::LookAtPlayer { .. } if kind == Kind::Fox => "FoxLookAtPlayerGoal",
        Goal::LookAtPlayer { .. } => "LookAtPlayerGoal",
        Goal::RandomLook { .. } => "RandomLookAroundGoal",
        Goal::Melee { kind, .. } => match kind {
            MeleeKind::Plain => "MeleeAttackGoal",
            MeleeKind::Zombie => "ZombieAttackGoal",
            MeleeKind::Spider => "SpiderAttackGoal",
        },
        Goal::Swell => "SwellGoal",
        Goal::Leap { .. } => "LeapAtTargetGoal",
        Goal::FleeSun { .. } => "FleeSunGoal",
        Goal::RestrictSun => "RestrictSunGoal",
        Goal::Bow { .. } => "RangedBowAttackGoal",
        Goal::EatBlock { .. } => "EatBlockGoal",
        Goal::FollowParent { .. } => "FollowParentGoal",
        Goal::NearestTarget { spider: true, .. } => "SpiderTargetGoal",
        Goal::NearestTarget { .. } => "NearestAttackableTargetGoal",
        Goal::HurtBy { .. } => "HurtByTargetGoal",
        Goal::EndermanFreeze { .. } => "EndermanFreezeWhenLookedAt",
        Goal::EndermanStare { .. } => "EndermanLookForPlayerGoal",
        Goal::Breed { .. } => "BreedGoal",
        Goal::Tempt { .. } => "TemptGoal",
        Goal::SitWhenOrdered => "SitWhenOrderedToGoal",
        Goal::EndermanLeaveBlock => "EndermanLeaveBlockGoal",
        Goal::EndermanTakeBlock => "EndermanTakeBlockGoal",
        Goal::FollowOwner { .. } => "FollowOwnerGoal",
        Goal::OwnerHurtBy { .. } => "OwnerHurtByTargetGoal",
        Goal::OwnerHurt { .. } => "OwnerHurtTargetGoal",
        Goal::Common(c) => common_goal_name(c),
        Goal::Fam(f) => fam_goal_name(kind, f),
        Goal::Guardian(g) => {
            use vanillite_game::mob::boss::guardian::GuardianGoal as Gg;
            match g {
                Gg::Attack { .. } => "GuardianAttackGoal",
                Gg::MoveTowardsRestriction { .. } => "MoveTowardsRestrictionGoal",
                Gg::Stroll { .. } => "RandomStrollGoal",
                Gg::LookAtGuardian { .. } => "LookAtPlayerGoal",
                Gg::Target { .. } => "NearestAttackableTargetGoal",
            }
        }
        Goal::Wither(w) => {
            use vanillite_game::mob::boss::wither::WitherGoal as Wg;
            match w {
                Wg::DoNothing => "WitherDoNothingGoal",
                Wg::RangedAttack { .. } => "RangedAttackGoal",
                Wg::RandomFly { .. } => "WaterAvoidingRandomFlyingGoal",
                Wg::NearestLiving { .. } => "NearestAttackableTargetGoal",
            }
        }
    }
}

/// The Vanilla class of a goal many families share.
fn common_goal_name(goal: &CommonGoal) -> &'static str {
    match goal {
        CommonGoal::NearestAttackable { .. } => "NearestAttackableTargetGoal",
        CommonGoal::Avoid { .. } => "AvoidEntityGoal",
        CommonGoal::Ranged { .. } => "RangedAttackGoal",
        CommonGoal::MoveTowardsRestriction { .. } => "MoveTowardsRestrictionGoal",
        CommonGoal::ResetUniversalAnger { .. } => "ResetUniversalAngerTargetGoal",
        CommonGoal::Move(m) => match m {
            MoveGoal::RemoveBlock {
                sounds: RemoveSounds::ZombieEgg,
                ..
            } => "ZombieAttackTurtleEggGoal",
            MoveGoal::RemoveBlock { .. } => "RemoveBlockGoal",
            MoveGoal::OpenDoor { .. } => "OpenDoorGoal",
            MoveGoal::BreakDoor { .. } => "BreakDoorGoal",
            MoveGoal::BreathAir => "BreathAirGoal",
            MoveGoal::TryFindLiquid { .. } => "TryFindWaterGoal",
            MoveGoal::ClimbOnTopOfPowderSnow => "ClimbOnTopOfPowderSnowGoal",
            _ => "RandomStrollGoal",
        },
    }
}

/// The Vanilla class of a family goal of a mob of `kind`, read from the goal's `Debug` form
/// (`Illager(Spell(UseSpell { spell: 1, .. }))`): the family, its variant and what selects the
/// class inside it.
fn fam_goal_name(kind: Kind, goal: &FamGoal) -> &'static str {
    let debug = format!("{goal:?}");
    let words: Vec<&str> = debug
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|s| !s.is_empty())
        .collect();
    let word = |i: usize| words.get(i).copied().unwrap_or("");
    let field = |name: &str| {
        words
            .iter()
            .position(|w| *w == name)
            .map_or("", |i| word(i + 1))
    };
    let (family, variant) = (word(0), word(1));
    match (family, variant) {
        ("Undead", "DrownedMelee") => "DrownedAttackGoal",
        ("Undead", "DrownedTrident") => "DrownedTridentAttackGoal",
        ("Undead", "HurtByIgnoringDrowned") => "HurtByTargetGoal",
        ("Undead", "SkeletonMelee") => "MeleeAttackGoal",
        ("Undead", "CubeFloat") => "CubeMobFloatGoal",
        ("Undead", "CubeAttack") => "CubeMobAttackGoal",
        ("Undead", "CubeRandomDirection") => "CubeMobRandomDirectionGoal",
        ("Undead", "CubeKeepOnJumping") => "CubeMobKeepOnJumpingGoal",
        ("Undead", "PhantomSweep") => "PhantomSweepAttackGoal",
        ("Undead", "PhantomCircle") => "PhantomCircleAroundAnchorGoal",
        ("Undead", "PhantomTargetPlayer") => "PhantomAttackPlayerTargetGoal",
        ("Illager", "Patrol") => "LongDistancePatrolGoal",
        ("Illager", "HoldGround") => "HoldGroundAttackGoal",
        ("Illager", "HurtBy") => "HurtByTargetGoal",
        ("Illager", "LookAt") => "LookAtPlayerGoal",
        ("Illager", "Stroll") => match kind {
            Kind::Pillager | Kind::Witch => "WaterAvoidingRandomStrollGoal",
            _ => "RandomStrollGoal",
        },
        ("Illager", "Locked") => common_name_of(word(2)),
        ("Illager", "Target") => match field("gate") {
            "WitchCanAttack" => "NearestAttackableWitchTargetGoal",
            "Johnny" => "VindicatorJohnnyAttackGoal",
            "NotPeaceful" => "ShulkerNearestAttackGoal",
            _ => "NearestAttackableTargetGoal",
        },
        ("Illager", "WitchHeal") => "NearestHealableRaiderTargetGoal",
        ("Illager", "Crossbow") => "RangedCrossbowAttackGoal",
        ("Illager", "SpellCasting") => match kind {
            Kind::Evoker => "EvokerCastingSpellGoal",
            _ => "SpellcasterCastingSpellGoal",
        },
        ("Illager", "Spell") => match field("spell") {
            "1" => "EvokerSummonSpellGoal",
            "2" => "EvokerAttackSpellGoal",
            "3" => "EvokerWololoSpellGoal",
            "4" => "IllusionerMirrorSpellGoal",
            "5" => "IllusionerBlindnessSpellGoal",
            _ => "SpellcasterUseSpellGoal",
        },
        ("Illager", "VexCharge") => "VexChargeAttackGoal",
        ("Illager", "VexCopyOwner") => "VexCopyOwnerTargetGoal",
        ("Illager", "GhastFloat") => "RandomFloatAroundGoal",
        ("Illager", "GhastShoot") => "GhastShootFireballGoal",
        ("Companion", "Guard") => match (word(3), kind) {
            ("Avoid", Kind::Cat) => "CatAvoidEntityGoal",
            ("Avoid", Kind::Ocelot) => "OcelotAvoidEntityGoal",
            ("NearestAttackable", _) if word(2) == "NotTame" => "NonTameRandomTargetGoal",
            (inner, _) => common_name_of(inner),
        },
        ("Companion", "TamePanic") => "TamableAnimalPanicGoal",
        ("Companion", _) => match (variant, word(2)) {
            ("Horse", "RunAround") => "RunAroundLikeCrazyGoal",
            ("Llama", "FollowCaravan") => "LlamaFollowCaravanGoal",
            ("Llama", "AttackWolf") => "LlamaAttackWolfGoal",
            ("Llama", "DefendTrader") => "TraderLlamaDefendWanderingTraderGoal",
            ("Wolf", "AvoidLlama") => "WolfAvoidEntityGoal",
            ("Feline", "RelaxOnOwner") => "CatRelaxOnOwnerGoal",
            ("Feline", "LieOnBlock") => "CatLieOnBedGoal",
            ("Feline", "SitOnBlock") => "CatSitOnBlockGoal",
            ("Feline", "Attack") => "OcelotAttackGoal",
            ("Parrot", "Wander") => "ParrotWanderGoal",
            ("Fox", "Float") => "FoxFloatGoal",
            ("Fox", "Panic") => "FoxPanicGoal",
            ("Fox", "Avoid") => "AvoidEntityGoal",
            ("Fox", "Pounce") => "FoxPounceGoal",
            ("Fox", "EatBerries") => "FoxEatBerriesGoal",
            ("Fox", "SearchForItems") => "FoxSearchForItemsGoal",
            ("Fox", "DefendTrusted") => "DefendTrustedTargetGoal",
            ("Panda", "BreedCheck") => "PandaBreedGoal",
            ("Panda", inner) => leak(format!("Panda{inner}Goal")),
            ("Bear", "Panic") => "PolarBearPanicGoal",
            ("Bear", "AttackPlayers") => "PolarBearAttackPlayersGoal",
            ("Rabbit", "Panic") => "RabbitPanicGoal",
            ("Rabbit", "Avoid") => "RabbitAvoidEntityGoal",
            (_, inner) => leak(format!("{inner}Goal")),
        },
        ("Mount", "Ghast") => match word(2) {
            "Float" => "HappyGhastFloatGoal",
            "Tempt" => "TemptGoal",
            _ => "RandomFloatAroundGoal",
        },
        ("Wildlife", "Swim") => match kind {
            Kind::Dolphin => "RandomSwimmingGoal",
            _ => "FishSwimGoal",
        },
        ("Wildlife", "Puff") => "PufferfishPuffGoal",
        ("Wildlife", "TurtleStroll") => "TurtleRandomStrollGoal",
        ("Wildlife", "BeeValidateFlower") => "ValidateFlowerGoal",
        (_, variant) => leak(if variant.ends_with("Goal") {
            variant.to_owned()
        } else {
            format!("{variant}Goal")
        }),
    }
}

/// The Vanilla class of a [`CommonGoal`] variant by name.
fn common_name_of(variant: &str) -> &'static str {
    match variant {
        "NearestAttackable" => "NearestAttackableTargetGoal",
        "Avoid" => "AvoidEntityGoal",
        "Ranged" => "RangedAttackGoal",
        "MoveTowardsRestriction" => "MoveTowardsRestrictionGoal",
        "ResetUniversalAnger" => "ResetUniversalAngerTargetGoal",
        other => leak(format!("{other}Goal")),
    }
}

/// A name for the trace, kept for the process (one per distinct name).
fn leak(name: String) -> &'static str {
    use std::collections::HashSet;
    static NAMES: Mutex<Option<HashSet<&'static str>>> = Mutex::new(None);
    let mut names = NAMES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let names = names.get_or_insert_with(HashSet::new);
    if let Some(n) = names.get(name.as_str()) {
        return n;
    }
    let n: &'static str = Box::leak(name.into_boxed_str());
    names.insert(n);
    n
}

/// `PathType.name()`: `WalkableDoor` → `WALKABLE_DOOR`.
fn path_type_name(t: mob::path::PathType) -> String {
    let mut out = String::new();
    for (i, c) in format!("{t:?}").chars().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            out.push('_');
        }
        out.push(c.to_ascii_uppercase());
    }
    out
}

/// The recorder's default game rules (`ScenarioRecorder.DEFAULT_RULES`: no random ticks, no
/// mobs, no time or weather) with the scenario's rules on top.
/// A new level of `spec` over the chunks in `store`.
fn open_level(
    spec: &WorldSpec,
    store: Arc<MemoryStore>,
) -> Result<(Level, Arc<NetworkIds>), String> {
    let biomes = Arc::clone(BIOME_NAMES.get().expect("installed"));
    let default_biome = biomes
        .iter()
        .position(|b| *b == spec.biome)
        .and_then(|i| u16::try_from(i).ok())
        .ok_or_else(|| format!("unknown biome `{}`", spec.biome))?;
    let info = WorldInfo {
        dimension: "minecraft:overworld".to_owned(),
        dir: std::env::temp_dir(),
        min_y: MIN_Y,
        height: HEIGHT,
        seed: spec.seed,
        has_sky_light: true,
        biomes,
        default_biome,
        air: BlockState::AIR.0,
    };
    let mut layers = Vec::new();
    for (block, height) in &spec.layers {
        let state = BlockId::by_name(block)
            .ok_or_else(|| format!("unknown layer block `{block}`"))?
            .default_state();
        layers.extend(std::iter::repeat_n(state.0, *height as usize));
    }
    let codec = AnvilCodec::new(&info, Compression::None);
    let slots = ChunkSlots {
        store,
        codec: Arc::new(codec),
        generator: Arc::new(FlatGenerator::new(&info, &layers)),
        light: if spec.light {
            vanillite_light::BfsLightEngineProvider.create(&info)
        } else {
            Arc::new(NoLight)
        },
        executor: Arc::new(InlineExecutor),
    };
    let chunks = Chunks::new(info, ChunkSettings::default(), slots);
    let mut settings = LevelSettings::default();
    apply_rules(&mut settings, &mut GameRules::default(), &spec.rules)?;
    let network_ids = Arc::new(NetworkIds::new());
    let mut level = Level::new(
        chunks,
        settings,
        Arc::new(Behaviours::vanilla()),
        Arc::clone(&network_ids),
        spec.seed,
    );
    level.set_tree_features(Arc::clone(TREES.get().expect("installed")));
    level.set_climate(Arc::new(Climate {
        biomes: BIOMES.get().expect("installed").clone(),
        manager: vanillite_worldgen::biome::BiomeManager::for_world_seed(spec.seed),
        sea_level: FLAT_SEA_LEVEL,
    }));
    let (rain, thunder) = match spec.weather.as_str() {
        "clear" => (0.0, 0.0),
        "rain" => (1.0, 0.0),
        "thunder" => (1.0, 1.0),
        w => return Err(format!("unknown weather `{w}`")),
    };
    level.set_weather(LevelWeather {
        rain_level: rain,
        o_rain_level: rain,
        thunder_level: thunder,
        o_thunder_level: thunder,
    });
    Ok((level, network_ids))
}

fn apply_rules(
    settings: &mut LevelSettings,
    game_rules: &mut GameRules,
    rules: &std::collections::BTreeMap<String, String>,
) -> Result<(), String> {
    settings.random_tick_speed = 0;
    for name in [
        "random_tick_speed",
        "spawn_mobs",
        "advance_time",
        "advance_weather",
    ] {
        if let Some(rule) = Rule::by_name(name) {
            game_rules.set(rule, 0);
        }
    }
    for (name, value) in rules {
        let bad = || format!("game rule {name}: bad value `{value}`");
        let supported = matches!(
            name.as_str(),
            "random_tick_speed"
                | "water_source_conversion"
                | "lava_source_conversion"
                | "block_drops"
                | "entity_drops"
                | "max_snow_accumulation_height"
                | "fire_spread_radius_around_player"
                | "spread_vines"
                | "spawn_mobs"
                | "advance_time"
                | "advance_weather"
                | "mob_griefing"
                | "mob_drops"
        );
        let rule = Rule::by_name(name)
            .filter(|_| supported)
            .ok_or_else(|| format!("game rule {name} is not supported by the game world"))?;
        let v = match value.as_str() {
            "true" => 1,
            "false" => 0,
            v => v.parse().map_err(|_| bad())?,
        };
        game_rules.set(rule, v);
    }
    settings.apply_game_rules(game_rules);
    Ok(())
}

impl ScenarioWorld for GameWorld {
    fn is_loaded(&self, pos: BlockPos) -> bool {
        self.level.is_loaded(pos)
    }

    fn set_forced(&mut self, chunks: &[ChunkPos], forced: bool) -> Result<(), String> {
        for &pos in chunks {
            let Level {
                chunks: runtime,
                forced: set,
                ..
            } = &mut self.level;
            set.set(runtime, pos, forced);
        }
        for _ in 0..SETTLE_LIMIT {
            // The chunk system and the entity manager only: no game tick.
            self.level.tick_chunk_runtime();
            self.level.tick_entity_manager();
            if self.settled() {
                return Ok(());
            }
        }
        Err("the chunk system does not settle".to_owned())
    }

    /// `MinecraftServer.stopServer` (every chunk saved with its entities, block entities and
    /// scheduled ticks, and the chunk tickets), then a new level on the saved chunks: the saved
    /// tickets come back and `prepareLevels` runs the chunk system until their chunks are full.
    /// The game time is kept like `level.dat` keeps it.
    fn restart(&mut self) -> Result<(), String> {
        if !self.names.is_empty() || !self.level.entities.players.is_empty() {
            return Err("scenarios with entities cannot restart".to_owned());
        }
        self.level.save(true);
        let tickets = self.level.forced.save_tickets(&SavedTickets::default());
        // Through the saved file's bytes, like the saved data store.
        let mut bytes = Vec::new();
        vanillite_nbt::write::write_named("", &tickets, &mut bytes).map_err(|e| e.to_string())?;
        let budget = &mut vanillite_nbt::Budget::new(
            vanillite_nbt::read::FILE_QUOTA,
            vanillite_nbt::read::MAX_DEPTH,
        );
        let tickets = vanillite_nbt::read::read_named(&mut bytes.as_slice(), budget)
            .map_err(|e| e.to_string())?;
        let game_time = self.level.game_time();
        let (mut level, network_ids) = open_level(&self.spec, Arc::clone(&self.store))?;
        level.set_game_time(game_time);
        let Level { chunks, forced, .. } = &mut level;
        forced.load_tickets(chunks, &tickets);
        self.level = level;
        self.network_ids = network_ids;
        self.mobs = MobContext::default();
        self.effects.clear();
        for _ in 0..SETTLE_LIMIT {
            self.level.tick_chunk_runtime();
            if self.level.chunks.pending_full() == 0 {
                return Ok(());
            }
        }
        Err("the saved forced chunks do not load".to_owned())
    }

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
    /// entities, entity management (pending entity loads).
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
        level.tick_entity_manager();
        // Nobody watches: drop what would go to clients.
        *level.output() = LevelOutput::default();
        let mut changes = Vec::new();
        level.chunks.take_block_changes(&mut changes);
        let mut light = Vec::new();
        level.chunks.take_light_changes(&mut light);
        self.tick_players();
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

    fn light(&mut self, min: BlockPos, max: BlockPos, out: &mut Vec<u8>) {
        if !self.light {
            return;
        }
        let chunks = &mut self.level.chunks;
        chunks.flush_light();
        let layers = HEIGHT as usize / 16 + 2;
        let mut storing = std::collections::HashMap::new();
        for y in min.y..=max.y {
            for z in min.z..=max.z {
                for x in min.x..=max.x {
                    let pos = BlockPos::new(x, y, z);
                    let storing = storing
                        .entry(pos.chunk())
                        .or_insert_with(|| storing_sections(chunks, pos.chunk(), layers));
                    out.push(visible_light(chunks, storing, pos));
                }
            }
        }
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
            // A player moves by its client: its server-side motion is not part of the trace.
            let v = if core.kind[row] == ids::PLAYER {
                Vec3::ZERO
            } else {
                core.velocity[row]
            };
            out.push(EntityObs {
                kind: core.kind[row].name().to_owned(),
                pos: [pos.x, pos.y, pos.z],
                motion: [v.x, v.y, v.z],
                detail: self.detail(row),
            });
        }
    }

    /// Players, the level random reseeded with the world seed, then the other entities.
    fn add_entities(&mut self, entities: &[EntitySpec], entity_seed: i64) -> Result<(), String> {
        for (i, spec) in entities.iter().enumerate() {
            if spec.is_player() {
                self.add_player(spec, i);
            }
        }
        *self.level.random() = LegacyRandom::new(self.seed);
        self.level.set_rand_value(self.seed as i32);
        for (i, spec) in entities.iter().enumerate() {
            if !spec.is_player() {
                self.add_mob(spec, i)?;
            }
        }
        self.level.set_entity_seed(entity_seed);
        Ok(())
    }

    fn lightning(&mut self, column: BlockPos) -> Result<(), String> {
        let input = lightning::StrikeInput {
            spawn_mobs: self.rules.bool(Rule::SPAWN_MOBS),
            clock_time: self.clock_time,
        };
        lightning::strike(&mut self.level, column, input);
        Ok(())
    }

    fn level_rng(&mut self) -> Option<i64> {
        (self.level_rng || !self.names.is_empty()).then(|| self.level.random().state())
    }

    fn hurt(&mut self, hurt: &Hurt) -> Result<(), String> {
        let target = self.named(&hurt.entity)?;
        let by = hurt.by.as_deref().map(|b| self.named(b)).transpose()?;
        let is_player = by.is_some_and(|b| self.level.entities.player(b).is_some());
        self.with_env(|level, ents, env, cx| {
            let source = match by {
                Some(b) if is_player => combat::entity_source(ents, env.types.player_attack, b, b),
                Some(b) => combat::entity_source(ents, env.types.mob_attack, b, b),
                None => DamageSource::of(env.types.generic),
            };
            combat::hurt(level, ents, env, cx, target, source, hurt.amount);
        });
        self.finish_removals();
        self.spawn_drops();
        Ok(())
    }

    fn command(&mut self, line: &str) -> Result<(), String> {
        vanillite_server::run_command_on_level(
            command_server(),
            &mut self.level,
            Vec3::ZERO,
            line,
        )?;
        self.finish_removals();
        self.spawn_drops();
        Ok(())
    }

    fn throw_potion(&mut self, op: &crate::scenario::ThrowPotionOp) -> Result<(), String> {
        use vanillite_game::effect::potion::PotionContents;
        use vanillite_game::effect::thrown::{self, ThrownPotion};
        let by = op.by.as_deref().map(|b| self.named(b)).transpose()?;
        let item = vanillite_data::registry::ITEM
            .id(&op.item)
            .ok_or_else(|| format!("unknown item {}", op.item))?;
        let potion = vanillite_data::registry::POTION
            .id(&op.potion)
            .ok_or_else(|| format!("unknown potion {}", op.potion))?;
        let mut stack = vanillite_game::item::ItemStack::new(item, 1);
        stack.patch_mut().set(
            vanillite_protocol::components::ids::POTION_CONTENTS,
            PotionContents::of(potion).encode(),
        );
        let kind = thrown::kind_of(&stack).ok_or("throw_potion needs a throwable potion")?;
        let ents = &mut self.level.entities;
        let owner_uuid = by.and_then(|b| ents.store.get(b)).map(|e| e.uuid());
        let uuid = 0x7072_6f6a_0000_0000_0000_0000_0000_0000_u128 | u128::from(potion);
        let [x, y, z] = op.pos;
        let [dx, dy, dz] = op.motion;
        let mut new = vanillite_world::entity::NewEntity::new(
            kind,
            self.network_ids.next(),
            uuid,
            Vec3::new(x, y, z),
        );
        new.velocity = Vec3::new(dx, dy, dz);
        self.level.entities.spawn_special(
            new,
            vanillite_game::mob::boss::special::SpecialData::ThrownPotion(Box::new(ThrownPotion {
                owner: by,
                owner_uuid,
                stack,
                left_owner: false,
                tick_count: 0,
                has_been_shot: false,
            })),
        );
        Ok(())
    }

    fn effect(&mut self, effect: &crate::scenario::EffectOp) -> Result<(), String> {
        let target = self.named(&effect.entity)?;
        let kind = vanillite_game::effect::MobEffect::by_name(&effect.effect)
            .ok_or_else(|| format!("unknown effect {}", effect.effect))?;
        let instance =
            vanillite_game::effect::EffectInstance::new(kind, effect.duration, effect.amplifier);
        self.with_env(|level, ents, env, cx| {
            vanillite_game::effect::living::add_to_entity(level, ents, env, cx, target, instance);
        });
        self.finish_removals();
        self.spawn_drops();
        Ok(())
    }
}

/// Whether Vanilla keeps light data for each light section of a chunk (index 0 is the layer
/// below the world): those within one section in every direction of a non-air section
/// (`LayerLightSectionStorage.updateSectionStatus`).
fn storing_sections(chunks: &Chunks, pos: ChunkPos, layers: usize) -> Vec<bool> {
    let mut storing = vec![false; layers];
    for dz in -1..=1 {
        for dx in -1..=1 {
            let Some(chunk) = chunks.chunk(ChunkPos::new(pos.x + dx, pos.z + dz)) else {
                continue;
            };
            for (section, s) in chunk.chunk.sections().iter().enumerate() {
                if !s.summary().is_empty() {
                    // Section `section` is light layer `section + 1`.
                    let end = (section + 3).min(layers);
                    storing[section..end].fill(true);
                }
            }
        }
    }
    storing
}

/// The light a client sees at `pos` as `sky << 4 | block`: the stored levels in sections with
/// light data; elsewhere no block light and the sky light of the bottom row of the next section
/// with data above, or full sky light (`SkyLightSectionStorage.getLightValue`).
fn visible_light(chunks: &Chunks, storing: &[bool], pos: BlockPos) -> u8 {
    let Some(chunk) = chunks.chunk(pos.chunk()) else {
        return 0;
    };
    let light = chunk.chunk.light();
    let (x, y, z) = (
        (pos.x & 15) as u32,
        (pos.y & 15) as u32,
        (pos.z & 15) as u32,
    );
    let sky_at = |layer: usize, y: u32| match &light.sky()[layer] {
        LightNibbles::Absent => 15,
        data => data.get(x, y, z),
    };
    let layer = ((pos.y - MIN_Y) >> 4) as usize + 1;
    if storing[layer] {
        return (sky_at(layer, y) << 4) | light.block()[layer].get(x, y, z);
    }
    let sky = (layer + 1..storing.len())
        .find(|&l| storing[l])
        .map_or(15, |l| sky_at(l, 0));
    sky << 4
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
