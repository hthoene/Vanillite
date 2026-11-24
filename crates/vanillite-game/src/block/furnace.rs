//! Furnaces, blast furnaces and smokers (`AbstractFurnaceBlockEntity`).
//!
//! Slot 0 is the input, 1 the fuel, 2 the result. The block entity ticks every tick
//! ([`server_tick`]): burning fuel counts down, a recipe for the input cooks while the furnace
//! burns, and the `lit` state follows. The experience of cooked items is kept per recipe until
//! a player takes the result or the furnace is removed.

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Vec3};
use vanillite_data::block::BlockState;
use vanillite_data::item::ids as items;
use vanillite_nbt::{Compound, Tag};
use vanillite_protocol::text::Component;
use vanillite_worldgen::behaviour::StateExt;

use crate::item::{self, ItemStack};
use crate::level::{BlockEntity, Level};
use crate::recipe::{CookingKind, Recipes};

/// The data of a furnace block entity.
#[derive(Clone, Debug, PartialEq)]
pub struct Furnace {
    /// Input, fuel, result.
    pub items: [ItemStack; 3],
    /// Which recipes it cooks.
    pub kind: CookingKind,
    /// `litTimeRemaining`.
    pub lit_time_remaining: i32,
    /// `litTotalTime`.
    pub lit_total_time: i32,
    /// `cookingTimer`.
    pub cooking_timer: i32,
    /// `cookingTotalTime`.
    pub cooking_total_time: i32,
    /// `speedMultiplier` of the burning fuel.
    pub speed: f32,
    /// `recipesUsed`: recipe id → items cooked since the experience was last collected.
    pub recipes_used: Vec<(String, i32)>,
    /// `CustomName`.
    pub custom_name: Option<Component>,
}

impl Furnace {
    /// An empty furnace of `kind`.
    pub fn new(kind: CookingKind) -> Furnace {
        Furnace {
            items: std::array::from_fn(|_| ItemStack::EMPTY),
            kind,
            lit_time_remaining: 0,
            lit_total_time: 0,
            cooking_timer: 0,
            cooking_total_time: 0,
            speed: 1.0,
            recipes_used: Vec::new(),
            custom_name: None,
        }
    }

    /// The menu's data slots (`dataAccess`).
    pub fn data(&self, index: usize) -> i32 {
        match index {
            0 => self.lit_time_remaining,
            1 => self.lit_total_time,
            2 => self.cooking_timer,
            3 => self.cooking_total_time,
            _ => 0,
        }
    }

    fn fast(&self) -> bool {
        matches!(self.kind, CookingKind::Blasting | CookingKind::Smoking)
    }

