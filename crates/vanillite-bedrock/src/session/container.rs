//! Java menus as Bedrock containers: chests, barrels, shulker boxes, ender chests, hoppers,
//! dispensers and droppers, furnaces, brewing stands and crafting tables open at the block the
//! player used (chest minecarts, chest boats and hopper minecarts at the entity). A menu with no
//! block or entity behind it (a plugin menu) becomes a form whose buttons click the slots.
//!
//! The menu's own slots are mirrored in [`Open`]; its player inventory part maps onto the
//! inventory mirror. Bedrock requests on these slots become Java clicks in the open menu
//! ([`super::inventory`]).

use std::time::{Duration, Instant};

use vanillite_protocol::codec::Reader as JReader;
use vanillite_protocol::item::ItemStack;
use vanillite_protocol::packets::play::clientbound as pcb;
use vanillite_protocol::packets::play::serverbound::{self as psb, ContainerInput, HashedStack};

use super::Session;
use super::inventory::{Slot, Target};
use crate::forms::{Button, Form};
use crate::packets::{self as bp, container_type as ct, id, window};

/// Form id of a menu shown as a form.
pub(super) const MENU_FORM: u32 = u32::MAX - 1;

/// How long a block or entity use may open a menu.
const USE_WINDOW: Duration = Duration::from_secs(1);

/// Slots of the player inventory part of a menu.
const INVENTORY_PART: usize = 36;

/// The kind of an open menu: how its slots map to Bedrock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    /// `generic_9xN` and `shulker_box`.
    Chest,
    /// `generic_3x3`.
    Dispenser,
    Hopper,
    /// `furnace`, `blast_furnace`, `smoker`: ingredient, fuel, result.
    Furnace,
    /// `brewing_stand`: three bottles, ingredient, fuel.
    Brewing,
    /// `crafting`: result, 3x3 grid.
    Crafting,
}

/// The open Java menu.
#[derive(Debug)]
pub(super) struct Open {
    pub container: i32,
    pub window: u8,
    pub kind: Kind,
    pub slots: Vec<Slot>,
    pub state_id: i32,
    /// Bedrock container type (for `container_close`).
    container_type: i8,
    /// The Java total cooking time of a furnace (Bedrock's progress bar is 200 ticks).
    cook: i32,
    /// Shown as a form (no block or entity to open it at).
    form: bool,
    title: String,
}

/// What the player used last: a block or an entity (Java id).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Used {
    Block([i32; 3]),
    Entity(i32),
}

/// The kind, size and Bedrock container type of a Java menu type.
fn menu_kind(menu: &str) -> Option<(Kind, usize, i8)> {
    Some(match menu {
        "minecraft:generic_9x1" => (Kind::Chest, 9, ct::CONTAINER),
        "minecraft:generic_9x2" => (Kind::Chest, 18, ct::CONTAINER),
        "minecraft:generic_9x3" | "minecraft:shulker_box" => (Kind::Chest, 27, ct::CONTAINER),
        "minecraft:generic_9x4" => (Kind::Chest, 36, ct::CONTAINER),
        "minecraft:generic_9x5" => (Kind::Chest, 45, ct::CONTAINER),
        "minecraft:generic_9x6" => (Kind::Chest, 54, ct::CONTAINER),
        "minecraft:generic_3x3" => (Kind::Dispenser, 9, ct::DISPENSER),
        "minecraft:hopper" => (Kind::Hopper, 5, ct::HOPPER),
        "minecraft:furnace" => (Kind::Furnace, 3, ct::FURNACE),
        "minecraft:blast_furnace" => (Kind::Furnace, 3, ct::BLAST_FURNACE),
        "minecraft:smoker" => (Kind::Furnace, 3, ct::SMOKER),
        "minecraft:brewing_stand" => (Kind::Brewing, 5, ct::BREWING_STAND),
        "minecraft:crafting" => (Kind::Crafting, 10, ct::WORKBENCH),
        _ => return None,
    })
}

