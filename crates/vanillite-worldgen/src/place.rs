//! What `/place` generates outside the chunk pipeline (`PlaceCommand`): configured features,
//! jigsaw pools and structure starts. The caller provides the [`WorldGenRegion`] (a copy of
//! the live chunks) and applies what it recorded to the live world.

use serde_json::{Value, json};
use vanillite_base::random::DecorationRandom;
use vanillite_base::{BlockPos, ChunkPos};
use vanillite_datapack::LoadedPacks;

use crate::Error;
use crate::feature::{FeatureContext, FeatureEnv, FeatureId, FeatureParser, WorldGenRegion};
use crate::generator::TerrainGenerator;
use crate::structure::kinds::jigsaw::JigsawStructure;
use crate::structure::kinds::jigsaw::placement::add_pieces;
use crate::structure::kinds::jigsaw::pools::PoolAliasLookup;
use crate::structure::piece::{PieceContext, StructurePiece};
use crate::structure::structure::StructureType;
use crate::structure::{BiomeFilter, GenerationContext, LevelHeight, StructureId, StructureStart};
use crate::template::BoundingBox;

/// Whether the data packs define an entry `id` of the dynamic registry `registry`.
pub fn has_entry(packs: &LoadedPacks, registry: &str, id: &str) -> bool {
    packs
        .registry(registry)
        .is_some_and(|r| r.get(id).is_some())
}

/// A configured feature (`minecraft:worldgen/configured_feature`) parsed on its own, with the
/// environment it is placed with.
#[derive(Debug)]
pub struct CommandFeature {
    env: FeatureEnv,
    id: FeatureId,
}

impl CommandFeature {
    /// Parses a configured feature for `generator`'s dimension: a registry id of the packs or
    /// an inline definition.
    pub fn load(
        generator: &TerrainGenerator,
        packs: &LoadedPacks,
        feature: &Value,
    ) -> Result<Self, Error> {
        let mut parser = FeatureParser::new(packs, generator.biomes(), generator.random_state());
        let id = parser.feature(feature)?;
        Ok(Self {
            env: FeatureEnv {
                registry: parser.finish(),
                context: generator.features().env.context,
                biome_feature_sets: Vec::new(),
            },
            id,
        })
    }

    /// `Feature.place(level, generator, random, pos)`.
    pub fn place(
        &self,
        region: &mut WorldGenRegion<'_>,
        random: &mut DecorationRandom,
        pos: BlockPos,
    ) -> bool {
        let mut ctx = FeatureContext {
            level: region,
            env: &self.env,
            random,
        };
        ctx.place_feature(self.id, pos)
    }
}

/// Every biome (`b -> true`).
fn any_biome(generator: &TerrainGenerator) -> BiomeFilter {
    let count = generator.biomes().all().len();
    BiomeFilter::of((0..count).filter_map(|i| u16::try_from(i).ok()))
}

/// `Structure.generate(...)` of `/place structure` at `chunk`: start reference count 0, every
/// biome valid. `None` for an invalid start.
pub fn structure_start(
    generator: &TerrainGenerator,
    id: StructureId,
    chunk: ChunkPos,
    level: LevelHeight,
) -> Option<StructureStart> {
    let structure = generator.structures().structure(id);
    let biomes = any_biome(generator);
    let mut ctx = GenerationContext::new(generator, chunk, level, &biomes);
    let pieces = structure.generate_pieces(&mut ctx)?;
    StructureStart::new(id, chunk, 0, pieces, |bb| structure.adjust_bounding_box(bb))
}

/// `StructureStart.placeInChunk` for every chunk of `chunks` (min and max corner) with the
/// chunk column boxes of `/place structure`.
pub fn place_structure(
    generator: &TerrainGenerator,
    start: &StructureStart,
    region: &mut WorldGenRegion<'_>,
    random: &mut DecorationRandom,
    chunks: (ChunkPos, ChunkPos),
) {
    let (min, max) = chunks;
    let (min_y, max_y) = (region.min_y(), region.max_y());
    // `ChunkPos.rangeClosed`: x outer, z inner.
    for x in min.x..=max.x {
        for z in min.z..=max.z {
            let chunk = ChunkPos::new(x, z);
            let mut chunk_bb = BoundingBox::new(
                chunk.min_block_x(),
                min_y,
                chunk.min_block_z(),
                chunk.min_block_x() + 15,
                max_y + 1,
                chunk.min_block_z() + 15,
            );
            let mut ctx = PieceContext {
                level: region,
                random,
                generator,
            };
            start.place_in_chunk(generator.structures(), &mut ctx, &mut chunk_bb, chunk);
        }
    }
}

/// `JigsawPlacement.generateJigsaw`'s pieces: `addPieces` of the pool `pool` from its jigsaw
/// `target` at `pos` with `max_depth`, a maximum distance of 128, no heightmap projection and
/// no aliases. `Ok(None)` when no start piece fits.
pub fn jigsaw_pieces(
    generator: &TerrainGenerator,
    packs: &LoadedPacks,
    pool: &str,
    target: &str,
    max_depth: i32,
    pos: BlockPos,
    level: LevelHeight,
) -> Result<Option<Vec<Box<dyn StructurePiece>>>, Error> {
    let definition = json!({
        "type": "minecraft:jigsaw",
        "biomes": [],
        "spawn_overrides": {},
        "step": "surface_structures",
        "start_pool": pool,
        "start_jigsaw_name": target,
        "size": max_depth,
        "start_height": {"absolute": pos.y},
        "max_distance_from_center": 128,
        "use_expansion_hack": false,
    });
    let mut parser =
        crate::structure::StructureParser::new(packs, generator.biomes(), generator.features());
    let structure = JigsawStructure::parse(&definition, &mut parser)?;
    let biomes = any_biome(generator);
    let mut ctx = GenerationContext::new(generator, pos.chunk(), level, &biomes);
    let Some(stub) = add_pieces(&structure, &mut ctx, pos, PoolAliasLookup::default()) else {
        return Ok(None);
    };
    Ok(Some(stub.into_pieces_builder(&mut ctx).build()))
}

/// `PoolElementStructurePiece.place(level, ..., BoundingBox.infinite(), position, false)` of
/// every piece.
pub fn place_jigsaw(
    generator: &TerrainGenerator,
    pieces: &mut [Box<dyn StructurePiece>],
    region: &mut WorldGenRegion<'_>,
    random: &mut DecorationRandom,
    pos: BlockPos,
) {
    let chunk = pos.chunk();
    for piece in pieces {
        let mut infinite = BoundingBox::infinite();
        let mut ctx = PieceContext {
            level: region,
            random,
            generator,
        };
        piece.post_process(&mut ctx, &mut infinite, chunk, pos);
    }
}
