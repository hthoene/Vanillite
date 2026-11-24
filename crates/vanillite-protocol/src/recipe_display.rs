//! Recipe book displays (`SlotDisplay`, `RecipeDisplay`, `RecipeDisplayEntry`) and their
//! stream codecs.

use crate::codec::{Encode, write_len, write_varint};
use crate::item::ItemStack;

/// `minecraft:slot_display` ids.
mod slot_ids {
    pub const EMPTY: i32 = 0;
    pub const ANY_FUEL: i32 = 1;
    pub const WITH_ANY_POTION: i32 = 2;
    pub const ONLY_WITH_COMPONENT: i32 = 3;
    pub const ITEM: i32 = 4;
    pub const ITEM_STACK: i32 = 5;
    pub const TAG: i32 = 6;
    pub const DYED: i32 = 7;
    pub const SMITHING_TRIM: i32 = 8;
    pub const WITH_REMAINDER: i32 = 9;
    pub const COMPOSITE: i32 = 10;
}

/// `minecraft:recipe_display` ids.
mod display_ids {
    pub const CRAFTING_SHAPELESS: i32 = 0;
    pub const CRAFTING_SHAPED: i32 = 1;
    pub const FURNACE: i32 = 2;
    pub const STONECUTTER: i32 = 3;
    pub const SMITHING: i32 = 4;
}

/// `SlotDisplay`: what the client shows in one slot of a recipe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SlotDisplay {
    /// `empty`.
    Empty,
    /// `any_fuel`.
    AnyFuel,
    /// `with_any_potion`.
    WithAnyPotion(Box<SlotDisplay>),
    /// `only_with_component`: the source filtered by a data component type id.
    OnlyWithComponent(Box<SlotDisplay>, u16),
    /// `item`.
    Item(u16),
    /// `item_stack` (an `ItemStackTemplate`).
    ItemStack(ItemStack),
    /// `tag`: an item holder set (sent as a direct list of item ids).
    Tag(Box<[u16]>),
    /// `dyed`: dye and target.
    Dyed(Box<SlotDisplay>, Box<SlotDisplay>),
    /// `smithing_trim`: base, material and the `trim_pattern` registry id.
    SmithingTrim(Box<SlotDisplay>, Box<SlotDisplay>, u32),
    /// `with_remainder`: input and remainder.
    WithRemainder(Box<SlotDisplay>, Box<SlotDisplay>),
    /// `composite`.
    Composite(Vec<SlotDisplay>),
}

/// `ByteBufCodecs.holderSet` of a direct list.
pub fn encode_holder_set(out: &mut Vec<u8>, items: &[u16]) {
    write_len(out, items.len().saturating_add(1));
    for &item in items {
        write_varint(out, i32::from(item));
    }
}

impl Encode for SlotDisplay {
    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            SlotDisplay::Empty => write_varint(out, slot_ids::EMPTY),
            SlotDisplay::AnyFuel => write_varint(out, slot_ids::ANY_FUEL),
            SlotDisplay::WithAnyPotion(inner) => {
                write_varint(out, slot_ids::WITH_ANY_POTION);
                inner.encode(out);
            }
            SlotDisplay::OnlyWithComponent(inner, component) => {
                write_varint(out, slot_ids::ONLY_WITH_COMPONENT);
                inner.encode(out);
                write_varint(out, i32::from(*component));
            }
            SlotDisplay::Item(item) => {
                write_varint(out, slot_ids::ITEM);
                write_varint(out, i32::from(*item));
            }
            SlotDisplay::ItemStack(stack) => {
                write_varint(out, slot_ids::ITEM_STACK);
                // `ItemStackTemplate.STREAM_CODEC`: the optional form without its presence byte.
                let mut tmp = Vec::new();
                stack.encode_template_optional(&mut tmp);
                out.extend_from_slice(tmp.get(1..).unwrap_or_default());
            }
            SlotDisplay::Tag(items) => {
                write_varint(out, slot_ids::TAG);
                encode_holder_set(out, items);
            }
            SlotDisplay::Dyed(dye, target) => {
                write_varint(out, slot_ids::DYED);
                dye.encode(out);
                target.encode(out);
            }
            SlotDisplay::SmithingTrim(base, material, pattern) => {
                write_varint(out, slot_ids::SMITHING_TRIM);
                base.encode(out);
                material.encode(out);
                // `ByteBufCodecs.holder`: a registry reference is id + 1.
                write_varint(
                    out,
                    i32::try_from(*pattern).unwrap_or(i32::MAX - 1).saturating_add(1),
                );
            }
            SlotDisplay::WithRemainder(input, remainder) => {
                write_varint(out, slot_ids::WITH_REMAINDER);
                input.encode(out);
                remainder.encode(out);
            }
            SlotDisplay::Composite(list) => {
                write_varint(out, slot_ids::COMPOSITE);
                encode_list(out, list);
            }
        }
    }
}

