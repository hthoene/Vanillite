//! Villager and wandering trader trades: the Java merchant menu as a Bedrock form. Picking an
//! offer selects the trade (the server moves the payment into the trade slots) and takes the
//! result into the inventory, like a shift click on the Java result slot.

use vanillite_protocol::codec::{Reader as JReader, write_varint};
use vanillite_protocol::item::ItemStack;
use vanillite_protocol::packets::play::serverbound::{self as psb, ContainerInput, HashedStack};

use super::Session;
use crate::forms::{Button, Form};
use crate::packets::id;

/// Form id of the trade list (outside the ids of plugin forms, which count up from 0).
pub(super) const TRADE_FORM: u32 = u32::MAX;

/// Most offers shown (Vanilla merchants have at most a few dozen).
const MAX_OFFERS: usize = 64;

/// Java's merchant menu slots: two payments, the result, then the player inventory.
const RESULT_SLOT: i16 = 2;

/// An item and its count.
type Cost = (u16, i32);

/// One offer as the player sees it.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Offer {
    cost_a: Cost,
    cost_b: Option<Cost>,
    result: Cost,
    out_of_stock: bool,
}

/// The open Java merchant menu.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Merchant {
    container: i32,
    state_id: i32,
    title: String,
    offers: Vec<Offer>,
}

/// Reads an `ItemCost`: item, count and the exact components (skipped).
fn item_cost(r: &mut JReader<'_>) -> vanillite_protocol::codec::Result<Cost> {
    let item = u16::try_from(r.varint()?).unwrap_or(0);
    let count = r.varint()?;
    let n = r.varint()?;
    if !(0..=64).contains(&n) {
        return Err(vanillite_protocol::codec::Error::Invalid("component count"));
    }
    for _ in 0..n {
        let kind = u16::try_from(r.varint()?).unwrap_or(u16::MAX);
        let rest = r.rest();
        let len = vanillite_protocol::components::value_len(kind, rest)?;
        r.skip(len)?;
    }
    Ok((item, count))
}

/// Reads the offers of `merchant_offers` after the container id.
fn read_offers(r: &mut JReader<'_>) -> vanillite_protocol::codec::Result<Vec<Offer>> {
    let n = usize::try_from(r.varint()?).unwrap_or(usize::MAX);
    if n > MAX_OFFERS {
        return Err(vanillite_protocol::codec::Error::Invalid("offer count"));
    }
    let mut offers = Vec::with_capacity(n);
    for _ in 0..n {
        let (item, count) = item_cost(r)?;
        let result = ItemStack::decode_plain(r)?;
        let cost_b = if r.u8()? != 0 {
            Some(item_cost(r)?)
        } else {
            None
        };
        let out_of_stock = r.u8()? != 0;
        let (_uses, _max_uses, _xp) = (r.i32()?, r.i32()?, r.i32()?);
        let special = r.i32()?;
        let multiplier = r.f32()?;
        let demand = r.i32()?;
        // `MerchantOffer.getCostA`: the demand and the special price change the first cost.
        let extra = (f64::from(count) * f64::from(demand) * f64::from(multiplier)).floor();
        let extra = if extra > 0.0 {
            crate::num::floor_i32(extra.min(f64::from(i32::MAX)))
        } else {
            0
        };
        let max = if usize::from(item) < vanillite_data::item::ITEMS.len() {
            i32::from(vanillite_data::item::ItemId(item).info().max_stack_size).max(1)
        } else {
            64
        };
        let cost_a = count
            .saturating_add(extra)
            .saturating_add(special)
            .clamp(1, max);
        offers.push(Offer {
            cost_a: (item, cost_a),
            cost_b,
            result: (result.item, result.count),
            out_of_stock,
        });
    }
    Ok(offers)
}

/// "3 Emerald" for an item id and count.
fn label((item, count): Cost) -> String {
    let name = vanillite_data::registry::ITEM
        .name(item)
        .map_or("?", |n| n.strip_prefix("minecraft:").unwrap_or(n));
    let mut pretty = String::with_capacity(name.len().saturating_add(4));
    for (i, word) in name.split('_').enumerate() {
        if i > 0 {
            pretty.push(' ');
        }
        let mut chars = word.chars();
        if let Some(c) = chars.next() {
            pretty.extend(c.to_uppercase());
            pretty.push_str(chars.as_str());
        }
    }
    format!("{count} {pretty}")
}

impl Session {
    /// `open_screen`: a merchant menu starts a trade list; other menus are not shown.
    pub(super) fn java_open_screen(&mut self, r: &mut JReader<'_>) {
        let (Ok(container), Ok(menu)) = (r.varint(), r.varint()) else {
            return;
        };
        let merchant = u16::try_from(menu)
            .ok()
            .and_then(|m| vanillite_data::registry::MENU.name(m))
            == Some("minecraft:merchant");
        let title = crate::entity::component(r)
            .map(|c| c.plain())
            .unwrap_or_default();
        if let Some(w) = &mut self.world {
            w.merchant = merchant.then(|| Merchant {
                container,
                state_id: 0,
                title,
                offers: Vec::new(),
            });
        }
    }

