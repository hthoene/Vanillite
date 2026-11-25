//! Dupe fuzz tests of the loom, cartography table, crafter and lectern menus and of the special
//! crafting recipes: seeded random clicks, buttons, slot state changes, stale client
//! predictions, closing and reopening and breaking the block while the menu is open never
//! create items. Crafting can turn inputs into results, so each scenario weighs item kinds such
//! that every recipe reachable from its items keeps or lowers the total weight.

use std::sync::Arc;

use vanillite_base::random::{RandomSource, XoroshiroRandom};
use vanillite_base::{BlockPos, Vec3};
use vanillite_data::block::{BlockId, BlockState, ids as blocks};
use vanillite_protocol::codec::write_varint;
use vanillite_protocol::components::ids as components;
use vanillite_protocol::packets::play::serverbound::{HashedItem, HashedStack};

use super::container::{MenuKind, Opener, Storage};
use super::tests::{
    NullSink, SURVIVAL, install_data, item, level_with, open, random_click, request, stack,
};
use crate::inventory::{Drop, PlayerInventory};
use crate::item::ItemStack;
use crate::level::{BlockEntity, Level, update};

/// One fuzzed menu.
struct Scenario {
    kind: MenuKind,
    block: BlockId,
    /// The menu's slots live in the block entity (crafter, lectern).
    block_storage: bool,
    /// Puts the items into the inventory and the block.
    setup: fn(&mut Level, BlockPos, &mut PlayerInventory),
    /// Weight of an item kind (default 1).
    weights: &'static [(&'static str, i64)],
    /// Whether the weighted total must stay equal (no recipe can consume anything).
    exact: bool,
}

const WHO: Opener = Opener {
    spectator: false,
    infinite: false,
    disconnected: false,
};

fn weight(s: &Scenario, id: u16) -> i64 {
    s.weights
        .iter()
        .find(|(name, _)| item(name) == id)
        .map_or(1, |&(_, w)| w)
}

/// The weighted item total of everything the player holds, the open menu's real slots, the
/// block entity, the drops and the item entities.
fn total(s: &Scenario, inv: &PlayerInventory, level: &Level, pos: BlockPos, drops: &[Drop]) -> i64 {
    let mut all: Vec<&ItemStack> = inv.items.iter().collect();
    all.extend(inv.armor.iter());
    all.push(&inv.offhand);
    all.extend(inv.craft.iter());
    all.push(&inv.menu.carried);
    if let Some(menu) = &inv.container {
        all.push(&menu.state.carried);
        // Result slots only show what taking would make.
        let result = match menu.kind {
            MenuKind::Crafting | MenuKind::Crafter => Some(0),
            MenuKind::Loom => Some(3),
            MenuKind::CartographyTable => Some(2),
            _ => None,
        };
        if menu.kind != MenuKind::Crafter {
            all.extend(
                menu.local
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| Some(*i) != result)
                    .map(|(_, st)| st),
            );
        }
    }
    match level.block_entity(pos) {
        Some(BlockEntity::Crafter(c)) => all.extend(c.items.iter()),
        Some(BlockEntity::Lectern(l)) => all.push(&l.book),
        _ => {}
    }
    all.extend(drops.iter().map(|d| &d.stack));
    all.extend(level.entities.items.iter().map(|(_, d)| &d.stack));
    all.into_iter()
        .filter(|st| !st.is_empty())
        .map(|st| i64::from(st.count) * weight(s, st.item))
        .sum()
}

fn open_scenario(s: &Scenario, inv: &mut PlayerInventory, level: &mut Level, pos: BlockPos) {
    let storage = if s.block_storage {
        Storage::Block(pos)
    } else {
        Storage::Local
    };
    open(inv, level, request(s.kind, storage, pos, s.block));
}

fn pick<T: Copy>(rng: &mut XoroshiroRandom, values: &[T]) -> T {
    let n = i32::try_from(values.len()).unwrap_or(1);
    values[usize::try_from(rng.next_int_bounded(n)).unwrap_or(0)]
}

/// A random hashed stack the client may claim to have.
fn random_prediction(rng: &mut XoroshiroRandom) -> HashedStack {
    let item = (rng.next_int_bounded(2) == 0).then(|| HashedItem {
        item: rng.next_int_bounded(1000),
        count: rng.next_int_bounded(70),
        added: Vec::new(),
        removed: Vec::new(),
    });
    HashedStack { item }
}

