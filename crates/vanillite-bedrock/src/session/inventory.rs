//! The player inventory: a mirror of the Java inventory menu with Bedrock stack ids, Bedrock's
//! item stack requests as Java clicks (survival) or creative slot changes (creative), and the
//! Java inventory updates as Bedrock inventory packets.
//!
//! Java menu slots: 0 crafting result, 1-4 crafting grid, 5-8 armour (head..feet), 9-35 main
//! inventory, 36-44 hotbar, 45 offhand. Bedrock: window 0 slots 0-8 hotbar and 9-35 main
//! inventory, window 119 the offhand, window 120 the armour, the cursor in the UI window.

use vanillite_data::item::ItemId;
use vanillite_protocol::item::ItemStack;
use vanillite_protocol::packets::play::clientbound as pcb;
use vanillite_protocol::packets::play::serverbound::{self as psb, ContainerInput, HashedStack};

use super::Session;
use crate::packets::{self as bp, SlotRef, StackAction, StackRequest, container, id, window};

/// Slots of the Java inventory menu.
pub(super) const MENU: usize = 46;
/// Java clicks one request may become.
const MAX_CLICKS: usize = 256;

#[derive(Debug, Clone, Default)]
pub(super) struct Slot {
    pub stack: ItemStack,
    pub net: i32,
}

/// The mirrored Java inventory.
#[derive(Debug)]
pub(super) struct Inventory {
    pub slots: Vec<Slot>,
    pub cursor: Slot,
    pub state_id: i32,
    next_net: i32,
    pub selected: u8,
    /// The item of the last creative pick (`craft_creative`), taken from the created output.
    created: Option<ItemStack>,
    clicks: usize,
}

impl Default for Inventory {
    fn default() -> Self {
        Self {
            slots: vec![Slot::default(); MENU],
            cursor: Slot::default(),
            state_id: 0,
            next_net: 0,
            selected: 0,
            created: None,
            clicks: 0,
        }
    }
}

/// A slot named by a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Menu(usize),
    Cursor,
    Created,
}

/// The Java slot of a Bedrock request slot.
fn target(r: &SlotRef) -> Option<Target> {
    let s = usize::from(r.slot);
    Some(match r.container {
        container::COMBINED_INVENTORY | container::HOTBAR | container::INVENTORY => {
            if s < 9 {
                Target::Menu(s.saturating_add(36))
            } else if s < 36 {
                Target::Menu(s)
            } else {
                return None;
            }
        }
        container::ARMOUR if s < 4 => Target::Menu(s.saturating_add(5)),
        container::OFFHAND => Target::Menu(45),
        container::CRAFTING_INPUT if (28..32).contains(&s) => Target::Menu(s.saturating_sub(27)),
        container::CURSOR => Target::Cursor,
        container::CREATED_OUTPUT => Target::Created,
        _ => return None,
    })
}

/// The Bedrock window and slot of a Java menu slot.
pub(super) fn bedrock_slot(menu: usize) -> Option<(u32, u32)> {
    let s = u32::try_from(menu).ok()?;
    Some(match menu {
        1..=4 => (window::UI, s.saturating_add(27)),
        5..=8 => (window::ARMOUR, s.saturating_sub(5)),
        9..=35 => (window::INVENTORY, s),
        36..=44 => (window::INVENTORY, s.saturating_sub(36)),
        45 => (window::OFFHAND, 0),
        _ => return None,
    })
}

/// The menu slot of a Java player inventory index (`set_player_inventory`).
fn menu_of_inventory(index: i32) -> Option<usize> {
    let i = usize::try_from(index).ok()?;
    Some(match i {
        0..=8 => i.saturating_add(36),
        9..=35 => i,
        36..=39 => 8usize.saturating_sub(i.saturating_sub(36)),
        40 => 45,
        _ => return None,
    })
}

fn max_stack(stack: &ItemStack) -> i32 {
    if usize::from(stack.item) >= vanillite_data::item::ITEMS.len() {
        return 64;
    }
    i32::from(ItemId(stack.item).info().max_stack_size).max(1)
}

