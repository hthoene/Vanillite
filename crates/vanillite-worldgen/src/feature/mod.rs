//! Features: the `features` stage (`ChunkGenerator.applyBiomeDecoration`).
//!
//! The data driven parts mirror Vanilla 26.3: a [`Feature`] (registry `worldgen/feature`) is a
//! typed configuration that places blocks at a position; a [`PlacedFeature`]
//! (`worldgen/placed_feature`) pairs a feature with [`PlacementModifier`]s that turn one origin
//! into the positions the feature is placed at. Biomes list placed features per decoration step;
//! [`sorter`] orders them globally like Vanilla's `FeatureSorter`, and [`FeatureSet::decorate`]
//! runs them for one chunk inside a [`WorldGenRegion`].
//!
//! Everything is parsed once per dimension into flat vectors; features refer to each other by
//! index, so placing a feature never touches JSON, strings or hash maps.

pub mod kinds;
pub mod placement;
pub mod predicate;
pub mod region;
pub mod sorter;
pub mod state_provider;

use rustc_hash::{FxHashMap, FxHashSet};
use serde_json::Value;
use vanillite_base::BlockPos;
use vanillite_base::random::DecorationRandom;
use vanillite_chunk::BiomeId;
use vanillite_datapack::LoadedPacks;

use crate::Error;
use crate::anchor::WorldGenerationContext;
use crate::biome::BiomeRegistry;
use crate::blocks::{BlockSet, qualify};
use crate::random_state::RandomState;

pub use placement::{PlacementContext, PlacementModifier};
pub use predicate::BlockPredicate;
pub use region::WorldGenRegion;
pub use state_provider::BlockStateProvider;

/// Index of a [`Feature`] in a [`FeatureRegistry`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FeatureId(pub u32);

/// Index of a [`PlacedFeature`] in a [`FeatureRegistry`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PlacedFeatureId(pub u32);

/// A feature type: parsed from its JSON configuration, placed at an origin.
///
/// Implementations port one Vanilla `Feature` record each (`place` is `Feature.place`).
pub trait FeatureType: Sized + std::fmt::Debug + Send + Sync {
    /// Parses the configuration (the feature's JSON object, `type` included).
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error>;

    /// `place(level, generator, random, origin)`: whether anything was placed.
    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool;
}

macro_rules! features {
    ($($name:literal => $variant:ident($ty:ty),)*) => {
        /// A configured feature (`worldgen/feature` entry).
        #[derive(Debug)]
        pub enum Feature {
            $(
                #[doc = concat!("`minecraft:", $name, "`.")]
                $variant($ty),
            )*
        }

        impl Feature {
            fn parse_typed(
                kind: &str,
                json: &Value,
                parser: &mut FeatureParser<'_>,
            ) -> Result<Self, Error> {
                match kind.strip_prefix("minecraft:").unwrap_or(kind) {
                    $($name => Ok(Self::$variant(<$ty as FeatureType>::parse(json, parser)?)),)*
                    other => Err(Error::new(format!("unknown feature type {other}"))),
                }
            }

            /// `Feature.place`.
            pub fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
                match self {
                    $(Self::$variant(f) => f.place(ctx, origin),)*
                }
            }
        }
    };
}