/// One action like the connection would run it; returns whether the block was broken.
fn act(
    s: &Scenario,
    rng: &mut XoroshiroRandom,
    inv: &mut PlayerInventory,
    level: &mut Level,
    pos: BlockPos,
    drops: &mut Vec<Drop>,
) -> bool {
    let sink = NullSink;
    match rng.next_int_bounded(40) {
        // Close (the client's `container_close`) and reopen if the block is still there.
        0 => {
            super::container::close(inv, level, WHO, drops, None);
            if level.block(pos).block() == s.block {
                open_scenario(s, inv, level, pos);
            }
            false
        }
        // The block is broken while the menu is open.
        1 if rng.next_int_bounded(8) == 0 => {
            level.set_block(pos, BlockState::AIR, update::ALL);
            true
        }
        // `container_button_click`.
        2..=6 => {
            if inv.container.is_none() || !super::container::still_valid(inv, level, &|_| true, &|_| true) {
                return false;
            }
            let id = match rng.next_int_bounded(3) {
                0 => pick(rng, &[-1, 0, 1, 2, 3, 4, 5, 99, 100, 101, 105, 1000]),
                1 => pick(rng, &[i32::MAX, i32::MIN, i32::MIN + 1, i32::MAX - 100]),
                _ => rng.next_int_bounded(70),
            };
            let may_build = rng.next_int_bounded(4) != 0;
            super::container::with_view(inv, level, Vec3::default(), None, |view| {
                view.menu.may_build = may_build;
                if view.button(id, drops) {
                    super::broadcast_changes(view, &sink);
                }
            });
            false
        }
        // `container_slot_state_changed`.
        7..=8 => {
            if inv.container.is_none() {
                return false;
            }
            let slot = rng.next_int_bounded(60) - 5;
            let enabled = rng.next_int_bounded(2) == 0;
            super::container::with_view(inv, level, Vec3::default(), None, |view| {
                view.crafter_slot_state(slot, enabled);
            });
            false
        }
        // `container_click`, with a stale state id now and then.
        _ => {
            let Some(menu) = &inv.container else {
                return false;
            };
            let slots = i32::try_from(menu.slot_count()).unwrap_or(0) + 6;
            let stale = rng.next_int_bounded(5) == 0;
            let (slot, button, input) = random_click(rng, slots);
            let changed: Vec<(i16, HashedStack)> = (0..rng.next_int_bounded(3))
                .map(|_| {
                    (
                        i16::try_from(rng.next_int_bounded(slots) - 2).unwrap_or(0),
                        random_prediction(rng),
                    )
                })
                .collect();
            let carried = random_prediction(rng);
            if !super::container::still_valid(inv, level, &|_| true, &|_| true) {
                return false;
            }
            super::container::with_view(inv, level, Vec3::default(), None, |view| {
                if !super::is_valid_slot_index(view, slot) {
                    return;
                }
                view.menu.state.suppress_remote = true;
                super::click(view, slot, button, input, SURVIVAL, drops);
                view.flush_result_updates(&sink);
                super::receive_predictions(&mut view.menu.state, &changed, &carried);
                view.menu.state.suppress_remote = false;
                if stale {
                    super::broadcast_full_state(view, &sink);
                } else {
                    super::broadcast_changes(view, &sink);
                }
            });
            false
        }
    }
}

fn fuzz(name: &str, s: &Scenario, seed: i64, rounds: usize, steps: usize) {
    install_data();
    let pos = BlockPos::new(0, 0, 0);
    let mut rng = XoroshiroRandom::new(seed);
    for round in 0..rounds {
        let mut level = level_with(pos, s.block.default_state());
        let mut inv = PlayerInventory::default();
        (s.setup)(&mut level, pos, &mut inv);
        open_scenario(s, &mut inv, &mut level, pos);
        let mut drops = Vec::new();
        let start = total(s, &inv, &level, pos, &drops);
        let mut last = start;
        for step in 0..steps {
            act(s, &mut rng, &mut inv, &mut level, pos, &mut drops);
            let now = total(s, &inv, &level, pos, &drops);
            assert!(
                now <= last,
                "{name}: round {round} step {step}: weighted items grew from {last} to {now}"
            );
            if s.exact {
                assert_eq!(now, start, "{name}: round {round} step {step}: items lost");
            }
            last = now;
        }
        super::container::close(&mut inv, &mut level, WHO, &mut drops, None);
        let end = total(s, &inv, &level, pos, &drops);
        assert!(end <= last, "{name}: round {round}: closing made items");
        if s.exact {
            assert_eq!(end, start, "{name}: round {round}: closing lost items");
        }
    }
}

