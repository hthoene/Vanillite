//! Items and the default values of their core components.
//!
//! The full default component set of every item is in `data/vanilla/items.json`; typed component
//! structs are generated from it as gameplay needs them.

pub use crate::generated::items::{ITEMS, Rarity, ids};

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

    /// Looks an item up by its registry name.
    pub fn by_name(name: &str) -> Option<Self> {
        crate::registry::ITEM.id(name).map(Self)
    }
}
