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
use vanillite_protocol::packets::play::clientbound::{
    ContainerCloseClientbound, MountScreenOpen, OpenScreen,
};
use vanillite_protocol::text::Component;
use vanillite_world::entity::EntityId;

use super::{ClickContext, MenuSlots, MenuState};
use crate::advancements::award::Award;
use crate::advancements::trigger::Event;
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
    /// `beacon`: the payment slot (the menu's own) and the beacon's levels and powers.
    Beacon,
    /// `merchant`: two payment slots and the result of a villager's or wandering trader's
    /// offer (the menu's own slots).
    Merchant,
    /// The inventory of a horse, llama, camel or nautilus (`HorseInventoryMenu`,
    /// `NautilusInventoryMenu`): saddle, body armour, then the chest columns (three rows each).
    /// It has no `minecraft:menu` type: `mount_screen_open` opens it.
    Mount {
        /// `inventoryColumns`.
        columns: u8,
    },
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
            MenuKind::Beacon => "minecraft:beacon",
            MenuKind::Merchant => "minecraft:merchant",
            MenuKind::Mount { .. } => return -1,
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
            MenuKind::Merchant => 3,
            MenuKind::Stonecutter | MenuKind::Enchantment => 2,
            MenuKind::Smithing | MenuKind::Loom => 4,
            MenuKind::BrewingStand => 5,
            MenuKind::CartographyTable => 3,
            MenuKind::Crafter => 9,
            MenuKind::Lectern | MenuKind::Beacon => 1,
            MenuKind::Mount { columns } => 2 + usize::from(columns) * 3,
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
            MenuKind::Beacon => 3,
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
    /// A container entity: a chest or hopper minecart, a chest boat or raft.
    Entity(EntityId),
    /// The merchant (villager or wandering trader) of a merchant menu; the slots are the
    /// menu's own.
    Merchant(EntityId),
    /// The mount of a mount inventory menu: its saddle and body slots and its chest.
    Mount(EntityId),
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
    /// The merchant menu's state.
    pub merchant: super::merchant::MerchantMenu,
    /// A furnace result was quick-moved: the following `onTake` counts nothing
    /// (`FurnaceResultSlot.removeCount` is spent).
    pub(super) quick_crafted: bool,
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
            Storage::Local | Storage::Entity(_) | Storage::Merchant(_) | Storage::Mount(_) => {
                [None, None]
            }
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
        if self.menu.kind == MenuKind::Beacon {
            return self.menu.local.get(i).unwrap_or(empty);
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
            Storage::Local | Storage::Merchant(_) => self.menu.local.get(i).unwrap_or(empty),
            Storage::Mount(id) => self.mount_item(id, i),
            Storage::Entity(id) => self
                .level
                .entities
                .container_items(id)
                .and_then(|items| items.get(i))
                .unwrap_or(empty),
        }
    }

    fn container_item_mut(&mut self, i: usize) -> Option<&mut ItemStack> {
        if self.menu.kind == MenuKind::Lectern {
            return None;
        }
        if self.menu.kind == MenuKind::Beacon {
            return self.menu.local.get_mut(i);
        }
        match self.menu.storage {
            Storage::Block(pos) => blocks::items_mut(self.level, pos)?.get_mut(i),
            Storage::Double(a, b) => {
                let (pos, i) = if i < 27 { (a, i) } else { (b, i - 27) };
                blocks::items_mut(self.level, pos)?.get_mut(i)
            }
            Storage::Ender(_) => self.inv.ender_chest.get_mut(i),
            Storage::Local | Storage::Merchant(_) => self.menu.local.get_mut(i),
            Storage::Entity(id) => self.level.entities.container_items_mut(id)?.get_mut(i),
            Storage::Mount(id) => self.mount_item_mut(id, i),
        }
    }

    /// `slotChangedCraftingGrid` of the crafting table.
    fn craft_changed(&mut self) {
        let result = crate::player::play::game_data().map_or(ItemStack::EMPTY, |d| {
            let grid = self.menu.local.get(1..10).unwrap_or_default();
            let input = CraftingInput::of(3, 3, grid);
            d.recipes
                .crafting_for_in(&input, &|m| {
                    crate::map::info(self.level, m).map(|i| i.scale)
                })
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
        if self.menu.kind == MenuKind::Merchant {
            self.flush_merchant(sink);
        }
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
                if self.menu.kind == MenuKind::Merchant {
                    self.merchant_set_item(i, stack);
                    return;
                }
                if let Storage::Mount(id) = self.menu.storage {
                    self.mount_set_item(id, i, stack);
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

    fn before_set_by_player(&mut self, slot: usize, stack: &ItemStack) {
        if let (Storage::Mount(id), Some(Source::Container(i))) =
            (self.menu.storage, self.menu.source(slot))
        {
            self.mount_before_set_by_player(id, i, stack);
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
                Storage::Ender(_)
                | Storage::Local
                | Storage::Entity(_)
                | Storage::Merchant(_)
                | Storage::Mount(_) => {}
            }
            if self.menu.kind == MenuKind::Crafter {
                self.crafter_changed();
            }
            // `MerchantContainer.setChanged`.
            if self.menu.kind == MenuKind::Merchant {
                self.merchant_update_sell();
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

    fn remove(&mut self, slot: usize, amount: i32) -> ItemStack {
        if self.menu.kind == MenuKind::Merchant
            && let Some(Source::Container(i)) = self.menu.source(slot)
        {
            return self.merchant_remove(i, amount);
        }
        let taken = self
            .item_mut(slot)
            .map(|s| s.split(amount))
            .unwrap_or_default();
        if !taken.is_empty() {
            self.slot_contents_changed(slot);
        }
        taken
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
            // `MerchantResultSlot.mayPlace`.
            MenuKind::Merchant => i != 2,
            // `BeaconMenu.PaymentSlot`.
            MenuKind::Beacon => crate::player::play::game_data()
                .is_some_and(|d| d.item_in_tag(stack.item, "minecraft:beacon_payment_items")),
            MenuKind::Mount { .. } => match self.menu.storage {
                Storage::Mount(id) => self.mount_may_place(id, i, stack),
                _ => false,
            },
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
            | (MenuKind::Lectern, Some(Source::Container(0)))
            | (MenuKind::Beacon, Some(Source::Container(0)))
            | (MenuKind::Mount { .. }, Some(Source::Container(0..=1))) => 1,
            _ => super::CONTAINER_MAX,
        }
    }

    fn may_pickup(&self, slot: usize) -> bool {
        match self.menu.source(slot) {
            // Only the take button (with `mayBuild`) takes the book: Vanilla's slot would hand
            // a modified client the book (in adventure mode too) and its swap shares the
            // instance with the lectern.
            Some(Source::CrafterResult) => false,
            Some(Source::Container(_)) if self.menu.kind == MenuKind::Lectern => false,
            Some(Source::Container(i)) if matches!(self.menu.kind, MenuKind::Mount { .. }) => {
                self.mount_may_pickup(i)
            }
            Some(Source::Container(2)) if self.menu.kind == MenuKind::Merchant => {
                self.merchant_may_pickup_result()
            }
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
        | (MenuKind::Enchantment, Some(Source::Container(0)))
        | (MenuKind::Beacon, Some(Source::Container(0))) =
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
            (MenuKind::Merchant, 2) => self.merchant_take_result(),
            (MenuKind::BrewingStand, 0..=2) => {
                // `PotionSlot.onTake`: `BREWED_POTION` with the potion contents.
                if taken
                    .component(vanillite_protocol::components::ids::POTION_CONTENTS)
                    .is_some()
                {
                    let potion = crate::recipe::potion_of(taken)
                        .and_then(|p| vanillite_data::registry::POTION.name(p))
                        .map(str::to_owned);
                    self.inv
                        .awards
                        .push(Award::Trigger(Event::BrewedPotion(potion)));
                }
            }
            (MenuKind::Loom, 3) => self.loom_take(),
            (MenuKind::CartographyTable, 2) => self.cartography_take(taken),
            (kind, i)
                if kind.is_workstation() && Some(i) == super::workstation::result_slot(kind) =>
            {
                self.workstation_crafted(taken);
                self.workstation_take(ctx, drops);
            }
            (MenuKind::Crafting, 0) => {
                // `ResultSlot.checkTakeAchievements` → `onCraftedBy` → `onCraftedPostProcess`.
                crate::map::on_crafted(self.level, taken);
                self.take_crafted(taken, ctx, drops);
            }
            (MenuKind::Furnace(_), 2) => {
                let count = if std::mem::take(&mut self.menu.quick_crafted) {
                    0
                } else {
                    taken.count
                };
                self.furnace_take(taken.item, count);
            }
            _ => {}
        }
        self.set_changed(slot);
    }

    fn on_quick_craft(&mut self, slot: usize, count: i32) {
        if self.menu.kind == MenuKind::Merchant
            && self.menu.source(slot) == Some(Source::Container(2))
        {
            self.menu.merchant.remove_count += count;
        }
        if let (MenuKind::Furnace(_), Some(Source::Container(2))) =
            (self.menu.kind, self.menu.source(slot))
        {
            let item = self.item(slot).item;
            self.furnace_take(item, count);
            self.menu.quick_crafted = true;
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
            MenuKind::Merchant => self.merchant_quick_move(index, ctx, drops),
            MenuKind::Mount { .. } => self.mount_quick_move(index, ctx, drops),
            MenuKind::Beacon => {
                let payment_free = self.item(0).is_empty();
                let moved = if index == 0 {
                    super::move_to(self, &mut stack, 1, 37, true)
                } else if payment_free && self.may_place(0, &stack) && stack.count == 1 {
                    super::move_to(self, &mut stack, 0, 1, false)
                } else if (1..28).contains(&index) {
                    super::move_to(self, &mut stack, 28, 37, false)
                } else if (28..37).contains(&index) {
                    super::move_to(self, &mut stack, 1, 28, false)
                } else {
                    super::move_to(self, &mut stack, 1, 37, false)
                };
                if !moved {
                    return ItemStack::EMPTY;
                }
                super::finish_quick_move(self, index, stack, &original, ctx, drops)
            }
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
        // `CraftingMenu` and `StonecutterMenu.canTakeItemForPickAll`: not the result;
        // `MerchantMenu`: no slot.
        !((self.menu.kind == MenuKind::Crafting && slot == 0)
            || self.menu.kind == MenuKind::Merchant
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
            MenuKind::Beacon => match self.menu.storage {
                Storage::Block(pos) => match self.level.block_entity(pos) {
                    Some(BlockEntity::Beacon(b)) => b.data(index),
                    _ => 0,
                },
                _ => 0,
            },
            _ => self.menu.data.get(index).copied().unwrap_or(0),
        }
    }
}

impl ContainerView<'_> {
    /// `BeaconMenu.updateEffects`: new powers paid with the payment slot's item.
    pub fn update_beacon(&mut self, primary: Option<i32>, secondary: Option<i32>) {
        let Storage::Block(pos) = self.menu.storage else {
            return;
        };
        let Some(payment) = self.menu.local.get_mut(0) else {
            return;
        };
        crate::block::beacon::update_effects(self.level, pos, payment, primary, secondary);
    }

    /// `clickMenuButton`: whether the button did something.
    pub fn button(&mut self, id: i32, drops: &mut Vec<Drop>) -> bool {
        match self.menu.kind {
            MenuKind::Lectern => self.lectern_button(id, drops),
            _ => self.workstation_button(id),
        }
    }

    /// `FurnaceResultSlot.checkTakeAchievements`: `onCraftedBy` (the crafted statistic), then
    /// `awardUsedRecipesAndPopExperience`: the experience, the used recipes unlocked and
    /// `recipe_crafted` for each with the furnace's items.
    fn furnace_take(&mut self, item: u16, count: i32) {
        if count > 0 && item != 0 {
            let stat = crate::stats::Stat::of(crate::stats::StatType::Crafted, item);
            self.inv.awards.push(Award::Stat(stat, count));
        }
        let Storage::Block(pos) = self.menu.storage else {
            return;
        };
        let (used, items) = match self.level.block_entity(pos) {
            Some(BlockEntity::Furnace(f)) => (
                f.recipes_used
                    .iter()
                    .map(|(id, _)| id.clone())
                    .collect::<Vec<_>>(),
                f.items.to_vec(),
            ),
            _ => return,
        };
        let known: Vec<String> = crate::player::play::game_data().map_or_else(Vec::new, |d| {
            used.into_iter()
                .filter(|id| d.recipes.cooking.iter().any(|r| r.id == *id))
                .collect()
        });
        let xp = crate::block::furnace::pop_experience(self.level, pos);
        let at = self.player_pos;
        self.level.award_experience(at, xp);
        if !known.is_empty() {
            self.inv.awards.push(Award::Recipes(known.clone()));
            for id in known {
                self.inv
                    .awards
                    .push(Award::Trigger(Event::RecipeCrafted(id, items.clone())));
            }
        }
    }

    /// `ResultSlot.onTake` of the crafting table.
    fn take_crafted(&mut self, taken: &ItemStack, ctx: ClickContext, drops: &mut Vec<Drop>) {
        let Some(data) = crate::player::play::game_data() else {
            return;
        };
        let grid: Vec<ItemStack> = self.menu.local.get(1..10).unwrap_or_default().to_vec();
        let input = CraftingInput::of(3, 3, &grid);
        let recipe = data.recipes.crafting_for_in(&input, &|m| {
            crate::map::info(self.level, m).map(|i| i.scale)
        });
        self.inv
            .crafted
            .push(crate::inventory::crafted(taken, recipe, grid.clone()));
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
    // `RandomizableContainerBlockEntity.canOpen`: spectators cannot open containers with a
    // loot table; `createMenu` rolls the tables for the player (both halves of a chest).
    let positions = match request.storage {
        Storage::Block(p) => [Some(p), None],
        Storage::Double(a, b) => [Some(a), Some(b)],
        Storage::Ender(_)
        | Storage::Local
        | Storage::Entity(_)
        | Storage::Merchant(_)
        | Storage::Mount(_) => [None, None],
    };
    if who.spectator
        && positions
            .iter()
            .flatten()
            .any(|&p| crate::block::chest_loot::has_loot(level, p))
    {
        return;
    }
    for p in positions.into_iter().flatten() {
        crate::block::chest_loot::unpack(level, p, request.player);
    }
    // `AbstractMinecartContainer.createMenu` and `AbstractChestBoat.createMenu`: the same
    // for a chest minecart, a hopper minecart and a chest boat.
    if let Storage::Entity(id) = request.storage {
        if who.spectator && crate::block::chest_loot::entity_has_loot(level, id) {
            return;
        }
        crate::block::chest_loot::unpack_entity(level, id, request.player);
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
        MenuKind::Crafter | MenuKind::Beacon => vec![ItemStack::EMPTY],
        MenuKind::Merchant => vec![ItemStack::EMPTY; 3],
        _ => Vec::new(),
    };
    let data = match kind {
        // `DataSlot.standalone()` starts at 0; a changed input sets -1.
        MenuKind::Stonecutter | MenuKind::Loom => vec![0],
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
        quick_crafted: false,
        crafter_grid: Vec::new(),
        may_build: true,
        merchant: super::merchant::MerchantMenu::default(),
    };
    // The menu's constructor counts the viewer (`container.startOpen`).
    for pos in menu.viewed().into_iter().flatten() {
        blocks::start_open(level, pos, who.spectator);
    }
    if let Storage::Mount(mount) = request.storage {
        // `openHorseInventory` / `openNautilusInventory`.
        let MenuKind::Mount { columns } = kind else {
            return;
        };
        let Some(entity_id) = level.entities.store.get(mount).map(|e| e.network_id().0) else {
            return;
        };
        sink.send(&MountScreenOpen {
            container_id: id,
            inventory_columns: i32::from(columns),
            entity_id,
        });
    } else {
        sink.send(&OpenScreen {
            container_id: id,
            menu: kind.type_id(),
            title: request.title,
        });
    }
    inv.container = Some(Box::new(menu));
    with_view(inv, level, Vec3::default(), None, |view| {
        if view.menu.kind == MenuKind::Crafter {
            view.crafter_changed();
        }
        super::send_all(view, sink);
        // `openTradingScreen`: the offers follow the screen (none when there are none).
        if let Storage::Merchant(id) = view.menu.storage
            && let Some(packet) = crate::mob::fam::village::merchant::offers_packet(
                view.level,
                id,
                view.menu.state.container_id,
            )
            && !packet.offers.is_empty()
        {
            sink.send(&packet);
        }
    });
}

/// Opens the menus players asked for at entities (`ServerPlayer.openMenu`), with the
/// entities back in the level; what the closed menus give back and does not fit drops at the
/// player.
pub fn open_requests(level: &mut Level, requests: Vec<MenuRequest>) {
    if requests.is_empty() {
        return;
    }
    let mut players = std::mem::take(&mut level.entities.players);
    let mut drops: Vec<(Vec3, Drop)> = Vec::new();
    for request in requests {
        let Some(player) = request.player else {
            continue;
        };
        let Some(row) = level.entities.store.get(player).map(|e| e.kind_row()) else {
            continue;
        };
        let pos = level
            .entities
            .store
            .get(player)
            .map_or(Vec3::default(), |e| e.pos());
        let Some(p) = players.get_mut(row) else {
            continue;
        };
        let who = Opener {
            spectator: p.game_mode == vanillite_config::GameMode::Spectator,
            infinite: p.game_mode == vanillite_config::GameMode::Creative,
            disconnected: false,
        };
        let sink = std::sync::Arc::clone(&p.connection.sink);
        let mut out = Vec::new();
        open(
            &mut p.inventory,
            level,
            request,
            who,
            &mut out,
            sink.as_ref(),
        );
        drops.extend(out.into_iter().map(|d| (pos, d)));
    }
    level.entities.players = players;
    for (pos, drop) in drops {
        crate::block::drops::spawn_item(level, pos, drop.stack, Vec3::default(), 40, None);
    }
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
    // `MerchantMenu.removed`: the merchant stops trading, the payment goes back (drops when
    // the player left).
    if let Storage::Merchant(id) = menu.storage {
        crate::mob::fam::village::merchant::stop_trading_in_level(level, id);
        for stack in menu.local.drain(..).take(2) {
            give_back(inv, stack, who, drops);
        }
    }
    // `BeaconMenu.removed`: the payment drops.
    if menu.kind == MenuKind::Beacon {
        for stack in menu.local.drain(..) {
            if !stack.is_empty() {
                drops.push(Drop {
                    stack,
                    thrown: false,
                });
            }
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
/// (`isWithinBlockInteractionRange(pos, 4)`); a container entity is still there and the
/// player close enough to its box (`isWithinEntityInteractionRange(box, 4)`).
pub fn still_valid(
    inv: &PlayerInventory,
    level: &Level,
    in_range: &dyn Fn(BlockPos) -> bool,
    in_entity_range: &dyn Fn(&vanillite_base::Aabb) -> bool,
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
                | BlockEntity::Crafter(_)
                | BlockEntity::Beacon(_),
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
        Storage::Entity(id) => {
            level.entities.container_items(id).is_some()
                && level
                    .entities
                    .store
                    .get(id)
                    .is_some_and(|e| in_entity_range(&e.aabb()))
        }
        // `AbstractMountInventoryMenu.stillValid`: the mount's inventory is the one the menu
        // shows (no chest equipped or removed since), it is alive and close enough.
        Storage::Mount(id) => {
            let MenuKind::Mount { columns } = menu.kind else {
                return false;
            };
            level
                .entities
                .mob(id)
                .is_some_and(|m| m.is_alive() && super::mount::columns(m) == columns)
                && level
                    .entities
                    .store
                    .get(id)
                    .is_some_and(|e| in_entity_range(&e.aabb()))
        }
        // `AbstractVillager.stillValid`: the merchant still trades with the player, is alive
        // and close enough.
        Storage::Merchant(id) => {
            crate::mob::fam::village::merchant::still_trading(level, id)
                && level
                    .entities
                    .store
                    .get(id)
                    .is_some_and(|e| in_entity_range(&e.aabb()))
        }
    }
}
