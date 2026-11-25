//! Block breaking and placing, item use and dropping (Vanilla's `ServerPlayerGameMode` and the
//! matching handlers of `ServerGamePacketListenerImpl`).

use std::collections::HashMap;

use vanillite_base::math::{cos, floor, sin};
use vanillite_base::{Aabb, BlockPos, Direction, Vec3};
use vanillite_config::GameMode;
use vanillite_data::block::{BlockKind, BlockState, flags as block_flags};
use vanillite_protocol::codec::Reader;
use vanillite_protocol::components::ids;
use vanillite_protocol::packets::play::clientbound::BlockUpdate;
use vanillite_protocol::packets::play::serverbound::{DigAction, Hand};
use vanillite_protocol::types::BlockPos as WirePos;

use super::sink::PacketSink;
use crate::advancements::trigger::Event;
use crate::block::place::{self, BlockReader, PlaceContext};
use crate::inventory::{Drop, PlayerInventory};
use crate::item::{self, ItemStack, ToolTable};
use crate::level::update;
use crate::loot::{BlockLootContext, LootTables};
use crate::player::world::{PlayLevel, SoundSource, events};
use crate::stats::{Stat, StatType, custom};

/// Static gameplay data the handlers read: tool rules, loot tables and enchantment ids.
#[derive(Debug, Default)]
pub struct GameData {
    /// Tool rules with resolved tags.
    pub tools: ToolTable,
    /// Block loot tables.
    pub loot: LootTables,
    /// `minecraft:enchantment` ids by name.
    pub enchantments: HashMap<String, u32>,
    /// Item tags: tag id → item ids.
    pub item_tags: HashMap<String, Vec<u16>>,
    /// Entity type tags: tag id → entity type ids.
    pub entity_tags: HashMap<String, Vec<u16>>,
    /// The combat effects of the enchantments, by registry id.
    pub enchants: crate::enchant::Enchantments,
    /// `#minecraft:enderman_holdable` block ids.
    pub enderman_holdable: Vec<u16>,
    /// Recipes.
    pub recipes: crate::recipe::Recipes,
    /// Enchantment data.
    pub enchanting: crate::enchant::Enchanting,
    /// The definitions of the enchantments by name.
    pub enchantment_info: HashMap<String, EnchantmentInfo>,
    /// The `minecraft:trial_spawner` registry.
    pub trial_spawner_configs: crate::block::trial_spawner::TrialConfigs,
    /// The `minecraft:banner_pattern` registry.
    pub banner_patterns: crate::menu::BannerPatterns,
    /// The tags of the registries: registry → tag id (without `#`) → entry ids in tag order.
    pub registry_tags: HashMap<String, HashMap<String, Vec<u32>>>,
}

/// What `/enchant` reads of an enchantment (`Enchantment` and its `EnchantmentDefinition`).
#[derive(Debug, Clone)]
pub struct EnchantmentInfo {
    /// `description`.
    pub description: vanillite_protocol::text::Component,
    /// `max_level`.
    pub max_level: i32,
    /// `supported_items`: item ids.
    pub supported_items: Vec<u16>,
    /// `exclusive_set`: enchantment names.
    pub exclusive_set: Vec<String>,
}

impl crate::predicate::EntityTags for GameData {
    fn entity_in_tag(&self, kind: vanillite_data::entity::EntityTypeId, tag: &str) -> bool {
        self.entity_tags
            .get(tag)
            .is_some_and(|v| v.contains(&kind.0))
    }
}

static GAME_DATA: std::sync::OnceLock<std::sync::Arc<GameData>> = std::sync::OnceLock::new();

/// Installs the gameplay data for code that has no player context (block drops of the level
/// simulation). Data packs are frozen for the process: later calls are ignored.
pub fn install_game_data(data: std::sync::Arc<GameData>) {
    let _ = GAME_DATA.set(data);
}

/// The installed gameplay data, if any.
pub fn game_data() -> Option<&'static GameData> {
    GAME_DATA.get().map(|d| &**d)
}

impl GameData {
    /// Level of the enchantment `name` on `stack`.
    pub fn enchantment_level(&self, stack: &ItemStack, name: &str) -> i32 {
        let Some(&id) = self.enchantments.get(name) else {
            return 0;
        };
        item::enchantments(stack)
            .into_iter()
            .find(|(e, _)| *e == id)
            .map_or(0, |(_, level)| level)
    }

    /// Whether an item is in an item tag.
    pub fn item_in_tag(&self, item: u16, tag: &str) -> bool {
        self.item_tags.get(tag).is_some_and(|v| v.contains(&item))
    }

    /// One holder set entry of `registry` (`"ns:id"` or `"#ns:tag"`) as entry ids, in tag
    /// order; unknown entries give none.
    pub fn holder_set(&self, registry: &str, entry: &str) -> Vec<u32> {
        if let Some(tag) = entry.strip_prefix('#') {
            return self
                .registry_tags
                .get(registry)
                .and_then(|tags| tags.get(tag))
                .cloned()
                .unwrap_or_default();
        }
        if registry == "minecraft:enchantment" {
            return self.enchantments.get(entry).copied().into_iter().collect();
        }
        if let Some(r) = vanillite_data::registry::Registry::by_name(registry) {
            return r.id(entry).map(u32::from).into_iter().collect();
        }
        use vanillite_protocol::components_nbt::RegistryNames;
        vanillite_protocol::item::InstalledRegistries
            .entry_id(registry, entry)
            .and_then(|id| u32::try_from(id).ok())
            .into_iter()
            .collect()
    }
}

/// The acting player.
#[derive(Clone, Copy, Debug)]
pub struct Actor {
    /// Entity id in the world (`None` in tests without a world).
    pub entity: Option<vanillite_world::entity::EntityId>,
    /// Entity network id.
    pub network_id: i32,
    /// UUID.
    pub uuid: u128,
    /// Feet position.
    pub pos: Vec3,
    /// Eye height of the current pose.
    pub eye_height: f64,
    /// Yaw.
    pub y_rot: f32,
    /// Pitch.
    pub x_rot: f32,
    /// On ground.
    pub on_ground: bool,
    /// Sneaking (`isSecondaryUseActive`).
    pub sneaking: bool,
    /// Game mode.
    pub game_mode: GameMode,
    /// Operator permission level (0–4).
    pub permission_level: u8,
    /// Whether the player's eyes are in water.
    pub eyes_in_water: bool,
    /// The status effects that change the mining speed.
    pub dig: crate::inventory::DigEffects,
}

impl Actor {
    fn creative(&self) -> bool {
        self.game_mode == GameMode::Creative
    }

    fn eye(&self) -> Vec3 {
        Vec3::new(self.pos.x, self.pos.y + self.eye_height, self.pos.z)
    }

    /// `blockInteractionRange`: 4.5, plus 0.5 in creative.
    fn block_range(&self) -> f64 {
        if self.creative() { 5.0 } else { 4.5 }
    }

