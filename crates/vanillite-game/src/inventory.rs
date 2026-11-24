//! The player inventory (`Inventory`) and the inventory menu of container 0 (`InventoryMenu`)
//! with Vanilla's click handling and slot synchronisation.
//!
//! Menu slots: 0 crafting result, 1–4 crafting grid, 5–8 armour (head, chest, legs, feet),
//! 9–35 main inventory, 36–44 hotbar, 45 off hand. Inventory indices: 0–8 hotbar, 9–35 main,
//! 36–39 armour (feet, legs, chest, head), 40 off hand.

use vanillite_protocol::packets::play::clientbound::ContainerSetSlot;
use vanillite_protocol::packets::play::serverbound::{ContainerInput, HashedStack};

use crate::item::{self, EquipmentSlot, ItemStack, ToolTable};
use crate::menu::{self, MenuSlots, MenuState};
use crate::player::PacketSink;

pub use crate::menu::ClickContext;

/// Slots of the inventory menu.
pub const MENU_SLOTS: usize = 46;
/// Hotbar size (`Inventory.getSelectionSize`).
pub const HOTBAR: usize = 9;
/// Off hand inventory index.
pub const OFFHAND: usize = 40;
/// Size of the ender chest inventory.
pub const ENDER_CHEST: usize = 27;

/// A stack the player drops (`Player.drop`): thrown from the eye position in look direction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Drop {
    /// The stack.
    pub stack: ItemStack,
    /// Whether the player threw it on purpose (it remembers the thrower).
    pub thrown: bool,
}

/// Where a menu slot is stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlotRef {
    Result,
    Craft(usize),
    Inv(usize),
}

fn slot_ref(menu_slot: usize) -> Option<SlotRef> {
    Some(match menu_slot {
        0 => SlotRef::Result,
        1..=4 => SlotRef::Craft(menu_slot - 1),
        5..=8 => SlotRef::Inv(39 - (menu_slot - 5)),
        9..=35 => SlotRef::Inv(menu_slot),
        36..=44 => SlotRef::Inv(menu_slot - 36),
        45 => SlotRef::Inv(OFFHAND),
        _ => return None,
    })
}

/// The menu slot that shows an inventory index (`findSlot`).
pub fn menu_slot_of(inventory_index: usize) -> Option<usize> {
    Some(match inventory_index {
        0..=8 => inventory_index + 36,
        9..=35 => inventory_index,
        36..=39 => 8 - (inventory_index - 36),
        OFFHAND => 45,
        _ => return None,
    })
}

/// The armour slot of menu slots 5–8.
fn armor_slot(menu_slot: usize) -> Option<EquipmentSlot> {
    match menu_slot {
        5 => Some(EquipmentSlot::Head),
        6 => Some(EquipmentSlot::Chest),
        7 => Some(EquipmentSlot::Legs),
        8 => Some(EquipmentSlot::Feet),
        _ => None,
    }
}

/// A player's inventory and its inventory menu.
#[derive(Clone, Debug)]
pub struct PlayerInventory {
    /// Hotbar (0–8) and main inventory (9–35).
    pub items: [ItemStack; 36],
    /// Feet, legs, chest, head.
    pub armor: [ItemStack; 4],
    /// Off hand.
    pub offhand: ItemStack,
    /// Selected hotbar slot.
    pub selected: usize,
    /// Crafting grid of the inventory menu.
    pub craft: [ItemStack; 4],
    /// Crafting result of the inventory menu.
    pub craft_result: ItemStack,
    /// The ender chest inventory (`PlayerEnderChestContainer`).
    pub ender_chest: [ItemStack; ENDER_CHEST],
    /// The inventory menu's state (carried stack, synchronisation).
    pub menu: MenuState,
    /// The open container menu (`containerMenu` when it is not the inventory menu).
    pub container: Option<Box<menu::container::ContainerMenu>>,
    /// `containerCounter`: the id of the last opened container menu (1–100).
    pub container_counter: i32,
    /// Result slots of the crafting grid to send (`slotChangedCraftingGrid` sends them right
    /// away, in this order).
    pub(crate) result_updates: Vec<ItemStack>,
}

