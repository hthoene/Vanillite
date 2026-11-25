//! Upgrades of own-format worlds to a new Minecraft version (#79).
//!
//! Own-format records keep block state, block, fluid and block entity type ids of the data
//! version that wrote them, and every world keeps the names of those ids in
//! `vanillite/ids-<data version>.txt.zst`. A server of a later version builds an old → new
//! [`IdRemap`] from that file and its own registries for every older data version the world's
//! records may still have ([`Upgrades`]); [`crate::OwnCodec`] maps the ids of such records on
//! load and every save writes the current data version.
//!
//! Names are mapped like Vanilla reads a stored block state after its data fixers ran
//! (`NbtUtils.readBlockState`): the block by its (possibly renamed) name, starting from its
//! default state, with every stored property the block still has; an unknown block becomes
//! air. Ticks of unknown blocks and fluids are dropped and block entities of an unknown type
//! keep their data without a type, as Vanilla skips them.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::{fs, io};

use vanillite_data::block::{BlockId, BlockState, STATE_COUNT};
use vanillite_data::registry::{BLOCK, BLOCK_ENTITY_TYPE, FLUID, Registry};
use vanillite_data::version::DATA_VERSION;

use crate::compression::zstd_decompress;

/// Blocks Vanilla's data fixers renamed (`old name`, `new name`), applied to blocks and block
/// states of older tables. A version update adds the renames of the new version's schemas.
const RENAMED_BLOCKS: &[(&str, &str)] = &[];

/// `<level>/vanillite/ids-<data version>.txt.zst`.
pub fn id_table_path(level_dir: &Path, data_version: i32) -> PathBuf {
    level_dir
        .join("vanillite")
        .join(format!("ids-{data_version}.txt.zst"))
}

/// The id table of this server: block states as `name[key=value,…]`, then blocks, fluids and
/// block entity types, one name per line in id order, each list after its `[section]` line.
pub fn id_table_text() -> String {
    let mut text = String::with_capacity(2 << 20);
    text.push_str("[block_states]\n");
    for id in 0..STATE_COUNT {
        let state = BlockState(id as u16);
        text.push_str(state.block().name());
        let mut first = true;
        for (key, value) in state.properties() {
            text.push(if first { '[' } else { ',' });
            first = false;
            let _ = write!(text, "{key}={value}");
        }
        if !first {
            text.push(']');
        }
        text.push('\n');
    }
    for (name, registry) in TABLES {
        let _ = writeln!(text, "[{name}]");
        for entry in registry.entries {
            text.push_str(entry);
            text.push('\n');
        }
    }
    text
}

const TABLES: [(&str, &Registry); 3] = [
    ("blocks", &BLOCK),
    ("fluids", &FLUID),
    ("block_entity_types", &BLOCK_ENTITY_TYPE),
];

fn renamed(name: &str) -> &str {
    RENAMED_BLOCKS
        .iter()
        .find(|(old, _)| *old == name)
        .map_or(name, |(_, new)| new)
}

/// Ids of one older data version mapped to this server's ids.
#[derive(Debug)]
pub struct IdRemap {
    data_version: u32,
    states: Box<[u16]>,
    blocks: Box<[Option<u16>]>,
    fluids: Box<[Option<u16>]>,
    block_entity_types: Box<[Option<u16>]>,
    /// Block states that exist with other properties now.
    pub changed_states: usize,
    /// Block states whose block is gone: they became air.
    pub air_states: usize,
    /// Names of blocks, fluids and block entity types that are gone.
    pub lost: Vec<String>,
}

