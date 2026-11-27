//! The mount inventory menus (`AbstractMountInventoryMenu`, `HorseInventoryMenu`,
//! `NautilusInventoryMenu`): the saddle and body armour slots of a horse, donkey, mule, llama,
//! skeleton or zombie horse, camel or nautilus, then the columns of its chest.
//!
//! The equipment slots are the mob's own (`createEquipmentSlotContainer`, `ArmorSlot`), the
//! chest is the companion state's inventory. The client opens the screen from
//! `mount_screen_open` and finds the mob by its id.

use vanillite_base::BlockPos;
use vanillite_base::random::RandomSource;
use vanillite_world::entity::EntityId;

use super::container::{ContainerView, MenuKind, MenuRequest, Storage};
use super::{ClickContext, MenuSlots};
use crate::entities::Entities;
use crate::inventory::Drop;
use crate::item::{self, EquipmentSlot, ItemStack};
use crate::mob::kinds::Kind;
use crate::mob::{MobData, fam::companion};
use crate::predicate::EntityTags;

/// `SLOT_SADDLE`, `SLOT_BODY_ARMOR`, `SLOT_INVENTORY_START`.
const SADDLE: usize = 0;
const BODY: usize = 1;
const CHEST: usize = 2;

/// Whether a kind has a mount inventory menu (`HasCustomInventoryScreen` of the mobs).
pub fn is_mount(kind: Kind) -> bool {
    is_horse(kind) || is_nautilus(kind)
}

fn is_horse(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::Horse
            | Kind::Donkey
            | Kind::Mule
            | Kind::Llama
            | Kind::TraderLlama
            | Kind::SkeletonHorse
            | Kind::ZombieHorse
            | Kind::Camel
            | Kind::CamelHusk
    )
}

fn is_nautilus(kind: Kind) -> bool {
    matches!(kind, Kind::Nautilus | Kind::ZombieNautilus)
}

/// `getInventoryColumns`: five for a chested donkey or mule, the strength of a chested llama.
pub fn columns(m: &MobData) -> u8 {
    if !is_horse(m.kind) {
        return 0;
    }
    u8::try_from(companion::horse::inventory_size(m) / 3).unwrap_or(0)
}

/// `isTamed` (camels always are).
fn tamed(m: &MobData) -> bool {
    m.tame || matches!(m.kind, Kind::Camel | Kind::CamelHusk)
}

/// `openCustomInventoryScreen` of a horse or nautilus: the menu a player opens, if the mount
/// is tame and carries nobody but the player (camels open for anyone).
pub fn request(ents: &Entities, mount: EntityId, player: EntityId) -> Option<MenuRequest> {
    let m = ents.mob(mount)?;
    if !is_mount(m.kind) {
        return None;
    }
    if !matches!(m.kind, Kind::Camel | Kind::CamelHusk) {
        let passengers = ents.riding.passengers_of(mount);
        if !m.tame || (!passengers.is_empty() && !passengers.contains(&player)) {
            return None;
        }
    }
    let pos = ents.store.get(mount)?.pos();
    Some(MenuRequest {
        player: Some(player),
        menu: MenuKind::Mount {
            columns: columns(m),
        },
        storage: Storage::Mount(mount),
        title: vanillite_protocol::text::Component::text(""),
        pos: BlockPos::containing(pos.x, pos.y, pos.z),
        block: vanillite_data::block::BlockId(0),
    })
}

/// `canUseSlot(slot)` of a mount.
fn can_use_slot(m: &MobData, slot: EquipmentSlot) -> bool {
    let guarded = match m.kind {
        Kind::Donkey | Kind::Mule | Kind::Camel | Kind::CamelHusk => slot == EquipmentSlot::Saddle,
        Kind::Nautilus | Kind::ZombieNautilus => {
            matches!(slot, EquipmentSlot::Saddle | EquipmentSlot::Body)
        }
        _ => false,
    };
    !guarded || (m.is_alive() && !m.baby && tamed(m))
}

