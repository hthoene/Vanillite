//! Java recipe book displays as Bedrock crafting recipes (`crafting_data`).
//!
//! Bedrock only shows a crafting result for recipes it knows, so the shaped and shapeless
//! displays of the Java recipe book (`recipe_book_add`) are sent as Bedrock recipes. A Bedrock
//! ingredient names one item: an ingredient with several items (a tag) becomes one recipe per
//! item, with all slots of the same item set taking the same item. The Java server still decides
//! what a grid crafts; a Bedrock recipe network id leads back to its Java `RecipeDisplayId` for
//! the recipe book (`place_recipe`).
//!
//! Stonecutter recipes (`update_recipes`) become shapeless `stonecutter` recipes, smithing
//! displays of the book smithing transform recipes, and one smithing trim recipe over the
//! Bedrock trim tags lets the client preview trims.

use vanillite_protocol::item::ItemStack;
use vanillite_protocol::recipe_display::{RecipeDisplay, RecipeDisplayEntry, SlotDisplay};

use crate::io;
use crate::items::Items;

/// Most Bedrock recipes one Java display becomes.
const MAX_VARIANTS: usize = 32;
/// Most Bedrock recipes of one session.
const MAX_RECIPES: usize = 1 << 16;

/// What a Bedrock recipe network id stands for.
#[derive(Debug, Clone)]
enum Entry {
    /// A recipe book display: its Java `RecipeDisplayId` and result.
    Book(i32, ItemStack),
    /// A stonecutter recipe: its index in the `update_recipes` list and result.
    Stonecutter(usize, ItemStack),
    /// The smithing trim recipe (the Java server computes the result).
    Trim,
    /// A Bedrock multi recipe (the client and the Java server compute the result).
    Multi,
}

/// Bedrock's hard-coded crafting and cartography recipes (multi recipes, by their fixed UUIDs):
/// the client shows their results only when the server lists them. The Java server's special
/// recipes decide the real result. The list is the one of the vanilla Bedrock server (the
/// public `special_hardcoded` recipe list); the four without a note cover the remaining special
/// recipes (banners, firework stars, dyeing, shield decoration).
const MULTI_RECIPES: [u128; 14] = [
    0xd1ca6b84_338e_4f2f_9c6b_76cc8b4bd98d, // book cloning
    0x00000000_0000_0000_0000_000000000001, // repairing in the grid
    0xd392b075_4ba1_40ae_8789_af868d56f6ce, // map extending
    0x85939755_ba10_4d9d_a4cc_efb7a8e943c4, // map cloning
    0x00000000_0000_0000_0000_000000000002, // firework rockets
    0x685a742a_c42e_4a4e_88ea_5eb83fc98e5b, // decorated pots
    0x8b36268c_1829_483c_a0f1_993b7156a8f2, // cartography: map extending
    0x442d85ed_8272_4543_a6f1_418f90ded05d, // cartography: map cloning
    0x98c84b38_1085_46bd_b1ce_dd38c159e6cc, // cartography: map upgrading
    0x602234e4_cac1_4353_8bb7_b1ebff70024b, // cartography: map locking
    0x00000000_0000_0000_0000_0000000000c8,
    0xaecd2294_4b94_434b_8667_4499bb2c9327,
    0xb5c5d105_75a2_4076_af2b_923ea2bf4bf0,
    0xd81aaeaf_e172_4440_9225_868df030d27b,
];

/// One stonecutter recipe of `update_recipes`: the items it takes and its result.
#[derive(Debug, Clone)]
pub struct Stonecut {
    pub input: Box<[u16]>,
    pub result: Option<ItemStack>,
}

/// The Bedrock recipes sent to one client.
#[derive(Debug, Default)]
pub struct Recipes {
    /// Per Bedrock recipe network id - 1.
    java: Vec<Entry>,
    /// The stonecutter recipes of the server, in Java order.
    stonecutter: Vec<Stonecut>,
}

