//! Container menus (`ChestMenu`, `DispenserMenu`, `HopperMenu`, `ShulkerBoxMenu`,
//! `CraftingMenu`, `AbstractFurnaceMenu`): opening, closing and the slots over a block's
//! container, the player's ender chest or the menu's own crafting grid.
//!
//! A block asks for a menu with a [`MenuRequest`]; [`open`] turns it into the player's
//! [`ContainerMenu`] (`ServerPlayer.openMenu`). While it is open, [`ContainerView`] gives the
//! click code ([`super::click`]) its slots: the container's first, then the main inventory
//! (27) and the hotbar (9), like `addStandardInventorySlots`.

use vanillite_base::{BlockPos, Vec3};
use vanillite_data::block::BlockId;
use vanillite_data::item::ids as items;
use vanillite_protocol::packets::play::clientbound::{ContainerCloseClientbound, OpenScreen};
use vanillite_protocol::text::Component;
use vanillite_world::entity::EntityId;

use super::{ClickContext, MenuSlots, MenuState};
use crate::block::container as blocks;
use crate::inventory::{Drop, PlayerInventory};
use crate::item::{self, ItemStack};
use crate::level::{BlockEntity, Level};
use crate::player::PacketSink;
use crate::recipe::{CookingKind, CraftingInput};

/// The menu types this server opens (`MenuType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuKind {
    /// `generic_9xN`: chests, barrels, ender chests.
    Generic {
        /// Rows of nine slots (3 or 6).
        rows: u8,
    },
    /// `generic_3x3`: dispensers and droppers.
    Dispenser,
    /// `hopper`.
    Hopper,
    /// `shulker_box`.
    ShulkerBox,
    /// `crafting`: the crafting table.
    Crafting,
    /// `furnace`, `blast_furnace`, `smoker`.
    Furnace(CookingKind),
    /// `stonecutter`.
    Stonecutter,
    /// `anvil`.
    Anvil,
    /// `grindstone`.
    Grindstone,
    /// `smithing`.
    Smithing,
    /// `brewing_stand`.
    BrewingStand,
    /// `enchantment`: the enchanting table.
    Enchantment,
    /// `loom`.
    Loom,
    /// `cartography_table`.
    CartographyTable,
    /// `crafter_3x3`: the grid of a crafter block entity and a result display slot after the
    /// player's inventory.
    Crafter,
    /// `lectern`: the book of a lectern, without the player's inventory.
    Lectern,
}

impl MenuKind {
    /// The `minecraft:menu` registry id.
    pub fn type_id(self) -> i32 {
        let name = match self {
            MenuKind::Generic { rows } => match rows {
                1 => "minecraft:generic_9x1",
                2 => "minecraft:generic_9x2",
                3 => "minecraft:generic_9x3",
                4 => "minecraft:generic_9x4",
                5 => "minecraft:generic_9x5",
                _ => "minecraft:generic_9x6",
            },
            MenuKind::Dispenser => "minecraft:generic_3x3",
            MenuKind::Hopper => "minecraft:hopper",
            MenuKind::ShulkerBox => "minecraft:shulker_box",
            MenuKind::Crafting => "minecraft:crafting",
            MenuKind::Furnace(CookingKind::Blasting) => "minecraft:blast_furnace",
            MenuKind::Furnace(CookingKind::Smoking) => "minecraft:smoker",
            MenuKind::Furnace(_) => "minecraft:furnace",
            MenuKind::Stonecutter => "minecraft:stonecutter",
            MenuKind::Anvil => "minecraft:anvil",
            MenuKind::Grindstone => "minecraft:grindstone",
            MenuKind::Smithing => "minecraft:smithing",
            MenuKind::BrewingStand => "minecraft:brewing_stand",
            MenuKind::Enchantment => "minecraft:enchantment",
            MenuKind::Loom => "minecraft:loom",
            MenuKind::CartographyTable => "minecraft:cartography_table",
            MenuKind::Crafter => "minecraft:crafter_3x3",
            MenuKind::Lectern => "minecraft:lectern",
        };
        vanillite_data::registry::MENU.id(name).map_or(0, i32::from)
    }

    /// Slots before the player's inventory.
    pub fn container_slots(self) -> usize {
        match self {
            MenuKind::Generic { rows } => usize::from(rows) * 9,
            MenuKind::Dispenser => 9,
            MenuKind::Hopper => 5,
            MenuKind::ShulkerBox => 27,
            MenuKind::Crafting => 10,
            MenuKind::Furnace(_) | MenuKind::Anvil | MenuKind::Grindstone => 3,
            MenuKind::Stonecutter | MenuKind::Enchantment => 2,
            MenuKind::Smithing | MenuKind::Loom => 4,
            MenuKind::BrewingStand => 5,
            MenuKind::CartographyTable => 3,
            MenuKind::Crafter => 9,
            MenuKind::Lectern => 1,
        }
    }

