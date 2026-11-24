//! The player inventory (`Inventory`) and the inventory menu of container 0 (`InventoryMenu`)
//! with Vanilla's click handling and slot synchronisation.
//!
//! Menu slots: 0 crafting result, 1–4 crafting grid, 5–8 armour (head, chest, legs, feet),
//! 9–35 main inventory, 36–44 hotbar, 45 off hand. Inventory indices: 0–8 hotbar, 9–35 main,
//! 36–39 armour (feet, legs, chest, head), 40 off hand.

use vanillite_protocol::packets::play::clientbound::{
    ContainerSetContent, ContainerSetSlot, SetCursorItem,
};
use vanillite_protocol::packets::play::serverbound::{ContainerInput, HashedStack};

use crate::item::{self, EquipmentSlot, ItemStack, ToolTable};
use crate::player::PacketSink;

/// Slots of the inventory menu.
pub const MENU_SLOTS: usize = 46;
/// Hotbar size (`Inventory.getSelectionSize`).
pub const HOTBAR: usize = 9;
/// Off hand inventory index.
pub const OFFHAND: usize = 40;
/// Most items of one slot in a container (`Container.DEFAULT_MAX_STACK_SIZE`).
const CONTAINER_MAX: i32 = 99;
/// `AbstractContainerMenu.SLOT_CLICKED_OUTSIDE`.
const CLICKED_OUTSIDE: i16 = -999;

/// A stack the player drops (`Player.drop`): thrown from the eye position in look direction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Drop {
    /// The stack.
    pub stack: ItemStack,
    /// Whether the player threw it on purpose (it remembers the thrower).
    pub thrown: bool,
}

