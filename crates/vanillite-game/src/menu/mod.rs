//! Menus (`AbstractContainerMenu`): Vanilla's click handling, slot rules and slot
//! synchronisation, shared by the inventory menu and every container menu.
//!
//! A menu is a list of slots over some storage. [`MenuSlots`] is what a menu kind supplies:
//! where its slots live and their rules (`Slot.mayPlace`, `getMaxStackSize`, `onTake`, …) and
//! its `quickMoveStack`. [`click`] is `AbstractContainerMenu.doClick` over any menu,
//! [`broadcast_changes`] and [`send_all`] are its synchronisation with the client.

pub mod container;
mod station;
mod workstation;

pub use station::{BannerPatterns, add_layer, dye_color};

use vanillite_protocol::packets::play::clientbound::{
    ContainerSetContent, ContainerSetData, ContainerSetSlot, SetCursorItem,
};
use vanillite_protocol::packets::play::serverbound::{ContainerInput, HashedStack};

use crate::inventory::{Drop, PlayerInventory};
use crate::item::{self, ItemStack};
use crate::player::PacketSink;

/// Most items of one slot in a container (`Container.DEFAULT_MAX_STACK_SIZE`).
pub const CONTAINER_MAX: i32 = 99;
/// `AbstractContainerMenu.SLOT_CLICKED_OUTSIDE`.
pub const CLICKED_OUTSIDE: i16 = -999;

/// What the player's state allows in a click.
#[derive(Clone, Copy, Debug)]
pub struct ClickContext {
    /// `hasInfiniteMaterials` (creative).
    pub infinite: bool,
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

/// The state of one menu besides its slots: the carried stack, the quick craft (drag) in
/// progress and what the client last saw.
#[derive(Clone, Debug)]
pub struct MenuState {
    /// `containerId`: 0 for the inventory menu, 1–100 for container menus.
    pub container_id: i32,
    /// Stack on the mouse cursor.
    pub carried: ItemStack,
    state_id: i32,
    remote: Vec<Remote>,
    remote_carried: Remote,
    remote_data: Vec<i32>,
    quickcraft_status: i32,
    quickcraft_type: i32,
    quickcraft_slots: Vec<usize>,
    /// `suppressRemoteUpdates` (while a recipe is placed).
    pub suppress_remote: bool,
    /// Whether the player has infinite materials (creative), as of the last click.
    pub creative: bool,
}

impl MenuState {
    /// A menu with `slots` slots and `data` data slots.
    pub fn new(container_id: i32, slots: usize, data: usize) -> MenuState {
        MenuState {
            container_id,
            carried: ItemStack::EMPTY,
            state_id: 0,
            remote: vec![Remote::Placeholder; slots],
            remote_carried: Remote::Placeholder,
            remote_data: vec![0; data],
            quickcraft_status: 0,
            quickcraft_type: -1,
            quickcraft_slots: Vec::new(),
            suppress_remote: false,
            creative: false,
        }
    }

    /// The client's state id (`getStateId`).
    pub fn state_id(&self) -> i32 {
        self.state_id
    }

    /// `incrementStateId`.
    pub fn next_state_id(&mut self) -> i32 {
        self.state_id = (self.state_id + 1) & 32767;
        self.state_id
    }

    /// `setRemoteSlot`: the client knows `stack` in `slot`.
    pub fn set_remote(&mut self, slot: usize, stack: &ItemStack) {
        if let Some(r) = self.remote.get_mut(slot) {
            *r = Remote::Stack(stack.clone());
        }
    }

    /// `resetQuickCraft`.
    fn reset_quickcraft(&mut self) {
        self.quickcraft_status = 0;
        self.quickcraft_slots.clear();
    }

