//! Structures (`world/level/levelgen/structure`): structure sets and their placement, structure
//! starts and pieces, the `Beardifier`, and the structure types.
//!
//! # Pipeline
//!
//! 1. `Stage::StructureStarts` ([`crate::TerrainGenerator::generate_structure_starts`],
//!    `ChunkGenerator.createStructures`): for every possible [`StructureSet`] whose
//!    [`StructurePlacement`] selects the chunk, the set's structures are tried (weighted) and the
//!    first valid [`StructureStart`] is stored in the chunk
//!    ([`crate::ProtoChunk::set_start_for_structure`]).
//! 2. `Stage::StructureReferences` ([`crate::TerrainGenerator::generate_structure_references`],
//!    `createReferences`): every start of the 17×17 chunks around a chunk whose box reaches it is
//!    recorded as a reference ([`crate::ProtoChunk::add_reference`]).
//! 3. `Stage::Terrain`: the [`Beardifier`] of the starts the chunk references is added to the
//!    final density ([`crate::TerrainJob::structures`]).
//! 4. `Stage::Features` ([`crate::TerrainGenerator::generate_features`], `applyBiomeDecoration`): before
//!    the features of each decoration step, the structures of that step (registry order,
//!    `setFeatureSeed(decorationSeed, index, step)`) place the referenced starts' pieces that
//!    intersect the chunk ([`StructureStart::place_in_chunk`]).
//!
//! # Porting a structure type
//!
//! A structure type implements [`StructureType`] and is listed in `structure_types!` in
//! `structure.rs`:
//!
//! - `parse(json, parser)` reads the type's own fields. [`StructureParser`] resolves biome sets,
//!   templates ([`StructureParser::template`]; [`StructureParser::template_outline`] for types
//!   with many templates, which then load them from the shared cache when placing), processor lists, block tags and placed features.
//! - `find_generation_point(ctx)` ports `findGenerationPoint`. [`GenerationContext`] has the
//!   legacy `random` (`setLargeFeatureSeed(seed, chunkX, chunkZ)`), `chunk_pos`, `height` (the
//!   `LevelHeightAccessor`), the biome checks (`could_valid_biome_exist_on_top_of_chunk_center`,
//!   `valid_biome`, `noise_biome`) and the generator (`generator.first_occupied_height`,
//!   `base_height`, `base_column`, `sea_level`, ...). The helpers `onTopOfChunkCenter`,
//!   `getLowestY`, `getMeanFirstOccupiedHeight` and `getLowestYIn5by5Box` are free functions in
//!   [`structure`]. Return a [`GenerationStub`]: [`GenerationStub::lazy`] with a closure that
//!   fills a [`StructurePiecesBuilder`] (Vanilla's `Consumer`), or [`GenerationStub::built`].
//! - `after_place` ports `afterPlace` (e.g. the desert pyramid's suspicious sand).
//! - Keep [`StructureType::PORTED`] at its default `true` (`false` marks a stub that parses
//!   and never generates; the parity tests report its structures separately).
//!
//! # Porting a piece
//!
//! A piece is a struct implementing [`StructurePiece`] (stored as `Box<dyn StructurePiece>`):
//!
//! - Keep the base class state in a [`PieceBase`] (`kind` is the `StructurePieceType` id, e.g.
//!   `minecraft:tesh`; [`PieceBase::set_orientation`] sets the mirror/rotation `placeBlock`
//!   applies), or embed [`ScatteredPiece`] (`ScatteredFeaturePiece`: `width`/`height`/`depth`,
//!   `updateAverageGroundHeight`, `updateHeightPositionToLowestGroundHeight`) or
//!   [`TemplatePiece`] (`TemplateStructurePiece`: template, owned [`PieceSettings`], template
//!   position, [`TemplatePiece::place`] with the piece's `handleDataMarker`) and delegate
//!   `base`/`base_mut` (and for template pieces `move_by`/`rotation`) to it.
//! - `post_process(ctx, chunk_bb, chunk_pos, reference)` ports `postProcess`. [`PieceContext`]
//!   holds the level ([`crate::feature::WorldGenRegion`], `level.random()` is
//!   `level.getRandom()`), the decoration `random` and the `generator`. `chunk_bb` is the
//!   writable area of the decorated chunk, fresh per start and shared by its pieces and
//!   `after_place` (`&mut`: pieces that `encapsulate` it in Vanilla grow it); reborrow it as
//!   `let bb: &BoundingBox = chunk_bb;` for the helpers. Pieces must not lock other starts'
//!   pieces (the start's own pieces are locked while they are placed).
//! - The base class helpers are provided trait methods with Vanilla's names in snake case:
//!   `world_pos`/`base().world_x`/`world_y`/`world_z`, `place_block`, `get_block`,
//!   `is_interior`, `generate_box` (edge/fill), `generate_box_in` (a box), `generate_box_with`
//!   / `generate_box_in_with` (a [`BlockSelector`]), `generate_air_box`, `generate_maybe_box`,
//!   `maybe_generate_block`, `generate_upper_half_sphere`, `fill_column_down`, `create_chest`
//!   (and [`create_chest_at`] for world positions / explicit states), `create_dispenser`,
//!   `is_close_to_chunk`. Override `can_be_replaced`, `is_replaceable_by_structures`,
//!   `fill_column_down`, `move_by` or `rotation` where the Vanilla class overrides them.
//!   Statics: [`make_bounding_box`], [`random_horizontal_direction`], [`reorient`],
//!   [`create_bounding_box`], [`find_collision_piece`].
//! - Pieces that build children (`addChildren` of mineshafts, strongholds, fortresses) do so in
//!   their structure's generator on concrete types and push boxed pieces into the
//!   [`StructurePiecesBuilder`] (`find_collision_piece` works on its `pieces`).
//! - Pieces are shared by every chunk the start reaches, and changes a piece makes to itself in
//!   `post_process` (settled height, placed chests) are seen by later chunks, as in Vanilla.
//!   `after_place` and the `Beardifier` downcast with `<dyn StructurePiece>::downcast_ref`.
//! - Jigsaw pieces ([`pool::PoolElementStructurePiece`], built by `JigsawPlacement` in
//!   [`kinds::jigsaw`]) implement [`StructurePiece::pool_element`] for the `Beardifier`.
//!
//! - `additional_save_data` ports `addAdditionalSaveData` (the piece's own NBT fields; the
//!   parity test `tests/structure_goldens.rs` compares it with Vanilla's `createTag`).
//!
//! Entities of structures (witches, cats, drowned, template entities, …) are recorded in the
//! chunk with [`crate::feature::WorldGenRegion::add_fresh_entity`] (type and position, see
//! [`crate::feature::region::entity_tag`]); `finalizeSpawn` is not run, so its draws from the
//! level's random are not made (no golden shows that random drift so far).
//!
//! # Parity tests
//!
//! `tests/structure_goldens.rs` compares starts, references, terrain with the `Beardifier` and
//! decoration with Vanilla's goldens (`tests/goldens/STRUCTURES.md`); structures of stub types
//! ([`StructureType::PORTED`] is `false` for them), and starts of ported structures where
//! Vanilla chose a stubbed one of the same structure set, are reported apart.
//! `tests/jigsaw_goldens.rs` checks the jigsaw starts piece by piece and the jigsaw decoration
//! areas near their pieces. Set
//! `VANILLITE_STRUCTURE=minecraft:igloo` to check one structure.

