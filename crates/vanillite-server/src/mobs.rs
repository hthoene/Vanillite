//! Mobs in a world: the entity tick of mobs, arrows and experience orbs in Vanilla's tick
//! order, the attacks of players, natural spawning and the animals of new chunks.

use std::sync::Arc;

use vanillite_base::{Aabb, BlockPos, ChunkPos, Vec3};
use vanillite_config::{GameMode, WorldRules};
use vanillite_data::entity::ids;
use vanillite_game::entities::{Entities, is_mob_kind};
use vanillite_game::item::ItemStack;
use vanillite_game::level::Level;
use vanillite_game::level::explosion::BlockInteraction;
use vanillite_game::mob::spawn::{self, SpawnInput, SpawnTable, Spawner};
use vanillite_game::mob::{
    self, Environment, MobContext, MobSettings, WorldAction, animal, attack, combat, projectile,
    tame,
};
use vanillite_game::player::{EntityAction, VehicleInput};
use vanillite_game::riding;
use vanillite_game::survival::blocks::BlockAccess;
use vanillite_game::survival::damage::DamageSource;
use vanillite_game::survival::rules::{Difficulty, Rule};
use vanillite_game::vehicle::{self, BoatKind};
use vanillite_protocol::packets::play::serverbound::Hand;
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
    /// The custom spawners of the overworld (`ServerLevel.customSpawners`).
    phantoms: vanillite_game::mob::fam::undead::phantom::PhantomSpawner,
    patrols: vanillite_game::mob::fam::illager::patrol::PatrolSpawner,
    cats: spawn::CatSpawner,
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
            phantoms: Default::default(),
            patrols: Default::default(),
            cats: spawn::CatSpawner::default(),
        }
    }

    /// Takes over changed rules (config reload).
    pub(crate) fn set_rules(&mut self, rules: &WorldRules) {
        self.settings = settings(rules);
    }

    /// The spawn settings of the biomes.
    pub(crate) fn table(&self) -> Option<&SpawnTable> {
        self.table.as_deref()
    }

    /// The attacks, interactions, entity item uses and vehicle input of the players since the
    /// last tick (`handleAttack`, `handleInteract`, `handleUseItem`, `handleMoveVehicle`,
    /// `handlePaddleBoat`, sneaking off a vehicle).
    pub(crate) fn player_actions(
        &mut self,
        level: &mut Level,
        survival: &mut WorldSurvival,
        global: &Global,
        dimension: &str,
    ) {
        let mut pending: Vec<(EntityId, EntityAction)> = Vec::new();
        let mut inputs: Vec<(EntityId, VehicleInput)> = Vec::new();
        let riding = !level.entities.riding.is_empty();
        for (id, p) in level.entities.players.iter_mut() {
            for action in p.connection.entity_actions.drain(..) {
                pending.push((id, action));
            }
            let v = &mut p.connection.vehicle;
            if riding || v.moved.is_some() {
                inputs.push((id, *v));
                v.moved = None;
                v.jump = None;
            }
        }
        if pending.is_empty() && inputs.is_empty() {
            return;
        }
        let state = &global.level;
        let hooks = crate::plugins::Hooks::of(&global.plugins, dimension);
        let rules = state.rules().clone();
        let difficulty = state.difficulty;
        let network_ids = std::sync::Arc::clone(level.network_ids_arc());
        let mut ents = std::mem::take(&mut level.entities);
        {
            let blocks = WorldBlocks(&level.chunks);
            let (weather, climate) = (survival.weather, survival.climate().cloned());
            let chunks = &level.chunks;
            let raining = move |pos| raining_at(chunks, climate.as_deref(), &weather, pos);
            let mut env = survival.env(state, &rules, &blocks, &raining, dimension, &hooks);
            let level: &Level = level;
            for (player, action) in pending {
                let usable = ents.player(player).is_some_and(|p| {
                    p.game_mode != GameMode::Spectator && !p.survival.is_dead_or_dying()
                });
                match action {
                    EntityAction::Attack(network_id) => {
                        if !usable {
                            continue;
                        }
                        let Some(target) = in_reach(&ents, player, network_id) else {
                            continue;
                        };
                        attack_entity(level, &mut ents, &mut env, &mut self.cx, player, target);
                    }
                    EntityAction::Interact {
                        entity,
                        hand,
                        sneaking,
                        ..
                    } => {
                        let Some(target) = in_reach(&ents, player, entity) else {
                            continue;
                        };
                        let r = if ents
                            .store
                            .get(target)
                            .is_some_and(|e| e.kind() == ids::LEASH_KNOT)
                        {
                            knot_interact(&mut ents, &mut env, &mut self.cx, player, target)
                        } else {
                            animal::interact_on(
                                level,
                                &mut ents,
                                &mut env,
                                &mut self.cx,
                                player,
                                target,
                                hand,
                                sneaking,
                            )
                        };
                        if r == animal::Interaction::Success
                            && let Some(p) = ents.player_mut(player)
                        {
                            // `swingAndResetAttackStrength`.
                            p.survival.attack_ticker = 0;
                        }
                    }
                    EntityAction::UseItem(hand) => {
                        if usable {
                            use_item(level, &mut ents, &mut env, &network_ids, player, hand);
                        }
                    }
                    EntityAction::UseItemOn { hand, pos, face } => {
                        if usable {
                            use_item_on(
                                level,
                                &mut ents,
                                &mut env,
                                &network_ids,
                                player,
                                hand,
                                pos,
                                face,
                                difficulty,
                            );
                        }
                    }
                    EntityAction::ReleaseUseItem => {
                        vanillite_game::player::using::release(
                            &mut ents,
                            &mut env,
                            &network_ids,
                            player,
                        );
                    }
                }
            }
            for (player, input) in inputs {
                if let Some((pos, y_rot, x_rot, on_ground)) = input.moved {
                    riding::move_vehicle(
                        level,
                        &mut ents,
                        &mut self.cx,
                        player,
                        pos,
                        y_rot,
                        x_rot,
                        on_ground,
                    );
                }
                riding::paddle_boat(&mut ents, player, input.paddles.0, input.paddles.1);
                // `wantsToStopRiding`: sneaking gets a player off its vehicle.
                if input.input.shift && ents.vehicle_of(player).is_some() {
                    riding::stop_riding(&mut ents, &mut env, player);
                }
            }
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
        let difficulty = state.difficulty;
        let env = environment(global, dimension, survival, self.seed);
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
        if dimension == "minecraft:overworld" {
            self.custom_spawners(
                level,
                &table,
                &rules,
                input.spawn_enemies,
                difficulty,
                env,
                &players,
            );
        }
    }

    /// `tickCustomSpawners`: phantoms, patrols and cats (the wandering trader is #60).
    #[allow(clippy::too_many_arguments)]
    fn custom_spawners(
        &mut self,
        level: &mut Level,
        table: &SpawnTable,
        rules: &vanillite_game::survival::rules::GameRules,
        spawn_enemies: bool,
        difficulty: Difficulty,
        env: Environment,
        players: &[(EntityId, Vec3)],
    ) {
        use vanillite_game::mob::fam::illager::patrol::{self, PatrolInput};
        use vanillite_game::mob::fam::undead::phantom;
        let rest = |id: EntityId| {
            level
                .entities
                .player(id)
                .map_or(0, |p| p.survival.time_since_rest)
        };
        let rests: Vec<(EntityId, i32)> = players.iter().map(|&(id, _)| (id, rest(id))).collect();
        let time_since_rest = |id: EntityId| rests.iter().find(|r| r.0 == id).map_or(0, |r| r.1);
        phantom::phantom_spawner_tick(
            &mut self.phantoms,
            level,
            &env,
            difficulty,
            rules.bool(Rule::SPAWN_PHANTOMS),
            spawn_enemies,
            &time_since_rest,
        );
        let patrol_players: Vec<(Vec3, bool)> = level
            .entities
            .players
            .iter()
            .filter_map(|(id, p)| {
                let e = level.entities.store.get(id)?;
                Some((e.pos(), p.game_mode == GameMode::Spectator))
            })
            .collect();
        let no_village = |_| false;
        let can_patrol = |level: &Level, pos: BlockPos| {
            table.biome(level.biome(pos)).is_none_or(|b| b.patrol_spawn)
        };
        let input = PatrolInput {
            enabled: spawn_enemies && rules.bool(Rule::SPAWN_PATROLS),
            difficulty,
            env,
            players: &patrol_players,
            table: Some(table),
            close_to_village: &no_village,
            can_patrol_spawn: &can_patrol,
        };
        patrol::patrol_spawner_tick(level, &mut self.patrols, &input);
        let ids: Vec<EntityId> = players.iter().map(|p| p.0).collect();
        spawn::cat_spawner_tick(
            level,
            Some(table),
            &mut self.cats,
            &mut self.spawner.structures,
            &env,
            difficulty,
            &ids,
        );
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
        let env = environment(global, dimension, survival, self.seed);
        self.cx.begin_tick(env, self.settings, has_players);
        self.lightning_hits(level, survival, global, dimension);
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
            let is_boat = BoatKind::of(kind).is_some();
            let is_missile = vanillite_game::mob::missile::is_missile_kind(kind);
            if !is_mob
                && !is_boat
                && !is_missile
                && kind != ids::ARROW
                && kind != ids::EXPERIENCE_ORB
            {
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
                let mut env = survival.env(state, &rules, &blocks, &raining, dimension, &hooks);
                let level_ref: &Level = level;
                if is_mob {
                    mob::tick(level_ref, &mut ents, &mut env, &mut self.cx, id);
                } else if is_boat {
                    vehicle::tick_boat(level_ref, &mut ents, &mut env, &mut self.cx, id);
                } else if kind == ids::ARROW {
                    projectile::tick_arrow(level_ref, &mut ents, &mut env, &mut self.cx, id);
                } else if is_missile {
                    vanillite_game::mob::missile::tick(
                        level_ref,
                        &mut ents,
                        &mut env,
                        &mut self.cx,
                        id,
                    );
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
        // `rideTick`: passengers follow their vehicles; leads of knots and loaded mobs.
        riding::position_all_riders(&mut ents);
        {
            let blocks = WorldBlocks(&level.chunks);
            let raining = |_| false;
            let mut env = survival.env(state, &rules, &blocks, &raining, dimension, &hooks);
            let mut discarded = Vec::new();
            vanillite_game::player::using::tick_all(&mut ents, &mut env);
            tame::tick_leashes(level, &mut ents, &mut env, &mut discarded);
            for d in discarded {
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
                WorldAction::SetDoorOpen(pos, open, source) => {
                    vanillite_game::block::redstone::set_door_open(level, pos, open, source);
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
        let mut env = survival.env(state, &rules, &no_blocks, &raining, dimension, &hooks);
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
            if ents.boat(hurt.target).is_some() {
                // `VehicleEntity.ignoreExplosion`: mob explosions without mob griefing.
                let mob_source = hurt.indirect.is_some_and(|c| ents.mob(c).is_some());
                if !(mob_source && !rules.bool(Rule::MOB_GRIEFING)) {
                    vehicle::hurt_boat(
                        &mut ents,
                        &mut env,
                        &mut self.cx,
                        hurt.target,
                        source,
                        hurt.amount,
                    );
                }
                continue;
            }
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

    /// `thunderHit` of the non-player entities lightning struck: mobs burn and take 5 damage,
    /// creepers become charged, items and boats are hurt.
    fn lightning_hits(
        &mut self,
        level: &mut Level,
        survival: &mut WorldSurvival,
        global: &Global,
        dimension: &str,
    ) {
        if survival.struck.is_empty() {
            return;
        }
        let struck = std::mem::take(&mut survival.struck);
        let state = &global.level;
        let hooks = crate::plugins::Hooks::of(&global.plugins, dimension);
        let rules = state.rules().clone();
        let mut ents = std::mem::take(&mut level.entities);
        {
            let no_blocks = NoBlocks;
            let raining = |_| false;
            let mut env = survival.env(state, &rules, &no_blocks, &raining, dimension, &hooks);
            let source = DamageSource::of(env.types.lightning_bolt);
            for id in struck {
                if let Some(d) = ents.item_mut(id) {
                    d.health -= 5;
                    if d.health <= 0 {
                        self.cx.discarded.push(id);
                    }
                    continue;
                }
                if ents.boat(id).is_some() {
                    vehicle::hurt_boat(&mut ents, &mut env, &mut self.cx, id, source, 5.0);
                    continue;
                }
                let Some(kind) = ents.mob(id).filter(|m| m.is_alive()).map(|m| m.kind) else {
                    continue;
                };
                if kind.family() != mob::Family::Classic {
                    let mut h = vanillite_game::mob::fam::Hurt {
                        level,
                        ents: &mut ents,
                        env: &mut env,
                        cx: &mut self.cx,
                        target: id,
                        source,
                    };
                    if vanillite_game::mob::fam::thunder_hit(&mut h, kind) {
                        continue;
                    }
                }
                let Some(m) = ents.mob_mut(id) else {
                    continue;
                };
                m.fire_ticks += 1;
                if m.fire_ticks == 0 {
                    m.fire_ticks = 160;
                }
                let creeper = m.kind == mob::Kind::Creeper;
                if creeper {
                    m.powered = true;
                }
                if let Some(mut e) = ents.store.get_mut(id) {
                    e.mark_data_dirty();
                }
                combat::hurt(level, &mut ents, &mut env, &mut self.cx, id, source, 5.0);
            }
        }
        level.entities = ents;
        self.finish_removals(level);
        survival.deliver(&mut level.entities, global);
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
fn environment(
    global: &Global,
    dimension: &str,
    survival: &WorldSurvival,
    seed: i64,
) -> Environment {
    let overworld = dimension == "minecraft:overworld";
    let nether = dimension == "minecraft:the_nether";
    if !overworld {
        return Environment {
            surface_slime_chance: 0.0,
            world_seed: seed,
            sea_level: if nether { 32 } else { 0 },
            sky_darken: if nether { 0 } else { 11 },
            monsters_burn: false,
            thundering: false,
            raining: false,
            has_sky_light: false,
            monster_block_light_limit: if nether { 15 } else { 0 },
            monster_light_test: if nether { (7, 7) } else { (0, 7) },
            ambient_light: if nether { 0.1 } else { 0.0 },
            has_ceiling: nether,
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
    // The `minecraft:moon` timeline: the surface slime chance is half the moon's brightness.
    let moon = {
        let clocks = global.level.clocks();
        clocks.timeline("minecraft:moon").and_then(|t| {
            clocks
                .clocks
                .get(t.clock)
                .map(|c| t.current_ticks(c.total_ticks))
        })
    }
    .unwrap_or(0);
    const MOON_BRIGHTNESS: [f32; 8] = [1.0, 0.75, 0.5, 0.25, 0.0, 0.25, 0.5, 0.75];
    let phase = usize::try_from((moon / 24000).rem_euclid(8)).unwrap_or(0);
    Environment {
        surface_slime_chance: MOON_BRIGHTNESS[phase] * 0.5,
        world_seed: seed,
        sea_level: 63,
        sky_darken,
        monsters_burn: burn,
        thundering: thunder > 0.9,
        raining: rain > 0.2,
        has_sky_light: true,
        monster_block_light_limit: 0,
        monster_light_test: (0, 7),
        ambient_light: 0.0,
        has_ceiling: false,
    }
}

/// The entity with `network_id` if `player` can reach it (`isWithinEntityInteractionRange`:
/// 3 blocks plus 3 from the eyes to its box).
fn in_reach(ents: &Entities, player: EntityId, network_id: i32) -> Option<EntityId> {
    let target = ents.by_network_id(network_id)?;
    if target == player {
        return None;
    }
    let pe = ents.store.get(player)?;
    let te = ents.store.get(target)?;
    let eye = Vec3::new(
        pe.pos().x,
        pe.pos().y + f64::from(pe.kind().info().eye_height) * f64::from(pe.dims().height / 1.8),
        pe.pos().z,
    );
    (distance_sqr_to_box(&te.aabb(), eye) < 36.0).then_some(target)
}

/// `Player.attack` of mobs and players, boats and knots.
fn attack_entity(
    level: &Level,
    ents: &mut Entities,
    env: &mut vanillite_game::survival::player::Env<'_>,
    cx: &mut MobContext,
    player: EntityId,
    target: EntityId,
) {
    let kind = ents.store.get(target).map(|e| e.kind());
    if kind == Some(ids::LEASH_KNOT) {
        // `LeashFenceKnotEntity.hurtServer`: it breaks.
        tame::break_knot(ents, env, target, &mut cx.discarded);
        return;
    }
    if ents.boat(target).is_some() {
        let scale = attack::attack_strength_scale(ents, player, 0.5);
        let base =
            attack::player_attribute(ents, player, "minecraft:attack_damage", 1.0, 0.0, 2048.0);
        #[allow(clippy::cast_possible_truncation)]
        let damage = base as f32 * (0.2 + scale * scale * 0.8);
        if let Some(p) = ents.player_mut(player) {
            p.survival.attack_ticker = 0;
        }
        let mut source = combat::entity_source(ents, env.types.player_attack, player, player);
        source.cause_is_mob = false;
        vehicle::hurt_boat(ents, env, cx, target, source, damage);
        return;
    }
    // `canHarmPlayer`: the pvp rule.
    if ents.player(target).is_some() && !env.rules.bool(Rule::PVP) {
        return;
    }
    attack::player_attack(level, ents, env, cx, player, target);
}

/// `LeashFenceKnotEntity.interact`: ties the player's leashed mobs to the knot, or breaks it.
fn knot_interact(
    ents: &mut Entities,
    env: &mut vanillite_game::survival::player::Env<'_>,
    cx: &mut MobContext,
    player: EntityId,
    knot: EntityId,
) -> animal::Interaction {
    let Some(pos) = ents.store.get(knot).map(|e| e.pos()) else {
        return animal::Interaction::Pass;
    };
    let area = Aabb::new(
        pos.x - 7.0,
        pos.y - 7.0,
        pos.z - 7.0,
        pos.x + 7.0,
        pos.y + 7.0,
        pos.z + 7.0,
    );
    let mut mine = Vec::new();
    ents.store.for_each_in(&area, false, |id| {
        if ents.leashes.get(&id) == Some(&player) {
            mine.push(id);
        }
    });
    if mine.is_empty() {
        tame::break_knot(ents, env, knot, &mut cx.discarded);
    } else {
        for m in mine {
            tame::set_leashed_to(ents, env, m, knot);
        }
    }
    animal::Interaction::Success
}

/// The inventory index of a hand.
fn hand_index(ents: &Entities, player: EntityId, hand: Hand) -> usize {
    match hand {
        Hand::MainHand => ents.player(player).map_or(0, |p| p.inventory.selected),
        Hand::OffHand => vanillite_game::inventory::OFFHAND,
    }
}

/// Takes the stack out of a hand to use it with the entities; [`put_back`] returns it.
fn take_held(ents: &mut Entities, player: EntityId, hand: Hand) -> ItemStack {
    let i = hand_index(ents, player, hand);
    ents.player_mut(player)
        .and_then(|p| p.inventory.get_mut(i).map(std::mem::take))
        .unwrap_or_default()
}

fn put_back(ents: &mut Entities, player: EntityId, hand: Hand, stack: ItemStack) {
    let i = hand_index(ents, player, hand);
    if let Some(p) = ents.player_mut(player) {
        p.inventory.set(i, stack);
    }
}

/// `use_item` of the items that need entities: boats, the carrot on a stick, and items used
/// over time.
fn use_item(
    level: &Level,
    ents: &mut Entities,
    env: &mut vanillite_game::survival::player::Env<'_>,
    network_ids: &vanillite_world::entity::NetworkIds,
    player: EntityId,
    hand: Hand,
) {
    let mut stack = take_held(ents, player, hand);
    let name = vanillite_game::item::item_id(&stack).name();
    let creative = ents
        .player(player)
        .is_some_and(|p| p.game_mode == GameMode::Creative);
    if name.ends_with("_boat") || name.ends_with("_raft") {
        let reach = if creative { 5.0 } else { 4.5 };
        vehicle::use_boat_item(
            level,
            ents,
            env,
            network_ids,
            player,
            &mut stack,
            creative,
            reach,
        );
    } else if name == "minecraft:carrot_on_a_stick" || name == "minecraft:warped_fungus_on_a_stick"
    {
        riding::boost(ents, env, player, &mut stack, creative);
    } else {
        put_back(ents, player, hand, stack);
        vanillite_game::player::using::start(ents, env, player, hand);
        return;
    }
    put_back(ents, player, hand, stack);
}

/// `use_item_on` with spawn eggs and leads.
#[allow(clippy::too_many_arguments)]
fn use_item_on(
    level: &Level,
    ents: &mut Entities,
    env: &mut vanillite_game::survival::player::Env<'_>,
    network_ids: &vanillite_world::entity::NetworkIds,
    player: EntityId,
    hand: Hand,
    pos: BlockPos,
    face: vanillite_base::Direction,
    difficulty: Difficulty,
) {
    let mut stack = take_held(ents, player, hand);
    let creative = ents
        .player(player)
        .is_some_and(|p| p.game_mode == GameMode::Creative);
    if let Some(kind) = animal::spawn_egg_kind(&stack) {
        animal::use_spawn_egg_on(
            level,
            ents,
            env,
            network_ids,
            kind,
            pos,
            face,
            &mut stack,
            creative,
            difficulty,
        );
    } else if vanillite_game::item::item_id(&stack).name() == "minecraft:lead" {
        tame::use_lead_on_fence(level, ents, env, player, pos);
    }
    put_back(ents, player, hand, stack);
}