    /// `getTotalCookTime(recipe)`: the recipe's time divided by the fuel's speed.
    fn total_cook_time(&self, cooking_time: i32) -> i32 {
        if self.speed > 0.0 {
            #[allow(
                clippy::as_conversions,
                clippy::cast_possible_truncation,
                clippy::cast_precision_loss
            )]
            let t = (cooking_time as f32 / self.speed).ceil() as i32;
            t
        } else {
            cooking_time
        }
    }

    /// `getTotalCookTime(level)`: of the recipe for the current input, 200 without one.
    fn total_cook_time_for_input(&self, recipes: &Recipes) -> i32 {
        recipes
            .cooking_for(self.kind, &self.items[0])
            .map_or(200, |r| self.total_cook_time(r.cooking_time))
    }

    /// `setItem(slot, stack)`: a new kind of input restarts the cooking.
    pub fn set_item(&mut self, slot: usize, mut stack: ItemStack) {
        let Some(old) = self.items.get(slot) else {
            return;
        };
        let same = !stack.is_empty() && old.same_item_same_components(&stack);
        // `limitSize(getMaxStackSize(stack))`.
        let max = item::max_stack_size(&stack).min(99);
        if stack.count > max {
            stack.count = max;
        }
        if let Some(target) = self.items.get_mut(slot) {
            *target = stack;
        }
        if slot == 0
            && !same
            && let Some(data) = crate::player::play::game_data()
        {
            self.cooking_total_time = self.total_cook_time_for_input(&data.recipes);
            self.cooking_timer = 0;
        }
    }

    /// `canPlaceItem` (hoppers and the fuel slot).
    pub fn can_place_item(&self, slot: usize, stack: &ItemStack, recipes: &Recipes) -> bool {
        match slot {
            2 => false,
            1 => {
                recipes.fuel(stack, self.fast()).is_some()
                    || (item::item_id(stack) == items::BUCKET
                        && item::item_id(&self.items[1]) != items::BUCKET)
            }
            _ => true,
        }
    }

    /// Experience waiting in the furnace: `getRecipesToAwardAndPopExperience` with the level's
    /// random. Clears the counts.
    pub fn take_experience(&mut self, recipes: &Recipes, random: &mut dyn FnMut() -> f32) -> i32 {
        let mut total = 0;
        for (id, count) in std::mem::take(&mut self.recipes_used) {
            let Some(recipe) = recipes.cooking.iter().find(|r| r.id == id) else {
                continue;
            };
            #[allow(clippy::as_conversions, clippy::cast_precision_loss)]
            let value = count as f32 * recipe.experience;
            #[allow(clippy::as_conversions, clippy::cast_possible_truncation)]
            let mut reward = value.floor() as i32;
            let frac = value - value.floor();
            if frac != 0.0 && random() < frac {
                reward += 1;
            }
            total += reward;
        }
        total
    }

    /// Loads the stored fields.
    pub fn load(&mut self, nbt: &Compound) {
        let int = |key: &str| {
            nbt.get(key)
                .and_then(Tag::as_i64)
                .and_then(|v| i32::try_from(v).ok())
        };
        self.custom_name = super::container::load_name(nbt);
        super::container::load_items(nbt, &mut self.items);
        self.cooking_timer = int("cooking_time_spent").unwrap_or(0);
        self.cooking_total_time = int("cooking_total_time").unwrap_or(0);
        self.lit_time_remaining = int("lit_time_remaining").unwrap_or(0);
        self.lit_total_time = int("lit_total_time").unwrap_or(0);
        self.speed = match nbt.get("speed_multiplier") {
            Some(Tag::Float(v)) => *v,
            _ => 1.0,
        };
        self.recipes_used.clear();
        if let Some(Tag::Compound(used)) = nbt.get("RecipesUsed") {
            for (id, count) in used.iter() {
                if let Some(count) = count.as_i64().and_then(|v| i32::try_from(v).ok()) {
                    self.recipes_used.push((id.to_owned(), count));
                }
            }
        }
    }

    /// Saves the stored fields.
    pub fn save(&self, out: &mut Compound) {
        super::container::save_name(&self.custom_name, out);
        out.insert("cooking_time_spent", Tag::Int(self.cooking_timer));
        out.insert("cooking_total_time", Tag::Int(self.cooking_total_time));
        out.insert("lit_time_remaining", Tag::Int(self.lit_time_remaining));
        out.insert("lit_total_time", Tag::Int(self.lit_total_time));
        out.insert("speed_multiplier", Tag::Float(self.speed));
        super::container::save_items(&self.items, out);
        let mut used = Compound::new();
        for (id, count) in &self.recipes_used {
            used.insert(id, Tag::Int(*count));
        }
        out.insert("RecipesUsed", Tag::Compound(used));
    }
}

/// `canBurn`: the result fits into the result slot.
fn can_burn(items: &[ItemStack; 3], result: &ItemStack) -> bool {
    let out = &items[2];
    if out.is_empty() {
        return true;
    }
    if !out.same_item_same_components(result) {
        return false;
    }
    out.count + result.count <= 99.min(item::max_stack_size(result))
}