pub mod beardifier;
pub mod kinds;
pub mod long_set;
pub mod piece;
pub mod pieces;
pub mod placement;
pub mod pool;
pub mod scattered;
pub mod start;
pub mod state;
#[allow(clippy::module_inception)]
pub mod structure;
pub mod template_piece;

use std::sync::Arc;

use rustc_hash::FxHashMap;
use serde_json::Value;
use vanillite_chunk::BiomeId;
use vanillite_datapack::LoadedPacks;

use crate::Error;
use crate::biome::BiomeRegistry;
use crate::blocks::{BlockSet, qualify};
use crate::feature::FeatureSet;
use crate::template::TemplateRef;
use crate::template::{ProcessorList, StructureTemplate, TemplateLoader};

pub use beardifier::Beardifier;
pub use long_set::LongOpenHashSet;
pub use piece::{
    BlockSelector, PieceBase, PieceContext, StructurePiece, create_bounding_box, create_chest_at,
    find_collision_piece, make_bounding_box, random_horizontal_direction, reorient,
};
pub use pieces::StructurePiecesBuilder;
pub use placement::StructurePlacement;
pub use scattered::ScatteredPiece;
pub use start::{NeighbourStarts, ReferencedStarts, StructureStart};
pub use state::StructureState;
pub use structure::{GenerationContext, GenerationStub, Structure, StructureType};
pub use template_piece::{PieceSettings, TemplatePiece};

