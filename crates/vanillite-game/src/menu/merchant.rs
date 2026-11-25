//! The merchant menu (`MerchantMenu`, `MerchantContainer`, `MerchantResultSlot`): two payment
//! slots and the result of the selected or matching offer of a villager or wandering trader.
//!
//! The menu's slots are its own ([`super::container::ContainerMenu::local`]: payment A,
//! payment B, result); the offers live in the merchant mob. What the merchant does about a
//! trade (uses, experience, levels, sounds) is in `crate::mob::fam::village::merchant`.

use vanillite_world::entity::EntityId;

use super::container::{ContainerView, Storage};
use super::{ClickContext, MenuSlots};
use crate::inventory::Drop;
use crate::item::{self, ItemStack};
use crate::mob::fam::village::{self as village, merchant};

/// The merchant part of a merchant menu (`MerchantContainer` and the menu's flags).
#[derive(Clone, Debug, Default)]
pub struct MerchantMenu {
    /// `selectionHint`: the offer the player picked.
    pub selection_hint: i32,
    /// `activeOffer`: the index of the matching offer.
    pub active_offer: Option<usize>,
    /// `futureXp`.
    pub future_xp: i32,
    /// `MerchantResultSlot.removeCount`.
    pub remove_count: i32,
    /// The offers changed: send them again (`sendMerchantOffers`).
    pub send_offers: bool,
    /// The merchant changed its offers: `updateSellItem` before the next sync.
    pub update_sell: bool,
}

/// `MerchantMenu` slot ids.
const PAYMENT_A: usize = 0;
const PAYMENT_B: usize = 1;
const RESULT: usize = 2;

