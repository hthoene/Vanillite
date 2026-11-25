//! Mobs in a world: the entity tick of mobs, arrows and experience orbs in Vanilla's tick
//! order, the attacks of players, natural spawning and the animals of new chunks.

use std::sync::Arc;

use vanillite_base::{Aabb, BlockPos, ChunkPos, Vec3};
use vanillite_config::{GameMode, WorldRules};
use vanillite_data::entity::ids;
use vanillite_game::entities::is_mob_kind;
use vanillite_game::level::Level;
use vanillite_game::level::explosion::BlockInteraction;
use vanillite_game::mob::spawn::{self, SpawnInput, SpawnTable, Spawner};
use vanillite_game::mob::{
    self, Environment, MobContext, MobSettings, WorldAction, combat, projectile,
};
use vanillite_game::survival::blocks::BlockAccess;
use vanillite_game::survival::damage::DamageSource;
use vanillite_game::survival::rules::{Difficulty, Rule};
use vanillite_world::entity::EntityId;

use crate::global::Global;
use crate::survival::{WorldBlocks, WorldSurvival, raining_at};

/// Block access without blocks, for effects applied after a tick.
struct NoBlocks;

impl BlockAccess for NoBlocks {
    fn block_state(&self, _: BlockPos) -> Option<vanillite_data::block::BlockState> {
        None
    }
}

/// The mob state of one world.
#[derive(Debug)]
pub(crate) struct WorldMobs {
    cx: MobContext,
    spawner: Spawner,
    table: Option<Arc<SpawnTable>>,
    seed: i64,
    settings: MobSettings,
    /// Generated chunks waiting to become live, with the ticks they waited.
    generated: Vec<(ChunkPos, u32)>,
    scratch: Vec<ChunkPos>,
    ticks: u64,
}

impl WorldMobs {
    pub(crate) fn new(table: Option<Arc<SpawnTable>>, seed: i64, rules: &WorldRules) -> WorldMobs {
        WorldMobs {
            cx: MobContext::default(),
            spawner: Spawner::default(),
            table,
            seed,
            settings: settings(rules),
            generated: Vec::new(),
            scratch: Vec::new(),
            ticks: 0,
        }
    }

    /// Takes over changed rules (config reload).
    pub(crate) fn set_rules(&mut self, rules: &WorldRules) {
        self.settings = settings(rules);
    }

    /// The attacks of the players since the last tick (`handleAttack`).
    pub(crate) fn attacks(
        &mut self,
        level: &mut Level,
        survival: &mut WorldSurvival,
        global: &Global,
        dimension: &str,
    ) {
        let mut pending = Vec::new();
        for (id, p) in level.entities.players.iter_mut() {
            for target in p.connection.attacks.drain(..) {
                pending.push((id, target));
            }
        }
        if pending.is_empty() {
            return;
        }
        let state = &global.level;
        let hooks = crate::plugins::Hooks::of(&global.plugins, dimension);
        let rules = state.rules().clone();
        let mut ents = std::mem::take(&mut level.entities);
        for (player, network_id) in pending {
            let Some(target) = ents.by_network_id(network_id) else {
                continue;
            };
            let (Some(pe), Some(te)) = (ents.store.get(player), ents.store.get(target)) else {
                continue;
            };
            let Some(pd) = ents.player(player) else {
                continue;
            };
            if pd.game_mode == GameMode::Spectator || pd.survival.is_dead_or_dying() {
                continue;
            }
            // `isWithinAttackRange(item, box, 3.0)`: the interaction range of 3 plus 3.
            let eye = Vec3::new(pe.pos().x, pe.pos().y + 1.62, pe.pos().z);
            if distance_sqr_to_box(&te.aabb(), eye) >= 36.0 {
                continue;
            }
            let blocks = WorldBlocks(&level.chunks);
            let (weather, climate) = (survival.weather, survival.climate().cloned());
            let chunks = &level.chunks;
            let raining = move |pos| raining_at(chunks, climate.as_deref(), &weather, pos);
            let mut no_gateway = |_| false;
            let names = |name: &str, uuid| crate::scoreboard::display_name(global, name, uuid);
            let mut env = survival.env(
                state,
                &rules,
                &blocks,
                &raining,
                dimension,
                &mut no_gateway,
                &hooks,
                &names,
            );
            combat::player_attack(level, &mut ents, &mut env, &mut self.cx, player, target);
        }
        level.entities = ents;
        self.finish_removals(level);
        survival.deliver(&mut level.entities, global);
    }