/// Index of a [`Structure`] in the [`StructureRegistry`] (registry order).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StructureId(pub u32);

/// Index of a [`StructureSet`] in the [`StructureRegistry`] (registry order).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StructureSetId(pub u32);

/// The height range of a level or chunk (`LevelHeightAccessor`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LevelHeight {
    /// `getMinY()`.
    pub min_y: i32,
    /// `getHeight()`.
    pub height: i32,
}

impl LevelHeight {
    /// `getMaxY()`.
    pub fn max_y(&self) -> i32 {
        self.min_y + self.height - 1
    }
}

/// `GenerationStep.Decoration`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Decoration {
    RawGeneration,
    Lakes,
    LocalModifications,
    UndergroundStructures,
    SurfaceStructures,
    Strongholds,
    UndergroundOres,
    UndergroundDecoration,
    FluidSprings,
    VegetalDecoration,
    TopLayerModification,
}

impl Decoration {
    /// Every step in order.
    pub const ALL: [Self; 11] = [
        Self::RawGeneration,
        Self::Lakes,
        Self::LocalModifications,
        Self::UndergroundStructures,
        Self::SurfaceStructures,
        Self::Strongholds,
        Self::UndergroundOres,
        Self::UndergroundDecoration,
        Self::FluidSprings,
        Self::VegetalDecoration,
        Self::TopLayerModification,
    ];

    /// Parses the serialized name.
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "raw_generation" => Self::RawGeneration,
            "lakes" => Self::Lakes,
            "local_modifications" => Self::LocalModifications,
            "underground_structures" => Self::UndergroundStructures,
            "surface_structures" => Self::SurfaceStructures,
            "strongholds" => Self::Strongholds,
            "underground_ores" => Self::UndergroundOres,
            "underground_decoration" => Self::UndergroundDecoration,
            "fluid_springs" => Self::FluidSprings,
            "vegetal_decoration" => Self::VegetalDecoration,
            "top_layer_modification" => Self::TopLayerModification,
            _ => return None,
        })
    }
}

/// `TerrainAdjustment`: how the terrain stage shapes the ground around a structure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TerrainAdjustment {
    None,
    Bury,
    BeardThin,
    BeardBox,
    Encapsulate,
}

impl TerrainAdjustment {
    /// Parses the serialized name.
    pub fn from_name(name: &str) -> Result<Self, Error> {
        Ok(match name {
            "none" => Self::None,
            "bury" => Self::Bury,
            "beard_thin" => Self::BeardThin,
            "beard_box" => Self::BeardBox,
            "encapsulate" => Self::Encapsulate,
            other => return Err(Error::new(format!("unknown terrain adjustment {other}"))),
        })
    }
}

/// A set of biomes (a resolved `HolderSet<Biome>`), one bit per biome id.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BiomeFilter {
    bits: Vec<u64>,
}

impl BiomeFilter {
    /// A filter of the given biomes.
    pub fn of(biomes: impl IntoIterator<Item = BiomeId>) -> Self {
        let mut filter = Self::default();
        for biome in biomes {
            let (word, bit) = (usize::from(biome) / 64, usize::from(biome) % 64);
            if filter.bits.len() <= word {
                filter.bits.resize(word + 1, 0);
            }
            filter.bits[word] |= 1 << bit;
        }
        filter
    }

    /// `contains(biome)`.
    #[inline]
    pub fn contains(&self, biome: BiomeId) -> bool {
        let (word, bit) = (usize::from(biome) / 64, usize::from(biome) % 64);
        self.bits.get(word).is_some_and(|w| w & (1 << bit) != 0)
    }

    /// Whether any of `biomes` is in the filter.
    pub fn contains_any(&self, biomes: &[BiomeId]) -> bool {
        biomes.iter().any(|&b| self.contains(b))
    }
}

