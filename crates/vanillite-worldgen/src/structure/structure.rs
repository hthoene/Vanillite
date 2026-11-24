//! `Structure`: a configured structure (registry `worldgen/structure`), its generation context
//! and the generation helpers of the Vanilla base class.

use std::fmt;

use serde_json::Value;
use vanillite_base::{BlockPos, ChunkPos};
use vanillite_chunk::{BiomeId, HeightmapKind};

use super::kinds;
use super::piece::{PieceContext, StructurePiece};
use super::pieces::StructurePiecesBuilder;
use super::{BiomeFilter, Decoration, LevelHeight, StructureParser, TerrainAdjustment};
use crate::Error;
use crate::biome::BiomeResolver;
use crate::biome::source::Resolver;
use crate::density::SamplerContext;
use crate::feature::field;
use crate::generator::TerrainGenerator;
use crate::random::WorldgenRandom;
use crate::template::{BoundingBox, Rotation};

/// A structure type: parsed from its JSON, finds where (and whether) a start generates.
///
/// Implementations port one Vanilla `Structure` subclass each; `find_generation_point` is
/// `findGenerationPoint` and `after_place` is `afterPlace` (a `PostPlacementProcessor`).
pub trait StructureType: Sized + fmt::Debug + Send + Sync {
    /// Whether the type is ported: `false` for stubs, which never generate (parity tests report
    /// their structures separately).
    const PORTED: bool = true;

    /// Parses the type's own fields (the structure's JSON object; the shared settings are
    /// parsed by the caller).
    fn parse(json: &Value, parser: &mut StructureParser<'_>) -> Result<Self, Error>;

    /// `findGenerationPoint(context)`: the start position and the piece generator, or `None`.
    fn find_generation_point<'s>(
        &'s self,
        ctx: &mut GenerationContext<'_>,
    ) -> Option<GenerationStub<'s>>;

    /// `afterPlace(level, structureManager, generator, random, chunkBB, chunkPos, pieces)`:
    /// runs after the pieces of a start were placed in a chunk.
    fn after_place(
        &self,
        _ctx: &mut PieceContext<'_, '_>,
        _chunk_bb: &BoundingBox,
        _chunk_pos: ChunkPos,
        _pieces: &mut [Box<dyn StructurePiece>],
    ) {
    }
}

macro_rules! structure_types {
    ($($name:literal => $variant:ident($ty:ty),)*) => {
        /// The type of a structure with its configuration (`StructureType` registry).
        #[derive(Debug)]
        pub enum StructureKind {
            $(
                #[doc = concat!("`minecraft:", $name, "`.")]
                $variant($ty),
            )*
        }

        impl StructureKind {
            fn parse_typed(
                kind: &str,
                json: &Value,
                parser: &mut StructureParser<'_>,
            ) -> Result<Self, Error> {
                match kind.strip_prefix("minecraft:").unwrap_or(kind) {
                    $($name => Ok(Self::$variant(<$ty as StructureType>::parse(json, parser)?)),)*
                    other => Err(Error::new(format!("unknown structure type {other}"))),
                }
            }

            /// Whether the type is ported ([`StructureType::PORTED`]).
            pub fn is_ported(&self) -> bool {
                match self {
                    $(Self::$variant(_) => <$ty as StructureType>::PORTED,)*
                }
            }

            /// `findGenerationPoint(context)`.
            pub fn find_generation_point<'s>(
                &'s self,
                ctx: &mut GenerationContext<'_>,
            ) -> Option<GenerationStub<'s>> {
                match self {
                    $(Self::$variant(s) => s.find_generation_point(ctx),)*
                }
            }

            /// `afterPlace(...)`.
            pub fn after_place(
                &self,
                ctx: &mut PieceContext<'_, '_>,
                chunk_bb: &BoundingBox,
                chunk_pos: ChunkPos,
                pieces: &mut [Box<dyn StructurePiece>],
            ) {
                match self {
                    $(Self::$variant(s) => s.after_place(ctx, chunk_bb, chunk_pos, pieces),)*
                }
            }
        }
    };
}