impl Default for PlayerInventory {
    fn default() -> Self {
        PlayerInventory {
            items: std::array::from_fn(|_| ItemStack::EMPTY),
            armor: std::array::from_fn(|_| ItemStack::EMPTY),
            offhand: ItemStack::EMPTY,
            selected: 0,
            craft: std::array::from_fn(|_| ItemStack::EMPTY),
            craft_result: ItemStack::EMPTY,
            ender_chest: std::array::from_fn(|_| ItemStack::EMPTY),
            menu: MenuState::new(0, MENU_SLOTS, 0),
            container: None,
            container_counter: 0,
            result_updates: Vec::new(),
        }
    }
}

impl PlayerInventory {
    // --- Inventory (`Inventory`) ---------------------------------------------------------

    /// The stack at an inventory index (0–40).
    pub fn get(&self, index: usize) -> &ItemStack {
        match index {
            0..=35 => self.items.get(index),
            36..=39 => self.armor.get(index - 36),
            OFFHAND => Some(&self.offhand),
            _ => None,
        }
        .unwrap_or(&ItemStack::EMPTY)
    }

    /// The stack at an inventory index, mutably.
    pub fn get_mut(&mut self, index: usize) -> Option<&mut ItemStack> {
        match index {
            0..=35 => self.items.get_mut(index),
            36..=39 => self.armor.get_mut(index - 36),
            OFFHAND => Some(&mut self.offhand),
            _ => None,
        }
    }

    /// Sets an inventory index.
    pub fn set(&mut self, index: usize, stack: ItemStack) {
        if let Some(slot) = self.get_mut(index) {
            *slot = stack;
        }
    }

    /// The selected hotbar stack (`getSelectedItem`).
    pub fn selected_item(&self) -> &ItemStack {
        self.get(self.selected)
    }

    /// The selected hotbar stack, mutably.
    pub fn selected_item_mut(&mut self) -> Option<&mut ItemStack> {
        self.items.get_mut(self.selected)
    }

    /// The stack in an equipment slot.
    pub fn equipment(&self, slot: EquipmentSlot) -> &ItemStack {
        match slot {
            EquipmentSlot::MainHand => self.selected_item(),
            EquipmentSlot::OffHand => &self.offhand,
            EquipmentSlot::Feet => self.get(36),
            EquipmentSlot::Legs => self.get(37),
            EquipmentSlot::Chest => self.get(38),
            EquipmentSlot::Head => self.get(39),
            EquipmentSlot::Body | EquipmentSlot::Saddle => &ItemStack::EMPTY,
        }
    }

    fn has_remaining_space(slot: &ItemStack, new: &ItemStack) -> bool {
        !slot.is_empty()
            && slot.same_item_same_components(new)
            && item::is_stackable(slot)
            && slot.count < item::max_stack_size(slot).min(menu::CONTAINER_MAX)
    }

    /// `getFreeSlot`.
    pub fn free_slot(&self) -> Option<usize> {
        self.items.iter().position(ItemStack::is_empty)
    }

    /// `getSlotWithRemainingSpace`.
    pub fn slot_with_remaining_space(&self, new: &ItemStack) -> Option<usize> {
        if Self::has_remaining_space(self.selected_item(), new) {
            return Some(self.selected);
        }
        if Self::has_remaining_space(&self.offhand, new) {
            return Some(OFFHAND);
        }
        self.items
            .iter()
            .position(|s| Self::has_remaining_space(s, new))
    }

    /// `addResource(slot, stack)`: returns the count that did not fit.
    fn add_resource_at(&mut self, slot: usize, stack: &ItemStack) -> i32 {
        let count = stack.count;
        let Some(target) = self.get_mut(slot) else {
            return count;
        };
        if target.is_empty() {
            *target = ItemStack {
                count: 0,
                ..stack.clone()
            };
        }
        let max_to_add = item::max_stack_size(target)
            .min(menu::CONTAINER_MAX)
            .saturating_sub(target.count);
        let to_add = count.min(max_to_add).max(0);
        target.grow(to_add);
        count - to_add
    }

    fn add_resource(&mut self, stack: &ItemStack) -> i32 {
        match self
            .slot_with_remaining_space(stack)
            .or_else(|| self.free_slot())
        {
            Some(slot) => self.add_resource_at(slot, stack),
            None => stack.count,
        }
    }