impl IdRemap {
    /// The remap of the id table `text` written by `data_version`.
    pub fn parse(data_version: u32, text: &str) -> io::Result<Self> {
        let air = BlockId::by_name("minecraft:air").map_or(0, |b| b.default_state().0);
        let mut remap = IdRemap {
            data_version,
            states: Box::default(),
            blocks: Box::default(),
            fluids: Box::default(),
            block_entity_types: Box::default(),
            changed_states: 0,
            air_states: 0,
            lost: Vec::new(),
        };
        let mut states = Vec::with_capacity(STATE_COUNT);
        let mut lists: [Vec<Option<u16>>; 3] = Default::default();
        let mut section = None;
        for line in text.lines().filter(|l| !l.is_empty()) {
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                section = Some(match name {
                    "block_states" => 3,
                    _ => TABLES.iter().position(|(n, _)| *n == name).ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("id table of data version {data_version}: unknown list {name}"),
                        )
                    })?,
                });
                continue;
            }
            match section {
                Some(3) => {
                    let (state, exact) = map_state(line);
                    match state {
                        Some(state) => {
                            remap.changed_states += usize::from(!exact);
                            states.push(state);
                        }
                        None => {
                            remap.air_states += 1;
                            states.push(air);
                        }
                    }
                }
                Some(list) => {
                    let (kind, registry) = TABLES[list];
                    let name = if list == 0 { renamed(line) } else { line };
                    let id = registry.id(name);
                    if id.is_none() {
                        remap.lost.push(format!("{kind} {line}"));
                    }
                    lists[list].push(id);
                }
                None => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("id table of data version {data_version}: no list header"),
                    ));
                }
            }
        }
        let [blocks, fluids, block_entity_types] = lists;
        remap.states = states.into();
        remap.blocks = blocks.into();
        remap.fluids = fluids.into();
        remap.block_entity_types = block_entity_types.into();
        Ok(remap)
    }

    /// The data version of the old ids.
    pub fn data_version(&self) -> u32 {
        self.data_version
    }

    /// The current block state of an old one; `None` for ids the old version did not have.
    #[inline]
    pub fn state(&self, old: u16) -> Option<u16> {
        self.states.get(usize::from(old)).copied()
    }

    /// The current block of an old one, `None` if it is gone.
    pub fn block(&self, old: u16) -> Option<u16> {
        self.blocks.get(usize::from(old)).copied().flatten()
    }

    /// The current fluid of an old one, `None` if it is gone.
    pub fn fluid(&self, old: u16) -> Option<u16> {
        self.fluids.get(usize::from(old)).copied().flatten()
    }

    /// The current block entity type of an old one, `None` if it is gone.
    pub fn block_entity_type(&self, old: u16) -> Option<u16> {
        self.block_entity_types
            .get(usize::from(old))
            .copied()
            .flatten()
    }

    fn log(&self) {
        log::info!(
            "Records of data version {} are upgraded to {DATA_VERSION} when they load: {} block \
             states have other properties now, {} became air{}",
            self.data_version,
            self.changed_states,
            self.air_states,
            if self.lost.is_empty() {
                String::new()
            } else {
                format!(
                    "; gone (their ticks and block entity types are dropped): {}",
                    self.lost.join(", ")
                )
            }
        );
    }
}

/// The current block state of a stored `name[key=value,…]` and whether it is the same state;
/// `None` if the block is gone.
fn map_state(line: &str) -> (Option<u16>, bool) {
    let (name, properties) = match line.split_once('[') {
        Some((name, rest)) => (name, rest.strip_suffix(']').unwrap_or(rest)),
        None => (line, ""),
    };
    let Some(block) = BlockId::by_name(renamed(name)) else {
        return (None, false);
    };
    let mut state = block.default_state();
    let mut exact = name == block.name();
    let mut stored = 0;
    for pair in properties.split(',').filter(|p| !p.is_empty()) {
        stored += 1;
        match pair
            .split_once('=')
            .and_then(|(key, value)| state.with(key, value))
        {
            Some(with) => state = with,
            None => exact = false,
        }
    }
    if state.properties().count() != stored {
        exact = false;
    }
    (Some(state.0), exact)
}

/// The id remaps of every older data version an own-format world's records may have.
#[derive(Debug, Default)]
pub struct Upgrades {
    remaps: Vec<IdRemap>,
}

impl Upgrades {
    /// No older data versions.
    pub fn none() -> Arc<Self> {
        Arc::new(Upgrades::default())
    }

    /// Remaps of the given tables.
    pub fn new(remaps: Vec<IdRemap>) -> Arc<Self> {
        Arc::new(Upgrades { remaps })
    }

    /// The remaps of the older data versions in the world's marker, built from their id tables.
    pub fn load(level_dir: &Path) -> io::Result<Arc<Self>> {
        let mut remaps = Vec::new();
        for version in crate::format::older_data_versions(level_dir)? {
            let path = id_table_path(level_dir, version);
            let context = |e: io::Error| {
                io::Error::new(
                    e.kind(),
                    format!(
                        "id table of data version {version} ({}): {e}",
                        path.display()
                    ),
                )
            };
            let text = fs::read(&path)
                .and_then(|bytes| zstd_decompress(&bytes))
                .map_err(context)?;
            let text = String::from_utf8(text)
                .map_err(|_| context(io::Error::from(io::ErrorKind::InvalidData)))?;
            let remap = IdRemap::parse(version as u32, &text)?;
            remap.log();
            remaps.push(remap);
        }
        Ok(Arc::new(Upgrades { remaps }))
    }

