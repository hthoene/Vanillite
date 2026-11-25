//! Chiseled bookshelves (`ChiseledBookShelfBlock`, `ChiseledBookShelfBlockEntity`): six book
//! slots picked by where the front face is hit; the block state shows the filled slots.

use vanillite_base::{BlockPos, Direction, Vec3};
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_data::item::ids as items;
use vanillite_nbt::{Compound, Tag};
use vanillite_worldgen::behaviour::StateExt;

use super::{BlockBehaviour, Interaction, ItemInteraction, UseContext, sound_id};
use crate::item::{self, ItemStack};
use crate::level::game_event::{GameEventContext, events};
use crate::level::{BlockEntity, Level, SOUND_BLOCKS};

/// `SLOT_OCCUPIED_PROPERTIES`.
const SLOT_PROPERTIES: [&str; 6] = [
    "slot_0_occupied",
    "slot_1_occupied",
    "slot_2_occupied",
    "slot_3_occupied",
    "slot_4_occupied",
    "slot_5_occupied",
];

/// The data of a `ChiseledBookShelfBlockEntity`.
#[derive(Clone, Debug, PartialEq)]
pub struct Bookshelf {
    /// The six slots (one book each).
    pub items: Box<[ItemStack]>,
    /// `last_interacted_slot` (-1: none).
    pub last_interacted_slot: i32,
}

impl Default for Bookshelf {
    fn default() -> Self {
        Bookshelf {
            items: vec![ItemStack::EMPTY; 6].into_boxed_slice(),
            last_interacted_slot: -1,
        }
    }
}

impl Bookshelf {
    /// `loadAdditional`.
    pub fn load(&mut self, nbt: &Compound) {
        self.items.iter_mut().for_each(|s| *s = ItemStack::EMPTY);
        super::container::load_items(nbt, &mut self.items);
        self.last_interacted_slot = match nbt.get("last_interacted_slot") {
            Some(Tag::Int(v)) => *v,
            _ => -1,
        };
    }

    /// `saveAdditional`.
    pub fn save(&self, out: &mut Compound) {
        super::container::save_items_always(&self.items, out);
        out.insert("last_interacted_slot", Tag::Int(self.last_interacted_slot));
    }
}

/// `acceptsItemType`: `#bookshelf_books`.
pub fn accepts(stack: &ItemStack) -> bool {
    crate::player::play::game_data()
        .is_some_and(|d| d.item_in_tag(stack.item, "minecraft:bookshelf_books"))
}

/// Fills the chiseled bookshelf behaviours.
pub(super) fn fill(block: BlockId, b: &mut BlockBehaviour) {
    if block == ids::CHISELED_BOOKSHELF {
        b.use_item_on = Some(use_item_on);
        b.use_without_item = Some(use_without_item);
        b.affect_neighbors_after_removal = Some(super::dispenser::container_removed);
        b.analog_output = Some(|level, _, pos, _| match level.block_entity(pos) {
            Some(BlockEntity::ChiseledBookshelf(b)) => b.last_interacted_slot + 1,
            _ => 0,
        });
    }
}

/// `SelectableSlotContainer.getHitSlot` with 2 rows of 3 columns.
pub fn hit_slot(face: Direction, facing: Direction, pos: BlockPos, hit: Vec3) -> Option<usize> {
    if face != facing {
        return None;
    }
    let block = pos.relative(face);
    let (rx, ry, rz) = (
        hit.x - f64::from(block.x),
        hit.y - f64::from(block.y),
        hit.z - f64::from(block.z),
    );
    let (x, y) = match face {
        Direction::North => ((1.0 - rx) as f32, ry as f32),
        Direction::South => (rx as f32, ry as f32),
        Direction::West => (rz as f32, ry as f32),
        Direction::East => ((1.0 - rz) as f32, ry as f32),
        Direction::Up | Direction::Down => return None,
    };
    let section = |c: f32, n: i32| -> i32 {
        let size = 16.0 / n as f32;
        ((c * 16.0 / size).floor() as i32).clamp(0, n - 1)
    };
    let row = section(1.0 - y, 2);
    let column = section(x, 3);
    usize::try_from(column + row * 3).ok()
}

fn facing(state: BlockState) -> Direction {
    state.direction("facing").unwrap_or(Direction::North)
}