    /// Whether the menu's own slots are a workstation's inputs and result.
    pub fn is_workstation(self) -> bool {
        matches!(
            self,
            MenuKind::Stonecutter
                | MenuKind::Anvil
                | MenuKind::Grindstone
                | MenuKind::Smithing
                | MenuKind::Enchantment
                | MenuKind::Loom
                | MenuKind::CartographyTable
        )
    }

    /// Number of data slots.
    pub fn data_count(self) -> usize {
        match self {
            MenuKind::Furnace(_) | MenuKind::BrewingStand => 4,
            MenuKind::Stonecutter
            | MenuKind::Anvil
            | MenuKind::Smithing
            | MenuKind::Loom
            | MenuKind::Lectern => 1,
            MenuKind::Enchantment | MenuKind::Crafter => 10,
            _ => 0,
        }
    }
}

/// Where a menu's container slots live.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Storage {
    /// The block entity at a position.
    Block(BlockPos),
    /// A double chest: the first half holds slots 0–26.
    Double(BlockPos, BlockPos),
    /// The player's ender chest, opened at an ender chest block.
    Ender(BlockPos),
    /// The menu's own slots (crafting grid and result).
    Local,
}

/// A menu a block asks to open (`MenuProvider`).
#[derive(Clone, Debug, PartialEq)]
pub struct MenuRequest {
    /// The player that used the block.
    pub player: Option<EntityId>,
    /// The menu type.
    pub menu: MenuKind,
    /// Where its slots live.
    pub storage: Storage,
    /// The title.
    pub title: Component,
    /// The block (for `stillValid`).
    pub pos: BlockPos,
    /// The block that opened it.
    pub block: BlockId,
}

/// The player's open container menu (`player.containerMenu`).
#[derive(Clone, Debug)]
pub struct ContainerMenu {
    /// Carried stack and synchronisation.
    pub state: MenuState,
    /// The menu type.
    pub kind: MenuKind,
    /// Where its container slots live.
    pub storage: Storage,
    /// The block it was opened at.
    pub pos: BlockPos,
    /// The block it was opened at.
    pub block: BlockId,
    /// The menu's own slots (crafting result and grid).
    pub local: Vec<ItemStack>,
    /// Crafting results to send right away (`slotChangedCraftingGrid`).
    result_updates: Vec<ItemStack>,
    /// The menu's own data slots (stonecutter selection, anvil cost, smithing error).
    pub data: Vec<i32>,
    /// `AnvilMenu.itemName`.
    pub item_name: Option<String>,
    /// `AnvilMenu.repairItemCountCost`.
    pub repair_item_count_cost: i32,
    /// `AnvilMenu.onlyRenaming`.
    pub only_renaming: bool,
    /// `StonecutterMenu.input`: the input the recipe list was made for.
    pub last_input: ItemStack,
    /// The stonecutter recipes (indices) for the input; the loom's selectable patterns
    /// (`minecraft:banner_pattern` ids).
    pub recipes: Vec<usize>,
    /// The crafter grid the result display was computed for.
    pub(super) crafter_grid: Vec<ItemStack>,
    /// `player.mayBuild()` of the last button click (the lectern's take button).
    pub may_build: bool,
    /// `StonecutterMenu.lastSoundTime`.
    pub last_sound: i64,
}

/// Where a menu slot is stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Source {
    Container(usize),
    Inv(usize),
    /// The crafter's result display (after the inventory).
    CrafterResult,
}

impl ContainerMenu {
    pub(super) fn source(&self, slot: usize) -> Option<Source> {
        let n = self.kind.container_slots();
        if self.kind == MenuKind::Lectern {
            return (slot == 0).then_some(Source::Container(0));
        }
        if self.kind == MenuKind::Crafter && slot == n + 36 {
            return Some(Source::CrafterResult);
        }
        if slot < n {
            Some(Source::Container(slot))
        } else if slot < n + 27 {
            Some(Source::Inv(slot - n + 9))
        } else if slot < n + 36 {
            Some(Source::Inv(slot - n - 27))
        } else {
            None
        }
    }

    /// The menu slot that shows an inventory index (`findSlot(inventory, index)`).
    pub fn slot_of_inventory(&self, index: usize) -> Option<usize> {
        let n = self.kind.container_slots();
        if self.kind == MenuKind::Lectern {
            return None;
        }
        match index {
            0..=8 => Some(n + 27 + index),
            9..=35 => Some(n + index - 9),
            _ => None,
        }
    }

    /// Number of slots.
    pub fn slot_count(&self) -> usize {
        match self.kind {
            MenuKind::Lectern => 1,
            MenuKind::Crafter => 46,
            kind => kind.container_slots() + 36,
        }
    }

    /// The container positions whose viewers this menu counts.
    fn viewed(&self) -> [Option<BlockPos>; 2] {
        match self.storage {
            Storage::Block(p) | Storage::Ender(p) => [Some(p), None],
            Storage::Double(a, b) => [Some(a), Some(b)],
            Storage::Local => [None, None],
        }
    }
}

