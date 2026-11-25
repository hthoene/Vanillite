//! Bundles (`BundleItem`, `BundleContents`): weights, inserting and removing items, the
//! selected item and the click overrides of menus (`overrideStackedOnOther`,
//! `overrideOtherStackedOnMe`).
//!
//! The selected item (`BundleContents.selectedItem`) is runtime state that no codec carries.
//! The client selects one item of one bundle at a time (`bundle_item_selected`), so the player
//! keeps one selection: the contents it belongs to and the index. A removal from a bundle with
//! those contents takes the selected item and clears the selection, as Vanilla's
//! `removeOne` does.

use vanillite_data::block::BlockKind;
use vanillite_protocol::codec::{Reader, write_len};
use vanillite_protocol::components::ids;

use crate::item::{self, ItemStack};

/// A fraction `num / den` (`org.apache.commons.lang3.math.Fraction`), reduced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fraction {
    num: i64,
    den: i64,
}

fn gcd(a: i64, b: i64) -> i64 {
    let (mut a, mut b) = (a.abs(), b.abs());
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a.max(1)
}

impl Fraction {
    /// `ZERO`.
    pub const ZERO: Fraction = Fraction { num: 0, den: 1 };
    /// `ONE`.
    pub const ONE: Fraction = Fraction { num: 1, den: 1 };

    /// `getFraction(num, den)`.
    pub fn new(num: i64, den: i64) -> Fraction {
        let den = if den == 0 { 1 } else { den };
        let g = gcd(num, den);
        let sign = if den < 0 { -1 } else { 1 };
        Fraction {
            num: sign * num / g,
            den: sign * den / g,
        }
    }

    fn add(self, o: Fraction) -> Fraction {
        Fraction::new(
            self.num.saturating_mul(o.den) + o.num.saturating_mul(self.den),
            self.den.saturating_mul(o.den),
        )
    }

    fn sub(self, o: Fraction) -> Fraction {
        self.add(Fraction::new(-o.num, o.den))
    }

    fn mul(self, n: i64) -> Fraction {
        Fraction::new(self.num.saturating_mul(n), self.den)
    }

    /// `compareTo(ONE) >= 0`.
    pub fn is_full(self) -> bool {
        self.num >= self.den
    }

    /// `ONE.subtract(weight).divideBy(itemWeight).intValue()`, at least 0.
    fn max_amount(self, item_weight: Fraction) -> i32 {
        let rest = Fraction::ONE.sub(self);
        if item_weight.num <= 0 {
            return 0;
        }
        // (rest.num / rest.den) / (w.num / w.den), truncated towards zero.
        let q = rest.num.saturating_mul(item_weight.den) / rest.den.saturating_mul(item_weight.num);
        i32::try_from(q.max(0)).unwrap_or(i32::MAX)
    }
}

/// Whether the item is a bundle (its prototype has `bundle_contents`).
fn is_bundle_item(stack: &ItemStack) -> bool {
    let name = item::item_id(stack).name();
    name == "minecraft:bundle" || name.ends_with("_bundle")
}

/// `get(BUNDLE_CONTENTS) != null`.
pub fn has_contents(stack: &ItemStack) -> bool {
    !stack.is_empty()
        && (stack.component(ids::BUNDLE_CONTENTS).is_some()
            || (is_bundle_item(stack)
                && !stack
                    .patch()
                    .is_some_and(|p| p.is_removed(ids::BUNDLE_CONTENTS))))
}

/// The items of a bundle (`items()`), `None` without the component.
pub fn items(stack: &ItemStack) -> Option<Vec<ItemStack>> {
    if !has_contents(stack) {
        return None;
    }
    let Some(value) = stack.component(ids::BUNDLE_CONTENTS) else {
        return Some(Vec::new());
    };
    let mut r = Reader::new(value);
    let n = r.len_prefix("bundle contents", 1 << 16, 3).ok()?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(ItemStack::decode_template(&mut r).ok()?);
    }
    Some(out)
}