    /// Natural spawning (`NaturalSpawner` via `ServerChunkCache.tickChunks`) and the animals of
    /// chunks generated since the last tick.
    pub(crate) fn spawn(
        &mut self,
        level: &mut Level,
        survival: &WorldSurvival,
        global: &Global,
        dimension: &str,
        spawn_point: Option<BlockPos>,
    ) {
        let Some(table) = self.table.clone() else {
            return;
        };
        let state = &global.level;
        let rules = state.rules().clone();
        let difficulty = state.difficulty();
        let env = environment(global, dimension, survival);
        // New chunks: their animals once they are live.
        level.chunks.take_generated(&mut self.scratch);
        self.generated
            .extend(self.scratch.drain(..).map(|c| (c, 0)));
        if !self.generated.is_empty() {
            let mut waiting = Vec::new();
            for (chunk, waited) in std::mem::take(&mut self.generated) {
                if level.chunks.chunk(chunk).is_none() {
                    // Generated chunks become live within a few ticks unless they unload.
                    if waited < 200 {
                        waiting.push((chunk, waited + 1));
                    }
                    continue;
                }
                if rules.bool(Rule::SPAWN_MOBS) {
                    spawn::spawn_for_chunk_generation(
                        level, &table, &env, chunk, self.seed, difficulty,
                    );
                }
            }
            self.generated = waiting;
        }
        if !rules.bool(Rule::SPAWN_MOBS) {
            return;
        }
        let players: Vec<(EntityId, Vec3)> = level
            .entities
            .players
            .iter()
            .filter(|(_, p)| p.game_mode != GameMode::Spectator)
            .filter_map(|(id, _)| level.entities.store.get(id).map(|e| (id, e.pos())))
            .collect();
        let input = SpawnInput {
            players: &players,
            spawn_point,
            spawn_enemies: difficulty != Difficulty::Peaceful && rules.bool(Rule::SPAWN_MONSTERS),
            difficulty,
            env,
        };
        let persistent = level.game_time() % 400 == 0;
        spawn::tick(level, &table, &mut self.spawner, &input, persistent);
    }

    /// `entityTickList.forEach(tickNonPassenger)`: falling blocks, mobs, arrows and orbs in
    /// tick order. Mobs, arrows and orbs only tick in chunks that tick.
    pub(crate) fn tick_entities(
        &mut self,
        level: &mut Level,
        survival: &mut WorldSurvival,
        global: &Global,
        dimension: &str,
    ) {
        self.ticks += 1;
        let state = &global.level;
        let hooks = crate::plugins::Hooks::of(&global.plugins, dimension);
        let rules = state.rules().clone();
        let has_players = !level.entities.players.is_empty();
        let env = environment(global, dimension, survival);
        self.cx.begin_tick(env, self.settings, has_players);
        let activation = f64::from(self.settings.activation_range);
        let mut cursor = level.entities.store.tick_cursor();
        let mut ents = std::mem::take(&mut level.entities);
        while let Some(id) = ents.store.next_to_tick(&mut cursor) {
            let Some(mut entity) = ents.store.get_mut(id) else {
                continue;
            };
            entity.set_old_pos();
            let kind = entity.as_ref().kind();
            let pos = entity.as_ref().pos();
            if kind == ids::FALLING_BLOCK || kind == ids::TNT {
                level.entities = ents;
                vanillite_game::entities::tick_entity(level, id, kind);
                self.apply_explosion_hurts(level, survival, global, dimension);
                ents = std::mem::take(&mut level.entities);
                continue;
            }
            let is_mob = is_mob_kind(kind);
            if !is_mob && kind != ids::ARROW && kind != ids::EXPERIENCE_ORB {
                continue;
            }
            let chunk = BlockPos::containing(pos.x, pos.y, pos.z).chunk();
            if !level.is_block_ticking(chunk) {
                continue;
            }
            if is_mob && activation > 0.0 && !self.ticks.is_multiple_of(20) {
                let area = Aabb::new(
                    pos.x - activation,
                    pos.y - activation,
                    pos.z - activation,
                    pos.x + activation,
                    pos.y + activation,
                    pos.z + activation,
                );
                let mut near = false;
                ents.store.for_each_in(&area, true, |_| near = true);
                if !near {
                    continue;
                }
            }
            {
                let blocks = WorldBlocks(&level.chunks);
                let (weather, climate) = (survival.weather, survival.climate().cloned());
                let chunks = &level.chunks;
                let raining = move |pos| raining_at(chunks, climate.as_deref(), &weather, pos);
                let mut no_gateway = |_| false;
                let names = |name: &str, uuid| crate::scoreboard::display_name(global, name, uuid);
                let mut env = survival.env(
                    state,
                    &rules,
                    &blocks,
                    &raining,
                    dimension,
                    &mut no_gateway,
                    &hooks,
                    &names,
                );
                let level_ref: &Level = level;
                if is_mob {
                    mob::tick(level_ref, &mut ents, &mut env, &mut self.cx, id);
                } else if kind == ids::ARROW {
                    projectile::tick_arrow(level_ref, &mut ents, &mut env, &mut self.cx, id);
                } else {
                    projectile::tick_orb(level_ref, &mut ents, &mut env, &mut self.cx, id);
                }
            }
            if !self.cx.actions.is_empty() {
                level.entities = ents;
                self.apply_actions(level, survival, global, dimension);
                ents = std::mem::take(&mut level.entities);
            }
            if ents.mob(id).is_some_and(|m| m.removed) {
                ents.despawn(id);
            }
            for d in std::mem::take(&mut self.cx.discarded) {
                ents.despawn(d);
            }
        }
        level.entities = ents;
        survival.deliver(&mut level.entities, global);
    }