    /// `Inventory.add(slot, stack)`: moves what fits from `stack` into the inventory (`slot`
    /// `None` = anywhere). Returns whether anything was added. With infinite materials a stack
    /// that does not fit is consumed.
    pub fn add(&mut self, slot: Option<usize>, stack: &mut ItemStack, infinite: bool) -> bool {
        if stack.is_empty() {
            return false;
        }
        if item::is_damageable(stack) && item::damage(stack) > 0 {
            let slot = slot.or_else(|| self.free_slot());
            return match slot {
                Some(slot) => {
                    self.set(slot, std::mem::take(stack));
                    true
                }
                None if infinite => {
                    *stack = ItemStack::EMPTY;
                    true
                }
                None => false,
            };
        }
        loop {
            let last = stack.count;
            let rest = match slot {
                Some(slot) => self.add_resource_at(slot, stack),
                None => self.add_resource(stack),
            };
            stack.count = rest;
            if stack.count <= 0 {
                *stack = ItemStack::EMPTY;
                return true;
            }
            if stack.count >= last {
                if infinite {
                    *stack = ItemStack::EMPTY;
                    return true;
                }
                return stack.count < last;
            }
        }
    }

    /// `placeItemBackInInventory`: what does not fit is dropped.
    pub fn place_back(&mut self, mut stack: ItemStack, infinite: bool, drops: &mut Vec<Drop>) {
        while !stack.is_empty() {
            let Some(slot) = self
                .slot_with_remaining_space(&stack)
                .or_else(|| self.free_slot())
            else {
                drops.push(Drop {
                    stack,
                    thrown: false,
                });
                break;
            };
            let space = item::max_stack_size(&stack).saturating_sub(self.get(slot).count);
            let mut part = stack.split(space);
            self.add(Some(slot), &mut part, infinite);
        }
    }

    /// `removeFromSelected`: one item, or the whole stack.
    pub fn remove_from_selected(&mut self, all: bool) -> ItemStack {
        let selected = self.selected;
        let Some(stack) = self.items.get_mut(selected) else {
            return ItemStack::EMPTY;
        };
        if stack.is_empty() {
            return ItemStack::EMPTY;
        }
        let n = if all { stack.count } else { 1 };
        stack.split(n)
    }

    /// `ServerPlayer.drop(all)` (Q / Ctrl+Q): the removed stack; the client already predicted
    /// the slot.
    pub fn drop_selected(&mut self, all: bool) -> ItemStack {
        let removed = self.remove_from_selected(all);
        let current = self.selected_item().clone();
        match &mut self.container {
            Some(open) => {
                if let Some(slot) = open.slot_of_inventory(self.selected) {
                    open.state.set_remote(slot, &current);
                }
            }
            None => {
                if let Some(slot) = menu_slot_of(self.selected) {
                    self.menu.set_remote(slot, &current);
                }
            }
        }
        removed
    }

    /// Swaps main and off hand (`SWAP_ITEM_WITH_OFFHAND`).
    pub fn swap_hands(&mut self) {
        let selected = self.selected;
        if let Some(main) = self.items.get_mut(selected) {
            std::mem::swap(main, &mut self.offhand);
        }
    }

    /// `pickSlot`/`addAndPickItem` helper: `getSuitableHotbarSlot`.
    pub fn suitable_hotbar_slot(&self) -> usize {
        for i in 0..HOTBAR {
            let index = (self.selected + i) % HOTBAR;
            if self.get(index).is_empty() {
                return index;
            }
        }
        for i in 0..HOTBAR {
            let index = (self.selected + i) % HOTBAR;
            if item::enchantments(self.get(index)).is_empty() {
                return index;
            }
        }
        self.selected
    }

    /// `findSlotMatchingItem`.
    pub fn find_matching(&self, stack: &ItemStack) -> Option<usize> {
        self.items
            .iter()
            .position(|s| !s.is_empty() && s.same_item_same_components(stack))
    }