/// `set(BUNDLE_CONTENTS, contents)`: the default (empty) contents of a bundle item drop out of
/// the patch.
pub fn set_items(stack: &mut ItemStack, items: &[ItemStack]) {
    if items.is_empty() && is_bundle_item(stack) {
        let patch = stack.patch_mut();
        patch.unset(ids::BUNDLE_CONTENTS);
        patch.removed.retain(|&t| t != ids::BUNDLE_CONTENTS);
        return;
    }
    let mut out = Vec::new();
    write_len(&mut out, items.len());
    for s in items {
        s.encode_template(&mut out);
    }
    stack
        .patch_mut()
        .set(ids::BUNDLE_CONTENTS, out.into_boxed_slice());
}

/// `BundleContents.getWeight(item)`; `None` for an error (an excessive nested weight).
fn weight_of(stack: &ItemStack, depth: u32) -> Option<Fraction> {
    if has_contents(stack) {
        if depth > 16 {
            return None;
        }
        let nested = content_weight(&items(stack)?, depth + 1)?;
        return Some(nested.add(Fraction::new(1, 16)));
    }
    let bees = stack
        .component(ids::BEES)
        .and_then(|v| Reader::new(v).varint().ok())
        .is_some_and(|n| n > 0);
    if bees {
        return Some(Fraction::ONE);
    }
    Some(Fraction::new(
        1,
        i64::from(item::max_stack_size(stack).max(1)),
    ))
}

/// `BundleContents.getWeight(item)` of one item.
pub fn item_weight(stack: &ItemStack) -> Option<Fraction> {
    weight_of(stack, 0)
}

/// `computeContentWeight`.
fn content_weight(items: &[ItemStack], depth: u32) -> Option<Fraction> {
    let mut w = Fraction::ZERO;
    for s in items {
        w = w.add(weight_of(s, depth)?.mul(i64::from(s.count)));
    }
    Some(w)
}

/// The weight of a bundle's contents, `ONE` on an error (`getWeightSafe`).
pub fn weight(stack: &ItemStack) -> Fraction {
    items(stack)
        .and_then(|i| content_weight(&i, 0))
        .unwrap_or(Fraction::ONE)
}

/// `canItemBeInBundle`: not empty and not a shulker box (`canFitInsideContainerItems`).
pub fn can_be_in_bundle(stack: &ItemStack) -> bool {
    !stack.is_empty()
        && !item::item_id(stack)
            .block()
            .is_some_and(|b| b.info().kind == BlockKind::ShulkerBoxBlock)
}

/// A key of a bundle's contents, to find the bundle a selection belongs to.
pub fn key(stack: &ItemStack) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    stack.item.hash(&mut h);
    stack.component(ids::BUNDLE_CONTENTS).hash(&mut h);
    h.finish()
}

/// `BundleContents.Mutable`.
#[derive(Clone, Debug)]
pub struct Contents {
    /// The stacks, newest first.
    pub items: Vec<ItemStack>,
    weight: Fraction,
    /// `selectedItem` (`-1`: none).
    pub selected: i32,
}

impl Contents {
    /// `asMutable()` of a stack with the bundle selection `selection` of its player.
    pub fn of(stack: &ItemStack, selection: Option<(u64, i32)>) -> Option<Contents> {
        let items = items(stack)?;
        let selected = selection
            .filter(|(k, _)| *k == key(stack))
            .map_or(-1, |(_, i)| i);
        Some(match content_weight(&items, 0) {
            Some(weight) => Contents {
                items,
                weight,
                selected,
            },
            // An invalid weight gives an empty mutable copy.
            None => Contents {
                items: Vec::new(),
                weight: Fraction::ZERO,
                selected: -1,
            },
        })
    }

    /// Writes the contents back to `stack` (`toImmutable` + `set`); the selection to keep.
    pub fn store(&self, stack: &mut ItemStack) -> Option<(u64, i32)> {
        set_items(stack, &self.items);
        (self.selected >= 0).then(|| (key(stack), self.selected))
    }

    fn out_of_bounds(&self, i: i32) -> bool {
        usize::try_from(i).map_or(true, |i| i >= self.items.len())
    }

    /// `toggleSelectedItem`.
    pub fn toggle_selected(&mut self, i: i32) {
        self.selected = if self.selected != i && !self.out_of_bounds(i) {
            i
        } else {
            -1
        };
    }

    /// `getMaxAmountToAdd(weight)`.
    pub fn max_amount(&self, item_weight: Fraction) -> i32 {
        self.weight.max_amount(item_weight)
    }

