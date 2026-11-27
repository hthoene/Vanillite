//! Merchant tests: the Vanilla trades load and give offers, offers survive NBT and the wire,
//! and a trade through the merchant menu pays, uses the offer and gives the villager
//! experience.

use vanillite_base::{BlockPos, Vec3};
use vanillite_protocol::codec::{Decode, Encode, Reader};
use vanillite_protocol::packets::play::serverbound::ContainerInput;
use vanillite_world::entity::EntityId;

use super::container::{MenuKind, Storage};
use super::tests::{install_data, open, request, stack};
use crate::inventory::PlayerInventory;
use crate::item::ItemStack;
use crate::level::Level;
use crate::mob::fam::village::{self, merchant};
use crate::mob::{Kind, MobData};
use crate::trading::{ItemCost, MerchantOffer, MerchantOffers};

fn villager(level: &mut Level, profession: &str, lvl: i32) -> EntityId {
    let mut m = MobData::new(Kind::Villager, 1);
    let s = village::state_mut(&mut m);
    s.villager.profession = vanillite_data::registry::VILLAGER_PROFESSION
        .id(profession)
        .expect("profession");
    s.villager.villager_type = vanillite_data::registry::VILLAGER_TYPE
        .id("minecraft:plains")
        .expect("type");
    s.villager.level = lvl;
    crate::mob::spawn::add_mob(
        level,
        m,
        Vec3::new(0.5, 1.0, 0.5),
        0.0,
        crate::mob::fam::SpawnReason::Command,
    )
}

#[test]
fn vanilla_trades_load() {
    let data = install_data();
    let t = &data.trades;
    assert!(t.trades.len() > 380, "{}", t.trades.len());
    assert!(t.trades.iter().all(Option::is_some));
    assert!(t.sets.len() >= 68, "{}", t.sets.len());
    let set = &t.sets["minecraft:librarian/level_1"];
    assert!(!set.trades.is_empty());
    assert_eq!(
        set.random_sequence.as_deref(),
        Some("minecraft:trade_set/librarian/level_1")
    );
}

#[test]
fn offers_generate_from_the_trade_sets_and_round_trip() {
    install_data();
    let mut level = crate::level::tests::flat_level();
    let id = villager(&mut level, "minecraft:farmer", 1);
    let offers = merchant::offers_in_level(&mut level, id)
        .expect("offers")
        .clone();
    // `farmer/level_1`: two offers.
    assert_eq!(offers.0.len(), 2);
    for o in &offers.0 {
        assert!(o.max_uses >= 1);
        assert!(o.base_cost_a.count >= 1);
        assert!(!o.result.is_empty());
    }
    // The same world seed gives the same offers (the trade set's random sequence).
    let mut other = crate::level::tests::flat_level();
    let id2 = villager(&mut other, "minecraft:farmer", 1);
    assert_eq!(
        merchant::offers_in_level(&mut other, id2).expect("offers"),
        &offers
    );
    // NBT.
    let nbt = crate::trading::nbt::offers_to_nbt(&offers);
    assert_eq!(crate::trading::nbt::offers_from_nbt(&nbt), offers);
    // Wire.
    let packet = merchant::offers_packet(&mut level, id, 3).expect("packet");
    let mut bytes = Vec::new();
    packet.encode(&mut bytes);
    let decoded = vanillite_protocol::packets::play::clientbound::MerchantOffers::decode(
        &mut Reader::new(&bytes),
    )
    .expect("decode");
    assert_eq!(decoded, packet);
    assert_eq!((decoded.villager_level, decoded.can_restock), (1, true));
}

#[test]
fn nitwits_have_no_offers() {
    install_data();
    let mut level = crate::level::tests::flat_level();
    let id = villager(&mut level, "minecraft:nitwit", 1);
    assert!(
        merchant::offers_in_level(&mut level, id)
            .expect("offers")
            .0
            .is_empty()
    );
}

#[test]
fn a_trade_pays_uses_the_offer_and_gives_experience() {
    install_data();
    let mut level = crate::level::tests::flat_level();
    let id = villager(&mut level, "minecraft:librarian", 1);
    let paper = stack("paper", 1).item;
    let emerald = stack("emerald", 1).item;
    let offer = MerchantOffer::new(
        ItemCost::new(paper, 24),
        None,
        ItemStack::new(emerald, 1),
        0,
        16,
        2,
        0.05,
        0,
    );
    if let Some(m) = level.entities.mob_mut(id) {
        let s = village::state_mut(m);
        s.merchant.offers = Some(MerchantOffers(vec![offer]));
        s.merchant.trading_player = Some(id);
    }
    let mut inv = PlayerInventory::default();
    inv.items[5] = stack("paper", 64);
    open(
        &mut inv,
        &mut level,
        request(
            MenuKind::Merchant,
            Storage::Merchant(id),
            BlockPos::new(0, 1, 0),
            vanillite_data::block::BlockId(0),
        ),
    );
    let mut drops = Vec::new();
    super::container::with_view(&mut inv, &mut level, Vec3::default(), None, None, |view| {
        view.merchant_select(0);
        assert_eq!(view.menu.local[0].count, 64);
        assert_eq!(view.menu.local[2].item, emerald);
        super::click(
            view,
            2,
            0,
            ContainerInput::QuickMove,
            super::tests::SURVIVAL,
            &mut drops,
        );
    });
    let menu = inv.container.as_ref().expect("menu");
    // Two trades of 24 paper: 16 left, three emeralds would need 72.
    assert_eq!(menu.local[0].count, 16);
    let emeralds: i32 = inv
        .items
        .iter()
        .filter(|s| s.item == emerald)
        .map(|s| s.count)
        .sum();
    assert_eq!(emeralds, 2);
    let m = level.entities.mob(id).expect("villager");
    let s = village::state(m);
    assert_eq!(s.merchant.offers.as_ref().expect("offers").0[0].uses, 2);
    assert_eq!(s.merchant.villager_xp, 4);
}

#[test]
fn the_inventory_stacks_like_a_simple_container() {
    let mut m = village::Merchant::default();
    for _ in 0..10 {
        let rest = merchant::add_item(&mut m, ItemStack::new(1, 64));
        if !rest.is_empty() {
            assert_eq!(rest.count, 64);
        }
    }
    assert!(m.inventory.iter().all(|s| s.count == 64));
    assert!(!merchant::can_add_item(&m, &ItemStack::new(1, 1)));
}