fn same(a: &ItemStack, b: &ItemStack) -> bool {
    a.same_item_same_components(b)
}

impl Inventory {
    fn fresh(&mut self) -> i32 {
        self.next_net = self.next_net.wrapping_add(1).max(1);
        self.next_net
    }

    fn get(&self, t: Target) -> ItemStack {
        match t {
            Target::Menu(i) => self
                .slots
                .get(i)
                .map(|s| s.stack.clone())
                .unwrap_or_default(),
            Target::Cursor => self.cursor.stack.clone(),
            Target::Created => ItemStack::EMPTY,
        }
    }

    fn slot(&self, t: Target) -> Option<&Slot> {
        match t {
            Target::Menu(i) => self.slots.get(i),
            Target::Cursor => Some(&self.cursor),
            Target::Created => None,
        }
    }

    /// Sets a slot; a changed stack gets a new stack id. Returns whether it changed.
    fn put(&mut self, t: Target, stack: ItemStack) -> bool {
        let stack = if stack.is_empty() {
            ItemStack::EMPTY
        } else {
            stack
        };
        let net = self.fresh();
        let slot = match t {
            Target::Menu(i) => match self.slots.get_mut(i) {
                Some(s) => s,
                None => return false,
            },
            Target::Cursor => &mut self.cursor,
            Target::Created => return false,
        };
        if slot.stack == stack {
            return false;
        }
        *slot = Slot {
            net: if stack.is_empty() { 0 } else { net },
            stack,
        };
        true
    }

    /// Applies a Java click to the mirror (`AbstractContainerMenu.doClick` for plain slots).
    fn simulate_click(&mut self, slot: i16, button: i8, input: ContainerInput) {
        let mut c = self.cursor.stack.clone();
        if slot == -999 {
            if input == ContainerInput::Pickup && !c.is_empty() {
                if button == 0 {
                    c = ItemStack::EMPTY;
                } else {
                    c.shrink(1);
                }
                self.put(Target::Cursor, c);
            }
            return;
        }
        let Some(i) = usize::try_from(slot).ok().filter(|&i| i > 0 && i < MENU) else {
            return;
        };
        let mut s = self.get(Target::Menu(i));
        match (input, button) {
            (ContainerInput::Throw, b) => {
                if c.is_empty() && !s.is_empty() {
                    if b == 0 {
                        s.shrink(1);
                    } else {
                        s = ItemStack::EMPTY;
                    }
                }
            }
            (ContainerInput::Pickup, 0) => {
                if s.is_empty() && !c.is_empty() {
                    let n = c.count.min(max_stack(&c));
                    s = c.with_count(n);
                    c.shrink(n);
                } else if c.is_empty() {
                    c = std::mem::take(&mut s);
                } else if same(&s, &c) {
                    let n = c.count.min(max_stack(&s).saturating_sub(s.count)).max(0);
                    s.grow(n);
                    c.shrink(n);
                } else {
                    std::mem::swap(&mut s, &mut c);
                }
            }
            (ContainerInput::Pickup, _) => {
                if c.is_empty() && !s.is_empty() {
                    let n = s.count.saturating_add(1) / 2;
                    c = s.with_count(n);
                    s.shrink(n);
                } else if s.is_empty() && !c.is_empty() {
                    s = c.with_count(1);
                    c.shrink(1);
                } else if same(&s, &c) {
                    if s.count < max_stack(&s) {
                        s.grow(1);
                        c.shrink(1);
                    }
                } else {
                    std::mem::swap(&mut s, &mut c);
                }
            }
            _ => {}
        }
        self.put(Target::Menu(i), s);
        self.put(Target::Cursor, c);
    }
}

/// A request that cannot be translated.
#[derive(Debug)]
struct Refused;