/// The Bedrock window and slot of a slot of the open menu.
fn bedrock_slot(open: &Open, i: usize) -> Option<(u32, u32)> {
    let w = u32::from(open.window);
    let s = u32::try_from(i).ok()?;
    Some(match open.kind {
        // Bedrock: ingredient, three bottles, fuel.
        Kind::Brewing => match i {
            0..=2 => (w, s.saturating_add(1)),
            3 => (w, 0),
            4 => (w, 4),
            _ => return None,
        },
        Kind::Crafting => match i {
            1..=9 => (window::UI, s.saturating_add(31)),
            _ => return None,
        },
        _ => (w, s),
    })
}

/// "Stone" or "16 Stone" or the custom name of a stack.
fn stack_label(stack: &ItemStack) -> String {
    let custom = [
        vanillite_protocol::components::ids::CUSTOM_NAME,
        vanillite_protocol::components::ids::ITEM_NAME,
    ]
    .into_iter()
    .find_map(|c| stack.component(c))
    .and_then(|raw| crate::entity::component(&mut JReader::new(raw)).ok())
    .map(|c| c.plain());
    let name = custom.unwrap_or_else(|| super::trade::item_name(stack.item));
    if stack.count > 1 {
        format!("{} {name}", stack.count)
    } else {
        name
    }
}

impl Session {
    /// Remembers what the player used: a menu opening next opens at it.
    pub(super) fn note_use(&mut self, used: Used) {
        self.used = Some((used, Instant::now()));
    }

    /// `open_screen` of a menu other than a merchant's: opens it on Bedrock.
    pub(super) fn open_menu(&mut self, container: i32, menu: &str, title: String) {
        self.close_open_window(true);
        let Some((kind, size, mut container_type)) = menu_kind(menu) else {
            // Not translated yet (anvil, enchanting, smithing, ...): close it again.
            self.java_send(&psb::ContainerClose {
                container_id: container,
            });
            self.send(
                id::TEXT,
                &bp::text_tip("This menu is not available on Bedrock yet"),
            );
            return;
        };
        let used = self
            .used
            .take()
            .filter(|(_, at)| at.elapsed() <= USE_WINDOW)
            .map(|(u, _)| u);
        let (pos, entity) = match used {
            Some(Used::Block(pos)) => (Some(pos), -1),
            Some(Used::Entity(java)) => {
                let target = self.world.as_ref().and_then(|w| {
                    let e = w.entities.get(&java)?;
                    let pos = e.pos.map(crate::num::floor_i32);
                    Some((pos, super::uid(java), e.java_type))
                });
                match target {
                    Some((pos, uid, java_type)) => {
                        container_type = match super::play::java_type_name(java_type) {
                            "minecraft:hopper_minecart" => ct::CART_HOPPER,
                            "minecraft:chest_minecart" => ct::CART_CHEST,
                            n if n.ends_with("_chest_boat") || n.ends_with("_chest_raft") => {
                                ct::CHEST_BOAT
                            }
                            _ => container_type,
                        };
                        (Some(pos), uid)
                    }
                    None => (None, -1),
                }
            }
            None => (None, -1),
        };
        self.next_window = if self.next_window >= 99 {
            1
        } else {
            self.next_window.saturating_add(1)
        };
        let window = self.next_window;
        let java_total = if container_type == ct::FURNACE {
            200
        } else {
            100
        };
        self.inv.open = Some(Open {
            container,
            window,
            kind,
            slots: vec![Slot::default(); size],
            state_id: 0,
            container_type,
            cook: java_total,
            form: pos.is_none(),
            title,
        });
        if let Some(pos) = pos {
            self.send(
                id::CONTAINER_OPEN,
                &bp::container_open(window, container_type, pos, entity),
            );
        }
    }

    /// Closes the Bedrock window of the open menu (`server_side`: the server closed it).
    pub(super) fn close_open_window(&mut self, server_side: bool) {
        if let Some(open) = self.inv.open.take()
            && !open.form
        {
            self.send(
                id::CONTAINER_CLOSE,
                &bp::container_close(open.window, open.container_type, server_side),
            );
        }
    }

