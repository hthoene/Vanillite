//! The recipe book: the displays of all recipes (`RecipeManager.unpackRecipeInfo`) and the
//! recipes a player knows (`ServerRecipeBook`).

use std::collections::{HashMap, HashSet};

use serde_json::Value;
use vanillite_data::item::ids as items;
use vanillite_data::registry::RECIPE_BOOK_CATEGORY;
use vanillite_nbt::{Compound, Tag};
use vanillite_protocol::components::ids as components;
use vanillite_protocol::packets::play::clientbound::{
    RecipeBookAdd, RecipeBookRemove, RecipeBookSettings,
};
use vanillite_protocol::recipe_display::{RecipeDisplay, RecipeDisplayEntry, SlotDisplay};

use super::{CookingKind, Crafting, Ingredient, Recipes, Smithing};
use crate::item::ItemStack;

/// `Recipe.group()`, `showNotification()` and the book category name of a recipe file.
#[derive(Clone, Debug, Default)]
pub struct RecipeInfo {
    /// `group` (empty = none).
    pub group: Option<Box<str>>,
    /// `category` (`CraftingBookCategory` / `CookingBookCategory` name).
    pub category: Option<Box<str>>,
    /// `show_notification` (default true).
    pub notify: bool,
}

impl RecipeInfo {
    /// Reads the common fields of a recipe file.
    pub fn of(json: &Value) -> RecipeInfo {
        let text = |key: &str| {
            json.get(key)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(Box::from)
        };
        RecipeInfo {
            group: text("group"),
            category: text("category"),
            notify: json
                .get("show_notification")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        }
    }
}

/// Which recipe a display belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecipeRef {
    /// Index into [`Recipes::crafting`].
    Crafting(usize),
    /// Index into [`Recipes::cooking`].
    Cooking(usize),
    /// Index into [`Recipes::stonecutting`].
    Stonecutting(usize),
    /// Index into [`Recipes::smithing`].
    Smithing(usize),
}

/// One recipe display (`RecipeManager.ServerDisplayInfo`).
#[derive(Clone, Debug)]
pub struct BookEntry {
    /// The recipe.
    pub recipe: RecipeRef,
    /// The recipe id.
    pub id: Box<str>,
    /// The display without registry-dependent parts resolved.
    pub display: RecipeDisplay,
    /// Smithing trim pattern of the result display (resolved when sent).
    trim_pattern: Option<Box<str>>,
    /// Group id.
    pub group: Option<i32>,
    /// `minecraft:recipe_book_category` id.
    pub category: i32,
    /// `PlacementInfo.ingredients` (`None` for impossible placements).
    pub requirements: Option<Vec<Ingredient>>,
}

/// The displays of all recipes, by `RecipeDisplayId`.
#[derive(Debug, Default)]
pub struct Book {
    entries: Vec<BookEntry>,
    /// Recipe id → its display ids.
    by_recipe: HashMap<Box<str>, Vec<i32>>,
    /// Recipe id → `showNotification`.
    notify: HashMap<Box<str>, bool>,
}

fn category(name: &str) -> i32 {
    RECIPE_BOOK_CATEGORY
        .id(name)
        .map_or(0, |id| i32::from(id))
}

fn tag(i: &Ingredient) -> SlotDisplay {
    SlotDisplay::Tag(i.items().into())
}

fn stack(s: &ItemStack) -> SlotDisplay {
    SlotDisplay::ItemStack(s.clone())
}

