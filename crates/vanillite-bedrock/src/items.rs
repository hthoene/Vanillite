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

    fn short(&mut self, name: &str, v: i16) {
        self.head(2, name);
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

    /// `display`: the shown name and lore lines.
    fn display(&mut self, name: Option<&str>, lore: &[String]) {
        self.head(10, "display");
        let mut inner = Nbt::default();
        if let Some(name) = name {
            inner.string("Name", name);
        }
        if !lore.is_empty() {
            inner.head(9, "Lore");
            inner.0.push(8);
            inner
                .0
                .extend_from_slice(&i32::try_from(lore.len()).unwrap_or(0).to_le_bytes());
            for line in lore {
                nbt_str(&mut inner.0, line);
            }
        }
        self.0.extend_from_slice(&inner.0);
        self.0.push(0);
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

/// Bedrock enchantment ids by name (`Enchantment` ids of the Bedrock protocol).
const BEDROCK_ENCHANTMENTS: &[&str] = &[
    "protection",
    "fire_protection",
    "feather_falling",
    "blast_protection",
    "projectile_protection",
    "thorns",
    "respiration",
    "depth_strider",
    "aqua_affinity",
    "sharpness",
    "smite",
    "bane_of_arthropods",
    "knockback",
    "fire_aspect",
    "looting",
    "efficiency",
    "silk_touch",
    "unbreaking",
    "fortune",
    "power",
    "punch",
    "flame",
    "infinity",
    "luck_of_the_sea",
    "lure",
    "frost_walker",
    "mending",
    "binding_curse",
    "vanishing_curse",
    "impaling",
    "riptide",
    "loyalty",
    "channeling",
    "multishot",
    "piercing",
    "quick_charge",
    "soul_speed",
    "swift_sneak",
    "wind_burst",
    "density",
    "breach",
    "lunge",
];

/// The Bedrock enchantment id of a Java enchantment (`minecraft:sharpness`).
pub fn bedrock_enchantment(java: &str) -> Option<u8> {
    let name = java.strip_prefix("minecraft:")?;
    BEDROCK_ENCHANTMENTS
        .iter()
        .position(|&n| n == name)
        .and_then(|i| u8::try_from(i).ok())
}

/// Java enchantment registry ids -> Bedrock enchantment ids, from the synchronised
/// `minecraft:enchantment` registry of the server (data packs may add or reorder entries).
/// Cheap to clone.
#[derive(Debug, Default, Clone)]
pub struct Enchantments(std::sync::Arc<[Option<u8>]>);

impl Enchantments {
    pub fn from_names(names: &[String]) -> Self {
        Self(names.iter().map(|n| bedrock_enchantment(n)).collect())
    }

    /// The Bedrock id of a Java enchantment registry id.
    pub fn bedrock(&self, java: i32) -> Option<u8> {
        self.0.get(usize::try_from(java).ok()?).copied().flatten()
    }

    /// The Java registry id of a Bedrock enchantment id.
    pub fn java(&self, bedrock: u8) -> Option<i32> {
        let i = self.0.iter().position(|&b| b == Some(bedrock))?;
        i32::try_from(i).ok()
    }
}

/// Bedrock goat horn metadata -> Java instrument.
const GOAT_HORNS: [&str; 8] = [
    "minecraft:ponder_goat_horn",
    "minecraft:sing_goat_horn",
    "minecraft:seek_goat_horn",
    "minecraft:feel_goat_horn",
    "minecraft:admire_goat_horn",
    "minecraft:call_goat_horn",
    "minecraft:yearn_goat_horn",
    "minecraft:dream_goat_horn",
];

/// The Java components of a creative item variant (`creative_java.bin`, kind and data).
fn creative_components(
    stack: &mut ItemStack,
    kind: u8,
    data: &[u8],
    enchantments: &Enchantments,
) -> Option<()> {
    use vanillite_protocol::codec::write_varint;
    use vanillite_protocol::components_nbt::RegistryNames;
    let mut v = Vec::with_capacity(8);
    let first = i32::from(*data.first()?);
    let component = match kind {
        1 => {
            let name = BEDROCK_POTIONS.get(usize::try_from(first).ok()?)?;
            let id = vanillite_data::registry::POTION.id(&format!("minecraft:{name}"))?;
            v.push(1);
            write_varint(&mut v, i32::from(id));
            v.extend_from_slice(&[0, 0, 0]);
            ids::POTION_CONTENTS
        }
        2 => {
            let pairs: Vec<(i32, i32)> = data
                .get(1..)?
                .as_chunks::<2>()
                .0
                .iter()
                .filter_map(|&[e, l]| Some((enchantments.java(e)?, i32::from(l))))
                .collect();
            write_varint(&mut v, i32::try_from(pairs.len()).ok()?);
            for (e, l) in pairs {
                write_varint(&mut v, e);
                write_varint(&mut v, l);
            }
            ids::STORED_ENCHANTMENTS
        }
        3 => {
            write_varint(&mut v, first);
            v.push(0);
            ids::FIREWORKS
        }
        4 => {
            let name = GOAT_HORNS.get(usize::try_from(first).ok()?)?;
            let id = vanillite_protocol::item::InstalledRegistries
                .entry_id("minecraft:instrument", name)?;
            write_varint(&mut v, i32::try_from(id).ok()?.checked_add(1)?);
            ids::INSTRUMENT
        }
        5 => {
            write_varint(&mut v, first);
            ids::OMINOUS_BOTTLE_AMPLIFIER
        }
        _ => return None,
    };
    stack.patch_mut().set(component, v.into_boxed_slice());
    Some(())
}

/// The Bedrock goat horn metadata of a stack's `instrument`.
fn goat_horn(stack: &ItemStack) -> Option<u16> {
    use vanillite_protocol::components_nbt::RegistryNames;
    let mut r = vanillite_protocol::codec::Reader::new(stack.component(ids::INSTRUMENT)?);
    let id = usize::try_from(r.varint().ok()?.checked_sub(1)?).ok()?;
    let name =
        vanillite_protocol::item::InstalledRegistries.entry_name("minecraft:instrument", id)?;
    let i = GOAT_HORNS.iter().position(|&n| n == name)?;
    u16::try_from(i).ok()
}

/// Bedrock potion ids (item metadata of potions; tipped arrows add one) by Java potion name.
const BEDROCK_POTIONS: &[&str] = &[
    "water",
    "mundane",
    "long_mundane",
    "thick",
    "awkward",
    "night_vision",
    "long_night_vision",
    "invisibility",
    "long_invisibility",
    "leaping",
    "long_leaping",
    "strong_leaping",
    "fire_resistance",
    "long_fire_resistance",
    "swiftness",
    "long_swiftness",
    "strong_swiftness",
    "slowness",
    "long_slowness",
    "water_breathing",
    "long_water_breathing",
    "healing",
    "strong_healing",
    "harming",
    "strong_harming",
    "poison",
    "long_poison",
    "strong_poison",
    "regeneration",
    "long_regeneration",
    "strong_regeneration",
    "strength",
    "long_strength",
    "strong_strength",
    "weakness",
    "long_weakness",
    "wither",
    "turtle_master",
    "long_turtle_master",
    "strong_turtle_master",
    "slow_falling",
    "long_slow_falling",
    "strong_slowness",
    "wind_charged",
    "weaving",
    "oozing",
    "infested",
];

/// The Bedrock potion id of a stack's `potion_contents` (`None`: no base potion).
fn potion(stack: &ItemStack) -> Option<u16> {
    let mut r = vanillite_protocol::codec::Reader::new(stack.component(ids::POTION_CONTENTS)?);
    if r.u8().ok()? == 0 {
        return None;
    }
    let id = u16::try_from(r.varint().ok()?).ok()?;
    let name = vanillite_data::registry::POTION
        .name(id)?
        .strip_prefix("minecraft:")?;
    let i = BEDROCK_POTIONS.iter().position(|&n| n == name)?;
    u16::try_from(i).ok()
}

/// Most enchantments written per item.
const MAX_ENCHANTMENTS: usize = 64;

/// `ench`: the enchantments (or stored enchantments of a book) of a stack.
fn enchantment_tags(nbt: &mut Nbt, stack: &ItemStack, enchantments: &Enchantments) {
    let Some(raw) = stack
        .component(ids::ENCHANTMENTS)
        .filter(|v| v.first().is_some_and(|&n| n != 0))
        .or_else(|| stack.component(ids::STORED_ENCHANTMENTS))
    else {
        return;
    };
    let mut r = vanillite_protocol::codec::Reader::new(raw);
    let Ok(n) = r.len_prefix("enchantments", MAX_ENCHANTMENTS, 2) else {
        return;
    };
    let mut list = Vec::with_capacity(n);
    for _ in 0..n {
        let (Ok(id), Ok(level)) = (r.varint(), r.varint()) else {
            break;
        };
        if let Some(b) = enchantments.bedrock(id) {
            list.push((b, i16::try_from(level).unwrap_or(i16::MAX)));
        }
    }
    if list.is_empty() {
        return;
    }
    nbt.head(9, "ench");
    nbt.0.push(10);
    nbt.0
        .extend_from_slice(&i32::try_from(list.len()).unwrap_or(0).to_le_bytes());
    for (id, lvl) in list {
        let mut e = Nbt::default();
        e.short("id", i16::from(id));
        e.short("lvl", lvl);
        nbt.0.extend_from_slice(&e.0);
        nbt.0.push(0);
    }
}

/// Most lore lines shown (Java allows 256).
const MAX_LORE: usize = 64;

/// The custom name and lore of a stack as Bedrock `display` tags.
fn display_tags(nbt: &mut Nbt, stack: &ItemStack, lore_component: Option<u16>) {
    use vanillite_protocol::codec::DecodeLimited;
    use vanillite_protocol::text::Component;
    let name = stack
        .component(ids::CUSTOM_NAME)
        .and_then(|v| {
            Component::decode_limited(&mut vanillite_protocol::codec::Reader::new(v), 1 << 18).ok()
        })
        .map(|c| c.plain());
    let mut lore = Vec::new();
    if let Some(v) = lore_component.and_then(|c| stack.component(c)) {
        let mut r = vanillite_protocol::codec::Reader::new(v);
        if let Ok(n) = r.len_prefix("lore", 256, 1) {
            for _ in 0..n.min(MAX_LORE) {
                match Component::decode_limited(&mut r, 1 << 18) {
                    Ok(line) => lore.push(line.plain()),
                    Err(_) => break,
                }
            }
        }
    }
    if name.is_some() || !lore.is_empty() {
        nbt.display(name.as_deref(), &lore);
    }
}

/// The Bedrock book tags of a writable or written book.
fn book_tags(nbt: &mut Nbt, stack: &ItemStack) {
    let Some((pages, written)) = book_content(stack) else {
        return;
    };
    nbt.pages(pages.into_iter());
    if let Some((title, author, generation)) = written {
        nbt.string("title", &title);
        nbt.string("author", &author);
        nbt.int("generation", generation);
        nbt.string("xuid", "");
    }
}

/// The pages (plain text) of a writable or written book, with title, author and generation of
/// a written one.
pub fn book_content(stack: &ItemStack) -> Option<BookContent> {
    use vanillite_protocol::codec::DecodeLimited;
    use vanillite_protocol::text::Component;
    if stack.component(ids::WRITABLE_BOOK_CONTENT).is_some() {
        return Some((writable_pages(stack), None));
    }
    let value = stack.component(ids::WRITTEN_BOOK_CONTENT)?;
    // `WrittenBookContent`: filterable title, author, generation, filterable component pages.
    let mut r = vanillite_protocol::codec::Reader::new(value);
    let (Some(title), Ok(author), Ok(generation)) = (
        filterable_string(&mut r),
        r.string(32767).map(str::to_owned),
        r.varint(),
    ) else {
        return None;
    };
    let n = r.len_prefix("book pages", 1 << 16, 2).ok()?;
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
    Some((pages, Some((title, author, generation))))
}

/// Pages and, for written books, title, author and generation.
pub type BookContent = (Vec<String>, Option<(String, String, i32)>);

/// The item tables of one palette ([`crate::data::Palette::items`]).
#[derive(Debug)]
pub struct Items {
    /// Bedrock item id and metadata per Java item id.
    to_bedrock: Vec<(i16, u16)>,
    /// Java item per creative item network id - 1, with the kind and data of its components.
    creative: Vec<(u16, u8, Box<[u8]>)>,
    /// Bedrock block runtime id per Java item (0 for items that place no block).
    block: Vec<u32>,
    /// `minecraft:damage` component type id.
    damage_component: Option<u16>,
    /// `minecraft:lore` component type id.
    lore_component: Option<u16>,
    /// Bedrock item names by Bedrock item id (`item_registry`).
    names: rustc_hash::FxHashMap<i16, Box<str>>,
    /// Bedrock id of the shield (its user data carries the blocking tick).
    shield: i16,
    /// Java tipped arrow, potion, splash potion and lingering potion: their metadata is the
    /// Bedrock potion id (plus one for arrows).
    potion_items: [u16; 4],
}

/// The Bedrock item names of an `item_registry` body.
fn registry_names(body: &[u8]) -> rustc_hash::FxHashMap<i16, Box<str>> {
    let mut r = Reader::new(body);
    let mut names = rustc_hash::FxHashMap::default();
    let Ok(n) = r.len("items", 1 << 16) else {
        return names;
    };
    for _ in 0..n {
        let entry = (|| -> Result<(Box<str>, i16)> {
            let name = Box::from(r.string("item name", 256)?);
            let id = i16::from_le_bytes(r.array::<2>()?);
            r.bool()?;
            r.vari32()?;
            crate::nbt::skip_root(&mut r)?;
            Ok((name, id))
        })();
        match entry {
            Ok((name, id)) => {
                names.insert(id, name);
            }
            Err(_) => break,
        }
    }
    names
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    b.get(i..i.saturating_add(4))
        .and_then(|b| b.try_into().ok())
        .map_or(0, u32::from_le_bytes)
}

impl Items {
    /// Parses `items.bin` and `creative_java.bin` of a palette.
    pub(crate) fn parse(
        items: &[u8],
        creative_java: &[u8],
        registry: &[u8],
        blocks: &crate::data::Blocks,
    ) -> Items {
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
        let mut creative = Vec::with_capacity(n);
        let mut at = 4usize;
        for _ in 0..n {
            let Some(&[a, b, kind, len]) = creative_java.get(at..at.saturating_add(4)) else {
                break;
            };
            let start = at.saturating_add(4);
            let end = start.saturating_add(usize::from(len));
            let data = creative_java.get(start..end).unwrap_or_default();
            creative.push((u16::from_le_bytes([a, b]), kind, Box::from(data)));
            at = end;
        }
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
        let components =
            vanillite_data::registry::Registry::by_name("minecraft:data_component_type");
        let damage_component = components.and_then(|r| r.id("minecraft:damage"));
        let lore_component = components.and_then(|r| r.id("minecraft:lore"));
        let shield = vanillite_data::registry::ITEM
            .id("minecraft:shield")
            .and_then(|j| to_bedrock.get(usize::from(j)))
            .map_or(0, |b| b.0);
        let item = |n: &str| vanillite_data::registry::ITEM.id(n).unwrap_or(u16::MAX);
        let potion_items = [
            item("minecraft:tipped_arrow"),
            item("minecraft:potion"),
            item("minecraft:splash_potion"),
            item("minecraft:lingering_potion"),
        ];
        Items {
            potion_items,
            to_bedrock,
            creative,
            block,
            damage_component,
            lore_component,
            names: registry_names(registry),
            shield,
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

    /// The Bedrock name and metadata of a Java item.
    pub fn name(&self, java: u16) -> Option<(&str, u16)> {
        let (id, meta) = self.bedrock(java);
        (id != 0).then_some(())?;
        Some((self.names.get(&id)?, meta))
    }

    /// The Java stack (count 1) of a creative item network id, with the components of its
    /// variant (potion, enchanted book, ...).
    pub fn creative(&self, net_id: u32, enchantments: &Enchantments) -> Option<ItemStack> {
        let i = usize::try_from(net_id.checked_sub(1)?).ok()?;
        let (item, kind, data) = self.creative.get(i)?;
        let mut stack = ItemStack::new(*item, 1);
        if *kind != 0 {
            creative_components(&mut stack, *kind, data, enchantments);
        }
        Some(stack)
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
    fn write_user_data(
        &self,
        out: &mut Vec<u8>,
        stack: &ItemStack,
        shield: bool,
        enchantments: &Enchantments,
    ) {
        let mut nbt = Nbt::default();
        if let Some(d) = self.damage(stack).filter(|&d| d > 0) {
            nbt.int("Damage", d);
        }
        book_tags(&mut nbt, stack);
        display_tags(&mut nbt, stack, self.lore_component);
        enchantment_tags(&mut nbt, stack, enchantments);
        if let Some(cost) = stack
            .component(ids::REPAIR_COST)
            .and_then(|v| vanillite_protocol::codec::Reader::new(v).varint().ok())
            .filter(|&c| c > 0)
        {
            nbt.int("RepairCost", cost);
        }
        if let Some(rgb) = stack
            .component(ids::DYED_COLOR)
            .and_then(|v| v.get(..4)?.try_into().ok())
            .map(i32::from_be_bytes)
        {
            nbt.int("customColor", rgb | i32::from_be_bytes([0xff, 0, 0, 0]));
        }
        if let Some(id) = stack
            .component(ids::MAP_ID)
            .and_then(|v| vanillite_protocol::codec::Reader::new(v).varint().ok())
        {
            nbt.long("map_uuid", i64::from(id));
            nbt.int("map_name_index", id);
            nbt.byte("map_display_players", 1);
        }
        if nbt.is_empty() {
            // No NBT, no can-place-on and can-break lists, and the shield's blocking tick.
            io::write_bytes(out, if shield { &[0; 18] } else { &[0; 10] });
            return;
        }
        let mut user = Vec::with_capacity(nbt.0.len().saturating_add(16));
        user.extend_from_slice(&(-1i16).to_le_bytes());
        user.push(1);
        user.extend_from_slice(&[0x0a, 0, 0]);
        user.extend_from_slice(&nbt.0);
        user.push(0);
        user.extend_from_slice(&[0; 8]);
        if shield {
            user.extend_from_slice(&[0; 8]);
        }
        io::write_bytes(out, &user);
    }

    /// Bedrock id, metadata and block runtime id of a stack; `None` for empty stacks.
    fn parts(&self, stack: &ItemStack) -> Option<(i16, u16, u32, u16)> {
        if stack.is_empty() {
            return None;
        }
        let (id, mut meta) = self.bedrock(stack.item);
        if id == 0 {
            return None;
        }
        if let Some(p) = potion(stack) {
            if stack.item == self.potion_items[0] {
                meta = p.saturating_add(1);
            } else if self.potion_items[1..].contains(&stack.item) {
                meta = p;
            }
        } else if stack.item == vanillite_data::item::ids::GOAT_HORN.0 {
            meta = goat_horn(stack).unwrap_or(meta);
        } else if stack.item == vanillite_data::item::ids::OMINOUS_BOTTLE.0
            && let Some(raw) = stack.component(ids::OMINOUS_BOTTLE_AMPLIFIER)
        {
            let amplifier = vanillite_protocol::codec::Reader::new(raw)
                .varint()
                .unwrap_or(0);
            meta = u16::try_from(amplifier.clamp(0, 4)).unwrap_or(0);
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
    pub fn write_instance(
        &self,
        out: &mut Vec<u8>,
        stack: &ItemStack,
        net_id: i32,
        enchantments: &Enchantments,
    ) {
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
        self.write_user_data(out, stack, id == self.shield, enchantments);
    }

    /// Writes a stack as `ItemStack` (recipe outputs: no stack network id).
    pub fn write_stack(&self, out: &mut Vec<u8>, stack: &ItemStack) {
        let Some((id, meta, block, count)) = self.parts(stack) else {
            // Id, count, metadata, block runtime id and user data, all zero.
            out.extend_from_slice(&[0; 6]);
            return;
        };
        io::write_vari32(out, i32::from(id));
        out.extend_from_slice(&count.to_le_bytes());
        io::write_varu32(out, u32::from(meta));
        io::write_vari32(out, i32::try_from(block).unwrap_or(0));
        self.write_user_data(out, stack, id == self.shield, &Enchantments::default());
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
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]
mod tests {
    use super::*;

    fn java(name: &str) -> u16 {
        vanillite_data::registry::ITEM.id(name).unwrap()
    }

    #[test]
    fn potions_enchantments_and_colours_reach_bedrock() {
        let items = crate::Version::latest().palette.items();
        let potion_id = |n: &str| vanillite_data::registry::POTION.id(n).unwrap();
        let with = |item: &str, component: u16, value: Vec<u8>| {
            let mut s = ItemStack::new(java(item), 1);
            s.patch_mut().set(component, value.into_boxed_slice());
            s
        };
        // potion_contents: present, the potion id, no colour, no effects, no name.
        let contents = |p: &str| vec![1, u8::try_from(potion_id(p)).unwrap(), 0, 0, 0];
        let healing = with(
            "minecraft:potion",
            ids::POTION_CONTENTS,
            contents("minecraft:strong_healing"),
        );
        assert_eq!(items.parts(&healing).map(|p| p.1), Some(22));
        let arrow = with(
            "minecraft:tipped_arrow",
            ids::POTION_CONTENTS,
            contents("minecraft:poison"),
        );
        assert_eq!(items.parts(&arrow).map(|p| p.1), Some(26));

        // Enchantments through the server's registry order.
        let names: Vec<String> = ["minecraft:aqua_affinity", "minecraft:sharpness"]
            .map(String::from)
            .to_vec();
        let ench = Enchantments::from_names(&names);
        assert_eq!(ench.bedrock(1), Some(9));
        let sword = with("minecraft:diamond_sword", ids::ENCHANTMENTS, vec![1, 1, 5]);
        let mut out = Vec::new();
        items.write_instance(&mut out, &sword, 1, &ench);
        let tag = out.windows(4).position(|w| w == b"ench").expect("ench tag");
        // List of one compound: id 9 (sharpness), lvl 5.
        assert_eq!(out[tag + 4], 10);
        assert_eq!(out[tag + 5..tag + 9], 1i32.to_le_bytes());
        let id = out.windows(2).position(|w| w == b"id").unwrap();
        assert_eq!(out[id + 2..id + 4], 9i16.to_le_bytes());

        let boots = with(
            "minecraft:leather_boots",
            ids::DYED_COLOR,
            vec![0, 0x12, 0x34, 0x56],
        );
        let mut out = Vec::new();
        items.write_instance(&mut out, &boots, 1, &ench);
        let tag = out.windows(11).position(|w| w == b"customColor").unwrap();
        assert_eq!(out[tag + 11..tag + 15], [0x56, 0x34, 0x12, 0xff]);
    }

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
        assert!(items.creative(1, &Enchantments::default()).is_some());
        // Creative potions and enchanted books carry their Java components.
        let ench = Enchantments::from_names(&["minecraft:protection".to_owned()]);
        let variants: Vec<ItemStack> = (1..=u32::try_from(items.creative.len()).unwrap_or(0))
            .filter_map(|n| items.creative(n, &ench))
            .collect();
        let healing = vanillite_data::registry::POTION
            .id("minecraft:healing")
            .unwrap();
        assert!(variants.iter().any(|s| {
            ItemId(s.item).name() == "minecraft:potion"
                && s.component(ids::POTION_CONTENTS)
                    == Some(&[1, u8::try_from(healing).unwrap(), 0, 0, 0][..])
        }));
        assert!(variants.iter().any(|s| {
            ItemId(s.item).name() == "minecraft:enchanted_book"
                && s.component(ids::STORED_ENCHANTMENTS) == Some(&[1, 0, 4][..])
        }));
        assert!(
            variants
                .iter()
                .any(|s| ItemId(s.item).name() == "minecraft:tipped_arrow")
        );
        let mut out = Vec::new();
        items.write_instance(
            &mut out,
            &ItemStack::new(stone, 5),
            7,
            &Enchantments::default(),
        );
        let i = read_instance(&mut Reader::new(&out)).unwrap_or_default();
        assert_eq!((i.id, i.count, i.net_id), (id, 5, 7));
    }
}
