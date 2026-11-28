//! The player inventory: a mirror of the Java inventory menu with Bedrock stack ids, Bedrock's
//! item stack requests as Java clicks (survival) or creative slot changes (creative), and the
//! Java inventory updates as Bedrock inventory packets. An open Java menu (chest, furnace,
//! crafting table, ...) adds its own slots ([`super::container`]); the player inventory part of
//! that menu stays in the inventory mirror.
//!
//! Java menu slots: 0 crafting result, 1-4 crafting grid, 5-8 armour (head..feet), 9-35 main
//! inventory, 36-44 hotbar, 45 offhand. Bedrock: window 0 slots 0-8 hotbar and 9-35 main
//! inventory, window 119 the offhand, window 120 the armour, the cursor in the UI window.

use vanillite_data::item::ItemId;
use vanillite_protocol::item::ItemStack;
use vanillite_protocol::packets::play::clientbound as pcb;
use vanillite_protocol::packets::play::serverbound::{self as psb, ContainerInput, HashedStack};

use super::Session;
use super::container::{Kind, Open};
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
    /// The open Java menu other than the inventory.
    pub open: Option<Open>,
    /// Crafted items on the cursor that the request has not placed yet.
    crafted: i32,
    /// The result of the recipe the request crafts.
    recipe: Option<ItemStack>,
    /// The Java recipe of a recipe book craft: placed before each craft.
    auto: Option<i32>,
    /// The Java server made the result itself (enchanting): taking it changes nothing.
    server_made: bool,
    /// The filter strings of the request (the anvil's name field).
    filters: Vec<String>,
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
            open: None,
            crafted: 0,
            recipe: None,
            auto: None,
            server_made: false,
            filters: Vec::new(),
        }
    }
}

/// A slot named by a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Target {
    /// A slot of the Java inventory menu.
    Menu(usize),
    /// A slot of the open menu's own part.
    Open(usize),
    Cursor,
    Created,
}