/// What the player's state allows in a click.
#[derive(Clone, Copy, Debug)]
pub struct ClickContext {
    /// `hasInfiniteMaterials` (creative).
    pub infinite: bool,
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

/// What the client last saw in a slot (`RemoteSlot`).
#[derive(Clone, Debug, Default)]
enum Remote {
    /// Unknown: always differs.
    #[default]
    Placeholder,
    /// A stack the server sent.
    Stack(ItemStack),
    /// The client's prediction from a click.
    Hashed(HashedStack),
}

impl Remote {
    fn matches(&self, stack: &ItemStack) -> bool {
        match self {
            Remote::Placeholder => false,
            Remote::Stack(s) => item::matches(s, stack),
            Remote::Hashed(h) => match &h.item {
                None => stack.is_empty(),
                // Component hashes are not computed: a stack with components counts as
                // changed and is sent again.
                Some(h) => {
                    !stack.is_empty()
                        && h.count == stack.count
                        && h.item == i32::from(stack.item)
                        && h.added.is_empty()
                        && h.removed.is_empty()
                        && stack.patch().is_none()
                }
            },
        }
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
    /// Stack on the mouse cursor.
    pub carried: ItemStack,
    state_id: i32,
    remote: [Remote; MENU_SLOTS],
    remote_carried: Remote,
    quickcraft_status: i32,
    quickcraft_type: i32,
    quickcraft_slots: Vec<usize>,
}

impl Default for PlayerInventory {
    fn default() -> Self {
        PlayerInventory {
            items: std::array::from_fn(|_| ItemStack::EMPTY),
            armor: std::array::from_fn(|_| ItemStack::EMPTY),
            offhand: ItemStack::EMPTY,
            selected: 0,
            craft: std::array::from_fn(|_| ItemStack::EMPTY),
            carried: ItemStack::EMPTY,
            state_id: 0,
            remote: std::array::from_fn(|_| Remote::Placeholder),
            remote_carried: Remote::Placeholder,
            quickcraft_status: 0,
            quickcraft_type: -1,
            quickcraft_slots: Vec::new(),
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
            && slot.count < item::max_stack_size(slot).min(CONTAINER_MAX)
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
            .min(CONTAINER_MAX)
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
        if let Some(slot) = menu_slot_of(self.selected) {
            let current = self.selected_item().clone();
            if let Some(r) = self.remote.get_mut(slot) {
                *r = Remote::Stack(current);
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

    // --- Menu slots ----------------------------------------------------------------------

    fn slot(&self, menu_slot: usize) -> &ItemStack {
        match slot_ref(menu_slot) {
            Some(SlotRef::Result) | None => &ItemStack::EMPTY,
            Some(SlotRef::Craft(i)) => self.craft.get(i).unwrap_or(&ItemStack::EMPTY),
            Some(SlotRef::Inv(i)) => self.get(i),
        }
    }

    fn slot_mut(&mut self, menu_slot: usize) -> Option<&mut ItemStack> {
        match slot_ref(menu_slot)? {
            SlotRef::Result => None,
            SlotRef::Craft(i) => self.craft.get_mut(i),
            SlotRef::Inv(i) => self.get_mut(i),
        }
    }

    fn set_slot(&mut self, menu_slot: usize, stack: ItemStack) {
        if let Some(s) = self.slot_mut(menu_slot) {
            *s = stack;
        }
    }

    /// `Slot.mayPlace`.
    fn may_place(menu_slot: usize, stack: &ItemStack) -> bool {
        match armor_slot(menu_slot) {
            Some(slot) => item::equipment_slot(stack) == slot,
            None => menu_slot != 0,
        }
    }

    /// `Slot.getMaxStackSize(stack)`.
    fn slot_max(menu_slot: usize, stack: &ItemStack) -> i32 {
        if armor_slot(menu_slot).is_some() {
            1
        } else {
            CONTAINER_MAX.min(item::max_stack_size(stack))
        }
    }

    /// `Slot.safeInsert`: moves up to `amount` of `input` into the slot.
    fn safe_insert(&mut self, menu_slot: usize, input: &mut ItemStack, amount: i32) {
        if input.is_empty() || !Self::may_place(menu_slot, input) {
            return;
        }
        let in_slot = self.slot(menu_slot).clone();
        let n = amount
            .min(input.count)
            .min(Self::slot_max(menu_slot, input).saturating_sub(in_slot.count));
        if n <= 0 {
            return;
        }
        if in_slot.is_empty() {
            let part = input.split(n);
            self.set_slot(menu_slot, part);
        } else if in_slot.same_item_same_components(input) {
            input.shrink(n);
            if let Some(s) = self.slot_mut(menu_slot) {
                s.grow(n);
            }
        }
    }

    /// `Slot.tryRemove`.
    fn try_remove(&mut self, menu_slot: usize, amount: i32, max_amount: i32) -> Option<ItemStack> {
        let in_slot = self.slot(menu_slot);
        let allow_modification = Self::may_place(menu_slot, in_slot);
        if !allow_modification && max_amount < in_slot.count {
            return None;
        }
        let n = amount.min(max_amount);
        let removed = self.slot_mut(menu_slot)?.split(n);
        (!removed.is_empty()).then_some(removed)
    }

    /// `moveItemStackTo` over menu slots `start..end`.
    fn move_to(
        &mut self,
        stack: &mut ItemStack,
        start: usize,
        end: usize,
        backwards: bool,
    ) -> bool {
        let mut changed = false;
        let order: Vec<usize> = if backwards {
            (start..end).rev().collect()
        } else {
            (start..end).collect()
        };
        if item::is_stackable(stack) {
            for &dest in &order {
                if stack.is_empty() {
                    break;
                }
                let max = Self::slot_max(dest, stack);
                let Some(target) = self.slot_mut(dest) else {
                    continue;
                };
                if !target.is_empty() && target.same_item_same_components(stack) {
                    let total = target.count.saturating_add(stack.count);
                    if total <= max {
                        target.count = total;
                        *stack = ItemStack::EMPTY;
                        changed = true;
                    } else if target.count < max {
                        stack.shrink(max - target.count);
                        target.count = max;
                        changed = true;
                    }
                }
            }
        }
        if !stack.is_empty() {
            for &dest in &order {
                if self.slot(dest).is_empty() && Self::may_place(dest, stack) {
                    let max = Self::slot_max(dest, stack);
                    let part = stack.split(stack.count.min(max));
                    self.set_slot(dest, part);
                    changed = true;
                    break;
                }
            }
        }
        changed
    }

    /// `InventoryMenu.quickMoveStack`.
    fn quick_move_stack(&mut self, index: usize) -> ItemStack {
        let original = self.slot(index).clone();
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
            self.move_to(&mut stack, 9, 45, true)
        } else if (1..9).contains(&index) {
            self.move_to(&mut stack, 9, 45, false)
        } else if let Some(pos) = armor_dest.filter(|&p| self.slot(p).is_empty()) {
            self.move_to(&mut stack, pos, pos + 1, false)
        } else if eq == EquipmentSlot::OffHand && self.slot(45).is_empty() {
            self.move_to(&mut stack, 45, 46, false)
        } else if (9..36).contains(&index) {
            self.move_to(&mut stack, 36, 45, false)
        } else if (36..45).contains(&index) {
            self.move_to(&mut stack, 9, 36, false)
        } else {
            self.move_to(&mut stack, 9, 45, false)
        };
        if !moved {
            // Nothing moved: Vanilla leaves the slot untouched.
            return ItemStack::EMPTY;
        }
        let same_count = stack.count == original.count;
        self.set_slot(index, stack);
        if same_count {
            return ItemStack::EMPTY;
        }
        original
    }

    fn reset_quickcraft(&mut self) {
        self.quickcraft_status = 0;
        self.quickcraft_slots.clear();
    }

    /// `canItemQuickReplace`.
    fn can_quick_replace(&self, menu_slot: usize, stack: &ItemStack, ignore_size: bool) -> bool {
        let in_slot = self.slot(menu_slot);
        if in_slot.is_empty() {
            return true;
        }
        in_slot.same_item_same_components(stack)
            && in_slot
                .count
                .saturating_add(if ignore_size { 0 } else { stack.count })
                <= item::max_stack_size(stack)
    }

    /// `AbstractContainerMenu.doClick` for the inventory menu. Dropped stacks go to `drops`.
    pub fn click(
        &mut self,
        slot: i16,
        button: i8,
        input: ContainerInput,
        ctx: ClickContext,
        drops: &mut Vec<Drop>,
    ) {
        let button = i32::from(button);
        let index = usize::try_from(slot).ok().filter(|&s| s < MENU_SLOTS);
        if input == ContainerInput::QuickCraft {
            let expected = self.quickcraft_status;
            self.quickcraft_status = button & 3;
            if ((expected != 1 || self.quickcraft_status != 2)
                && expected != self.quickcraft_status)
                || self.carried.is_empty()
            {
                self.reset_quickcraft();
            } else if self.quickcraft_status == 0 {
                self.quickcraft_type = (button >> 2) & 3;
                let valid = self.quickcraft_type == 0
                    || self.quickcraft_type == 1
                    || (self.quickcraft_type == 2 && ctx.infinite);
                if valid {
                    self.quickcraft_status = 1;
                    self.quickcraft_slots.clear();
                } else {
                    self.reset_quickcraft();
                }
            } else if self.quickcraft_status == 1 {
                let Some(index) = index else {
                    return;
                };
                let carried = &self.carried;
                if self.can_quick_replace(index, carried, true)
                    && Self::may_place(index, carried)
                    && (self.quickcraft_type == 2
                        || usize::try_from(carried.count)
                            .is_ok_and(|c| c > self.quickcraft_slots.len()))
                    && !self.quickcraft_slots.contains(&index)
                {
                    self.quickcraft_slots.push(index);
                }
            } else if self.quickcraft_status == 2 {
                if !self.quickcraft_slots.is_empty() {
                    if let [only] = self.quickcraft_slots[..] {
                        let kind = self.quickcraft_type;
                        self.reset_quickcraft();
                        let only = i16::try_from(only).unwrap_or(CLICKED_OUTSIDE);
                        self.click(
                            only,
                            i8::try_from(kind).unwrap_or(0),
                            ContainerInput::Pickup,
                            ctx,
                            drops,
                        );
                        return;
                    }
                    let source = self.carried.clone();
                    if source.is_empty() {
                        self.reset_quickcraft();
                        return;
                    }
                    let mut remaining = self.carried.count;
                    let slots = std::mem::take(&mut self.quickcraft_slots);
                    let n = i32::try_from(slots.len()).unwrap_or(i32::MAX);
                    for &s in &slots {
                        let carried = self.carried.clone();
                        if self.can_quick_replace(s, &carried, true)
                            && Self::may_place(s, &carried)
                            && (self.quickcraft_type == 2 || carried.count >= n)
                        {
                            let carry = self.slot(s).count;
                            let max = item::max_stack_size(&source).min(Self::slot_max(s, &source));
                            let place = match self.quickcraft_type {
                                0 => source.count / n,
                                1 => 1,
                                2 => item::max_stack_size(&source),
                                _ => source.count,
                            };
                            let new_count = place.saturating_add(carry).min(max);
                            remaining -= new_count - carry;
                            self.set_slot(s, source.with_count(new_count));
                        }
                    }
                    self.carried = source.with_count(remaining);
                }
                self.reset_quickcraft();
            } else {
                self.reset_quickcraft();
            }
            return;
        }
        if self.quickcraft_status != 0 {
            self.reset_quickcraft();
            return;
        }
        match input {
            ContainerInput::Pickup | ContainerInput::QuickMove if button == 0 || button == 1 => {
                let primary = button == 0;
                if slot == CLICKED_OUTSIDE {
                    if !self.carried.is_empty() {
                        let stack = if primary {
                            std::mem::take(&mut self.carried)
                        } else {
                            self.carried.split(1)
                        };
                        drops.push(Drop {
                            stack,
                            thrown: true,
                        });
                    }
                    return;
                }
                let Some(index) = index else {
                    return;
                };
                if input == ContainerInput::QuickMove {
                    let mut clicked = self.quick_move_stack(index);
                    while !clicked.is_empty() && self.slot(index).item == clicked.item {
                        clicked = self.quick_move_stack(index);
                    }
                    return;
                }
                let clicked = self.slot(index).clone();
                if clicked.is_empty() {
                    if !self.carried.is_empty() {
                        let amount = if primary { self.carried.count } else { 1 };
                        let mut carried = std::mem::take(&mut self.carried);
                        self.safe_insert(index, &mut carried, amount);
                        self.carried = carried;
                    }
                } else if self.carried.is_empty() {
                    let amount = if primary {
                        clicked.count
                    } else {
                        (clicked.count + 1) / 2
                    };
                    if let Some(taken) = self.try_remove(index, amount, i32::MAX) {
                        self.carried = taken;
                    }
                } else if Self::may_place(index, &self.carried) {
                    if clicked.same_item_same_components(&self.carried) {
                        let amount = if primary { self.carried.count } else { 1 };
                        let mut carried = std::mem::take(&mut self.carried);
                        self.safe_insert(index, &mut carried, amount);
                        self.carried = carried;
                    } else if self.carried.count <= Self::slot_max(index, &self.carried) {
                        let carried = std::mem::take(&mut self.carried);
                        self.carried = clicked;
                        self.set_slot(index, carried);
                    }
                } else if clicked.same_item_same_components(&self.carried) {
                    let space = item::max_stack_size(&self.carried) - self.carried.count;
                    if let Some(taken) = self.try_remove(index, clicked.count, space) {
                        self.carried.grow(taken.count);
                    }
                }
            }
            ContainerInput::Swap if (0..9).contains(&button) || button == 40 => {
                let Some(index) = index else {
                    return;
                };
                let Ok(source_index) = usize::try_from(button) else {
                    return;
                };
                let mut source = self.get(source_index).clone();
                let target = self.slot(index).clone();
                if source.is_empty() && target.is_empty() {
                    return;
                }
                if source.is_empty() {
                    self.set(source_index, target);
                    self.set_slot(index, ItemStack::EMPTY);
                } else if target.is_empty() {
                    if Self::may_place(index, &source) {
                        let max = Self::slot_max(index, &source);
                        if source.count > max {
                            let part = source.split(max);
                            self.set(source_index, source);
                            self.set_slot(index, part);
                        } else {
                            self.set(source_index, ItemStack::EMPTY);
                            self.set_slot(index, source);
                        }
                    }
                } else if Self::may_place(index, &source) {
                    let max = Self::slot_max(index, &source);
                    if source.count > max {
                        let part = source.split(max);
                        self.set(source_index, source);
                        self.set_slot(index, part);
                        let mut rest = target;
                        if !self.add(None, &mut rest, ctx.infinite) || !rest.is_empty() {
                            drops.push(Drop {
                                stack: rest,
                                thrown: true,
                            });
                        }
                    } else {
                        self.set(source_index, target);
                        self.set_slot(index, source);
                    }
                }
            }
            ContainerInput::Clone if ctx.infinite && self.carried.is_empty() => {
                let Some(index) = index else {
                    return;
                };
                let s = self.slot(index);
                if !s.is_empty() {
                    self.carried = s.with_count(item::max_stack_size(s));
                }
            }
            ContainerInput::Throw if self.carried.is_empty() => {
                let Some(index) = index else {
                    return;
                };
                let amount = if button == 0 {
                    1
                } else {
                    self.slot(index).count
                };
                let Some(taken) = self.try_remove(index, amount, i32::MAX) else {
                    return;
                };
                let item = taken.item;
                drops.push(Drop {
                    stack: taken,
                    thrown: true,
                });
                if button == 1 {
                    while self.slot(index).item == item && !self.slot(index).is_empty() {
                        let Some(taken) = self.try_remove(index, amount, i32::MAX) else {
                            break;
                        };
                        drops.push(Drop {
                            stack: taken,
                            thrown: true,
                        });
                    }
                }
            }
            ContainerInput::PickupAll => {
                let Some(index) = index else {
                    return;
                };
                if self.carried.is_empty() || !self.slot(index).is_empty() {
                    return;
                }
                let (start, step): (isize, isize) = if button == 0 { (0, 1) } else { (45, -1) };
                for pass in 0..2 {
                    let mut i = start;
                    while let Ok(u) = usize::try_from(i) {
                        if u >= MENU_SLOTS
                            || self.carried.count >= item::max_stack_size(&self.carried)
                        {
                            break;
                        }
                        let target = self.slot(u).clone();
                        // The crafting result is never collected (`canTakeItemForPickAll`).
                        if u != 0
                            && !target.is_empty()
                            && self.can_quick_replace(u, &self.carried.clone(), true)
                            && (pass != 0 || target.count != item::max_stack_size(&target))
                        {
                            let space = item::max_stack_size(&self.carried) - self.carried.count;
                            if let Some(taken) = self.try_remove(u, target.count, space) {
                                self.carried.grow(taken.count);
                            }
                        }
                        i += step;
                    }
                }
            }
            _ => {}
        }
    }

    /// `handleSetCreativeModeSlot` for a valid menu slot (1–45).
    pub fn set_creative_slot(&mut self, menu_slot: usize, stack: ItemStack) {
        self.set_slot(menu_slot, stack.clone());
        if let Some(r) = self.remote.get_mut(menu_slot) {
            *r = Remote::Stack(stack);
        }
    }

    /// `InventoryMenu.removed` (closing the inventory): the carried stack and the crafting
    /// grid go back into the inventory or are dropped.
    pub fn close(&mut self, infinite: bool, drops: &mut Vec<Drop>) {
        let carried = std::mem::take(&mut self.carried);
        self.place_back(carried, infinite, drops);
        for i in 0..self.craft.len() {
            let stack = self
                .craft
                .get_mut(i)
                .map(std::mem::take)
                .unwrap_or_default();
            self.place_back(stack, infinite, drops);
        }
    }

    // --- Synchronisation -----------------------------------------------------------------

    /// The client's state id.
    pub fn state_id(&self) -> i32 {
        self.state_id
    }

    fn next_state_id(&mut self) -> i32 {
        self.state_id = (self.state_id + 1) & 32767;
        self.state_id
    }

    /// Takes over the client's predictions of a click (`setRemoteSlotUnsafe`,
    /// `setRemoteCarried`).
    pub fn receive_predictions(&mut self, changed: &[(i16, HashedStack)], carried: &HashedStack) {
        for (slot, stack) in changed {
            if let Some(r) = usize::try_from(*slot)
                .ok()
                .and_then(|s| self.remote.get_mut(s))
            {
                *r = Remote::Hashed(stack.clone());
            }
        }
        self.remote_carried = Remote::Hashed(carried.clone());
    }

    /// `sendAllDataToRemote`: all slots and the carried stack.
    pub fn send_all(&mut self, sink: &dyn PacketSink) {
        let items: Vec<ItemStack> = (0..MENU_SLOTS).map(|i| self.slot(i).clone()).collect();
        for (r, stack) in self.remote.iter_mut().zip(&items) {
            *r = Remote::Stack(stack.clone());
        }
        self.remote_carried = Remote::Stack(self.carried.clone());
        let state_id = self.next_state_id();
        sink.send(&ContainerSetContent {
            container_id: 0,
            state_id,
            items,
            carried: self.carried.clone(),
        });
    }

    /// `broadcastChanges`: every slot whose client view differs.
    pub fn broadcast_changes(&mut self, sink: &dyn PacketSink) {
        for i in 0..MENU_SLOTS {
            let current = self.slot(i).clone();
            let differs = self.remote.get(i).is_some_and(|r| !r.matches(&current));
            if differs {
                if let Some(r) = self.remote.get_mut(i) {
                    *r = Remote::Stack(current.clone());
                }
                let state_id = self.next_state_id();
                sink.send(&ContainerSetSlot {
                    container_id: 0,
                    state_id,
                    slot: i16::try_from(i).unwrap_or(0),
                    item: current,
                });
            }
        }
        if !self.remote_carried.matches(&self.carried) {
            self.remote_carried = Remote::Stack(self.carried.clone());
            sink.send(&SetCursorItem {
                item: self.carried.clone(),
            });
        }
    }

    /// `broadcastFullState`.
    pub fn broadcast_full_state(&mut self, sink: &dyn PacketSink) {
        self.send_all(sink);
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

/// An item stack as NBT (`ItemStack.CODEC`).
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

/// An item stack from NBT.
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
        inv
    }
}