impl Session {
    /// One Java click: applied to the mirror and sent.
    fn click(&mut self, slot: i16, button: i8, input: ContainerInput) -> Result<(), Refused> {
        self.inv.clicks = self.inv.clicks.saturating_add(1);
        if self.inv.clicks > MAX_CLICKS {
            return Err(Refused);
        }
        self.inv.simulate_click(slot, button, input);
        let state_id = self.inv.state_id;
        self.java_send(&psb::ContainerClick {
            container_id: 0,
            state_id,
            slot,
            button,
            input,
            changed: Vec::new(),
            carried: HashedStack::default(),
        });
        Ok(())
    }

    fn java_slot(t: Target) -> Result<i16, Refused> {
        match t {
            Target::Menu(i) if i > 0 => i16::try_from(i).map_err(|_| Refused),
            _ => Err(Refused),
        }
    }

    /// Takes `n` items of menu slot `t` onto the (Java) cursor.
    fn take_to_cursor(&mut self, t: Target, n: i32) -> Result<(), Refused> {
        let slot = Self::java_slot(t)?;
        let s = self.inv.get(t);
        let c = self.inv.cursor.stack.clone();
        if n <= 0 || s.count < n {
            return Err(Refused);
        }
        if c.is_empty() {
            self.click(slot, 0, ContainerInput::Pickup)?;
            for _ in 0..s.count.saturating_sub(n) {
                self.click(slot, 1, ContainerInput::Pickup)?;
            }
            return Ok(());
        }
        if !same(&s, &c) || s.count.saturating_add(c.count) > max_stack(&s) {
            return Err(Refused);
        }
        // Put the cursor down, pick everything up and give back what stays.
        self.click(slot, 0, ContainerInput::Pickup)?;
        self.click(slot, 0, ContainerInput::Pickup)?;
        for _ in 0..s.count.saturating_sub(n) {
            self.click(slot, 1, ContainerInput::Pickup)?;
        }
        Ok(())
    }

    /// Places `n` items of the cursor into menu slot `t`.
    fn place_from_cursor(&mut self, t: Target, n: i32) -> Result<(), Refused> {
        let slot = Self::java_slot(t)?;
        let c = self.inv.cursor.stack.clone();
        let d = self.inv.get(t);
        if n <= 0 || c.count < n || (!d.is_empty() && !same(&d, &c)) {
            return Err(Refused);
        }
        if n == c.count && d.count.saturating_add(n) <= max_stack(&c) {
            return self.click(slot, 0, ContainerInput::Pickup);
        }
        for _ in 0..n {
            self.click(slot, 1, ContainerInput::Pickup)?;
        }
        Ok(())
    }

    /// One action in survival: Java clicks.
    fn survival_action(&mut self, a: &StackAction) -> Result<(), Refused> {
        let t = |r: &SlotRef| target(r).ok_or(Refused);
        match *a {
            StackAction::Take { count, src, dst } | StackAction::Place { count, src, dst } => {
                let (src, dst, n) = (t(&src)?, t(&dst)?, i32::from(count));
                match (src, dst) {
                    (Target::Created, _) | (_, Target::Created) => Err(Refused),
                    (Target::Cursor, Target::Cursor) => Ok(()),
                    (s, Target::Cursor) => self.take_to_cursor(s, n),
                    (Target::Cursor, d) => self.place_from_cursor(d, n),
                    (s, d) => {
                        if !self.inv.cursor.stack.is_empty() {
                            return Err(Refused);
                        }
                        self.take_to_cursor(s, n)?;
                        self.place_from_cursor(d, n)
                    }
                }
            }
            StackAction::Swap { src, dst } => match (t(&src)?, t(&dst)?) {
                (Target::Cursor, Target::Menu(_)) | (Target::Menu(_), Target::Cursor) => {
                    let other = if t(&src)? == Target::Cursor {
                        t(&dst)?
                    } else {
                        t(&src)?
                    };
                    self.click(Self::java_slot(other)?, 0, ContainerInput::Pickup)
                }
                (a @ Target::Menu(_), b @ Target::Menu(_)) => {
                    if !self.inv.cursor.stack.is_empty() {
                        return Err(Refused);
                    }
                    let (a, b) = (Self::java_slot(a)?, Self::java_slot(b)?);
                    self.click(a, 0, ContainerInput::Pickup)?;
                    self.click(b, 0, ContainerInput::Pickup)?;
                    self.click(a, 0, ContainerInput::Pickup)
                }
                _ => Err(Refused),
            },
            StackAction::Drop { count, src } => {
                let n = i32::from(count);
                match t(&src)? {
                    Target::Cursor => {
                        let c = self.inv.cursor.stack.count;
                        if n == c {
                            return self.click(-999, 0, ContainerInput::Pickup);
                        }
                        for _ in 0..n.min(c) {
                            self.click(-999, 1, ContainerInput::Pickup)?;
                        }
                        Ok(())
                    }
                    s @ Target::Menu(_) => {
                        let slot = Self::java_slot(s)?;
                        let have = self.inv.get(s).count;
                        if n >= have {
                            return self.click(slot, 1, ContainerInput::Throw);
                        }
                        for _ in 0..n {
                            self.click(slot, 0, ContainerInput::Throw)?;
                        }
                        Ok(())
                    }
                    Target::Created => Err(Refused),
                }
            }
            StackAction::MineBlock => Ok(()),
            _ => Err(Refused),
        }
    }