    /// `isWithinBlockInteractionRange(pos, buffer)`.
    pub fn within_block_range(&self, pos: BlockPos, buffer: f64) -> bool {
        let max = self.block_range() + buffer;
        aabb_distance_sqr(&Aabb::of_block(pos), self.eye()) < max * max
    }

    /// `isWithinEntityInteractionRange(box, buffer)`: the entity range is 3, plus 2 in
    /// creative.
    pub fn within_entity_range(&self, aabb: &Aabb, buffer: f64) -> bool {
        let range = if self.creative() { 5.0 } else { 3.0 };
        let max = range + buffer;
        aabb_distance_sqr(aabb, self.eye()) < max * max
    }
}

/// `AABB.distanceToSqr(Vec3)`.
fn aabb_distance_sqr(b: &Aabb, p: Vec3) -> f64 {
    let dx = (b.min_x - p.x).max(p.x - b.max_x).max(0.0);
    let dy = (b.min_y - p.y).max(p.y - b.max_y).max(0.0);
    let dz = (b.min_z - p.z).max(p.z - b.max_z).max(0.0);
    dx * dx + dy * dy + dz * dz
}

/// What a play handler needs besides the connection.
pub struct PlayContext<'a> {
    /// The world.
    pub level: &'a mut dyn PlayLevel,
    /// The player's inventory.
    pub inventory: &'a mut PlayerInventory,
    /// The player's experience (`None` in tests).
    pub xp: Option<&'a mut crate::survival::player::Experience>,
    /// The player's recipe book (`None` in tests).
    pub recipe_book: Option<&'a mut crate::recipe::book::PlayerRecipeBook>,
    /// The player.
    pub actor: Actor,
    /// Gameplay data.
    pub data: &'a GameData,
    /// Whether a block is protected around the spawn for this player.
    pub spawn_protected: &'a dyn Fn(BlockPos) -> bool,
    /// Stacks the player drops this tick.
    pub drops: &'a mut Vec<Drop>,
    /// Hand swings other players must see.
    pub swings: &'a mut Vec<Hand>,
    /// Plugin hooks (block breaking and placing).
    pub hooks: &'a dyn crate::hooks::GameHooks,
    /// The player's statistics (`None` in tests).
    pub stats: Option<&'a mut crate::stats::StatsCounter>,
    /// The player's advancements (`None` in tests).
    pub advancements: Option<&'a mut crate::advancements::PlayerAdvancements>,
}

impl PlayContext<'_> {
    /// `ServerPlayer.awardStat`.
    pub fn award_stat(&mut self, stat: Stat, count: i32) {
        if let Some(stats) = self.stats.as_deref_mut() {
            stats.award(stat, count);
        }
    }

    /// Fires a criterion trigger of the player.
    pub fn trigger(&mut self, event: Event) {
        if let Some(advancements) = self.advancements.as_deref_mut() {
            advancements.trigger(event);
        }
    }
}

impl std::fmt::Debug for PlayContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlayContext")
            .field("actor", &self.actor)
            .finish_non_exhaustive()
    }
}

/// `Direction.from3DDataValue(abs(v % 6))`.
pub fn direction_from_byte(v: u8) -> Direction {
    Direction::ALL
        .get(usize::from(v % 6))
        .copied()
        .unwrap_or(Direction::Down)
}

/// `Direction.BY_ID` with `WRAP`.
pub fn direction_wrapped(v: i32) -> Direction {
    Direction::ALL
        .get(usize::try_from(v.rem_euclid(6)).unwrap_or(0))
        .copied()
        .unwrap_or(Direction::Down)
}

fn relative(pos: BlockPos, d: Direction) -> BlockPos {
    let [x, y, z] = d.normal();
    BlockPos::new(
        pos.x.wrapping_add(x),
        pos.y.wrapping_add(y),
        pos.z.wrapping_add(z),
    )
}

fn wire(pos: BlockPos) -> WirePos {
    WirePos {
        x: pos.x,
        y: pos.y,
        z: pos.z,
    }
}

fn block_update(sink: &dyn PacketSink, level: &dyn PlayLevel, pos: BlockPos) {
    sink.send(&BlockUpdate {
        pos: wire(pos),
        state: i32::from(level.block(pos).0),
    });
}

/// Blocks only operators in creative may break or use (`GameMasterBlock`).
fn is_game_master_block(state: BlockState) -> bool {
    matches!(
        state.block().info().kind,
        BlockKind::CommandBlock
            | BlockKind::StructureBlock
            | BlockKind::JigsawBlock
            | BlockKind::TestBlock
            | BlockKind::TestInstanceBlock
    )
}

/// Blocks whose `useWithoutItem`/`useItemOn` consume a click (doors, levers, containers, …).
/// Their interactions are not implemented yet; the click is consumed so no block is placed
/// against them, and the client's prediction is corrected by the block updates.
fn has_use_action(state: BlockState) -> bool {
    matches!(
        state.block().info().kind,
        BlockKind::DoorBlock
            | BlockKind::TrapDoorBlock
            | BlockKind::FenceGateBlock
            | BlockKind::ButtonBlock
            | BlockKind::LeverBlock
            | BlockKind::ChestBlock
            | BlockKind::TrappedChestBlock
            | BlockKind::CopperChestBlock
            | BlockKind::WeatheringCopperChestBlock
            | BlockKind::EnderChestBlock
            | BlockKind::BarrelBlock
            | BlockKind::ShulkerBoxBlock
            | BlockKind::FurnaceBlock
            | BlockKind::BlastFurnaceBlock
            | BlockKind::SmokerBlock
            | BlockKind::CraftingTableBlock
            | BlockKind::CrafterBlock
            | BlockKind::AnvilBlock
            | BlockKind::BedBlock
            | BlockKind::NoteBlock
            | BlockKind::RepeaterBlock
            | BlockKind::ComparatorBlock
            | BlockKind::DispenserBlock
            | BlockKind::DropperBlock
            | BlockKind::HopperBlock
            | BlockKind::EnchantingTableBlock
            | BlockKind::BrewingStandBlock
            | BlockKind::LoomBlock
            | BlockKind::CartographyTableBlock
            | BlockKind::GrindstoneBlock
            | BlockKind::StonecutterBlock
            | BlockKind::SmithingTableBlock
            | BlockKind::BeaconBlock
            | BlockKind::LecternBlock
            | BlockKind::DaylightDetectorBlock
            | BlockKind::CakeBlock
            | BlockKind::BellBlock
            | BlockKind::CommandBlock
            | BlockKind::StructureBlock
            | BlockKind::JigsawBlock
    )
}

/// Block access for the placement rules.
struct LevelReader<'a>(&'a dyn PlayLevel);

impl BlockReader for LevelReader<'_> {
    fn state(&self, pos: BlockPos) -> BlockState {
        self.0.block(pos)
    }

    fn min_y(&self) -> i32 {
        self.0.min_y()
    }

    fn max_y(&self) -> i32 {
        self.0.max_y().saturating_add(1)
    }
}