features! {
    "bamboo" => Bamboo(kinds::vegetation::BambooFeature),
    "block_blob" => BlockBlob(kinds::terrain::BlockBlobFeature),
    "block_column" => BlockColumn(kinds::vegetation::BlockColumnFeature),
    "block_pile" => BlockPile(kinds::vegetation::BlockPileFeature),
    "blue_ice" => BlueIce(kinds::terrain::BlueIceFeature),
    "bonus_chest" => BonusChest(kinds::end::BonusChestFeature),
    "chorus_plant" => ChorusPlant(kinds::end::ChorusPlantFeature),
    "coral_claw" => CoralClaw(kinds::tree::coral::CoralClawFeature),
    "coral_tree" => CoralTree(kinds::tree::coral::CoralTreeFeature),
    "delta_feature" => Delta(kinds::end::DeltaFeature),
    "disk" => Disk(kinds::terrain::DiskFeature),
    "end_gateway" => EndGateway(kinds::end::EndGatewayFeature),
    "end_island" => EndIsland(kinds::end::EndIslandFeature),
    "end_platform" => EndPlatform(kinds::end::EndPlatformFeature),
    "end_podium" => EndPodium(kinds::end::EndPodiumFeature),
    "end_spike" => EndSpike(kinds::end::EndSpikeFeature),
    "fallen_tree" => FallenTree(kinds::tree::fallen::FallenTreeFeature),
    "fill_layer" => FillLayer(kinds::terrain::FillLayerFeature),
    "fossil" => Fossil(kinds::template::FossilFeature),
    "freeze_top_layer" => FreezeTopLayer(kinds::terrain::SnowAndFreezeFeature),
    "geode" => Geode(kinds::terrain::GeodeFeature),
    "huge_brown_mushroom" => HugeBrownMushroom(kinds::tree::mushroom::HugeBrownMushroomFeature),
    "huge_fungus" => HugeFungus(kinds::tree::fungus::HugeFungusFeature),
    "huge_red_mushroom" => HugeRedMushroom(kinds::tree::mushroom::HugeRedMushroomFeature),
    "iceberg" => Iceberg(kinds::terrain::IcebergFeature),
    "lake" => Lake(kinds::terrain::LakeFeature),
    "large_dripstone" => LargeDripstone(kinds::terrain::LargeDripstoneFeature),
    "monster_room" => MonsterRoom(kinds::terrain::MonsterRoomFeature),
    "multiface_growth" => MultifaceGrowth(kinds::vegetation::MultifaceGrowthFeature),
    "netherrack_replace_blobs" => ReplaceBlobs(kinds::terrain::ReplaceBlobsFeature),
    "no_op" => NoOp(kinds::selector::NoOpFeature),
    "ore" => Ore(kinds::terrain::OreFeature),
    "overlay" => Overlay(kinds::terrain::OverlayFeature),
    "projected_random_patchy_square" => ProjectedRandomPatchySquare(kinds::vegetation::ProjectedRandomPatchySquare),
    "random_boolean_selector" => RandomBooleanSelector(kinds::selector::RandomBooleanSelectorFeature),
    "random_neighbor_spread" => RandomNeighborSpread(kinds::vegetation::RandomNeighborSpreadFeature),
    "random_selector" => RandomSelector(kinds::selector::RandomSelectorFeature),
    "replace_single_block" => ReplaceSingleBlock(kinds::terrain::ReplaceBlockFeature),
    "root_system" => RootSystem(kinds::terrain::RootSystemFeature),
    "scattered_ore" => ScatteredOre(kinds::terrain::ScatteredOreFeature),
    "sculk_patch" => SculkPatch(kinds::terrain::SculkPatchFeature),
    "sequence" => Sequence(kinds::selector::SequenceFeature),
    "simple_block" => SimpleBlock(kinds::vegetation::SimpleBlockFeature),
    "simple_random_selector" => SimpleRandomSelector(kinds::selector::SimpleRandomSelectorFeature),
    "single_block_pillar" => SingleBlockPillar(kinds::vegetation::SingleBlockPillarFeature),
    "speleothem" => Speleothem(kinds::terrain::SpeleothemFeature),
    "speleothem_cluster" => SpeleothemCluster(kinds::terrain::SpeleothemClusterFeature),
    "spike" => Spike(kinds::terrain::SpikeFeature),
    "spring_feature" => Spring(kinds::terrain::SpringFeature),
    "stepped_column_cluster" => SteppedColumnCluster(kinds::end::SteppedColumnClusterFeature),
    "template" => Template(kinds::template::TemplateFeature),
    "tree" => Tree(kinds::tree::TreeFeature),
    "underwater_magma" => UnderwaterMagma(kinds::terrain::UnderwaterMagmaFeature),
    "vegetation_patch" => VegetationPatch(kinds::vegetation::VegetationPatchFeature),
    "vines" => Vines(kinds::vegetation::VinesFeature),
    "void_start_platform" => VoidStartPlatform(kinds::end::VoidStartPlatformFeature),
    "waterlogged_vegetation_patch" => WaterloggedVegetationPatch(kinds::vegetation::WaterloggedVegetationPatchFeature),
    "weighted_random_selector" => WeightedRandomSelector(kinds::selector::WeightedRandomSelectorFeature),
}

