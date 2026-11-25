//! Statistics and advancements of the players of a world, once per tick after their own tick:
//! the triggers of `ServerPlayer.tick` (`tick`, `location`, `enter_block`, the inventory
//! listener's `inventory_changed`, crafted results), `flushDirty`, the scores of the stat
//! criteria (`Scoreboard.forAllObjectives`) and the rewards of completed advancements.

use vanillite_base::BlockPos;
use vanillite_config::GameMode;
use vanillite_data::block::BlockState;
use vanillite_game::advancements::predicate::{EntityView, World};
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
fn player_view<'a>(
    e: &EntityRef<'_>,
    inv: &'a PlayerInventory,
    survival: &Survival,
    stats: &'a StatsCounter,
    game_mode: GameMode,
    team: Option<String>,
) -> EntityView<'a> {
    static EMPTY: ItemStack = ItemStack::EMPTY;
    let flags = e.flags();
    let [feet, legs, chest, head] = &inv.armor;
    EntityView {
        kind: "minecraft:player",
        pos: e.pos(),
        on_ground: flags.contains(EntityFlags::ON_GROUND),
        on_fire: flags.contains(EntityFlags::ON_FIRE),
        crouching: e.pose() == Pose::Crouching,
        sprinting: flags.contains(EntityFlags::SPRINTING),
        swimming: flags.contains(EntityFlags::SWIMMING),
        flying: false,
        fall_flying: flags.contains(EntityFlags::FALL_FLYING),
        in_water: survival.fluids.in_water(),
        equipment: [
            head,
            chest,
            legs,
            feet,
            &EMPTY,
            inv.selected_item(),
            &inv.offhand,
        ],
        team,
        player: Some((game_mode, survival.xp.level, stats)),
    }
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
    let mut players = std::mem::take(&mut level.entities.players);
    let mut scores = Vec::new();
    let mut rewards = Vec::new();
    for (id, player) in players.iter_mut() {
        let Some(entity) = level.entities.store.get(id) else {
            continue;
        };
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
            let view = player_view(&entity, inventory, survival, stats, *game_mode, team);
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
        let changes = player.stats.take_score_changes();
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

/// `AdvancementRewards.grant`'s loot: the loot tables have no `advancement_reward` context
/// (`getRandomItems` for a player) yet, so nothing is given.
fn give_loot(_level: &mut Level, _id: EntityId, _tables: &[String]) {}

/// `ServerPlayer.awardRecipesByKey`: players have no recipe book yet, so nothing is unlocked.
fn award_recipes(_level: &mut Level, _id: EntityId, _recipes: &[String]) {}

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
        let rewards = &a.rewards;
        crate::commands::add_experience(env.level, id, rewards.experience, false);
        give_loot(env.level, id, &rewards.loot);
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
    for index in 0..crate::server::DIMENSIONS.len() {
        let Ok(index) = u16::try_from(index) else {
            continue;
        };
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