/// A container menu with the player's inventory and the level: the slots the click code works
/// on.
#[derive(Debug)]
pub struct ContainerView<'a> {
    /// The menu.
    pub menu: &'a mut ContainerMenu,
    /// The player's inventory.
    pub inv: &'a mut PlayerInventory,
    /// The level.
    pub level: &'a mut Level,
    /// The player's position (experience of furnace results).
    pub player_pos: Vec3,
    /// The player's experience (anvil costs).
    pub xp: Option<&'a mut crate::survival::player::Experience>,
}

impl ContainerView<'_> {
    fn container_item(&self, i: usize) -> &ItemStack {
        let empty = &ItemStack::EMPTY;
        if self.menu.kind == MenuKind::Lectern {
            return match self.menu.storage {
                Storage::Block(pos) => match self.level.block_entity(pos) {
                    Some(BlockEntity::Lectern(l)) if i == 0 => &l.book,
                    _ => empty,
                },
                _ => empty,
            };
        }
        match self.menu.storage {
            Storage::Block(pos) => blocks::items(self.level, pos)
                .and_then(|items| items.get(i))
                .unwrap_or(empty),
            Storage::Double(a, b) => {
                let (pos, i) = if i < 27 { (a, i) } else { (b, i - 27) };
                blocks::items(self.level, pos)
                    .and_then(|items| items.get(i))
                    .unwrap_or(empty)
            }
            Storage::Ender(_) => self.inv.ender_chest.get(i).unwrap_or(empty),
            Storage::Local => self.menu.local.get(i).unwrap_or(empty),
        }
    }

    fn container_item_mut(&mut self, i: usize) -> Option<&mut ItemStack> {
        if self.menu.kind == MenuKind::Lectern {
            return None;
        }
        match self.menu.storage {
            Storage::Block(pos) => blocks::items_mut(self.level, pos)?.get_mut(i),
            Storage::Double(a, b) => {
                let (pos, i) = if i < 27 { (a, i) } else { (b, i - 27) };
                blocks::items_mut(self.level, pos)?.get_mut(i)
            }
            Storage::Ender(_) => self.inv.ender_chest.get_mut(i),
            Storage::Local => self.menu.local.get_mut(i),
        }
    }

    /// `slotChangedCraftingGrid` of the crafting table.
    fn craft_changed(&mut self) {
        let result = crate::player::play::game_data().map_or(ItemStack::EMPTY, |d| {
            let grid = self.menu.local.get(1..10).unwrap_or_default();
            let input = CraftingInput::of(3, 3, grid);
            d.recipes
                .crafting_for_in(&input, &|m| crate::map::info(self.level, m).map(|i| i.scale))
                .map(|r| d.recipes.assemble(r, &input))
                .unwrap_or_default()
        });
        if let Some(slot) = self.menu.local.get_mut(0) {
            *slot = result.clone();
        }
        self.menu.state.set_remote(0, &result);
        self.menu.result_updates.push(result);
    }

    /// The furnace block entity of a furnace menu.
    fn furnace(&self) -> Option<&crate::block::furnace::Furnace> {
        match self.menu.storage {
            Storage::Block(pos) => match self.level.block_entity(pos) {
                Some(BlockEntity::Furnace(f)) => Some(f),
                _ => None,
            },
            _ => None,
        }
    }

    fn can_smelt(&self, stack: &ItemStack) -> bool {
        let MenuKind::Furnace(kind) = self.menu.kind else {
            return false;
        };
        crate::player::play::game_data().is_some_and(|d| d.recipes.is_cooking_input(kind, stack))
    }

    fn is_fuel(&self, stack: &ItemStack) -> bool {
        let MenuKind::Furnace(kind) = self.menu.kind else {
            return false;
        };
        let fast = matches!(kind, CookingKind::Blasting | CookingKind::Smoking);
        crate::player::play::game_data().is_some_and(|d| d.recipes.fuel(stack, fast).is_some())
    }

    /// Sends the crafting results computed during a click.
    pub fn flush_result_updates(&mut self, sink: &dyn PacketSink) {
        for item in std::mem::take(&mut self.menu.result_updates) {
            let state_id = self.menu.state.next_state_id();
            sink.send(
                &vanillite_protocol::packets::play::clientbound::ContainerSetSlot {
                    container_id: self.menu.state.container_id,
                    state_id,
                    slot: 0,
                    item,
                },
            );
        }
    }
}