/// `ServerPlayerGameMode`: the digging state of a player.
#[derive(Clone, Debug, Default)]
pub struct Digging {
    game_ticks: i32,
    is_destroying: bool,
    destroy_progress_start: i32,
    destroy_pos: Option<BlockPos>,
    destroy_direction: u8,
    has_delayed_destroy: bool,
    delayed_destroy_pos: BlockPos,
    delayed_tick_start: i32,
    last_sent_state: i32,
    /// Highest block change sequence to acknowledge this tick (`ackBlockChangesUpTo`).
    pub ack_up_to: i32,
    /// `dropSpamThrottler` for creative drops.
    drop_spam: i32,
}

/// `getDestroyProgress` of `state` for the actor.
fn destroy_progress(ctx: &PlayContext<'_>, state: BlockState) -> f32 {
    let speed = state.misc().destroy_speed;
    if speed == -1.0 {
        return 0.0;
    }
    let tool = ctx.inventory.selected_item();
    let correct = !state.has(block_flags::REQUIRES_CORRECT_TOOL)
        || ctx.data.tools.is_correct_for_drops(tool, state);
    let efficiency = ctx.data.enchantment_level(tool, "minecraft:efficiency");
    let aqua = ctx.inventory.equipment(item::EquipmentSlot::Head).clone();
    let submerged = ctx.actor.eyes_in_water
        && ctx.data.enchantment_level(&aqua, "minecraft:aqua_affinity") == 0;
    let player_speed = crate::inventory::destroy_speed(
        &ctx.data.tools,
        tool,
        state,
        efficiency,
        ctx.actor.dig,
        submerged,
        ctx.actor.on_ground,
    );
    let modifier = if correct { 30.0 } else { 100.0 };
    player_speed / speed / modifier
}

/// `(int)(progress * 10.0F)`.
fn crack_state(progress: f32) -> i32 {
    let v = progress * 10.0;
    if v.is_nan() {
        0
    } else {
        // Saturating float-to-int like Java's `(int)`.
        #[allow(clippy::as_conversions)]
        let i = v as i32;
        i
    }
}

impl Digging {
    /// The state of a joining player (`ackBlockChangesUpTo = -1`).
    pub fn new() -> Digging {
        Digging {
            ack_up_to: -1,
            last_sent_state: -1,
            ..Digging::default()
        }
    }

    /// `ackBlockChangesUpTo`.
    fn ack(&mut self, sequence: i32) {
        self.ack_up_to = self.ack_up_to.max(sequence);
    }

    /// `ServerPlayerGameMode.tick`.
    pub fn tick(&mut self, ctx: &mut PlayContext<'_>, sink: &dyn PacketSink) {
        self.game_ticks = self.game_ticks.wrapping_add(1);
        self.drop_spam = self.drop_spam.saturating_sub(20).max(0);
        if self.has_delayed_destroy {
            let pos = self.delayed_destroy_pos;
            let state = ctx.level.block(pos);
            if state.is_air() {
                self.has_delayed_destroy = false;
            } else {
                let ticks = self.game_ticks.wrapping_sub(self.delayed_tick_start);
                let progress = self.increment_progress(ctx, state, pos, ticks);
                if progress >= 1.0 {
                    self.has_delayed_destroy = false;
                    self.destroy_block(ctx, pos, sink);
                }
            }
        } else if self.is_destroying
            && let Some(pos) = self.destroy_pos
        {
            let state = ctx.level.block(pos);
            if state.is_air() {
                ctx.level.destroy_progress(ctx.actor.network_id, pos, -1);
                self.last_sent_state = -1;
                self.is_destroying = false;
            } else {
                let ticks = self.game_ticks.wrapping_sub(self.destroy_progress_start);
                let event = if ticks % 4 == 0 {
                    events::PARTICLES_AND_SOUND_BLOCK_DESTROY_PROGRESS
                } else {
                    events::PARTICLES_BLOCK_DESTROY_PROGRESS
                };
                self.increment_progress(ctx, state, pos, ticks);
                ctx.level
                    .level_event(None, event, pos, i32::from(self.destroy_direction));
            }
        }
    }

    fn increment_progress(
        &mut self,
        ctx: &mut PlayContext<'_>,
        state: BlockState,
        pos: BlockPos,
        ticks: i32,
    ) -> f32 {
        #[allow(clippy::as_conversions)]
        let factor = ticks.saturating_add(1) as f32;
        let progress = destroy_progress(ctx, state) * factor;
        let crack = crack_state(progress);
        if crack != self.last_sent_state {
            ctx.level.destroy_progress(ctx.actor.network_id, pos, crack);
            self.last_sent_state = crack;
        }
        progress
    }

    /// `handlePlayerAction` for the digging actions, then `ackBlockChangesUpTo`.
    pub fn handle_action(
        &mut self,
        ctx: &mut PlayContext<'_>,
        action: DigAction,
        pos: BlockPos,
        direction: u8,
        sequence: i32,
        sink: &dyn PacketSink,
    ) {
        self.handle_block_break_action(ctx, action, pos, direction, sink);
        self.ack(sequence.max(0));
    }

    fn handle_block_break_action(
        &mut self,
        ctx: &mut PlayContext<'_>,
        action: DigAction,
        pos: BlockPos,
        direction: u8,
        sink: &dyn PacketSink,
    ) {
        if !ctx.actor.within_block_range(pos, 1.0) {
            if self.is_destroying && action == DigAction::AbortDestroyBlock {
                self.abort(ctx, pos);
            }
            return;
        }
        if pos.y > ctx.level.max_y() {
            block_update(sink, ctx.level, pos);
            return;
        }
        match action {
            DigAction::StartDestroyBlock => {
                if (ctx.spawn_protected)(pos) {
                    return;
                }
                if !ctx.level.is_loaded(pos) {
                    block_update(sink, ctx.level, pos);
                    return;
                }
                if ctx.actor.creative() {
                    self.destroy_and_ack(ctx, pos, sink);
                    return;
                }
                if block_action_restricted(ctx) {
                    block_update(sink, ctx.level, pos);
                    return;
                }
                self.destroy_progress_start = self.game_ticks;
                let state = ctx.level.block(pos);
                let mut progress = 1.0;
                if !state.is_air() {
                    progress = destroy_progress(ctx, state);
                }
                if !state.is_air() && progress >= 1.0 {
                    self.destroy_and_ack(ctx, pos, sink);
                } else {
                    if self.is_destroying
                        && let Some(old) = self.destroy_pos
                    {
                        block_update(sink, ctx.level, old);
                    }
                    self.is_destroying = true;
                    self.destroy_pos = Some(pos);
                    self.destroy_direction = direction % 6;
                    let crack = crack_state(progress);
                    ctx.level.destroy_progress(ctx.actor.network_id, pos, crack);
                    self.last_sent_state = crack;
                }
            }
            DigAction::ChangeDestroyDirection => self.destroy_direction = direction % 6,
            DigAction::StopDestroyBlock => {
                if Some(pos) == self.destroy_pos {
                    let ticks = self.game_ticks.wrapping_sub(self.destroy_progress_start);
                    let state = ctx.level.block(pos);
                    if !state.is_air() {
                        #[allow(clippy::as_conversions)]
                        let factor = ticks.saturating_add(1) as f32;
                        let progress = destroy_progress(ctx, state) * factor;
                        // Vanilla accepts 70 % to allow for latency (fast-break check).
                        if progress >= 0.7 {
                            self.is_destroying = false;
                            ctx.level.destroy_progress(ctx.actor.network_id, pos, -1);
                            self.destroy_and_ack(ctx, pos, sink);
                            return;
                        }
                        if !self.has_delayed_destroy {
                            self.is_destroying = false;
                            self.has_delayed_destroy = true;
                            self.delayed_destroy_pos = pos;
                            self.delayed_tick_start = self.destroy_progress_start;
                        }
                    }
                }
            }
            DigAction::AbortDestroyBlock => self.abort(ctx, pos),
            _ => {}
        }
    }