/// `StructureSet`: weighted structures sharing one placement.
#[derive(Debug)]
pub struct StructureSet {
    /// Registry name.
    pub name: String,
    /// `structures`: each with its weight.
    pub structures: Vec<(StructureId, i32)>,
    pub placement: StructurePlacement,
}

/// Every structure and structure set of the data packs, parsed once per dimension, plus the
/// sets the dimension's biome source can place (`ChunkGeneratorStructureState`).
#[derive(Debug)]
pub struct StructureRegistry {
    structures: Vec<Structure>,
    structure_ids: FxHashMap<String, StructureId>,
    sets: Vec<StructureSet>,
    /// Per decoration step: the structures of that step in registry order.
    by_step: Vec<Vec<StructureId>>,
    /// `possibleStructureSets()`.
    possible_sets: Vec<StructureSetId>,
}

impl StructureRegistry {
    /// Parses every `worldgen/structure` and `worldgen/structure_set` entry; the sets with a
    /// structure in `possible_biomes` are the dimension's possible sets.
    pub fn load(
        packs: &LoadedPacks,
        biomes: &BiomeRegistry,
        features: &FeatureSet,
        possible_biomes: &[BiomeId],
        templates: &Arc<crate::template::StructureTemplateManager>,
    ) -> Result<Self, Error> {
        let mut parser = StructureParser::new(packs, biomes, features).with_templates(templates);
        let mut structures = Vec::new();
        if let Some(registry) = packs.registry(STRUCTURE_REGISTRY) {
            for (index, entry) in registry.entries().iter().enumerate() {
                let json: Value = serde_json::from_str(entry.json.get())
                    .map_err(|e| Error::new(format!("structure {}: {e}", entry.id)))?;
                let structure = Structure::parse(entry.id.clone(), &json, &mut parser)
                    .map_err(|e| Error::new(format!("structure {}: {e}", entry.id)))?;
                parser
                    .structure_ids
                    .insert(entry.id.clone(), StructureId(index as u32));
                structures.push(structure);
            }
        }
        if let Some(registry) = packs.registry(STRUCTURE_SET_REGISTRY) {
            for entry in registry.entries() {
                parser
                    .structure_set(&Value::String(entry.id.clone()))
                    .map_err(|e| Error::new(format!("structure set {}: {e}", entry.id)))?;
            }
        }
        let mut sets: Vec<StructureSet> = parser
            .sets
            .into_iter()
            .map(|s| s.ok_or_else(|| Error::new("structure set references itself")))
            .collect::<Result<_, _>>()?;
        let mut by_step = vec![Vec::new(); Decoration::ALL.len()];
        for (index, structure) in structures.iter().enumerate() {
            by_step[structure.step() as usize].push(StructureId(index as u32));
        }
        // `ChunkGeneratorStructureState.createForNormal`: `hasBiomesForStructureSet`, in
        // registry order (the registry's sets come first; sets parsed inline follow).
        let set_count = packs
            .registry(STRUCTURE_SET_REGISTRY)
            .map_or(0, |r| r.entries().len());
        let mut possible_sets = Vec::new();
        for (index, set) in sets.iter_mut().enumerate() {
            let possible = set.structures.iter().any(|(id, _)| {
                structures[id.0 as usize]
                    .settings
                    .biomes
                    .contains_any(possible_biomes)
            });
            if let StructurePlacement::ConcentricRings { rings, .. } = &mut set.placement {
                rings.placeable = possible;
            }
            if possible && index < set_count {
                possible_sets.push(StructureSetId(index as u32));
            }
        }
        Ok(Self {
            structure_ids: parser.structure_ids,
            structures,
            sets,
            by_step,
            possible_sets,
        })
    }

    /// A structure by id.
    pub fn structure(&self, id: StructureId) -> &Structure {
        &self.structures[id.0 as usize]
    }

    /// Every structure in registry order.
    pub fn structures(&self) -> &[Structure] {
        &self.structures
    }

    /// A structure id by registry name (namespace optional).
    pub fn structure_id(&self, name: &str) -> Option<StructureId> {
        self.structure_ids.get(&qualify(name)).copied()
    }