impl ContainerView<'_> {
    /// The merchant mob of a merchant menu.
    fn merchant_id(&self) -> Option<EntityId> {
        match self.menu.storage {
            Storage::Merchant(id) => Some(id),
            _ => None,
        }
    }

    fn m_local(&self, i: usize) -> ItemStack {
        self.menu.local.get(i).cloned().unwrap_or_default()
    }

    fn m_set_local(&mut self, i: usize, stack: ItemStack) {
        if let Some(slot) = self.menu.local.get_mut(i) {
            *slot = stack;
        }
    }

    /// `MerchantContainer.setItem`: the stack is limited to its size; a payment slot updates
    /// the result.
    pub(super) fn merchant_set_item(&mut self, i: usize, mut stack: ItemStack) {
        let max = super::CONTAINER_MAX.min(item::max_stack_size(&stack));
        if stack.count > max {
            stack.count = max;
        }
        self.m_set_local(i, stack);
        if i == PAYMENT_A || i == PAYMENT_B {
            self.merchant_update_sell();
        }
    }

    /// `MerchantContainer.updateSellItem`.
    pub fn merchant_update_sell(&mut self) {
        let Some(id) = self.merchant_id() else {
            return;
        };
        self.menu.merchant.active_offer = None;
        let (a, b) = if self.m_local(PAYMENT_A).is_empty() {
            (self.m_local(PAYMENT_B), ItemStack::EMPTY)
        } else {
            (self.m_local(PAYMENT_A), self.m_local(PAYMENT_B))
        };
        if a.is_empty() {
            self.m_set_local(RESULT, ItemStack::EMPTY);
            self.menu.merchant.future_xp = 0;
            return;
        }
        let hint = self.menu.merchant.selection_hint;
        let found = {
            let level = &mut *self.level;
            let Some(offers) = merchant::offers_in_level(level, id) else {
                return;
            };
            if offers.0.is_empty() {
                None
            } else {
                let mut active = None;
                let mut offer = offers.recipe_for(&a, &b, hint);
                if offer.is_none_or(|i| offers.0[i].is_out_of_stock()) {
                    active = offer;
                    offer = offers.recipe_for(&b, &a, hint);
                }
                match offer.filter(|&i| !offers.0[i].is_out_of_stock()) {
                    Some(i) => Some((Some(i), offers.0[i].result.clone(), offers.0[i].xp)),
                    None => Some((active, ItemStack::EMPTY, 0)),
                }
            }
        };
        if let Some((active, result, xp)) = found {
            self.menu.merchant.active_offer = active;
            self.m_set_local(RESULT, result);
            self.menu.merchant.future_xp = xp;
        }
        let result = self.m_local(RESULT);
        merchant::notify_trade_updated_in_level(self.level, id, &result);
    }

    /// `MerchantContainer.removeItem` and `MerchantResultSlot.remove`: the result goes as a
    /// whole.
    pub(super) fn merchant_remove(&mut self, i: usize, amount: i32) -> ItemStack {
        let Some(slot) = self.menu.local.get_mut(i) else {
            return ItemStack::EMPTY;
        };
        if i == RESULT {
            if !slot.is_empty() {
                self.menu.merchant.remove_count += amount.min(slot.count);
            }
            return std::mem::take(slot);
        }
        let taken = slot.split(amount);
        if !taken.is_empty() {
            self.merchant_update_sell();
        }
        taken
    }

    /// `MerchantResultSlot.mayPickup`.
    pub(super) fn merchant_may_pickup_result(&self) -> bool {
        let (Some(id), Some(active)) = (self.merchant_id(), self.menu.merchant.active_offer) else {
            return false;
        };
        let Some(offer) = self
            .level
            .entities
            .mob(id)
            .and_then(|m| village::state(m).merchant.offers.as_ref())
            .and_then(|o| o.0.get(active))
        else {
            return false;
        };
        let (a, b) = (self.m_local(PAYMENT_A), self.m_local(PAYMENT_B));
        offer.satisfied_by(&a, &b) || offer.satisfied_by(&b, &a)
    }

    /// `MerchantResultSlot.onTake`: the price is paid and the merchant notified.
    pub(super) fn merchant_take_result(&mut self) {
        self.menu.merchant.remove_count = 0;
        let (Some(id), Some(active)) = (self.merchant_id(), self.menu.merchant.active_offer) else {
            return;
        };
        let mut a = self.m_local(PAYMENT_A);
        let mut b = self.m_local(PAYMENT_B);
        let paid = {
            let Some(offer) = self
                .level
                .entities
                .mob(id)
                .and_then(|m| village::state(m).merchant.offers.as_ref())
                .and_then(|o| o.0.get(active))
            else {
                return;
            };
            offer.take(&mut a, &mut b) || offer.take(&mut b, &mut a)
        };
        if paid {
            let pos = self.player_pos;
            if merchant::notify_trade_in_level(self.level, id, active, pos) {
                self.menu.merchant.send_offers = true;
            }
            self.merchant_set_item(PAYMENT_A, a);
            self.merchant_set_item(PAYMENT_B, b);
        }
    }

    /// `MerchantMenu.quickMoveStack`.
    pub(super) fn merchant_quick_move(
        &mut self,
        index: usize,
        ctx: ClickContext,
        drops: &mut Vec<Drop>,
    ) -> ItemStack {
        let original = self.item(index).clone();
        if original.is_empty() {
            return ItemStack::EMPTY;
        }
        let mut stack = original.clone();
        let moved = if index == RESULT {
            let moved = super::move_to(self, &mut stack, 3, 39, true);
            if moved {
                // `slot.onQuickCraft(stack, clicked)`; the trade sound is client side.
                self.menu.merchant.remove_count += original.count - stack.count;
            }
            moved
        } else if index != PAYMENT_A && index != PAYMENT_B {
            if (3..30).contains(&index) {
                super::move_to(self, &mut stack, 30, 39, false)
            } else if (30..39).contains(&index) {
                super::move_to(self, &mut stack, 3, 30, false)
            } else {
                true
            }
        } else {
            super::move_to(self, &mut stack, 3, 39, false)
        };
        if !moved {
            return ItemStack::EMPTY;
        }
        super::finish_quick_move(self, index, stack, &original, ctx, drops)
    }

    /// `handleSelectTrade`: `setSelectionHint` and `tryMoveItems`.
    pub fn merchant_select(&mut self, index: i32) {
        let Some(id) = self.merchant_id() else {
            return;
        };
        self.menu.merchant.selection_hint = index;
        self.merchant_update_sell();
        // `tryMoveItems(newTradeIndex)`.
        let costs = {
            let Some(offers) = merchant::offers_in_level(self.level, id) else {
                return;
            };
            let Ok(i) = usize::try_from(index) else {
                return;
            };
            let Some(offer) = offers.0.get(i) else {
                return;
            };
            (offer.base_cost_a.clone(), offer.cost_b.clone())
        };
        for slot in [PAYMENT_A, PAYMENT_B] {
            let mut old = self.m_local(slot);
            if !old.is_empty() {
                if !super::move_to(self, &mut old, 3, 39, true) {
                    return;
                }
                self.merchant_set_item(slot, old);
            }
        }
        if self.m_local(PAYMENT_A).is_empty() && self.m_local(PAYMENT_B).is_empty() {
            self.move_to_payment(PAYMENT_A, &costs.0);
            if let Some(b) = &costs.1 {
                self.move_to_payment(PAYMENT_B, b);
            }
        }
    }

    /// `moveFromInventoryToPaymentSlot(paymentSlot, cost)`.
    fn move_to_payment(&mut self, payment: usize, cost: &crate::trading::ItemCost) {
        for i in 3..39 {
            let inv = self.item(i).clone();
            if inv.is_empty() || !cost.test(&inv) {
                continue;
            }
            let current = self.m_local(payment);
            if !current.is_empty() && !current.same_item_same_components(&inv) {
                continue;
            }
            let max = item::max_stack_size(&inv);
            let count = (max - current.count).min(inv.count);
            let new = inv.with_count(current.count + count);
            if let Some(s) = self.item_mut(i) {
                s.shrink(count);
            }
            let full = new.count >= max;
            self.merchant_set_item(payment, new);
            if full {
                break;
            }
        }
    }

    /// Runs the merchant updates other code asked for and sends the offers again.
    pub(super) fn flush_merchant(&mut self, sink: &dyn crate::player::PacketSink) {
        if std::mem::take(&mut self.menu.merchant.update_sell) {
            self.merchant_update_sell();
        }
        if std::mem::take(&mut self.menu.merchant.send_offers)
            && let Some(id) = self.merchant_id()
            && let Some(packet) =
                merchant::offers_packet(self.level, id, self.menu.state.container_id)
        {
            sink.send(&packet);
        }
    }
}
