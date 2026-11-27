//! Crafters (`CrafterBlock`, `CrafterBlockEntity`): a 3×3 grid with slots that can be
//! disabled; a redstone pulse crafts one result and ejects it in front.

use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_nbt::{Compound, Tag};
use vanillite_worldgen::behaviour::StateExt;

use super::BlockBehaviour;
use crate::item::{self, ItemStack};
use crate::level::{BlockEntity, Level};
use crate::recipe::CraftingInput;

/// The data of a `CrafterBlockEntity`.
#[derive(Clone, Debug, PartialEq)]
pub struct Crafter {
    /// The grid.
    pub items: Box<[ItemStack]>,
    /// `containerData` slots 0–8: 1 for a disabled slot.
    pub disabled: [bool; 9],
    /// `containerData` slot 9.
    pub triggered: bool,
    /// `craftingTicksRemaining`.
    pub crafting_ticks: i32,
    /// `CustomName`.
    pub custom_name: Option<vanillite_protocol::text::Component>,
    /// `LootTable` not rolled yet.
    pub loot_table: Option<String>,
    /// `LootTableSeed`.
    pub loot_seed: i64,
    /// `lock` (`LockCode`).
    pub lock: Option<vanillite_nbt::Tag>,
}

impl Default for Crafter {
    fn default() -> Self {
        Crafter {
            items: vec![ItemStack::EMPTY; 9].into_boxed_slice(),
            disabled: [false; 9],
            triggered: false,
            crafting_ticks: 0,
            custom_name: None,
            loot_table: None,
            loot_seed: 0,
            lock: None,
        }
    }
}

impl Crafter {
    /// `loadAdditional`.
    pub fn load(&mut self, nbt: &Compound) {
        self.lock = super::lock::load(nbt);
        self.custom_name = super::container::load_name(nbt);
        self.crafting_ticks = int(nbt, "crafting_ticks_remaining").unwrap_or(0);
        self.items.iter_mut().for_each(|s| *s = ItemStack::EMPTY);
        if let Some(Tag::String(table)) = nbt.get("LootTable") {
            self.loot_table = Some(table.clone());
            self.loot_seed = nbt.get("LootTableSeed").and_then(Tag::as_i64).unwrap_or(0);
        } else {
            super::container::load_items(nbt, &mut self.items);
        }
        self.disabled = [false; 9];
        if let Some(Tag::IntArray(slots)) = nbt.get("disabled_slots") {
            for &slot in slots {
                if let Ok(slot) = usize::try_from(slot)
                    && self.can_be_disabled(slot)
                    && let Some(d) = self.disabled.get_mut(slot)
                {
                    *d = true;
                }
            }
        }
        self.triggered = int(nbt, "triggered").unwrap_or(0) == 1;
    }

    /// `saveAdditional`.
    pub fn save(&self, out: &mut Compound) {
        super::lock::save(&self.lock, out);
        super::container::save_name(&self.custom_name, out);
        out.insert("crafting_ticks_remaining", Tag::Int(self.crafting_ticks));
        if let Some(table) = &self.loot_table {
            out.insert("LootTable", Tag::String(table.clone()));
            if self.loot_seed != 0 {
                out.insert("LootTableSeed", Tag::Long(self.loot_seed));
            }
        } else {
            super::container::save_items_always(&self.items, out);
        }
        let disabled: Vec<i32> = (0..9)
            .filter(|&i| self.is_disabled(i))
            .filter_map(|i| i32::try_from(i).ok())
            .collect();
        out.insert("disabled_slots", Tag::IntArray(disabled));
        out.insert("triggered", Tag::Int(i32::from(self.triggered)));
    }

    /// `isSlotDisabled`.
    pub fn is_disabled(&self, slot: usize) -> bool {
        self.disabled.get(slot).copied().unwrap_or(false)
    }

    /// `slotCanBeDisabled`: only empty slots.
    fn can_be_disabled(&self, slot: usize) -> bool {
        self.items.get(slot).is_some_and(ItemStack::is_empty)
    }

    /// `setSlotState(slot, enabled)`; whether it changed anything.
    pub fn set_slot_state(&mut self, slot: usize, enabled: bool) -> bool {
        if !self.can_be_disabled(slot) {
            return false;
        }
        if let Some(d) = self.disabled.get_mut(slot) {
            *d = !enabled;
        }
        true
    }

    /// `canPlaceItem`: not into a disabled slot, and the smallest stacks fill first.
    pub fn can_place_item(&self, slot: usize, stack: &ItemStack) -> bool {
        let _ = stack;
        if self.is_disabled(slot) {
            return false;
        }
        let Some(current) = self.items.get(slot) else {
            return false;
        };
        let count = current.count;
        if !current.is_empty() && count >= item::max_stack_size(current) {
            return false;
        }
        if current.is_empty() {
            return true;
        }
        !self.smaller_stack_exists(count, current, slot)
    }