    /// The client closed a window: closes the Java menu. Returns whether it was the open menu.
    pub(super) fn client_closed(&mut self, window: u8) -> bool {
        let Some(open) = self.inv.open.as_ref().filter(|o| o.window == window) else {
            return false;
        };
        let (container, container_type) = (open.container, open.container_type);
        self.inv.open = None;
        self.java_send(&psb::ContainerClose {
            container_id: container,
        });
        self.send(
            id::CONTAINER_CLOSE,
            &bp::container_close(window, container_type, false),
        );
        true
    }

    /// The server closed a menu.
    pub(super) fn java_menu_closed(&mut self, container: i32) {
        if self
            .inv
            .open
            .as_ref()
            .is_some_and(|o| o.container == container)
        {
            self.close_open_window(true);
        }
    }

    /// Sends one slot of the open menu.
    fn send_open_slot(&mut self, i: usize) {
        let Some(open) = &self.inv.open else {
            return;
        };
        if open.form {
            return;
        }
        let (Some((w, s)), Some(slot)) = (bedrock_slot(open, i), open.slots.get(i)) else {
            return;
        };
        let item = self.instance(slot);
        self.send(id::INVENTORY_SLOT, &bp::inventory_slot(w, s, &item));
    }

    /// Sends all slots of the open menu.
    fn send_open_content(&mut self) {
        let Some(open) = &self.inv.open else {
            return;
        };
        if open.form {
            self.show_menu_form();
            return;
        }
        if matches!(open.kind, Kind::Brewing | Kind::Crafting) {
            for i in 0..open.slots.len() {
                self.send_open_slot(i);
            }
            return;
        }
        let items = self.version.palette.items();
        let mut body = Vec::with_capacity(open.slots.len().saturating_mul(12));
        for s in &open.slots {
            items.write_instance(&mut body, &s.stack, s.net);
        }
        let packet = bp::inventory_content(u32::from(open.window), open.slots.len(), &body);
        self.send(id::INVENTORY_CONTENT, &packet);
    }

    /// `container_set_content` of the open menu.
    pub(super) fn open_content(&mut self, c: pcb::ContainerSetContent) {
        let Some(open) = self
            .inv
            .open
            .as_mut()
            .filter(|o| o.container == c.container_id)
        else {
            return;
        };
        open.state_id = c.state_id;
        let size = open.slots.len();
        let mut items = c.items.into_iter();
        for i in 0..size {
            let stack = items.next().unwrap_or_default();
            self.inv.put(Target::Open(i), stack);
        }
        for k in 0..INVENTORY_PART {
            let stack = items.next().unwrap_or_default();
            self.inv.put(Target::Menu(k.saturating_add(9)), stack);
        }
        self.inv.put(Target::Cursor, c.carried);
        self.send_open_content();
        self.send_inventory();
    }

    /// `container_set_slot` of the open menu.
    pub(super) fn open_slot(&mut self, c: pcb::ContainerSetSlot) {
        let Some(open) = self
            .inv
            .open
            .as_mut()
            .filter(|o| o.container == c.container_id)
        else {
            return;
        };
        open.state_id = c.state_id;
        let size = open.slots.len();
        let form = open.form;
        let Ok(i) = usize::try_from(c.slot) else {
            return;
        };
        if i < size {
            if self.inv.put(Target::Open(i), c.item) {
                if form {
                    self.show_menu_form();
                } else {
                    self.send_open_slot(i);
                }
            }
        } else if let Some(k) = i.checked_sub(size).filter(|&k| k < INVENTORY_PART) {
            let menu = k.saturating_add(9);
            if self.inv.put(Target::Menu(menu), c.item) {
                self.send_slot(menu);
            }
        }
    }

