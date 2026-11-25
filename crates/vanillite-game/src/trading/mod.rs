//! Trading (`net.minecraft.world.item.trading`): merchant offers with their prices, demand
//! and stock, the data-driven trades ([`data`]) and their NBT and wire forms ([`nbt`]).

pub mod data;
pub mod nbt;

use crate::item::ItemStack;

/// `ItemCost`: an item, a count and the exact components it must have.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ItemCost {
    pub item: u16,
    pub count: i32,
    /// `DataComponentExactPredicate`: `(component type, wire value)` the stack must have.
    pub components: Vec<(u16, Box<[u8]>)>,
}

impl ItemCost {
    /// `ItemCost(item, count)`.
    pub fn new(item: u16, count: i32) -> ItemCost {
        ItemCost {
            item,
            count,
            components: Vec::new(),
        }
    }

    /// `itemStack()`: the stack the cost shows.
    pub fn item_stack(&self) -> ItemStack {
        let mut stack = ItemStack::new(self.item, self.count);
        for (t, v) in &self.components {
            stack.patch_mut().set(*t, v.clone());
        }
        stack
    }

    /// `test(stack)`: the item with the exact components.
    pub fn test(&self, stack: &ItemStack) -> bool {
        stack.item == self.item
            && !stack.is_empty()
            && self
                .components
                .iter()
                .all(|(t, v)| stack.component(*t) == Some(&v[..]))
    }
}

/// `MerchantOffer`.
#[derive(Clone, Debug, PartialEq)]
pub struct MerchantOffer {
    pub base_cost_a: ItemCost,
    pub cost_b: Option<ItemCost>,
    pub result: ItemStack,
    pub uses: i32,
    pub max_uses: i32,
    pub reward_exp: bool,
    pub special_price_diff: i32,
    pub demand: i32,
    pub price_multiplier: f32,
    pub xp: i32,
}

impl MerchantOffer {
    /// `MerchantOffer(costA, costB, result, uses, maxUses, xp, priceMultiplier, demand)`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        base_cost_a: ItemCost,
        cost_b: Option<ItemCost>,
        result: ItemStack,
        uses: i32,
        max_uses: i32,
        xp: i32,
        price_multiplier: f32,
        demand: i32,
    ) -> MerchantOffer {
        MerchantOffer {
            base_cost_a,
            cost_b,
            result,
            uses,
            max_uses,
            reward_exp: true,
            special_price_diff: 0,
            demand,
            price_multiplier,
            xp,
        }
    }

    /// `getModifiedCostCount(cost)`: the base price with demand and special price, at least
    /// 1 and at most a stack.
    pub fn modified_cost_count(&self, cost: &ItemCost) -> i32 {
        let base = cost.count;
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
        let demand_diff = ((base * self.demand) as f32 * self.price_multiplier).floor() as i32;
        let max = crate::item::max_stack_size(&cost.item_stack());
        (base + demand_diff.max(0) + self.special_price_diff).clamp(1, max)
    }

    /// `getCostA`: the first cost with the modified price.
    pub fn cost_a(&self) -> ItemStack {
        self.base_cost_a
            .item_stack()
            .with_count(self.modified_cost_count(&self.base_cost_a))
    }

    /// `getCostB`.
    pub fn cost_b(&self) -> ItemStack {
        self.cost_b
            .as_ref()
            .map_or(ItemStack::EMPTY, ItemCost::item_stack)
    }

    /// `updateDemand`.
    pub fn update_demand(&mut self) {
        self.demand = self.demand + self.uses - (self.max_uses - self.uses);
    }

    /// `resetUses`.
    pub fn reset_uses(&mut self) {
        self.uses = 0;
    }

    /// `increaseUses`.
    pub fn increase_uses(&mut self) {
        self.uses += 1;
    }

    /// `addToSpecialPriceDiff`.
    pub fn add_to_special_price_diff(&mut self, add: i32) {
        self.special_price_diff += add;
    }

    /// `resetSpecialPriceDiff`.
    pub fn reset_special_price_diff(&mut self) {
        self.special_price_diff = 0;
    }

    /// `isOutOfStock`.
    pub fn is_out_of_stock(&self) -> bool {
        self.uses >= self.max_uses
    }

    /// `setToOutOfStock`.
    pub fn set_to_out_of_stock(&mut self) {
        self.uses = self.max_uses;
    }

    /// `needsRestock`.
    pub fn needs_restock(&self) -> bool {
        self.uses > 0
    }

    /// `satisfiedBy(buyA, buyB)`.
    pub fn satisfied_by(&self, buy_a: &ItemStack, buy_b: &ItemStack) -> bool {
        if !self.base_cost_a.test(buy_a)
            || buy_a.count < self.modified_cost_count(&self.base_cost_a)
        {
            return false;
        }
        match &self.cost_b {
            None => buy_b.is_empty(),
            Some(b) => b.test(buy_b) && buy_b.count >= b.count,
        }
    }

    /// `take(buyA, buyB)`: removes the price from the payment slots.
    pub fn take(&self, buy_a: &mut ItemStack, buy_b: &mut ItemStack) -> bool {
        if !self.satisfied_by(buy_a, buy_b) {
            return false;
        }
        buy_a.shrink(self.cost_a().count);
        let b = self.cost_b();
        if !b.is_empty() {
            buy_b.shrink(b.count);
        }
        true
    }
}

/// `MerchantOffers`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MerchantOffers(pub Vec<MerchantOffer>);

impl MerchantOffers {
    /// `getRecipeFor(buyA, buyB, selectionHint)`.
    pub fn recipe_for(&self, buy_a: &ItemStack, buy_b: &ItemStack, hint: i32) -> Option<usize> {
        if hint > 0
            && let Ok(i) = usize::try_from(hint)
            && i < self.0.len()
        {
            return self.0[i].satisfied_by(buy_a, buy_b).then_some(i);
        }
        self.0.iter().position(|o| o.satisfied_by(buy_a, buy_b))
    }
}
