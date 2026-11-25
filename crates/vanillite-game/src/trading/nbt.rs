//! Offers as NBT (`MerchantOffers.CODEC`: `{Recipes: [...]}`) and on the wire
//! (`MerchantOffer.STREAM_CODEC`).

use vanillite_nbt::{Compound, List, Tag};
use vanillite_protocol::packets::play::clientbound::{TradeCost, TradeOffer};

use super::{ItemCost, MerchantOffer, MerchantOffers};
use crate::item_nbt;

/// `ItemCost.CODEC`: `id`, `count` (always written), `components`.
fn cost_to_nbt(cost: &ItemCost) -> Compound {
    let mut c = Compound::new();
    c.insert(
        "id",
        vanillite_data::registry::ITEM
            .name(cost.item)
            .unwrap_or("minecraft:air"),
    );
    c.insert("count", cost.count);
    if !cost.components.is_empty() {
        let mut components = Compound::new();
        for (t, value) in &cost.components {
            if let (Some(name), Some(tag)) = (
                vanillite_protocol::components_nbt::type_name(*t),
                item_nbt::component_to_nbt(*t, value),
            ) {
                components.insert(name, tag);
            }
        }
        c.insert("components", components);
    }
    c
}

fn cost_from_nbt(c: &Compound) -> Option<ItemCost> {
    let item = c
        .get("id")
        .and_then(Tag::as_str)
        .and_then(|n| vanillite_data::registry::ITEM.id(n))?;
    let count = c
        .get("count")
        .and_then(Tag::as_i64)
        .and_then(|v| i32::try_from(v).ok())
        .unwrap_or(1);
    let mut components = Vec::new();
    if let Some(Tag::Compound(map)) = c.get("components") {
        for (name, tag) in map.iter() {
            if let Some(t) = vanillite_protocol::components_nbt::type_by_name(name)
                && let Some(value) = item_nbt::component_from_nbt(t, tag)
            {
                components.push((t, value));
            }
        }
    }
    Some(ItemCost {
        item,
        count,
        components,
    })
}

/// `MerchantOffer.CODEC` (defaults are omitted).
pub fn offer_to_nbt(o: &MerchantOffer) -> Compound {
    let mut c = Compound::new();
    c.insert("buy", cost_to_nbt(&o.base_cost_a));
    if let Some(b) = &o.cost_b {
        c.insert("buyB", cost_to_nbt(b));
    }
    c.insert("sell", item_nbt::stack_to_nbt(&o.result));
    if o.uses != 0 {
        c.insert("uses", o.uses);
    }
    if o.max_uses != 4 {
        c.insert("maxUses", o.max_uses);
    }
    if !o.reward_exp {
        c.insert("rewardExp", false);
    }
    if o.special_price_diff != 0 {
        c.insert("specialPrice", o.special_price_diff);
    }
    if o.demand != 0 {
        c.insert("demand", o.demand);
    }
    if o.price_multiplier != 0.0 {
        c.insert("priceMultiplier", o.price_multiplier);
    }
    if o.xp != 1 {
        c.insert("xp", o.xp);
    }
    c
}

/// Reads an offer; `None` without a valid `buy` or `sell`.
pub fn offer_from_nbt(c: &Compound) -> Option<MerchantOffer> {
    let buy = cost_from_nbt(c.get("buy")?.as_compound()?)?;
    let sell = item_nbt::stack_from_nbt(c.get("sell")?.as_compound()?);
    if sell.is_empty() {
        return None;
    }
    let int = |key: &str, default: i32| -> i32 {
        c.get(key)
            .and_then(Tag::as_i64)
            .and_then(|v| i32::try_from(v).ok())
            .unwrap_or(default)
    };
    Some(MerchantOffer {
        base_cost_a: buy,
        cost_b: c
            .get("buyB")
            .and_then(Tag::as_compound)
            .and_then(cost_from_nbt),
        result: sell,
        uses: int("uses", 0),
        max_uses: int("maxUses", 4),
        reward_exp: !matches!(c.get("rewardExp"), Some(Tag::Byte(0))),
        special_price_diff: int("specialPrice", 0),
        demand: int("demand", 0),
        price_multiplier: match c.get("priceMultiplier") {
            Some(Tag::Float(f)) => *f,
            #[allow(clippy::cast_possible_truncation)]
            Some(Tag::Double(d)) => *d as f32,
            _ => 0.0,
        },
        xp: int("xp", 1),
    })
}

/// `MerchantOffers.CODEC`: `{Recipes: [...]}` (an empty list is omitted).
pub fn offers_to_nbt(offers: &MerchantOffers) -> Compound {
    let mut c = Compound::new();
    if !offers.0.is_empty() {
        let list: Vec<Tag> = offers
            .0
            .iter()
            .map(|o| Tag::Compound(offer_to_nbt(o)))
            .collect();
        c.insert("Recipes", List(list));
    }
    c
}

/// Reads [`offers_to_nbt`]; broken offers are skipped.
pub fn offers_from_nbt(c: &Compound) -> MerchantOffers {
    let mut offers = MerchantOffers::default();
    if let Some(Tag::List(list)) = c.get("Recipes") {
        offers.0.extend(
            list.0
                .iter()
                .filter_map(Tag::as_compound)
                .filter_map(offer_from_nbt),
        );
    }
    offers
}

fn wire_cost(cost: &ItemCost) -> TradeCost {
    TradeCost {
        item: cost.item,
        count: cost.count,
        components: cost.components.clone(),
    }
}

/// `MerchantOffers.STREAM_CODEC` with the offers' current state.
pub fn to_wire(offers: &MerchantOffers) -> Vec<TradeOffer> {
    offers
        .0
        .iter()
        .map(|o| TradeOffer {
            cost_a: wire_cost(&o.base_cost_a),
            result: o.result.clone(),
            cost_b: o.cost_b.as_ref().map(wire_cost),
            out_of_stock: o.is_out_of_stock(),
            uses: o.uses,
            max_uses: o.max_uses,
            xp: o.xp,
            special_price: o.special_price_diff,
            price_multiplier: o.price_multiplier,
            demand: o.demand,
        })
        .collect()
}