/// A placed feature (`worldgen/placed_feature` entry or inline definition).
#[derive(Debug)]
pub struct PlacedFeature {
    /// Registry name, `None` for inline definitions.
    pub name: Option<String>,
    pub feature: FeatureId,
    pub placement: Vec<PlacementModifier>,
}

/// Every feature and placed feature of a dimension, parsed once.
#[derive(Debug, Default)]
pub struct FeatureRegistry {
    features: Vec<Feature>,
    feature_names: Vec<Option<String>>,
    placed: Vec<PlacedFeature>,
    /// Named placed features by registry name.
    placed_ids: FxHashMap<String, PlacedFeatureId>,
}

impl FeatureRegistry {
    /// A feature by id.
    pub fn feature(&self, id: FeatureId) -> &Feature {
        &self.features[id.0 as usize]
    }

    /// Registry name of a feature (`None` for inline ones).
    pub fn feature_name(&self, id: FeatureId) -> Option<&str> {
        self.feature_names[id.0 as usize].as_deref()
    }

    /// A placed feature by id.
    pub fn placed(&self, id: PlacedFeatureId) -> &PlacedFeature {
        &self.placed[id.0 as usize]
    }

    /// Number of placed features.
    pub fn placed_len(&self) -> usize {
        self.placed.len()
    }

    /// A named placed feature by registry name (namespace optional).
    pub fn placed_id(&self, name: &str) -> Option<PlacedFeatureId> {
        self.placed_ids.get(&qualify(name)).copied()
    }
}

/// Parses features, placed features and everything they contain, resolving references to
/// registry entries once (each named entry is parsed exactly once).
#[derive(Debug)]
pub struct FeatureParser<'a> {
    packs: &'a LoadedPacks,
    biomes: &'a BiomeRegistry,
    random_state: &'a RandomState,
    registry: FeatureRegistry,
    feature_ids: FxHashMap<String, FeatureId>,
    placed_ids: FxHashMap<String, PlacedFeatureId>,
    /// Named block state providers (`worldgen/block_state_provider`), parsed on first use.
    providers: FxHashMap<String, BlockStateProvider>,
    /// Structure templates and processor lists (`fossil` and `template` features).
    templates: crate::template::TemplateLoader,
}

impl<'a> FeatureParser<'a> {
    /// A parser over the data packs.
    pub fn new(
        packs: &'a LoadedPacks,
        biomes: &'a BiomeRegistry,
        random_state: &'a RandomState,
    ) -> Self {
        Self {
            packs,
            biomes,
            random_state,
            registry: FeatureRegistry::default(),
            feature_ids: FxHashMap::default(),
            placed_ids: FxHashMap::default(),
            providers: FxHashMap::default(),
            templates: crate::template::TemplateLoader::new(packs),
        }
    }