    /// `Player.tryPickItem` with `addAndPickItem`/`pickSlot`: selects or fetches `stack` for
    /// the hotbar. Returns whether the selected slot changed (`set_held_slot` to send).
    pub fn pick_item(&mut self, stack: ItemStack, infinite: bool) -> bool {
        let before = self.selected;
        match self.find_matching(&stack) {
            Some(slot) if slot < HOTBAR => self.selected = slot,
            Some(slot) => {
                self.selected = self.suitable_hotbar_slot();
                self.items.swap(self.selected, slot);
            }
            None if infinite => {
                self.selected = self.suitable_hotbar_slot();
                let selected = self.selected;
                if !self.get(selected).is_empty()
                    && let Some(free) = self.free_slot()
                {
                    self.items.swap(selected, free);
                }
                self.set(selected, stack);
            }
            None => {}
        }
        self.selected != before
    }

    // --- Inventory menu (`InventoryMenu`) ------------------------------------------------

    /// `Slot.mayPickup` of the armour slots: a stack with Curse of Binding stays unless the
    /// player is in creative.
    fn armor_locked(&self, stack: &ItemStack) -> bool {
        !self.menu.creative
            && crate::player::play::game_data()
                .is_some_and(|d| d.enchantment_level(stack, "minecraft:binding_curse") > 0)
    }

    /// `slotChangedCraftingGrid`: the result of the 2×2 grid; the client gets it at once.
    fn craft_changed(&mut self) {
        let result = crate::player::play::game_data().map_or(ItemStack::EMPTY, |d| {
            let input = crate::recipe::CraftingInput::of(2, 2, &self.craft);
            d.recipes
                .crafting_for(&input)
                .map(|r| d.recipes.assemble(r, &input))
                .unwrap_or_default()
        });
        self.craft_result = result.clone();
        self.menu.set_remote(0, &result);
        self.result_updates.push(result);
    }

    /// Sends the crafting results [`Self::craft_changed`] computed
    /// (`ClientboundContainerSetSlotPacket` of slot 0).
    pub fn flush_result_updates(&mut self, sink: &dyn PacketSink) {
        for item in std::mem::take(&mut self.result_updates) {
            let state_id = self.menu.next_state_id();
            sink.send(&ContainerSetSlot {
                container_id: 0,
                state_id,
                slot: 0,
                item,
            });
        }
    }

    /// `ResultSlot.onTake`: one of every ingredient is used up; remainders stay in the grid or
    /// go to the inventory.
    fn take_crafted(&mut self, ctx: ClickContext, drops: &mut Vec<Drop>) {
        let Some(data) = crate::player::play::game_data() else {
            return;
        };
        let input = crate::recipe::CraftingInput::of(2, 2, &self.craft);
        let recipe = data.recipes.crafting_for(&input);
        let remaining = data.recipes.remainders(recipe, &input);
        for y in 0..input.height {
            for x in 0..input.width {
                let slot = x + input.left + (y + input.top) * 2;
                let replacement = remaining
                    .get(x + y * input.width)
                    .cloned()
                    .unwrap_or_default();
                take_ingredient(self, 1 + slot, replacement, ctx, drops);
            }
        }
    }

    /// `handleSetCreativeModeSlot` for a valid menu slot (1–45).
    pub fn set_creative_slot(&mut self, menu_slot: usize, stack: ItemStack) {
        menu::set_by_player(self, menu_slot, stack.clone());
        self.menu.set_remote(menu_slot, &stack);
    }

    /// `InventoryMenu.removed` (closing the inventory): the carried stack and the crafting
    /// grid go back into the inventory or are dropped.
    pub fn close(&mut self, infinite: bool, drops: &mut Vec<Drop>) {
        let carried = std::mem::take(&mut self.menu.carried);
        self.place_back(carried, infinite, drops);
        self.craft_result = ItemStack::EMPTY;
        for i in 0..self.craft.len() {
            let stack = self
                .craft
                .get_mut(i)
                .map(std::mem::take)
                .unwrap_or_default();
            self.place_back(stack, infinite, drops);
        }
    }

    /// `doClick` on the inventory menu.
    pub fn click(
        &mut self,
        slot: i16,
        button: i8,
        input: ContainerInput,
        ctx: ClickContext,
        drops: &mut Vec<Drop>,
    ) {
        menu::click(self, slot, button, input, ctx, drops);
    }