impl MenuSlots for ContainerView<'_> {
    fn state(&mut self) -> &mut MenuState {
        &mut self.menu.state
    }

    fn state_ref(&self) -> &MenuState {
        &self.menu.state
    }

    fn slot_count(&self) -> usize {
        self.menu.slot_count()
    }

    fn item(&self, slot: usize) -> &ItemStack {
        match self.menu.source(slot) {
            Some(Source::Container(i)) => self.container_item(i),
            Some(Source::Inv(i)) => self.inv.get(i),
            Some(Source::CrafterResult) => self.menu.local.first().unwrap_or(&ItemStack::EMPTY),
            None => &ItemStack::EMPTY,
        }
    }

    fn item_mut(&mut self, slot: usize) -> Option<&mut ItemStack> {
        match self.menu.source(slot)? {
            Source::Container(i) => self.container_item_mut(i),
            Source::Inv(i) => self.inv.get_mut(i),
            Source::CrafterResult => None,
        }
    }

    fn set_item(&mut self, slot: usize, stack: ItemStack) {
        match self.menu.source(slot) {
            Some(Source::Container(i)) => {
                if let (MenuKind::Furnace(_), Storage::Block(pos)) =
                    (self.menu.kind, self.menu.storage)
                {
                    if let Some(BlockEntity::Furnace(f)) = self.level.block_entity_mut(pos) {
                        f.set_item(i, stack);
                    }
                    return;
                }
                if let (MenuKind::Crafter, Storage::Block(pos)) =
                    (self.menu.kind, self.menu.storage)
                {
                    if let Some(BlockEntity::Crafter(c)) = self.level.block_entity_mut(pos) {
                        c.set_item(i, stack);
                    }
                    self.crafter_changed();
                    return;
                }
                if self.menu.kind == MenuKind::Lectern {
                    // The lectern's book access ignores `setItem`.
                    return;
                }
                if let Some(target) = self.container_item_mut(i) {
                    *target = stack;
                }
                if self.menu.kind == MenuKind::Crafting && i > 0 {
                    self.craft_changed();
                }
                if self.menu.kind.is_workstation()
                    && Some(i) != super::workstation::result_slot(self.menu.kind)
                {
                    self.workstation_changed();
                }
            }
            Some(Source::Inv(i)) => self.inv.set(i, stack),
            Some(Source::CrafterResult) | None => {}
        }
    }

    fn set_changed(&mut self, slot: usize) {
        if let Some(Source::Container(i)) = self.menu.source(slot)
            && self.menu.kind.is_workstation()
            && Some(i) != super::workstation::result_slot(self.menu.kind)
        {
            self.workstation_changed();
        }
        if let Some(Source::Container(_)) = self.menu.source(slot) {
            match self.menu.storage {
                Storage::Block(pos) => blocks::set_changed(self.level, pos),
                Storage::Double(a, b) => {
                    blocks::set_changed(self.level, a);
                    blocks::set_changed(self.level, b);
                }
                Storage::Ender(_) | Storage::Local => {}
            }
            if self.menu.kind == MenuKind::Crafter {
                self.crafter_changed();
            }
        }
    }

    fn slot_contents_changed(&mut self, slot: usize) {
        if let Some(Source::Container(i)) = self.menu.source(slot)
            && self.menu.kind.is_workstation()
            && Some(i) != super::workstation::result_slot(self.menu.kind)
        {
            self.workstation_changed();
        }
        if self.menu.kind == MenuKind::Crafting
            && matches!(self.menu.source(slot), Some(Source::Container(i)) if i > 0)
        {
            self.craft_changed();
        }
    }

    fn may_place(&self, slot: usize, stack: &ItemStack) -> bool {
        let source = self.menu.source(slot);
        if source == Some(Source::CrafterResult) {
            return false;
        }
        let Some(Source::Container(i)) = source else {
            return true;
        };
        match self.menu.kind {
            // `ShulkerBoxSlot`: `canFitInsideContainerItems`.
            MenuKind::ShulkerBox => !is_shulker_box(stack),
            MenuKind::Crafting => i != 0,
            MenuKind::Furnace(_) => match i {
                0 => true,
                1 => self.is_fuel(stack) || item::item_id(stack) == items::BUCKET,
                _ => false,
            },
            kind if kind.is_workstation() => super::workstation::may_place(kind, i, stack),
            MenuKind::Crafter => !self.crafter_disabled(i),
            // The lectern's book access ignores `setItem`: Vanilla would void the stack.
            MenuKind::Lectern => false,
            MenuKind::BrewingStand => match i {
                0..=2 => crate::block::brewing::is_potion_input(stack),
                3 => crate::block::brewing::is_reagent(stack),
                _ => crate::block::brewing::is_fuel(stack),
            },
            _ => true,
        }
    }

    fn slot_max(&self, slot: usize) -> i32 {
        match (self.menu.kind, self.menu.source(slot)) {
            (MenuKind::BrewingStand, Some(Source::Container(0..=2)))
            | (MenuKind::Enchantment, Some(Source::Container(0)))
            | (MenuKind::Lectern, Some(Source::Container(0))) => 1,
            _ => super::CONTAINER_MAX,
        }
    }

    fn may_pickup(&self, slot: usize) -> bool {
        match self.menu.source(slot) {
            Some(Source::CrafterResult) => false,
            Some(Source::Container(i))
                if self.menu.kind.is_workstation()
                    && Some(i) == super::workstation::result_slot(self.menu.kind) =>
            {
                super::workstation::may_pickup_result(self)
            }
            _ => true,
        }
    }

    fn max_stack(&self, slot: usize, stack: &ItemStack) -> i32 {
        if let (MenuKind::Furnace(_), Some(Source::Container(1))) =
            (self.menu.kind, self.menu.source(slot))
            && item::item_id(stack) == items::BUCKET
        {
            return 1;
        }
        if let (MenuKind::BrewingStand, Some(Source::Container(0..=2)))
        | (MenuKind::Enchantment, Some(Source::Container(0))) =
            (self.menu.kind, self.menu.source(slot))
        {
            return 1;
        }
        self.slot_max(slot).min(item::max_stack_size(stack))
    }

    fn on_take(
        &mut self,
        slot: usize,
        taken: &mut ItemStack,
        ctx: ClickContext,
        drops: &mut Vec<Drop>,
    ) {
        let Some(Source::Container(i)) = self.menu.source(slot) else {
            return;
        };
        match (self.menu.kind, i) {
            (MenuKind::Loom, 3) => self.loom_take(),
            (MenuKind::CartographyTable, 2) => self.cartography_take(taken),
            (kind, i)
                if kind.is_workstation() && Some(i) == super::workstation::result_slot(kind) =>
            {
                self.workstation_take(ctx, drops);
            }
            (MenuKind::Crafting, 0) => {
                // `ResultSlot.checkTakeAchievements` → `onCraftedBy` → `onCraftedPostProcess`.
                crate::map::on_crafted(self.level, taken);
                self.take_crafted(ctx, drops);
            }
            (MenuKind::Furnace(_), 2) => {
                if let Storage::Block(pos) = self.menu.storage {
                    let xp = crate::block::furnace::pop_experience(self.level, pos);
                    let at = self.player_pos;
                    self.level.award_experience(at, xp);
                }
            }
            _ => {}
        }
        self.set_changed(slot);
    }

    fn on_quick_craft(&mut self, slot: usize, _count: i32) {
        if let (MenuKind::Furnace(_), Some(Source::Container(2)), Storage::Block(pos)) =
            (self.menu.kind, self.menu.source(slot), self.menu.storage)
        {
            let xp = crate::block::furnace::pop_experience(self.level, pos);
            let at = self.player_pos;
            self.level.award_experience(at, xp);
        }
    }

    fn quick_move(&mut self, index: usize, ctx: ClickContext, drops: &mut Vec<Drop>) -> ItemStack {
        let original = self.item(index).clone();
        if original.is_empty() {
            return ItemStack::EMPTY;
        }
        let mut stack = original.clone();
        let n = self.menu.kind.container_slots();
        let total = self.slot_count();
        if self.menu.kind.is_workstation() {
            return self.workstation_quick_move(index, ctx, drops);
        }
        match self.menu.kind {
            MenuKind::Lectern => ItemStack::EMPTY,
            MenuKind::Crafter => {
                if index >= 45 {
                    return ItemStack::EMPTY;
                }
                let moved = if index < 9 {
                    super::move_to(self, &mut stack, 9, 45, true)
                } else {
                    super::move_to(self, &mut stack, 0, 9, false)
                };
                if !moved {
                    return ItemStack::EMPTY;
                }
                super::finish_quick_move(self, index, stack, &original, ctx, drops)
            }
            MenuKind::Generic { .. } | MenuKind::Hopper | MenuKind::ShulkerBox => {
                let moved = if index < n {
                    super::move_to(self, &mut stack, n, total, true)
                } else {
                    super::move_to(self, &mut stack, 0, n, false)
                };
                if !moved {
                    return ItemStack::EMPTY;
                }
                // These menus return the original stack even if only part moved.
                if stack.is_empty() {
                    super::set_by_player(self, index, ItemStack::EMPTY);
                } else {
                    if let Some(target) = self.item_mut(index) {
                        *target = stack;
                    }
                    self.set_changed(index);
                }
                original
            }
            MenuKind::Dispenser => {
                let moved = if index < 9 {
                    super::move_to(self, &mut stack, 9, 45, true)
                } else {
                    super::move_to(self, &mut stack, 0, 9, false)
                };
                if !moved {
                    return ItemStack::EMPTY;
                }
                super::finish_quick_move(self, index, stack, &original, ctx, drops)
            }
            MenuKind::Crafting => {
                let moved = if index == 0 {
                    // `item.onCraftedBy(stack, player)` before moving.
                    crate::map::on_crafted(self.level, &mut stack);
                    let moved = super::move_to(self, &mut stack, 10, 46, true);
                    if moved {
                        self.on_quick_craft(index, original.count - stack.count);
                    }
                    moved
                } else if (10..46).contains(&index) {
                    super::move_to(self, &mut stack, 1, 10, false)
                        || if index < 37 {
                            super::move_to(self, &mut stack, 37, 46, false)
                        } else {
                            super::move_to(self, &mut stack, 10, 37, false)
                        }
                } else {
                    super::move_to(self, &mut stack, 10, 46, false)
                };
                if !moved {
                    return ItemStack::EMPTY;
                }
                let result =
                    super::finish_quick_move(self, index, stack.clone(), &original, ctx, drops);
                if index == 0 && !result.is_empty() && !stack.is_empty() {
                    drops.push(Drop {
                        stack,
                        thrown: false,
                    });
                }
                result
            }
            MenuKind::Stonecutter
            | MenuKind::Anvil
            | MenuKind::Grindstone
            | MenuKind::Smithing
            | MenuKind::Enchantment
            | MenuKind::Loom
            | MenuKind::CartographyTable => ItemStack::EMPTY,
            MenuKind::BrewingStand => {
                use crate::block::brewing;
                let moved = if index > 4 {
                    if brewing::is_fuel(&original) {
                        if super::move_to(self, &mut stack, 4, 5, false) {
                            // Vanilla moved part of the slot's own stack and stops the shift
                            // click here: the slot keeps the rest.
                            if let Some(target) = self.item_mut(index) {
                                *target = stack;
                            }
                            self.set_changed(index);
                            return ItemStack::EMPTY;
                        }
                        if brewing::is_reagent(&stack)
                            && !super::move_to(self, &mut stack, 3, 4, false)
                        {
                            return ItemStack::EMPTY;
                        }
                        true
                    } else if brewing::is_reagent(&stack) {
                        super::move_to(self, &mut stack, 3, 4, false)
                    } else if brewing::is_potion_input(&original) {
                        super::move_to(self, &mut stack, 0, 3, false)
                    } else if (5..32).contains(&index) {
                        super::move_to(self, &mut stack, 32, 41, false)
                    } else if (32..41).contains(&index) {
                        super::move_to(self, &mut stack, 5, 32, false)
                    } else {
                        super::move_to(self, &mut stack, 5, 41, false)
                    }
                } else {
                    super::move_to(self, &mut stack, 5, 41, true)
                };
                if !moved {
                    return ItemStack::EMPTY;
                }
                super::finish_quick_move(self, index, stack, &original, ctx, drops)
            }
            MenuKind::Furnace(_) => {
                let moved = if index == 2 {
                    let moved = super::move_to(self, &mut stack, 3, 39, true);
                    if moved {
                        self.on_quick_craft(index, original.count - stack.count);
                    }
                    moved
                } else if index != 1 && index != 0 {
                    if self.can_smelt(&stack) {
                        super::move_to(self, &mut stack, 0, 1, false)
                    } else if self.is_fuel(&stack) {
                        super::move_to(self, &mut stack, 1, 2, false)
                    } else if (3..30).contains(&index) {
                        super::move_to(self, &mut stack, 30, 39, false)
                    } else if (30..39).contains(&index) {
                        super::move_to(self, &mut stack, 3, 30, false)
                    } else {
                        true
                    }
                } else {
                    super::move_to(self, &mut stack, 3, 39, false)
                };
                if !moved {
                    return ItemStack::EMPTY;
                }
                super::finish_quick_move(self, index, stack, &original, ctx, drops)
            }
        }
    }

    fn can_take_for_pick_all(&self, slot: usize) -> bool {
        // `CraftingMenu` and `StonecutterMenu.canTakeItemForPickAll`: not the result.
        !((self.menu.kind == MenuKind::Crafting && slot == 0)
            || (self.menu.kind == MenuKind::Stonecutter && slot == 1)
            || (self.menu.kind == MenuKind::CartographyTable && slot == 2))
    }

    fn inventory(&mut self) -> &mut PlayerInventory {
        self.inv
    }

    fn data_count(&self) -> usize {
        self.menu.kind.data_count()
    }

    fn data(&self, index: usize) -> i32 {
        match self.menu.kind {
            MenuKind::Furnace(_) => self.furnace().map_or(0, |f| f.data(index)),
            MenuKind::BrewingStand => match self.menu.storage {
                Storage::Block(pos) => match self.level.block_entity(pos) {
                    Some(BlockEntity::BrewingStand(b)) => b.data(index),
                    _ => 0,
                },
                _ => 0,
            },
            MenuKind::Crafter => match self.menu.storage {
                Storage::Block(pos) => match self.level.block_entity(pos) {
                    Some(BlockEntity::Crafter(c)) => c.data(index),
                    _ => 0,
                },
                _ => 0,
            },
            MenuKind::Lectern => match self.menu.storage {
                Storage::Block(pos) => match self.level.block_entity(pos) {
                    Some(BlockEntity::Lectern(l)) if index == 0 => l.page,
                    _ => 0,
                },
                _ => 0,
            },
            _ => self.menu.data.get(index).copied().unwrap_or(0),
        }
    }
}