    /// `container_set_data`: furnace and brewing stand progress.
    pub(super) fn java_container_data(&mut self, r: &mut JReader<'_>) {
        let Ok(d) = <pcb::ContainerSetData as vanillite_protocol::codec::Decode>::decode(r) else {
            return;
        };
        let Some(open) = self
            .inv
            .open
            .as_mut()
            .filter(|o| o.container == d.container_id && !o.form)
        else {
            return;
        };
        let window = open.window;
        let value = i32::from(d.value);
        let data: &[(i32, i32)] = match (open.kind, d.id) {
            // Java: lit time, lit duration, cooking progress, total cooking time.
            (Kind::Furnace, 0) => &[(1, value)],
            (Kind::Furnace, 1) => &[(2, value)],
            (Kind::Furnace, 2) => {
                let ticks = i64::from(value)
                    .saturating_mul(200)
                    .checked_div(i64::from(open.cook.max(1)))
                    .unwrap_or(0);
                let ticks = i32::try_from(ticks).unwrap_or(0);
                self.send(
                    id::CONTAINER_SET_DATA,
                    &bp::container_set_data(window, 0, ticks),
                );
                return;
            }
            (Kind::Furnace, 3) => {
                open.cook = value.max(1);
                return;
            }
            // Java: brew time, fuel; Bedrock also wants the full fuel.
            (Kind::Brewing, 0) => &[(0, value)],
            (Kind::Brewing, 1) => &[(1, value), (2, 20)],
            _ => return,
        };
        for &(key, v) in data {
            self.send(
                id::CONTAINER_SET_DATA,
                &bp::container_set_data(window, key, v),
            );
        }
    }

    /// Shows a menu without block or entity as a form: one button per filled slot.
    fn show_menu_form(&mut self) {
        let Some(open) = self.inv.open.as_ref().filter(|o| o.form) else {
            return;
        };
        let buttons = open
            .slots
            .iter()
            .filter(|s| !s.stack.is_empty())
            .map(|s| Button {
                text: stack_label(&s.stack),
                image: None,
            })
            .collect();
        let form = Form::Simple {
            title: open.title.clone(),
            content: String::new(),
            buttons,
        };
        self.send(
            id::MODAL_FORM_REQUEST,
            &bp::modal_form_request(MENU_FORM, &form.to_json()),
        );
    }

    /// The answer to a menu form: a click on the slot of the button, or the menu closed.
    pub(super) fn menu_form_answer(&mut self, data: Option<&str>) {
        let Some(open) = self.inv.open.as_ref().filter(|o| o.form) else {
            return;
        };
        let choice = data
            .and_then(|d| d.trim().parse::<usize>().ok())
            .and_then(|b| {
                open.slots
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| !s.stack.is_empty())
                    .nth(b)
                    .map(|(i, _)| i)
            });
        let (container, state_id) = (open.container, open.state_id);
        let Some(slot) = choice.and_then(|i| i16::try_from(i).ok()) else {
            self.inv.open = None;
            self.java_send(&psb::ContainerClose {
                container_id: container,
            });
            return;
        };
        self.java_send(&psb::ContainerClick {
            container_id: container,
            state_id,
            slot,
            button: 0,
            input: ContainerInput::Pickup,
            changed: Vec::new(),
            carried: HashedStack::default(),
        });
        self.show_menu_form();
    }

    /// `block_event`: chest and shulker box lids.
    pub(super) fn java_block_event(&mut self, r: &mut JReader<'_>) {
        let (Ok(pos), Ok(action), Ok(param), Ok(block)) = (
            <vanillite_protocol::types::BlockPos as vanillite_protocol::codec::Decode>::decode(r),
            r.u8(),
            r.u8(),
            r.varint(),
        ) else {
            return;
        };
        let name = u16::try_from(block)
            .ok()
            .and_then(|b| vanillite_data::registry::BLOCK.name(b))
            .unwrap_or("");
        if action == 1 && (name.ends_with("chest") || name.ends_with("shulker_box")) {
            self.send(
                id::BLOCK_EVENT,
                &bp::block_event([pos.x, pos.y, pos.z], 1, i32::from(param > 0)),
            );
        }
    }
}
