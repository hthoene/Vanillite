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
use vanillite_game::minecart::{self, MinecartKind};
use vanillite_game::mob::boss;
use vanillite_game::mob::boss::dragon::{self, fight::DragonFight};
use vanillite_game::mob::boss::special::{self, is_special_kind};
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

/// The dragon fight's saved data: the last state the fight wrote, taken by the next save.
#[derive(Debug, Default)]
struct FightData(std::sync::Mutex<Option<vanillite_nbt::Compound>>);

impl vanillite_storage::saved_data::SavedData for FightData {
    fn id(&self) -> &str {
        DragonFight::ID
    }

    fn save(&self) -> Option<vanillite_nbt::Compound> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

/// The mob state of one world.
#[derive(Debug)]
pub(crate) struct WorldMobs {
    /// The End's dragon fight.
    fight: Option<DragonFight>,
    fight_data: Option<Arc<FightData>>,
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
            fight: None,
            fight_data: None,
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

    /// Creates the End's dragon fight from its saved data (`EnderDragonFight.TYPE`).
    pub(crate) fn init_dragon_fight(
        &mut self,
        data: &vanillite_storage::saved_data::SavedDataStore,
        level: &mut Level,
    ) {
        let saved = match data.read(DragonFight::ID) {
            Ok(saved) => saved,
            Err(e) => {
                log::error!("Could not read the dragon fight: {e}");
                None
            }
        };
        let mut bar_random = vanillite_base::random::LegacyRandom::new(
            vanillite_base::random::RandomSource::next_long(level.random()),
        );
        let fight = DragonFight::new(saved.as_ref(), self.seed, &mut bar_random);
        let store = Arc::new(FightData::default());
        data.register(Arc::clone(&store) as _);
        self.fight = Some(fight);
        self.fight_data = Some(store);
    }

    /// `EnderDragonFight.tick` (before the entities), and the crystal placed since the last
    /// tick (`tryRespawn`).
    pub(crate) fn tick_dragon_fight(
        &mut self,
        level: &mut Level,
        survival: &mut WorldSurvival,
        global: &Global,
        dimension: &str,
    ) {
        let Some(fight) = &mut self.fight else {
            self.cx.dragon_fight = None;
            return;
        };
        let state = &global.level;
        let hooks = crate::plugins::Hooks::of(&global.plugins, dimension);
        let rules = state.rules().clone();
        let no_blocks = NoBlocks;
        let raining = |_| false;
        let mut no_gateway = |_| false;
        let mut env = survival.env(
            state,
            &rules,
            &no_blocks,
            &raining,
            dimension,
            &mut no_gateway,
            &hooks,
        );
        if std::mem::take(&mut level.output().try_dragon_respawn) {
            fight.try_respawn(level);
        }
        fight.tick(level, &mut env, &mut self.cx);
        self.cx.dragon_fight = Some(fight.view());
        self.store_fight();
        self.apply_explosion_hurts(level, survival, global, dimension);
        self.apply_actions(level, survival, global, dimension);
        survival.deliver(&mut level.entities, global);
    }

    /// Hands the fight's changed state to its saved data.
    fn store_fight(&mut self) {
        if let (Some(fight), Some(data)) = (&mut self.fight, &self.fight_data)
            && fight.take_dirty()
        {
            *data
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(fight.save());
        }
    }

