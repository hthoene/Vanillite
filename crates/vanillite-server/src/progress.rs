//! Statistics and advancements of the players of a world, once per tick after their own tick:
//! the triggers of `ServerPlayer.tick` (`tick`, `location`, `enter_block`, the inventory
//! listener's `inventory_changed`, crafted results), `flushDirty`, the scores of the stat
//! criteria (`Scoreboard.forAllObjectives`) and the rewards of completed advancements.

use vanillite_base::BlockPos;
use vanillite_config::GameMode;
use vanillite_data::block::BlockState;
use vanillite_game::advancements::predicate::{EffectView, EntityView, PlayerFacts, World};
use vanillite_game::advancements::trigger::{Event, TriggerContext, TriggerKind};
use vanillite_game::inventory::PlayerInventory;
use vanillite_game::item::ItemStack;
use vanillite_game::level::Level;
use vanillite_game::player::PlayerData;
use vanillite_game::player::join::display_name;
use vanillite_game::scoreboard::CriteriaKind;
use vanillite_game::stats::{ScoreChange, Stat, StatType, StatsCounter};
use vanillite_game::survival::player::Survival;
use vanillite_game::survival::rules::Rule;
use vanillite_protocol::text::Component;
use vanillite_world::entity::{EntityFlags, EntityId, EntityRef, Pose};

use crate::commands::Env;
use crate::global::Global;
use crate::systems::RegistryTags;
use crate::world::WorldId;

/// The world as the advancement predicates read it.
struct LevelWorld<'a> {
    level: &'a Level,
    dimension: &'a str,
    tags: &'a RegistryTags,
    biomes: &'a [String],
}

impl World for LevelWorld<'_> {
    fn dimension(&self) -> &str {
        self.dimension
    }

    fn block(&self, pos: BlockPos) -> Option<BlockState> {
        self.level.chunks.block(pos).map(BlockState)
    }

    fn biome(&self, pos: BlockPos) -> Option<String> {
        self.biomes.get(usize::from(self.level.biome(pos))).cloned()
    }

    fn in_tag(&self, registry: &str, tag: &str, id: &str) -> bool {
        self.tags
            .get(registry)
            .and_then(|t| t.get(tag))
            .is_some_and(|e| e.iter().any(|e| e == id))
    }
}

/// A player as the entity predicates see it.
#[allow(clippy::too_many_arguments)]
fn player_view<'a>(
    level: &Level,
    id: EntityId,
    e: &EntityRef<'_>,
    inv: &'a PlayerInventory,
    survival: &Survival,
    stats: &'a StatsCounter,
    game_mode: GameMode,
    team: Option<String>,
) -> EntityView<'a> {
    use std::borrow::Cow;
    let flags = e.flags();
    let [feet, legs, chest, head] = &inv.armor;
    let mut v = EntityView::new("minecraft:player", e.pos());
    v.on_ground = flags.contains(EntityFlags::ON_GROUND);
    v.on_fire = flags.contains(EntityFlags::ON_FIRE);
    v.crouching = e.pose() == Pose::Crouching;
    v.sprinting = flags.contains(EntityFlags::SPRINTING);
    v.swimming = flags.contains(EntityFlags::SWIMMING);
    v.flying = survival.flying;
    v.fall_flying = flags.contains(EntityFlags::FALL_FLYING);
    v.in_water = survival.fluids.in_water();
    v.equipment = [
        Cow::Borrowed(head),
        Cow::Borrowed(chest),
        Cow::Borrowed(legs),
        Cow::Borrowed(feet),
        Cow::Owned(ItemStack::EMPTY),
        Cow::Borrowed(inv.selected_item()),
        Cow::Borrowed(&inv.offhand),
    ];
    v.team = team;
    v.player = Some(PlayerFacts {
        game_mode,
        level: survival.xp.level,
        stats: Some(stats),
    });
    v.effects = survival
        .effects
        .iter()
        .map(|e| EffectView {
            effect: e.effect,
            amplifier: i32::from(e.amplifier),
            duration: e.duration,
            ambient: e.ambient,
            visible: e.visible,
        })
        .collect();
    if let Some(vehicle) = level.entities.riding.vehicle_of(id)
        && let Some(view) = vanillite_game::advancements::view::snapshot(&level.entities, vehicle)
    {
        v.vehicle = Some(view);
    }
    v
}

/// `onInsideBlock` of `checkInsideBlocks`: the non-air blocks the player's box is inside.
fn inside_blocks(level: &Level, e: &EntityRef<'_>, out: &mut Vec<Event>) {
    let b = e.aabb().inflate(-1.0e-5, -1.0e-5, -1.0e-5);
    let floor = vanillite_base::math::floor;
    for x in floor(b.min_x)..=floor(b.max_x) {
        for y in floor(b.min_y)..=floor(b.max_y) {
            for z in floor(b.min_z)..=floor(b.max_z) {
                if let Some(state) = level.chunks.block(BlockPos::new(x, y, z)).map(BlockState)
                    && !state.is_air()
                {
                    out.push(Event::EnterBlock(state));
                }
            }
        }
    }
}