    /// The data packs.
    pub fn packs(&self) -> &'a LoadedPacks {
        self.packs
    }

    /// The biome registry (for biome predicates).
    pub fn biomes(&self) -> &'a BiomeRegistry {
        self.biomes
    }

    /// The dimension's random state (for noise based providers that read its noises).
    pub fn random_state(&self) -> &'a RandomState {
        self.random_state
    }

    /// `StructureTemplateManager.getOrCreate(id)`: a structure template, loaded once.
    pub fn structure_template(
        &self,
        id: &str,
    ) -> Result<std::sync::Arc<crate::template::StructureTemplate>, Error> {
        self.templates.template(id)
    }

    /// A `Holder<StructureProcessorList>`: a registry id or an inline list.
    pub fn processor_list(
        &mut self,
        json: &Value,
    ) -> Result<std::sync::Arc<crate::template::ProcessorList>, Error> {
        self.templates.processor_list(self.packs, json)
    }

    /// The finished registry.
    pub fn finish(mut self) -> FeatureRegistry {
        self.registry.placed_ids = self.placed_ids;
        self.registry
    }

    fn registry_json(&self, registry: &str, id: &str) -> Result<Value, Error> {
        let entry = self
            .packs
            .registry(registry)
            .and_then(|r| r.get(id))
            .ok_or_else(|| Error::new(format!("unknown {registry} entry {id}")))?;
        serde_json::from_str(entry.json.get()).map_err(|e| Error::new(format!("{id}: {e}")))
    }

    /// A `Holder<Feature>`: a registry id or an inline definition.
    pub fn feature(&mut self, json: &Value) -> Result<FeatureId, Error> {
        if let Some(name) = json.as_str() {
            let name = qualify(name);
            if let Some(&id) = self.feature_ids.get(&name) {
                return Ok(id);
            }
            let definition = self.registry_json("minecraft:worldgen/feature", &name)?;
            let feature = self
                .parse_feature(&definition)
                .map_err(|e| Error::new(format!("feature {name}: {e}")))?;
            let id = self.push_feature(feature, Some(name.clone()));
            self.feature_ids.insert(name, id);
            return Ok(id);
        }
        let feature = self.parse_feature(json)?;
        Ok(self.push_feature(feature, None))
    }

    fn push_feature(&mut self, feature: Feature, name: Option<String>) -> FeatureId {
        self.registry.features.push(feature);
        self.registry.feature_names.push(name);
        FeatureId(self.registry.features.len() as u32 - 1)
    }

    fn parse_feature(&mut self, json: &Value) -> Result<Feature, Error> {
        let kind = json
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::new("feature without type"))?;
        Feature::parse_typed(kind, json, self)
    }

    /// A `HolderSet<Feature>`: a list of holders, one holder or a `#tag`.
    pub fn feature_list(&mut self, json: &Value) -> Result<Vec<FeatureId>, Error> {
        self.holder_set(json, "minecraft:worldgen/feature", Self::feature)
    }

    /// A `Holder<PlacedFeature>`: a registry id or an inline definition.
    pub fn placed(&mut self, json: &Value) -> Result<PlacedFeatureId, Error> {
        if let Some(name) = json.as_str() {
            let name = qualify(name);
            if let Some(&id) = self.placed_ids.get(&name) {
                return Ok(id);
            }
            let definition = self.registry_json("minecraft:worldgen/placed_feature", &name)?;
            let placed = self
                .parse_placed(&definition, Some(name.clone()))
                .map_err(|e| Error::new(format!("placed feature {name}: {e}")))?;
            let id = self.push_placed(placed);
            self.placed_ids.insert(name, id);
            return Ok(id);
        }
        let placed = self.parse_placed(json, None)?;
        Ok(self.push_placed(placed))
    }

    fn push_placed(&mut self, placed: PlacedFeature) -> PlacedFeatureId {
        self.registry.placed.push(placed);
        PlacedFeatureId(self.registry.placed.len() as u32 - 1)
    }

    fn parse_placed(&mut self, json: &Value, name: Option<String>) -> Result<PlacedFeature, Error> {
        let feature = self.feature(field(json, "feature")?)?;
        let placement = self.placement_list(json.get("placement").unwrap_or(&Value::Null))?;
        Ok(PlacedFeature {
            name,
            feature,
            placement,
        })
    }

    /// A `HolderSet<PlacedFeature>`.
    pub fn placed_list(&mut self, json: &Value) -> Result<Vec<PlacedFeatureId>, Error> {
        self.holder_set(json, "minecraft:worldgen/placed_feature", Self::placed)
    }

    /// A list of placement modifiers (missing = empty).
    pub fn placement_list(&mut self, json: &Value) -> Result<Vec<PlacementModifier>, Error> {
        match json {
            Value::Null => Ok(Vec::new()),
            Value::Array(items) => items.iter().map(|m| self.placement(m)).collect(),
            other => Ok(vec![self.placement(other)?]),
        }
    }

    /// One placement modifier.
    pub fn placement(&mut self, json: &Value) -> Result<PlacementModifier, Error> {
        PlacementModifier::parse(json, self)
    }

    /// A block predicate.
    pub fn block_predicate(&mut self, json: &Value) -> Result<BlockPredicate, Error> {
        BlockPredicate::parse(json, self)
    }

    /// A `Holder<BlockStateProvider>`: a registry id, a block state or an inline provider.
    pub fn state_provider(&mut self, json: &Value) -> Result<BlockStateProvider, Error> {
        if let Some(name) = json.as_str() {
            let name = qualify(name);
            if let Some(provider) = self.providers.get(&name) {
                return Ok(provider.clone());
            }
            let exists = self
                .packs
                .registry("minecraft:worldgen/block_state_provider")
                .and_then(|r| r.get(&name))
                .is_some();
            if exists {
                let definition =
                    self.registry_json("minecraft:worldgen/block_state_provider", &name)?;
                let provider = BlockStateProvider::parse(&definition, self)
                    .map_err(|e| Error::new(format!("block state provider {name}: {e}")))?;
                self.providers.insert(name, provider.clone());
                return Ok(provider);
            }
        }
        BlockStateProvider::parse(json, self)
    }

    /// A block tag (`#` optional) resolved to a set.
    pub fn block_tag(&self, tag: &str) -> BlockSet {
        BlockSet::from_tag(self.packs, &qualify(tag.trim_start_matches('#')))
    }

    /// A `HolderSet<Block>`: a block id, a list of ids or a `#tag`.
    pub fn block_set(&self, json: &Value) -> Result<BlockSet, Error> {
        crate::blocks::parse_block_set(self.packs, json)
    }

    /// A `HolderSet<Biome>` resolved to biome ids.
    pub fn biome_set(&self, json: &Value) -> Result<Vec<BiomeId>, Error> {
        let names = self.resolve_set(json, "minecraft:worldgen/biome")?;
        names.iter().map(|n| self.biomes.require(n)).collect()
    }

    /// The entry names of a `HolderSet` of a dynamic registry given by name, list or `#tag`.
    pub fn resolve_set(&self, json: &Value, registry: &str) -> Result<Vec<String>, Error> {
        match json {
            Value::String(s) if s.starts_with('#') => {
                let tag = qualify(&s[1..]);
                self.packs
                    .tags(registry)
                    .and_then(|t| t.get(&tag))
                    .cloned()
                    .ok_or_else(|| Error::new(format!("unknown tag #{tag} of {registry}")))
            }
            Value::String(s) => Ok(vec![qualify(s)]),
            Value::Array(items) => items
                .iter()
                .map(|i| {
                    i.as_str()
                        .map(qualify)
                        .ok_or_else(|| Error::new(format!("invalid holder set entry {i}")))
                })
                .collect(),
            other => Err(Error::new(format!("invalid holder set {other}"))),
        }
    }

    fn holder_set<T>(
        &mut self,
        json: &Value,
        registry: &str,
        mut one: impl FnMut(&mut Self, &Value) -> Result<T, Error>,
    ) -> Result<Vec<T>, Error> {
        match json {
            Value::String(s) if s.starts_with('#') => {
                let names = self.resolve_set(json, registry)?;
                names
                    .iter()
                    .map(|n| one(self, &Value::String(n.clone())))
                    .collect()
            }
            Value::Array(items) => items.iter().map(|i| one(self, i)).collect(),
            other => Ok(vec![one(self, other)?]),
        }
    }
}

