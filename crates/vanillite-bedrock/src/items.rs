//! Java items <-> Bedrock items (generated `items.bin` per palette, see `data/README.md`) and
//! the Bedrock item stack formats.

use vanillite_data::item::ItemId;
use vanillite_protocol::item::ItemStack;

use vanillite_protocol::components::ids;

use crate::io::{self, Reader, Result};

/// The entries of a little-endian NBT compound (Bedrock item user data), without the end tag.
#[derive(Debug, Default)]
struct Nbt(Vec<u8>);

/// Bedrock NBT strings are length-prefixed with a `u16`.
fn nbt_str(out: &mut Vec<u8>, s: &str) {
    let mut end = s.len().min(usize::from(u16::MAX));
    while !s.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    let s = s.get(..end).unwrap_or("");
    out.extend_from_slice(&u16::try_from(s.len()).unwrap_or(0).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

impl Nbt {
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn head(&mut self, kind: u8, name: &str) {
        self.0.push(kind);
        nbt_str(&mut self.0, name);
    }

    fn byte(&mut self, name: &str, v: u8) {
        self.head(1, name);
        self.0.push(v);
    }

    fn int(&mut self, name: &str, v: i32) {
        self.head(3, name);
        self.0.extend_from_slice(&v.to_le_bytes());
    }

    fn long(&mut self, name: &str, v: i64) {
        self.head(4, name);
        self.0.extend_from_slice(&v.to_le_bytes());
    }

    fn string(&mut self, name: &str, v: &str) {
        self.head(8, name);
        nbt_str(&mut self.0, v);
    }

    /// `pages`: a list of `{photoname: "", text}` compounds.
    fn pages<'a>(&mut self, pages: impl ExactSizeIterator<Item = String> + 'a) {
        self.head(9, "pages");
        self.0.push(10);
        self.0
            .extend_from_slice(&i32::try_from(pages.len()).unwrap_or(0).to_le_bytes());
        for text in pages {
            let mut page = Nbt::default();
            page.string("photoname", "");
            page.string("text", &text);
            self.0.extend_from_slice(&page.0);
            self.0.push(0);
        }
    }
}

/// Most pages a book shows on Bedrock (Java written books hold at most 100 pages too).
const MAX_PAGES: usize = 100;

/// Reads a `Filterable<String>` and returns the raw string.
fn filterable_string(r: &mut vanillite_protocol::codec::Reader<'_>) -> Option<String> {
    let raw = r.string(32767).ok()?.to_owned();
    if r.u8().ok()? != 0 {
        r.string(32767).ok()?;
    }
    Some(raw)
}

/// The pages of `writable_book_content` (raw text).
pub fn writable_pages(stack: &ItemStack) -> Vec<String> {
    let Some(value) = stack.component(ids::WRITABLE_BOOK_CONTENT) else {
        return Vec::new();
    };
    let mut r = vanillite_protocol::codec::Reader::new(value);
    let Ok(n) = r.len_prefix("book pages", MAX_PAGES, 2) else {
        return Vec::new();
    };
    (0..n).map_while(|_| filterable_string(&mut r)).collect()
}

/// The Bedrock book tags of a writable or written book.
fn book_tags(nbt: &mut Nbt, stack: &ItemStack) {
    use vanillite_protocol::codec::DecodeLimited;
    use vanillite_protocol::text::Component;
    if stack.component(ids::WRITABLE_BOOK_CONTENT).is_some() {
        nbt.pages(writable_pages(stack).into_iter());
        return;
    }
    let Some(value) = stack.component(ids::WRITTEN_BOOK_CONTENT) else {
        return;
    };
    // `WrittenBookContent`: filterable title, author, generation, filterable component pages.
    let mut r = vanillite_protocol::codec::Reader::new(value);
    let (Some(title), Ok(author), Ok(generation)) = (
        filterable_string(&mut r),
        r.string(32767).map(str::to_owned),
        r.varint(),
    ) else {
        return;
    };
    let Ok(n) = r.len_prefix("book pages", 1 << 16, 2) else {
        return;
    };
    let mut pages = Vec::with_capacity(n.min(MAX_PAGES));
    for _ in 0..n {
        let Ok(page) = Component::decode_limited(&mut r, 2_097_152) else {
            break;
        };
        if r.u8().is_ok_and(|f| f != 0) && Component::decode_limited(&mut r, 2_097_152).is_err() {
            break;
        }
        if pages.len() < MAX_PAGES {
            pages.push(page.plain());
        }
    }
    nbt.pages(pages.into_iter());
    nbt.string("title", &title);
    nbt.string("author", &author);
    nbt.int("generation", generation);
    nbt.string("xuid", "");
}

/// The item tables of one palette ([`crate::data::Palette::items`]).
#[derive(Debug)]
pub struct Items {
    /// Bedrock item id and metadata per Java item id.
    to_bedrock: Vec<(i16, u16)>,
    /// Java item per creative item network id - 1.
    creative: Vec<u16>,
    /// Bedrock block runtime id per Java item (0 for items that place no block).
    block: Vec<u32>,
    /// `minecraft:damage` component type id.
    damage_component: Option<u16>,
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    b.get(i..i.saturating_add(4))
        .and_then(|b| b.try_into().ok())
        .map_or(0, u32::from_le_bytes)
}

impl Items {
    /// Parses `items.bin` and `creative_java.bin` of a palette.
    pub(crate) fn parse(items: &[u8], creative_java: &[u8], blocks: &crate::data::Blocks) -> Items {
        let count = usize::try_from(u32_at(items, 0)).unwrap_or(0);
        let to_bedrock: Vec<(i16, u16)> = items
            .get(4..)
            .unwrap_or_default()
            .as_chunks::<4>()
            .0
            .iter()
            .take(count)
            .map(|c| {
                (
                    i16::from_le_bytes([c[0], c[1]]),
                    u16::from_le_bytes([c[2], c[3]]),
                )
            })
            .collect();
        let n = usize::try_from(u32_at(creative_java, 0)).unwrap_or(0);
        let creative = creative_java
            .get(4..)
            .unwrap_or_default()
            .as_chunks::<2>()
            .0
            .iter()
            .take(n)
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        let block = (0..to_bedrock.len())
            .map(|java| {
                u16::try_from(java)
                    .ok()
                    .filter(|&j| usize::from(j) < vanillite_data::item::ITEMS.len())
                    .and_then(|j| ItemId(j).block())
                    .map_or(0, |b| {
                        let e = blocks.entry(u32::from(b.default_state().0));
                        u32::from(e & !crate::data::WATERLOGGED)
                    })
            })
            .collect();
        let damage_component =
            vanillite_data::registry::Registry::by_name("minecraft:data_component_type")
                .and_then(|r| r.id("minecraft:damage"));
        Items {
            to_bedrock,
            creative,
            block,
            damage_component,
        }
    }

    /// Bedrock item id and metadata of a Java item (0 = air for unknown items).
    #[inline]
    pub fn bedrock(&self, java: u16) -> (i16, u16) {
        self.to_bedrock
            .get(usize::from(java))
            .copied()
            .unwrap_or((0, 0))
    }

    /// The Java item of a creative item network id.
    pub fn creative(&self, net_id: u32) -> Option<u16> {
        let i = usize::try_from(net_id.checked_sub(1)?).ok()?;
        self.creative.get(i).copied()
    }

    /// Number of Java items.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.to_bedrock.len()
    }

    /// Damage of a stack (`minecraft:damage`).
    fn damage(&self, stack: &ItemStack) -> Option<i32> {
        let raw = stack.component(self.damage_component?)?;
        vanillite_protocol::codec::Reader::new(raw).varint().ok()
    }

    /// The user data of a stack: NBT with the damage of tools, the `map_uuid` of filled maps
    /// and the pages, title and author of books.
    fn write_user_data(&self, out: &mut Vec<u8>, stack: &ItemStack) {
        let mut nbt = Nbt::default();
        if let Some(d) = self.damage(stack).filter(|&d| d > 0) {
            nbt.int("Damage", d);
        }
        book_tags(&mut nbt, stack);
        if let Some(id) = stack
            .component(ids::MAP_ID)
            .and_then(|v| vanillite_protocol::codec::Reader::new(v).varint().ok())
        {
            nbt.long("map_uuid", i64::from(id));
            nbt.int("map_name_index", id);
            nbt.byte("map_display_players", 1);
        }
        if nbt.is_empty() {
            io::write_bytes(out, &[0; 10]);
            return;
        }
        let mut user = Vec::with_capacity(nbt.0.len().saturating_add(16));
        user.extend_from_slice(&(-1i16).to_le_bytes());
        user.push(1);
        user.extend_from_slice(&[0x0a, 0, 0]);
        user.extend_from_slice(&nbt.0);
        user.push(0);
        user.extend_from_slice(&[0; 8]);
        io::write_bytes(out, &user);
    }

    /// Bedrock id, metadata and block runtime id of a stack; `None` for empty stacks.
    fn parts(&self, stack: &ItemStack) -> Option<(i16, u16, u32, u16)> {
        if stack.is_empty() {
            return None;
        }
        let (id, meta) = self.bedrock(stack.item);
        if id == 0 {
            return None;
        }
        let block = self
            .block
            .get(usize::from(stack.item))
            .copied()
            .unwrap_or(0);
        let count = u16::try_from(stack.count.clamp(0, 255)).unwrap_or(0);
        Some((id, meta, block, count))
    }

    /// Writes a stack as `ItemInstance` (inventories, equipment) with its stack network id.
    pub fn write_instance(&self, out: &mut Vec<u8>, stack: &ItemStack, net_id: i32) {
        let Some((id, meta, block, count)) = self.parts(stack) else {
            out.extend_from_slice(&[0; 8]);
            return;
        };
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        io::write_varu32(out, u32::from(meta));
        if net_id != 0 {
            out.push(1);
            io::write_vari32(out, net_id);
        } else {
            out.push(0);
        }
        io::write_varu32(out, block);
        self.write_user_data(out, stack);
    }
}

