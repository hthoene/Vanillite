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
use vanillite_base::hash::FxHashMap;
use vanillite_base::random::LegacyRandom;
use vanillite_base::{BlockPos, ChunkPos, Vec3};
use vanillite_chunk::ProtoChunk;
use vanillite_chunk::light::LightNibbles;
use vanillite_chunk::slots::{
    FlatGenerator, LightEngine, LightEngineProvider, MemoryStore, Neighbours, WorldInfo,
};
use vanillite_data::block::{BlockId, BlockState};
use vanillite_data::entity::{EntityTypeId, ids};
use vanillite_game::block::Behaviours;
use vanillite_game::entities::{Entities, is_mob_kind};
use vanillite_game::item_entity::ItemData;
use vanillite_game::level::{Level, LevelOutput, LevelSettings, update};
use vanillite_game::minecart::{self, MinecartKind};
use vanillite_game::mob::goals::{Goal, MeleeKind, Slot};
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
use vanillite_protocol::SharedPacket;
use vanillite_storage::{AnvilCodec, Compression};
use vanillite_world::chunks::{
    ChunkSettings, ChunkSlots, Chunks, InlineExecutor, TicketId, Visibility,
};
use vanillite_world::entity::{EntityId, NetworkIds, Rotation};
use vanillite_world::ticks::LevelTicks;

use crate::scenario::{EntitySpec, Hurt, WorldSpec};
use crate::trace::{EntityObs, TickObs};
use crate::world::{Execution, ScenarioWorld};

/// Overworld height range.
const MIN_Y: i32 = -64;
const HEIGHT: u32 = 384;
/// `ChunkLevel.byStatus(ENTITY_TICKING)`: the level of a forced chunk.
const FORCED_LEVEL: u8 = 31;
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
        let _ = PACK.set((
            vanillite_server::load_damage_types(&packs),
            vanillite_server::load_block_facts(&packs),
        ));
        // Loot tables and item tags (block drops), as the server installs them.
        vanillite_game::player::play::install_game_data(Arc::new(vanillite_server::game_data(
            Some(&packs),
        )));
    });
}