impl Book {
    /// `RecipeManager.unpackRecipeInfo` over the loaded recipes.
    pub fn build(recipes: &Recipes, info: &HashMap<String, RecipeInfo>) -> Book {
        let mut book = Book::default();
        let mut groups: HashMap<Box<str>, i32> = HashMap::new();
        let table = SlotDisplay::Item(items::CRAFTING_TABLE.0);
        for (i, r) in recipes.crafting.iter().enumerate() {
            let meta = info.get(&r.id).cloned().unwrap_or_default();
            let cat = category(match meta.category.as_deref() {
                Some("building") => "minecraft:crafting_building_blocks",
                Some("equipment") => "minecraft:crafting_equipment",
                Some("redstone") => "minecraft:crafting_redstone",
                _ => "minecraft:crafting_misc",
            });
            let (displays, requirements): (Vec<RecipeDisplay>, Vec<Ingredient>) = match &r.kind {
                Crafting::Shaped {
                    width,
                    height,
                    cells,
                    ingredients,
                    result,
                    ..
                } => (
                    vec![RecipeDisplay::Shaped {
                        width: i32::try_from(*width).unwrap_or(0),
                        height: i32::try_from(*height).unwrap_or(0),
                        ingredients: cells
                            .iter()
                            .map(|c| c.and_then(|c| ingredients.get(c)).map_or(SlotDisplay::Empty, tag))
                            .collect(),
                        result: stack(result),
                        station: table.clone(),
                    }],
                    cells
                        .iter()
                        .filter_map(|c| c.and_then(|c| ingredients.get(c)).cloned())
                        .collect(),
                ),
                Crafting::Shapeless {
                    ingredients,
                    result,
                } => (
                    vec![RecipeDisplay::Shapeless {
                        ingredients: ingredients.iter().map(tag).collect(),
                        result: stack(result),
                        station: table.clone(),
                    }],
                    ingredients.clone(),
                ),
                Crafting::Transmute {
                    input,
                    material,
                    min,
                    max,
                    add_material_count,
                    result,
                } => {
                    let mut displays = Vec::new();
                    let mut slots = vec![tag(input)];
                    for count in *min..=*max {
                        slots.push(tag(material));
                        let size = if *add_material_count {
                            count.saturating_add(result.count)
                        } else {
                            result.count
                        };
                        let result = match result.item {
                            Some(item) => stack(&ItemStack::new(item, size)),
                            None => SlotDisplay::Composite(
                                input
                                    .items()
                                    .iter()
                                    .map(|&item| stack(&ItemStack::new(item, size)))
                                    .collect(),
                            ),
                        };
                        displays.push(RecipeDisplay::Shapeless {
                            ingredients: slots.clone(),
                            result,
                            station: table.clone(),
                        });
                    }
                    let mut req = vec![input.clone()];
                    for _ in 0..*max {
                        req.push(material.clone());
                    }
                    (displays, req)
                }
                Crafting::Imbue {
                    source,
                    material,
                    result,
                } => {
                    let m = tag(material);
                    let s = SlotDisplay::WithAnyPotion(Box::new(tag(source)));
                    let mut cells = vec![m; 9];
                    if let Some(c) = cells.get_mut(4) {
                        *c = s;
                    }
                    let mut req = vec![material.clone(); 9];
                    if let Some(c) = req.get_mut(4) {
                        *c = source.clone();
                    }
                    (
                        vec![RecipeDisplay::Shaped {
                            width: 3,
                            height: 3,
                            ingredients: cells,
                            result: SlotDisplay::WithAnyPotion(Box::new(stack(result))),
                            station: table.clone(),
                        }],
                        req,
                    )
                }
                Crafting::Dye { target, dye, .. } => {
                    let dyes = SlotDisplay::OnlyWithComponent(Box::new(tag(dye)), components::DYE);
                    (
                        vec![RecipeDisplay::Shapeless {
                            ingredients: vec![tag(target), dyes.clone()],
                            result: SlotDisplay::Dyed(Box::new(dyes), Box::new(tag(target))),
                            station: table.clone(),
                        }],
                        vec![target.clone(), dye.clone()],
                    )
                }
                // `CustomRecipe`s are special: no displays, never added to a book.
                _ => continue,
            };
            book.add(
                &r.id,
                RecipeRef::Crafting(i),
                displays,
                &meta,
                cat,
                requirements,
                &mut groups,
            );
        }
        for (i, r) in recipes.cooking.iter().enumerate() {
            let meta = info.get(&r.id).cloned().unwrap_or_default();
            let c = meta.category.as_deref();
            let (cat, icon) = match r.kind {
                CookingKind::Smelting => (
                    match c {
                        Some("blocks") => "minecraft:furnace_blocks",
                        Some("food") => "minecraft:furnace_food",
                        _ => "minecraft:furnace_misc",
                    },
                    items::FURNACE,
                ),
                CookingKind::Blasting => (
                    match c {
                        Some("blocks") => "minecraft:blast_furnace_blocks",
                        _ => "minecraft:blast_furnace_misc",
                    },
                    items::BLAST_FURNACE,
                ),
                CookingKind::Smoking => ("minecraft:smoker_food", items::SMOKER),
                CookingKind::Campfire => ("minecraft:campfire", items::CAMPFIRE),
            };
            let display = RecipeDisplay::Furnace {
                ingredient: tag(&r.ingredient),
                fuel: SlotDisplay::AnyFuel,
                result: stack(&r.result),
                station: SlotDisplay::Item(icon.0),
                duration: r.cooking_time,
                experience: r.experience,
            };
            book.add(
                &r.id,
                RecipeRef::Cooking(i),
                vec![display],
                &meta,
                category(cat),
                vec![r.ingredient.clone()],
                &mut groups,
            );
        }
        for (i, r) in recipes.stonecutting.iter().enumerate() {
            let meta = info.get(&r.id).cloned().unwrap_or_default();
            let display = RecipeDisplay::Stonecutter {
                input: tag(&r.ingredient),
                result: stack(&r.result),
                station: SlotDisplay::Item(items::STONECUTTER.0),
            };
            book.add(
                &r.id,
                RecipeRef::Stonecutting(i),
                vec![display],
                &meta,
                category("minecraft:stonecutter"),
                vec![r.ingredient.clone()],
                &mut groups,
            );
        }
        let smithing = category("minecraft:smithing");
        for (i, (id, r)) in recipes.smithing.iter().enumerate() {
            let meta = info.get(id).cloned().unwrap_or_default();
            let station = SlotDisplay::Item(items::SMITHING_TABLE.0);
            let opt = |i: &Option<Ingredient>| i.as_ref().map_or(SlotDisplay::Empty, tag);
            let (display, pattern, req) = match r {
                Smithing::Transform {
                    template,
                    base,
                    addition,
                    result,
                } => (
                    RecipeDisplay::Smithing {
                        template: opt(template),
                        base: opt(base),
                        addition: opt(addition),
                        result: stack(result),
                        station,
                    },
                    None,
                    [template, base, addition]
                        .into_iter()
                        .filter_map(Clone::clone)
                        .collect(),
                ),
                Smithing::Trim {
                    template,
                    base,
                    addition,
                    pattern,
                } => (
                    RecipeDisplay::Smithing {
                        template: tag(template),
                        base: tag(base),
                        addition: tag(addition),
                        result: SlotDisplay::Empty,
                        station,
                    },
                    Some(Box::from(pattern.as_str())),
                    vec![template.clone(), base.clone(), addition.clone()],
                ),
            };
            let first = book.entries.len();
            book.add(
                id,
                RecipeRef::Smithing(i),
                vec![display],
                &meta,
                smithing,
                req,
                &mut groups,
            );
            if let Some(e) = book.entries.get_mut(first) {
                e.trim_pattern = pattern;
            }
        }
        book
    }