    /// Sets a slot in creative mode: mirror and Java creative slot.
    fn creative_put(&mut self, t: Target, stack: ItemStack) {
        if self.inv.put(t, stack.clone())
            && let Target::Menu(i) = t
            && let Ok(slot) = i16::try_from(i)
        {
            self.java_send(&psb::SetCreativeModeSlot { slot, item: stack });
        }
    }

    /// One action in creative mode: Bedrock's own semantics, Java creative slots.
    fn creative_action(&mut self, a: &StackAction) -> Result<(), Refused> {
        let t = |r: &SlotRef| target(r).ok_or(Refused);
        match *a {
            StackAction::CraftCreative { net_id, .. } => {
                let item = self
                    .version
                    .palette
                    .items()
                    .creative(net_id)
                    .ok_or(Refused)?;
                let stack = ItemStack::new(item, 1);
                let max = max_stack(&stack);
                self.inv.created = Some(stack.with_count(max));
                Ok(())
            }
            StackAction::Take { count, src, dst } | StackAction::Place { count, src, dst } => {
                let (src, dst, n) = (t(&src)?, t(&dst)?, i32::from(count));
                let from = match src {
                    Target::Created => self.inv.created.clone().ok_or(Refused)?,
                    s => self.inv.get(s),
                };
                let to = self.inv.get(dst);
                if n <= 0 || from.count < n || dst == Target::Created || src == dst {
                    return Err(Refused);
                }
                let moved = if to.is_empty() {
                    from.with_count(n)
                } else if same(&to, &from) && to.count.saturating_add(n) <= max_stack(&to) {
                    to.with_count(to.count.saturating_add(n))
                } else {
                    return Err(Refused);
                };
                if src != Target::Created {
                    let mut rest = from;
                    rest.shrink(n);
                    self.creative_put(src, rest);
                }
                self.creative_put(dst, moved);
                Ok(())
            }
            StackAction::Swap { src, dst } => {
                let (a, b) = (t(&src)?, t(&dst)?);
                if a == Target::Created || b == Target::Created {
                    return Err(Refused);
                }
                let (sa, sb) = (self.inv.get(a), self.inv.get(b));
                self.creative_put(a, sb);
                self.creative_put(b, sa);
                Ok(())
            }
            StackAction::Drop { count, src } | StackAction::Destroy { count, src } => {
                let s = t(&src)?;
                let from = self.inv.get(s);
                let n = i32::from(count).min(from.count);
                if n <= 0 {
                    return Err(Refused);
                }
                let mut rest = from.clone();
                rest.shrink(n);
                self.creative_put(s, rest);
                if matches!(a, StackAction::Drop { .. }) {
                    self.java_send(&psb::SetCreativeModeSlot {
                        slot: -1,
                        item: from.with_count(n),
                    });
                }
                Ok(())
            }
            StackAction::MineBlock => Ok(()),
            _ => Err(Refused),
        }
    }