    /// The remap of records written by `data_version`.
    pub fn get(&self, data_version: u32) -> Option<&IdRemap> {
        self.remaps.iter().find(|r| r.data_version == data_version)
    }

    /// Whether there are no older data versions.
    pub fn is_empty(&self) -> bool {
        self.remaps.is_empty()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// An id table of a made-up older version: block state `removed` is not in it (the later
    /// states are one lower) and a removed block's state is last, air has a property it lost
    /// since; with `shift_lists` a removed block and block entity type at id 0 put the others
    /// one higher.
    pub(crate) fn older_table(removed: usize, shift_lists: bool) -> String {
        let mut out = String::new();
        let mut section = "";
        let mut index = 0;
        for line in id_table_text().lines() {
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                if section == "block_states" {
                    out.push_str("minecraft:removed_block\n");
                }
                section = name;
                out.push_str(line);
                out.push('\n');
                match name {
                    _ if !shift_lists => {}
                    "blocks" => out.push_str("minecraft:removed_block\n"),
                    "block_entity_types" => out.push_str("minecraft:removed_entity\n"),
                    _ => {}
                }
                continue;
            }
            if section == "block_states" {
                index += 1;
                if index - 1 == removed {
                    continue;
                }
                out.push_str(line);
                if index == 1 {
                    out.push_str("[ghost=1]");
                }
            } else {
                out.push_str(line);
            }
            out.push('\n');
        }
        out
    }

    /// The current state of an old one of [`older_table`].
    pub(crate) fn upgraded_state(removed: usize, old: u16) -> u16 {
        match usize::from(old) {
            old if old < removed => old as u16,
            old if old == STATE_COUNT - 1 => 0,
            old => old as u16 + 1,
        }
    }

    #[test]
    fn the_current_table_maps_to_itself() {
        let remap = IdRemap::parse(1, &id_table_text()).unwrap();
        assert!((0..STATE_COUNT as u16).all(|id| remap.state(id) == Some(id)));
        assert!((0..BLOCK.len() as u16).all(|id| remap.block(id) == Some(id)));
        assert!((0..FLUID.len() as u16).all(|id| remap.fluid(id) == Some(id)));
        assert!(
            (0..BLOCK_ENTITY_TYPE.len() as u16).all(|id| remap.block_entity_type(id) == Some(id))
        );
        assert_eq!((remap.changed_states, remap.air_states), (0, 0));
        assert!(remap.lost.is_empty());
    }

    #[test]
    fn shifted_and_removed_ids_are_mapped() {
        let removed = 10;
        let remap = IdRemap::parse(1, &older_table(removed, true)).unwrap();
        for old in 0..STATE_COUNT as u16 {
            assert_eq!(
                remap.state(old),
                Some(upgraded_state(removed, old)),
                "{old}"
            );
        }
        assert_eq!(remap.state(STATE_COUNT as u16), None);
        assert_eq!((remap.changed_states, remap.air_states), (1, 1));
        assert_eq!(remap.block(0), None);
        assert_eq!(remap.block(5), Some(4));
        assert_eq!(remap.fluid(1), Some(1));
        assert_eq!(remap.block_entity_type(0), None);
        assert_eq!(remap.block_entity_type(3), Some(2));
        assert_eq!(
            remap.lost,
            [
                "blocks minecraft:removed_block",
                "block_entity_types minecraft:removed_entity"
            ]
        );
    }

    /// Stored properties a block lost are dropped, new ones take their default values.
    #[test]
    fn states_keep_the_properties_the_block_still_has() {
        let block = BlockId::by_name("minecraft:oak_log").unwrap();
        let x = block.default_state().with("axis", "x").unwrap();
        assert_eq!(map_state("minecraft:oak_log[axis=x]"), (Some(x.0), true));
        assert_eq!(
            map_state("minecraft:oak_log[axis=x,burnt=true]"),
            (Some(x.0), false)
        );
        assert_eq!(
            map_state("minecraft:oak_log"),
            (Some(block.default_state().0), false)
        );
        assert_eq!(map_state("minecraft:no_such_block[a=b]"), (None, false));
    }
}