    /// The client's state id.
    pub fn state_id(&self) -> i32 {
        self.menu.state_id()
    }

    /// Takes over the client's predictions of a click.
    pub fn receive_predictions(&mut self, changed: &[(i16, HashedStack)], carried: &HashedStack) {
        menu::receive_predictions(&mut self.menu, changed, carried);
    }

    /// `sendAllDataToRemote`.
    pub fn send_all(&mut self, sink: &dyn PacketSink) {
        self.flush_result_updates(sink);
        menu::send_all(self, sink);
    }

    /// `broadcastChanges`.
    pub fn broadcast_changes(&mut self, sink: &dyn PacketSink) {
        self.flush_result_updates(sink);
        menu::broadcast_changes(self, sink);
    }

    /// `broadcastFullState`.
    pub fn broadcast_full_state(&mut self, sink: &dyn PacketSink) {
        self.flush_result_updates(sink);
        menu::broadcast_full_state(self, sink);
    }
}

/// One cell of `ResultSlot.onTake`: removes one item from crafting slot `slot` of `s` and puts
/// the recipe's remainder for it back into the grid, the inventory or the world.
pub(crate) fn take_ingredient<S: MenuSlots + ?Sized>(
    s: &mut S,
    slot: usize,
    mut replacement: ItemStack,
    ctx: ClickContext,
    drops: &mut Vec<Drop>,
) {
    if !s.item(slot).is_empty() {
        s.remove(slot, 1);
    }
    if replacement.is_empty() {
        return;
    }
    let current = s.item(slot).clone();
    if current.is_empty() {
        s.set_item(slot, replacement);
    } else if current.same_item_same_components(&replacement) {
        replacement.grow(current.count);
        s.set_item(slot, replacement);
    } else if !s.inventory().add(None, &mut replacement, ctx.infinite) || !replacement.is_empty() {
        drops.push(Drop {
            stack: replacement,
            thrown: false,
        });
    }
}

impl MenuSlots for PlayerInventory {
    fn state(&mut self) -> &mut MenuState {
        &mut self.menu
    }

    fn state_ref(&self) -> &MenuState {
        &self.menu
    }

    fn slot_count(&self) -> usize {
        MENU_SLOTS
    }

    fn item(&self, slot: usize) -> &ItemStack {
        match slot_ref(slot) {
            Some(SlotRef::Result) => &self.craft_result,
            Some(SlotRef::Craft(i)) => self.craft.get(i).unwrap_or(&ItemStack::EMPTY),
            Some(SlotRef::Inv(i)) => self.get(i),
            None => &ItemStack::EMPTY,
        }
    }

    fn item_mut(&mut self, slot: usize) -> Option<&mut ItemStack> {
        match slot_ref(slot)? {
            SlotRef::Result => Some(&mut self.craft_result),
            SlotRef::Craft(i) => self.craft.get_mut(i),
            SlotRef::Inv(i) => self.get_mut(i),
        }
    }

    fn set_item(&mut self, slot: usize, stack: ItemStack) {
        match slot_ref(slot) {
            Some(SlotRef::Result) => self.craft_result = stack,
            Some(SlotRef::Craft(i)) => {
                if let Some(s) = self.craft.get_mut(i) {
                    *s = stack;
                }
                self.craft_changed();
            }
            Some(SlotRef::Inv(i)) => self.set(i, stack),
            None => {}
        }
    }

    fn slot_contents_changed(&mut self, slot: usize) {
        if let Some(SlotRef::Craft(_)) = slot_ref(slot) {
            self.craft_changed();
        }
    }

    fn may_place(&self, slot: usize, stack: &ItemStack) -> bool {
        match armor_slot(slot) {
            Some(eq) => item::equipment_slot(stack) == eq,
            None => slot != 0,
        }
    }

    fn may_pickup(&self, slot: usize) -> bool {
        if armor_slot(slot).is_some() {
            let stack = self.item(slot);
            return stack.is_empty() || !self.armor_locked(stack);
        }
        true
    }

    fn slot_max(&self, slot: usize) -> i32 {
        if armor_slot(slot).is_some() {
            1
        } else {
            menu::CONTAINER_MAX
        }
    }