impl ContainerView<'_> {
    /// `clickMenuButton`: whether the button did something.
    pub fn button(&mut self, id: i32, drops: &mut Vec<Drop>) -> bool {
        match self.menu.kind {
            MenuKind::Lectern => self.lectern_button(id, drops),
            _ => self.workstation_button(id),
        }
    }

    /// `ResultSlot.onTake` of the crafting table.
    fn take_crafted(&mut self, ctx: ClickContext, drops: &mut Vec<Drop>) {
        let Some(data) = crate::player::play::game_data() else {
            return;
        };
        let grid: Vec<ItemStack> = self.menu.local.get(1..10).unwrap_or_default().to_vec();
        let input = CraftingInput::of(3, 3, &grid);
        let recipe = data
            .recipes
            .crafting_for_in(&input, &|m| crate::map::info(self.level, m).map(|i| i.scale));
        let remaining = data.recipes.remainders(recipe, &input);
        for y in 0..input.height {
            for x in 0..input.width {
                let slot = x + input.left + (y + input.top) * 3;
                let replacement = remaining
                    .get(x + y * input.width)
                    .cloned()
                    .unwrap_or_default();
                crate::inventory::take_ingredient(self, 1 + slot, replacement, ctx, drops);
            }
        }
    }
}

/// Whether a stack is a shulker box (`ShulkerBoxSlot` and `canFitInsideContainerItems`).
pub fn is_shulker_box(stack: &ItemStack) -> bool {
    item::item_id(stack)
        .block()
        .is_some_and(|b| b.info().kind == vanillite_data::block::BlockKind::ShulkerBoxBlock)
}

