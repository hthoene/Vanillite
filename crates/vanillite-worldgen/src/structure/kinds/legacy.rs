//! Helpers shared by the procedural piece structures (mineshafts, strongholds, nether
//! fortresses): spawners, block state properties and the `PieceWeight` tables.

use vanillite_base::BlockPos;
use vanillite_data::block::{BlockState, ids};
use vanillite_nbt::{Compound, List, Tag};

use crate::feature::region::WorldGenRegion;

/// `state.setValue(property, value)` for each pair (properties the block lacks are skipped).
pub(crate) fn with_props(state: BlockState, pairs: &[(&str, &str)]) -> BlockState {
    pairs
        .iter()
        .fold(state, |s, (k, v)| s.with(k, v).unwrap_or(s))
}

/// `level.setBlock(pos, Blocks.SPAWNER.defaultBlockState(), 2)` followed by
/// `spawner.setEntityId(entity, random)` on its block entity, as the pieces write their
/// spawners. A fresh spawner has no spawn potentials, so `setEntityId` draws no random value.
pub(crate) fn place_spawner(level: &mut WorldGenRegion<'_>, pos: BlockPos, entity: &str) {
    level.set_block(pos, ids::SPAWNER.default_state(), 2);
    if level.block(pos).block() != ids::SPAWNER {
        return;
    }
    let Some(tag) = level.block_entity_mut(pos) else {
        return;
    };
    set_spawner_entity_id(tag, entity);
}

/// `SpawnerBlockEntity.setEntityId(type, random)` on the saved data: a fresh spawner gets
/// `BaseSpawner`'s default fields and `SpawnData.entity.id` is set (as `MonsterRoomFeature`
/// writes its spawner).
fn set_spawner_entity_id(tag: &mut Compound, entity: &str) {
    if tag.get("Delay").is_none() {
        tag.insert("Delay", Tag::Short(20));
        tag.insert("MinSpawnDelay", Tag::Short(200));
        tag.insert("MaxSpawnDelay", Tag::Short(800));
        tag.insert("SpawnCount", Tag::Short(4));
        tag.insert("MaxNearbyEntities", Tag::Short(6));
        tag.insert("RequiredPlayerRange", Tag::Short(16));
        tag.insert("SpawnRange", Tag::Short(4));
        tag.insert("SpawnPotentials", Tag::List(List::new()));
    }
    let mut spawn_data = match tag.remove("SpawnData") {
        Some(Tag::Compound(data)) => data,
        _ => Compound::new(),
    };
    let mut entity_tag = match spawn_data.remove("entity") {
        Some(Tag::Compound(e)) => e,
        _ => Compound::new(),
    };
    entity_tag.insert("id", Tag::String(entity.to_owned()));
    spawn_data.insert("entity", Tag::Compound(entity_tag));
    tag.insert("SpawnData", Tag::Compound(spawn_data));
}

/// A `PieceWeight` of a weighted piece table (`StrongholdPieces.PieceWeight`,
/// `NetherFortressPieces.PieceWeight`). Pieces are identified by `kind`, as each class appears
/// once in Vanilla's tables (which compare the weight objects by identity).
#[derive(Clone, Copy, Debug)]
pub(crate) struct PieceWeight<K> {
    /// `pieceClass`.
    pub kind: K,
    pub weight: i32,
    /// `placeCount`.
    pub place_count: i32,
    /// `maxPlaceCount` (0: unlimited).
    pub max_place_count: i32,
    /// `allowInRow` (fortress pieces only).
    pub allow_in_row: bool,
    /// The extra depth the stronghold library and portal room need (`doPlace` overrides:
    /// `depth > min_depth`); `i32::MIN` for none.
    pub min_depth: i32,
}

impl<K> PieceWeight<K> {
    /// A table entry.
    pub const fn new(kind: K, weight: i32, max_place_count: i32) -> Self {
        Self {
            kind,
            weight,
            place_count: 0,
            max_place_count,
            allow_in_row: false,
            min_depth: i32::MIN,
        }
    }

    /// The entry with `allowInRow`.
    pub const fn in_row(mut self) -> Self {
        self.allow_in_row = true;
        self
    }

    /// The entry with a `doPlace` override requiring `depth > min_depth`.
    pub const fn deeper_than(mut self, min_depth: i32) -> Self {
        self.min_depth = min_depth;
        self
    }

    /// `doPlace(depth)`.
    pub fn do_place(&self, depth: i32) -> bool {
        (self.max_place_count == 0 || self.place_count < self.max_place_count)
            && depth > self.min_depth
    }

    /// `isValid()`.
    pub fn is_valid(&self) -> bool {
        self.max_place_count == 0 || self.place_count < self.max_place_count
    }
}

/// Whether the block is a `FallingBlock` (sand, red sand, gravel, concrete powder, anvils and
/// the dragon egg).
pub(crate) fn is_falling_block(state: BlockState) -> bool {
    matches!(
        state.block(),
        ids::SAND
            | ids::RED_SAND
            | ids::GRAVEL
            | ids::DRAGON_EGG
            | ids::ANVIL
            | ids::CHIPPED_ANVIL
            | ids::DAMAGED_ANVIL
            | ids::WHITE_CONCRETE_POWDER
            | ids::ORANGE_CONCRETE_POWDER
            | ids::MAGENTA_CONCRETE_POWDER
            | ids::LIGHT_BLUE_CONCRETE_POWDER
            | ids::YELLOW_CONCRETE_POWDER
            | ids::LIME_CONCRETE_POWDER
            | ids::PINK_CONCRETE_POWDER
            | ids::GRAY_CONCRETE_POWDER
            | ids::LIGHT_GRAY_CONCRETE_POWDER
            | ids::CYAN_CONCRETE_POWDER
            | ids::PURPLE_CONCRETE_POWDER
            | ids::BLUE_CONCRETE_POWDER
            | ids::BROWN_CONCRETE_POWDER
            | ids::GREEN_CONCRETE_POWDER
            | ids::RED_CONCRETE_POWDER
            | ids::BLACK_CONCRETE_POWDER
    )
}