    fn abort(&mut self, ctx: &mut PlayContext<'_>, pos: BlockPos) {
        self.is_destroying = false;
        if let Some(old) = self.destroy_pos
            && old != pos
        {
            ctx.level.destroy_progress(ctx.actor.network_id, old, -1);
        }
        ctx.level.destroy_progress(ctx.actor.network_id, pos, -1);
    }

    fn destroy_and_ack(&mut self, ctx: &mut PlayContext<'_>, pos: BlockPos, sink: &dyn PacketSink) {
        if !self.destroy_block(ctx, pos, sink) {
            block_update(sink, ctx.level, pos);
        }
    }

    /// `ServerPlayerGameMode.destroyBlock`.
    fn destroy_block(
        &mut self,
        ctx: &mut PlayContext<'_>,
        pos: BlockPos,
        sink: &dyn PacketSink,
    ) -> bool {
        let state = ctx.level.block(pos);
        let tool = ctx.inventory.selected_item().clone();
        if ctx.actor.creative() && !ctx.data.tools.can_destroy_in_creative(&tool) {
            return false;
        }
        if is_game_master_block(state) && !(ctx.actor.creative() && ctx.actor.permission_level >= 2)
        {
            block_update(sink, ctx.level, pos);
            return false;
        }
        if block_action_restricted(ctx) {
            return false;
        }
        if !ctx.hooks.block_break(ctx.actor.uuid, pos, state) {
            block_update(sink, ctx.level, pos);
            return false;
        }
        // `playerWillDestroy`: break particles and sound for everyone else, and the other half
        // of double blocks.
        ctx.level.level_event(
            Some(ctx.actor.network_id),
            events::PARTICLES_DESTROY_BLOCK,
            pos,
            i32::from(state.0),
        );
        let mut extra = Vec::new();
        place::destroy_extra(
            state,
            pos,
            &LevelReader(&*ctx.level),
            ctx.actor.creative(),
            &mut extra,
        );
        for (p, s) in extra {
            let old = ctx.level.block(p);
            if ctx
                .level
                .set_block(p, s, update::ALL | update::SUPPRESS_DROPS)
            {
                ctx.level.level_event(
                    Some(ctx.actor.network_id),
                    events::PARTICLES_DESTROY_BLOCK,
                    p,
                    i32::from(old.0),
                );
            }
        }
        // `ShulkerBoxBlock.playerWillDestroy`: the loot table is rolled for the player (in
        // creative by `isEmpty`, without the player).
        if crate::block::container::kind_of(state.block())
            == Some(crate::block::container::Kind::ShulkerBox)
            && let Some(level) = ctx.level.level_mut()
        {
            let player = if ctx.actor.creative() {
                None
            } else {
                ctx.actor.entity
            };
            crate::block::chest_loot::unpack(level, pos, player);
        }
        // The block entity as `playerDestroy` sees it (captured before the removal).
        let entity = ctx
            .level
            .level_mut()
            .and_then(|l| l.block_entity(pos).cloned());
        if ctx.actor.creative()
            && let Some(entity) = &entity
            && let Some(level) = ctx.level.level_mut()
        {
            // `ShulkerBoxBlock.playerWillDestroy`: a creative player still gets a filled box.
            crate::block::container::creative_shulker_drop(level, pos, state, entity);
        }
        ctx.level.game_event(
            events_ids().block_destroy,
            pos,
            ctx.actor.entity,
            Some(state),
        );
        // `removeBlock`: the fluid of a waterlogged block stays.
        let replacement = state.fluid_state().block_state();
        let changed = ctx.level.set_block(pos, replacement, update::ALL);
        if ctx.actor.creative() {
            return true;
        }
        let correct = !state.has(block_flags::REQUIRES_CORRECT_TOOL)
            || ctx.data.tools.is_correct_for_drops(&tool, state);
        self.mine_block(ctx, state);
        if changed && correct {
            // `Block.playerDestroy`.
            ctx.award_stat(Stat::of(StatType::Mined, state.block().0), 1);
            drop_resources(ctx, state, pos, &tool, entity.as_ref());
        }
        true
    }

    /// `ItemStack.mineBlock`: tools count as used and lose durability.
    fn mine_block(&mut self, ctx: &mut PlayContext<'_>, state: BlockState) {
        let tool = ctx.inventory.selected_item();
        if !ctx.data.tools.is_tool(tool) {
            return;
        }
        let used = tool.item;
        ctx.award_stat(Stat::of(StatType::Used, used), 1);
        let damage = {
            let tool = ctx.inventory.selected_item();
            if state.misc().destroy_speed == 0.0 {
                return;
            }
            ctx.data.tools.damage_per_block(tool)
        };
        if damage == 0 {
            return;
        }
        let unbreaking = ctx
            .data
            .enchantment_level(ctx.inventory.selected_item(), "minecraft:unbreaking");
        let mut amount: i32 = 0;
        for _ in 0..damage {
            // `minecraft:unbreaking`: a tool loses durability with chance 1 / (level + 1).
            #[allow(clippy::as_conversions)]
            let chance = 1.0 / (unbreaking.max(0) as f32 + 1.0);
            if unbreaking <= 0 || ctx.level.random_float() < chance {
                amount = amount.saturating_add(1);
            }
        }
        let Some(tool) = ctx.inventory.selected_item_mut() else {
            return;
        };
        if amount == 0 || !item::is_damageable(tool) {
            return;
        }
        let new_damage = item::damage(tool).saturating_add(amount);
        if new_damage >= item::max_damage(tool) {
            let broken = tool.item;
            tool.shrink(1);
            // `onEquippedItemBroken`.
            ctx.award_stat(Stat::of(StatType::Broken, broken), 1);
        } else {
            item::set_damage(tool, new_damage);
        }
    }