    /// `merchant_offers`: the offers of the open merchant menu.
    pub(super) fn java_merchant_offers(&mut self, r: &mut JReader<'_>) {
        let Ok(container) = r.varint() else {
            return;
        };
        let Ok(offers) = read_offers(r) else {
            return;
        };
        let Some(m) = self
            .world
            .as_mut()
            .and_then(|w| w.merchant.as_mut())
            .filter(|m| m.container == container)
        else {
            return;
        };
        m.offers = offers;
        self.show_trades();
    }

    /// Whether `container` is the open merchant menu (and takes its state id).
    pub(super) fn merchant_state(&mut self, container: i32, state_id: i32) -> bool {
        match self.world.as_mut().and_then(|w| w.merchant.as_mut()) {
            Some(m) if m.container == container => {
                m.state_id = state_id;
                true
            }
            _ => false,
        }
    }

    /// The server closed a menu.
    pub(super) fn java_container_closed(&mut self, container: i32) {
        if let Some(w) = &mut self.world
            && w.merchant
                .as_ref()
                .is_some_and(|m| m.container == container)
        {
            w.merchant = None;
        }
    }

    fn show_trades(&mut self) {
        let Some(m) = self.world.as_ref().and_then(|w| w.merchant.as_ref()) else {
            return;
        };
        let buttons = m
            .offers
            .iter()
            .map(|o| {
                let mut text = label(o.cost_a);
                if let Some(b) = o.cost_b {
                    text.push_str(" + ");
                    text.push_str(&label(b));
                }
                text.push_str(" -> ");
                text.push_str(&label(o.result));
                if o.out_of_stock {
                    text.push_str(" (out of stock)");
                }
                Button { text, image: None }
            })
            .collect();
        let form = Form::Simple {
            title: if m.title.is_empty() {
                "Trades".to_owned()
            } else {
                m.title.clone()
            },
            content: String::new(),
            buttons,
        };
        self.send(
            id::MODAL_FORM_REQUEST,
            &crate::packets::modal_form_request(TRADE_FORM, &form.to_json()),
        );
    }

    /// The answer to the trade list: an offer to trade once, or closed.
    pub(super) fn trade_answer(&mut self, data: Option<&str>) {
        let Some(m) = self.world.as_ref().and_then(|w| w.merchant.clone()) else {
            return;
        };
        let choice = data
            .and_then(|d| d.trim().parse::<usize>().ok())
            .filter(|&i| i < m.offers.len());
        let Some(i) = choice else {
            if let Some(w) = &mut self.world {
                w.merchant = None;
            }
            self.java_send(&psb::ContainerClose {
                container_id: m.container,
            });
            return;
        };
        let mut body = Vec::with_capacity(8);
        write_varint(
            &mut body,
            vanillite_protocol::ids::play::serverbound::SELECT_TRADE,
        );
        write_varint(&mut body, i32::try_from(i).unwrap_or(0));
        self.java_send_raw(body);
        self.java_send(&psb::ContainerClick {
            container_id: m.container,
            state_id: m.state_id,
            slot: RESULT_SLOT,
            button: 0,
            input: ContainerInput::QuickMove,
            changed: Vec::new(),
            carried: HashedStack::default(),
        });
        self.show_trades();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_offers_with_the_effective_price() {
        let emerald = vanillite_data::registry::ITEM
            .id("minecraft:emerald")
            .unwrap_or(0);
        let mut b = Vec::new();
        write_varint(&mut b, 1);
        // Cost A: 10 emeralds, no components.
        write_varint(&mut b, i32::from(emerald));
        write_varint(&mut b, 10);
        write_varint(&mut b, 0);
        // Result: 1 emerald.
        ItemStack::new(emerald, 1).encode_untrusted(&mut b);
        b.push(0);
        b.push(0);
        for v in [0i32, 12, 1, -2] {
            b.extend_from_slice(&v.to_be_bytes());
        }
        b.extend_from_slice(&0.05f32.to_be_bytes());
        b.extend_from_slice(&4i32.to_be_bytes());
        let offers = read_offers(&mut JReader::new(&b)).unwrap_or_default();
        // 10 + floor(10 * 4 * 0.05) - 2 = 10.
        assert_eq!(
            offers,
            vec![Offer {
                cost_a: (emerald, 10),
                cost_b: None,
                result: (emerald, 1),
                out_of_stock: false,
            }]
        );
        assert_eq!(label((emerald, 3)), "3 Emerald");
    }
}