/// A headless level over flat chunks.
pub struct GameWorld {
    level: Level,
    /// The forced tickets (loading and simulation) per chunk.
    forced: FxHashMap<ChunkPos, (TicketId, TicketId)>,
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
        let codec = AnvilCodec::new(&info, Compression::None);
        let slots = ChunkSlots {
            store: Arc::new(MemoryStore::default()),
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
        let mut rules = GameRules::default();
        apply_rules(&mut settings, &mut rules, &spec.rules)?;
        let network_ids = Arc::new(NetworkIds::new());
        let level = Level::new(
            chunks,
            settings,
            Arc::new(Behaviours::vanilla()),
            Arc::clone(&network_ids),
            spec.seed,
        );
        let difficulty = match spec.difficulty.as_str() {
            "peaceful" => Difficulty::Peaceful,
            "easy" => Difficulty::Easy,
            "normal" => Difficulty::Normal,
            "hard" => Difficulty::Hard,
            d => return Err(format!("unknown difficulty `{d}`")),
        };
        // The world starts at day time 0 unless the scenario sets one; time does not advance.
        let env = Environment::overworld(spec.time.unwrap_or(0), 0.0, 0.0);
        let mut world = Self {
            level,
            forced: FxHashMap::default(),
            light: spec.observe_light,
            network_ids,
            mobs: MobContext::default(),
            rules,
            difficulty,
            env,
            effects: Vec::new(),
            names: Vec::new(),
            seed: spec.seed,
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
            && self.forced.keys().all(|&pos| {
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
            let raining = |_| false;
            let mut no_gateway = |_| false;
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
                enter_gateway: &mut no_gateway,
                random: &mut random,
                out: &mut self.effects,
                hooks: &vanillite_game::hooks::NoHooks,
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
            let is_mob = is_mob_kind(kind);
            let is_minecart = MinecartKind::of(kind).is_some();
            if !is_mob && !is_minecart && kind != ids::ARROW && kind != ids::EXPERIENCE_ORB {
                continue;
            }
            if !self
                .level
                .is_block_ticking(BlockPos::containing(pos.x, pos.y, pos.z).chunk())
            {
                continue;
            }
            self.with_env(|level, ents, env, cx| {
                if is_mob {
                    mob::tick(level, ents, env, cx, id);
                } else if is_minecart {
                    minecart::tick(level, ents, env, cx, id);
                } else if kind == ids::ARROW {
                    projectile::tick_arrow(level, ents, env, cx, id);
                } else {
                    projectile::tick_orb(level, ents, env, cx, id);
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
            if let WorldAction::Explode(e) = action {
                e.run(&mut self.level, &self.rules);
                self.explosion_hurts();
            } else {
                action.apply_to_level(&mut self.level);
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
        self.with_env(|_, ents, env, _| {
            let Entities { store, players, .. } = ents;
            for (id, data) in players.iter_mut() {
                if let Some(mut entity) = store.get_mut(id) {
                    survival_player::do_tick(&mut entity, data, env);
                }
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
        let (_tx, inbound) = std::sync::mpsc::channel();
        let chat = ChatSettings::default();
        let data = PlayerData {
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
        };
        let rot = Rotation {
            y_rot: spec.rot[0],
            x_rot: spec.rot[1],
            y_head_rot: spec.rot[0],
        };
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
            kinds::finalize_spawn(&mut m, self.difficulty, kinds::FarmClimate::Temperate, 0);
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
            living(&mut d, rot.y_rot, p.survival.health, p.survival.death_time);
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
            d.insert("goals".into(), running(&m.goals.slots));
            d.insert("targets".into(), running(&m.targets.slots));
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
fn running(slots: &[Slot]) -> Value {
    let mut names: Vec<&str> = slots
        .iter()
        .filter(|s| s.running)
        .map(|s| goal_name(&s.goal))
        .collect();
    names.sort_unstable();
    json!(names)
}

/// The Vanilla class of a goal.
fn goal_name(goal: &Goal) -> &'static str {
    match goal {
        Goal::Float => "FloatGoal",
        Goal::Panic { .. } => "PanicGoal",
        Goal::Stroll { .. } => "WaterAvoidingRandomStrollGoal",
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
    }
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
        if let Some(rule) = Rule::by_name(name) {
            let v = match value.as_str() {
                "true" => 1,
                "false" => 0,
                v => v.parse().map_err(|_| bad())?,
            };
            game_rules.set(rule, v);
        }
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
            "spawn_mobs" | "advance_time" | "advance_weather" | "mob_griefing" | "mob_drops" => {}
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
    fn is_loaded(&self, pos: BlockPos) -> bool {
        self.level.is_loaded(pos)
    }

    fn set_forced(&mut self, chunks: &[ChunkPos], forced: bool) -> Result<(), String> {
        for &pos in chunks {
            let runtime = &mut self.level.chunks;
            if forced {
                self.forced.entry(pos).or_insert_with(|| {
                    (
                        runtime.add_ticket(pos, FORCED_LEVEL),
                        runtime.add_simulation_ticket(pos, FORCED_LEVEL),
                    )
                });
            } else if let Some((ticket, simulation)) = self.forced.remove(&pos) {
                runtime.remove_ticket(ticket);
                runtime.remove_simulation_ticket(simulation);
            }
        }
        for _ in 0..SETTLE_LIMIT {
            // The chunk system only: no game tick.
            self.level.tick_chunk_runtime();
            if self.settled() {
                return Ok(());
            }
        }
        Err("the chunk system does not settle".to_owned())
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
            let v = core.velocity[row];
            out.push(EntityObs {
                kind: core.kind[row].name().to_owned(),
                pos: [pos.x, pos.y, pos.z],
                motion: [v.x, v.y, v.z],
                detail: self.detail(row),
            });
        }
    }

    /// Players, the level random reseeded with the world seed, then the other entities.
    fn add_entities(&mut self, entities: &[EntitySpec], _entity_seed: i64) -> Result<(), String> {
        for (i, spec) in entities.iter().enumerate() {
            if spec.is_player() {
                self.add_player(spec, i);
            }
        }
        *self.level.random() = LegacyRandom::new(self.seed);
        for (i, spec) in entities.iter().enumerate() {
            if !spec.is_player() {
                self.add_mob(spec, i)?;
            }
        }
        Ok(())
    }

    fn level_rng(&mut self) -> Option<i64> {
        (!self.names.is_empty()).then(|| self.level.random().state())
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
