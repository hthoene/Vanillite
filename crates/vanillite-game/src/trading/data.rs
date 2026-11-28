//! The data-driven trades (`minecraft:villager_trade`, `minecraft:trade_set`) and turning them
//! into offers (`VillagerTrade.getOffer`, `AbstractVillager.addOffersFromTradeSet`).

use std::collections::HashMap;

use serde_json::Value;
use vanillite_nbt::{Compound, List, Tag};

use super::{ItemCost, MerchantOffer, MerchantOffers};
use crate::item::ItemStack;
use crate::loot::{LootCondition, LootEval, LootFloat, LootFunction, LootInt};

/// Component type ids with their wire values.
type Components = Vec<(u16, Box<[u8]>)>;

/// `TradeCost`: an item, a count provider and the exact components.
#[derive(Debug)]
pub struct TradeCost {
    pub item: u16,
    pub count: LootInt,
    pub components: Vec<(u16, Box<[u8]>)>,
}

impl TradeCost {
    fn parse(v: &Value) -> Result<TradeCost, String> {
        let o = v.as_object().ok_or("trade cost is not an object")?;
        let item = item_id(o.get("id").and_then(Value::as_str).ok_or("missing id")?)?;
        let count = match o.get("count") {
            Some(c) => LootInt::parse(c)?,
            None => LootInt::constant(1),
        };
        Ok(TradeCost {
            item,
            count,
            components: components(o.get("components"))?,
        })
    }

    /// `toItemCost(context, additionalCost)`.
    fn to_item_cost(&self, eval: &mut LootEval<'_>, additional: i32) -> ItemCost {
        let max = crate::item::max_stack_size(&ItemStack::new(self.item, 1));
        let count = self
            .count
            .sample(eval)
            .wrapping_add(additional)
            .clamp(0, max);
        ItemCost {
            item: self.item,
            count,
            components: self.components.clone(),
        }
    }
}

/// `VillagerTrade`.
#[derive(Debug)]
pub struct VillagerTrade {
    pub wants: TradeCost,
    pub additional_wants: Option<TradeCost>,
    /// `gives`: an `ItemStackTemplate`.
    pub gives: ItemStack,
    pub max_uses: LootInt,
    pub xp: LootInt,
    pub reputation_discount: LootFloat,
    pub merchant_predicate: Option<LootCondition>,
    pub given_item_modifier: Option<LootFunction>,
    /// `double_trade_price_enchantments`: holder set entries of `minecraft:enchantment`.
    pub double_trade_price_enchantments: Option<Vec<String>>,
}

impl VillagerTrade {
    /// Parses a `minecraft:villager_trade` entry.
    pub fn parse(id: &str, v: &Value) -> Result<VillagerTrade, String> {
        let o = v.as_object().ok_or("not an object")?;
        let gives = o.get("gives").ok_or("missing gives")?;
        let gives_item = item_id(
            gives
                .get("id")
                .and_then(Value::as_str)
                .ok_or("missing id")?,
        )?;
        let count = gives
            .get("count")
            .and_then(Value::as_i64)
            .and_then(|c| i32::try_from(c).ok())
            .unwrap_or(1);
        let mut stack = ItemStack::new(gives_item, count);
        for (t, value) in components(gives.get("components"))? {
            stack.set_component(t, value);
        }
        let int = |key: &str, default: i32| -> Result<LootInt, String> {
            o.get(key)
                .map_or(Ok(LootInt::constant(default)), LootInt::parse)
        };
        Ok(VillagerTrade {
            wants: TradeCost::parse(o.get("wants").ok_or("missing wants")?)?,
            additional_wants: o
                .get("additional_wants")
                .map(TradeCost::parse)
                .transpose()?,
            gives: stack,
            max_uses: int("max_uses", 4)?,
            xp: int("xp", 1)?,
            reputation_discount: o
                .get("reputation_discount")
                .map_or(Ok(LootFloat::constant(0.0)), LootFloat::parse)?,
            merchant_predicate: o
                .get("merchant_predicate")
                .map(|c| LootCondition::parse(c, id))
                .transpose()?,
            given_item_modifier: o
                .get("given_item_modifier")
                .map(|f| LootFunction::parse(f, id))
                .transpose()?,
            double_trade_price_enchantments: o
                .get("double_trade_price_enchantments")
                .map(holder_entries),
        })
    }