    /// `handleUseItemOn`.
    pub fn use_item_on(
        &mut self,
        ctx: &mut PlayContext<'_>,
        p: &vanillite_protocol::packets::play::serverbound::UseItemOn,
        sink: &dyn PacketSink,
    ) {
        self.ack(p.sequence.max(0));
        let pos = BlockPos::new(p.pos.x, p.pos.y, p.pos.z);
        let face = direction_wrapped(p.face);
        if !ctx.actor.within_block_range(pos, 1.0) {
            return;
        }
        let hit = Vec3::new(
            f64::from(pos.x) + f64::from(p.cursor_x),
            f64::from(pos.y) + f64::from(p.cursor_y),
            f64::from(pos.z) + f64::from(p.cursor_z),
        );
        let limit = 1.000_000_1;
        let center = Vec3::new(
            f64::from(pos.x) + 0.5,
            f64::from(pos.y) + 0.5,
            f64::from(pos.z) + 0.5,
        );
        if !((hit.x - center.x).abs() < limit
            && (hit.y - center.y).abs() < limit
            && (hit.z - center.z).abs() < limit)
        {
            log::warn!("Rejecting UseItemOnPacket: location too far away from hit block {pos:?}");
            return;
        }
        if pos.y <= ctx.level.max_y() && pos.y >= ctx.level.min_y() {
            if (ctx.spawn_protected)(pos) || !ctx.level.is_loaded(pos) {
                // Spawn protection: nothing happens.
            } else if self.use_item_on_block(ctx, p.hand, pos, face, hit, p.inside, sink) {
                // Vanilla 26.3 swings twice for a successful use (two branches of
                // `handleUseItemOn` both call `swingAndResetAttackStrength`).
                ctx.swings.push(p.hand);
                ctx.swings.push(p.hand);
            }
            block_update(sink, ctx.level, pos);
            block_update(sink, ctx.level, relative(pos, face));
        }
    }

    /// `ServerPlayerGameMode.useItemOn`; returns whether the hand swings for others.
    #[allow(clippy::too_many_arguments)]
    fn use_item_on_block(
        &mut self,
        ctx: &mut PlayContext<'_>,
        hand: Hand,
        pos: BlockPos,
        face: Direction,
        hit: Vec3,
        inside: bool,
        sink: &dyn PacketSink,
    ) -> bool {
        let state = ctx.level.block(pos);
        if ctx.actor.game_mode == GameMode::Spectator {
            // Spectators open the menus of blocks that have one, nothing else.
            if let Some(level) = ctx.level.level_mut()
                && let Some(request) = crate::block::container::menu_provider(level, state, pos)
            {
                open_menu(ctx, request, sink);
            }
            return false;
        }
        let has_items =
            !ctx.inventory.selected_item().is_empty() || !ctx.inventory.offhand.is_empty();
        let suppress_use = ctx.actor.sneaking && has_items;
        if !suppress_use {
            // `state.useItemOn(stack, …)`, then `useWithoutItem` for the main hand when it
            // tries with an empty hand.
            let (has_use, has_item_use) = ctx.level.level_mut().map_or((false, false), |l| {
                let b = l.behaviours().get(state.block());
                (b.use_without_item.is_some(), b.use_item_on.is_some())
            });
            if (has_use || has_item_use)
                && let Some(player) = ctx.actor.entity
            {
                let use_ctx = crate::block::UseContext {
                    player,
                    may_build: ctx.actor.game_mode != GameMode::Adventure,
                    yaw: ctx.actor.y_rot,
                    face,
                    hit,
                };
                let mut held = match hand {
                    Hand::MainHand => ctx.inventory.selected_item().clone(),
                    Hand::OffHand => ctx.inventory.offhand.clone(),
                };
                let before = held.clone();
                let item_use = match ctx.level.level_mut() {
                    Some(level) if has_item_use => level.use_item_on(pos, &use_ctx, &mut held),
                    _ => crate::block::ItemInteraction::TryWithEmptyHand,
                };
                let (result, item_used) = match item_use {
                    crate::block::ItemInteraction::Done(result) => {
                        if held != before && !ctx.actor.creative() {
                            let slot = match hand {
                                Hand::MainHand => ctx.inventory.selected_item_mut(),
                                Hand::OffHand => Some(&mut ctx.inventory.offhand),
                            };
                            if let Some(slot) = slot {
                                *slot = held;
                            }
                        }
                        (Some(result), true)
                    }
                    crate::block::ItemInteraction::TryWithEmptyHand
                        if hand == Hand::MainHand && has_use =>
                    {
                        let used = ctx
                            .level
                            .level_mut()
                            .map(|level| level.use_without_item(pos, &use_ctx));
                        (used, false)
                    }
                    _ => (None, false),
                };
                self.open_requested(ctx, sink);
                if let Some(
                    result @ (crate::block::Interaction::Success
                    | crate::block::Interaction::Consume),
                ) = result
                {
                    ctx.trigger(if item_used {
                        Event::ItemUsedOnBlock(pos, before)
                    } else {
                        Event::DefaultBlockUse(pos)
                    });
                    any_block_use(ctx, hand, pos);
                    return result == crate::block::Interaction::Success;
                }
            } else if has_use_action(state) {
                // Blocks whose use is not implemented yet consume the click.
                return false;
            }
        }
        let stack = match hand {
            Hand::MainHand => ctx.inventory.selected_item().clone(),
            Hand::OffHand => ctx.inventory.offhand.clone(),
        };
        // `VaultBlock.useItemOn`: an active vault takes the key, the use is consumed.
        if !suppress_use
            && state.block() == vanillite_data::block::ids::VAULT
            && let Some(level) = ctx.level.level_mut()
            && let Some(taken) =
                crate::block::vault::use_item_on(level, pos, ctx.actor.uuid, &stack)
        {
            if taken && !ctx.actor.creative() {
                let count = crate::block::vault::key_count(level, pos);
                let slot = match hand {
                    Hand::MainHand => ctx.inventory.selected_item_mut(),
                    Hand::OffHand => Some(&mut ctx.inventory.offhand),
                };
                if let Some(slot) = slot {
                    slot.shrink(count);
                }
            }
            return true;
        }
        if stack.is_empty() {
            return false;
        }
        let used = item::item_id(&stack);
        if used == vanillite_data::item::ids::FLINT_AND_STEEL
            || used == vanillite_data::item::ids::ENDER_EYE
        {
            // `ItemStack.useOn`: adventure players cannot build.
            if ctx.actor.game_mode == GameMode::Adventure {
                return false;
            }
            return if used == vanillite_data::item::ids::FLINT_AND_STEEL {
                self.use_flint_and_steel(ctx, hand, pos, face)
            } else {
                use_ender_eye(ctx, hand, pos)
            };
        }
        if used == vanillite_data::item::ids::END_CRYSTAL {
            if !ctx.level.use_end_crystal(pos) {
                return false;
            }
            if !ctx.actor.creative() {
                let slot = match hand {
                    Hand::MainHand => ctx.inventory.selected_item_mut(),
                    Hand::OffHand => Some(&mut ctx.inventory.offhand),
                };
                if let Some(slot) = slot {
                    slot.shrink(1);
                }
            }
            return true;
        }
        let Some(_block) = item::item_id(&stack).block() else {
            // Non-block items (buckets, tools on blocks, spawn eggs, ...) come later.
            return false;
        };
        if ctx.actor.game_mode == GameMode::Adventure {
            // `can_place_on` predicates are not evaluated yet: adventure players cannot build.
            return false;
        }
        let place_ctx = PlaceContext::new(
            pos,
            face,
            hit,
            inside,
            ctx.actor.y_rot,
            ctx.actor.x_rot,
            ctx.actor.sneaking,
            item::item_id(&stack),
            &LevelReader(&*ctx.level),
        );
        let Some(place_ctx) =
            place::update_placement_context(&place_ctx, &LevelReader(&*ctx.level))
        else {
            return false;
        };
        let placed = self.place_block(ctx, hand, &stack, &place_ctx);
        self.open_requested(ctx, sink);
        if placed {
            // `ItemStack.useOn` of a successful item interaction.
            ctx.award_stat(Stat::of(StatType::Used, stack.item), 1);
            ctx.trigger(Event::ItemUsedOnBlock(pos, stack));
            any_block_use(ctx, hand, pos);
        }
        placed
    }