/// A required JSON field.
pub fn field<'v>(json: &'v Value, name: &str) -> Result<&'v Value, Error> {
    json.get(name)
        .ok_or_else(|| Error::new(format!("missing field {name}")))
}

/// A required integer field.
pub fn int_field(json: &Value, name: &str) -> Result<i32, Error> {
    field(json, name)?
        .as_i64()
        .map(|v| v as i32)
        .ok_or_else(|| Error::new(format!("field {name} is not an integer")))
}

/// An optional integer field with a default.
pub fn int_or(json: &Value, name: &str, default: i32) -> i32 {
    json.get(name)
        .and_then(Value::as_i64)
        .map_or(default, |v| v as i32)
}

/// A required float field (`float` in Java).
pub fn float_field(json: &Value, name: &str) -> Result<f32, Error> {
    field(json, name)?
        .as_f64()
        .map(|v| v as f32)
        .ok_or_else(|| Error::new(format!("field {name} is not a number")))
}

/// An optional float field with a default.
pub fn float_or(json: &Value, name: &str, default: f32) -> f32 {
    json.get(name)
        .and_then(Value::as_f64)
        .map_or(default, |v| v as f32)
}

/// An optional boolean field with a default.
pub fn bool_or(json: &Value, name: &str, default: bool) -> bool {
    json.get(name).and_then(Value::as_bool).unwrap_or(default)
}

/// The per-dimension data the features stage reads besides the region.
#[derive(Debug)]
pub struct FeatureEnv {
    /// Parsed features.
    pub registry: FeatureRegistry,
    /// `WorldGenerationContext` of the generator and level (height providers, anchors).
    pub context: WorldGenerationContext,
    /// Placed features per biome id (`BiomeGenerationSettings.hasFeature`, for the biome filter).
    pub biome_feature_sets: Vec<FxHashSet<PlacedFeatureId>>,
}