    /// A structure set by id.
    pub fn set(&self, id: StructureSetId) -> &StructureSet {
        &self.sets[id.0 as usize]
    }

    /// Every structure set (registry entries first).
    pub fn sets(&self) -> &[StructureSet] {
        &self.sets
    }

    /// The structures of a decoration step, in registry order (`structuresByStep`).
    pub fn by_step(&self, step: usize) -> &[StructureId] {
        self.by_step.get(step).map_or(&[], Vec::as_slice)
    }

    /// `possibleStructureSets()`.
    pub fn possible_sets(&self) -> &[StructureSetId] {
        &self.possible_sets
    }
}

const STRUCTURE_REGISTRY: &str = "minecraft:worldgen/structure";
const STRUCTURE_SET_REGISTRY: &str = "minecraft:worldgen/structure_set";

/// Parses structures and structure sets, resolving references once.
#[derive(Debug)]
pub struct StructureParser<'a> {
    packs: &'a LoadedPacks,
    biomes: &'a BiomeRegistry,
    features: &'a FeatureSet,
    templates: TemplateLoader,
    structure_ids: FxHashMap<String, StructureId>,
    /// Parsed sets by index (`None` while a set is being parsed).
    sets: Vec<Option<StructureSet>>,
    set_ids: FxHashMap<String, StructureSetId>,
}

impl<'a> StructureParser<'a> {
    pub(crate) fn new(
        packs: &'a LoadedPacks,
        biomes: &'a BiomeRegistry,
        features: &'a FeatureSet,
    ) -> Self {
        // The registry's sets keep their registry index as id.
        let mut set_ids = FxHashMap::default();
        let mut sets = Vec::new();
        if let Some(registry) = packs.registry(STRUCTURE_SET_REGISTRY) {
            for (index, entry) in registry.entries().iter().enumerate() {
                set_ids.insert(entry.id.clone(), StructureSetId(index as u32));
                sets.push(None);
            }
        }
        Self {
            packs,
            biomes,
            features,
            templates: TemplateLoader::new(packs),
            structure_ids: FxHashMap::default(),
            sets,
            set_ids,
        }
    }

    /// Reads templates through `templates` (the cache of the generator) instead of an own
    /// cache.
    pub fn with_templates(
        mut self,
        templates: &Arc<crate::template::StructureTemplateManager>,
    ) -> Self {
        self.templates = TemplateLoader::with_manager(Arc::clone(templates));
        self
    }