    /// Opens the menus and sign editors blocks asked for during a use.
    fn open_requested(&mut self, ctx: &mut PlayContext<'_>, sink: &dyn PacketSink) {
        let Some(level) = ctx.level.level_mut() else {
            return;
        };
        let requests = level.take_menu_requests();
        let editors = level.take_sign_editors();
        let gives = level.take_player_gives();
        for request in requests {
            open_menu(ctx, request, sink);
        }
        for (player, mut stack) in gives {
            if Some(player) == ctx.actor.entity {
                let infinite = ctx.actor.creative();
                if !ctx.inventory.add(None, &mut stack, infinite) {
                    ctx.drops.push(Drop {
                        stack,
                        thrown: false,
                    });
                }
            }
        }
        for (player, pos, front) in editors {
            if Some(player) == ctx.actor.entity {
                sink.send(
                    &vanillite_protocol::packets::play::clientbound::OpenSignEditor {
                        pos: wire(pos),
                        front,
                    },
                );
            }
        }
    }

    /// `FlintAndSteelItem.useOn`: lights a campfire, candle or candle cake, or places fire
    /// against the clicked face (lighting a nether portal frame).
    fn use_flint_and_steel(
        &mut self,
        ctx: &mut PlayContext<'_>,
        hand: Hand,
        pos: BlockPos,
        face: Direction,
    ) -> bool {
        let state = ctx.level.block(pos);
        let lightable = can_light(state);
        let target = if lightable { pos } else { relative(pos, face) };
        if !lightable {
            // `BaseFireBlock.canBePlacedAt`.
            let reader = LevelReader(&*ctx.level);
            let fire = place::fire_state(&reader, target);
            let forward = place::horizontal_direction_of(ctx.actor.y_rot);
            let placeable = ctx.level.block(target).is_air()
                && (place::can_survive(fire, target, &reader)
                    || ctx.level.lights_portal(target, forward));
            if !placeable {
                return false;
            }
        }
        let pitch = ctx.level.random_float() * 0.4 + 0.8;
        let sound = vanillite_data::registry::SOUND_EVENT
            .id("minecraft:item.flintandsteel.use")
            .unwrap_or(0);
        ctx.level.play_sound(
            Some(ctx.actor.network_id),
            Vec3::new(
                f64::from(target.x) + 0.5,
                f64::from(target.y) + 0.5,
                f64::from(target.z) + 0.5,
            ),
            sound,
            SoundSource::Blocks,
            1.0,
            pitch,
        );
        let new_state = if lightable {
            state.with("lit", "true").unwrap_or(state)
        } else {
            place::fire_state(&LevelReader(&*ctx.level), target)
        };
        ctx.level
            .set_block(target, new_state, update::ALL_IMMEDIATE);
        hurt_and_break(ctx, hand, 1);
        true
    }

    /// `BlockItem.place`.
    fn place_block(
        &mut self,
        ctx: &mut PlayContext<'_>,
        hand: Hand,
        stack: &ItemStack,
        place_ctx: &PlaceContext,
    ) -> bool {
        let target = place_ctx.pos;
        if target.y < ctx.level.min_y() || target.y > ctx.level.max_y() {
            return false;
        }
        if !ctx.level.is_loaded(target) {
            return false;
        }
        let existing = ctx.level.block(target);
        if !place_ctx.replace_clicked && !place::can_be_replaced(existing, place_ctx) {
            return false;
        }
        let Some(mut state) = place::state_for_placement(place_ctx, &LevelReader(&*ctx.level))
        else {
            return false;
        };
        if !ctx.level.is_unobstructed(state, target) {
            return false;
        }
        if !ctx.hooks.block_place(ctx.actor.uuid, target, state) {
            return false;
        }
        if !ctx.level.set_block(target, state, update::ALL_IMMEDIATE) {
            return false;
        }
        ctx.trigger(Event::PlacedBlock(target, stack.clone()));
        if ctx.level.block(target).block() == state.block() {
            if let Some(updated) = apply_block_state_component(stack, state) {
                ctx.level.set_block(target, updated, update::CLIENTS);
                state = updated;
            }
            let mut extra = Vec::new();
            place::placed_extra(
                state,
                target,
                place_ctx,
                &LevelReader(&*ctx.level),
                &mut extra,
            );
            for (p, s) in extra {
                ctx.level.set_block(p, s, update::ALL);
            }
            if state.block().info().kind == BlockKind::MossyCarpetBlock {
                // The topper draws one boolean per side that can hang down; which sides can is
                // fixed by the level, so count them first, then draw from the level random.
                let mut sides = 0u8;
                place::mossy_carpet_topper(state, target, &LevelReader(&*ctx.level), &mut || {
                    sides = sides.saturating_add(1);
                    false
                });
                let draws: Vec<bool> = (0..sides).map(|_| ctx.level.random_boolean()).collect();
                let mut draws = draws.into_iter();
                if let Some(topper) = place::mossy_carpet_topper(
                    state,
                    target,
                    &LevelReader(&*ctx.level),
                    &mut || draws.next().unwrap_or(false),
                ) {
                    ctx.level
                        .set_block(target.relative(Direction::Up), topper, update::ALL);
                }
            }
        }
        let placed = ctx.level.block(target);
        if let Some(level) = ctx.level.level_mut()
            && placed.block() == state.block()
        {
            // `BlockItem.updateCustomBlockEntityTag` and `Block.setPlacedBy`.
            crate::level::block_entity::apply_components(level, target, stack);
            crate::block::redstone::set_placed_by(level, placed, target);
            crate::block::crafter::set_placed_by(level, placed, target);
            if placed.block() == vanillite_data::block::ids::TRIPWIRE_HOOK {
                crate::block::tripwire::hook_set_placed_by(level, target, placed);
            }
            // `SignItem.updateCustomBlockEntityTag`: the editor of the front opens.
            if crate::block::sign::is_sign(placed.block())
                && stack.component(ids::BLOCK_ENTITY_DATA).is_none()
                && let Some(player) = ctx.actor.entity
                && let Some(crate::level::BlockEntity::Sign(sign)) = level.block_entity_mut(target)
            {
                sign.editor = Some(ctx.actor.uuid);
                level.request_sign_editor(player, target, true);
            }
        }
        if placed.block() == state.block() {
            ctx.level.placed_by(target, placed);
        }
        let sound = placed.block().sound();
        ctx.level.play_sound(
            Some(ctx.actor.network_id),
            Vec3::new(
                f64::from(target.x) + 0.5,
                f64::from(target.y) + 0.5,
                f64::from(target.z) + 0.5,
            ),
            sound.place_sound,
            SoundSource::Blocks,
            (sound.volume + 1.0) / 2.0,
            sound.pitch * 0.8,
        );
        ctx.level.game_event(
            events_ids().block_place,
            target,
            ctx.actor.entity,
            Some(placed),
        );
        if !ctx.actor.creative() {
            let slot = match hand {
                Hand::MainHand => ctx.inventory.selected_item_mut(),
                Hand::OffHand => Some(&mut ctx.inventory.offhand),
            };
            if let Some(slot) = slot {
                slot.shrink(1);
            }
        }
        true
    }