    #[allow(clippy::too_many_arguments)]
    fn add(
        &mut self,
        id: &str,
        recipe: RecipeRef,
        displays: Vec<RecipeDisplay>,
        meta: &RecipeInfo,
        category: i32,
        requirements: Vec<Ingredient>,
        groups: &mut HashMap<Box<str>, i32>,
    ) {
        let group = meta.group.as_ref().map(|g| {
            let next = i32::try_from(groups.len()).unwrap_or(i32::MAX);
            *groups.entry(g.clone()).or_insert(next)
        });
        let ids = self.by_recipe.entry(Box::from(id)).or_default();
        self.notify.insert(Box::from(id), meta.notify);
        for display in displays {
            let display_id = i32::try_from(self.entries.len()).unwrap_or(i32::MAX);
            ids.push(display_id);
            self.entries.push(BookEntry {
                recipe,
                id: Box::from(id),
                display,
                trim_pattern: None,
                group,
                category,
                requirements: Some(requirements.clone()),
            });
        }
    }

    /// Whether `id` is a recipe with displays (the ids a book may hold).
    pub fn contains(&self, id: &str) -> bool {
        self.by_recipe.contains_key(id)
    }

    /// The display ids of recipe `id`.
    pub fn displays(&self, id: &str) -> &[i32] {
        self.by_recipe.get(id).map_or(&[], Vec::as_slice)
    }