    /// Answers one item stack request.
    pub(super) fn stack_request(&mut self, req: &StackRequest) -> bp::StackResponse {
        let creative = self
            .world
            .as_ref()
            .is_some_and(|w| w.game_mode == pcb::GameMode::Creative);
        self.inv.clicks = 0;
        let mut ok = true;
        for a in &req.actions {
            let r = if creative {
                self.creative_action(a)
            } else {
                self.survival_action(a)
            };
            if r.is_err() {
                ok = false;
                break;
            }
        }
        if !ok {
            // Bedrock rolls its prediction back; resend the true state as well.
            self.inventory_resync = true;
            return bp::StackResponse {
                id: req.id,
                containers: None,
            };
        }
        let mut containers: Vec<(u8, Vec<bp::ResponseSlot>)> = Vec::new();
        let refs = req.actions.iter().flat_map(|a| match *a {
            StackAction::Take { src, dst, .. }
            | StackAction::Place { src, dst, .. }
            | StackAction::Swap { src, dst } => [Some(src), Some(dst)],
            StackAction::Drop { src, .. }
            | StackAction::Destroy { src, .. }
            | StackAction::Consume { src, .. } => [Some(src), None],
            _ => [None, None],
        });
        for r in refs.flatten() {
            let Some(slot) = target(&r).and_then(|t| self.inv.slot(t)) else {
                continue;
            };
            let info = bp::ResponseSlot {
                slot: r.slot,
                count: u8::try_from(slot.stack.count.clamp(0, 255)).unwrap_or(0),
                net_id: slot.net,
                durability: 0,
            };
            match containers.iter_mut().find(|(c, _)| *c == r.container) {
                Some((_, slots)) => {
                    if !slots.iter().any(|s| s.slot == r.slot) {
                        slots.push(info);
                    }
                }
                None => containers.push((r.container, vec![info])),
            }
        }
        bp::StackResponse {
            id: req.id,
            containers: Some(containers),
        }
    }

    /// Answers the requests of an `item_stack_request` packet.
    pub(super) fn stack_requests(&mut self, body: &[u8]) -> crate::io::Result<()> {
        let requests = bp::read_stack_requests(body, self.version)?;
        let responses: Vec<bp::StackResponse> =
            requests.iter().map(|r| self.stack_request(r)).collect();
        self.send(
            id::ITEM_STACK_RESPONSE,
            &bp::item_stack_response(&responses, self.version),
        );
        if std::mem::take(&mut self.inventory_resync) {
            self.send_inventory();
        }
        Ok(())
    }

    // ---- Java -> Bedrock --------------------------------------------------------------

    fn instance(&self, slot: &Slot) -> Vec<u8> {
        let mut out = Vec::with_capacity(16);
        self.version
            .palette
            .items()
            .write_instance(&mut out, &slot.stack, slot.net);
        out
    }

    /// Sends the whole inventory.
    pub(super) fn send_inventory(&mut self) {
        let items = self.version.palette.items();
        let mut main = Vec::with_capacity(36 * 12);
        for i in 0..36usize {
            let menu = if i < 9 { i.saturating_add(36) } else { i };
            if let Some(s) = self.inv.slots.get(menu) {
                items.write_instance(&mut main, &s.stack, s.net);
            }
        }
        self.send(
            id::INVENTORY_CONTENT,
            &bp::inventory_content(window::INVENTORY, 36, &main),
        );
        let mut armour = Vec::new();
        for menu in 5..9usize {
            if let Some(s) = self.inv.slots.get(menu) {
                items.write_instance(&mut armour, &s.stack, s.net);
            }
        }
        self.send(
            id::INVENTORY_CONTENT,
            &bp::inventory_content(window::ARMOUR, 4, &armour),
        );
        let offhand = self
            .inv
            .slots
            .get(45)
            .map(|s| self.instance(s))
            .unwrap_or_default();
        self.send(
            id::INVENTORY_CONTENT,
            &bp::inventory_content(window::OFFHAND, 1, &offhand),
        );
        for menu in 1..5usize {
            self.send_slot(menu);
        }
        self.send_cursor();
    }