    fn on_take(
        &mut self,
        slot: usize,
        _taken: &ItemStack,
        ctx: ClickContext,
        drops: &mut Vec<Drop>,
    ) {
        if slot == 0 {
            self.take_crafted(ctx, drops);
        }
    }

    fn quick_move(&mut self, index: usize, ctx: ClickContext, drops: &mut Vec<Drop>) -> ItemStack {
        let original = self.item(index).clone();
        if original.is_empty() {
            return ItemStack::EMPTY;
        }
        let mut stack = original.clone();
        let eq = item::equipment_slot(&original);
        let armor_dest = match eq {
            EquipmentSlot::Head => Some(5),
            EquipmentSlot::Chest => Some(6),
            EquipmentSlot::Legs => Some(7),
            EquipmentSlot::Feet => Some(8),
            _ => None,
        };
        let moved = if index == 0 {
            menu::move_to(self, &mut stack, 9, 45, true)
        } else if (1..9).contains(&index) {
            menu::move_to(self, &mut stack, 9, 45, false)
        } else if let Some(pos) = armor_dest.filter(|&p| self.item(p).is_empty()) {
            menu::move_to(self, &mut stack, pos, pos + 1, false)
        } else if eq == EquipmentSlot::OffHand && self.item(45).is_empty() {
            menu::move_to(self, &mut stack, 45, 46, false)
        } else if (9..36).contains(&index) {
            menu::move_to(self, &mut stack, 36, 45, false)
        } else if (36..45).contains(&index) {
            menu::move_to(self, &mut stack, 9, 36, false)
        } else {
            menu::move_to(self, &mut stack, 9, 45, false)
        };
        if !moved {
            return ItemStack::EMPTY;
        }
        let result = menu::finish_quick_move(self, index, stack.clone(), &original, ctx, drops);
        if index == 0 && !result.is_empty() {
            // `player.drop(stack, false)`: what did not fit of the crafted stack.
            if !stack.is_empty() {
                drops.push(Drop {
                    stack,
                    thrown: false,
                });
            }
        }
        result
    }

    fn can_take_for_pick_all(&self, slot: usize) -> bool {
        slot != 0
    }

    fn inventory(&mut self) -> &mut PlayerInventory {
        self
    }
}

/// `Player.getDestroySpeed` without effects and attributes other than the enchantment-driven
/// ones the caller passes: `efficiency` level, whether the player is in water without aqua
/// affinity, and whether it stands on the ground.
pub fn destroy_speed(
    tools: &ToolTable,
    stack: &ItemStack,
    state: vanillite_data::block::BlockState,
    efficiency: i32,
    submerged_penalty: bool,
    on_ground: bool,
) -> f32 {
    let mut speed = tools.destroy_speed(stack, state);
    if speed > 1.0 && efficiency > 0 {
        // `minecraft:efficiency` adds `level² + 1` to `mining_efficiency`.
        let bonus = efficiency.saturating_mul(efficiency).saturating_add(1);
        #[allow(clippy::as_conversions)]
        let bonus = bonus as f32;
        speed += bonus;
    }
    if submerged_penalty {
        speed *= 0.2;
    }
    if !on_ground {
        speed /= 5.0;
    }
    speed
}

// --- Persistence -------------------------------------------------------------------------

use vanillite_nbt::{Compound, List, Tag};

/// Key of a stack's component patch in saved data: the patch in the delimited wire form.
/// Vanilla readers ignore it and see the item and count.
const PATCH_KEY: &str = "vanillite:patch";

/// Equipment keys of Vanilla's `equipment` compound with their inventory index.
const EQUIPMENT_KEYS: [(&str, usize); 5] = [
    ("feet", 36),
    ("legs", 37),
    ("chest", 38),
    ("head", 39),
    ("offhand", OFFHAND),
];

/// A stack as saved NBT (`ItemStack.CODEC`: id, count and the patch).
pub fn stack_to_nbt(stack: &ItemStack) -> Compound {
    let mut c = Compound::new();
    c.insert(
        "id",
        vanillite_data::registry::ITEM
            .name(stack.item)
            .unwrap_or("minecraft:air"),
    );
    c.insert("count", stack.count);
    if stack.patch().is_some() {
        let mut bytes = Vec::new();
        stack.encode_untrusted(&mut bytes);
        // Drop the count and item id: only the patch is stored.
        let mut r = vanillite_protocol::codec::Reader::new(&bytes);
        if r.varint().is_ok() && r.varint().is_ok() {
            let patch: Vec<i8> = r
                .take_rest()
                .iter()
                .map(|b| i8::from_ne_bytes([*b]))
                .collect();
            c.insert(PATCH_KEY, patch);
        }
    }
    c
}

