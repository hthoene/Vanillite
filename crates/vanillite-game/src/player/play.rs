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
use crate::block::place::{self, BlockReader, PlaceContext};
use crate::inventory::{Drop, PlayerInventory};
use crate::item::{self, ItemStack, ToolTable};
use crate::level::update;
use crate::loot::{BlockLootContext, LootTables};
use crate::player::world::{PlayLevel, SoundSource, events};

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
    /// The definitions of the enchantments by name.
    pub enchantment_info: HashMap<String, EnchantmentInfo>,
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
}

/// The acting player.
#[derive(Clone, Copy, Debug)]
pub struct Actor {
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
            drop_resources(ctx, state, pos, &tool);
        }
        true
    }

    /// `ItemStack.mineBlock`: tools lose durability.
    fn mine_block(&mut self, ctx: &mut PlayContext<'_>, state: BlockState) {
        let damage = {
            let tool = ctx.inventory.selected_item();
            if tool.is_empty() || state.misc().destroy_speed == 0.0 {
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
            tool.shrink(1);
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
        _sink: &dyn PacketSink,
    ) -> bool {
        if ctx.actor.game_mode == GameMode::Spectator {
            return false;
        }
        let state = ctx.level.block(pos);
        let has_items =
            !ctx.inventory.selected_item().is_empty() || !ctx.inventory.offhand.is_empty();
        let suppress_use = ctx.actor.sneaking && has_items;
        if !suppress_use && has_use_action(state) {
            return false;
        }
        let stack = match hand {
            Hand::MainHand => ctx.inventory.selected_item().clone(),
            Hand::OffHand => ctx.inventory.offhand.clone(),
        };
        if stack.is_empty() {
            return false;
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
        self.place_block(ctx, hand, &stack, &place_ctx)
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
        if !ctx.level.set_block(target, state, update::ALL_IMMEDIATE) {
            return false;
        }
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

/// `blockActionRestricted`: spectators never, adventure players only with `can_break`
/// (not evaluated yet, so never).
fn block_action_restricted(ctx: &PlayContext<'_>) -> bool {
    matches!(
        ctx.actor.game_mode,
        GameMode::Spectator | GameMode::Adventure
    )
}

/// `BlockItem.updateBlockStateFromTag`: the `block_state` component sets properties.
fn apply_block_state_component(stack: &ItemStack, state: BlockState) -> Option<BlockState> {
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
fn drop_resources(ctx: &mut PlayContext<'_>, state: BlockState, pos: BlockPos, tool: &ItemStack) {
    let mut stacks = Vec::new();
    {
        let data = ctx.data;
        let enchantment_level = |name: &str| data.enchantment_level(tool, name);
        let item_in_tag = |item: u16, tag: &str| data.item_in_tag(item, tag);
        let loot = BlockLootContext {
            state,
            tool,
            enchantment_level: &enchantment_level,
            item_in_tag: &item_in_tag,
            explosion_radius: None,
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