/// `isEquippableInSlot(stack, slot)`: the item's `equippable` slot, the mount can use it and
/// the item allows the mount's type.
pub fn is_equippable(m: &MobData, stack: &ItemStack, slot: EquipmentSlot) -> bool {
    if stack.is_empty() || item::item_id(stack).info().equip_slot != slot.ordinal() {
        return false;
    }
    if !can_use_slot(m, slot) {
        return false;
    }
    let name = item::item_id(stack).name();
    let in_tag = |tag: &str| {
        crate::player::play::game_data().is_some_and(|d| d.entity_in_tag(m.kind.type_id(), tag))
    };
    match slot {
        EquipmentSlot::Saddle => name == "minecraft:saddle" && in_tag("minecraft:can_equip_saddle"),
        EquipmentSlot::Body if name.ends_with("_horse_armor") => {
            in_tag("minecraft:can_wear_horse_armor")
        }
        EquipmentSlot::Body if name.ends_with("_nautilus_armor") => {
            in_tag("minecraft:can_wear_nautilus_armor")
        }
        // `Equippable.llamaSwag`: the wool carpets.
        EquipmentSlot::Body if name.ends_with("_carpet") => {
            matches!(m.kind, Kind::Llama | Kind::TraderLlama)
        }
        _ => false,
    }
}

/// The equipment slot of a mount menu slot.
fn equipment_slot(i: usize) -> Option<EquipmentSlot> {
    match i {
        SADDLE => Some(EquipmentSlot::Saddle),
        BODY => Some(EquipmentSlot::Body),
        _ => None,
    }
}

/// `getEquipSound(slot, stack, equippable)` of a mount.
fn equip_sound(m: &MobData, slot: EquipmentSlot, stack: &ItemStack) -> &'static str {
    if slot == EquipmentSlot::Saddle {
        return match m.kind {
            Kind::Camel => "entity.camel.saddle",
            Kind::CamelHusk => "entity.camel_husk.saddle",
            Kind::Nautilus | Kind::ZombieNautilus if m.fluids.eye_in_water => {
                "item.nautilus_saddle_underwater_equip"
            }
            Kind::Nautilus | Kind::ZombieNautilus => "item.nautilus_saddle_equip",
            _ => "entity.horse.saddle",
        };
    }
    let name = item::item_id(stack).name();
    if name.ends_with("_horse_armor") {
        "entity.horse.armor"
    } else if name.ends_with("_nautilus_armor") {
        "item.armor.equip_nautilus"
    } else {
        "entity.llama.swag"
    }
}

/// `LivingEntity.onEquipItem(slot, old, new)`: the equip sound (from the mob's random) and the
/// equip or unequip game event when the stack changed.
fn on_equip(
    level: &mut crate::level::Level,
    id: EntityId,
    slot: EquipmentSlot,
    old: &ItemStack,
    new: &ItemStack,
) {
    if old.same_item_same_components(new) {
        return;
    }
    let Some(pos) = level.entities.store.get(id).map(|e| e.pos()) else {
        return;
    };
    let Some(m) = level.entities.mob_mut(id) else {
        return;
    };
    let equippable = !new.is_empty() && item::item_id(new).info().equip_slot == slot.ordinal();
    let sound = (equippable && !m.silent).then(|| {
        let seed = m.random.next_long();
        (crate::block::sound_id(equip_sound(m, slot, new)), seed)
    });
    if let Some((sound, seed)) = sound {
        level.play_seeded_sound(
            None,
            pos.x,
            pos.y,
            pos.z,
            sound,
            crate::level::SOUND_NEUTRAL,
            1.0,
            1.0,
            seed,
        );
    }
    let events = crate::level::game_event::events();
    let event = if equippable {
        events.equip
    } else {
        events.unequip
    };
    level.game_event(
        event,
        pos,
        crate::level::game_event::GameEventContext::of(Some(id)),
    );
}

/// The stack in a slot of a mount's inventory menu.
pub(super) fn mount_item_in(level: &crate::level::Level, id: EntityId, i: usize) -> &ItemStack {
    let empty = &ItemStack::EMPTY;
    let Some(m) = level.entities.mob(id) else {
        return empty;
    };
    match equipment_slot(i) {
        Some(slot) => m.equipment.get(slot),
        None => companion::st(m).inventory.get(i - CHEST).unwrap_or(empty),
    }
}

