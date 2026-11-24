//! Decorated pots (`DecoratedPotBlock`, `DecoratedPotBlockEntity`): sherds on four sides and
//! one stack of items inside (`ContainerSingleItem`), wobbling when used.

use vanillite_base::BlockPos;
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_nbt::{Compound, Tag};
use vanillite_protocol::components::ids as components;

use super::{BlockBehaviour, Interaction, ItemInteraction, UseContext, sound_id};
use crate::item::{self, ItemStack};
use crate::level::block_entity::{load_component, save_component};
use crate::level::game_event::{GameEventContext, events};
use crate::level::{BlockEntity, Level, SOUND_BLOCKS};

/// `DecoratedPotBlockEntity.EVENT_POT_WOBBLES`.
const EVENT_WOBBLE: i32 = 1;
/// `WobbleStyle.POSITIVE` and `NEGATIVE` ordinals.
const WOBBLE_POSITIVE: i32 = 0;
const WOBBLE_NEGATIVE: i32 = 1;

/// The data of a `DecoratedPotBlockEntity`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DecoratedPot {
    /// `sherds` (a `minecraft:pot_decorations` value; `None` for no decorations).
    pub sherds: Option<Box<[u8]>>,
    /// The stack inside.
    pub item: ItemStack,
    /// `LootTable` not rolled yet.
    pub loot_table: Option<String>,
    /// `LootTableSeed`.
    pub loot_seed: i64,
}

impl DecoratedPot {
    /// `loadAdditional`.
    pub fn load(&mut self, nbt: &Compound) {
        self.sherds = load_component(nbt, "sherds", components::POT_DECORATIONS)
            .filter(|v| !is_empty_decorations(v));
        self.loot_table = match nbt.get("LootTable") {
            Some(Tag::String(t)) => Some(t.clone()),
            _ => None,
        };
        self.loot_seed = nbt.get("LootTableSeed").and_then(Tag::as_i64).unwrap_or(0);
        self.item = match (&self.loot_table, nbt.get("item")) {
            (None, Some(Tag::Compound(c))) => crate::inventory::stack_from_nbt(c),
            _ => ItemStack::EMPTY,
        };
    }

    /// `saveAdditional` (the update tag is `saveCustomOnly`, the same).
    pub fn save(&self, out: &mut Compound) {
        if let Some(sherds) = &self.sherds {
            save_component(out, "sherds", components::POT_DECORATIONS, sherds);
        }
        if let Some(table) = &self.loot_table {
            out.insert("LootTable", Tag::String(table.clone()));
            if self.loot_seed != 0 {
                out.insert("LootTableSeed", Tag::Long(self.loot_seed));
            }
        } else if !self.item.is_empty() {
            out.insert("item", crate::inventory::stack_to_nbt(&self.item));
        }
    }

    /// `collectImplicitComponents`.
    pub fn component(&self, id: u16) -> Option<Box<[u8]>> {
        if id == components::POT_DECORATIONS {
            Some(
                self.sherds
                    .clone()
                    .unwrap_or_else(|| Box::new([0, 0, 0, 0])),
            )
        } else if id == components::CONTAINER {
            // `ItemContainerContents.fromItems(List.of(item))`.
            let mut out = vec![1];
            self.item.encode_template_optional(&mut out);
            Some(out.into_boxed_slice())
        } else {
            None
        }
    }

    /// `applyImplicitComponents`.
    pub fn apply(&mut self, stack: &ItemStack) {
        self.sherds = stack
            .component(components::POT_DECORATIONS)
            .filter(|v| !is_empty_decorations(v))
            .map(Box::from);
        self.item = stack
            .component(components::CONTAINER)
            .and_then(|v| vanillite_protocol::item::decode_container_contents(v).ok())
            .and_then(|items| items.into_iter().next())
            .unwrap_or_default();
    }

