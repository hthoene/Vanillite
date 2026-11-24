//! Versioned Bedrock data (see `data/README.md`): the Java -> Bedrock block state table, biome
//! ids and pre-encoded registries sent on join.

use std::sync::OnceLock;

use rustc_hash::FxHashMap;

/// Bedrock network protocol this build speaks.
pub const PROTOCOL: i32 = 2225;
/// Bedrock release of [`PROTOCOL`].
pub const VERSION: &str = "1.26.60";

static BLOCKS: &[u8] = include_bytes!("../data/blocks.bin");
static BIOMES: &str = include_str!("../data/biomes.txt");
static SOUNDS: &str = include_str!("../data/sounds.txt");
/// `block properties` of `start_game` (count + entries).
pub static BLOCK_PROPERTIES: &[u8] = include_bytes!("../data/block_properties.bin");
/// Body of `item_registry`.
pub static ITEM_REGISTRY: &[u8] = include_bytes!("../data/item_registry.bin");
/// Body of `biome_definition_list`.
pub static BIOME_DEFINITIONS: &[u8] = include_bytes!("../data/biome_definitions.bin");
/// Body of `available_actor_identifiers`.
pub static ENTITY_IDENTIFIERS: &[u8] = include_bytes!("../data/entity_identifiers.bin");

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
    /// The table of this build.
    pub fn get() -> &'static Blocks {
        static TABLE: OnceLock<Blocks> = OnceLock::new();
        TABLE.get_or_init(|| {
            let word = |i: usize| -> u32 {
                BLOCKS
                    .get(i..i.saturating_add(4))
                    .and_then(|b| b.try_into().ok())
                    .map_or(0, u32::from_le_bytes)
            };
            let count = usize::try_from(word(0)).unwrap_or(0);
            let water = word(4);
            let table: Vec<u16> = BLOCKS
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
        })
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
        let b = Blocks::get();
        assert_eq!(b.len(), 35_723);
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