    /// The entry of display id `id`.
    pub fn entry(&self, id: i32) -> Option<&BookEntry> {
        self.entries.get(usize::try_from(id).ok()?)
    }

    /// The recipe id of a display id.
    pub fn recipe_of(&self, display: i32) -> Option<&str> {
        self.entry(display).map(|e| &*e.id)
    }

    /// The display as sent: registry-dependent parts resolved.
    pub fn display(&self, id: i32) -> Option<RecipeDisplay> {
        let e = self.entry(id)?;
        let mut display = e.display.clone();
        if let (Some(pattern), RecipeDisplay::Smithing { base, addition, result, .. }) =
            (&e.trim_pattern, &mut display)
        {
            let id = crate::player::play::game_data()
                .and_then(|d| d.enchanting.trim_pattern(pattern))
                .unwrap_or(0);
            *result = SlotDisplay::SmithingTrim(Box::new(base.clone()), Box::new(addition.clone()), id);
        }
        Some(display)
    }

    /// `RecipeDisplayEntry` of display id `id`.
    pub fn wire_entry(&self, id: i32) -> Option<RecipeDisplayEntry> {
        let e = self.entry(id)?;
        Some(RecipeDisplayEntry {
            id,
            display: self.display(id)?,
            group: e.group,
            category: e.category,
            requirements: e
                .requirements
                .as_ref()
                .map(|r| r.iter().map(|i| Box::from(i.items())).collect()),
        })
    }
}

/// `RecipeBookSettings.TypeSettings` NBT keys per `RecipeBookType`.
const SETTINGS_KEYS: [(&str, &str); 4] = [
    ("isGuiOpen", "isFilteringCraftable"),
    ("isFurnaceGuiOpen", "isFurnaceFilteringCraftable"),
    ("isBlastingFurnaceGuiOpen", "isBlastingFurnaceFilteringCraftable"),
    ("isSmokerGuiOpen", "isSmokerFilteringCraftable"),
];

/// `FLAG_NOTIFICATION`.
const FLAG_NOTIFICATION: u8 = 1;
/// `FLAG_HIGHLIGHT`.
const FLAG_HIGHLIGHT: u8 = 2;

/// `ServerRecipeBook`: the recipes a player knows, the new ones (highlight) and the settings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlayerRecipeBook {
    /// Known recipe ids in unlock order.
    known: Vec<Box<str>>,
    known_set: HashSet<Box<str>>,
    /// `highlight` (`toBeDisplayed`).
    highlight: HashSet<Box<str>>,
    /// Open and filtering per book type.
    pub settings: [(bool, bool); 4],
}

impl PlayerRecipeBook {
    /// Whether the player knows recipe `id`.
    pub fn contains(&self, id: &str) -> bool {
        self.known_set.contains(id)
    }

    /// The known recipe ids.
    pub fn known(&self) -> impl Iterator<Item = &str> {
        self.known.iter().map(|s| &**s)
    }

    fn add(&mut self, id: &str) {
        if self.known_set.insert(Box::from(id)) {
            self.known.push(Box::from(id));
        }
    }

    fn remove(&mut self, id: &str) {
        if self.known_set.remove(id) {
            self.known.retain(|k| &**k != id);
        }
        self.highlight.remove(id);
    }

    /// `removeHighlight` (`recipe_book_seen_recipe`).
    pub fn remove_highlight(&mut self, book: &Book, display: i32) {
        if let Some(id) = book.recipe_of(display) {
            self.highlight.remove(id);
        }
    }

    /// `ServerboundRecipeBookChangeSettingsPacket`.
    pub fn set_settings(&mut self, book_type: i32, open: bool, filtering: bool) {
        if let Some(s) = usize::try_from(book_type)
            .ok()
            .and_then(|i| self.settings.get_mut(i))
        {
            *s = (open, filtering);
        }
    }