    /// `getOffer(context)`: `None` when the predicate fails or the item or a price is empty.
    pub fn offer(
        &self,
        eval: &mut LootEval<'_>,
        holder_set: &dyn Fn(&str, &str) -> Vec<u32>,
    ) -> Option<MerchantOffer> {
        if let Some(p) = &self.merchant_predicate
            && !p.test(eval)
        {
            return None;
        }
        let mut result = self.gives.clone();
        let mut additional = 0i32;
        if let Some(f) = &self.given_item_modifier {
            f.apply(eval, &mut result);
            if result.is_empty() {
                return None;
            }
        }
        if let Some(t) = additional_trade_cost_type()
            && let Some(value) = result.component(t)
        {
            let cost = vanillite_protocol::codec::Reader::new(value)
                .varint()
                .unwrap_or(0);
            remove_component(&mut result, t);
            additional = additional.wrapping_add(cost);
        }
        if let Some(entries) = &self.double_trade_price_enchantments {
            let stored = crate::enchant::read(
                result.component(vanillite_protocol::components::ids::STORED_ENCHANTMENTS),
            );
            let set: Vec<u32> = entries
                .iter()
                .flat_map(|e| holder_set("minecraft:enchantment", e))
                .collect();
            if stored.iter().any(|(id, _)| set.contains(id)) {
                additional = additional.wrapping_mul(2);
            }
        }
        let cost = self.wants.to_item_cost(eval, additional);
        if cost.count < 1 {
            return None;
        }
        let extra = self
            .additional_wants
            .as_ref()
            .map(|w| w.to_item_cost(eval, 0));
        if extra.as_ref().is_some_and(|c| c.count < 1) {
            return None;
        }
        let max_uses = self.max_uses.sample(eval).max(1);
        let xp = self.xp.sample(eval).max(0);
        let discount = self.reputation_discount.sample(eval).max(0.0);
        Some(MerchantOffer::new(
            cost, extra, result, 0, max_uses, xp, discount, 0,
        ))
    }
}

/// `TradeSet`.
#[derive(Debug)]
pub struct TradeSet {
    /// `trades`: `minecraft:villager_trade` registry ids in holder set order.
    pub trades: Vec<u32>,
    pub amount: LootInt,
    pub allow_duplicates: bool,
    pub random_sequence: Option<String>,
}

/// The trades of the loaded data packs.
#[derive(Debug, Default)]
pub struct TradeData {
    /// By `minecraft:villager_trade` registry id (`None` for entries that failed to parse).
    pub trades: Vec<Option<VillagerTrade>>,
    /// By `minecraft:trade_set` id.
    pub sets: HashMap<String, TradeSet>,
}

impl TradeData {
    /// Parses the registries: `trades` in registry id order, `sets` by id; `tag` resolves a
    /// `minecraft:villager_trade` tag (without `#`) to ids in tag order and `trade_id` an id.
    pub fn load(
        trades: &[(String, Value)],
        sets: &[(String, Value)],
        tag: &dyn Fn(&str) -> Vec<u32>,
        trade_id: &dyn Fn(&str) -> Option<u32>,
    ) -> TradeData {
        let trades = trades
            .iter()
            .map(|(id, json)| match VillagerTrade::parse(id, json) {
                Ok(t) => Some(t),
                Err(e) => {
                    log::warn!("villager trade {id}: {e}; skipped");
                    None
                }
            })
            .collect();
        let mut by_id = HashMap::new();
        for (id, json) in sets {
            match parse_set(json, tag, trade_id) {
                Ok(set) => {
                    by_id.insert(id.clone(), set);
                }
                Err(e) => log::warn!("trade set {id}: {e}; skipped"),
            }
        }
        TradeData {
            trades,
            sets: by_id,
        }
    }

    /// The trade with registry id `id`.
    pub fn trade(&self, id: u32) -> Option<&VillagerTrade> {
        self.trades
            .get(usize::try_from(id).ok()?)
            .and_then(Option::as_ref)
    }