    /// Sends one menu slot.
    fn send_slot(&mut self, menu: usize) {
        let (Some((w, s)), Some(slot)) = (bedrock_slot(menu), self.inv.slots.get(menu)) else {
            return;
        };
        let item = self.instance(slot);
        self.send(id::INVENTORY_SLOT, &bp::inventory_slot(w, s, &item));
    }

    fn send_cursor(&mut self) {
        let item = self.instance(&self.inv.cursor);
        self.send(
            id::INVENTORY_SLOT,
            &bp::inventory_slot(window::UI, 0, &item),
        );
    }

    pub(super) fn java_container_content(&mut self, c: pcb::ContainerSetContent) {
        if c.container_id != 0 {
            return;
        }
        self.inv.state_id = c.state_id;
        for (i, stack) in c.items.into_iter().enumerate().take(MENU) {
            self.inv.put(Target::Menu(i), stack);
        }
        self.inv.put(Target::Cursor, c.carried);
        self.send_inventory();
    }

    pub(super) fn java_container_slot(&mut self, c: pcb::ContainerSetSlot) {
        if c.container_id != 0 {
            return;
        }
        self.inv.state_id = c.state_id;
        if let Ok(i) = usize::try_from(c.slot)
            && self.inv.put(Target::Menu(i), c.item)
        {
            self.send_slot(i);
        }
    }

    pub(super) fn java_cursor(&mut self, item: ItemStack) {
        if self.inv.put(Target::Cursor, item) {
            self.send_cursor();
        }
    }

    pub(super) fn java_player_inventory(&mut self, slot: i32, item: ItemStack) {
        if let Some(menu) = menu_of_inventory(slot)
            && self.inv.put(Target::Menu(menu), item)
        {
            self.send_slot(menu);
        }
    }

    pub(super) fn java_held_slot(&mut self, slot: i32) {
        if let Ok(s) = u8::try_from(slot)
            && s < 9
        {
            self.inv.selected = s;
            self.send(id::PLAYER_HOTBAR, &bp::player_hotbar(u32::from(s)));
        }
    }

    /// Bedrock selected a hotbar slot.
    pub(super) fn select_hotbar(&mut self, slot: u8) {
        if slot < 9 && slot != self.inv.selected {
            self.inv.selected = slot;
            self.java_send(&psb::SetCarriedItem {
                slot: i16::from(slot),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_mapping_round_trips() {
        for menu in 1..MENU {
            let Some((w, s)) = bedrock_slot(menu) else {
                continue;
            };
            let container = match w {
                window::INVENTORY => container::INVENTORY,
                window::ARMOUR => container::ARMOUR,
                window::OFFHAND => container::OFFHAND,
                _ => container::CRAFTING_INPUT,
            };
            let r = SlotRef {
                container,
                slot: u8::try_from(s).unwrap_or(0),
                net_id: 0,
            };
            assert_eq!(target(&r), Some(Target::Menu(menu)), "menu {menu}");
        }
        assert_eq!(menu_of_inventory(39), Some(5));
        assert_eq!(menu_of_inventory(36), Some(8));
    }

    #[test]
    fn clicks_follow_java() {
        let mut inv = Inventory::default();
        let stone = vanillite_data::item::ItemId::by_name("minecraft:stone").map_or(1, |i| i.0);
        inv.put(Target::Menu(36), ItemStack::new(stone, 10));
        // Right click takes half, rounded up.
        inv.simulate_click(36, 1, ContainerInput::Pickup);
        assert_eq!(inv.cursor.stack.count, 5);
        assert_eq!(inv.get(Target::Menu(36)).count, 5);
        // Right click into an empty slot places one.
        inv.simulate_click(9, 1, ContainerInput::Pickup);
        assert_eq!(inv.get(Target::Menu(9)).count, 1);
        assert_eq!(inv.cursor.stack.count, 4);
        // Left click merges.
        inv.simulate_click(36, 0, ContainerInput::Pickup);
        assert_eq!(inv.get(Target::Menu(36)).count, 9);
        assert!(inv.cursor.stack.is_empty());
    }
}