    /// The sherd items of the four sides in the order a cracked pot drops them (left, back,
    /// front, right); sides without one are skipped.
    pub fn sherd_items(&self) -> Vec<ItemStack> {
        let Some(value) = &self.sherds else {
            return Vec::new();
        };
        let mut r = vanillite_protocol::codec::Reader::new(value);
        // back, left, right, front
        let mut sides: [ItemStack; 4] = Default::default();
        for side in &mut sides {
            match ItemStack::decode_template_optional(&mut r) {
                Ok(stack) => *side = stack,
                Err(_) => return Vec::new(),
            }
        }
        let [back, left, right, front] = sides;
        [left, back, front, right]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect()
    }
}

/// Whether a `pot_decorations` value has no side (`PotDecorations.EMPTY`).
fn is_empty_decorations(value: &[u8]) -> bool {
    value == [0, 0, 0, 0]
}

/// Fills the decorated pot behaviours.
pub(super) fn fill(block: BlockId, b: &mut BlockBehaviour) {
    if block == ids::DECORATED_POT {
        b.use_item_on = Some(use_item_on);
        b.use_without_item = Some(use_without_item);
        b.trigger_event = Some(trigger_event);
        b.analog_output = Some(super::dispenser::container_signal);
        b.affect_neighbors_after_removal = Some(super::dispenser::container_removed);
    }
}

/// `triggerEvent` of the block entity: the wobble goes to clients.
fn trigger_event(level: &mut Level, _: BlockState, pos: BlockPos, a: i32, b: i32) -> bool {
    a == EVENT_WOBBLE
        && (0..2).contains(&b)
        && matches!(level.block_entity(pos), Some(BlockEntity::DecoratedPot(_)))
}

/// `useItemOn`: one item of the hand goes in if the pot is empty or holds the same.
fn use_item_on(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    ctx: &UseContext,
    stack: &mut ItemStack,
) -> ItemInteraction {
    let Some(BlockEntity::DecoratedPot(pot)) = level.block_entity(pos) else {
        return ItemInteraction::Pass;
    };
    let inside = pot.item.clone();
    let fits = inside.is_empty()
        || (inside.same_item_same_components(stack)
            && inside.count < item::max_stack_size(&inside));
    if stack.is_empty() || !fits {
        return ItemInteraction::TryWithEmptyHand;
    }
    level.block_event(pos, state.block(), EVENT_WOBBLE, WOBBLE_POSITIVE);
    let one = stack.split(1);
    let pitch_bend = if let Some(BlockEntity::DecoratedPot(pot)) = level.block_entity_mut(pos) {
        if pot.item.is_empty() {
            pot.item = one;
        } else {
            pot.item.grow(1);
        }
        pot.loot_table = None;
        pot.item.count as f32 / item::max_stack_size(&pot.item) as f32
    } else {
        0.0
    };
    level.play_block_sound(
        None,
        pos,
        sound_id("block.decorated_pot.insert"),
        SOUND_BLOCKS,
        1.0,
        0.7 + 0.5 * pitch_bend,
    );
    super::container::set_changed(level, pos);
    level.block_entity_changed(pos);
    level.game_event_block(
        events().block_change,
        pos,
        GameEventContext::of(Some(ctx.player)),
    );
    ItemInteraction::Done(Interaction::Success)
}

/// `useWithoutItem`: the pot wobbles the other way.
fn use_without_item(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    ctx: &UseContext,
) -> Interaction {
    if !matches!(level.block_entity(pos), Some(BlockEntity::DecoratedPot(_))) {
        return Interaction::Pass;
    }
    level.play_block_sound(
        None,
        pos,
        sound_id("block.decorated_pot.insert_fail"),
        SOUND_BLOCKS,
        1.0,
        1.0,
    );
    level.block_event(pos, state.block(), EVENT_WOBBLE, WOBBLE_NEGATIVE);
    level.game_event_block(
        events().block_change,
        pos,
        GameEventContext::of(Some(ctx.player)),
    );
    Interaction::Success
}
