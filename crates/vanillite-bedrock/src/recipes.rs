//! Java recipe book displays as Bedrock crafting recipes (`crafting_data`).
//!
//! Bedrock only shows a crafting result for recipes it knows, so the shaped and shapeless
//! displays of the Java recipe book (`recipe_book_add`) are sent as Bedrock recipes. A Bedrock
//! ingredient names one item: an ingredient with several items (a tag) becomes one recipe per
//! item, with all slots of the same item set taking the same item. The Java server still decides
//! what a grid crafts; a Bedrock recipe network id leads back to its Java `RecipeDisplayId` for
//! the recipe book (`place_recipe`).

use vanillite_protocol::item::ItemStack;
use vanillite_protocol::recipe_display::{RecipeDisplay, RecipeDisplayEntry, SlotDisplay};

use crate::io;
use crate::items::Items;

/// Most Bedrock recipes one Java display becomes.
const MAX_VARIANTS: usize = 32;
/// Most Bedrock recipes of one session.
const MAX_RECIPES: usize = 1 << 16;

/// The Bedrock recipes sent to one client.
#[derive(Debug, Default)]
pub struct Recipes {
    /// Java `RecipeDisplayId` and result per Bedrock recipe network id - 1.
    java: Vec<(i32, ItemStack)>,
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
fn write_tail(out: &mut Vec<u8>, items: &Items, result: &ItemStack, shaped: bool, net_id: u32) {
    io::write_varu32(out, 1);
    items.write_stack(out, result);
    out.extend_from_slice(&[0; 16]); // uuid
    io::write_str(out, "crafting_table");
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
    /// The Java `RecipeDisplayId` of a Bedrock recipe network id.
    pub fn java(&self, net_id: u32) -> Option<i32> {
        let i = usize::try_from(net_id.checked_sub(1)?).ok()?;
        self.java.get(i).map(|r| r.0)
    }

    /// The result of a Bedrock recipe network id.
    pub fn result(&self, net_id: u32) -> Option<ItemStack> {
        let i = usize::try_from(net_id.checked_sub(1)?).ok()?;
        self.java.get(i).map(|r| r.1.clone())
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
        let (mut n_shaped, mut n_shapeless) = (0usize, 0usize);
        for (entry, _) in entries {
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
                self.java.push((entry.id, result.clone()));
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
                write_tail(out, items, &result, width.is_some(), net_id);
                *n = n.saturating_add(1);
            }
        }
        if n_shaped == 0 && n_shapeless == 0 && !replace {
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
        // Multi, user data shapeless, chemistry shapeless and shaped, smithing transform and
        // trim, potion and potion container recipes, material reducers: none.
        body.extend_from_slice(&[0; 9]);
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
        assert_eq!(
            *body.last().unwrap(),
            1,
            "the whole book replaces the client's recipes"
        );
    }
}