/// Most stonecutter recipes kept (vanilla has about 300).
const MAX_STONECUTTER: usize = 4096;

/// Reads the stonecutter recipes of an `update_recipes` body (after the packet id).
pub fn read_stonecutter(r: &mut vanillite_protocol::codec::Reader<'_>) -> Option<Vec<Stonecut>> {
    use vanillite_protocol::codec::Decode;
    let sets = r.len_prefix("property sets", 256, 2).ok()?;
    for _ in 0..sets {
        r.string(32767).ok()?;
        let n = r.len_prefix("property set", 1 << 16, 1).ok()?;
        for _ in 0..n {
            r.varint().ok()?;
        }
    }
    let n = r
        .len_prefix("stonecutter recipes", MAX_STONECUTTER, 2)
        .ok()?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let input = vanillite_protocol::recipe_display::decode_holder_set(r).ok()?;
        let display = SlotDisplay::decode(r).ok()?;
        out.push(Stonecut {
            input,
            result: result(&display),
        });
    }
    Some(out)
}

/// Writes an `ItemDescriptorCount` naming an item tag.
fn write_tag_descriptor(out: &mut Vec<u8>, tag: &str) {
    io::write_varu32(out, 1);
    io::write_str(out, "item_tag");
    io::write_str(out, tag);
    io::write_vari32(out, 32767); // aux: every descriptor but "name" carries one
    io::write_vari32(out, 1);
}

/// The items one slot display accepts (empty: an empty slot or nothing known).
fn options(d: &SlotDisplay, out: &mut Vec<u16>) {
    match d {
        SlotDisplay::Item(i) => out.push(*i),
        SlotDisplay::ItemStack(s) if !s.is_empty() => out.push(s.item),
        SlotDisplay::Tag(items) => out.extend_from_slice(items),
        SlotDisplay::Composite(list) => list.iter().for_each(|d| options(d, out)),
        SlotDisplay::WithRemainder(d, _)
        | SlotDisplay::OnlyWithComponent(d, _)
        | SlotDisplay::WithAnyPotion(d) => options(d, out),
        _ => {}
    }
}

