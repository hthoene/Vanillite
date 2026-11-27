//! Recipes and the recipe book below the newest step.
//!
//! From 1.21.2 on the server sends recipe *displays* (`recipe_book_add`, `place_ghost_recipe`)
//! and, in `update_recipes`, only the item sets and stonecutter recipes; between two such
//! versions the displays are rewritten ([`Items::recipe_book_add`] and friends). Clients before
//! 1.21.2 know full recipes instead: the step 1.21.1 ↔ 1.21.2 builds them from the displays
//! ([`OldBook`]), names them `vanillite:book/<display id>` and unlocks them with `recipe`.

use std::collections::BTreeMap;

use crate::Result;
use crate::io;
use crate::portable::Items;
use crate::step::{V1_21_5, V26_1};
use vanillite_protocol::Error;
use vanillite_protocol::codec::{Reader, write_str, write_varint};

/// Nesting limit of slot displays.
const MAX_DEPTH: u8 = 16;
/// Elements of one list (ingredients, holder sets, composites, entries).
const MAX_LIST: usize = 1 << 16;

/// `minecraft:slot_display` in id order, from 26.1 on.
const SLOT_DISPLAYS: &[&str] = &[
    "empty",
    "any_fuel",
    "with_any_potion",
    "only_with_component",
    "item",
    "item_stack",
    "tag",
    "dyed",
    "smithing_trim",
    "with_remainder",
    "composite",
];
/// `minecraft:slot_display` in id order, 1.21.2 to 1.21.11.
const SLOT_DISPLAYS_1_21_2: &[&str] = &[
    "empty",
    "any_fuel",
    "item",
    "item_stack",
    "tag",
    "smithing_trim",
    "with_remainder",
    "composite",
];

fn slot_names(protocol: i32) -> &'static [&'static str] {
    if protocol >= V26_1 {
        SLOT_DISPLAYS
    } else {
        SLOT_DISPLAYS_1_21_2
    }
}

fn write_slot_type(out: &mut Vec<u8>, protocol: i32, name: &str) {
    let id = slot_names(protocol)
        .iter()
        .position(|n| *n == name)
        .and_then(|i| i32::try_from(i).ok())
        .unwrap_or(0);
    write_varint(out, id);
}