/// The per-tick work of the players' statistics and advancements.
pub(crate) fn tick(level: &mut Level, global: &Global, world: WorldId, dimension: &str) {
    let tags = global.systems.tags();
    let no_biomes = Vec::new();
    let biomes = global
        .registries
        .get("minecraft:worldgen/biome")
        .unwrap_or(&no_biomes);
    // Statistics, triggers and recipes block code awarded during the tick.
    for (id, award) in level.take_player_awards() {
        if let Some(player) = level.entities.player_mut(id) {
            award.apply(player);
        }
    }
    // `ServerPlayer.tick`'s distance triggers, and the damage and deaths of the tick whose
    // triggers need the entities.
    let ids: Vec<EntityId> = level.entities.players.iter().map(|(id, _)| id).collect();
    for id in ids {
        vanillite_game::advancements::tracking::tick(&mut level.entities, id);
        vanillite_game::advancements::view::resolve_combat(
            &mut level.entities,
            &global.level.damage_types,
            id,
        );
    }
    let mut players = std::mem::take(&mut level.entities.players);
    let mut scores = Vec::new();
    let mut rewards = Vec::new();
    for (id, player) in players.iter_mut() {
        let Some(entity) = level.entities.store.get(id) else {
            continue;
        };
        for award in std::mem::take(&mut player.inventory.awards) {
            award.apply(player);
        }
        // `ResultSlot.checkTakeAchievements`.
        for crafted in std::mem::take(&mut player.inventory.crafted) {
            if !crafted.stack.is_empty() {
                let stat = Stat::of(StatType::Crafted, crafted.stack.item);
                player.stats.award(stat, crafted.stack.count);
            }
            if let Some(recipe) = crafted.recipe {
                let event = Event::RecipeCrafted(recipe, crafted.ingredients);
                player.advancements.trigger(event);
            }
        }
        player.advancements.check_inventory(&player.inventory);
        let a = &player.advancements;
        let mut extra = Vec::new();
        if a.listens(TriggerKind::TICK) {
            extra.push(Event::Tick);
        }
        if a.listens(TriggerKind::ENTER_BLOCK) {
            inside_blocks(level, &entity, &mut extra);
        }
        if player.survival.tick_count % 20 == 0 && a.listens(TriggerKind::LOCATION) {
            extra.push(Event::Location);
        }
        if !extra.is_empty() || a.has_events() {
            let name = &player.profile.name;
            let team = global
                .systems
                .scoreboard()
                .team_of(name)
                .map(|t| t.name.clone());
            let PlayerData {
                advancements,
                inventory,
                survival,
                stats,
                game_mode,
                ..
            } = player;
            let view = player_view(
                level, id, &entity, inventory, survival, stats, *game_mode, team,
            );
            let world = LevelWorld {
                level,
                dimension,
                tags: &tags,
                biomes,
            };
            advancements.process(extra, &TriggerContext::new(&world, &view, inventory));
        }
        if player.advancements.has_completed() {
            rewards.push(id);
        }
        if let Some(packet) = player.advancements.flush(true) {
            player.connection.sink.send(&packet);
        }
        let mut changes = player.stats.take_score_changes();
        if !changes.is_empty() && global.plugins.wants(vanillite_abi::event::PLAYER_STATISTIC) {
            let hooks = crate::plugins::Hooks::of(global, dimension);
            player
                .stats
                .fire_increases(&mut changes, &hooks, entity.uuid());
        }
        if !changes.is_empty() {
            scores.push((player.profile.name.clone(), entity.uuid(), changes));
        }
    }
    level.entities.players = players;
    apply_scores(global, scores);
    let mut env = Env {
        level,
        global,
        world,
        others: Vec::new(),
    };
    for id in rewards {
        grant_rewards(&mut env, id);
    }
}

/// A player's name, UUID and statistic changes.
pub(crate) type PlayerScores = (String, u128, Vec<(Stat, ScoreChange)>);

/// `forAllObjectives(stat, player, ...)` for the statistic changes of the tick.
pub(crate) fn apply_scores(global: &Global, scores: Vec<PlayerScores>) {
    if scores.is_empty() {
        return;
    }
    let mut board = global.systems.scoreboard();
    let objectives: Vec<(String, String)> = board
        .objectives()
        .filter_map(|o| match &o.criteria {
            CriteriaKind::Stat(name) => Some((name.clone(), o.name.clone())),
            _ => None,
        })
        .collect();
    if objectives.is_empty() {
        return;
    }
    for (holder, uuid, changes) in scores {
        let display = display_name(&holder, uuid);
        for (stat, change) in changes {
            let criteria = stat.name();
            for (_, objective) in objectives.iter().filter(|(c, _)| *c == criteria) {
                match change {
                    ScoreChange::Add(n) => {
                        let value = board.score(&holder, objective).map_or(0, |s| s.value);
                        board.set_score(&holder, objective, value.wrapping_add(n), Some(&display));
                    }
                    ScoreChange::Reset => {
                        board.reset_score(&holder, objective);
                    }
                }
            }
        }
    }
}