/// A stack from saved NBT.
pub fn stack_from_nbt(c: &Compound) -> ItemStack {
    let Some(id) = c
        .get("id")
        .and_then(Tag::as_str)
        .and_then(|n| vanillite_data::registry::ITEM.id(n))
    else {
        return ItemStack::EMPTY;
    };
    let count = c
        .get("count")
        .and_then(Tag::as_i64)
        .and_then(|v| i32::try_from(v).ok())
        .unwrap_or(1);
    let plain = ItemStack::new(id, count.clamp(1, 99));
    let Some(Tag::ByteArray(patch)) = c.get(PATCH_KEY) else {
        return plain;
    };
    let mut bytes = Vec::with_capacity(patch.len() + 8);
    vanillite_protocol::codec::write_varint(&mut bytes, plain.count);
    vanillite_protocol::codec::write_varint(&mut bytes, i32::from(id));
    bytes.extend(patch.iter().map(|b| b.to_ne_bytes()[0]));
    let mut r = vanillite_protocol::codec::Reader::new(&bytes);
    ItemStack::decode_untrusted(&mut r, &vanillite_protocol::item::InstalledRegistries)
        .unwrap_or(plain)
}

impl PlayerInventory {
    /// Writes Vanilla's `Inventory`, `SelectedItemSlot` and `equipment` into `out`.
    pub fn save(&self, out: &mut Compound) {
        let mut list = List::new();
        for (i, stack) in self.items.iter().enumerate() {
            if !stack.is_empty() {
                let mut c = stack_to_nbt(stack);
                c.insert("Slot", i8::try_from(i).unwrap_or(0));
                list.0.push(Tag::Compound(c));
            }
        }
        out.insert("Inventory", list);
        out.insert(
            "SelectedItemSlot",
            i32::try_from(self.selected).unwrap_or(0),
        );
        let mut equipment = Compound::new();
        for (key, index) in EQUIPMENT_KEYS {
            let stack = self.get(index);
            if !stack.is_empty() {
                equipment.insert(key, stack_to_nbt(stack));
            }
        }
        out.insert("equipment", equipment);
        let mut ender = Compound::new();
        crate::block::container::save_items(&self.ender_chest, &mut ender);
        out.insert(
            "EnderItems",
            ender
                .get("Items")
                .cloned()
                .unwrap_or(Tag::List(List::new())),
        );
    }

    /// Reads what [`Self::save`] wrote (or Vanilla's data: stacks without their components).
    pub fn load(data: &Compound) -> PlayerInventory {
        let mut inv = PlayerInventory::default();
        if let Some(Tag::List(list)) = data.get("Inventory") {
            for tag in &list.0 {
                let Some(c) = tag.as_compound() else {
                    continue;
                };
                let slot = c.get("Slot").and_then(Tag::as_i64).unwrap_or(-1);
                if let Ok(slot) = usize::try_from(slot)
                    && slot < 36
                {
                    inv.set(slot, stack_from_nbt(c));
                }
            }
        }
        inv.selected = data
            .get("SelectedItemSlot")
            .and_then(Tag::as_i64)
            .and_then(|v| usize::try_from(v).ok())
            .filter(|&v| v < HOTBAR)
            .unwrap_or(0);
        if let Some(equipment) = data.get("equipment").and_then(Tag::as_compound) {
            for (key, index) in EQUIPMENT_KEYS {
                if let Some(c) = equipment.get(key).and_then(Tag::as_compound) {
                    inv.set(index, stack_from_nbt(c));
                }
            }
        }
        if let Some(list @ Tag::List(_)) = data.get("EnderItems") {
            let mut ender = Compound::new();
            ender.insert("Items", list.clone());
            crate::block::container::load_items(&ender, &mut inv.ender_chest);
        }
        inv
    }
}