structure_types! {
    "buried_treasure" => BuriedTreasure(kinds::buried_treasure::BuriedTreasureStructure),
    "desert_pyramid" => DesertPyramid(kinds::desert_pyramid::DesertPyramidStructure),
    "end_city" => EndCity(kinds::end_city::EndCityStructure),
    "fortress" => Fortress(kinds::nether_fortress::NetherFortressStructure),
    "igloo" => Igloo(kinds::igloo::IglooStructure),
    "jigsaw" => Jigsaw(kinds::jigsaw::JigsawStructure),
    "jungle_temple" => JungleTemple(kinds::jungle_temple::JungleTempleStructure),
    "mineshaft" => Mineshaft(kinds::mineshaft::MineshaftStructure),
    "nether_fossil" => NetherFossil(kinds::nether_fossil::NetherFossilStructure),
    "ocean_monument" => OceanMonument(kinds::ocean_monument::OceanMonumentStructure),
    "ocean_ruin" => OceanRuin(kinds::ocean_ruin::OceanRuinStructure),
    "ruined_portal" => RuinedPortal(kinds::ruined_portal::RuinedPortalStructure),
    "shipwreck" => Shipwreck(kinds::shipwreck::ShipwreckStructure),
    "stronghold" => Stronghold(kinds::stronghold::StrongholdStructure),
    "swamp_hut" => SwampHut(kinds::swamp_hut::SwampHutStructure),
    "woodland_mansion" => WoodlandMansion(kinds::woodland_mansion::WoodlandMansionStructure),
}

/// `Structure.StructureSettings`. `spawn_overrides` is parsed for validity and dropped: it only
/// matters for mob spawning, not for generation.
#[derive(Clone, Debug)]
pub struct StructureSettings {
    pub biomes: BiomeFilter,
    pub step: Decoration,
    pub terrain_adaptation: TerrainAdjustment,
}

impl StructureSettings {
    fn parse(json: &Value, parser: &StructureParser<'_>) -> Result<Self, Error> {
        let biomes = parser.biome_filter(field(json, "biomes")?)?;
        if !field(json, "spawn_overrides")?.is_object() {
            return Err(Error::new("spawn_overrides is not a map"));
        }
        let step = field(json, "step")?
            .as_str()
            .and_then(Decoration::from_name)
            .ok_or_else(|| Error::new("invalid structure step"))?;
        let terrain_adaptation = match json.get("terrain_adaptation").and_then(Value::as_str) {
            Some(name) => TerrainAdjustment::from_name(name)?,
            None => TerrainAdjustment::None,
        };
        Ok(Self {
            biomes,
            step,
            terrain_adaptation,
        })
    }
}

/// A configured structure (`worldgen/structure` entry).
#[derive(Debug)]
pub struct Structure {
    /// Registry name.
    pub name: String,
    pub settings: StructureSettings,
    pub kind: StructureKind,
}

impl Structure {
    /// Parses a `worldgen/structure` entry (`Structure.DIRECT_CODEC`).
    pub(crate) fn parse(
        name: String,
        json: &Value,
        parser: &mut StructureParser<'_>,
    ) -> Result<Self, Error> {
        let kind = json
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::new("structure without type"))?;
        let settings = StructureSettings::parse(json, parser)?;
        let kind = StructureKind::parse_typed(kind, json, parser)?;
        Ok(Self {
            name,
            settings,
            kind,
        })
    }

    /// `step()`.
    pub fn step(&self) -> Decoration {
        self.settings.step
    }

    /// `terrainAdaptation()`.
    pub fn terrain_adaptation(&self) -> TerrainAdjustment {
        self.settings.terrain_adaptation
    }

    /// `adjustBoundingBox(box)`: inflated by 12 with terrain adaptation.
    pub fn adjust_bounding_box(&self, bounding_box: BoundingBox) -> BoundingBox {
        if self.terrain_adaptation() == TerrainAdjustment::None {
            bounding_box
        } else {
            bounding_box.inflated_by(12, 12, 12)
        }
    }

    /// `generate(...)` up to the pieces: `findValidGenerationPoint` and the pieces of the stub,
    /// or `None` (`INVALID_START`) when there is no valid point or no piece.
    pub fn generate_pieces(
        &self,
        ctx: &mut GenerationContext<'_>,
    ) -> Option<Vec<Box<dyn StructurePiece>>> {
        let stub = self.find_valid_generation_point(ctx)?;
        let builder = stub.into_pieces_builder(ctx);
        let pieces = builder.build();
        (!pieces.is_empty()).then_some(pieces)
    }

    /// `findValidGenerationPoint(context)`: the generation point if its biome is valid.
    pub fn find_valid_generation_point<'s>(
        &'s self,
        ctx: &mut GenerationContext<'_>,
    ) -> Option<GenerationStub<'s>> {
        let stub = self.kind.find_generation_point(ctx)?;
        ctx.is_valid_biome(stub.position).then_some(stub)
    }
}