/// What a placing feature works with: the region, the dimension's feature data and the
/// decoration random.
#[derive(Debug)]
pub struct FeatureContext<'r, 'a> {
    pub level: &'r mut WorldGenRegion<'a>,
    pub env: &'r FeatureEnv,
    pub random: &'r mut DecorationRandom,
}

impl FeatureContext<'_, '_> {
    /// `Holder<Feature>.value().place(level, generator, random, origin)`.
    pub fn place_feature(&mut self, id: FeatureId, origin: BlockPos) -> bool {
        let env = self.env;
        env.registry.feature(id).place(self, origin)
    }

    /// `PlacedFeature.place(level, generator, random, origin)`: without biome check.
    pub fn place_placed(&mut self, id: PlacedFeatureId, origin: BlockPos) -> bool {
        place_placed(self, id, origin, None)
    }
}

/// `FeaturePlacer.place`: runs the placement modifiers depth first, in Vanilla's order, and
/// places the feature at every resulting position.
pub fn place_placed(
    ctx: &mut FeatureContext<'_, '_>,
    id: PlacedFeatureId,
    origin: BlockPos,
    top_feature: Option<PlacedFeatureId>,
) -> bool {
    let env = ctx.env;
    let placed = env.registry.placed(id);
    let feature = env.registry.feature(placed.feature);
    if placed.placement.is_empty() {
        return feature.place(ctx, origin);
    }
    let mut placed_any = false;
    let mut stack: Vec<(BlockPos, usize)> = vec![(origin, 0)];
    let mut modified = Vec::new();
    while let Some((pos, index)) = stack.pop() {
        modified.clear();
        {
            let placement_ctx = PlacementContext {
                level: ctx.level,
                env,
                top_feature,
            };
            placed.placement[index].modify(&placement_ctx, ctx.random, pos, &mut modified);
        }
        let next = index + 1;
        if next < placed.placement.len() {
            stack.extend(modified.iter().rev().map(|&p| (p, next)));
        } else {
            for &p in &modified {
                placed_any |= feature.place(ctx, p);
            }
        }
    }
    placed_any
}

/// Helper for kinds that keep their (unported) configuration: always places nothing.
///
/// Used as the body of feature types whose port is not finished yet, so the registry parses
/// every Vanilla feature.
#[macro_export]
macro_rules! unported_feature {
    ($ty:ident) => {
        #[doc = "Not ported yet: parses its configuration and places nothing."]
        #[derive(Debug)]
        pub struct $ty;

        impl $crate::feature::FeatureType for $ty {
            fn parse(
                _json: &serde_json::Value,
                _parser: &mut $crate::feature::FeatureParser<'_>,
            ) -> Result<Self, $crate::Error> {
                Ok(Self)
            }

            fn place(
                &self,
                _ctx: &mut $crate::feature::FeatureContext<'_, '_>,
                _origin: vanillite_base::BlockPos,
            ) -> bool {
                false
            }
        }
    };
}

/// The decoration steps (`GenerationStep.Decoration`) in order.
pub const DECORATION_STEPS: usize = 11;

/// The features of a dimension: the registry plus Vanilla's global per-step order.
#[derive(Debug)]
pub struct FeatureSet {
    pub env: FeatureEnv,
    /// Per step: placed features in global order (`FeatureSorter.StepFeatureData.features`).
    pub steps: Vec<Vec<PlacedFeatureId>>,
    /// Per step: global index of each placed feature in that step.
    pub step_index: Vec<FxHashMap<PlacedFeatureId, i32>>,
    /// Per biome id, per step: the biome's placed features.
    pub biome_steps: Vec<Vec<Vec<PlacedFeatureId>>>,
    /// The biome source's possible biomes (`possibleBiomes()`), as a lookup.
    pub possible_biomes: FxHashSet<BiomeId>,
}