fn result(d: &SlotDisplay) -> Option<ItemStack> {
    match d {
        SlotDisplay::Item(i) => Some(ItemStack::new(*i, 1)),
        SlotDisplay::ItemStack(s) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// Writes an `ItemDescriptorCount`: a named item, or the empty descriptor for `None`.
fn write_descriptor(out: &mut Vec<u8>, item: Option<(&str, u16)>) {
    match item {
        Some((name, meta)) => {
            io::write_varu32(out, 1);
            io::write_str(out, "name");
            io::write_str(out, name);
            io::write_vari32(out, i32::from(meta));
            io::write_vari32(out, 1);
        }
        None => {
            io::write_varu32(out, 0);
            io::write_vari32(out, 32767);
            io::write_vari32(out, 0);
        }
    }
}

/// The common tail of shaped and shapeless recipes after the inputs.
fn write_tail(
    out: &mut Vec<u8>,
    items: &Items,
    result: &ItemStack,
    block: &str,
    shaped: bool,
    net_id: u32,
) {
    io::write_varu32(out, 1);
    items.write_stack(out, result);
    out.extend_from_slice(&[0; 16]); // uuid
    io::write_str(out, block);
    io::write_vari32(out, 0); // priority
    if shaped {
        out.push(1); // mirrored patterns match too, as on Java
    }
    // Unlock requirement: always unlocked (the Java book already decided).
    out.push(1);
    io::write_vari32(out, 1);
    out.push(0);
    io::write_varu32(out, net_id);
}

impl Recipes {
    fn entry(&self, net_id: u32) -> Option<&Entry> {
        let i = usize::try_from(net_id.checked_sub(1)?).ok()?;
        self.java.get(i)
    }

    /// The Java `RecipeDisplayId` of a Bedrock recipe network id.
    pub fn java(&self, net_id: u32) -> Option<i32> {
        match self.entry(net_id)? {
            Entry::Book(id, _) => Some(*id),
            _ => None,
        }
    }

    /// The result of a Bedrock recipe network id (`None`: the Java server's result slot).
    pub fn result(&self, net_id: u32) -> Option<ItemStack> {
        match self.entry(net_id)? {
            Entry::Book(_, r) | Entry::Stonecutter(_, r) => Some(r.clone()),
            Entry::Trim | Entry::Multi => None,
        }
    }

    /// The Java stonecutter button of a Bedrock recipe network id with `input` in the
    /// stonecutter: the recipe's index among the recipes that take `input`.
    pub fn stonecutter_button(&self, net_id: u32, input: u16) -> Option<i32> {
        let Entry::Stonecutter(index, _) = self.entry(net_id)? else {
            return None;
        };
        let recipe = self.stonecutter.get(*index)?;
        if !recipe.input.contains(&input) {
            return None;
        }
        let before = self
            .stonecutter
            .iter()
            .take(*index)
            .filter(|r| r.input.contains(&input))
            .count();
        i32::try_from(before).ok()
    }

    /// Writes the stonecutter recipes as shapeless `stonecutter` recipes; returns how many.
    fn write_stonecutter(&mut self, items: &Items, out: &mut Vec<u8>) -> usize {
        let mut n = 0usize;
        for (index, recipe) in self.stonecutter.iter().enumerate() {
            let Some(result) = recipe
                .result
                .as_ref()
                .filter(|r| items.name(r.item).is_some())
            else {
                continue;
            };
            for &input in recipe.input.iter().take(MAX_VARIANTS) {
                let Some(name) = items.name(input) else {
                    continue;
                };
                if self.java.len() >= MAX_RECIPES {
                    return n;
                }
                self.java.push(Entry::Stonecutter(index, result.clone()));
                let net_id = u32::try_from(self.java.len()).unwrap_or(0);
                io::write_str(out, &format!("vanillite:s{index}_{input}"));
                io::write_len(out, 1);
                write_descriptor(out, Some(name));
                write_tail(out, items, result, "stonecutter", false, net_id);
                n = n.saturating_add(1);
            }
        }
        n
    }

    /// The body of a `crafting_data` with the stonecutter recipes of `update_recipes`.
    pub fn set_stonecutter(&mut self, items: &Items, recipes: Vec<Stonecut>) -> Vec<u8> {
        self.stonecutter = recipes;
        let mut shapeless = Vec::new();
        let n = self.write_stonecutter(items, &mut shapeless);
        let mut body = Vec::with_capacity(shapeless.len().saturating_add(16));
        io::write_len(&mut body, 0);
        io::write_len(&mut body, n);
        body.extend_from_slice(&shapeless);
        body.extend_from_slice(&[0; 9]);
        body.push(0);
        body
    }

    /// Writes the smithing transform recipes of one smithing display (one per combination of
    /// the template, base and addition items, at most [`MAX_VARIANTS`]); returns how many.
    fn write_transform(
        &mut self,
        items: &Items,
        out: &mut Vec<u8>,
        java: i32,
        slots: [&SlotDisplay; 3],
        result: &ItemStack,
    ) -> usize {
        if items.name(result.item).is_none() {
            return 0;
        }
        let [templates, bases, additions] = slots.map(|d| {
            let mut o = Vec::new();
            options(d, &mut o);
            o.retain(|&i| items.name(i).is_some());
            o
        });
        let mut n = 0usize;
        for &t in &templates {
            for &b in &bases {
                for &a in &additions {
                    if n >= MAX_VARIANTS || self.java.len() >= MAX_RECIPES {
                        return n;
                    }
                    self.java.push(Entry::Book(java, result.clone()));
                    let net_id = u32::try_from(self.java.len()).unwrap_or(0);
                    io::write_str(out, &format!("vanillite:j{java}_{n}"));
                    for i in [t, b, a] {
                        write_descriptor(out, items.name(i));
                    }
                    items.write_stack(out, result);
                    io::write_str(out, "smithing_table");
                    io::write_varu32(out, net_id);
                    n = n.saturating_add(1);
                }
            }
        }
        n
    }

    /// The body of a `crafting_data` with the crafting displays of `entries` (`replace`: the
    /// whole book, the client's recipes are cleared first). `None` when nothing is sent.
    pub fn add(
        &mut self,
        items: &Items,
        entries: &[(RecipeDisplayEntry, u8)],
        replace: bool,
    ) -> Option<Vec<u8>> {
        if replace {
            self.java.clear();
        }
        let mut shaped = Vec::new();
        let mut shapeless = Vec::new();
        let mut transform = Vec::new();
        let (mut n_shaped, mut n_shapeless, mut n_transform) = (0usize, 0usize, 0usize);
        for (entry, _) in entries {
            if let RecipeDisplay::Smithing {
                template,
                base,
                addition,
                result: result_display,
                ..
            } = &entry.display
            {
                if let Some(result) = result(result_display) {
                    n_transform = n_transform.saturating_add(self.write_transform(
                        items,
                        &mut transform,
                        entry.id,
                        [template, base, addition],
                        &result,
                    ));
                }
                continue;
            }
            let (width, ingredients, result_display, out, n) = match &entry.display {
                RecipeDisplay::Shaped {
                    width,
                    height,
                    ingredients,
                    result,
                    ..
                } => {
                    if i64::from(*width).saturating_mul(i64::from(*height))
                        != i64::try_from(ingredients.len()).unwrap_or(-1)
                    {
                        continue;
                    }
                    (
                        Some((*width, *height)),
                        ingredients,
                        result,
                        &mut shaped,
                        &mut n_shaped,
                    )
                }
                RecipeDisplay::Shapeless {
                    ingredients,
                    result,
                    ..
                } => (None, ingredients, result, &mut shapeless, &mut n_shapeless),
                _ => continue,
            };
            let Some(result) = result(result_display) else {
                continue;
            };
            if items.name(result.item).is_none() || ingredients.len() > 9 {
                continue;
            }
            let slots: Vec<Vec<u16>> = ingredients
                .iter()
                .map(|d| {
                    let mut o = Vec::new();
                    options(d, &mut o);
                    o.retain(|&i| items.name(i).is_some());
                    o
                })
                .collect();
            // A shapeless ingredient must name an item; a shaped one may be an empty cell.
            let unknown = ingredients
                .iter()
                .zip(&slots)
                .any(|(d, o)| o.is_empty() && (width.is_none() || *d != SlotDisplay::Empty));
            if unknown {
                continue;
            }
            // Item sets with a choice vary together.
            let mut groups: Vec<&Vec<u16>> = Vec::new();
            for o in &slots {
                if o.len() > 1 && !groups.contains(&o) {
                    groups.push(o);
                }
            }
            let variants = groups
                .iter()
                .try_fold(1usize, |acc, g| acc.checked_mul(g.len()))
                .unwrap_or(usize::MAX)
                .min(MAX_VARIANTS);
            for k in 0..variants {
                if self.java.len() >= MAX_RECIPES {
                    break;
                }
                self.java.push(Entry::Book(entry.id, result.clone()));
                let net_id = u32::try_from(self.java.len()).unwrap_or(0);
                io::write_str(out, &format!("vanillite:j{}_{k}", entry.id));
                if let Some((w, h)) = width {
                    io::write_vari32(out, w);
                    io::write_vari32(out, h);
                }
                io::write_len(out, slots.len());
                for o in &slots {
                    let item = match o.len() {
                        0 => None,
                        1 => o.first().copied(),
                        _ => {
                            // The k-th combination of the groups' items.
                            let mut rest = k;
                            let mut pick = None;
                            for g in &groups {
                                let len = g.len().max(1);
                                if *g == o {
                                    pick = g.get(rest.checked_rem(len).unwrap_or(0)).copied();
                                    break;
                                }
                                rest = rest.checked_div(len).unwrap_or(0);
                            }
                            pick
                        }
                    };
                    write_descriptor(out, item.and_then(|i| items.name(i)));
                }
                write_tail(
                    out,
                    items,
                    &result,
                    "crafting_table",
                    width.is_some(),
                    net_id,
                );
                *n = n.saturating_add(1);
            }
        }
        // The whole book clears the client's recipes: the stonecutter, trim and multi recipes
        // again.
        let mut trim = Vec::new();
        let mut multi = Vec::new();
        let mut n_multi = 0usize;
        if replace {
            n_shapeless = n_shapeless.saturating_add(self.write_stonecutter(items, &mut shapeless));
            if self.java.len() < MAX_RECIPES {
                self.java.push(Entry::Trim);
                let net_id = u32::try_from(self.java.len()).unwrap_or(0);
                io::write_str(&mut trim, "minecraft:smithing_armor_trim");
                write_tag_descriptor(&mut trim, "minecraft:trim_templates");
                write_tag_descriptor(&mut trim, "minecraft:trimmable_armors");
                write_tag_descriptor(&mut trim, "minecraft:trim_materials");
                io::write_str(&mut trim, "smithing_table");
                io::write_varu32(&mut trim, net_id);
            }
            for uuid in MULTI_RECIPES {
                if self.java.len() >= MAX_RECIPES {
                    break;
                }
                self.java.push(Entry::Multi);
                io::write_uuid(&mut multi, uuid);
                io::write_varu32(&mut multi, u32::try_from(self.java.len()).unwrap_or(0));
                n_multi = n_multi.saturating_add(1);
            }
        }
        if n_shaped == 0 && n_shapeless == 0 && n_transform == 0 && !replace {
            return None;
        }
        let mut body = Vec::with_capacity(
            shaped
                .len()
                .saturating_add(shapeless.len())
                .saturating_add(16),
        );
        io::write_len(&mut body, n_shaped);
        body.extend_from_slice(&shaped);
        io::write_len(&mut body, n_shapeless);
        body.extend_from_slice(&shapeless);
        io::write_len(&mut body, n_multi);
        body.extend_from_slice(&multi);
        // User data shapeless, chemistry shapeless and shaped: none.
        body.extend_from_slice(&[0; 3]);
        io::write_len(&mut body, n_transform);
        body.extend_from_slice(&transform);
        io::write_len(&mut body, usize::from(!trim.is_empty()));
        body.extend_from_slice(&trim);
        // Potion and potion container recipes, material reducers: none.
        body.extend_from_slice(&[0; 3]);
        body.push(u8::from(replace));
        Some(body)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use vanillite_protocol::codec::{Decode, Encode, Reader};
    use vanillite_protocol::packets::play::clientbound::RecipeBookAdd;

    fn item(name: &str) -> u16 {
        vanillite_data::registry::ITEM.id(name).unwrap()
    }

    #[test]
    fn stonecutter_buttons_follow_the_java_list() {
        let (stone, cobble) = (item("minecraft:stone"), item("minecraft:cobblestone"));
        let cut = |input: u16, result: &str| Stonecut {
            input: Box::new([input]),
            result: Some(ItemStack::new(item(result), 1)),
        };
        let list = vec![
            cut(stone, "minecraft:stone_slab"),
            cut(cobble, "minecraft:cobblestone_slab"),
            cut(stone, "minecraft:stone_stairs"),
        ];
        let items = crate::Version::latest().palette.items();
        let mut recipes = Recipes::default();
        let body = recipes.set_stonecutter(items, list);
        // No shaped recipes, three shapeless ones.
        assert_eq!(body[..2], [0, 3]);
        assert_eq!(recipes.stonecutter_button(1, stone), Some(0));
        assert_eq!(recipes.stonecutter_button(2, cobble), Some(0));
        assert_eq!(recipes.stonecutter_button(3, stone), Some(1));
        assert_eq!(recipes.stonecutter_button(3, cobble), None);
        assert_eq!(recipes.java(3), None);
        assert!(recipes.result(3).is_some());

        // The whole book sends them again, with the trim recipe.
        let netherite = RecipeDisplayEntry {
            id: 4,
            display: RecipeDisplay::Smithing {
                template: SlotDisplay::Item(item("minecraft:netherite_upgrade_smithing_template")),
                base: SlotDisplay::Item(item("minecraft:diamond_sword")),
                addition: SlotDisplay::Item(item("minecraft:netherite_ingot")),
                result: SlotDisplay::ItemStack(ItemStack::new(
                    item("minecraft:netherite_sword"),
                    1,
                )),
                station: SlotDisplay::Item(item("minecraft:smithing_table")),
            },
            group: None,
            category: 0,
            requirements: None,
        };
        let body = recipes.add(items, &[(netherite, 0)], true).unwrap();
        assert_eq!(body[..2], [0, 3]);
        assert_eq!(recipes.java(1), Some(4));
        assert_eq!(recipes.stonecutter_button(4, stone), Some(1));
        assert_eq!(
            recipes.result(5),
            None,
            "the trim recipe takes the Java result"
        );
        // Item tag descriptors carry an aux value before the count (#182: without it the
        // client read the count -1 and rejected `crafting_data`).
        let mut tag = b"\x18minecraft:trim_templates".to_vec();
        tag.extend_from_slice(&[0xfe, 0xff, 0x03, 0x02]);
        assert!(body.windows(tag.len()).any(|w| w == tag));
    }

    #[test]
    fn book_entries_become_bedrock_recipes() {
        let planks = [item("minecraft:oak_planks"), item("minecraft:birch_planks")];
        let table = RecipeDisplayEntry {
            id: 7,
            display: RecipeDisplay::Shaped {
                width: 2,
                height: 2,
                ingredients: vec![SlotDisplay::Tag(planks.into()); 4],
                result: SlotDisplay::ItemStack(ItemStack::new(item("minecraft:crafting_table"), 1)),
                station: SlotDisplay::Item(item("minecraft:crafting_table")),
            },
            group: None,
            category: 0,
            requirements: None,
        };
        let planks_recipe = RecipeDisplayEntry {
            id: 9,
            display: RecipeDisplay::Shapeless {
                ingredients: vec![SlotDisplay::Item(item("minecraft:oak_log"))],
                result: SlotDisplay::ItemStack(ItemStack::new(planks[0], 4)),
                station: SlotDisplay::Item(item("minecraft:crafting_table")),
            },
            group: None,
            category: 0,
            requirements: None,
        };
        // Through the Java wire format, as the frontend receives it.
        let book = RecipeBookAdd {
            entries: vec![(table, 0), (planks_recipe, 0)],
            replace: true,
        };
        let mut wire = Vec::new();
        book.encode(&mut wire);
        let decoded = RecipeBookAdd::decode(&mut Reader::new(&wire)).unwrap();
        assert_eq!(decoded, book);

        let items = crate::Version::latest().palette.items();
        let mut recipes = Recipes::default();
        let body = recipes.add(items, &decoded.entries, true).unwrap();
        // One shaped recipe per plank type, then the shapeless one.
        assert_eq!(body[0], 2);
        assert_eq!(recipes.java(1), Some(7));
        assert_eq!(recipes.java(2), Some(7));
        assert_eq!(recipes.java(3), Some(9));
        assert_eq!(recipes.result(3).map(|r| r.count), Some(4));
        assert_eq!(recipes.java(4), None);
        // The multi recipes (here the firework rocket's UUID, both halves little-endian).
        let firework = [0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0];
        assert!(body.windows(16).any(|w| w == firework));
        assert_eq!(
            *body.last().unwrap(),
            1,
            "the whole book replaces the client's recipes"
        );
    }
}