    /// Applies what the last mob tick queued: explosions and block changes.
    fn apply_actions(
        &mut self,
        level: &mut Level,
        survival: &mut WorldSurvival,
        global: &Global,
        dimension: &str,
    ) {
        let state = &global.level;
        let rules = state.rules().clone();
        for action in std::mem::take(&mut self.cx.actions) {
            match action {
                WorldAction::Explode(e) => {
                    // `Level.ExplosionInteraction.MOB`: `getDestroyType` with `mobGriefing`.
                    let decay = if e.mob {
                        Rule::MOB_EXPLOSION_DROP_DECAY
                    } else {
                        Rule::TNT_EXPLOSION_DROP_DECAY
                    };
                    let interaction = if e.mob && !rules.bool(Rule::MOB_GRIEFING) {
                        BlockInteraction::Keep
                    } else if rules.bool(decay) {
                        BlockInteraction::DestroyWithDecay
                    } else {
                        BlockInteraction::Destroy
                    };
                    // `getIndirectSourceEntity`: a living source caused it itself.
                    level.explode(e.source, e.source, e.pos, e.radius, interaction);
                    self.apply_explosion_hurts(level, survival, global, dimension);
                }
                WorldAction::SetBlock(pos, block, flags) => {
                    level.set_block(pos, block, flags);
                }
                WorldAction::DestroyBlock(pos, drop) => {
                    level.destroy_block(pos, drop, None);
                }
                WorldAction::LevelEvent(event, pos, data) => {
                    level.level_event(None, event, pos, data);
                }
            }
        }
        self.finish_removals(level);
    }