/// `AbstractFurnaceBlockEntity.serverTick`.
pub fn server_tick(level: &mut Level, pos: BlockPos, state: BlockState) {
    let Some(data) = crate::player::play::game_data() else {
        return;
    };
    let recipes = &data.recipes;
    let Some(BlockEntity::Furnace(furnace)) = level.block_entity_mut(pos) else {
        return;
    };
    let mut changed = false;
    let was_lit = furnace.lit_time_remaining > 0;
    let mut is_lit = false;
    if was_lit {
        furnace.lit_time_remaining -= 1;
        is_lit = furnace.lit_time_remaining > 0;
    }
    let fuel = furnace.items[1].clone();
    let has_ingredient = !furnace.items[0].is_empty();
    let has_fuel = !fuel.is_empty();
    let mut drop_remainder = None;
    if is_lit || (has_fuel && has_ingredient) {
        if has_ingredient {
            if let Some(recipe) = recipes.cooking_for(furnace.kind, &furnace.items[0]) {
                let result = recipe.result.clone();
                if !result.is_empty() && can_burn(&furnace.items, &result) {
                    if !is_lit {
                        let value = recipes.fuel(&fuel, furnace.fast()).unwrap_or_default();
                        furnace.lit_time_remaining = value.burn_time;
                        furnace.lit_total_time = value.burn_time;
                        furnace.speed = value.speed;
                        if furnace.cooking_total_time > 0
                            && furnace.cooking_timer < furnace.cooking_total_time
                        {
                            #[allow(clippy::as_conversions, clippy::cast_precision_loss)]
                            let ratio =
                                furnace.cooking_timer as f32 / furnace.cooking_total_time as f32;
                            furnace.cooking_total_time =
                                furnace.total_cook_time(recipe.cooking_time);
                            #[allow(
                                clippy::as_conversions,
                                clippy::cast_possible_truncation,
                                clippy::cast_precision_loss
                            )]
                            let timer = (ratio * furnace.cooking_total_time as f32).ceil() as i32;
                            furnace.cooking_timer = timer;
                        }
                        if value.burn_time > 0 {
                            // `consumeFuel`.
                            let remainder = crate::recipe::crafting_remainder(fuel.item);
                            let mut new_fuel = fuel.clone();
                            new_fuel.shrink(1);
                            if let Some(r) = remainder {
                                if new_fuel.is_empty() {
                                    new_fuel = ItemStack::new(r, 1);
                                } else {
                                    drop_remainder = Some(ItemStack::new(r, 1));
                                }
                            }
                            furnace.items[1] = new_fuel;
                            is_lit = true;
                            changed = true;
                        }
                    }
                    if is_lit {
                        furnace.cooking_timer += 1;
                        if furnace.cooking_timer >= furnace.cooking_total_time {
                            furnace.cooking_timer = 0;
                            furnace.cooking_total_time =
                                furnace.total_cook_time(recipe.cooking_time);
                            // `burn`.
                            let input_item = item::item_id(&furnace.items[0]);
                            if furnace.items[2].is_empty() {
                                furnace.items[2] = result.clone();
                            } else {
                                furnace.items[2].grow(result.count);
                            }
                            if input_item == items::WET_SPONGE
                                && item::item_id(&furnace.items[1]) == items::BUCKET
                            {
                                furnace.items[1] = ItemStack::new(items::WATER_BUCKET.0, 1);
                            }
                            furnace.items[0].shrink(1);
                            // `setRecipeUsed`.
                            match furnace
                                .recipes_used
                                .iter_mut()
                                .find(|(id, _)| *id == recipe.id)
                            {
                                Some((_, n)) => *n += 1,
                                None => furnace.recipes_used.push((recipe.id.clone(), 1)),
                            }
                            changed = true;
                        }
                    } else {
                        furnace.cooking_timer = 0;
                    }
                } else {
                    furnace.cooking_timer = 0;
                }
            }
        } else {
            furnace.cooking_timer = 0;
        }
    } else if furnace.cooking_timer > 0 {
        furnace.cooking_timer =
            (furnace.cooking_timer - 2).clamp(0, furnace.cooking_total_time.max(0));
    }
    if let Some(remainder) = drop_remainder {
        super::container::drop_item_stack(level, pos, remainder);
    }
    let mut state = state;
    if was_lit != is_lit {
        changed = true;
        state = state.set_flag("lit", is_lit);
        level.set_block_and_update(pos, state);
    }
    if changed {
        super::container::set_changed(level, pos);
    }
}

/// The experience the furnace at `pos` gives when its result is taken or it is removed.
pub fn pop_experience(level: &mut Level, pos: BlockPos) -> i32 {
    let Some(data) = crate::player::play::game_data() else {
        return 0;
    };
    let mut furnace = match level.block_entity_mut(pos) {
        Some(BlockEntity::Furnace(f)) => std::mem::take(&mut f.recipes_used),
        _ => return 0,
    };
    let mut f = Furnace::new(CookingKind::Smelting);
    f.recipes_used = std::mem::take(&mut furnace);
    let mut random = || level.random().next_float();
    f.take_experience(&data.recipes, &mut random)
}

/// The centre of a block.
pub fn center(pos: BlockPos) -> Vec3 {
    Vec3::new(
        f64::from(pos.x) + 0.5,
        f64::from(pos.y) + 0.5,
        f64::from(pos.z) + 0.5,
    )
}