/// The loot of `AdvancementRewards.grant`: the tables rolled with the player as `this`
/// (`ADVANCEMENT_REWARD`), each stack added to the inventory with a pickup sound or dropped
/// for the player alone.
fn give_loot(env: &mut Env<'_>, id: EntityId, tables: &[String]) {
    if tables.is_empty() {
        return;
    }
    let Some(pos) = env.level.entities.store.get(id).map(|e| e.pos()) else {
        return;
    };
    let params = crate::commands::loot_cmd::Params {
        origin: Some(pos),
        this_entity: Some(id),
        ..Default::default()
    };
    let mut stacks = Vec::new();
    crate::commands::loot_cmd::with_context(env, params, |data, ctx, rng| {
        for table in tables {
            data.loot.roll(table, ctx, rng, &mut stacks);
        }
    });
    let mut changed = false;
    for mut stack in stacks {
        let level = &mut *env.level;
        let Some(player) = level.entities.player_mut(id) else {
            return;
        };
        let infinite = player.game_mode == vanillite_config::GameMode::Creative;
        if player.inventory.add(None, &mut stack, infinite) && stack.is_empty() {
            use vanillite_base::random::RandomSource;
            let r = level.random();
            let pitch = ((r.next_float() - r.next_float()) * 0.7 + 1.0) * 2.0;
            if let Some(sound) =
                vanillite_data::registry::SOUND_EVENT.id("minecraft:entity.item.pickup")
            {
                level.play_sound(
                    None,
                    pos.x,
                    pos.y,
                    pos.z,
                    sound,
                    vanillite_game::level::SOUND_PLAYERS,
                    0.2,
                    pitch,
                );
            }
            changed = true;
        } else {
            crate::commands::items::drop_for(level, id, stack);
        }
    }
    if changed {
        crate::commands::items::sync_inventory(&mut env.level.entities, id);
    }
}

/// `ServerPlayer.awardRecipesByKey`.
fn award_recipes(level: &mut Level, id: EntityId, recipes: &[String]) {
    if recipes.is_empty() {
        return;
    }
    if let Some(player) = level.entities.player_mut(id) {
        let ids: Vec<&str> = recipes.iter().map(String::as_str).collect();
        player.unlock_recipes(&ids);
    }
}

/// `AdvancementRewards.grant` and the chat announcement of the advancements a player of this
/// world completed.
pub(crate) fn grant_rewards(env: &mut Env<'_>, id: EntityId) {
    let global = env.global;
    let Some(uuid) = env.level.entities.store.get(id).map(|e| e.uuid()) else {
        return;
    };
    let Some(player) = env.level.entities.player_mut(id) else {
        return;
    };
    let completed = player.advancements.take_completed();
    let set = std::sync::Arc::clone(player.advancements.advancements());
    let name = player.profile.name.clone();
    for index in completed {
        let Some(a) = set.get(index) else {
            continue;
        };
        global
            .plugins
            .advancement_done(u32::from(env.world.0), uuid, &a.id);
        let rewards = &a.rewards;
        crate::commands::add_experience(env.level, id, rewards.experience, false);
        give_loot(env, id, &rewards.loot);
        award_recipes(env.level, id, &rewards.recipes);
        if let Some(function) = &rewards.function {
            let library = global.systems.functions();
            if let (Some(f), Some(mut source)) = (
                library.get(function),
                crate::commands::player_source(&env.level.entities, global, id, env.world),
            ) {
                // `withSuppressedOutput().withPermission(GAMEMASTER)`.
                source.silent = true;
                source.level = vanillite_game::commands::level::GAMEMASTERS;
                crate::commands::functions::run_function(f, &source, env);
            }
        }
        if let Some(display) = &a.display
            && display.announce_to_chat
            && global.level.rules().bool(Rule::SHOW_ADVANCEMENT_MESSAGES)
        {
            let message = Component::translatable(
                format!("chat.type.advancement.{}", display.frame.name()),
                vec![display_name(&name, uuid), a.name.clone()],
            );
            global.players.broadcast_system(message, false, None);
        }
    }
}

/// `PlayerAdvancements.reload` of every online player after `/reload`: the progress kept for
/// the advancements that still exist, sent again as a full update.
pub(crate) fn reload_advancements(global: &Global) {
    for index in global.dimensions.worlds().map(|w| w.0) {
        let Some(arc) = global.arc() else {
            return;
        };
        global.run_in_world(WorldId(index), move |w| {
            let set = arc.systems.advancements();
            let entities = w.entities_mut();
            let ids: Vec<EntityId> = entities.players.iter().map(|(id, _)| id).collect();
            for id in ids {
                if let Some(p) = entities.player_mut(id) {
                    let saved = p.advancements.to_json();
                    p.advancements = vanillite_game::advancements::PlayerAdvancements::load(
                        std::sync::Arc::clone(&set),
                        Some(&saved),
                    );
                }
            }
        });
    }
}