/// The piece generator of a lazy [`GenerationStub`] (Vanilla's `Consumer<StructurePiecesBuilder>`
/// with the context it captured passed in).
pub type PieceGenerator<'s> =
    Box<dyn FnOnce(&mut StructurePiecesBuilder, &mut GenerationContext<'_>) + 's>;

/// The pieces of a [`GenerationStub`]: generated on demand (`Either.left`) or already built
/// (`Either.right`, as jigsaw structures do).
pub enum StubPieces<'s> {
    /// `Consumer<StructurePiecesBuilder>`: runs only once the stub's biome was found valid.
    Lazy(PieceGenerator<'s>),
    /// A builder filled while the point was searched.
    Built(StructurePiecesBuilder),
}

impl fmt::Debug for StubPieces<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lazy(_) => f.write_str("Lazy"),
            Self::Built(builder) => f.debug_tuple("Built").field(builder).finish(),
        }
    }
}

/// `Structure.GenerationStub`: where a start generates and how its pieces are made.
#[derive(Debug)]
pub struct GenerationStub<'s> {
    pub position: BlockPos,
    pub pieces: StubPieces<'s>,
}

impl<'s> GenerationStub<'s> {
    /// A stub whose pieces are generated on demand.
    pub fn lazy(
        position: BlockPos,
        generator: impl FnOnce(&mut StructurePiecesBuilder, &mut GenerationContext<'_>) + 's,
    ) -> Self {
        Self {
            position,
            pieces: StubPieces::Lazy(Box::new(generator)),
        }
    }

    /// A stub with already generated pieces.
    pub fn built(position: BlockPos, builder: StructurePiecesBuilder) -> Self {
        Self {
            position,
            pieces: StubPieces::Built(builder),
        }
    }

    /// `getPiecesBuilder()`.
    pub fn into_pieces_builder(self, ctx: &mut GenerationContext<'_>) -> StructurePiecesBuilder {
        match self.pieces {
            StubPieces::Lazy(generator) => {
                let mut builder = StructurePiecesBuilder::new();
                generator(&mut builder, ctx);
                builder
            }
            StubPieces::Built(builder) => builder,
        }
    }
}

/// `Structure.GenerationContext`: what a structure sees while it looks for its start.
///
/// The random is `WorldgenRandom(LegacyRandomSource)` seeded with
/// `setLargeFeatureSeed(seed, chunkX, chunkZ)`; the biome resolver samples the climate per quart
/// (`biomeSource.createResolver(climateSampler)`).
pub struct GenerationContext<'a> {
    /// The chunk generator (`chunkGenerator()`, `biomeSource()`, `randomState()`).
    pub generator: &'a TerrainGenerator,
    /// `random()`.
    pub random: WorldgenRandom,
    /// `seed()`: the world seed.
    pub seed: i64,
    /// `chunkPos()`: the start chunk.
    pub chunk_pos: ChunkPos,
    /// `heightAccessor()`: the height range of the start chunk.
    pub height: LevelHeight,
    valid_biomes: &'a BiomeFilter,
    biome_resolver: Resolver<'a>,
}

impl fmt::Debug for GenerationContext<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GenerationContext")
            .field("chunk_pos", &self.chunk_pos)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