impl ContainerView<'_> {
    pub(super) fn mount_item(&self, id: EntityId, i: usize) -> &ItemStack {
        mount_item_in(self.level, id, i)
    }

    pub(super) fn mount_item_mut(&mut self, id: EntityId, i: usize) -> Option<&mut ItemStack> {
        if equipment_slot(i).is_some() {
            self.level.entities.equipment_updates.push(id);
        }
        let m = self.level.entities.mob_mut(id)?;
        match equipment_slot(i) {
            Some(slot) => m.equipment.items.get_mut(usize::from(slot.ordinal())),
            None => companion::st_mut(m).inventory.get_mut(i - CHEST),
        }
    }

    /// `setItem`: an equipment slot sets the mob's item (`Mob.createEquipmentSlotContainer`:
    /// `setItemSlot`, kept on death and persistent when not empty); a chest slot its stack.
    pub(super) fn mount_set_item(&mut self, id: EntityId, i: usize, stack: ItemStack) {
        match equipment_slot(i) {
            Some(slot) => {
                let Some(m) = self.level.entities.mob_mut(id) else {
                    return;
                };
                let filled = !stack.is_empty();
                let old = m.equipment.get(slot).clone();
                m.equipment.set(slot, stack.clone());
                if filled {
                    m.equipment.set_guaranteed_drop(slot);
                    m.persistent = true;
                }
                self.level.entities.equipment_updates.push(id);
                on_equip(self.level, id, slot, &old, &stack);
            }
            None => {
                if let Some(target) = self
                    .level
                    .entities
                    .mob_mut(id)
                    .and_then(|m| companion::st_mut(m).inventory.get_mut(i - CHEST))
                {
                    *target = stack;
                }
            }
        }
    }

    /// `ArmorSlot.setByPlayer`: the owner's `onEquipItem` before the slot is set.
    pub(super) fn mount_before_set_by_player(&mut self, id: EntityId, i: usize, new: &ItemStack) {
        if let Some(slot) = equipment_slot(i) {
            let old = self.mount_item(id, i).clone();
            on_equip(self.level, id, slot, &old, new);
        }
    }

    /// `ArmorSlot.mayPlace`; chest slots take anything.
    pub(super) fn mount_may_place(&self, id: EntityId, i: usize, stack: &ItemStack) -> bool {
        match equipment_slot(i) {
            Some(slot) => self
                .level
                .entities
                .mob(id)
                .is_some_and(|m| is_equippable(m, stack, slot)),
            None => true,
        }
    }

    /// `ArmorSlot.mayPickup`: Curse of Binding keeps the item unless the player is creative.
    pub(super) fn mount_may_pickup(&self, i: usize) -> bool {
        equipment_slot(i).is_none() || !self.inv.armor_locked(self.mount_slot_item(i))
    }

    fn mount_slot_item(&self, i: usize) -> &ItemStack {
        match self.menu.storage {
            Storage::Mount(id) => self.mount_item(id, i),
            _ => &ItemStack::EMPTY,
        }
    }

    /// `AbstractMountInventoryMenu.quickMoveStack`.
    pub(super) fn mount_quick_move(
        &mut self,
        index: usize,
        _ctx: ClickContext,
        _drops: &mut Vec<Drop>,
    ) -> ItemStack {
        let original = self.item(index).clone();
        if original.is_empty() {
            return ItemStack::EMPTY;
        }
        let mut stack = original.clone();
        let inventory = self.menu.kind.container_slots() - CHEST;
        let player_start = CHEST + inventory;
        let total = self.slot_count();
        if index < player_start {
            if !super::move_to(self, &mut stack, player_start, total, true) {
                return ItemStack::EMPTY;
            }
        } else if self.may_place(BODY, &stack) && self.item(BODY).is_empty() {
            if !super::move_to(self, &mut stack, BODY, BODY + 1, false) {
                return ItemStack::EMPTY;
            }
        } else if self.may_place(SADDLE, &stack) && self.item(SADDLE).is_empty() {
            if !super::move_to(self, &mut stack, SADDLE, SADDLE + 1, false) {
                return ItemStack::EMPTY;
            }
        } else if inventory == 0 || !super::move_to(self, &mut stack, CHEST, player_start, false) {
            let container_end = player_start + 27;
            let hotbar_end = container_end + 9;
            if (container_end..hotbar_end).contains(&index) {
                super::move_to(self, &mut stack, player_start, container_end, false);
            } else if (player_start..container_end).contains(&index) {
                super::move_to(self, &mut stack, container_end, hotbar_end, false);
            }
            // Vanilla moved the slot's own stack in place and returns the empty stack, without
            // `setChanged`.
            if let Some(target) = self.item_mut(index) {
                *target = stack;
            }
            return ItemStack::EMPTY;
        }
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
}
