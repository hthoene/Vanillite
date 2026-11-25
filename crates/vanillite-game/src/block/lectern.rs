//! Lecterns (`LecternBlock`, `LecternBlockEntity`): a book placed on them opens the lectern
//! menu ([`crate::menu::container::MenuKind::Lectern`]); a page turn gives a 2 tick pulse,
//! comparators read the page.

use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_world::entity::EntityId;
use vanillite_worldgen::behaviour::{PosExt, StateExt};

use super::{BlockBehaviour, Interaction, ItemInteraction, UseContext, sound_id};
use crate::item::ItemStack;
use crate::level::game_event::{GameEventContext, events};
use crate::level::{BlockEntity, Level, SOUND_BLOCKS};
use vanillite_nbt::{Compound, Tag};

/// The data of a `LecternBlockEntity`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lectern {
    /// The book.
    pub book: ItemStack,
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
        (progress * 14.0).floor() as i32 + i32::from(!self.book.is_empty())
    }

    /// `loadAdditional`.
    pub fn load(&mut self, nbt: &Compound) {
        self.book = match nbt.get("Book") {
            Some(Tag::Compound(c)) => crate::inventory::stack_from_nbt(c),
            _ => ItemStack::EMPTY,
        };
        self.page_count = page_count(&self.book);
        let page = match nbt.get("Page") {
            Some(Tag::Int(v)) => *v,
            _ => 0,
        };
        // `Mth.clamp(page, 0, pageCount - 1)`: 0 wins over a negative maximum.
        self.page = page.min(self.page_count - 1).max(0);
    }

    /// `saveAdditional`.
    pub fn save(&self, out: &mut Compound) {
        if !self.book.is_empty() {
            out.insert("Book", crate::inventory::stack_to_nbt(&self.book));
            out.insert("Page", Tag::Int(self.page));
        }
    }
}

/// `getPageCount`: the pages of a written or writable book.
pub fn page_count(book: &ItemStack) -> i32 {
    use vanillite_protocol::codec::Reader;
    use vanillite_protocol::components::ids;
    if let Some(value) = book.component(ids::WRITTEN_BOOK_CONTENT) {
        // `WrittenBookContent`: title (filterable string), author, generation, pages.
        let mut r = Reader::new(value);
        let mut count = || -> Option<i32> {
            filterable_string(&mut r)?;
            r.string(32767).ok()?;
            r.varint().ok()?;
            r.varint().ok()
        };
        return count().unwrap_or(0);
    }
    book.component(ids::WRITABLE_BOOK_CONTENT)
        .and_then(|value| Reader::new(value).varint().ok())
        .unwrap_or(0)
}

/// Skips a `Filterable<String>` (raw, optional filtered).
fn filterable_string(r: &mut vanillite_protocol::codec::Reader<'_>) -> Option<()> {
    r.string(32767).ok()?;
    if r.u8().ok()? != 0 {
        r.string(32767).ok()?;
    }
    Some(())
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
        b.use_item_on = Some(use_item_on);
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

/// `useWithoutItem`: with a book the lectern menu opens.
fn use_without_item(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    ctx: &UseContext,
) -> Interaction {
    if !state.flag("has_book") {
        return Interaction::Consume;
    }
    if matches!(level.block_entity(pos), Some(BlockEntity::Lectern(_))) {
        level.request_menu(crate::menu::container::MenuRequest {
            player: Some(ctx.player),
            menu: crate::menu::container::MenuKind::Lectern,
            storage: crate::menu::container::Storage::Block(pos),
            title: vanillite_protocol::text::Component::translatable("container.lectern", vec![]),
            pos,
            block: ids::LECTERN,
        });
    }
    Interaction::Success
}

/// `useItemOn`: a book from the hand goes on an empty lectern.
fn use_item_on(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    ctx: &UseContext,
    stack: &mut ItemStack,
) -> ItemInteraction {
    if state.flag("has_book") {
        return ItemInteraction::TryWithEmptyHand;
    }
    let is_book = crate::player::play::game_data()
        .is_some_and(|d| d.item_in_tag(stack.item, "minecraft:lectern_books"));
    if is_book {
        let book = stack.split(1);
        return if place_book(level, Some(ctx.player), pos, book) {
            ItemInteraction::Done(Interaction::Success)
        } else {
            ItemInteraction::Pass
        };
    }
    if stack.is_empty() {
        ItemInteraction::Pass
    } else {
        ItemInteraction::TryWithEmptyHand
    }
}

/// `LecternBlockEntity.preRemoveSideEffects`: the book drops on the lectern's front.
pub fn drop_book(level: &mut Level, pos: BlockPos, state: BlockState, lectern: &Lectern) {
    if !state.flag("has_book") || lectern.book.is_empty() {
        return;
    }
    let direction = state.direction("facing").unwrap_or(Direction::North);
    let [dx, _, dz] = direction.normal();
    let at = vanillite_base::Vec3::new(
        f64::from(pos.x) + 0.5 + f64::from(0.25 * dx as f32),
        f64::from(pos.y) + 1.0,
        f64::from(pos.z) + 0.5 + f64::from(0.25 * dz as f32),
    );
    super::drops::spawn_item_moving(level, at, lectern.book.clone(), None, 10, None);
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

/// `LecternBlock.placeBook`: a book goes on the lectern. Returns whether it was placed
/// (`tryPlaceBook`).
pub fn place_book(
    level: &mut Level,
    source: Option<EntityId>,
    pos: BlockPos,
    book: ItemStack,
) -> bool {
    if !level.block(pos).is(ids::LECTERN) || level.block(pos).flag("has_book") {
        return false;
    }
    let Some(BlockEntity::Lectern(lectern)) = level.block_entity_mut(pos) else {
        return false;
    };
    lectern.page_count = page_count(&book);
    lectern.book = book;
    lectern.page = 0;
    reset_book_state(level, source, pos, true);
    level.play_block_sound(None, pos, sound_id("item.book.put"), SOUND_BLOCKS, 1.0, 1.0);
    true
}

/// `removeItemNoUpdate(0)` + `onBookItemRemove`: the book is taken.
pub fn remove_book(level: &mut Level, pos: BlockPos) -> ItemStack {
    let Some(BlockEntity::Lectern(lectern)) = level.block_entity_mut(pos) else {
        return ItemStack::EMPTY;
    };
    let book = std::mem::take(&mut lectern.book);
    *lectern = Lectern::default();
    reset_book_state(level, None, pos, false);
    book
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
