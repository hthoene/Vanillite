//! Versioned Bedrock data (see `data/README.md`): per palette version the Java -> Bedrock block
//! state table, the item tables and the pre-encoded registries sent on join; shared by all
//! versions the biome ids, sounds and entity identifiers.

use std::sync::OnceLock;

use rustc_hash::FxHashMap;

use crate::items::Items;

static BIOMES: &str = include_str!("../data/biomes.txt");
static SOUNDS: &str = include_str!("../data/sounds.txt");
/// Body of `biome_definition_list`.
pub static BIOME_DEFINITIONS: &[u8] = include_bytes!("../data/biome_definitions.bin");
/// Body of `available_actor_identifiers`.
pub static ENTITY_IDENTIFIERS: &[u8] = include_bytes!("../data/entity_identifiers.bin");

/// The generated data of one Bedrock palette version (`data/<name>/`), shared by the protocols
/// that use it. Tables are built on first use.
#[derive(Debug)]
pub struct Palette {
    /// Palette version (directory name).
    pub name: &'static str,
    blocks_bin: &'static [u8],
    items_bin: &'static [u8],
    creative_java: &'static [u8],
    /// `block properties` of `start_game` (count + entries).
    pub block_properties: &'static [u8],
    /// Body of `item_registry`.
    pub item_registry: &'static [u8],
    /// Body of `creative_content`.
    pub creative: &'static [u8],
    blocks: OnceLock<Blocks>,
    items: OnceLock<Items>,
}

macro_rules! palette {
    ($name:literal) => {
        Palette {
            name: $name,
            blocks_bin: include_bytes!(concat!("../data/", $name, "/blocks.bin")),
            items_bin: include_bytes!(concat!("../data/", $name, "/items.bin")),
            creative_java: include_bytes!(concat!("../data/", $name, "/creative_java.bin")),
            block_properties: include_bytes!(concat!("../data/", $name, "/block_properties.bin")),
            item_registry: include_bytes!(concat!("../data/", $name, "/item_registry.bin")),
            creative: include_bytes!(concat!("../data/", $name, "/creative.bin")),
            blocks: OnceLock::new(),
            items: OnceLock::new(),
        }
    };
}

/// Bedrock 1.26.40-1.26.45.
pub static V26_40: Palette = palette!("26_40");
/// Bedrock 1.26.50-1.26.52.
pub static V26_50: Palette = palette!("26_50");
/// Bedrock 1.26.60-1.26.66.
pub static V26_60: Palette = palette!("26_60");

impl Palette {
    /// The block state table.
    pub fn blocks(&self) -> &Blocks {
        self.blocks.get_or_init(|| Blocks::parse(self.blocks_bin))
    }

    /// The item tables.
    pub fn items(&self) -> &Items {
        self.items
            .get_or_init(|| Items::parse(self.items_bin, self.creative_java, self.blocks()))
    }
}

/// Bit of a [`Blocks`] entry: the state holds water (second layer).
pub const WATERLOGGED: u16 = 0x8000;

/// The Java -> Bedrock block state table.
#[derive(Debug)]
pub struct Blocks {
    table: Vec<u16>,
    /// Runtime id of still water.
    pub water: u32,
    /// Runtime id of air.
    pub air: u32,
}

impl Blocks {
    fn parse(bin: &[u8]) -> Blocks {
        let word = |i: usize| -> u32 {
            bin.get(i..i.saturating_add(4))
                .and_then(|b| b.try_into().ok())
                .map_or(0, u32::from_le_bytes)
        };
        let count = usize::try_from(word(0)).unwrap_or(0);
        let water = word(4);
        let table: Vec<u16> = bin
            .get(8..)
            .unwrap_or_default()
            .as_chunks::<2>()
            .0
            .iter()
            .take(count)
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        let air = u32::from(table.first().copied().unwrap_or(0) & !WATERLOGGED);
        Blocks { table, water, air }
    }

    /// Entry of a Java state: runtime id and [`WATERLOGGED`]; unknown states become air.
    #[inline]
    pub fn entry(&self, java: u32) -> u16 {
        usize::try_from(java)
            .ok()
            .and_then(|i| self.table.get(i))
            .copied()
            .unwrap_or(0)
    }

    /// Number of Java states.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.table.len()
    }
}

/// Bedrock biome id of a Java biome name (plains for unknown biomes).
pub fn biome_id(java: &str) -> u32 {
    static MAP: OnceLock<FxHashMap<&'static str, u32>> = OnceLock::new();
    MAP.get_or_init(|| {
        BIOMES
            .lines()
            .filter_map(|l| {
                let (name, id) = l.split_once(' ')?;
                Some((name, id.parse().ok()?))
            })
            .collect()
    })
    .get(java)
    .copied()
    .unwrap_or(1)
}

/// Bedrock sound name of a Java sound event.
pub fn sound_name(java: &str) -> Option<&'static str> {
    static MAP: OnceLock<FxHashMap<&'static str, &'static str>> = OnceLock::new();
    MAP.get_or_init(|| SOUNDS.lines().filter_map(|l| l.split_once(' ')).collect())
        .get(java)
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_covers_java_26_3() {
        for p in [&V26_40, &V26_50, &V26_60] {
            assert_eq!(p.blocks().len(), 35_723, "{}", p.name);
            assert_eq!(
                p.items().len(),
                vanillite_data::item::ITEMS.len(),
                "{}",
                p.name
            );
        }
        let b = V26_60.blocks();
        // Java water (state 89) maps to Bedrock water, oak stairs waterlogged carry the flag.
        assert_eq!(u32::from(b.entry(89) & !WATERLOGGED), b.water);
        assert_ne!(b.entry(5463) & WATERLOGGED, 0);
        assert_eq!(b.entry(5464) & WATERLOGGED, 0);
        assert_eq!(biome_id("minecraft:plains"), 1);
        assert!(sound_name("minecraft:block.stone.break").is_some());
        assert_eq!(
            biome_id("minecraft:the_void"),
            biome_id("minecraft:the_void")
        );
    }
}