    /// The data packs.
    pub fn packs(&self) -> &'a LoadedPacks {
        self.packs
    }

    /// The biome registry.
    pub fn biomes(&self) -> &'a BiomeRegistry {
        self.biomes
    }

    /// The dimension's features (placed features of jigsaw feature elements).
    pub fn features(&self) -> &'a FeatureSet {
        self.features
    }

    /// The template `id`, read when a piece uses it.
    pub fn template(&self, id: &str) -> TemplateRef {
        self.templates.template(id)
    }

    /// The size and jigsaw blocks of a template ([`StructureTemplateManager::outline`]) for
    /// elements that load the template only when they place it.
    ///
    /// [`StructureTemplateManager::outline`]: crate::template::StructureTemplateManager::outline
    pub fn template_outline(&self, id: &str) -> Result<Arc<StructureTemplate>, Error> {
        self.templates.manager().outline(id)
    }

    /// The template cache shared by the structures of this parser.
    pub fn template_manager(&self) -> &Arc<crate::template::StructureTemplateManager> {
        self.templates.manager()
    }

    /// A `Holder<StructureProcessorList>`: a registry id or an inline list.
    pub fn processor_list(&mut self, json: &Value) -> Result<Arc<ProcessorList>, Error> {
        self.templates.processor_list(self.packs, json)
    }

    /// A block tag (`#` optional) resolved to a set.
    pub fn block_tag(&self, tag: &str) -> BlockSet {
        BlockSet::from_tag(self.packs, &qualify(tag.trim_start_matches('#')))
    }

    /// A `HolderSet<Biome>` (id, list or `#tag`) as a filter.
    pub fn biome_filter(&self, json: &Value) -> Result<BiomeFilter, Error> {
        let names: Vec<String> = match json {
            Value::String(s) if s.starts_with('#') => {
                let tag = qualify(&s[1..]);
                self.packs
                    .tags("minecraft:worldgen/biome")
                    .and_then(|t| t.get(&tag))
                    .cloned()
                    .ok_or_else(|| Error::new(format!("unknown biome tag #{tag}")))?
            }
            Value::String(s) => vec![qualify(s)],
            Value::Array(items) => items
                .iter()
                .map(|i| {
                    i.as_str()
                        .map(qualify)
                        .ok_or_else(|| Error::new(format!("invalid biome {i}")))
                })
                .collect::<Result<_, _>>()?,
            other => return Err(Error::new(format!("invalid biome set {other}"))),
        };
        let ids = names
            .iter()
            .map(|n| self.biomes.require(n))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(BiomeFilter::of(ids))
    }

    /// A `Holder<Structure>` by registry id (structures are parsed before sets). Inline
    /// structures are not supported: Vanilla never places them (they are not in the registry
    /// the features stage walks).
    pub fn structure(&self, json: &Value) -> Result<StructureId, Error> {
        let name = json
            .as_str()
            .ok_or_else(|| Error::new("inline structures are not supported"))?;
        self.structure_ids
            .get(&qualify(name))
            .copied()
            .ok_or_else(|| Error::new(format!("unknown structure {name}")))
    }

    /// A `Holder<StructureSet>`: a registry id (parsed on first use) or an inline set.
    pub fn structure_set(&mut self, json: &Value) -> Result<StructureSetId, Error> {
        if let Some(name) = json.as_str() {
            let name = qualify(name);
            let id = *self
                .set_ids
                .get(&name)
                .ok_or_else(|| Error::new(format!("unknown structure set {name}")))?;
            if self.sets[id.0 as usize].is_none() {
                let entry = self
                    .packs
                    .registry(STRUCTURE_SET_REGISTRY)
                    .and_then(|r| r.get(&name))
                    .ok_or_else(|| Error::new(format!("unknown structure set {name}")))?;
                let definition: Value = serde_json::from_str(entry.json.get())
                    .map_err(|e| Error::new(format!("{name}: {e}")))?;
                let set = self.parse_set(name, &definition)?;
                self.sets[id.0 as usize] = Some(set);
            }
            return Ok(id);
        }
        let set = self.parse_set(String::new(), json)?;
        self.sets.push(Some(set));
        Ok(StructureSetId(self.sets.len() as u32 - 1))
    }

    fn parse_set(&mut self, name: String, json: &Value) -> Result<StructureSet, Error> {
        let structures = crate::feature::field(json, "structures")?
            .as_array()
            .ok_or_else(|| Error::new("structures is not a list"))?
            .iter()
            .map(|entry| {
                let structure = self.structure(crate::feature::field(entry, "structure")?)?;
                let weight = crate::feature::int_field(entry, "weight")?;
                if weight <= 0 {
                    return Err(Error::new("structure weight must be positive"));
                }
                Ok((structure, weight))
            })
            .collect::<Result<Vec<_>, Error>>()?;
        let placement = StructurePlacement::parse(crate::feature::field(json, "placement")?, self)?;
        Ok(StructureSet {
            name,
            structures,
            placement,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generators and chunks with starts are shared across generation threads.
    #[test]
    fn shared_types_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<crate::TerrainGenerator>();
        assert_send_sync::<crate::ProtoChunk>();
        assert_send_sync::<StructureStart>();
        assert_send_sync::<ReferencedStarts>();
    }

    #[test]
    fn biome_filter_contains_its_biomes() {
        let filter = BiomeFilter::of([3, 64, 130]);
        assert!(filter.contains(3) && filter.contains(64) && filter.contains(130));
        assert!(!filter.contains(4) && !filter.contains(1000));
        assert!(filter.contains_any(&[1, 2, 130]));
    }

    #[test]
    fn stage_requirements_follow_the_generation_pyramid() {
        use crate::Stage;
        assert_eq!(
            Stage::StructureReferences.requirements(),
            &[(Stage::StructureStarts, 8)]
        );
        assert_eq!(
            Stage::Features.requirements(),
            &[(Stage::StructureStarts, 8), (Stage::Terrain, 1)]
        );
        assert_eq!(Stage::StructureReferences.dependency_radius(), 8);
    }
}