/// The inventory menu slot of a Bedrock request slot.
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
    /// The slot of a Bedrock request slot, with the open menu.
    fn target(&self, r: &SlotRef) -> Option<Target> {
        let Some(open) = &self.open else {
            return target(r);
        };
        let s = usize::from(r.slot);
        let own = |i: usize| (i < open.slots.len()).then_some(Target::Open(i));
        match (open.kind, r.container) {
            (Kind::Crafting, container::CRAFTING_INPUT) => s
                .checked_sub(31)
                .filter(|i| (1..10).contains(i))
                .and_then(own),
            (_, container::CRAFTING_INPUT | container::ARMOUR | container::OFFHAND) => None,
            (
                Kind::Chest | Kind::Dispenser | Kind::Hopper,
                container::LEVEL_ENTITY | container::BARREL | container::SHULKER_BOX,
            ) => own(s),
            (
                Kind::Furnace,
                container::FURNACE_INGREDIENT
                | container::BLAST_FURNACE_INGREDIENT
                | container::SMOKER_INGREDIENT,
            ) => own(0),
            (Kind::Furnace, container::FURNACE_FUEL) => own(1),
            (Kind::Furnace, container::FURNACE_RESULT) => own(2),
            (Kind::Brewing, container::BREWING_INPUT) => own(3),
            (Kind::Brewing, container::BREWING_RESULT) => {
                s.checked_sub(1).filter(|&i| i < 3).and_then(own)
            }
            (Kind::Brewing, container::BREWING_FUEL) => own(4),
            (Kind::Crafter, container::CRAFTER) => own(s),
            (Kind::Anvil, container::ANVIL_INPUT)
            | (Kind::Enchanting, container::ENCHANTING_INPUT)
            | (Kind::Smithing, container::SMITHING_TEMPLATE)
            | (Kind::Stonecutter, container::STONECUTTER_INPUT)
            | (Kind::Grindstone, container::GRINDSTONE_INPUT)
            | (Kind::Cartography, container::CARTOGRAPHY_INPUT) => own(0),
            (Kind::Anvil, container::ANVIL_MATERIAL)
            | (Kind::Enchanting, container::ENCHANTING_MATERIAL)
            | (Kind::Smithing, container::SMITHING_INPUT)
            | (Kind::Grindstone, container::GRINDSTONE_ADDITIONAL)
            | (Kind::Cartography, container::CARTOGRAPHY_ADDITIONAL) => own(1),
            (Kind::Smithing, container::SMITHING_MATERIAL) => own(2),
            (Kind::Beacon, container::BEACON_PAYMENT) | (Kind::Loom, container::LOOM_INPUT) => {
                own(0)
            }
            (Kind::Loom, container::LOOM_DYE) => own(1),
            (Kind::Loom, container::LOOM_MATERIAL) => own(2),
            (Kind::Mount(m), container::HORSE_EQUIP | container::LEVEL_ENTITY) => own(m.java(s)),
            _ => target(r),
        }
    }

    /// The Java slot of a target in the open menu (`None`: outside the window, -999).
    fn java_slot(&self, t: Option<Target>) -> Result<i16, Refused> {
        let slot = match (t, &self.open) {
            (None, _) => return Ok(-999),
            (Some(Target::Open(i)), Some(o)) if i < o.slots.len() => i,
            (Some(Target::Menu(i)), Some(o)) if (9..45).contains(&i) => {
                o.slots.len().saturating_add(i).saturating_sub(9)
            }
            (Some(Target::Menu(i)), None) if (1..MENU).contains(&i) => i,
            (Some(Target::Created), None) => 0,
            (Some(Target::Created), Some(_)) => match self.result_slot() {
                Some(Target::Open(i)) => i,
                _ => return Err(Refused),
            },
            _ => return Err(Refused),
        };
        i16::try_from(slot).map_err(|_| Refused)
    }

    /// The crafting result slot of the open menu.
    fn result_slot(&self) -> Option<Target> {
        match &self.open {
            None => Some(Target::Menu(0)),
            Some(o) => match o.kind {
                Kind::Crafting => Some(Target::Open(0)),
                Kind::Stonecutter => Some(Target::Open(1)),
                Kind::Anvil | Kind::Grindstone | Kind::Cartography => Some(Target::Open(2)),
                Kind::Smithing | Kind::Loom => Some(Target::Open(3)),
                _ => None,
            },
        }
    }

    fn fresh(&mut self) -> i32 {
        self.next_net = self.next_net.wrapping_add(1).max(1);
        self.next_net
    }

    fn get(&self, t: Target) -> ItemStack {
        self.slot(t).map(|s| s.stack.clone()).unwrap_or_default()
    }

    fn slot(&self, t: Target) -> Option<&Slot> {
        match t {
            Target::Menu(i) => self.slots.get(i),
            Target::Open(i) => self.open.as_ref()?.slots.get(i),
            Target::Cursor => Some(&self.cursor),
            Target::Created => None,
        }
    }

    /// Sets a slot; a changed stack gets a new stack id. Returns whether it changed.
    pub(super) fn put(&mut self, t: Target, stack: ItemStack) -> bool {
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
            Target::Open(i) => match self.open.as_mut().and_then(|o| o.slots.get_mut(i)) {
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
    fn simulate_click(&mut self, t: Option<Target>, button: i8, input: ContainerInput) {
        let mut c = self.cursor.stack.clone();
        let Some(t) = t else {
            if input == ContainerInput::Pickup && !c.is_empty() {
                if button == 0 {
                    c = ItemStack::EMPTY;
                } else {
                    c.shrink(1);
                }
                self.put(Target::Cursor, c);
            }
            return;
        };
        if matches!(t, Target::Cursor | Target::Created | Target::Menu(0)) {
            return;
        }
        let mut s = self.get(t);
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
        self.put(t, s);
        self.put(Target::Cursor, c);
    }
}

/// A request that cannot be translated.
#[derive(Debug)]
struct Refused;

impl Session {
    /// One Java click: applied to the mirror and sent.
    fn click(
        &mut self,
        t: Option<Target>,
        button: i8,
        input: ContainerInput,
    ) -> Result<(), Refused> {
        let slot = self.inv.java_slot(t)?;
        self.send_click(slot, button, input)?;
        self.inv.simulate_click(t, button, input);
        Ok(())
    }

    /// Sends one Java click without changing the mirror.
    fn send_click(&mut self, slot: i16, button: i8, input: ContainerInput) -> Result<(), Refused> {
        self.inv.clicks = self.inv.clicks.saturating_add(1);
        if self.inv.clicks > MAX_CLICKS {
            return Err(Refused);
        }
        let (container_id, state_id) = self
            .inv
            .open
            .as_ref()
            .map_or((0, self.inv.state_id), |o| (o.container, o.state_id));
        self.java_send(&psb::ContainerClick {
            container_id,
            state_id,
            slot,
            button,
            input,
            changed: Vec::new(),
            carried: HashedStack::default(),
        });
        Ok(())
    }

    /// Crafts `n` items onto the cursor (clicks on the result slot) and places them in `dst`.
    fn craft_take(&mut self, n: i32, dst: Target) -> Result<(), Refused> {
        let result_slot = self.inv.result_slot().ok_or(Refused)?;
        let result = self
            .inv
            .recipe
            .clone()
            .unwrap_or_else(|| self.inv.get(result_slot));
        if result.is_empty() || n <= 0 {
            return Err(Refused);
        }
        let java = self.inv.java_slot(Some(Target::Created))?;
        while self.inv.crafted < n {
            let c = self.inv.cursor.stack.clone();
            let grown = if c.is_empty() {
                result.clone()
            } else if same(&c, &result) && c.count.saturating_add(result.count) <= max_stack(&c) {
                c.with_count(c.count.saturating_add(result.count))
            } else {
                return Err(Refused);
            };
            if let Some(recipe) = self.inv.auto {
                let container_id = self.inv.open.as_ref().map_or(0, |o| o.container);
                self.java_send(&psb::PlaceRecipe {
                    container_id,
                    recipe,
                    use_max_items: false,
                });
            }
            self.send_click(java, 0, ContainerInput::Pickup)?;
            self.inv.put(Target::Cursor, grown);
            self.inv.crafted = self.inv.crafted.saturating_add(result.count.max(1));
        }
        self.inv.crafted = self.inv.crafted.saturating_sub(n);
        if dst == Target::Cursor {
            return Ok(());
        }
        self.place_from_cursor(dst, n)
    }

    /// Takes `n` items of menu slot `t` onto the (Java) cursor.
    fn take_to_cursor(&mut self, t: Target, n: i32) -> Result<(), Refused> {
        let slot = Some(t);
        self.inv.java_slot(slot)?;
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
        let slot = Some(t);
        self.inv.java_slot(slot)?;
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
        let inv = &self.inv;
        let t = |r: &SlotRef| inv.target(r).ok_or(Refused);
        match *a {
            StackAction::Take { count, src, dst } | StackAction::Place { count, src, dst } => {
                let (src, dst, n) = (t(&src)?, t(&dst)?, i32::from(count));
                match (src, dst) {
                    // The enchanted item is already in its slot on the Java side.
                    (Target::Created, _) if self.inv.server_made => Ok(()),
                    (Target::Created, d) if d != Target::Created => self.craft_take(n, d),
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
                (Target::Cursor, Target::Cursor) | (Target::Created, _) | (_, Target::Created) => {
                    Err(Refused)
                }
                (Target::Cursor, other) | (other, Target::Cursor) => {
                    self.click(Some(other), 0, ContainerInput::Pickup)
                }
                (a, b) => {
                    if !self.inv.cursor.stack.is_empty() {
                        return Err(Refused);
                    }
                    self.click(Some(a), 0, ContainerInput::Pickup)?;
                    self.click(Some(b), 0, ContainerInput::Pickup)?;
                    self.click(Some(a), 0, ContainerInput::Pickup)
                }
            },
            StackAction::Drop { count, src } => {
                let n = i32::from(count);
                match t(&src)? {
                    Target::Cursor => {
                        let c = self.inv.cursor.stack.count;
                        if n == c {
                            return self.click(None, 0, ContainerInput::Pickup);
                        }
                        for _ in 0..n.min(c) {
                            self.click(None, 1, ContainerInput::Pickup)?;
                        }
                        Ok(())
                    }
                    s @ (Target::Menu(_) | Target::Open(_)) => {
                        let slot = Some(s);
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
            StackAction::Consume { count, src } => {
                // The Java server takes the ingredients itself; the mirror follows.
                let s = t(&src)?;
                let mut rest = self.inv.get(s);
                rest.shrink(i32::from(count));
                self.inv.put(s, rest);
                Ok(())
            }
            StackAction::CraftRecipe { net_id, .. } => self.craft_recipe(net_id),
            StackAction::CraftRecipeOptional { filter, .. } => {
                if self
                    .inv
                    .open
                    .as_ref()
                    .is_some_and(|o| o.kind == Kind::Anvil)
                {
                    // The anvil's name field; empty keeps no custom name, as on Bedrock.
                    let name = usize::try_from(filter)
                        .ok()
                        .and_then(|i| self.inv.filters.get(i))
                        .cloned()
                        .unwrap_or_default();
                    self.java_send(&psb::RenameItem { name });
                }
                Ok(())
            }
            StackAction::CraftGrindstone => Ok(()),
            StackAction::BeaconPayment { primary, secondary } => {
                if !self
                    .inv
                    .open
                    .as_ref()
                    .is_some_and(|o| o.kind == Kind::Beacon)
                {
                    return Err(Refused);
                }
                self.java_send(&psb::SetBeacon {
                    primary: crate::entity::java_effect(primary),
                    secondary: crate::entity::java_effect(secondary),
                });
                Ok(())
            }
            StackAction::CraftLoom { pattern } => {
                let len = pattern
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(pattern.len());
                let code = std::str::from_utf8(pattern.get(..len).unwrap_or_default())
                    .map_err(|_| Refused)?;
                self.craft_loom(code)
            }
            StackAction::CraftRecipeAuto { net_id, .. } => {
                self.inv.recipe = self.recipes.result(net_id);
                self.inv.auto = Some(self.recipes.java(net_id).ok_or(Refused)?);
                Ok(())
            }
            StackAction::MineBlock | StackAction::Create | StackAction::Results => Ok(()),
            _ => Err(Refused),
        }
    }

    /// `craft_recipe`: a crafting, stonecutter or smithing recipe, or an enchanting option.
    fn craft_recipe(&mut self, net_id: u32) -> Result<(), Refused> {
        let Some(open) = &self.inv.open else {
            self.inv.recipe = self.recipes.result(net_id);
            return Ok(());
        };
        let container_id = open.container;
        match open.kind {
            Kind::Enchanting => {
                let option = net_id
                    .checked_sub(super::container::ENCHANT_NET_ID)
                    .filter(|&o| o < 3)
                    .ok_or(Refused)?;
                self.java_send(&psb::ContainerButtonClick {
                    container_id,
                    button_id: i32::try_from(option).map_err(|_| Refused)?,
                });
                self.inv.server_made = true;
            }
            Kind::Stonecutter => {
                let input = self.inv.get(Target::Open(0));
                let button = self
                    .recipes
                    .stonecutter_button(net_id, input.item)
                    .filter(|_| !input.is_empty())
                    .ok_or(Refused)?;
                self.java_send(&psb::ContainerButtonClick {
                    container_id,
                    button_id: button,
                });
                self.inv.recipe = self.recipes.result(net_id);
            }
            _ => self.inv.recipe = self.recipes.result(net_id),
        }
        Ok(())
    }

    /// `craft_loom`: the Java button of the pattern in the loom's list (`LoomMenu`'s selectable
    /// patterns: the pattern item's patterns, or those that need none), and the expected result.
    fn craft_loom(&mut self, code: &str) -> Result<(), Refused> {
        let container_id = self
            .inv
            .open
            .as_ref()
            .filter(|o| o.kind == Kind::Loom)
            .ok_or(Refused)?
            .container;
        let java = crate::blockentity::java_pattern(code).ok_or(Refused)?;
        let pattern_item = self.inv.get(Target::Open(2));
        let tag = if pattern_item.is_empty() {
            "minecraft:no_item_required".to_owned()
        } else {
            let name = ItemId(pattern_item.item).name();
            let base = name
                .strip_prefix("minecraft:")
                .and_then(|n| n.strip_suffix("_banner_pattern"))
                .ok_or(Refused)?;
            format!("minecraft:pattern_item/{base}")
        };
        let registry_id = |id: &i32| {
            usize::try_from(*id)
                .ok()
                .and_then(|i| self.banner_patterns.get(i))
        };
        let list = self.banner_tags.get(&tag).ok_or(Refused)?;
        let button = list
            .iter()
            .position(|id| registry_id(id).is_some_and(|n| *n == java))
            .ok_or(Refused)?;
        let pattern_id = *list.get(button).ok_or(Refused)?;
        self.java_send(&psb::ContainerButtonClick {
            container_id,
            button_id: i32::try_from(button).map_err(|_| Refused)?,
        });
        // `LoomMenu.setupResultSlot`: the banner with one more layer in the dye's colour.
        let banner = self.inv.get(Target::Open(0));
        let dye = self.inv.get(Target::Open(1));
        let color = crate::blockentity::dye_of_item(ItemId(dye.item).name()).ok_or(Refused)?;
        if banner.is_empty() || dye.is_empty() {
            return Err(Refused);
        }
        let mut result = banner.with_count(1);
        let component = vanillite_protocol::components::ids::BANNER_PATTERNS;
        let (count, layers) = match result.component(component) {
            Some(raw) => {
                let mut r = vanillite_protocol::codec::Reader::new(raw);
                let n = r.varint().map_err(|_| Refused)?;
                (n, r.rest().to_vec())
            }
            None => (0, Vec::new()),
        };
        let mut value = Vec::with_capacity(layers.len().saturating_add(8));
        vanillite_protocol::codec::write_varint(&mut value, count.saturating_add(1));
        value.extend_from_slice(&layers);
        vanillite_protocol::codec::write_varint(&mut value, pattern_id.saturating_add(1));
        vanillite_protocol::codec::write_varint(&mut value, i32::try_from(color).unwrap_or(0));
        result.set_component(component, value.into_boxed_slice());
        self.inv.recipe = Some(result);
        Ok(())
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
                let stack = self
                    .version
                    .palette
                    .items()
                    .creative(net_id, &self.enchantments)
                    .ok_or(Refused)?;
                let max = max_stack(&stack);
                self.inv.created = Some(stack.with_count(max));
                Ok(())
            }
            StackAction::Take { count, src, dst } | StackAction::Place { count, src, dst } => {
                let (src, dst, n) = (t(&src)?, t(&dst)?, i32::from(count));
                if matches!(src, Target::Created) && self.inv.created.is_none() {
                    return self.survival_action(a);
                }
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
            StackAction::MineBlock | StackAction::Create | StackAction::Results => Ok(()),
            // Crafting in creative works like in survival.
            _ => self.survival_action(a),
        }
    }

    /// Answers one item stack request.
    pub(super) fn stack_request(&mut self, req: &StackRequest) -> bp::StackResponse {
        // Java creative slots only reach the inventory menu: an open menu takes clicks.
        let creative = self.inv.open.is_none()
            && self
                .world
                .as_ref()
                .is_some_and(|w| w.game_mode == pcb::GameMode::Creative);
        self.inv.clicks = 0;
        self.inv.crafted = 0;
        self.inv.recipe = None;
        self.inv.auto = None;
        self.inv.created = None;
        self.inv.server_made = false;
        self.inv.filters.clone_from(&req.filters);
        let mut ok = true;
        for a in &req.actions {
            let r = if creative {
                self.creative_action(a)
            } else {
                self.survival_action(a)
            };
            if r.is_err() {
                log::debug!(
                    "Bedrock {}: item stack request {} refused at {a:?}",
                    self.peer,
                    req.id
                );
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
            let Some(slot) = self.inv.target(&r).and_then(|t| self.inv.slot(t)) else {
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

    pub(super) fn instance(&self, slot: &Slot) -> Vec<u8> {
        let mut out = Vec::with_capacity(16);
        self.version.palette.items().write_instance(
            &mut out,
            &slot.stack,
            slot.net,
            &self.enchantments,
        );
        out
    }

    /// Sends the whole inventory.
    pub(super) fn send_inventory(&mut self) {
        let items = self.version.palette.items();
        let mut main = Vec::with_capacity(36 * 12);
        for i in 0..36usize {
            let menu = if i < 9 { i.saturating_add(36) } else { i };
            if let Some(s) = self.inv.slots.get(menu) {
                items.write_instance(&mut main, &s.stack, s.net, &self.enchantments);
            }
        }
        self.send(
            id::INVENTORY_CONTENT,
            &bp::inventory_content(window::INVENTORY, 36, &main),
        );
        let mut armour = Vec::new();
        for menu in 5..9usize {
            if let Some(s) = self.inv.slots.get(menu) {
                items.write_instance(&mut armour, &s.stack, s.net, &self.enchantments);
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
    pub(super) fn send_slot(&mut self, menu: usize) {
        let (Some((w, s)), Some(slot)) = (bedrock_slot(menu), self.inv.slots.get(menu)) else {
            return;
        };
        let item = self.instance(slot);
        self.send(id::INVENTORY_SLOT, &bp::inventory_slot(w, s, &item));
    }

    pub(super) fn send_cursor(&mut self) {
        let item = self.instance(&self.inv.cursor);
        self.send(
            id::INVENTORY_SLOT,
            &bp::inventory_slot(window::UI, 0, &item),
        );
    }

    pub(super) fn java_container_content(&mut self, c: pcb::ContainerSetContent) {
        if c.container_id != 0 {
            if self.merchant_state(c.container_id, c.state_id) {
                for (i, stack) in c.items.into_iter().enumerate() {
                    self.merchant_inventory_slot(i, stack);
                }
            } else {
                self.open_content(c);
            }
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
            if self.merchant_state(c.container_id, c.state_id) {
                if let Ok(i) = usize::try_from(c.slot) {
                    self.merchant_inventory_slot(i, c.item);
                }
            } else {
                self.open_slot(c);
            }
            return;
        }
        self.inv.state_id = c.state_id;
        if let Ok(i) = usize::try_from(c.slot)
            && self.inv.put(Target::Menu(i), c.item)
        {
            self.send_slot(i);
        }
    }

    /// Slot `i` of an open merchant menu: its player inventory part (after the 3 trade slots)
    /// goes into the mirror.
    pub(super) fn merchant_inventory_slot(&mut self, i: usize, stack: ItemStack) {
        let Some(k) = i.checked_sub(3) else {
            return;
        };
        // Main inventory then hotbar, in the order of the inventory menu (slots 9-44).
        if k >= 36 {
            return;
        }
        let menu = k.saturating_add(9);
        if self.inv.put(Target::Menu(menu), stack) {
            self.send_slot(menu);
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
        inv.simulate_click(Some(Target::Menu(36)), 1, ContainerInput::Pickup);
        assert_eq!(inv.cursor.stack.count, 5);
        assert_eq!(inv.get(Target::Menu(36)).count, 5);
        // Right click into an empty slot places one.
        inv.simulate_click(Some(Target::Menu(9)), 1, ContainerInput::Pickup);
        assert_eq!(inv.get(Target::Menu(9)).count, 1);
        assert_eq!(inv.cursor.stack.count, 4);
        // Left click merges.
        inv.simulate_click(Some(Target::Menu(36)), 0, ContainerInput::Pickup);
        assert_eq!(inv.get(Target::Menu(36)).count, 9);
        assert!(inv.cursor.stack.is_empty());
    }
}