fn read_slot_type(r: &mut Reader<'_>, protocol: i32) -> Result<&'static str> {
    let id = r.varint()?;
    usize::try_from(id)
        .ok()
        .and_then(|i| slot_names(protocol).get(i))
        .copied()
        .ok_or(Error::Invalid("slot display type"))
}

fn depth_below(depth: u8) -> Result<u8> {
    depth
        .checked_sub(1)
        .ok_or(Error::Invalid("slot display depth"))
}

impl Items<'_> {
    /// `SlotDisplay` (1.21.2 on). Types the target version lacks become their nearest form:
    /// the shown item (`with_any_potion`, `only_with_component`), the dyed target or the
    /// untrimmed base.
    pub(crate) fn slot_display(&self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
        self.slot_display_depth(r, out, MAX_DEPTH)
    }

    fn slot_display_depth(&self, r: &mut Reader<'_>, out: &mut Vec<u8>, depth: u8) -> Result<()> {
        let depth = depth_below(depth)?;
        let (from, to) = (self.from(), self.to());
        let name = read_slot_type(r, from)?;
        let new = to >= V26_1;
        match name {
            "empty" | "any_fuel" => write_slot_type(out, to, name),
            "with_any_potion" => {
                if new {
                    write_slot_type(out, to, name);
                }
                self.slot_display_depth(r, out, depth)?;
            }
            "only_with_component" => {
                let mut inner = Vec::new();
                self.slot_display_depth(r, &mut inner, depth)?;
                let component = r.varint()?;
                match self.component_type(component).filter(|_| new) {
                    Some(c) => {
                        write_slot_type(out, to, name);
                        out.extend_from_slice(&inner);
                        write_varint(out, c);
                    }
                    None => out.extend_from_slice(&inner),
                }
            }
            "item" => {
                write_slot_type(out, to, name);
                write_varint(out, self.id("minecraft:item", r.varint()?).unwrap_or(1));
            }
            "item_stack" => {
                write_slot_type(out, to, name);
                self.particle_item(r, out)?;
            }
            "tag" => {
                write_slot_type(out, to, name);
                io::string(r, out)?;
            }
            "dyed" => {
                let mut dye = Vec::new();
                self.slot_display_depth(r, &mut dye, depth)?;
                let mut target = Vec::new();
                self.slot_display_depth(r, &mut target, depth)?;
                if new {
                    write_slot_type(out, to, name);
                    out.extend_from_slice(&dye);
                }
                out.extend_from_slice(&target);
            }
            "smithing_trim" => {
                let mut base = Vec::new();
                self.slot_display_depth(r, &mut base, depth)?;
                let mut material = Vec::new();
                self.slot_display_depth(r, &mut material, depth)?;
                // The pattern: a `trim_pattern` holder from 1.21.5 on, a slot display before.
                let mut pattern = Vec::new();
                let kept = if from >= V1_21_5 {
                    let holder = r.varint()?;
                    let id = holder
                        .checked_sub(1)
                        .filter(|i| *i >= 0)
                        .ok_or(Error::Invalid("inline trim pattern"))?;
                    match self
                        .dynamic("minecraft:trim_pattern", id)
                        .filter(|_| to >= V1_21_5)
                    {
                        Some(id) => {
                            write_varint(&mut pattern, id.saturating_add(1));
                            true
                        }
                        None => false,
                    }
                } else {
                    self.slot_display_depth(r, &mut pattern, depth)?;
                    true
                };
                if kept {
                    write_slot_type(out, to, name);
                    out.extend_from_slice(&base);
                    out.extend_from_slice(&material);
                    out.extend_from_slice(&pattern);
                } else {
                    out.extend_from_slice(&base);
                }
            }
            "with_remainder" => {
                write_slot_type(out, to, name);
                self.slot_display_depth(r, out, depth)?;
                self.slot_display_depth(r, out, depth)?;
            }
            _ => {
                write_slot_type(out, to, "composite");
                let n = io::len(r, MAX_LIST, 1)?;
                io::write_len(out, n);
                for _ in 0..n {
                    self.slot_display_depth(r, out, depth)?;
                }
            }
        }
        Ok(())
    }

    /// `ByteBufCodecs.holderSet` of items (an `Ingredient`): a tag name or a direct list.
    pub(crate) fn item_set(&self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
        let n = r.varint()?;
        let Some(n) = n.checked_sub(1) else {
            write_varint(out, 0);
            return io::string(r, out);
        };
        let n = usize::try_from(n).map_err(|_| Error::NegativeLength)?;
        if n > MAX_LIST || n > r.remaining() {
            return Err(Error::Invalid("item set"));
        }
        let mut ids = Vec::with_capacity(n);
        for _ in 0..n {
            if let Some(id) = self.id("minecraft:item", r.varint()?) {
                ids.push(id);
            }
        }
        io::write_len(out, ids.len().saturating_add(1));
        for id in ids {
            write_varint(out, id);
        }
        Ok(())
    }

    fn displays(&self, r: &mut Reader<'_>, out: &mut Vec<u8>, n: usize) -> Result<()> {
        for _ in 0..n {
            self.slot_display(r, out)?;
        }
        Ok(())
    }

    /// `RecipeDisplay` (the same five types since 1.21.2).
    pub(crate) fn recipe_display(&self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
        let kind = io::varint(r, out)?;
        match kind {
            0 => {
                let n = io::len(r, MAX_LIST, 1)?;
                io::write_len(out, n);
                self.displays(r, out, n)?;
                self.displays(r, out, 2)
            }
            1 => {
                io::varint(r, out)?;
                io::varint(r, out)?;
                let n = io::len(r, MAX_LIST, 1)?;
                io::write_len(out, n);
                self.displays(r, out, n)?;
                self.displays(r, out, 2)
            }
            2 => {
                self.displays(r, out, 4)?;
                io::varint(r, out)?;
                io::bytes(r, out, 4)
            }
            3 => self.displays(r, out, 3),
            4 => self.displays(r, out, 5),
            _ => Err(Error::Invalid("recipe display type")),
        }
    }

    /// `RecipeDisplayEntry`: id, display, group, book category, crafting requirements.
    fn display_entry(&self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
        io::varint(r, out)?;
        self.recipe_display(r, out)?;
        io::varint(r, out)?;
        let category = r.varint()?;
        write_varint(
            out,
            self.id("minecraft:recipe_book_category", category)
                .unwrap_or(0),
        );
        if io::boolean(r, out)? {
            let n = io::len(r, MAX_LIST, 1)?;
            io::write_len(out, n);
            for _ in 0..n {
                self.item_set(r, out)?;
            }
        }
        Ok(())
    }

    /// `recipe_book_add`: entries with their flags, the replace flag.
    pub(crate) fn recipe_book_add(&self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
        let n = io::len(r, MAX_LIST, 3)?;
        io::write_len(out, n);
        for _ in 0..n {
            self.display_entry(r, out)?;
            io::bytes(r, out, 1)?;
        }
        io::bytes(r, out, 1)
    }

    /// `update_recipes` (1.21.2 on): item sets of recipe properties, stonecutter recipes.
    pub(crate) fn update_recipes(&self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
        let n = io::len(r, MAX_LIST, 2)?;
        io::write_len(out, n);
        for _ in 0..n {
            io::string(r, out)?;
            let m = io::len(r, MAX_LIST, 1)?;
            let mut ids = Vec::with_capacity(m);
            for _ in 0..m {
                if let Some(id) = self.id("minecraft:item", r.varint()?) {
                    ids.push(id);
                }
            }
            io::write_len(out, ids.len());
            for id in ids {
                write_varint(out, id);
            }
        }
        let n = io::len(r, MAX_LIST, 2)?;
        io::write_len(out, n);
        for _ in 0..n {
            self.item_set(r, out)?;
            self.slot_display(r, out)?;
        }
        Ok(())
    }

    /// `place_ghost_recipe` (1.21.2 on): container id, display.
    pub(crate) fn place_ghost_recipe(&self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
        io::varint(r, out)?;
        self.recipe_display(r, out)
    }

    /// `update_recipes` before 1.21.2: full recipes, of the serializers [`OldBook`] writes.
    pub(crate) fn old_recipes(&self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
        let n = io::len(r, MAX_LIST, 3)?;
        io::write_len(out, n);
        for _ in 0..n {
            io::string(r, out)?;
            let serializer = io::varint(r, out)?;
            match serializer {
                SHAPED => {
                    io::string(r, out)?;
                    io::varint(r, out)?;
                    let w = io::varint(r, out)?;
                    let h = io::varint(r, out)?;
                    let cells = usize::try_from(w.saturating_mul(h))
                        .ok()
                        .filter(|n| *n <= 9)
                        .ok_or(Error::Invalid("recipe size"))?;
                    for _ in 0..cells {
                        self.old_ingredient(r, out)?;
                    }
                    self.stack(r, out)?;
                    io::bytes(r, out, 1)?;
                }
                SHAPELESS => {
                    io::string(r, out)?;
                    io::varint(r, out)?;
                    let n = io::len(r, 9, 1)?;
                    io::write_len(out, n);
                    for _ in 0..n {
                        self.old_ingredient(r, out)?;
                    }
                    self.stack(r, out)?;
                }
                SMELTING..=CAMPFIRE => {
                    io::string(r, out)?;
                    io::varint(r, out)?;
                    self.old_ingredient(r, out)?;
                    self.stack(r, out)?;
                    io::bytes(r, out, 4)?;
                    io::varint(r, out)?;
                }
                STONECUTTING => {
                    io::string(r, out)?;
                    self.old_ingredient(r, out)?;
                    self.stack(r, out)?;
                }
                SMITHING_TRANSFORM | SMITHING_TRIM => {
                    for _ in 0..3 {
                        self.old_ingredient(r, out)?;
                    }
                    if serializer == SMITHING_TRANSFORM {
                        self.stack(r, out)?;
                    }
                }
                _ => return Err(Error::Invalid("recipe serializer")),
            }
        }
        Ok(())
    }

    /// `Ingredient` before 1.21.2: a list of stacks.
    fn old_ingredient(&self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
        let n = io::len(r, MAX_LIST, 1)?;
        io::write_len(out, n);
        for _ in 0..n {
            self.stack(r, out)?;
        }
        Ok(())
    }

    /// The stacks (1.21.1 `ItemStack`s) a 1.21.2 slot display shows, appended to `stacks`.
    fn old_stacks(&self, r: &mut Reader<'_>, stacks: &mut Vec<Vec<u8>>, depth: u8) -> Result<()> {
        let depth = depth_below(depth)?;
        match read_slot_type(r, self.from())? {
            "item" => {
                let item = self.id("minecraft:item", r.varint()?).unwrap_or(1);
                let mut s = Vec::with_capacity(5);
                write_varint(&mut s, 1);
                write_varint(&mut s, item);
                s.extend_from_slice(&[0, 0]);
                stacks.push(s);
            }
            "item_stack" => {
                let mut s = Vec::new();
                self.stack(r, &mut s)?;
                if s.first() != Some(&0) {
                    stacks.push(s);
                }
            }
            "tag" => {
                // Tag contents are unknown here (the server sends direct lists).
                r.string(io::MAX_STRING)?;
            }
            "smithing_trim" => {
                self.old_stacks(r, stacks, depth)?;
                self.old_stacks(r, &mut Vec::new(), depth)?;
                self.old_stacks(r, &mut Vec::new(), depth)?;
            }
            "with_remainder" => {
                self.old_stacks(r, stacks, depth)?;
                self.old_stacks(r, &mut Vec::new(), depth)?;
            }
            "composite" => {
                let n = io::len(r, MAX_LIST, 1)?;
                for _ in 0..n {
                    self.old_stacks(r, stacks, depth)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn old_display(&self, r: &mut Reader<'_>) -> Result<Vec<Vec<u8>>> {
        let mut stacks = Vec::new();
        self.old_stacks(r, &mut stacks, MAX_DEPTH)?;
        Ok(stacks)
    }
}

/// `minecraft:recipe_serializer` ids before 1.21.2 (the same in 1.20.5 and 1.21).
const SHAPED: i32 = 0;
const SHAPELESS: i32 = 1;
const SMELTING: i32 = 15;
const BLASTING: i32 = 16;
const SMOKING: i32 = 17;
const CAMPFIRE: i32 = 18;
const STONECUTTING: i32 = 19;
const SMITHING_TRANSFORM: i32 = 20;
const SMITHING_TRIM: i32 = 21;

/// `recipe` states before 1.21.2.
const INIT: i32 = 0;
const ADD: i32 = 1;
const REMOVE: i32 = 2;

/// Prefix of the names of the recipes built from displays.
const BOOK: &str = "vanillite:book/";

fn write_old_ingredient(out: &mut Vec<u8>, stacks: &[Vec<u8>]) {
    io::write_len(out, stacks.len());
    for s in stacks {
        out.extend_from_slice(s);
    }
}

/// The recipes and recipe book a client before 1.21.2 was sent (step 1.21.1 ↔ 1.21.2).
#[derive(Debug, Default)]
pub(crate) struct OldBook {
    /// Stonecutter recipes (`RecipeHolder`s) from `update_recipes`.
    stonecutter: Vec<Vec<u8>>,
    /// Display id → the recipe built from it and the display (in the newer version).
    book: BTreeMap<i32, (Vec<u8>, Vec<u8>)>,
    /// Book settings: open and filtering of the four book types.
    settings: [u8; 8],
}

impl OldBook {
    /// `update_recipes` of 1.21.2 → the old one with the stonecutter recipes and the book.
    pub(crate) fn update_recipes(
        &mut self,
        items: &Items<'_>,
        r: &mut Reader<'_>,
        out: &mut Vec<u8>,
    ) -> Result<()> {
        let n = io::len(r, MAX_LIST, 2)?;
        for _ in 0..n {
            r.string(io::MAX_STRING)?;
            let m = io::len(r, MAX_LIST, 1)?;
            for _ in 0..m {
                r.varint()?;
            }
        }
        let n = io::len(r, MAX_LIST, 2)?;
        self.stonecutter.clear();
        for i in 0..n {
            let mut input = Vec::new();
            let set = r.varint()?;
            match set.checked_sub(1) {
                None => {
                    r.string(io::MAX_STRING)?;
                }
                Some(m) => {
                    let m = usize::try_from(m).map_err(|_| Error::NegativeLength)?;
                    if m > MAX_LIST || m > r.remaining() {
                        return Err(Error::Invalid("item set"));
                    }
                    for _ in 0..m {
                        if let Some(item) = items.id("minecraft:item", r.varint()?) {
                            let mut s = Vec::with_capacity(5);
                            write_varint(&mut s, 1);
                            write_varint(&mut s, item);
                            s.extend_from_slice(&[0, 0]);
                            input.push(s);
                        }
                    }
                }
            }
            let results = items.old_display(r)?;
            let Some(result) = results.first() else {
                continue;
            };
            let mut recipe = Vec::new();
            write_str(&mut recipe, &format!("vanillite:stonecutter/{i}"));
            write_varint(&mut recipe, STONECUTTING);
            write_str(&mut recipe, "");
            write_old_ingredient(&mut recipe, &input);
            recipe.extend_from_slice(result);
            self.stonecutter.push(recipe);
        }
        self.write_recipes(out);
        Ok(())
    }

    /// The body of the old `update_recipes` after its id.
    fn write_recipes(&self, out: &mut Vec<u8>) {
        io::write_len(out, self.stonecutter.len().saturating_add(self.book.len()));
        for r in &self.stonecutter {
            out.extend_from_slice(r);
        }
        for (r, _) in self.book.values() {
            out.extend_from_slice(r);
        }
    }

    /// `recipe_book_add` → the old `update_recipes` with every recipe the book now has (body
    /// after the id in `out`) and the `recipe` packet that unlocks the new ones (body after
    /// the id in `unlock`).
    pub(crate) fn add(
        &mut self,
        items: &Items<'_>,
        r: &mut Reader<'_>,
        out: &mut Vec<u8>,
        unlock: &mut Vec<u8>,
    ) -> Result<()> {
        let n = io::len(r, MAX_LIST, 3)?;
        let mut entries = Vec::with_capacity(n);
        for _ in 0..n {
            let id = r.varint()?;
            let recipe = self.recipe(items, id, r)?;
            let flags = r.u8()?;
            entries.push((id, recipe, flags));
        }
        let replace = r.u8()? != 0;
        if replace {
            self.book.clear();
        }
        let mut ids = Vec::new();
        let mut highlight = Vec::new();
        for (id, recipe, flags) in entries {
            let Some(recipe) = recipe else { continue };
            self.book.insert(id, recipe);
            ids.push(id);
            if flags & 2 != 0 {
                highlight.push(id);
            }
        }
        self.write_recipes(out);
        write_varint(unlock, if replace { INIT } else { ADD });
        unlock.extend_from_slice(&self.settings);
        write_names(unlock, &ids);
        if replace {
            write_names(unlock, &highlight);
        }
        Ok(())
    }

    /// `recipe_book_remove` → `recipe` (remove).
    pub(crate) fn remove(&mut self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
        let n = io::len(r, MAX_LIST, 1)?;
        let mut ids = Vec::with_capacity(n);
        for _ in 0..n {
            let id = r.varint()?;
            if self.book.contains_key(&id) {
                ids.push(id);
            }
        }
        write_varint(out, REMOVE);
        out.extend_from_slice(&self.settings);
        write_names(out, &ids);
        Ok(())
    }

    /// `recipe_book_settings` → `recipe` (add nothing, with the settings).
    pub(crate) fn settings(&mut self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
        let s = r.take(8)?;
        self.settings.copy_from_slice(s);
        write_varint(out, ADD);
        out.extend_from_slice(&self.settings);
        write_names(out, &[]);
        Ok(())
    }

    /// `place_ghost_recipe` of 1.21.2 → the old one (container byte, recipe name). `false`: the
    /// display belongs to no known recipe.
    pub(crate) fn ghost(&self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<bool> {
        let container = r.varint()?;
        let display = r.take_rest();
        let Some(id) = self
            .book
            .iter()
            .find(|(_, (_, d))| d.as_slice() == display)
            .map(|(id, _)| *id)
        else {
            return Ok(false);
        };
        out.push(u8::try_from(container).map_err(|_| Error::Invalid("container id"))?);
        write_str(out, &format!("{BOOK}{id}"));
        Ok(true)
    }

    /// The old recipe of one `RecipeDisplayEntry` (after its id) and its display bytes; `None`
    /// for displays without old recipe (stonecutter displays: the stonecutter recipes come from
    /// `update_recipes`; displays without result).
    fn recipe(
        &self,
        items: &Items<'_>,
        id: i32,
        r: &mut Reader<'_>,
    ) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
        let start = r.rest();
        let kind = r.varint()?;
        let mut data = Vec::new();
        let mut serializer = SHAPELESS;
        let mut result = None;
        // The display; the group and category follow it and are written once known.
        let mut body = Vec::new();
        match kind {
            0 | 1 => {
                let mut size = None;
                if kind == 1 {
                    serializer = SHAPED;
                    size = Some((r.varint()?, r.varint()?));
                }
                let n = io::len(r, 9, 1)?;
                let mut ingredients = Vec::with_capacity(n);
                for _ in 0..n {
                    ingredients.push(items.old_display(r)?);
                }
                result = items.old_display(r)?.into_iter().next();
                items.old_display(r)?;
                match size {
                    Some((w, h)) => {
                        if usize::try_from(w.saturating_mul(h)).ok() != Some(n) {
                            result = None;
                        }
                        write_varint(&mut body, w);
                        write_varint(&mut body, h);
                    }
                    None => io::write_len(&mut body, n),
                }
                for i in &ingredients {
                    write_old_ingredient(&mut body, i);
                }
            }
            2 => {
                let ingredient = items.old_display(r)?;
                items.old_display(r)?;
                result = items.old_display(r)?.into_iter().next();
                items.old_display(r)?;
                let duration = r.varint()?;
                let experience = r.take(4)?;
                write_old_ingredient(&mut body, &ingredient);
                if let Some(res) = &result {
                    body.extend_from_slice(res);
                }
                body.extend_from_slice(experience);
                write_varint(&mut body, duration);
            }
            3 => {
                for _ in 0..3 {
                    items.old_display(r)?;
                }
            }
            4 => {
                for _ in 0..3 {
                    let i = items.old_display(r)?;
                    write_old_ingredient(&mut body, &i);
                }
                let trim = read_slot_type(&mut r.clone(), items.from())? == "smithing_trim";
                let res = items.old_display(r)?.into_iter().next();
                items.old_display(r)?;
                if trim {
                    serializer = SMITHING_TRIM;
                    result = Some(Vec::new());
                } else {
                    serializer = SMITHING_TRANSFORM;
                    if let Some(res) = &res {
                        body.extend_from_slice(res);
                    }
                    result = res;
                }
            }
            _ => return Err(Error::Invalid("recipe display type")),
        }
        let display = start
            .get(..start.len().saturating_sub(r.remaining()))
            .ok_or(Error::UnexpectedEnd)?
            .to_vec();
        let group = r.varint()?;
        let category = r.varint()?;
        if io::boolean(r, &mut Vec::new())? {
            let n = io::len(r, MAX_LIST, 1)?;
            for _ in 0..n {
                items.item_set(r, &mut Vec::new())?;
            }
        }
        let Some(result) = result else {
            return Ok(None);
        };
        // `minecraft:recipe_book_category` (1.21.2): crafting building, redstone, equipment,
        // misc; furnace food, blocks, misc; blast furnace blocks, misc; smoker food;
        // stonecutter; smithing; campfire.
        let (cooking, book_category) = match category {
            4 => (SMELTING, 0),
            5 => (SMELTING, 1),
            6 => (SMELTING, 2),
            7 => (BLASTING, 1),
            8 => (BLASTING, 2),
            9 => (SMOKING, 0),
            12 => (CAMPFIRE, 0),
            0..=3 => (SMELTING, category),
            _ => (SMELTING, 3),
        };
        if kind == 2 {
            serializer = cooking;
        }
        let group = match group.checked_sub(1) {
            Some(g) if g >= 0 => format!("vanillite:{g}"),
            _ => String::new(),
        };
        write_str(&mut data, &format!("{BOOK}{id}"));
        write_varint(&mut data, serializer);
        match serializer {
            SHAPED | SHAPELESS => {
                write_str(&mut data, &group);
                write_varint(&mut data, book_category.min(3));
                data.extend_from_slice(&body);
                data.extend_from_slice(&result);
                if serializer == SHAPED {
                    data.push(1);
                }
            }
            SMELTING..=CAMPFIRE => {
                write_str(&mut data, &group);
                write_varint(&mut data, book_category.min(2));
                data.extend_from_slice(&body);
            }
            _ => data.extend_from_slice(&body),
        }
        Ok(Some((data, display)))
    }
}

/// A list of recipe names of display ids.
fn write_names(out: &mut Vec<u8>, ids: &[i32]) {
    io::write_len(out, ids.len());
    for id in ids {
        write_str(out, &format!("{BOOK}{id}"));
    }
}

/// The display id of a recipe name this layer gave an old client.
pub(crate) fn display_id(name: &str) -> Option<i32> {
    name.strip_prefix(BOOK)?.parse().ok()
}