    /// What the dragon and the crystals reported, handed to the fight.
    fn dragon_events(
        &mut self,
        level: &mut Level,
        survival: &mut WorldSurvival,
        global: &Global,
        dimension: &str,
    ) {
        let events = std::mem::take(&mut self.cx.dragon_events);
        let Some(fight) = &mut self.fight else {
            return;
        };
        let state = &global.level;
        let hooks = crate::plugins::Hooks::of(&global.plugins, dimension);
        let rules = state.rules().clone();
        let no_blocks = NoBlocks;
        let raining = |_| false;
        let mut no_gateway = |_| false;
        let mut env = survival.env(
            state,
            &rules,
            &no_blocks,
            &raining,
            dimension,
            &mut no_gateway,
            &hooks,
        );
        for event in events {
            fight.handle(level, &mut env, &mut self.cx, event);
        }
        self.cx.dragon_fight = Some(fight.view());
        self.store_fight();
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
            let mut no_gateway = |_| false;
            let mut env = survival.env(
                state,
                &rules,
                &blocks,
                &raining,
                dimension,
                &mut no_gateway,
                &hooks,
            );
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
                        // The hit goes to the dragon through the part.
                        if let Some(i) = dragon::part_of(&ents, target, network_id)
                            && let Some(s) = ents.mob_mut(target).and_then(|m| m.ext.dragon_mut())
                        {
                            s.hit_part = Some(i);
                        }
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
                    EntityAction::OpenVehicleInventory => {
                        // `openCustomInventoryScreen` of a chest boat the player rides.
                        if let Some(v) = ents.vehicle_of(player)
                            && ents.boat(v).is_some_and(|b| b.kind.chest)
                        {
                            minecart::open_container(&mut ents, &mut env, &mut self.cx, player, v);
                        }
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
        self.apply_actions(level, survival, global, dimension);
        vanillite_game::menu::container::open_requests(level, std::mem::take(&mut self.cx.menus));
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
        // `WitherSkullBlock.setPlacedBy` of the skulls players placed since the last tick.
        for pos in std::mem::take(&mut level.output().summon_checks) {
            let seed = self.seed ^ level.game_time() ^ i64::from(pos.x) << 32 ^ i64::from(pos.z);
            boss::wither::check_spawn(level, pos, state.difficulty, seed);
        }
        let has_players = !level.entities.players.is_empty();
        let env = environment(global, dimension, survival);
        self.cx.begin_tick(env, self.settings, has_players);
        // What spawner blocks read when the block entities tick after the entities.
        level.spawner_env = vanillite_game::block::spawner::SpawnerEnv {
            env,
            difficulty: state.difficulty,
            spawner_blocks_work: rules.bool(Rule::SPAWNER_BLOCKS_WORK),
            spawn_mobs: rules.bool(Rule::SPAWN_MOBS),
        };
        self.lightning_hits(level, survival, global, dimension);
        let activation = f64::from(self.settings.activation_range);
        minecart::players_push(&mut level.entities);
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
                let chunk = BlockPos::containing(pos.x, pos.y, pos.z).chunk();
                if !level.chunks.in_entity_ticking_range(chunk) {
                    continue;
                }
                level.entities = ents;
                vanillite_game::entities::tick_entity(level, id, kind);
                self.apply_explosion_hurts(level, survival, global, dimension);
                self.apply_actions(level, survival, global, dimension);
                ents = std::mem::take(&mut level.entities);
                continue;
            }
            let is_mob = is_mob_kind(kind);
            let is_special = is_special_kind(kind);
            let is_boat = BoatKind::of(kind).is_some();
            let is_minecart = MinecartKind::of(kind).is_some();
            if !is_mob
                && !is_special
                && !is_boat
                && !is_minecart
                && !projectile::is_arrow_kind(kind)
                && kind != ids::EXPERIENCE_ORB
                && kind != ids::AREA_EFFECT_CLOUD
            {
                continue;
            }
            let chunk = BlockPos::containing(pos.x, pos.y, pos.z).chunk();
            if !level.is_block_ticking(chunk) {
                continue;
            }
            if kind == ids::EXPERIENCE_ORB {
                level.entities = ents;
                vanillite_game::entities::tick_entity(level, id, kind);
                ents = std::mem::take(&mut level.entities);
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
                let mut env = survival.env(
                    state,
                    &rules,
                    &blocks,
                    &raining,
                    dimension,
                    &mut no_gateway,
                    &hooks,
                );
                let level_ref: &Level = level;
                if is_mob {
                    mob::tick(level_ref, &mut ents, &mut env, &mut self.cx, id);
                } else if is_special {
                    special::tick(level_ref, &mut ents, &mut env, &mut self.cx, id);
                } else if is_boat {
                    vehicle::tick_boat(level_ref, &mut ents, &mut env, &mut self.cx, id);
                } else if is_minecart {
                    minecart::tick(level_ref, &mut ents, &mut env, &mut self.cx, id);
                } else if projectile::is_arrow_kind(kind) {
                    projectile::tick_arrow(level_ref, &mut ents, &mut env, &mut self.cx, id);
                } else if kind == ids::AREA_EFFECT_CLOUD {
                    vanillite_game::effect::cloud::tick(
                        level_ref,
                        &mut ents,
                        &mut env,
                        &mut self.cx,
                        id,
                    );
                }
            }
            if !self.cx.actions.is_empty() || !self.cx.dragon_events.is_empty() {
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
        boss::wither::sweep_boss_bars(&mut self.cx, &ents, &mut survival.effects);
        // `rideTick`: passengers follow their vehicles; leads of knots and loaded mobs.
        riding::position_all_riders(&mut ents);
        {
            let blocks = WorldBlocks(&level.chunks);
            let raining = |_| false;
            let mut no_gateway = |_| false;
            let mut env = survival.env(
                state,
                &rules,
                &blocks,
                &raining,
                dimension,
                &mut no_gateway,
                &hooks,
            );
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
        while !self.cx.actions.is_empty() || !self.cx.dragon_events.is_empty() {
            self.apply_queued(level, survival, global, dimension, &rules);
            self.finish_removals(level);
            if !self.cx.dragon_events.is_empty() {
                self.dragon_events(level, survival, global, dimension);
                self.finish_removals(level);
            }
        }
        self.finish_removals(level);
    }

    /// One round of the queued world changes.
    fn apply_queued(
        &mut self,
        level: &mut Level,
        survival: &mut WorldSurvival,
        global: &Global,
        dimension: &str,
        rules: &vanillite_game::survival::rules::GameRules,
    ) {
        for action in std::mem::take(&mut self.cx.actions) {
            match action {
                WorldAction::ExplodeWith {
                    source,
                    indirect,
                    pos,
                    radius,
                    blocks,
                } => {
                    // `ExplosionInteraction.BLOCK` decays with `blockExplosionDropDecay`.
                    let interaction = if !blocks {
                        BlockInteraction::Keep
                    } else if rules.bool(Rule::BLOCK_EXPLOSION_DROP_DECAY) {
                        BlockInteraction::DestroyWithDecay
                    } else {
                        BlockInteraction::Destroy
                    };
                    level.explode(source, indirect, pos, radius, interaction);
                    self.apply_explosion_hurts(level, survival, global, dimension);
                }
                WorldAction::Explode(e) => {
                    e.run(level, rules);
                    self.apply_explosion_hurts(level, survival, global, dimension);
                }
                WorldAction::Level(action) => action.apply(level),
                other => {
                    other.apply_to_level(level);
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
        let mut env = survival.env(
            state,
            &rules,
            &no_blocks,
            &raining,
            dimension,
            &mut no_gateway,
            &hooks,
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
            if ents.boat(hurt.target).is_some() || ents.minecart(hurt.target).is_some() {
                // `VehicleEntity.ignoreExplosion`: mob explosions without mob griefing.
                let mob_source = hurt.indirect.is_some_and(|c| ents.mob(c).is_some());
                if !(mob_source && !rules.bool(Rule::MOB_GRIEFING)) {
                    hurt_vehicle(
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

    /// The magic damage block entities dealt this tick (conduits hunting mobs).
    pub(crate) fn apply_magic_hurts(
        &mut self,
        level: &mut Level,
        survival: &mut WorldSurvival,
        global: &Global,
        dimension: &str,
    ) {
        if level.output().magic_hurts.is_empty() {
            return;
        }
        let state = &global.level;
        let hooks = crate::plugins::Hooks::of(&global.plugins, dimension);
        let rules = state.rules().clone();
        let hurts = std::mem::take(&mut level.output().magic_hurts);
        let no_blocks = NoBlocks;
        let raining = |_| false;
        let mut no_gateway = |_| false;
        let mut env = survival.env(
            state,
            &rules,
            &no_blocks,
            &raining,
            dimension,
            &mut no_gateway,
            &hooks,
        );
        let mut ents = std::mem::take(&mut level.entities);
        for (target, amount) in hurts {
            let source = DamageSource::of(env.types.magic);
            combat::hurt(
                level,
                &mut ents,
                &mut env,
                &mut self.cx,
                target,
                source,
                amount,
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
        let rules = state.rules().clone();
        let mut ents = std::mem::take(&mut level.entities);
        {
            let no_blocks = NoBlocks;
            let raining = |_| false;
            let hooks = crate::plugins::Hooks::of(&global.plugins, dimension);
            let mut no_gateway = |_| false;
            let mut env = survival.env(
                state,
                &rules,
                &no_blocks,
                &raining,
                dimension,
                &mut no_gateway,
                &hooks,
            );
            let source = DamageSource::of(env.types.lightning_bolt);
            for id in struck {
                if let Some(d) = ents.item_mut(id) {
                    d.health -= 5;
                    if d.health <= 0 {
                        self.cx.discarded.push(id);
                    }
                    continue;
                }
                if ents.boat(id).is_some() || ents.minecart(id).is_some() {
                    hurt_vehicle(&mut ents, &mut env, &mut self.cx, id, source, 5.0);
                    continue;
                }
                let Some(m) = ents.mob_mut(id) else {
                    continue;
                };
                if !m.is_alive() {
                    continue;
                }
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
    Environment::overworld(day, w.rain(), w.thunder())
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
    // A dragon part (`getEntityOrPart`): the part's box.
    let target_box = match dragon::part_of(ents, target, network_id) {
        Some(i) => ents
            .mob(target)
            .and_then(|m| m.ext.dragon())
            .map_or(te.aabb(), |s| s.part_box(i)),
        None => te.aabb(),
    };
    let eye = Vec3::new(
        pe.pos().x,
        pe.pos().y + f64::from(pe.kind().info().eye_height) * f64::from(pe.dims().height / 1.8),
        pe.pos().z,
    );
    (distance_sqr_to_box(&target_box, eye) < 36.0).then_some(target)
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
    if ents.boat(target).is_some() || ents.minecart(target).is_some() {
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
        hurt_vehicle(ents, env, cx, target, source, damage);
        return;
    }
    // `canHarmPlayer`: the pvp rule.
    if ents.player(target).is_some() && !env.rules.bool(Rule::PVP) {
        return;
    }
    attack::player_attack(level, ents, env, cx, player, target);
}

/// `VehicleEntity.hurtServer` of a boat or minecart.
fn hurt_vehicle(
    ents: &mut Entities,
    env: &mut vanillite_game::survival::player::Env<'_>,
    cx: &mut MobContext,
    target: EntityId,
    source: DamageSource,
    amount: f32,
) {
    if ents.minecart(target).is_some() {
        minecart::hurt(ents, env, cx, target, source, amount);
    } else {
        vehicle::hurt_boat(ents, env, cx, target, source, amount);
    }
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

/// `use_item` of the items that need entities: boats, the carrot on a stick, thrown potions,
/// and items used over time.
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
    } else if name == "minecraft:carrot_on_a_stick" {
        riding::boost(ents, env, player, &mut stack, creative);
    } else if vanillite_game::effect::thrown::kind_of(&stack).is_some() {
        vanillite_game::effect::thrown::throw(ents, env, network_ids, player, &mut stack, creative);
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
    } else {
        minecart::use_item_on(level, ents, network_ids, pos, &mut stack, creative);
    }
    put_back(ents, player, hand, stack);
}