    /// `handleUseItem`: only the acknowledgement for now (item use comes with its items).
    pub fn use_item(&mut self, sequence: i32) {
        self.ack(sequence.max(0));
    }

    /// `drop(boolean all)` (Q / Ctrl+Q).
    pub fn drop_selected(&mut self, ctx: &mut PlayContext<'_>, all: bool) {
        if ctx.actor.game_mode == GameMode::Spectator {
            return;
        }
        let stack = ctx.inventory.drop_selected(all);
        if !stack.is_empty() {
            ctx.drops.push(Drop {
                stack,
                thrown: true,
            });
        }
    }

    /// `handleSetCreativeModeSlot`.
    pub fn set_creative_slot(&mut self, ctx: &mut PlayContext<'_>, slot: i16, stack: ItemStack) {
        if !ctx.actor.creative() {
            return;
        }
        let valid_data = stack.is_empty() || stack.count <= item::max_stack_size(&stack);
        if (1..=45).contains(&slot) && valid_data {
            ctx.inventory
                .set_creative_slot(usize::try_from(slot).unwrap_or(0), stack);
        } else if slot < 0 && valid_data && !stack.is_empty() {
            // `dropSpamThrottler`: 20 per tick, threshold 1480.
            if self.drop_spam < 1480 {
                self.drop_spam = self.drop_spam.saturating_add(20);
                ctx.drops.push(Drop {
                    stack,
                    thrown: true,
                });
            } else {
                log::warn!("A player was dropping items too fast in creative mode, ignoring.");
            }
        }
    }

    /// The block change sequence to acknowledge this tick, reset afterwards.
    pub fn take_ack(&mut self) -> Option<i32> {
        let ack = self.ack_up_to;
        self.ack_up_to = -1;
        (ack > -1).then_some(ack)
    }
}

/// `ANY_BLOCK_USE` after a use that consumed the click, with the stack now in the hand.
fn any_block_use(ctx: &mut PlayContext<'_>, hand: Hand, pos: BlockPos) {
    let held = match hand {
        Hand::MainHand => ctx.inventory.selected_item().clone(),
        Hand::OffHand => ctx.inventory.offhand.clone(),
    };
    ctx.trigger(Event::AnyBlockUse(pos, held));
}

/// The statistic a block awards when it opens its menu (`getOpenChestStat`,
/// `INTERACT_WITH_*`, `INSPECT_*`).
fn open_stat(block: vanillite_data::block::BlockId) -> Option<Stat> {
    let name = block.name().strip_prefix("minecraft:").unwrap_or("");
    Some(match name {
        "trapped_chest" => custom::TRIGGER_TRAPPED_CHEST,
        "ender_chest" => custom::OPEN_ENDERCHEST,
        "barrel" => custom::OPEN_BARREL,
        "dispenser" => custom::INSPECT_DISPENSER,
        "dropper" => custom::INSPECT_DROPPER,
        "hopper" => custom::INSPECT_HOPPER,
        "crafting_table" => custom::INTERACT_WITH_CRAFTING_TABLE,
        "furnace" => custom::INTERACT_WITH_FURNACE,
        "blast_furnace" => custom::INTERACT_WITH_BLAST_FURNACE,
        "smoker" => custom::INTERACT_WITH_SMOKER,
        "stonecutter" => custom::INTERACT_WITH_STONECUTTER,
        "anvil" | "chipped_anvil" | "damaged_anvil" => custom::INTERACT_WITH_ANVIL,
        "grindstone" => custom::INTERACT_WITH_GRINDSTONE,
        "smithing_table" => custom::INTERACT_WITH_SMITHING_TABLE,
        "brewing_stand" => custom::INTERACT_WITH_BREWINGSTAND,
        "cartography_table" => custom::INTERACT_WITH_CARTOGRAPHY_TABLE,
        "loom" => custom::INTERACT_WITH_LOOM,
        "beacon" => custom::INTERACT_WITH_BEACON,
        _ if name.ends_with("shulker_box") => custom::OPEN_SHULKER_BOX,
        _ if name == "chest" || name.ends_with("copper_chest") => custom::OPEN_CHEST,
        _ => return None,
    })
}

/// `ServerPlayer.openMenu` for a menu a block asked for.
fn open_menu(
    ctx: &mut PlayContext<'_>,
    request: crate::menu::container::MenuRequest,
    sink: &dyn PacketSink,
) {
    let Some(level) = ctx.level.level_mut() else {
        return;
    };
    let who = crate::menu::container::Opener {
        spectator: ctx.actor.game_mode == GameMode::Spectator,
        infinite: ctx.actor.creative(),
        disconnected: false,
    };
    // Spectators open menus without using the block, so without its statistic.
    let stat = open_stat(request.block).filter(|_| !who.spectator);
    crate::menu::container::open(ctx.inventory, level, request, who, ctx.drops, sink);
    if let Some(stat) = stat {
        ctx.award_stat(stat, 1);
    }
}

/// `CampfireBlock.canLight`, `CandleBlock.canLight` and `CandleCakeBlock.canLight`: an unlit
/// (and, for campfires and candles, dry) one.
fn can_light(state: BlockState) -> bool {
    let blocks = &vanillite_worldgen::behaviour::tags().blocks;
    let unlit = state.value("lit") == Some("false");
    let dry = state.value("waterlogged") == Some("false");
    if blocks.campfires.contains(state) || blocks.candles.contains(state) {
        unlit && dry
    } else {
        blocks.candle_cakes.contains(state) && unlit
    }
}