// --- Item helpers ----------------------------------------------------------------------------

fn put(inv: &mut PlayerInventory, list: Vec<ItemStack>) {
    for (slot, st) in [0, 1, 2, 9, 10, 20, 27, 35, 5, 30].into_iter().zip(list) {
        inv.items[slot] = st;
    }
}

fn banner(name: &str, layers: &[(u32, i32)], count: i32) -> ItemStack {
    let mut s = stack(name, count);
    for &(pattern, color) in layers {
        super::station::add_layer(&mut s, pattern, color);
    }
    s
}

/// A store with one fresh map; returns its id.
fn with_map(level: &mut Level) -> i32 {
    let maps = Arc::new(crate::map::Maps::new(Box::new(|_| None)));
    let id = maps.free_id();
    maps.set(
        id,
        crate::map::MapData::fresh(0.0, 0.0, 0, true, false, "minecraft:overworld".into()),
    );
    level.set_maps(maps);
    id
}

fn filled_map(id: i32, count: i32) -> ItemStack {
    let mut s = stack("filled_map", count);
    crate::map::set_map_id(&mut s, id);
    s
}

/// A writable book with `pages` pages.
fn book(pages: i32) -> ItemStack {
    let mut value = Vec::new();
    write_varint(&mut value, pages);
    for _ in 0..pages {
        value.extend_from_slice(&[1, b'a', 0]);
    }
    let mut s = stack("writable_book", 1);
    s.patch_mut()
        .set(components::WRITABLE_BOOK_CONTENT, value.into_boxed_slice());
    s
}

// --- Scenarios -------------------------------------------------------------------------------

#[test]
fn loom_fuzz_conserves_items() {
    fn setup(_: &mut Level, _: BlockPos, inv: &mut PlayerInventory) {
        put(
            inv,
            vec![
                stack("white_banner", 5),
                stack("red_dye", 10),
                stack("flower_banner_pattern", 1),
                banner("white_banner", &[(1, 3); 6], 1),
                stack("blue_dye", 3),
                stack("diamond", 7),
                banner("red_banner", &[(2, 0)], 2),
            ],
        );
    }
    let s = Scenario {
        kind: MenuKind::Loom,
        block: blocks::LOOM,
        block_storage: false,
        setup,
        weights: &[],
        exact: false,
    };
    fuzz("loom", &s, 11, 40, 300);
}

#[test]
fn cartography_table_fuzz_conserves_items() {
    fn setup(level: &mut Level, _: BlockPos, inv: &mut PlayerInventory) {
        let id = with_map(level);
        put(
            inv,
            vec![
                filled_map(id, 3),
                stack("paper", 6),
                stack("map", 4),
                stack("glass_pane", 3),
                filled_map(id, 1),
                stack("diamond", 2),
            ],
        );
    }
    let s = Scenario {
        kind: MenuKind::CartographyTable,
        block: blocks::CARTOGRAPHY_TABLE,
        block_storage: false,
        setup,
        weights: &[],
        exact: false,
    };
    fuzz("cartography", &s, 12, 40, 300);
}

#[test]
fn crafter_fuzz_conserves_items() {
    fn setup(level: &mut Level, pos: BlockPos, inv: &mut PlayerInventory) {
        if let Some(BlockEntity::Crafter(c)) = level.block_entity_mut(pos) {
            c.items[0] = stack("oak_planks", 2);
            c.items[4] = stack("stick", 5);
        }
        put(
            inv,
            vec![
                stack("oak_planks", 40),
                stack("stick", 20),
                stack("diamond", 3),
                stack("iron_ingot", 64),
            ],
        );
    }
    let s = Scenario {
        kind: MenuKind::Crafter,
        block: blocks::CRAFTER,
        block_storage: true,
        setup,
        weights: &[],
        exact: true,
    };
    fuzz("crafter", &s, 13, 40, 300);
}

