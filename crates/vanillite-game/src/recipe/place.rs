//! Recipe book placement (`ServerPlaceRecipe`, `PlaceRecipeHelper`, `StackedContents`).

use vanillite_protocol::components::ids as components;

use super::Ingredient;
use crate::inventory::{Drop, PlayerInventory};
use crate::item::{self, ItemStack};
use crate::menu::MenuSlots;

/// `Inventory.isUsableForCrafting`.
pub fn usable_for_crafting(stack: &ItemStack) -> bool {
    let damaged = item::is_damageable(stack) && item::damage(stack) > 0;
    let enchanted = stack
        .component(components::ENCHANTMENTS)
        .is_some_and(|v| v.first().is_some_and(|&n| n != 0));
    !damaged && !enchanted && stack.component(components::CUSTOM_NAME).is_none()
}

/// The default `max_stack_size` of an item.
fn item_max_stack(item: u16) -> i32 {
    i32::from(vanillite_data::item::ItemId(item).info().max_stack_size)
}

/// `StackedItemContents`: item counts (by item; Vanilla iterates them in hash order, this
/// in insertion order).
#[derive(Debug, Default)]
pub struct Stacked {
    amounts: Vec<(u16, i32)>,
}

impl Stacked {
    fn put(&mut self, item: u16, count: i32) {
        match self.amounts.iter_mut().find(|(i, _)| *i == item) {
            Some((_, c)) => *c = c.saturating_add(count),
            None => self.amounts.push((item, count)),
        }
    }

    fn amount(&self, item: u16) -> i32 {
        self.amounts
            .iter()
            .find(|(i, _)| *i == item)
            .map_or(0, |(_, c)| *c)
    }

    /// `accountSimpleStack`.
    pub fn account_simple(&mut self, stack: &ItemStack) {
        if usable_for_crafting(stack) {
            self.account(stack);
        }
    }

    /// `accountStack`.
    pub fn account(&mut self, stack: &ItemStack) {
        if !stack.is_empty() {
            let count = item::max_stack_size(stack).min(stack.count);
            self.put(stack.item, count);
        }
    }

    /// `getResultUpperBound`.
    fn upper_bound(&self, ingredients: &[Ingredient]) -> i32 {
        let mut min = i32::MAX;
        'ingredients: for ingredient in ingredients {
            let mut max = 0;
            for &(item, count) in &self.amounts {
                if count > max {
                    if ingredient.items().binary_search(&item).is_ok() {
                        max = count;
                    }
                    if max >= min {
                        continue 'ingredients;
                    }
                }
            }
            min = max;
            if max == 0 {
                break;
            }
        }
        min
    }

    /// `tryPick(ingredients, amount, output)`.
    pub fn try_pick(
        &mut self,
        ingredients: &[Ingredient],
        amount: i32,
        output: Option<&mut Vec<u16>>,
    ) -> bool {
        Picker::new(self, ingredients).try_pick(self, amount, output)
    }

    /// `tryPickAll(ingredients, maxSize, output)`.
    pub fn try_pick_all(
        &mut self,
        ingredients: &[Ingredient],
        max_size: i32,
        output: Option<&mut Vec<u16>>,
    ) -> i32 {
        let picker = Picker::new(self, ingredients);
        let mut min = 0i32;
        let mut max = max_size
            .min(self.upper_bound(ingredients))
            .saturating_add(1);
        let mut picker = picker;
        loop {
            let mid = min.saturating_add(max) / 2;
            if picker.try_pick(self, mid, None) {
                if max.saturating_sub(min) <= 1 {
                    if mid > 0 {
                        picker.try_pick(self, mid, output);
                    }
                    return mid;
                }
                min = mid;
            } else {
                max = mid;
            }
        }
    }
}

/// `StackedContents.RecipePicker`: bipartite matching of ingredients to items.
struct Picker<'i> {
    ingredients: &'i [Ingredient],
    items: Vec<u16>,
    visited_ingredient: Vec<bool>,
    visited_item: Vec<bool>,
    satisfied: Vec<bool>,
    connection: Vec<bool>,
    residual: Vec<bool>,
    path: Vec<usize>,
}

fn flag(v: &[bool], i: usize) -> bool {
    v.get(i).copied().unwrap_or(false)
}

fn set(v: &mut [bool], i: usize, value: bool) {
    if let Some(b) = v.get_mut(i) {
        *b = value;
    }
}