    /// `addOffersFromTradeSet`: adds the offers of the trade set `set` to `offers`, with the
    /// context's random (the set's random sequence).
    pub fn add_offers(
        &self,
        set: &TradeSet,
        eval: &mut LootEval<'_>,
        holder_set: &dyn Fn(&str, &str) -> Vec<u32>,
        offers: &mut MerchantOffers,
    ) {
        let count = set.amount.sample(eval);
        let mut potential: Vec<u32> = set.trades.clone();
        let mut found = 0;
        while found < count && !potential.is_empty() {
            let len = i32::try_from(potential.len()).unwrap_or(i32::MAX);
            let roll = usize::try_from(eval.rng.next_int(len)).unwrap_or(0);
            if set.allow_duplicates {
                // `addOffersFromItemListings`: a trade without an offer leaves the list.
                let offer = self
                    .trade(potential[roll])
                    .and_then(|t| t.offer(eval, holder_set));
                match offer {
                    Some(o) => {
                        offers.0.push(o);
                        found += 1;
                    }
                    None => {
                        potential.remove(roll);
                    }
                }
            } else {
                // `addOffersFromItemListingsWithoutDuplicates`.
                let trade = potential.remove(roll);
                if let Some(o) = self.trade(trade).and_then(|t| t.offer(eval, holder_set)) {
                    offers.0.push(o);
                    found += 1;
                }
            }
        }
    }
}

fn parse_set(
    v: &Value,
    tag: &dyn Fn(&str) -> Vec<u32>,
    trade_id: &dyn Fn(&str) -> Option<u32>,
) -> Result<TradeSet, String> {
    let o = v.as_object().ok_or("not an object")?;
    let mut trades = Vec::new();
    for entry in holder_entries(o.get("trades").ok_or("missing trades")?) {
        match entry.strip_prefix('#') {
            Some(t) => trades.extend(tag(t)),
            None => trades.extend(trade_id(&entry)),
        }
    }
    Ok(TradeSet {
        trades,
        amount: LootInt::parse(o.get("amount").ok_or("missing amount")?)?,
        allow_duplicates: o
            .get("allow_duplicates")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        random_sequence: o
            .get("random_sequence")
            .and_then(Value::as_str)
            .map(qualify),
    })
}

/// `minecraft:` added to ids without a namespace.
fn qualify(id: &str) -> String {
    if id.contains(':') {
        id.to_owned()
    } else {
        format!("minecraft:{id}")
    }
}

/// A holder set: `"#tag"`, `"id"` or a list of ids, qualified.
fn holder_entries(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => match s.strip_prefix('#') {
            Some(t) => vec![format!("#{}", qualify(t))],
            None => vec![qualify(s)],
        },
        Value::Array(list) => list.iter().filter_map(Value::as_str).map(qualify).collect(),
        _ => Vec::new(),
    }
}

fn item_id(name: &str) -> Result<u16, String> {
    vanillite_data::registry::ITEM
        .id(&qualify(name))
        .ok_or_else(|| format!("unknown item {name}"))
}

/// A `components` object (`DataComponentPatch` or `DataComponentExactPredicate` JSON) as wire
/// values.
fn components(v: Option<&Value>) -> Result<Components, String> {
    let Some(v) = v else {
        return Ok(Vec::new());
    };
    let Tag::Compound(c) = json_to_nbt(v) else {
        return Err("components are not an object".into());
    };
    let patch = crate::item_nbt::patch_from_nbt(&c);
    Ok(patch.added)
}

/// JSON as NBT (numbers become ints or doubles), for component codecs.
pub(crate) fn json_to_nbt(json: &Value) -> Tag {
    match json {
        Value::Null => Tag::Byte(0),
        Value::Bool(b) => Tag::Byte(i8::from(*b)),
        Value::Number(n) => match n.as_i64().and_then(|v| i32::try_from(v).ok()) {
            Some(v) => Tag::Int(v),
            None => Tag::Double(n.as_f64().unwrap_or(0.0)),
        },
        Value::String(s) => Tag::String(s.clone()),
        Value::Array(a) => Tag::List(List(a.iter().map(json_to_nbt).collect())),
        Value::Object(o) => {
            let mut c = Compound::new();
            for (k, v) in o {
                c.insert(k.as_str(), json_to_nbt(v));
            }
            Tag::Compound(c)
        }
    }
}

/// The type id of `minecraft:additional_trade_cost`.
fn additional_trade_cost_type() -> Option<u16> {
    vanillite_data::registry::DATA_COMPONENT_TYPE.id("minecraft:additional_trade_cost")
}

/// `ItemStack.remove(type)` of a patched component.
fn remove_component(stack: &mut ItemStack, t: u16) {
    if let Some(patch) = stack.patch.as_deref_mut() {
        patch.added.retain(|(id, _)| *id != t);
        if patch.is_empty() {
            stack.patch = None;
        }
    }
}