    /// `transferState(other)` for the player inventory slots both menus show: what the client
    /// knows of them carries over. `map` gives the slot of `other` for a slot of `self`.
    pub fn transfer_from(&mut self, other: &MenuState, map: impl Fn(usize) -> Option<usize>) {
        for (i, r) in self.remote.iter_mut().enumerate() {
            if let Some(o) = map(i).and_then(|o| other.remote.get(o)) {
                *r = o.clone();
            }
        }
    }
}

/// A menu kind: its slots and their rules (`Slot` subclasses and the menu's overrides).
pub trait MenuSlots {
    /// The menu's state.
    fn state(&mut self) -> &mut MenuState;
    /// The menu's state, read only.
    fn state_ref(&self) -> &MenuState;
    /// Number of slots.
    fn slot_count(&self) -> usize;
    /// `Slot.getItem`; the empty stack for invalid indices.
    fn item(&self, slot: usize) -> &ItemStack;
    /// The stack of a slot for in-place count changes (`moveItemStackTo`).
    fn item_mut(&mut self, slot: usize) -> Option<&mut ItemStack>;
    /// `Container.setItem` of the slot's container (with its own reactions, e.g. a crafting
    /// grid recomputing its result).
    fn set_item(&mut self, slot: usize, stack: ItemStack);
    /// `Slot.setChanged`.
    fn set_changed(&mut self, _slot: usize) {}
    /// `Slot.mayPlace`.
    fn may_place(&self, _slot: usize, _stack: &ItemStack) -> bool {
        true
    }
    /// `Slot.mayPickup`.
    fn may_pickup(&self, _slot: usize) -> bool {
        true
    }
    /// `Slot.getMaxStackSize()`.
    fn slot_max(&self, _slot: usize) -> i32 {
        CONTAINER_MAX
    }
    /// `Slot.getMaxStackSize(stack)`.
    fn max_stack(&self, slot: usize, stack: &ItemStack) -> i32 {
        self.slot_max(slot).min(item::max_stack_size(stack))
    }
    /// `Slot.remove(amount)` (`Container.removeItem`).
    fn remove(&mut self, slot: usize, amount: i32) -> ItemStack {
        let taken = self
            .item_mut(slot)
            .map(|s| s.split(amount))
            .unwrap_or_default();
        if !taken.is_empty() {
            self.slot_contents_changed(slot);
        }
        taken
    }
    /// What a container does when one of its slots changed outside `set_item` (a crafting
    /// grid recomputes its result).
    fn slot_contents_changed(&mut self, _slot: usize) {}
    /// `Slot.onTake`.
    fn on_take(
        &mut self,
        slot: usize,
        _taken: &mut ItemStack,
        _ctx: ClickContext,
        _drops: &mut Vec<Drop>,
    ) {
        self.set_changed(slot);
    }
    /// `Slot.onQuickCraft(picked, count)` (stats of result slots).
    fn on_quick_craft(&mut self, _slot: usize, _count: i32) {}
    /// `Slot.onSwapCraft(count)`.
    fn on_swap_craft(&mut self, _slot: usize, _count: i32) {}
    /// `AbstractContainerMenu.quickMoveStack` (shift click).
    fn quick_move(&mut self, slot: usize, ctx: ClickContext, drops: &mut Vec<Drop>) -> ItemStack;
    /// `canDragTo`.
    fn can_drag_to(&self, _slot: usize) -> bool {
        true
    }
    /// `canTakeItemForPickAll`.
    fn can_take_for_pick_all(&self, _slot: usize) -> bool {
        true
    }
    /// The player's inventory (`player.getInventory()`).
    fn inventory(&mut self) -> &mut PlayerInventory;
    /// Number of data slots.
    fn data_count(&self) -> usize {
        0
    }
    /// The value of a data slot.
    fn data(&self, _index: usize) -> i32 {
        0
    }
}

/// `Slot.setByPlayer(stack)` = `set`: the container's `setItem`, then `setChanged`.
pub fn set_by_player<S: MenuSlots + ?Sized>(s: &mut S, slot: usize, stack: ItemStack) {
    s.set_item(slot, stack);
    s.set_changed(slot);
}

/// `Slot.allowModification`.
fn allow_modification<S: MenuSlots + ?Sized>(s: &S, slot: usize) -> bool {
    s.may_pickup(slot) && s.may_place(slot, s.item(slot))
}

/// `Slot.tryRemove`.
pub fn try_remove<S: MenuSlots + ?Sized>(
    s: &mut S,
    slot: usize,
    amount: i32,
    max_amount: i32,
) -> Option<ItemStack> {
    if !s.may_pickup(slot) {
        return None;
    }
    if !allow_modification(s, slot) && max_amount < s.item(slot).count {
        return None;
    }
    let removed = s.remove(slot, amount.min(max_amount));
    if removed.is_empty() {
        return None;
    }
    if s.item(slot).is_empty() {
        set_by_player(s, slot, ItemStack::EMPTY);
    }
    Some(removed)
}

/// `Slot.safeTake`.
pub fn safe_take<S: MenuSlots + ?Sized>(
    s: &mut S,
    slot: usize,
    amount: i32,
    max_amount: i32,
    ctx: ClickContext,
    drops: &mut Vec<Drop>,
) -> ItemStack {
    match try_remove(s, slot, amount, max_amount) {
        Some(mut taken) => {
            s.on_take(slot, &mut taken, ctx, drops);
            taken
        }
        None => ItemStack::EMPTY,
    }
}

/// `Slot.safeInsert(input, amount)`: moves up to `amount` of `input` into the slot.
pub fn safe_insert<S: MenuSlots + ?Sized>(
    s: &mut S,
    slot: usize,
    input: &mut ItemStack,
    amount: i32,
) {
    if input.is_empty() || !s.may_place(slot, input) {
        return;
    }
    let in_slot = s.item(slot).clone();
    let n = amount
        .min(input.count)
        .min(s.max_stack(slot, input).saturating_sub(in_slot.count));
    if n <= 0 {
        return;
    }
    if in_slot.is_empty() {
        let part = input.split(n);
        set_by_player(s, slot, part);
    } else if in_slot.same_item_same_components(input) {
        input.shrink(n);
        let grown = in_slot.with_count(in_slot.count.saturating_add(n));
        set_by_player(s, slot, grown);
    }
}

/// `moveItemStackTo(stack, start, end, backwards)`.
pub fn move_to<S: MenuSlots + ?Sized>(
    s: &mut S,
    stack: &mut ItemStack,
    start: usize,
    end: usize,
    backwards: bool,
) -> bool {
    let mut changed = false;
    let end = end.min(s.slot_count());
    if start >= end {
        return false;
    }
    let order = |i: usize| if backwards { end - 1 - i } else { start + i };
    let n = end - start;
    if item::is_stackable(stack) {
        for i in 0..n {
            if stack.is_empty() {
                break;
            }
            let dest = order(i);
            let target = s.item(dest);
            if target.is_empty() || !target.same_item_same_components(stack) {
                continue;
            }
            let max = s.max_stack(dest, target);
            let count = target.count;
            let total = count.saturating_add(stack.count);
            if total <= max {
                stack.count = 0;
                *stack = ItemStack::EMPTY;
                if let Some(t) = s.item_mut(dest) {
                    t.count = total;
                }
                s.set_changed(dest);
                changed = true;
            } else if count < max {
                stack.shrink(max - count);
                if let Some(t) = s.item_mut(dest) {
                    t.count = max;
                }
                s.set_changed(dest);
                changed = true;
            }
        }
    }
    if !stack.is_empty() {
        for i in 0..n {
            let dest = order(i);
            if s.item(dest).is_empty() && s.may_place(dest, stack) {
                let max = s.max_stack(dest, stack);
                let part = stack.split(stack.count.min(max));
                set_by_player(s, dest, part);
                s.set_changed(dest);
                changed = true;
                break;
            }
        }
    }
    changed
}

/// The usual end of a `quickMoveStack`: the slot keeps the rest of `stack` (or is emptied);
/// returns the original stack if anything moved, else the empty stack (which ends the shift
/// click loop).
pub fn finish_quick_move<S: MenuSlots + ?Sized>(
    s: &mut S,
    slot: usize,
    stack: ItemStack,
    original: &ItemStack,
    ctx: ClickContext,
    drops: &mut Vec<Drop>,
) -> ItemStack {
    let same_count = stack.count == original.count;
    if stack.is_empty() {
        set_by_player(s, slot, ItemStack::EMPTY);
    } else {
        // Vanilla changed the slot's stack in place: no `setItem`.
        if let Some(target) = s.item_mut(slot) {
            *target = stack.clone();
        }
        s.set_changed(slot);
    }
    if same_count {
        return ItemStack::EMPTY;
    }
    let mut taken = stack;
    s.on_take(slot, &mut taken, ctx, drops);
    original.clone()
}

/// `canItemQuickReplace`.
fn can_quick_replace<S: MenuSlots + ?Sized>(
    s: &S,
    slot: usize,
    stack: &ItemStack,
    ignore_size: bool,
) -> bool {
    let in_slot = s.item(slot);
    if in_slot.is_empty() {
        return true;
    }
    in_slot.same_item_same_components(stack)
        && in_slot
            .count
            .saturating_add(if ignore_size { 0 } else { stack.count })
            <= item::max_stack_size(stack)
}

/// `ItemStack.isSameItem` of two stacks (empty stacks are air).
fn same_item(a: &ItemStack, b: &ItemStack) -> bool {
    let id = |s: &ItemStack| if s.is_empty() { 0 } else { s.item };
    id(a) == id(b)
}

/// `isValidSlotIndex`: -1, -999 or a slot of the menu.
pub fn is_valid_slot_index<S: MenuSlots + ?Sized>(s: &S, slot: i16) -> bool {
    slot == -1 || slot == CLICKED_OUTSIDE || usize::try_from(slot).is_ok_and(|i| i < s.slot_count())
}

/// `AbstractContainerMenu.doClick`. Dropped stacks go to `drops`. The slot index must have
/// passed [`is_valid_slot_index`].
pub fn click<S: MenuSlots + ?Sized>(
    s: &mut S,
    slot: i16,
    button: i8,
    input: ContainerInput,
    ctx: ClickContext,
    drops: &mut Vec<Drop>,
) {
    let button = i32::from(button);
    s.state().creative = ctx.infinite;
    let index = usize::try_from(slot).ok().filter(|&i| i < s.slot_count());
    if input == ContainerInput::QuickCraft {
        quick_craft(s, index, button, ctx, drops);
        return;
    }
    if s.state_ref().quickcraft_status != 0 {
        s.state().reset_quickcraft();
        return;
    }
    match input {
        ContainerInput::Pickup | ContainerInput::QuickMove if button == 0 || button == 1 => {
            let primary = button == 0;
            if slot == CLICKED_OUTSIDE {
                let carried = &mut s.state().carried;
                if !carried.is_empty() {
                    let stack = if primary {
                        std::mem::take(carried)
                    } else {
                        carried.split(1)
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
                if !s.may_pickup(index) {
                    return;
                }
                let mut clicked = s.quick_move(index, ctx, drops);
                while !clicked.is_empty() && same_item(s.item(index), &clicked) {
                    clicked = s.quick_move(index, ctx, drops);
                }
                return;
            }
            pickup(s, index, primary, ctx, drops);
            s.set_changed(index);
        }
        ContainerInput::Swap if (0..9).contains(&button) || button == 40 => {
            let (Some(index), Ok(source_index)) = (index, usize::try_from(button)) else {
                return;
            };
            swap(s, index, source_index, ctx, drops);
        }
        ContainerInput::Clone if ctx.infinite && s.state_ref().carried.is_empty() => {
            let Some(index) = index else {
                return;
            };
            let stack = s.item(index);
            if !stack.is_empty() {
                let clone = stack.with_count(item::max_stack_size(stack));
                s.state().carried = clone;
            }
        }
        ContainerInput::Throw if s.state_ref().carried.is_empty() => {
            let Some(index) = index else {
                return;
            };
            let amount = if button == 0 { 1 } else { s.item(index).count };
            let mut taken = safe_take(s, index, amount, i32::MAX, ctx, drops);
            let item_id = taken.item;
            let first = !taken.is_empty();
            if first {
                drops.push(Drop {
                    stack: std::mem::take(&mut taken),
                    thrown: true,
                });
            }
            if button == 1 && first {
                while !s.item(index).is_empty() && s.item(index).item == item_id {
                    let taken = safe_take(s, index, amount, i32::MAX, ctx, drops);
                    if taken.is_empty() {
                        break;
                    }
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
            pickup_all(s, index, button, ctx, drops);
        }
        _ => {}
    }
}

/// The `QUICK_CRAFT` (drag) branch of `doClick`.
fn quick_craft<S: MenuSlots + ?Sized>(
    s: &mut S,
    index: Option<usize>,
    button: i32,
    ctx: ClickContext,
    drops: &mut Vec<Drop>,
) {
    let st = s.state();
    let expected = st.quickcraft_status;
    st.quickcraft_status = button & 3;
    if ((expected != 1 || st.quickcraft_status != 2) && expected != st.quickcraft_status)
        || st.carried.is_empty()
    {
        st.reset_quickcraft();
    } else if st.quickcraft_status == 0 {
        st.quickcraft_type = (button >> 2) & 3;
        let valid = st.quickcraft_type == 0
            || st.quickcraft_type == 1
            || (st.quickcraft_type == 2 && ctx.infinite);
        if valid {
            st.quickcraft_status = 1;
            st.quickcraft_slots.clear();
        } else {
            st.reset_quickcraft();
        }
    } else if st.quickcraft_status == 1 {
        let Some(index) = index else {
            return;
        };
        let carried = s.state_ref().carried.clone();
        let kind = s.state_ref().quickcraft_type;
        let n = s.state_ref().quickcraft_slots.len();
        if can_quick_replace(s, index, &carried, true)
            && s.may_place(index, &carried)
            && (kind == 2 || usize::try_from(carried.count).is_ok_and(|c| c > n))
            && s.can_drag_to(index)
            && !s.state_ref().quickcraft_slots.contains(&index)
        {
            s.state().quickcraft_slots.push(index);
        }
    } else if st.quickcraft_status == 2 {
        if st.quickcraft_slots.is_empty() {
            st.reset_quickcraft();
            return;
        }
        if let [only] = st.quickcraft_slots[..] {
            let kind = st.quickcraft_type;
            st.reset_quickcraft();
            click(
                s,
                i16::try_from(only).unwrap_or(CLICKED_OUTSIDE),
                i8::try_from(kind).unwrap_or(0),
                ContainerInput::Pickup,
                ctx,
                drops,
            );
            return;
        }
        let source = st.carried.clone();
        if source.is_empty() {
            st.reset_quickcraft();
            return;
        }
        let mut remaining = source.count;
        let slots = std::mem::take(&mut st.quickcraft_slots);
        let kind = st.quickcraft_type;
        let n = i32::try_from(slots.len()).unwrap_or(i32::MAX);
        for &slot in &slots {
            let carried = s.state_ref().carried.clone();
            if can_quick_replace(s, slot, &carried, true)
                && s.may_place(slot, &carried)
                && (kind == 2 || carried.count >= n)
                && s.can_drag_to(slot)
            {
                let carry = s.item(slot).count;
                let max = item::max_stack_size(&source).min(s.max_stack(slot, &source));
                let place = match kind {
                    0 => source.count / n,
                    1 => 1,
                    2 => item::max_stack_size(&source),
                    _ => source.count,
                };
                let new_count = place.saturating_add(carry).min(max);
                remaining -= new_count - carry;
                set_by_player(s, slot, source.with_count(new_count));
            }
        }
        let st = s.state();
        st.carried = source.with_count(remaining);
        st.reset_quickcraft();
    } else {
        st.reset_quickcraft();
    }
}

/// The `PICKUP` branch of `doClick` on a slot (without bundle overrides).
fn pickup<S: MenuSlots + ?Sized>(
    s: &mut S,
    index: usize,
    primary: bool,
    ctx: ClickContext,
    drops: &mut Vec<Drop>,
) {
    let clicked = s.item(index).clone();
    let carried = s.state_ref().carried.clone();
    if clicked.is_empty() {
        if !carried.is_empty() {
            let amount = if primary { carried.count } else { 1 };
            let mut carried = std::mem::take(&mut s.state().carried);
            safe_insert(s, index, &mut carried, amount);
            s.state().carried = carried;
        }
    } else if s.may_pickup(index) {
        if carried.is_empty() {
            let amount = if primary {
                clicked.count
            } else {
                (clicked.count + 1) / 2
            };
            if let Some(mut taken) = try_remove(s, index, amount, i32::MAX) {
                s.state().carried = taken.clone();
                s.on_take(index, &mut taken, ctx, drops);
                // `onTake` changed the carried stack itself (map post-processing).
                s.state().carried = taken;
            }
        } else if s.may_place(index, &carried) {
            if clicked.same_item_same_components(&carried) {
                let amount = if primary { carried.count } else { 1 };
                let mut carried = std::mem::take(&mut s.state().carried);
                safe_insert(s, index, &mut carried, amount);
                s.state().carried = carried;
            } else if carried.count <= s.max_stack(index, &carried) {
                s.state().carried = clicked;
                set_by_player(s, index, carried);
            }
        } else if clicked.same_item_same_components(&carried) {
            let space = item::max_stack_size(&carried) - carried.count;
            if let Some(mut taken) = try_remove(s, index, clicked.count, space) {
                s.state().carried.grow(taken.count);
                s.on_take(index, &mut taken, ctx, drops);
            }
        }
    }
}

/// The `SWAP` branch of `doClick` (number keys and F).
fn swap<S: MenuSlots + ?Sized>(
    s: &mut S,
    index: usize,
    source_index: usize,
    ctx: ClickContext,
    drops: &mut Vec<Drop>,
) {
    let mut source = s.inventory().get(source_index).clone();
    let target = s.item(index).clone();
    if source.is_empty() && target.is_empty() {
        return;
    }
    if source.is_empty() {
        if s.may_pickup(index) {
            s.inventory().set(source_index, target.clone());
            s.on_swap_craft(index, target.count);
            set_by_player(s, index, ItemStack::EMPTY);
            let mut taken = target;
            s.on_take(index, &mut taken, ctx, drops);
            s.inventory().set(source_index, taken);
        }
    } else if target.is_empty() {
        if s.may_place(index, &source) {
            let max = s.max_stack(index, &source);
            if source.count > max {
                let part = source.split(max);
                s.inventory().set(source_index, source);
                set_by_player(s, index, part);
            } else {
                s.inventory().set(source_index, ItemStack::EMPTY);
                set_by_player(s, index, source);
            }
        }
    } else if s.may_pickup(index) && s.may_place(index, &source) {
        let max = s.max_stack(index, &source);
        if source.count > max {
            let part = source.split(max);
            s.inventory().set(source_index, source);
            set_by_player(s, index, part);
            let mut rest = target;
            s.on_take(index, &mut rest, ctx, drops);
            if !s.inventory().add(None, &mut rest, ctx.infinite) || !rest.is_empty() {
                drops.push(Drop {
                    stack: rest,
                    thrown: true,
                });
            }
        } else {
            s.inventory().set(source_index, target.clone());
            set_by_player(s, index, source);
            let mut taken = target;
            s.on_take(index, &mut taken, ctx, drops);
            s.inventory().set(source_index, taken);
        }
    }
}

/// The `PICKUP_ALL` branch of `doClick` (double click).
fn pickup_all<S: MenuSlots + ?Sized>(
    s: &mut S,
    index: usize,
    button: i32,
    ctx: ClickContext,
    drops: &mut Vec<Drop>,
) {
    let carried = s.state_ref().carried.clone();
    if carried.is_empty() || (!s.item(index).is_empty() && s.may_pickup(index)) {
        return;
    }
    let len = s.slot_count();
    let max = item::max_stack_size(&carried);
    for pass in 0..2 {
        for step in 0..len {
            let i = if button == 0 { step } else { len - 1 - step };
            let carried = s.state_ref().carried.clone();
            if carried.count >= max {
                break;
            }
            let target = s.item(i).clone();
            if !target.is_empty()
                && can_quick_replace(s, i, &carried, true)
                && s.may_pickup(i)
                && s.can_take_for_pick_all(i)
                && (pass != 0 || target.count != item::max_stack_size(&target))
            {
                let taken = safe_take(s, i, target.count, max - carried.count, ctx, drops);
                s.state().carried.grow(taken.count);
            }
        }
    }
}

/// Takes over the client's predictions of a click (`setRemoteSlotUnsafe`,
/// `setRemoteCarried`).
pub fn receive_predictions(
    state: &mut MenuState,
    changed: &[(i16, HashedStack)],
    carried: &HashedStack,
) {
    for (slot, stack) in changed {
        if let Some(r) = usize::try_from(*slot)
            .ok()
            .and_then(|s| state.remote.get_mut(s))
        {
            *r = Remote::Hashed(stack.clone());
        }
    }
    state.remote_carried = Remote::Hashed(carried.clone());
}

/// `sendAllDataToRemote`: all slots, the carried stack and the data slots.
pub fn send_all<S: MenuSlots + ?Sized>(s: &mut S, sink: &dyn PacketSink) {
    let items: Vec<ItemStack> = (0..s.slot_count()).map(|i| s.item(i).clone()).collect();
    let data: Vec<i32> = (0..s.data_count()).map(|i| s.data(i)).collect();
    let st = s.state();
    st.remote.resize(items.len(), Remote::Placeholder);
    for (r, stack) in st.remote.iter_mut().zip(&items) {
        *r = Remote::Stack(stack.clone());
    }
    st.remote_carried = Remote::Stack(st.carried.clone());
    st.remote_data.clone_from(&data);
    let state_id = st.next_state_id();
    sink.send(&ContainerSetContent {
        container_id: st.container_id,
        state_id,
        items,
        carried: st.carried.clone(),
    });
    for (i, value) in data.into_iter().enumerate() {
        send_data(sink, st.container_id, i, value);
    }
}

fn send_data(sink: &dyn PacketSink, container_id: i32, index: usize, value: i32) {
    sink.send(&ContainerSetData {
        container_id,
        id: i16::try_from(index).unwrap_or(0),
        // `writeShort`: the low 16 bits.
        value: i16::from_ne_bytes(value.to_ne_bytes()[..2].try_into().unwrap_or([0, 0])),
    });
}

/// `broadcastChanges`: every slot, the carried stack and every data slot whose client view
/// differs.
pub fn broadcast_changes<S: MenuSlots + ?Sized>(s: &mut S, sink: &dyn PacketSink) {
    if s.state_ref().suppress_remote {
        return;
    }
    for i in 0..s.slot_count() {
        let differs = s
            .state_ref()
            .remote
            .get(i)
            .is_none_or(|r| !r.matches(s.item(i)));
        if differs {
            let current = s.item(i).clone();
            let st = s.state();
            if let Some(r) = st.remote.get_mut(i) {
                *r = Remote::Stack(current.clone());
            }
            let state_id = st.next_state_id();
            sink.send(&ContainerSetSlot {
                container_id: st.container_id,
                state_id,
                slot: i16::try_from(i).unwrap_or(0),
                item: current,
            });
        }
    }
    let st = s.state();
    if !st.remote_carried.matches(&st.carried) {
        st.remote_carried = Remote::Stack(st.carried.clone());
        sink.send(&SetCursorItem {
            item: st.carried.clone(),
        });
    }
    for i in 0..s.data_count() {
        let value = s.data(i);
        let st = s.state();
        if st.remote_data.len() <= i {
            st.remote_data.resize(i + 1, 0);
        }
        if st.remote_data.get(i) != Some(&value) {
            if let Some(r) = st.remote_data.get_mut(i) {
                *r = value;
            }
            send_data(sink, st.container_id, i, value);
        }
    }
}

/// `broadcastFullState`.
pub fn broadcast_full_state<S: MenuSlots + ?Sized>(s: &mut S, sink: &dyn PacketSink) {
    send_all(s, sink);
}

/// `AbstractContainerMenu.getRedstoneSignalFromContainer`: the comparator signal of a
/// container's stacks.
pub fn redstone_signal(items: &[ItemStack], slot_max: i32) -> i32 {
    if items.is_empty() {
        return 0;
    }
    let mut total = 0.0f32;
    for stack in items {
        if !stack.is_empty() {
            #[allow(clippy::as_conversions, clippy::cast_precision_loss)]
            let part = stack.count as f32 / slot_max.min(item::max_stack_size(stack)) as f32;
            total += part;
        }
    }
    #[allow(clippy::as_conversions, clippy::cast_precision_loss)]
    let len = items.len() as f32;
    total /= len;
    // `Mth.lerpDiscrete(total, 0, 15)`: `floor(total * 15) + (total > 0 ? 1 : 0)` capped.
    lerp_discrete(total, 0, 15)
}

/// `Mth.lerpDiscrete(alpha, min, max)`.
fn lerp_discrete(alpha: f32, min: i32, max: i32) -> i32 {
    let range = max - min;
    #[allow(clippy::as_conversions, clippy::cast_precision_loss)]
    let scaled = alpha * (range - 1) as f32;
    #[allow(clippy::as_conversions, clippy::cast_possible_truncation)]
    let floor = scaled.floor() as i32;
    min + floor + i32::from(alpha > 0.0)
}

#[cfg(test)]
mod tests;
