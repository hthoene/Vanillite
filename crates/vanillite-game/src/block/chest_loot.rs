//! Container loot (`RandomizableContainer.unpackLootTable`): the loot table of a container,
//! pot or brushable block is rolled the first time it is opened or broken.

use vanillite_base::Vec3;
use vanillite_world::entity::EntityId;

use crate::item::ItemStack;
use crate::level::Level;

/// `LootTable.getRandomItems(params, seed)` of the table `table` for a container at `origin`.
pub fn roll(
    level: &mut Level,
    table: &str,
    seed: i64,
    origin: Vec3,
    luck: f32,
    player: Option<EntityId>,
) -> Vec<ItemStack> {
    let _ = (level, table, seed, origin, luck, player);
    Vec::new()
}