#[test]
fn lectern_fuzz_conserves_the_book() {
    fn setup(level: &mut Level, pos: BlockPos, inv: &mut PlayerInventory) {
        assert!(crate::block::lectern::place_book(level, None, pos, book(5)));
        put(inv, vec![stack("diamond", 64), stack("stone", 64)]);
    }
    let s = Scenario {
        kind: MenuKind::Lectern,
        block: blocks::LECTERN,
        block_storage: true,
        setup,
        weights: &[("writable_book", 1000)],
        exact: true,
    };
    fuzz("lectern", &s, 14, 40, 200);
}

/// A crafting table scenario with the items of one special recipe.
fn crafting(
    setup: fn(&mut Level, BlockPos, &mut PlayerInventory),
    weights: &'static [(&'static str, i64)],
) -> Scenario {
    Scenario {
        kind: MenuKind::Crafting,
        block: blocks::CRAFTING_TABLE,
        block_storage: false,
        setup,
        weights,
        exact: false,
    }
}

#[test]
fn banner_duplicate_fuzz_conserves_items() {
    fn setup(_: &mut Level, _: BlockPos, inv: &mut PlayerInventory) {
        put(
            inv,
            vec![
                banner("white_banner", &[(1, 3), (4, 5)], 1),
                stack("white_banner", 6),
                banner("white_banner", &[(2, 1)], 2),
                stack("red_banner", 2),
            ],
        );
    }
    fuzz("banner duplicate", &crafting(setup, &[]), 21, 40, 300);
}

#[test]
fn shield_decoration_fuzz_conserves_items() {
    fn setup(_: &mut Level, _: BlockPos, inv: &mut PlayerInventory) {
        put(
            inv,
            vec![
                stack("shield", 1),
                banner("red_banner", &[(1, 3)], 2),
                stack("shield", 1),
                stack("white_banner", 2),
            ],
        );
    }
    fuzz("shield", &crafting(setup, &[("shield", 100)]), 22, 40, 300);
}

#[test]
fn map_extending_and_cloning_fuzz_conserves_items() {
    fn extend(level: &mut Level, _: BlockPos, inv: &mut PlayerInventory) {
        let id = with_map(level);
        put(
            inv,
            vec![filled_map(id, 2), stack("paper", 30), stack("paper", 10)],
        );
    }
    fn clone(level: &mut Level, _: BlockPos, inv: &mut PlayerInventory) {
        let id = with_map(level);
        put(
            inv,
            vec![filled_map(id, 1), stack("map", 12), stack("map", 3)],
        );
    }
    fuzz(
        "map extending",
        &crafting(extend, &[("filled_map", 100)]),
        23,
        40,
        300,
    );
    fuzz("map cloning", &crafting(clone, &[]), 24, 40, 300);
}

#[test]
fn firework_fuzz_conserves_items() {
    fn setup(_: &mut Level, _: BlockPos, inv: &mut PlayerInventory) {
        put(
            inv,
            vec![
                stack("gunpowder", 20),
                stack("paper", 8),
                stack("red_dye", 6),
                stack("glowstone_dust", 4),
                stack("diamond", 2),
                stack("firework_star", 2),
                stack("blue_dye", 3),
            ],
        );
    }
    // A rocket takes at least a paper and a gunpowder for three rockets; a star at least a
    // gunpowder and a dye.
    let weights = &[
        ("gunpowder", 3),
        ("paper", 3),
        ("firework_rocket", 2),
        ("firework_star", 4),
    ];
    fuzz("firework", &crafting(setup, weights), 25, 40, 300);
}

#[test]
fn armor_dye_fuzz_conserves_items() {
    fn setup(_: &mut Level, _: BlockPos, inv: &mut PlayerInventory) {
        put(
            inv,
            vec![
                stack("leather_helmet", 1),
                stack("red_dye", 6),
                stack("blue_dye", 4),
                stack("leather_boots", 1),
                stack("white_dye", 2),
            ],
        );
    }
    let weights = &[("leather_helmet", 100), ("leather_boots", 100)];
    fuzz("dye", &crafting(setup, weights), 26, 40, 300);
}
