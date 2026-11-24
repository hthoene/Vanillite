//! Lecterns (`LecternBlock`, `LecternBlockEntity`) as far as redstone goes: a page turn gives a
//! 2 tick pulse, comparators read the page. The book item and the book screen belong to items
//! and containers; they reach the lectern through [`place_book`], [`remove_book`] and
//! [`set_page`].

use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_world::entity::EntityId;
use vanillite_worldgen::behaviour::{PosExt, StateExt};

use super::{BlockBehaviour, Interaction, UseContext, sound_id};
use crate::level::game_event::{GameEventContext, events};
use crate::level::{BlockEntity, Level, SOUND_BLOCKS};

/// The data of a `LecternBlockEntity` (the book itself is kept by the item code).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Lectern {
    /// `!book.isEmpty()`.
    pub has_book: bool,
    /// `page`.
    pub page: i32,
    /// `pageCount`.
    pub page_count: i32,
}

impl Lectern {
    /// `getRedstoneSignal()`.
    pub fn redstone_signal(&self) -> i32 {
        let progress = if self.page_count > 1 {
            self.page as f32 / (self.page_count as f32 - 1.0)
        } else {
            1.0
        };
        (progress * 14.0).floor() as i32 + i32::from(self.has_book)
    }
}

/// Fills the lectern behaviours.
pub(super) fn fill(block: BlockId, b: &mut BlockBehaviour) {
    if block == ids::LECTERN {
        b.tick = Some(tick);
        b.affect_neighbors_after_removal = Some(affect_neighbors_after_removal);
        b.signal = Some(signal);
        b.direct_signal = Some(direct_signal);
        b.analog_output = Some(analog_output);
        b.use_without_item = Some(use_without_item);
    }
}

/// `ownSignal`: 15 while powered.
fn signal(_: &Level, state: BlockState, _: BlockPos, _: Direction) -> i32 {
    if state.flag("powered") { 15 } else { 0 }
}

/// `getDirectSignal`: upwards only.
fn direct_signal(_: &Level, state: BlockState, _: BlockPos, direction: Direction) -> i32 {
    if direction == Direction::Up && state.flag("powered") {
        15
    } else {
        0
    }
}

/// `getAnalogOutputSignal`: the page progress.
fn analog_output(level: &Level, state: BlockState, pos: BlockPos, _: Direction) -> i32 {
    match level.block_entity(pos) {
        Some(BlockEntity::Lectern(lectern)) if state.flag("has_book") => lectern.redstone_signal(),
        _ => 0,
    }
}

/// `useWithoutItem`: with a book the book screen opens (the item code's part).
fn use_without_item(_: &mut Level, state: BlockState, _: BlockPos, _: &UseContext) -> Interaction {
    if state.flag("has_book") {
        Interaction::Success
    } else {
        Interaction::Consume
    }
}

/// `LecternBlock.tick`: the page turn pulse ends.
fn tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    change_powered(level, pos, state, false);
}

/// `affectNeighborsAfterRemoval`.
fn affect_neighbors_after_removal(level: &mut Level, state: BlockState, pos: BlockPos, _: bool) {
    if state.flag("powered") {
        update_below(level, pos);
    }
}

/// `LecternBlock.changePowered`.
fn change_powered(level: &mut Level, pos: BlockPos, state: BlockState, powered: bool) {
    level.set_block_and_update(pos, state.set_flag("powered", powered));
    update_below(level, pos);
}

/// `LecternBlock.updateBelow`.
fn update_below(level: &mut Level, pos: BlockPos) {
    level.update_neighbors_at(pos.below(), ids::LECTERN);
}

/// `LecternBlock.resetBookState`.
fn reset_book_state(level: &mut Level, source: Option<EntityId>, pos: BlockPos, has_book: bool) {
    let state = level.block(pos);
    let new = state
        .set_flag("powered", false)
        .set_flag("has_book", has_book);
    level.set_block_and_update(pos, new);
    level.game_event_block(
        events().block_change,
        pos,
        GameEventContext::with_state(source, new),
    );
    update_below(level, pos);
}

/// `LecternBlock.placeBook`: a book with `page_count` pages goes on the lectern. Returns
/// whether it was placed (`tryPlaceBook`).
pub fn place_book(
    level: &mut Level,
    source: Option<EntityId>,
    pos: BlockPos,
    page_count: i32,
) -> bool {
    if !level.block(pos).is(ids::LECTERN) || level.block(pos).flag("has_book") {
        return false;
    }
    let Some(BlockEntity::Lectern(lectern)) = level.block_entity_mut(pos) else {
        return false;
    };
    lectern.has_book = true;
    lectern.page = 0;
    lectern.page_count = page_count;
    reset_book_state(level, source, pos, true);
    level.play_block_sound(None, pos, sound_id("item.book.put"), SOUND_BLOCKS, 1.0, 1.0);
    true
}

/// `LecternBlockEntity.onBookItemRemove`: the book was taken.
pub fn remove_book(level: &mut Level, pos: BlockPos) {
    let Some(BlockEntity::Lectern(lectern)) = level.block_entity_mut(pos) else {
        return;
    };
    *lectern = Lectern::default();
    reset_book_state(level, None, pos, false);
}

/// `LecternBlockEntity.setPage(page)`: a page turn gives a pulse.
pub fn set_page(level: &mut Level, pos: BlockPos, page: i32) {
    let Some(BlockEntity::Lectern(lectern)) = level.block_entity_mut(pos) else {
        return;
    };
    let page = page.clamp(0, (lectern.page_count - 1).max(0));
    if page == lectern.page {
        return;
    }
    lectern.page = page;
    // `LecternBlock.signalPageChange`.
    let state = level.block(pos);
    change_powered(level, pos, state, true);
    level.schedule_block_tick(pos, ids::LECTERN, 2);
    level.level_event(None, 1043, pos, 0);
}