impl<'a> GenerationContext<'a> {
    /// `new GenerationContext(...)` for a start in `chunk_pos` of a structure with
    /// `valid_biomes`.
    pub fn new(
        generator: &'a TerrainGenerator,
        chunk_pos: ChunkPos,
        height: LevelHeight,
        valid_biomes: &'a BiomeFilter,
    ) -> Self {
        let seed = generator.seed();
        let mut random = WorldgenRandom::new(0);
        random.set_large_feature_seed(seed, chunk_pos.x, chunk_pos.z);
        let biome_resolver = generator.biome_source().create_resolver(
            generator.program(),
            generator.climate(),
            SamplerContext::new(true),
        );
        Self {
            generator,
            random,
            seed,
            chunk_pos,
            height,
            valid_biomes,
            biome_resolver,
        }
    }

    /// `validBiome().test(biome)`.
    pub fn valid_biome(&self, biome: BiomeId) -> bool {
        self.valid_biomes.contains(biome)
    }

    /// `biomeResolver().getNoiseBiome(quartX, quartY, quartZ)`.
    pub fn noise_biome(&mut self, quart_x: i32, quart_y: i32, quart_z: i32) -> BiomeId {
        self.biome_resolver.noise_biome(quart_x, quart_y, quart_z)
    }

    /// `isValidBiome(stub)` for the stub's position.
    pub fn is_valid_biome(&mut self, position: BlockPos) -> bool {
        let biome = self.noise_biome(position.x >> 2, position.y >> 2, position.z >> 2);
        self.valid_biome(biome)
    }

    /// `couldStructureExistInColumn(blockX, blockZ, minBlockY, maxBlockY)`.
    pub fn could_structure_exist_in_column(
        &self,
        block_x: i32,
        block_z: i32,
        min_block_y: i32,
        max_block_y: i32,
    ) -> bool {
        let (quart_x, quart_z) = (block_x >> 2, block_z >> 2);
        let (min_quart_y, max_quart_y) = (min_block_y >> 2, max_block_y >> 2);
        let generator = self.generator;
        let mut resolver = generator.biome_source().create_resolver_for_chunk(
            generator.program(),
            generator.climate(),
            SamplerContext::new(true),
            (quart_x, min_quart_y, quart_z),
            (1, max_quart_y - min_quart_y + 1, 1),
        );
        (min_quart_y..=max_quart_y)
            .any(|quart_y| self.valid_biome(resolver.noise_biome(quart_x, quart_y, quart_z)))
    }

    /// `couldValidBiomeExistInTerrainColumn(blockX, blockZ)`.
    pub fn could_valid_biome_exist_in_terrain_column(&self, block_x: i32, block_z: i32) -> bool {
        self.could_structure_exist_in_column(
            block_x,
            block_z,
            self.height.min_y - 1,
            self.height.max_y(),
        )
    }

    /// `couldValidBiomeExistOnTopOfChunkCenter()`.
    pub fn could_valid_biome_exist_on_top_of_chunk_center(&self) -> bool {
        self.could_valid_biome_exist_in_terrain_column(
            self.chunk_pos.min_block_x() + 8,
            self.chunk_pos.min_block_z() + 8,
        )
    }

    /// `chunkGenerator().getFirstOccupiedHeight(x, z, heightmap, heightAccessor, randomState)`.
    pub fn first_occupied_height(&self, x: i32, z: i32, heightmap: HeightmapKind) -> i32 {
        self.generator
            .first_occupied_height(x, z, heightmap, self.height)
    }

    /// `chunkGenerator().getFirstFreeHeight(x, z, heightmap, heightAccessor, randomState)`.
    pub fn first_free_height(&self, x: i32, z: i32, heightmap: HeightmapKind) -> i32 {
        self.generator
            .first_free_height(x, z, heightmap, self.height)
    }
}

/// `onTopOfChunkCenter(context, heightmap, generator)`.
pub fn on_top_of_chunk_center<'s>(
    ctx: &GenerationContext<'_>,
    heightmap: HeightmapKind,
    generator: impl FnOnce(&mut StructurePiecesBuilder, &mut GenerationContext<'_>) + 's,
) -> Option<GenerationStub<'s>> {
    if !ctx.could_valid_biome_exist_on_top_of_chunk_center() {
        return None;
    }
    Some(on_top_of_chunk_center_without_biome_check(
        ctx, heightmap, generator,
    ))
}