/// `useItemOn`: a book goes into the hit slot.
fn use_item_on(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    ctx: &UseContext,
    stack: &mut ItemStack,
) -> ItemInteraction {
    if !matches!(
        level.block_entity(pos),
        Some(BlockEntity::ChiseledBookshelf(_))
    ) {
        return ItemInteraction::Pass;
    }
    if !accepts(stack) {
        return ItemInteraction::TryWithEmptyHand;
    }
    let Some(slot) = hit_slot(ctx.face, facing(state), pos, ctx.hit) else {
        return ItemInteraction::Pass;
    };
    if SLOT_PROPERTIES.get(slot).is_some_and(|p| state.flag(p)) {
        return ItemInteraction::TryWithEmptyHand;
    }
    let sound = if item::item_id(stack) == items::ENCHANTED_BOOK {
        "block.chiseled_bookshelf.insert.enchanted"
    } else {
        "block.chiseled_bookshelf.insert"
    };
    // `addBook`.
    let used = crate::stats::Stat::of(crate::stats::StatType::Used, stack.item);
    level.award_stat(ctx.player, used, 1);
    let book = stack.split(1);
    set_item(level, pos, slot, book);
    level.play_block_sound(None, pos, sound_id(sound), SOUND_BLOCKS, 1.0, 1.0);
    ItemInteraction::Done(Interaction::Success)
}

/// `useWithoutItem`: the book of the hit slot goes to the player.
fn use_without_item(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    ctx: &UseContext,
) -> Interaction {
    if !matches!(
        level.block_entity(pos),
        Some(BlockEntity::ChiseledBookshelf(_))
    ) {
        return Interaction::Pass;
    }
    let Some(slot) = hit_slot(ctx.face, facing(state), pos, ctx.hit) else {
        return Interaction::Pass;
    };
    if !SLOT_PROPERTIES.get(slot).is_some_and(|p| state.flag(p)) {
        return Interaction::Consume;
    }
    let book = remove_item(level, pos, slot);
    let sound = if item::item_id(&book) == items::ENCHANTED_BOOK {
        "block.chiseled_bookshelf.pickup.enchanted"
    } else {
        "block.chiseled_bookshelf.pickup"
    };
    level.play_block_sound(None, pos, sound_id(sound), SOUND_BLOCKS, 1.0, 1.0);
    level.give_to_player(ctx.player, book);
    level.game_event_block(
        events().block_change,
        pos,
        GameEventContext::of(Some(ctx.player)),
    );
    Interaction::Success
}

/// `setItem`: accepted books go in (and update the state); an empty stack removes.
pub fn set_item(level: &mut Level, pos: BlockPos, slot: usize, stack: ItemStack) {
    if accepts(&stack) {
        if let Some(BlockEntity::ChiseledBookshelf(b)) = level.block_entity_mut(pos)
            && let Some(target) = b.items.get_mut(slot)
        {
            *target = stack;
        }
        update_state(level, pos, slot);
    } else if stack.is_empty() {
        remove_item(level, pos, slot);
    }
}

/// `removeItem(slot, count)`: the whole slot.
pub fn remove_item(level: &mut Level, pos: BlockPos, slot: usize) -> ItemStack {
    let taken = match level.block_entity_mut(pos) {
        Some(BlockEntity::ChiseledBookshelf(b)) => b
            .items
            .get_mut(slot)
            .map(std::mem::take)
            .unwrap_or_default(),
        _ => ItemStack::EMPTY,
    };
    if !taken.is_empty() {
        update_state(level, pos, slot);
    }
    taken
}

/// `updateState(interactedSlot)`.
fn update_state(level: &mut Level, pos: BlockPos, slot: usize) {
    let Some(BlockEntity::ChiseledBookshelf(b)) = level.block_entity_mut(pos) else {
        return;
    };
    b.last_interacted_slot = i32::try_from(slot).unwrap_or(-1);
    let filled: Vec<bool> = b.items.iter().map(|s| !s.is_empty()).collect();
    let mut state = level.block(pos);
    for (property, filled) in SLOT_PROPERTIES.iter().zip(filled) {
        state = state.set_flag(property, filled);
    }
    level.set_block_and_update(pos, state);
    level.game_event_block(
        events().block_change,
        pos,
        GameEventContext::with_state(None, state),
    );
}