    fn smaller_stack_exists(&self, size: i32, base: &ItemStack, slot: usize) -> bool {
        (slot + 1..9).any(|i| {
            !self.is_disabled(i)
                && self.items.get(i).is_some_and(|s| {
                    s.is_empty() || (s.count < size && s.same_item_same_components(base))
                })
        })
    }

    /// `getRedstoneSignal`: filled or disabled slots.
    pub fn redstone_signal(&self) -> i32 {
        let n = (0..9)
            .filter(|&i| self.is_disabled(i) || self.items.get(i).is_some_and(|s| !s.is_empty()))
            .count();
        i32::try_from(n).unwrap_or(0)
    }

    /// `setItem`: a stack put into a disabled slot enables it.
    pub fn set_item(&mut self, slot: usize, stack: ItemStack) {
        if self.is_disabled(slot) {
            self.set_slot_state(slot, true);
        }
        if let Some(target) = self.items.get_mut(slot) {
            *target = stack;
        }
    }

    /// The data slots of the menu (`containerData`).
    pub fn data(&self, index: usize) -> i32 {
        if index == 9 {
            i32::from(self.triggered)
        } else {
            i32::from(self.is_disabled(index))
        }
    }

    /// `asCraftInput`.
    pub fn craft_input(&self) -> CraftingInput {
        CraftingInput::of(3, 3, &self.items)
    }
}

fn int(nbt: &Compound, key: &str) -> Option<i32> {
    match nbt.get(key) {
        Some(Tag::Int(v)) => Some(*v),
        _ => None,
    }
}

/// Fills the crafter behaviours.
pub(super) fn fill(block: BlockId, b: &mut BlockBehaviour) {
    if block == ids::CRAFTER {
        b.use_without_item = Some(use_without_item);
        b.neighbor_changed = Some(neighbor_changed);
        b.tick = Some(tick);
        b.affect_neighbors_after_removal = Some(super::dispenser::container_removed);
        b.analog_output = Some(|level, _, pos, _| match level.block_entity(pos) {
            Some(BlockEntity::Crafter(c)) => c.redstone_signal(),
            _ => 0,
        });
    }
}

/// `CrafterBlock.useWithoutItem`: the crafter menu opens.
fn use_without_item(
    level: &mut Level,
    _: BlockState,
    pos: BlockPos,
    ctx: &super::UseContext,
) -> super::Interaction {
    if let Some(BlockEntity::Crafter(c)) = level.block_entity(pos) {
        let title = c.custom_name.clone().unwrap_or_else(|| {
            vanillite_protocol::text::Component::translatable("container.crafter", vec![])
        });
        level.request_menu(crate::menu::container::MenuRequest {
            player: Some(ctx.player),
            menu: crate::menu::container::MenuKind::Crafter,
            storage: crate::menu::container::Storage::Block(pos),
            title,
            pos,
            block: ids::CRAFTER,
        });
    }
    super::Interaction::Success
}

/// `CrafterBlock.neighborChanged`.
fn neighbor_changed(level: &mut Level, state: BlockState, pos: BlockPos, _: BlockId, _: bool) {
    let signal = level.has_neighbor_signal(pos);
    let triggered = state.flag("triggered");
    if signal && !triggered {
        level.schedule_block_tick(pos, ids::CRAFTER, 4);
        level.set_block(pos, state.set_flag("triggered", true), 2);
        set_triggered(level, pos, true);
    } else if !signal && triggered {
        level.set_block(
            pos,
            state
                .set_flag("triggered", false)
                .set_flag("crafting", false),
            2,
        );
        set_triggered(level, pos, false);
    }
}

fn set_triggered(level: &mut Level, pos: BlockPos, triggered: bool) {
    if let Some(BlockEntity::Crafter(c)) = level.block_entity_mut(pos) {
        c.triggered = triggered;
    }
}

/// `setPlacedBy`: a crafter placed powered crafts.
pub fn set_placed_by(level: &mut Level, state: BlockState, pos: BlockPos) {
    if state.is(ids::CRAFTER) && state.flag("triggered") {
        level.schedule_block_tick(pos, ids::CRAFTER, 4);
    }
}

/// The block entity ticker (`serverTick`): the crafting state ends after 6 ticks.
pub fn server_tick(level: &mut Level, pos: BlockPos, state: BlockState) {
    let Some(BlockEntity::Crafter(c)) = level.block_entity_mut(pos) else {
        return;
    };
    let remaining = c.crafting_ticks - 1;
    if remaining >= 0 {
        c.crafting_ticks = remaining;
        if remaining == 0 {
            level.set_block_and_update(pos, state.set_flag("crafting", false));
        }
    }
}