    /// `tryInsert`: moves what fits from `stack` to the front; the number moved.
    pub fn try_insert(&mut self, stack: &mut ItemStack) -> i32 {
        if !can_be_in_bundle(stack) {
            return 0;
        }
        let Some(w) = weight_of(stack, 0) else {
            return 0;
        };
        let amount = stack.count.min(self.max_amount(w));
        if amount <= 0 {
            return 0;
        }
        self.weight = self.weight.add(w.mul(i64::from(amount)));
        let found = if item::is_stackable(stack) {
            self.items
                .iter()
                .position(|s| s.same_item_same_components(stack))
        } else {
            None
        };
        match found {
            Some(i) => {
                let old = self.items.remove(i);
                let merged = old.with_count(old.count.saturating_add(amount));
                stack.shrink(amount);
                self.items.insert(0, merged);
            }
            None => {
                let part = stack.split(amount);
                self.items.insert(0, part);
            }
        }
        amount
    }

    /// `removeOne`: the selected stack (or the first), clearing the selection.
    pub fn remove_one(&mut self) -> Option<ItemStack> {
        if self.items.is_empty() {
            return None;
        }
        let i = if self.out_of_bounds(self.selected) {
            0
        } else {
            usize::try_from(self.selected).unwrap_or(0)
        };
        let stack = self.items.remove(i);
        if let Some(w) = weight_of(&stack, 0) {
            self.weight = self.weight.sub(w.mul(i64::from(stack.count)));
        }
        self.toggle_selected(-1);
        Some(stack)
    }
}

/// A bundle sound of a menu click (`playInsertSound`, …), played for the players around.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sound {
    /// `BUNDLE_INSERT`, volume 0.8, pitch 0.8–1.2.
    Insert,
    /// `BUNDLE_INSERT_FAIL`, volume 1, pitch 1.
    InsertFail,
    /// `BUNDLE_REMOVE_ONE`, volume 0.8, pitch 0.8–1.2.
    RemoveOne,
}

impl Sound {
    /// Sound event name, volume and whether the pitch is random.
    pub fn params(self) -> (&'static str, f32, bool) {
        match self {
            Sound::Insert => ("minecraft:item.bundle.insert", 0.8, true),
            Sound::InsertFail => ("minecraft:item.bundle.insert_fail", 1.0, false),
            Sound::RemoveOne => ("minecraft:item.bundle.remove_one", 0.8, true),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack(name: &str, count: i32) -> ItemStack {
        ItemStack::new(
            vanillite_data::registry::ITEM.id(name).expect("item"),
            count,
        )
    }

    #[test]
    fn a_bundle_holds_one_stack_worth() {
        let mut bundle = stack("minecraft:bundle", 1);
        let mut c = Contents::of(&bundle, None).expect("bundle");
        let mut dirt = stack("minecraft:dirt", 64);
        assert_eq!(c.try_insert(&mut dirt), 64);
        let mut more = stack("minecraft:dirt", 1);
        assert_eq!(c.try_insert(&mut more), 0);
        c.store(&mut bundle);
        assert!(weight(&bundle).is_full());
        let mut c = Contents::of(&bundle, None).expect("bundle");
        let out = c.remove_one().expect("stack");
        assert_eq!(out.count, 64);
        c.store(&mut bundle);
        assert!(bundle.component(ids::BUNDLE_CONTENTS).is_none());
    }

    #[test]
    fn insert_merges_and_moves_to_front() {
        let mut c = Contents::of(&stack("minecraft:bundle", 1), None).expect("bundle");
        c.try_insert(&mut stack("minecraft:dirt", 4));
        c.try_insert(&mut stack("minecraft:stone", 4));
        c.try_insert(&mut stack("minecraft:dirt", 4));
        assert_eq!(c.items.len(), 2);
        assert_eq!(c.items[0].count, 8);
        c.toggle_selected(1);
        let out = c.remove_one().expect("stack");
        assert_eq!(item::item_id(&out).name(), "minecraft:stone");
        assert_eq!(c.selected, -1);
    }

    #[test]
    fn shulker_boxes_do_not_fit() {
        let mut c = Contents::of(&stack("minecraft:bundle", 1), None).expect("bundle");
        assert_eq!(c.try_insert(&mut stack("minecraft:shulker_box", 1)), 0);
    }
}
