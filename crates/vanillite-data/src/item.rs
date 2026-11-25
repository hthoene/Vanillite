//! Items and the default values of their core components.
//!
//! The full default component set of every item is in `data/vanilla/items.json`; typed component
//! structs are generated from it as gameplay needs them.

pub use crate::generated::items::{ITEMS, Rarity, TOOLS, ids};

use crate::block::BlockId;

/// An item (registry id of `minecraft:item`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct ItemId(pub u16);

/// Static facts of an item.
#[derive(Debug)]
pub struct ItemInfo {
    pub name: &'static str,
    pub max_stack_size: u8,
    /// Durability, 0 if the item has no `minecraft:max_damage`.
    pub max_damage: u16,
    pub rarity: Rarity,
    /// Block placed by this item, or `u16::MAX`.
    pub block: u16,
    /// Index into [`TOOLS`] of the default `minecraft:tool` component, or `u16::MAX`.
    pub tool: u16,
    /// `EquipmentSlot` ordinal (mainhand, offhand, feet, legs, chest, head, body, saddle) of the
    /// default `minecraft:equippable` component, or 255.
    pub equip_slot: u8,
    /// The default `minecraft:attribute_modifiers`.
    pub modifiers: &'static [ItemModifier],
    /// The default `minecraft:weapon` component.
    pub weapon: Option<WeaponInfo>,
    /// The default `minecraft:food` component.
    pub food: Option<FoodInfo>,
    /// Whether the item has a default `minecraft:blocks_attacks` component (shields).
    pub blocks_attacks: bool,
}

/// One entry of a `minecraft:attribute_modifiers` component.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ItemModifier {
    /// Attribute id (index into `ATTRIBUTES`).
    pub attribute: u16,
    pub amount: f64,
    /// `AttributeModifier.Operation` ordinal: add value, add multiplied base, add multiplied
    /// total.
    pub operation: u8,
    /// `EquipmentSlotGroup` ordinal: any, mainhand, offhand, hand, feet, legs, chest, head,
    /// armor, body, saddle.
    pub slot: u8,
}

/// A `minecraft:weapon` component.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WeaponInfo {
    pub item_damage_per_attack: u32,
    pub disable_blocking_for_seconds: f32,
}

/// A `minecraft:food` component.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FoodInfo {
    pub nutrition: i32,
    pub saturation: f32,
    pub can_always_eat: bool,
}

/// The blocks a tool rule applies to.
#[derive(Debug)]
pub enum BlockSet {
    /// A block tag id without `#`, e.g. `minecraft:mineable/pickaxe` (resolved at run time).
    Tag(&'static str),
    /// Block ids.
    Blocks(&'static [u16]),
}

/// One rule of a `minecraft:tool` component.
#[derive(Debug)]
pub struct ToolRule {
    pub blocks: BlockSet,
    pub speed: Option<f32>,
    pub correct_for_drops: Option<bool>,
}

/// A `minecraft:tool` component.
#[derive(Debug)]
pub struct ToolInfo {
    pub rules: &'static [ToolRule],
    pub default_mining_speed: f32,
    pub damage_per_block: u32,
    pub can_destroy_blocks_in_creative: bool,
}

impl ItemId {
    #[inline]
    pub fn info(self) -> &'static ItemInfo {
        &ITEMS[usize::from(self.0)]
    }

    #[inline]
    pub fn name(self) -> &'static str {
        self.info().name
    }

    /// The block this item places, if it is a block item.
    #[inline]
    pub fn block(self) -> Option<BlockId> {
        let b = self.info().block;
        (b != u16::MAX).then_some(BlockId(b))
    }

    /// The default `minecraft:tool` component.
    #[inline]
    pub fn tool(self) -> Option<&'static ToolInfo> {
        TOOLS.get(usize::from(self.info().tool))
    }

    /// Looks an item up by its registry name.
    pub fn by_name(name: &str) -> Option<Self> {
        crate::registry::ITEM.id(name).map(Self)
    }
}