    /// `entity.hurtServer(explosion damage)` of the living entities the last explosions hit.
    fn apply_explosion_hurts(
        &mut self,
        level: &mut Level,
        survival: &mut WorldSurvival,
        global: &Global,
        dimension: &str,
    ) {
        if level.output().explosion_hurts.is_empty() {
            return;
        }
        let state = &global.level;
        let hooks = crate::plugins::Hooks::of(&global.plugins, dimension);
        let rules = state.rules().clone();
        let hurts = std::mem::take(&mut level.output().explosion_hurts);
        let no_blocks = NoBlocks;
        let raining = |_| false;
        let mut no_gateway = |_| false;
        let names = |name: &str, uuid| crate::scoreboard::display_name(global, name, uuid);
        let mut env = survival.env(
            state,
            &rules,
            &no_blocks,
            &raining,
            dimension,
            &mut no_gateway,
            &hooks,
            &names,
        );
        let mut ents = std::mem::take(&mut level.entities);
        for hurt in hurts {
            // `damageSources().explosion(source, indirect)`.
            let mut source = match (hurt.source, hurt.indirect) {
                (Some(direct), Some(cause)) => {
                    combat::entity_source(&ents, env.types.player_explosion, cause, direct)
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
            combat::hurt(
                level,
                &mut ents,
                &mut env,
                &mut self.cx,
                hurt.target,
                source,
                hurt.amount,
            );
        }
        level.entities = ents;
        self.finish_removals(level);
    }

    /// Despawns the entities removed outside their own tick.
    fn finish_removals(&mut self, level: &mut Level) {
        for d in std::mem::take(&mut self.cx.discarded) {
            level.entities.despawn(d);
        }
    }
}

fn settings(rules: &WorldRules) -> MobSettings {
    MobSettings {
        path_nodes_per_tick: rules.mobs.path_nodes_per_tick,
        activation_range: u32::from(rules.mobs.activation_range),
    }
}

fn distance_sqr_to_box(b: &Aabb, p: Vec3) -> f64 {
    let dx = (b.min_x - p.x).max(0.0).max(p.x - b.max_x);
    let dy = (b.min_y - p.y).max(0.0).max(p.y - b.max_y);
    let dz = (b.min_z - p.z).max(0.0).max(p.z - b.max_z);
    dx * dx + dy * dy + dz * dz
}

/// The overworld's sky light factor at `ticks` of the day (the `day` timeline's
/// `gameplay/sky_light_level` track).
fn day_sky_factor(ticks: i64) -> f32 {
    const KEYS: [(i64, f32); 4] = [
        (133, 1.0),
        (11867, 1.0),
        (13670, 0.266_666_68),
        (22330, 0.266_666_68),
    ];
    let t = ticks.rem_euclid(24000);
    for w in KEYS.windows(2) {
        let (a, b) = (w[0], w[1]);
        if t >= a.0 && t < b.0 {
            #[allow(clippy::cast_precision_loss)]
            let f = (t - a.0) as f32 / (b.0 - a.0) as f32;
            return a.1 + (b.1 - a.1) * f;
        }
    }
    // Wraps from the last key to the first of the next day.
    let (last, first) = (KEYS[3], KEYS[0]);
    let span = first.0 + 24000 - last.0;
    let since = if t >= last.0 {
        t - last.0
    } else {
        t + 24000 - last.0
    };
    #[allow(clippy::cast_precision_loss)]
    let f = since as f32 / span as f32;
    last.1 + (first.1 - last.1) * f
}

/// This tick's environment: sky darkness from the day time and the weather, whether monsters
/// burn, and the dimension's monster light rules.
fn environment(global: &Global, dimension: &str, survival: &WorldSurvival) -> Environment {
    let overworld = dimension == "minecraft:overworld";
    let nether = dimension == "minecraft:the_nether";
    if !overworld {
        return Environment {
            sky_darken: if nether { 0 } else { 11 },
            monsters_burn: false,
            thundering: false,
            raining: false,
            has_sky_light: false,
            monster_block_light_limit: if nether { 15 } else { 0 },
            monster_light_test: if nether { (7, 7) } else { (0, 7) },
            ambient_light: if nether { 0.1 } else { 0.0 },
        };
    }
    let day = {
        let clocks = global.level.clocks();
        clocks.timeline("minecraft:day").and_then(|t| {
            clocks
                .clocks
                .get(t.clock)
                .map(|c| t.current_ticks(c.total_ticks))
        })
    }
    .unwrap_or(6000);
    let w = survival.weather;
    let mut sky = 15.0 * day_sky_factor(day);
    let rain = w.rain();
    let thunder = w.thunder();
    sky += (4.0 - sky) * 0.3125 * rain;
    sky += (4.0 - sky) * 0.527_343_75 * thunder;
    #[allow(clippy::cast_possible_truncation)]
    let sky_darken = (15.0 - sky) as i32;
    let burn = !(12542..23460).contains(&day);
    Environment {
        sky_darken,
        monsters_burn: burn,
        thundering: thunder > 0.9,
        raining: rain > 0.2,
        has_sky_light: true,
        monster_block_light_limit: 0,
        monster_light_test: (0, 7),
        ambient_light: 0.0,
    }
}