/// What opening and closing need to know about the player.
#[derive(Clone, Copy, Debug)]
pub struct Opener {
    /// The player is a spectator (not counted as a viewer, cannot click).
    pub spectator: bool,
    /// The player has infinite materials (creative).
    pub infinite: bool,
    /// The player left: what the menu holds drops instead of going back into the inventory.
    pub disconnected: bool,
}

/// `ServerPlayer.openMenu`: closes the open menu, opens the requested one and sends it.
pub fn open(
    inv: &mut PlayerInventory,
    level: &mut Level,
    request: MenuRequest,
    who: Opener,
    drops: &mut Vec<Drop>,
    sink: &dyn PacketSink,
) {
    if inv.container.is_some() {
        close(inv, level, who, drops, Some(sink));
    }
    inv.container_counter = inv.container_counter % 100 + 1;
    let id = inv.container_counter;
    let kind = request.menu;
    let slots = kind.container_slots() + 36;
    let local = match kind {
        MenuKind::Crafting
        | MenuKind::Stonecutter
        | MenuKind::Anvil
        | MenuKind::Grindstone
        | MenuKind::Smithing
        | MenuKind::Enchantment
        | MenuKind::Loom
        | MenuKind::CartographyTable => vec![ItemStack::EMPTY; kind.container_slots()],
        MenuKind::Crafter => vec![ItemStack::EMPTY],
        _ => Vec::new(),
    };
    let data = match kind {
        MenuKind::Stonecutter | MenuKind::Loom => vec![-1],
        MenuKind::Anvil | MenuKind::Smithing => vec![0],
        MenuKind::Enchantment => vec![0, 0, 0, 0, -1, -1, -1, -1, -1, -1],
        _ => Vec::new(),
    };
    let menu = ContainerMenu {
        state: MenuState::new(id, slots, kind.data_count()),
        kind,
        storage: request.storage,
        pos: request.pos,
        block: request.block,
        local,
        result_updates: Vec::new(),
        data,
        item_name: None,
        repair_item_count_cost: 0,
        only_renaming: false,
        last_input: ItemStack::EMPTY,
        recipes: Vec::new(),
        last_sound: 0,
        crafter_grid: Vec::new(),
        may_build: true,
    };
    // The menu's constructor counts the viewer (`container.startOpen`).
    for pos in menu.viewed().into_iter().flatten() {
        blocks::start_open(level, pos, who.spectator);
    }
    sink.send(&OpenScreen {
        container_id: id,
        menu: kind.type_id(),
        title: request.title,
    });
    inv.container = Some(Box::new(menu));
    with_view(inv, level, Vec3::default(), None, |view| {
        if view.menu.kind == MenuKind::Crafter {
            view.crafter_changed();
        }
        super::send_all(view, sink)
    });
}