/// An `ItemInstance` as read from the client (user data skipped).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Instance {
    pub id: i16,
    pub count: u16,
    pub meta: u32,
    pub net_id: i32,
}

/// Reads an `ItemInstance`.
pub fn read_instance(r: &mut Reader<'_>) -> Result<Instance> {
    let [a, b] = r.array::<2>()?;
    let id = i16::from_le_bytes([a, b]);
    if id == 0 {
        // Air: count, metadata, no stack id, block runtime id, empty user data.
        let _ = r.array::<2>()?;
        r.varu32()?;
        if r.bool()? {
            r.vari32()?;
        }
        r.varu32()?;
        r.bytes("item user data", 0)?;
        return Ok(Instance::default());
    }
    let count = u16::from_le_bytes(r.array::<2>()?);
    let meta = r.varu32()?;
    let net_id = if r.bool()? { r.vari32()? } else { 0 };
    let _block = r.varu32()?;
    r.bytes("item user data", 1 << 16)?;
    Ok(Instance {
        id,
        count,
        meta,
        net_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_java_item_maps_to_a_bedrock_item() {
        for palette in [&crate::data::V26_40, &crate::data::V26_50] {
            let items = palette.items();
            for java in 1..items.len() {
                let java = u16::try_from(java).unwrap_or(0);
                assert_ne!(items.bedrock(java).0, 0, "{}", ItemId(java).name());
            }
        }
        let items = crate::data::V26_60.items();
        assert_eq!(items.len(), vanillite_data::item::ITEMS.len());
        for java in 1..items.len() {
            let java = u16::try_from(java).unwrap_or(0);
            let (id, _) = items.bedrock(java);
            assert_ne!(id, 0, "{} has no Bedrock item", ItemId(java).name());
        }
        // Round trips of plain items, block items carry their block.
        let stone = ItemId::by_name("minecraft:stone").map_or(0, |i| i.0);
        let (id, _) = items.bedrock(stone);
        assert_ne!(items.block.get(usize::from(stone)), Some(&0));
        assert!(!items.creative.is_empty());
        assert!(items.creative(1).is_some());
        let mut out = Vec::new();
        items.write_instance(&mut out, &ItemStack::new(stone, 5), 7);
        let i = read_instance(&mut Reader::new(&out)).unwrap_or_default();
        assert_eq!((i.id, i.count, i.net_id), (id, 5, 7));
    }
}