/// `EnderEyeItem.useOn`: an eye into an empty End portal frame.
fn use_ender_eye(ctx: &mut PlayContext<'_>, hand: Hand, pos: BlockPos) -> bool {
    let state = ctx.level.block(pos);
    if state.block() != vanillite_data::block::ids::END_PORTAL_FRAME
        || state.value("eye") != Some("false")
    {
        return false;
    }
    let filled = state.with("eye", "true").unwrap_or(state);
    // `Block.pushEntitiesUp` comes with entity collisions.
    ctx.level.set_block(pos, filled, update::CLIENTS);
    if !ctx.actor.creative() {
        let slot = match hand {
            Hand::MainHand => ctx.inventory.selected_item_mut(),
            Hand::OffHand => Some(&mut ctx.inventory.offhand),
        };
        if let Some(slot) = slot {
            slot.shrink(1);
        }
    }
    // The comparators, the eye's particles and sound (1503), then the portal.
    ctx.level.end_portal_frame_filled(pos);
    true
}

/// `ItemStack.hurtAndBreak(amount, player, hand)`: the item in `hand` loses durability (the
/// unbreaking enchantment may save each point); creative players' items do not.
fn hurt_and_break(ctx: &mut PlayContext<'_>, hand: Hand, amount: i32) {
    if ctx.actor.creative() {
        return;
    }
    let stack = match hand {
        Hand::MainHand => ctx.inventory.selected_item().clone(),
        Hand::OffHand => ctx.inventory.offhand.clone(),
    };
    if stack.is_empty() || !item::is_damageable(&stack) {
        return;
    }
    let unbreaking = ctx.data.enchantment_level(&stack, "minecraft:unbreaking");
    let mut damage: i32 = 0;
    for _ in 0..amount {
        #[allow(clippy::as_conversions)]
        let chance = 1.0 / (unbreaking.max(0) as f32 + 1.0);
        if unbreaking <= 0 || ctx.level.random_float() < chance {
            damage = damage.saturating_add(1);
        }
    }
    let slot = match hand {
        Hand::MainHand => ctx.inventory.selected_item_mut(),
        Hand::OffHand => Some(&mut ctx.inventory.offhand),
    };
    let Some(tool) = slot else {
        return;
    };
    if damage == 0 {
        return;
    }
    let new_damage = item::damage(tool).saturating_add(damage);
    if new_damage >= item::max_damage(tool) {
        tool.shrink(1);
    } else {
        item::set_damage(tool, new_damage);
    }
}

/// `blockActionRestricted`: spectators never, adventure players only with `can_break`
/// (not evaluated yet, so never).
fn block_action_restricted(ctx: &PlayContext<'_>) -> bool {
    matches!(
        ctx.actor.game_mode,
        GameMode::Spectator | GameMode::Adventure
    )
}

/// `BlockItem.updateBlockStateFromTag`: the `block_state` component sets properties.
pub(crate) fn apply_block_state_component(
    stack: &ItemStack,
    state: BlockState,
) -> Option<BlockState> {
    let data = stack.component(ids::BLOCK_STATE)?;
    let mut r = Reader::new(data);
    let n = r.varint().ok()?;
    let mut out = state;
    for _ in 0..n.clamp(0, 64) {
        let key = r.string(32767).ok()?;
        let value = r.string(32767).ok()?;
        if let Some(s) = out.with(key, value) {
            out = s;
        }
    }
    (out != state).then_some(out)
}

/// `Block.dropResources` + `popResource`: loot table drops as item entities.
fn drop_resources(
    ctx: &mut PlayContext<'_>,
    state: BlockState,
    pos: BlockPos,
    tool: &ItemStack,
    entity: Option<&crate::level::BlockEntity>,
) {
    let mut stacks = Vec::new();
    {
        let data = ctx.data;
        let enchantment_level = |name: &str| data.enchantment_level(tool, name);
        let item_in_tag = |item: u16, tag: &str| data.item_in_tag(item, tag);
        let components =
            |id: u16| entity.and_then(|e| crate::level::block_entity::collect_component(e, id));
        let loot = BlockLootContext {
            state,
            tool,
            enchantment_level: &enchantment_level,
            item_in_tag: &item_in_tag,
            explosion_radius: None,
            this_present: true,
            entity: None,
            block_entity: Some(&components),
        };
        let mut rng = LevelRandom(&mut *ctx.level);
        data.loot.block_drops(&loot, &mut rng, &mut stacks);
    }
    for stack in stacks {
        pop_resource(ctx.level, pos, stack);
    }
}

/// The world's random source for loot rolls.
struct LevelRandom<'a>(&'a mut dyn PlayLevel);

impl crate::loot::LootRandom for LevelRandom<'_> {
    fn next_int(&mut self, bound: i32) -> i32 {
        self.0.random_int(bound.max(1))
    }

    fn next_float(&mut self) -> f32 {
        self.0.random_float()
    }
}

/// `Block.popResource`.
pub fn pop_resource(level: &mut dyn PlayLevel, pos: BlockPos, stack: ItemStack) {
    if stack.is_empty() {
        return;
    }
    let half_height = 0.125;
    let offset = |level: &mut dyn PlayLevel| level.random_double() * 0.5 - 0.25;
    let x = f64::from(pos.x) + 0.5 + offset(level);
    let y = f64::from(pos.y) + 0.5 + offset(level) - half_height;
    let z = f64::from(pos.z) + 0.5 + offset(level);
    let velocity = Vec3::new(
        level.random_double() * 0.2 - 0.1,
        0.2,
        level.random_double() * 0.2 - 0.1,
    );
    level.spawn_item(Vec3::new(x, y, z), stack, velocity, 10, None);
}

/// `LivingEntity.createItemStackToDrop(stack, false, thrown)`: a stack thrown from the eye
/// position in look direction.
pub fn throw_item(level: &mut dyn PlayLevel, actor: &Actor, drop: Drop) {
    if drop.stack.is_empty() {
        return;
    }
    let y = actor.pos.y + actor.eye_height - 0.3;
    let deg = std::f64::consts::PI / 180.0;
    let sin_x = sin(f64::from(actor.x_rot) * deg);
    let cos_x = cos(f64::from(actor.x_rot) * deg);
    let sin_y = sin(f64::from(actor.y_rot) * deg);
    let cos_y = cos(f64::from(actor.y_rot) * deg);
    let dir = level.random_float() * std::f32::consts::TAU;
    let pow2 = 0.02 * level.random_float();
    let spread = level.random_float() - level.random_float();
    let velocity = Vec3::new(
        f64::from(-sin_y * cos_x * 0.3) + f64::from(dir).cos() * f64::from(pow2),
        f64::from(-sin_x * 0.3 + 0.1 + spread * 0.1),
        f64::from(cos_y * cos_x * 0.3) + f64::from(dir).sin() * f64::from(pow2),
    );
    let thrower = drop.thrown.then_some(actor.uuid);
    level.spawn_item(
        Vec3::new(actor.pos.x, y, actor.pos.z),
        drop.stack,
        velocity,
        40,
        thrower,
    );
}

/// `floor` of a position.
pub fn block_pos_of(v: Vec3) -> BlockPos {
    BlockPos::new(floor(v.x), floor(v.y), floor(v.z))
}

#[cfg(test)]
#[path = "play_tests.rs"]
mod tests;

/// The Vanilla game event ids.
fn events_ids() -> &'static crate::level::game_event::GameEvents {
    crate::level::game_event::events()
}