/// Runs `f` on the open container menu with the player's inventory and the level.
pub fn with_view<R>(
    inv: &mut PlayerInventory,
    level: &mut Level,
    player_pos: Vec3,
    xp: Option<&mut crate::survival::player::Experience>,
    f: impl FnOnce(&mut ContainerView<'_>) -> R,
) -> Option<R> {
    let mut menu = inv.container.take()?;
    let result = {
        let mut view = ContainerView {
            menu: &mut menu,
            inv,
            level,
            player_pos,
            xp,
        };
        f(&mut view)
    };
    inv.container = Some(menu);
    Some(result)
}

/// `closeContainer` (`sink` given: the server closes it and tells the client) and
/// `doCloseContainer` (the client closed it): `removed` runs, the inventory menu takes over
/// what the client knows of the inventory slots.
pub fn close(
    inv: &mut PlayerInventory,
    level: &mut Level,
    who: Opener,
    drops: &mut Vec<Drop>,
    sink: Option<&dyn PacketSink>,
) {
    let Some(mut menu) = inv.container.take() else {
        return;
    };
    if let Some(sink) = sink {
        sink.send(&ContainerCloseClientbound {
            container_id: menu.state.container_id,
        });
    }
    // `AbstractContainerMenu.removed`: the carried stack goes back.
    let carried = std::mem::take(&mut menu.state.carried);
    give_back(inv, carried, who, drops);
    // `ChestMenu.removed` etc.: one viewer less.
    for pos in menu.viewed().into_iter().flatten() {
        blocks::stop_open(level, pos, who.spectator);
    }
    // `CraftingMenu.removed`: the grid goes back; workstations give their inputs back.
    if menu.kind == MenuKind::Crafting {
        for stack in menu.local.drain(..).skip(1) {
            give_back(inv, stack, who, drops);
        }
    }
    let inputs = super::workstation::result_slot(menu.kind)
        .or((menu.kind == MenuKind::Enchantment).then_some(2));
    if let Some(result) = inputs {
        let inputs: Vec<ItemStack> = menu.local.drain(..).take(result).collect();
        for stack in inputs {
            give_back(inv, stack, who, drops);
        }
    }
    // `inventoryMenu.transferState(containerMenu)`.
    let n = menu.kind.container_slots();
    inv.menu.transfer_from(&menu.state, |slot| {
        let index = match slot {
            9..=35 => slot,
            36..=44 => slot - 36,
            _ => return None,
        };
        match index {
            0..=8 => Some(n + 27 + index),
            _ => Some(n + index - 9),
        }
    });
}

/// `dropOrPlaceInInventory`.
fn give_back(inv: &mut PlayerInventory, stack: ItemStack, who: Opener, drops: &mut Vec<Drop>) {
    if stack.is_empty() {
        return;
    }
    if who.disconnected {
        drops.push(Drop {
            stack,
            thrown: false,
        });
    } else {
        inv.place_back(stack, who.infinite, drops);
    }
}

/// `Player.remove` of a player that left: the open menu closes and the inventory menu's
/// carried stack and crafting grid drop (the player's data is already saved).
pub fn close_on_leave(
    inv: &mut PlayerInventory,
    level: &mut Level,
    spectator: bool,
    drops: &mut Vec<Drop>,
) {
    let who = Opener {
        spectator,
        infinite: false,
        disconnected: true,
    };
    close(inv, level, who, drops, None);
    let carried = std::mem::take(&mut inv.menu.carried);
    give_back(inv, carried, who, drops);
    for i in 0..inv.craft.len() {
        let stack = inv.craft.get_mut(i).map(std::mem::take).unwrap_or_default();
        give_back(inv, stack, who, drops);
    }
}

/// `stillValid`: the block is still there and the player close enough
/// (`isWithinBlockInteractionRange(pos, 4)`).
pub fn still_valid(
    inv: &PlayerInventory,
    level: &Level,
    in_range: &dyn Fn(BlockPos) -> bool,
) -> bool {
    let Some(menu) = &inv.container else {
        return true;
    };
    let block_ok = |pos: BlockPos| -> bool {
        match level.block_entity(pos) {
            Some(
                BlockEntity::Container(_)
                | BlockEntity::Hopper(_)
                | BlockEntity::Furnace(_)
                | BlockEntity::BrewingStand(_)
                | BlockEntity::Crafter(_),
            ) => in_range(pos),
            // `bookAccess.stillValid`: the lectern still has its book.
            Some(BlockEntity::Lectern(l)) => !l.book.is_empty() && in_range(pos),
            _ => false,
        }
    };
    match menu.storage {
        Storage::Block(pos) => block_ok(pos),
        Storage::Double(a, b) => block_ok(a) && block_ok(b),
        Storage::Ender(pos) => {
            matches!(
                level.block_entity(pos),
                Some(BlockEntity::EnderChest { .. })
            ) && in_range(pos)
        }
        Storage::Local => {
            let block = level.block(menu.pos).block();
            let fits = block == menu.block
                || (menu.kind == MenuKind::Anvil
                    && block.info().kind == vanillite_data::block::BlockKind::AnvilBlock);
            fits && in_range(menu.pos)
        }
    }
}