    /// `addRecipes`: unlocks `ids`; the packet for the new ones, if any, and their number
    /// (`awardRecipes`). Unknown and special recipes are skipped.
    pub fn unlock(&mut self, book: &Book, ids: &[&str]) -> (usize, Option<RecipeBookAdd>) {
        let mut entries = Vec::new();
        for &id in ids {
            if self.contains(id) || !book.contains(id) {
                continue;
            }
            self.add(id);
            self.highlight.insert(Box::from(id));
            let notify = book.notify.get(id).copied().unwrap_or(true);
            for &d in book.displays(id) {
                if let Some(e) = book.wire_entry(d) {
                    let flags = FLAG_HIGHLIGHT | if notify { FLAG_NOTIFICATION } else { 0 };
                    entries.push((e, flags));
                }
            }
        }
        let n = entries.len();
        let packet = (!entries.is_empty()).then_some(RecipeBookAdd {
            entries,
            replace: false,
        });
        (n, packet)
    }

    /// `removeRecipes`: locks `ids`; the packet for the removed displays and their number.
    pub fn lock(&mut self, book: &Book, ids: &[&str]) -> (usize, Option<RecipeBookRemove>) {
        let mut removed = Vec::new();
        for &id in ids {
            if self.contains(id) {
                self.remove(id);
                removed.extend_from_slice(book.displays(id));
            }
        }
        let n = removed.len();
        (n, (!removed.is_empty()).then_some(RecipeBookRemove { ids: removed }))
    }

    /// `sendInitialRecipeBook`: the settings and the whole book.
    pub fn initial(&self, book: &Book) -> (RecipeBookSettings, RecipeBookAdd) {
        let mut entries = Vec::new();
        for id in &self.known {
            let flags = if self.highlight.contains(id) {
                FLAG_HIGHLIGHT
            } else {
                0
            };
            for &d in book.displays(id) {
                if let Some(e) = book.wire_entry(d) {
                    entries.push((e, flags));
                }
            }
        }
        (
            RecipeBookSettings {
                types: self.settings,
            },
            RecipeBookAdd {
                entries,
                replace: true,
            },
        )
    }

    /// `ServerRecipeBook.Packed` into the player file's `recipeBook` compound.
    pub fn save(&self) -> Compound {
        let mut c = Compound::new();
        for ((open_key, filter_key), (open, filtering)) in SETTINGS_KEYS.iter().zip(self.settings)
        {
            c.insert(*open_key, Tag::Byte(i8::from(open)));
            c.insert(*filter_key, Tag::Byte(i8::from(filtering)));
        }
        let list = |ids: &mut dyn Iterator<Item = &str>| {
            Tag::List(vanillite_nbt::List(ids.map(|s| Tag::String(s.into())).collect()))
        };
        c.insert("recipes", list(&mut self.known.iter().map(|s| &**s)));
        let mut highlight: Vec<&str> = self.highlight.iter().map(|s| &**s).collect();
        highlight.sort_unstable();
        c.insert("toBeDisplayed", list(&mut highlight.into_iter()));
        c
    }

    /// `loadUntrusted`: recipes the book does not know are dropped.
    pub fn load(c: &Compound, book: &Book) -> PlayerRecipeBook {
        let mut out = PlayerRecipeBook::default();
        for ((open_key, filter_key), s) in SETTINGS_KEYS.iter().zip(out.settings.iter_mut()) {
            let flag = |k: &str| matches!(c.get(k), Some(Tag::Byte(v)) if *v != 0);
            *s = (flag(open_key), flag(filter_key));
        }
        let ids = |key: &str| -> Vec<String> {
            match c.get(key) {
                Some(Tag::List(list)) => list.0
                    .iter()
                    .filter_map(|t| match t {
                        Tag::String(s) => Some(s.to_string()),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            }
        };
        for id in ids("recipes") {
            if book.contains(&id) {
                out.add(&id);
            } else {
                log::error!("Tried to load unrecognized recipe: {id} removed now.");
            }
        }
        for id in ids("toBeDisplayed") {
            if book.contains(&id) {
                out.highlight.insert(Box::from(id.as_str()));
            } else {
                log::error!("Tried to load unrecognized recipe: {id} removed now.");
            }
        }
        out
    }
}