/// `onTopOfChunkCenterWithoutBiomeCheck(context, heightmap, generator)`.
pub fn on_top_of_chunk_center_without_biome_check<'s>(
    ctx: &GenerationContext<'_>,
    heightmap: HeightmapKind,
    generator: impl FnOnce(&mut StructurePiecesBuilder, &mut GenerationContext<'_>) + 's,
) -> GenerationStub<'s> {
    let block_x = ctx.chunk_pos.min_block_x() + 8;
    let block_z = ctx.chunk_pos.min_block_z() + 8;
    let block_y = ctx.first_occupied_height(block_x, block_z, heightmap);
    GenerationStub::lazy(BlockPos::new(block_x, block_y, block_z), generator)
}

/// `getCornerHeights(context, minX, sizeX, minZ, sizeZ)`.
fn corner_heights(
    ctx: &GenerationContext<'_>,
    min_x: i32,
    size_x: i32,
    min_z: i32,
    size_z: i32,
) -> [i32; 4] {
    let at = |x, z| ctx.first_occupied_height(x, z, HeightmapKind::WorldSurfaceWg);
    [
        at(min_x, min_z),
        at(min_x, min_z + size_z),
        at(min_x + size_x, min_z),
        at(min_x + size_x, min_z + size_z),
    ]
}

/// `getMeanFirstOccupiedHeight(context, minX, sizeX, minZ, sizeZ)`.
pub fn mean_first_occupied_height(
    ctx: &GenerationContext<'_>,
    min_x: i32,
    size_x: i32,
    min_z: i32,
    size_z: i32,
) -> i32 {
    let [a, b, c, d] = corner_heights(ctx, min_x, size_x, min_z, size_z);
    (a + b + c + d) / 4
}

/// `getLowestY(context, sizeX, sizeZ)`: at the start chunk's minimum corner.
pub fn lowest_y(ctx: &GenerationContext<'_>, size_x: i32, size_z: i32) -> i32 {
    lowest_y_at(
        ctx,
        ctx.chunk_pos.min_block_x(),
        ctx.chunk_pos.min_block_z(),
        size_x,
        size_z,
    )
}

/// `getLowestY(context, minX, minZ, sizeX, sizeZ)`.
pub fn lowest_y_at(
    ctx: &GenerationContext<'_>,
    min_x: i32,
    min_z: i32,
    size_x: i32,
    size_z: i32,
) -> i32 {
    let [a, b, c, d] = corner_heights(ctx, min_x, size_x, min_z, size_z);
    a.min(b).min(c.min(d))
}

/// `getLowestYIn5by5Box(context, blockX, blockZ, rotation)`.
pub fn lowest_y_in_5by5_box(
    ctx: &GenerationContext<'_>,
    block_x: i32,
    block_z: i32,
    rotation: Rotation,
) -> BlockPos {
    let (offset_x, offset_z) = match rotation {
        Rotation::Clockwise90 => (-5, 5),
        Rotation::Clockwise180 => (-5, -5),
        Rotation::Counterclockwise90 => (5, -5),
        Rotation::None => (5, 5),
    };
    BlockPos::new(
        block_x,
        lowest_y_at(ctx, block_x, block_z, offset_x, offset_z),
        block_z,
    )
}

/// `SinglePieceStructure.findGenerationPoint`: one piece at the start chunk's minimum corner
/// (`construct(random, minBlockX, minBlockZ)`), if a valid biome may exist at the chunk centre
/// and the lowest corner of the `width`×`depth` area is not below the sea level.
pub fn single_piece_generation_point<'s>(
    ctx: &GenerationContext<'_>,
    width: i32,
    depth: i32,
    construct: impl FnOnce(&mut WorldgenRandom, i32, i32) -> Box<dyn StructurePiece> + 's,
) -> Option<GenerationStub<'s>> {
    if !ctx.could_valid_biome_exist_on_top_of_chunk_center() {
        return None;
    }
    if lowest_y(ctx, width, depth) < ctx.generator.sea_level() {
        return None;
    }
    Some(on_top_of_chunk_center_without_biome_check(
        ctx,
        HeightmapKind::WorldSurfaceWg,
        move |builder, ctx| {
            let (x, z) = (ctx.chunk_pos.min_block_x(), ctx.chunk_pos.min_block_z());
            builder.add_piece(construct(&mut ctx.random, x, z));
        },
    ))
}