/// The front of the `orientation` property (`FrontAndTop.front()`).
fn front(state: BlockState) -> Direction {
    let value = state.value("orientation").unwrap_or("north_up");
    match value.split('_').next().unwrap_or("north") {
        "down" => Direction::Down,
        "up" => Direction::Up,
        "south" => Direction::South,
        "west" => Direction::West,
        "east" => Direction::East,
        _ => Direction::North,
    }
}

/// `tick`: `dispenseFrom`.
fn tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    let Some(BlockEntity::Crafter(c)) = level.block_entity(pos) else {
        return;
    };
    let input = c.craft_input();
    let Some(data) = crate::player::play::game_data() else {
        return;
    };
    let Some(recipe) = data
        .recipes
        .crafting_for_in(&input, &|m| crate::map::info(level, m).map(|i| i.scale))
    else {
        level.level_event(None, 1050, pos, 0);
        return;
    };
    let mut result = data.recipes.assemble(recipe, &input);
    if result.is_empty() {
        level.level_event(None, 1050, pos, 0);
        return;
    }
    let remainders = data.recipes.remainders(Some(recipe), &input);
    let recipe_id = data
        .recipes
        .crafting
        .get(recipe)
        .map(|r| r.id.clone())
        .unwrap_or_default();
    if let Some(BlockEntity::Crafter(c)) = level.block_entity_mut(pos) {
        c.crafting_ticks = 6;
    }
    level.set_block(pos, state.set_flag("crafting", true), 2);
    // `onCraftedBySystem` → `onCraftedPostProcess`.
    crate::map::on_crafted(level, &mut result);
    dispense_item(level, pos, state, result, &recipe_id);
    for rest in remainders {
        if !rest.is_empty() {
            dispense_item(level, pos, state, rest, &recipe_id);
        }
    }
    if let Some(BlockEntity::Crafter(c)) = level.block_entity_mut(pos) {
        for stack in c.items.iter_mut() {
            if !stack.is_empty() {
                stack.shrink(1);
            }
        }
    }
    super::container::set_changed(level, pos);
    level.block_entities.touch(pos.chunk());
}

/// `dispenseItem`: into the container in front, the rest as an item entity.
fn dispense_item(
    level: &mut Level,
    pos: BlockPos,
    state: BlockState,
    result: ItemStack,
    recipe: &str,
) {
    use super::container::ContainerRef;
    let direction = front(state);
    // `HopperBlockEntity.getContainerAt`: the block container (a composter too), else a
    // random container entity at the block's centre.
    let front = pos.relative(direction);
    let into = ContainerRef::at_point(
        level,
        vanillite_base::Vec3::new(
            f64::from(front.x) + 0.5,
            f64::from(front.y) + 0.5,
            f64::from(front.z) + 0.5,
        ),
    );
    let from = Some(ContainerRef::Block(pos));
    let face = Some(direction.opposite());
    let mut remaining = result;
    if let Some(into) = into {
        let into_crafter = matches!(
            into,
            ContainerRef::Block(p) if matches!(level.block_entity(p), Some(BlockEntity::Crafter(_)))
        );
        let into_bookshelf = matches!(
            into,
            ContainerRef::Block(p)
                if matches!(level.block_entity(p), Some(BlockEntity::ChiseledBookshelf(_)))
        );
        let container_max = if into_bookshelf {
            1
        } else {
            crate::menu::CONTAINER_MAX
        };
        if into_crafter || remaining.count > container_max.min(item::max_stack_size(&remaining)) {
            while !remaining.is_empty() {
                let one = remaining.with_count(1);
                let rest = super::hopper::add_item(level, from, into, one, face);
                if !rest.is_empty() {
                    break;
                }
                remaining.shrink(1);
            }
        } else {
            while !remaining.is_empty() {
                let before = remaining.count;
                remaining = super::hopper::add_item(level, from, into, remaining, face);
                if before == remaining.count {
                    break;
                }
            }
        }
    }
    if !remaining.is_empty() {
        super::dispenser::spawn_dispensed(level, pos, direction, remaining);
        // `CRAFTER_RECIPE_CRAFTED` for the players in the 17-block box around it.
        let c = super::furnace::center(pos);
        let area = vanillite_base::Aabb::new(
            c.x - 8.5,
            c.y - 8.5,
            c.z - 8.5,
            c.x + 8.5,
            c.y + 8.5,
            c.z + 8.5,
        );
        let mut players = Vec::new();
        level
            .entities
            .store
            .for_each_in(&area, true, |p| players.push(p));
        if !players.is_empty()
            && let Some(BlockEntity::Crafter(c)) = level.block_entity(pos)
        {
            let items = c.items.to_vec();
            for p in players {
                level.trigger(
                    p,
                    crate::advancements::trigger::Event::CrafterRecipeCrafted(
                        recipe.to_owned(),
                        items.clone(),
                    ),
                );
            }
        }
        level.level_event(None, 1049, pos, 0);
        level.level_event(None, 2010, pos, super::dispenser::direction_3d(direction));
    }
}