fn encode_list(out: &mut Vec<u8>, list: &[SlotDisplay]) {
    write_len(out, list.len());
    for d in list {
        d.encode(out);
    }
}

/// `RecipeDisplay`: how the client shows a recipe.
#[derive(Clone, Debug, PartialEq)]
pub enum RecipeDisplay {
    /// `crafting_shapeless`.
    Shapeless {
        ingredients: Vec<SlotDisplay>,
        result: SlotDisplay,
        station: SlotDisplay,
    },
    /// `crafting_shaped`.
    Shaped {
        width: i32,
        height: i32,
        ingredients: Vec<SlotDisplay>,
        result: SlotDisplay,
        station: SlotDisplay,
    },
    /// `furnace`.
    Furnace {
        ingredient: SlotDisplay,
        fuel: SlotDisplay,
        result: SlotDisplay,
        station: SlotDisplay,
        duration: i32,
        experience: f32,
    },
    /// `stonecutter`.
    Stonecutter {
        input: SlotDisplay,
        result: SlotDisplay,
        station: SlotDisplay,
    },
    /// `smithing`.
    Smithing {
        template: SlotDisplay,
        base: SlotDisplay,
        addition: SlotDisplay,
        result: SlotDisplay,
        station: SlotDisplay,
    },
}

impl Encode for RecipeDisplay {
    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            RecipeDisplay::Shapeless {
                ingredients,
                result,
                station,
            } => {
                write_varint(out, display_ids::CRAFTING_SHAPELESS);
                encode_list(out, ingredients);
                result.encode(out);
                station.encode(out);
            }
            RecipeDisplay::Shaped {
                width,
                height,
                ingredients,
                result,
                station,
            } => {
                write_varint(out, display_ids::CRAFTING_SHAPED);
                write_varint(out, *width);
                write_varint(out, *height);
                encode_list(out, ingredients);
                result.encode(out);
                station.encode(out);
            }
            RecipeDisplay::Furnace {
                ingredient,
                fuel,
                result,
                station,
                duration,
                experience,
            } => {
                write_varint(out, display_ids::FURNACE);
                ingredient.encode(out);
                fuel.encode(out);
                result.encode(out);
                station.encode(out);
                write_varint(out, *duration);
                out.extend_from_slice(&experience.to_be_bytes());
            }
            RecipeDisplay::Stonecutter {
                input,
                result,
                station,
            } => {
                write_varint(out, display_ids::STONECUTTER);
                input.encode(out);
                result.encode(out);
                station.encode(out);
            }
            RecipeDisplay::Smithing {
                template,
                base,
                addition,
                result,
                station,
            } => {
                write_varint(out, display_ids::SMITHING);
                template.encode(out);
                base.encode(out);
                addition.encode(out);
                result.encode(out);
                station.encode(out);
            }
        }
    }
}

// `f32` has no `Eq`; displays never hold NaN.
impl Eq for RecipeDisplay {}

/// `RecipeDisplayEntry`: one display of a recipe with its id, group, book category and the
/// ingredients the client checks for "craftable".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecipeDisplayEntry {
    /// `RecipeDisplayId`.
    pub id: i32,
    pub display: RecipeDisplay,
    /// The recipe group id (`OptionalInt`).
    pub group: Option<i32>,
    /// `minecraft:recipe_book_category` id.
    pub category: i32,
    /// `craftingRequirements`: the item sets of `PlacementInfo.ingredients`.
    pub requirements: Option<Vec<Box<[u16]>>>,
}

impl Encode for RecipeDisplayEntry {
    fn encode(&self, out: &mut Vec<u8>) {
        write_varint(out, self.id);
        self.display.encode(out);
        // `ByteBufCodecs.OPTIONAL_VAR_INT`: 0 for none, else value + 1.
        write_varint(out, self.group.map_or(0, |g| g.saturating_add(1)));
        write_varint(out, self.category);
        match &self.requirements {
            None => out.push(0),
            Some(list) => {
                out.push(1);
                write_len(out, list.len());
                for items in list {
                    encode_holder_set(out, items);
                }
            }
        }
    }
}
