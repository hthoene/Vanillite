//! Block states and block tags as world generation reads them from data packs.

use serde_json::Value;
use vanillite_data::block::{BlockId, BlockState};
use vanillite_datapack::LoadedPacks;

use crate::Error;

/// Parses a block state as `BlockState.CODEC` writes it: `"minecraft:stone"`,
/// `{"id": "minecraft:water", "properties": {"level": "0"}}` (26.3's `FULL_CODEC`) or the older
/// `{"Name": …, "Properties": {…}}`.
pub fn parse_block_state(json: &Value) -> Result<BlockState, Error> {
    let (name, properties) = match json {
        Value::String(name) => (name.as_str(), None),
        Value::Object(map) => {
            let (name_key, properties_key) = if map.contains_key("id") {
                ("id", "properties")
            } else {
                ("Name", "Properties")
            };
            (
                map.get(name_key)
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::new(format!("block state without id: {json}")))?,
                map.get(properties_key).and_then(Value::as_object),
            )
        }
        _ => return Err(Error::new(format!("invalid block state {json}"))),
    };
    let name = qualify(name);
    let block =
        BlockId::by_name(&name).ok_or_else(|| Error::new(format!("unknown block {name}")))?;
    let mut state = block.default_state();
    if let Some(properties) = properties {
        for (key, value) in properties {
            let value = value
                .as_str()
                .ok_or_else(|| Error::new(format!("{name}: property {key} is not a string")))?;
            state = state
                .with(key, value)
                .ok_or_else(|| Error::new(format!("{name}: invalid property {key}={value}")))?;
        }
    }
    Ok(state)
}

/// The default state of a block by name; panics for unknown names (programming error).
pub fn default_state(name: &str) -> BlockState {
    BlockId::by_name(name)
        .unwrap_or_else(|| panic!("unknown block {name}"))
        .default_state()
}

/// `minecraft:` prefix for unqualified ids.
pub fn qualify(id: &str) -> String {
    if id.contains(':') {
        id.to_owned()
    } else {
        format!("minecraft:{id}")
    }
}

/// A set of blocks (a resolved block tag), one bit per block id.
#[derive(Clone, Debug, Default)]
pub struct BlockSet {
    bits: Vec<u64>,
}

impl BlockSet {
    /// The blocks of tag `tag` (with or without `#`, e.g. `minecraft:uncarvable`); an unknown tag
    /// is an empty set, as Vanilla treats it.
    pub fn from_tag(packs: &LoadedPacks, tag: &str) -> Self {
        let tag = qualify(tag.trim_start_matches('#'));
        let mut set = Self::default();
        if let Some(entries) = packs.tags("minecraft:block").and_then(|t| t.get(&tag)) {
            for entry in entries {
                if let Some(block) = BlockId::by_name(entry) {
                    set.insert(block);
                }
            }
        }
        set
    }

    /// Adds a block.
    pub fn insert(&mut self, block: BlockId) {
        let index = usize::from(block.0);
        if self.bits.len() <= index / 64 {
            self.bits.resize(index / 64 + 1, 0);
        }
        self.bits[index / 64] |= 1 << (index % 64);
    }

    /// `state.is(tag)`.
    #[inline]
    pub fn contains(&self, state: BlockState) -> bool {
        let index = usize::from(state.block().0);
        self.bits
            .get(index / 64)
            .is_some_and(|word| word & (1 << (index % 64)) != 0)
    }
}

/// Parses a `HolderSet<Block>`: `"minecraft:stone"`, `["minecraft:stone", ...]` or `"#tag"`.
pub fn parse_block_set(packs: &LoadedPacks, json: &Value) -> Result<BlockSet, Error> {
    let mut set = BlockSet::default();
    let mut add = |name: &str| -> Result<(), Error> {
        if let Some(tag) = name.strip_prefix('#') {
            let tagged = BlockSet::from_tag(packs, &qualify(tag));
            for (i, word) in tagged.bits.iter().enumerate() {
                if set.bits.len() <= i {
                    set.bits.resize(i + 1, 0);
                }
                set.bits[i] |= word;
            }
            return Ok(());
        }
        let name = qualify(name);
        let block =
            BlockId::by_name(&name).ok_or_else(|| Error::new(format!("unknown block {name}")))?;
        set.insert(block);
        Ok(())
    };
    match json {
        Value::String(name) => add(name)?,
        Value::Array(items) => {
            for item in items {
                add(item
                    .as_str()
                    .ok_or_else(|| Error::new(format!("invalid block {item}")))?)?;
            }
        }
        other => return Err(Error::new(format!("invalid block set {other}"))),
    }
    Ok(set)
}