impl FeatureSet {
    /// Parses every feature the biomes of `possible_biomes` reference and sorts them like
    /// `FeatureSorter.buildFeaturesPerStep(possibleBiomes, ...)`.
    pub fn load(
        packs: &LoadedPacks,
        biomes: &BiomeRegistry,
        random_state: &RandomState,
        possible_biomes: &[BiomeId],
        context: WorldGenerationContext,
    ) -> Result<Self, Error> {
        crate::behaviour::init(packs);
        let mut parser = FeatureParser::new(packs, biomes, random_state);
        let mut biome_steps = vec![Vec::new(); biomes.all().len()];
        for biome in biomes.all() {
            let steps = biome
                .json
                .get("features")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let mut parsed = Vec::with_capacity(steps.len());
            for step in steps {
                parsed.push(
                    parser
                        .placed_list(step)
                        .map_err(|e| Error::new(format!("biome {}: {e}", biome.name)))?,
                );
            }
            biome_steps[usize::from(biome.id)] = parsed;
        }
        // The placed features no biome lists (placed by other features or structures), so every
        // placed feature of the packs parses and has an id.
        if let Some(placed) = packs.registry("minecraft:worldgen/placed_feature") {
            for entry in placed.entries() {
                parser.placed(&Value::String(entry.id.clone()))?;
            }
        }
        let registry = parser.finish();
        let sources: Vec<&[Vec<PlacedFeatureId>]> = possible_biomes
            .iter()
            .map(|&b| biome_steps[usize::from(b)].as_slice())
            .collect();
        let steps = sorter::build_features_per_step(&sources)?;
        let step_index = steps
            .iter()
            .map(|features| {
                features
                    .iter()
                    .enumerate()
                    .map(|(i, &f)| (f, i as i32))
                    .collect()
            })
            .collect();
        let biome_feature_sets = biome_steps
            .iter()
            .map(|steps| steps.iter().flatten().copied().collect())
            .collect();
        Ok(Self {
            env: FeatureEnv {
                registry,
                context,
                biome_feature_sets,
            },
            steps,
            step_index,
            biome_steps,
            possible_biomes: possible_biomes.iter().copied().collect(),
        })
    }

    /// `FeaturePlacer.placeWithBiomeCheck` (`biome_check`) or `FeaturePlacer.place`: one placed
    /// feature at `origin` with the caller's random, as `applyBiomeDecoration` places each
    /// feature (for parity tests and debugging).
    pub fn place_one(
        &self,
        level: &mut WorldGenRegion<'_>,
        id: PlacedFeatureId,
        origin: BlockPos,
        random: &mut DecorationRandom,
        biome_check: bool,
    ) -> bool {
        let mut ctx = FeatureContext {
            level,
            env: &self.env,
            random,
        };
        place_placed(&mut ctx, id, origin, biome_check.then_some(id))
    }

    /// `applyBiomeDecoration` without structures: decorates the centre chunk of `level`.
    pub fn decorate(&self, level: &mut WorldGenRegion<'_>) {
        let center = level.center();
        let (origin_x, origin_z) = (center.min_block_x(), center.min_block_z());
        let origin = BlockPos::new(origin_x, level.min_y(), origin_z);
        let mut random = DecorationRandom::new(0);
        let decoration_seed = random.set_decoration_seed(level.seed(), origin_x, origin_z);

        let mut possible: Vec<BiomeId> = Vec::new();
        for dz in -1..=1 {
            for dx in -1..=1 {
                let chunk = level.chunk(BlockPos::new(origin_x + dx * 16, 0, origin_z + dz * 16));
                for section in chunk.sections() {
                    section.biomes().for_each_count(|biome, count| {
                        if count > 0
                            && self.possible_biomes.contains(&biome)
                            && !possible.contains(&biome)
                        {
                            possible.push(biome);
                        }
                    });
                }
            }
        }

        let mut indices: Vec<i32> = Vec::new();
        for (step, features) in self.steps.iter().enumerate() {
            indices.clear();
            for &biome in &possible {
                if let Some(list) = self.biome_steps[usize::from(biome)].get(step) {
                    for feature in list {
                        indices.push(self.step_index[step][feature]);
                    }
                }
            }
            indices.sort_unstable();
            indices.dedup();
            for &index in &indices {
                let id = features[index as usize];
                random.set_feature_seed(decoration_seed, index, step as i32);
                let mut ctx = FeatureContext {
                    level,
                    env: &self.env,
                    random: &mut random,
                };
                place_placed(&mut ctx, id, origin, Some(id));
            }
        }
    }
}