impl<'i> Picker<'i> {
    fn new(contents: &Stacked, ingredients: &'i [Ingredient]) -> Picker<'i> {
        // `getUniqueAvailableIngredientItems`.
        let items: Vec<u16> = contents
            .amounts
            .iter()
            .filter(|&&(item, count)| {
                count > 0
                    && ingredients
                        .iter()
                        .any(|i| i.items().binary_search(&item).is_ok())
            })
            .map(|&(item, _)| item)
            .collect();
        let n = ingredients.len();
        let m = items.len();
        let mut connection = vec![false; n.saturating_mul(m)];
        for (ingredient, info) in ingredients.iter().enumerate() {
            for (item, &id) in items.iter().enumerate() {
                if info.items().binary_search(&id).is_ok() {
                    set(&mut connection, item * n + ingredient, true);
                }
            }
        }
        Picker {
            ingredients,
            items,
            visited_ingredient: vec![false; n],
            visited_item: vec![false; m],
            satisfied: vec![false; n],
            connection,
            residual: vec![false; n.saturating_mul(m)],
            path: Vec::new(),
        }
    }

    fn index(&self, item: usize, ingredient: usize) -> usize {
        item * self.ingredients.len() + ingredient
    }

    fn is_assigned(&self, item: usize, ingredient: usize) -> bool {
        flag(&self.residual, self.index(item, ingredient))
    }

    fn has_connection(&self, item: usize, ingredient: usize) -> bool {
        flag(&self.connection, self.index(item, ingredient))
    }

    fn clear_visited(&mut self) {
        self.visited_ingredient.fill(false);
        self.visited_item.fill(false);
    }

    fn try_pick(&mut self, contents: &mut Stacked, capacity: i32, output: Option<&mut Vec<u16>>) -> bool {
        if capacity <= 0 {
            return true;
        }
        let n = self.ingredients.len();
        let m = self.items.len();
        let mut satisfied_count = 0usize;
        loop {
            let Some(path) = self.try_assigning_new_item(contents, capacity) else {
                let valid = satisfied_count == n;
                let mut output = output.filter(|_| valid);
                self.clear_visited();
                self.satisfied.fill(false);
                for ingredient in 0..n {
                    for item in 0..m {
                        if self.is_assigned(item, ingredient) {
                            let i = self.index(item, ingredient);
                            set(&mut self.residual, i, false);
                            let id = self.items.get(item).copied().unwrap_or(0);
                            contents.put(id, capacity);
                            if let Some(out) = output.as_deref_mut() {
                                out.push(id);
                            }
                            break;
                        }
                    }
                }
                return valid;
            };
            let assigned = path.first().copied().unwrap_or(0);
            let id = self.items.get(assigned).copied().unwrap_or(0);
            contents.put(id, -capacity);
            if let Some(&last) = path.last() {
                set(&mut self.satisfied, last, true);
            }
            satisfied_count += 1;
            for i in 0..path.len().saturating_sub(1) {
                let (a, b) = (path.get(i).copied(), path.get(i + 1).copied());
                let (Some(a), Some(b)) = (a, b) else {
                    continue;
                };
                if i & 1 == 0 {
                    // item a → ingredient b
                    let idx = self.index(a, b);
                    set(&mut self.residual, idx, true);
                } else {
                    // ingredient a ← item b
                    let idx = self.index(b, a);
                    set(&mut self.residual, idx, false);
                }
            }
        }
    }

    fn try_assigning_new_item(&mut self, contents: &Stacked, capacity: i32) -> Option<Vec<usize>> {
        self.clear_visited();
        for item in 0..self.items.len() {
            let id = self.items.get(item).copied().unwrap_or(0);
            if contents.amount(id) >= capacity
                && let Some(path) = self.find_path(item)
            {
                return Some(path);
            }
        }
        None
    }

    /// `findNewItemAssignmentPath`.
    fn find_path(&mut self, start: usize) -> Option<Vec<usize>> {
        let n = self.ingredients.len();
        let m = self.items.len();
        self.path.clear();
        set(&mut self.visited_item, start, true);
        self.path.push(start);
        while let Some(&last) = self.path.last() {
            let len = self.path.len();
            if (len - 1) & 1 == 0 {
                for ingredient in 0..n {
                    if !flag(&self.visited_ingredient, ingredient)
                        && self.has_connection(last, ingredient)
                        && !self.is_assigned(last, ingredient)
                    {
                        set(&mut self.visited_ingredient, ingredient, true);
                        self.path.push(ingredient);
                        break;
                    }
                }
            } else {
                if !flag(&self.satisfied, last) {
                    return Some(self.path.clone());
                }
                for item in 0..m {
                    if !flag(&self.visited_item, item) && self.is_assigned(item, last) {
                        set(&mut self.visited_item, item, true);
                        self.path.push(item);
                        break;
                    }
                }
            }
            if self.path.len() == len {
                self.path.pop();
            }
        }
        None
    }
}

/// `PlacementInfo` of a recipe and its shape.
#[derive(Clone, Debug)]
pub struct Placement {
    /// `ingredients`.
    pub ingredients: Vec<Ingredient>,
    /// `slotsToIngredientIndex` (`None` = -1).
    pub slots: Vec<Option<usize>>,
    /// Width and height of a shaped recipe.
    pub shaped: Option<(usize, usize)>,
}

/// A recipe book menu's grid (`RecipeBookMenu.handlePlacement`).
#[derive(Clone, Debug)]
pub struct Grid {
    /// Grid width and height.
    pub width: usize,
    pub height: usize,
    /// The input slots (menu slot indices) in grid order.
    pub input: Vec<usize>,
    /// The slots emptied into the inventory before placing.
    pub clear: Vec<usize>,
    /// `clearCraftingContent`: the slots emptied after that.
    pub clear_content: Vec<usize>,
    /// The slots of the menu's own `fillCraftSlotsStackedContents`, and whether they count
    /// fully (`accountStack`, furnaces) or only usable stacks (`accountSimpleStack`, grids).
    pub account: Vec<usize>,
    pub account_full: bool,
    /// `CraftingMenu.beginPlacingRecipe`: grid slots change in place and the result is
    /// computed once at the end (`finishPlacingRecipe`); otherwise every `Slot.set` goes
    /// through the container.
    pub suppress: bool,
    /// `clearCraftingContent` empties the containers directly (crafting grids) instead of
    /// through `Slot.set` (furnaces).
    pub content_direct: bool,
}

/// `PlaceRecipeHelper.placeRecipe`: calls `out(ingredient index, grid index)`.
fn walk(grid: &Grid, recipe: &Placement, mut out: impl FnMut(Option<usize>, usize)) {
    let (gw, gh) = (grid.width, grid.height);
    let (rw, rh) = recipe.shaped.unwrap_or((gw, gh));
    #[allow(clippy::as_conversions, clippy::cast_precision_loss)]
    let half = |v: usize| v as f32 / 2.0;
    #[allow(clippy::as_conversions, clippy::cast_possible_truncation)]
    let floor = |v: f32| v.floor() as i64;
    let mut entries = recipe.slots.iter();
    let mut grid_index = 0usize;
    let mut y = 0usize;
    while y < gh {
        #[allow(clippy::as_conversions, clippy::cast_precision_loss)]
        let center_y = (rh as f32) < half(gh);
        #[allow(clippy::as_conversions, clippy::cast_precision_loss)]
        let start_y = floor(half(gh) - rh as f32 / 2.0);
        #[allow(clippy::as_conversions, clippy::cast_possible_wrap)]
        if center_y && start_y > y as i64 {
            grid_index += gw;
            y += 1;
        }
        for x in 0..gw {
            if entries.len() == 0 {
                return;
            }
            #[allow(clippy::as_conversions, clippy::cast_precision_loss)]
            let center_x = (rw as f32) < half(gw);
            #[allow(clippy::as_conversions, clippy::cast_precision_loss)]
            let start_x = floor(half(gw) - rw as f32 / 2.0);
            #[allow(clippy::as_conversions, clippy::cast_possible_wrap)]
            let (xi, rwi) = (x as i64, rw as i64);
            let mut total = rwi;
            let mut add = xi < rwi;
            if center_x {
                total = start_x + rwi;
                add = start_x <= xi && xi < start_x + rwi;
            }
            if add {
                if let Some(&entry) = entries.next() {
                    out(entry, grid_index);
                }
            } else if total == xi {
                grid_index += gw - x;
                break;
            }
            grid_index += 1;
        }
        y += 1;
    }
}

/// `ServerPlaceRecipe.placeRecipe`: whether the client gets a ghost recipe
/// (`PostPlaceAction.PLACE_GHOST_RECIPE`). `matches_placed` is `recipeMatches` (the grid
/// already holds this recipe).
#[allow(clippy::too_many_arguments)]
pub fn place<S: MenuSlots + ?Sized>(
    s: &mut S,
    grid: &Grid,
    recipe: &Placement,
    matches_placed: bool,
    use_max_items: bool,
    creative: bool,
    drops: &mut Vec<Drop>,
) -> bool {
    let ghost = place_inner(s, grid, recipe, matches_placed, use_max_items, creative, drops);
    if grid.suppress
        && let Some(&first) = grid.input.first()
    {
        s.slot_contents_changed(first);
    }
    ghost
}

#[allow(clippy::too_many_arguments)]
fn place_inner<S: MenuSlots + ?Sized>(
    s: &mut S,
    grid: &Grid,
    recipe: &Placement,
    matches_placed: bool,
    use_max_items: bool,
    creative: bool,
    drops: &mut Vec<Drop>,
) -> bool {
    if !creative && !test_clear_grid(s, grid) {
        return false;
    }
    let mut stacked = Stacked::default();
    for stack in &s.inventory().items {
        stacked.account_simple(stack);
    }
    for &slot in &grid.account {
        let stack = s.item(slot);
        if grid.account_full {
            stacked.account(stack);
        } else {
            stacked.account_simple(stack);
        }
    }
    if !stacked.try_pick(&recipe.ingredients, 1, None) {
        clear_grid(s, grid, creative, drops);
        return true;
    }
    place_recipe(s, grid, recipe, &mut stacked, matches_placed, use_max_items, creative, drops);
    false
}

#[allow(clippy::too_many_arguments)]
fn place_recipe<S: MenuSlots + ?Sized>(
    s: &mut S,
    grid: &Grid,
    recipe: &Placement,
    stacked: &mut Stacked,
    matches_placed: bool,
    use_max_items: bool,
    creative: bool,
    drops: &mut Vec<Drop>,
) {
    let biggest = stacked.try_pick_all(&recipe.ingredients, i32::MAX, None);
    if matches_placed {
        for &slot in &grid.input {
            let stack = s.item(slot);
            if !stack.is_empty()
                && biggest.min(item::max_stack_size(stack)) < stack.count.saturating_add(1)
            {
                return;
            }
        }
    }
    let amount = if use_max_items {
        biggest
    } else if matches_placed {
        let mut smallest = i32::MAX;
        for &slot in &grid.input {
            let stack = s.item(slot);
            if !stack.is_empty() && smallest > stack.count {
                smallest = stack.count;
            }
        }
        if smallest != i32::MAX {
            smallest += 1;
        }
        smallest
    } else {
        1
    };
    let mut used = Vec::new();
    if !stacked.try_pick(&recipe.ingredients, amount, Some(&mut used)) {
        return;
    }
    let adjusted = used
        .iter()
        .fold(amount, |v, &item| v.min(item_max_stack(item)));
    if adjusted != amount {
        used.clear();
        if !stacked.try_pick(&recipe.ingredients, adjusted, Some(&mut used)) {
            return;
        }
    }
    clear_grid(s, grid, creative, drops);
    let mut moves = Vec::new();
    walk(grid, recipe, |entry, grid_index| {
        if let (Some(ingredient), Some(&slot)) = (entry, grid.input.get(grid_index)) {
            moves.push((slot, ingredient));
        }
    });
    for (slot, ingredient) in moves {
        let Some(&item) = used.get(ingredient) else {
            continue;
        };
        let mut remaining = adjusted;
        while remaining > 0 {
            match move_item_to_grid(s, grid, slot, item, remaining) {
                Some(rest) => remaining = rest,
                None => break,
            }
        }
    }
}

/// `moveItemToGrid`: the count still to move, `None` if no inventory slot has the item.
fn move_item_to_grid<S: MenuSlots + ?Sized>(
    s: &mut S,
    grid: &Grid,
    target: usize,
    item: u16,
    count: i32,
) -> Option<i32> {
    let existing = s.item(target).clone();
    let inv: &mut PlayerInventory = s.inventory();
    // `findSlotMatchingCraftingIngredient`.
    let index = inv.items.iter().position(|stack| {
        !stack.is_empty()
            && stack.item == item
            && usable_for_crafting(stack)
            && (existing.is_empty() || existing.same_item_same_components(stack))
    })?;
    let taken = match inv.items.get_mut(index) {
        Some(stack) if count < stack.count => stack.split(count),
        Some(stack) => std::mem::take(stack),
        None => return None,
    };
    let taken_count = taken.count;
    if existing.is_empty() {
        set_grid(s, grid, target, taken);
    } else if let Some(stack) = s.item_mut(target) {
        stack.grow(taken_count);
    }
    Some(count - taken_count)
}

fn set_grid<S: MenuSlots + ?Sized>(s: &mut S, grid: &Grid, slot: usize, stack: ItemStack) {
    set_slot(s, grid.suppress, slot, stack);
}

fn set_slot<S: MenuSlots + ?Sized>(s: &mut S, direct: bool, slot: usize, stack: ItemStack) {
    if direct {
        if let Some(target) = s.item_mut(slot) {
            *target = stack;
        }
    } else {
        crate::menu::set_by_player(s, slot, stack);
    }
}

/// `clearGrid`: the slots to clear go back into the inventory, then `clearCraftingContent`.
fn clear_grid<S: MenuSlots + ?Sized>(s: &mut S, grid: &Grid, creative: bool, drops: &mut Vec<Drop>) {
    for &slot in &grid.clear {
        let stack = s.item(slot).clone();
        s.inventory().place_back(stack, creative, drops);
        set_grid(s, grid, slot, ItemStack::EMPTY);
    }
    for &slot in &grid.clear_content {
        set_slot(s, grid.content_direct, slot, ItemStack::EMPTY);
    }
}

/// `testClearGrid`: whether the grid fits back into the inventory.
fn test_clear_grid<S: MenuSlots + ?Sized>(s: &mut S, grid: &Grid) -> bool {
    let mut free: Vec<ItemStack> = Vec::new();
    let stacks: Vec<ItemStack> = grid.input.iter().map(|&i| s.item(i).clone()).collect();
    let inv = s.inventory();
    let free_slots = inv.items.iter().filter(|s| s.is_empty()).count();
    for mut stack in stacks {
        if stack.is_empty() {
            continue;
        }
        let slot = inv.slot_with_remaining_space(&stack);
        if slot.is_none() && free.len() <= free_slots {
            for other in &mut free {
                let max = item::max_stack_size(other);
                if other.item == stack.item
                    && other.count != max
                    && other.count.saturating_add(stack.count) <= max
                {
                    other.grow(stack.count);
                    stack.count = 0;
                    break;
                }
            }
            if stack.count > 0 {
                if free.len() >= free_slots {
                    return false;
                }
                free.push(stack);
            }
        } else if slot.is_none() {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use vanillite_data::item::ids as items;

    use super::*;
    use crate::recipe::book::RecipeRef;

    fn crafting_ref(id: &str) -> RecipeRef {
        let data = crate::menu::tests::install_data();
        let d = data.recipes.book.displays(id);
        data.recipes.book.entry(d[0]).unwrap().recipe
    }

    fn place_table(inv: &mut PlayerInventory, use_max: bool) -> bool {
        let data = crate::menu::tests::install_data();
        let placement = data
            .recipes
            .placement(crafting_ref("minecraft:crafting_table"))
            .unwrap();
        let input: Vec<usize> = (1..5).collect();
        let grid = Grid {
            width: 2,
            height: 2,
            clear: input.clone(),
            account: input.clone(),
            clear_content: (0..5).collect(),
            input,
            account_full: false,
            suppress: false,
            content_direct: true,
        };
        place(inv, &grid, &placement, false, use_max, false, &mut Vec::new())
    }

    #[test]
    fn every_display_encodes() {
        let data = crate::menu::tests::install_data();
        let book = &data.recipes.book;
        assert!(book.contains("minecraft:crafting_table"));
        assert!(!book.contains("minecraft:repair_item"));
        let mut id = 0;
        while let Some(e) = book.wire_entry(id) {
            let mut out = Vec::new();
            vanillite_protocol::codec::Encode::encode(&e, &mut out);
            assert!(!out.is_empty());
            id += 1;
        }
        assert!(id > 1000, "{id} displays");
    }

    #[test]
    fn places_and_ghosts_the_crafting_table() {
        let mut inv = PlayerInventory::default();
        inv.items[9] = ItemStack::new(items::OAK_PLANKS.0, 9);
        assert!(!place_table(&mut inv, false));
        assert!(inv.craft.iter().all(|s| s.item == items::OAK_PLANKS.0 && s.count == 1));
        assert_eq!(inv.items[9].count, 5);
        assert_eq!(inv.craft_result.item, items::CRAFTING_TABLE.0);
        // The same recipe again with the maximum: the grid goes back first, then 2 each.
        assert!(!place_table(&mut inv, true));
        assert!(inv.craft.iter().all(|s| s.count == 2));
        assert_eq!(inv.items[9].count, 1);
        // Too few planks: a ghost recipe, the grid is emptied into the inventory.
        let mut few = PlayerInventory::default();
        few.items[0] = ItemStack::new(items::OAK_PLANKS.0, 3);
        few.craft[0] = ItemStack::new(items::DIRT.0, 1);
        assert!(place_table(&mut few, false));
        assert!(few.craft.iter().all(ItemStack::is_empty));
        assert_eq!(few.items[1].item, items::DIRT.0);
    }
}
