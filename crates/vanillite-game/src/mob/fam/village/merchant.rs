//! The merchant side of villagers and wandering traders (`AbstractVillager` and the trading
//! parts of `Villager`): offers from the trade sets, special prices, restocking, trade
//! experience and levels, starting a trade, and the merchant NBT (`Offers`, `Inventory`).

use vanillite_nbt::Compound;

use super::Merchant;
use crate::mob::Tick;
use crate::mob::animal::Interaction;
use crate::mob::fam::Interact;

/// `addAdditionalSaveData` of `AbstractVillager`: `Offers` and the inventory.
pub fn save(_m: &Merchant, _out: &mut Compound) {}

/// `readAdditionalSaveData` of `AbstractVillager`.
pub fn load(_m: &mut Merchant, _tag: &Compound) {}

/// `Villager.mobInteract` / `WanderingTrader.mobInteract`: opens the trading screen.
pub fn interact(_ix: &mut Interact<'_, '_>) -> Interaction {
    Interaction::Pass
}

/// `Villager.shouldRestock(level)`.
pub fn should_restock(_t: &mut Tick<'_, '_>) -> bool {
    false
}

/// `Villager.restock()`.
pub fn restock(_t: &mut Tick<'_, '_>) {}

/// `Villager.updateTrades` after a profession or level change (the offers of the new level
/// added; `None` offers are generated on first use anyway).
pub fn update_trades(_t: &mut Tick<'_, '_>) {}

/// `AbstractVillager.stopTrading` (also when the villager dies or teleports).
pub fn stop_trading(_t: &mut Tick<'_, '_>) {}
