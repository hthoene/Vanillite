//! Ocean ruins (`OceanRuinStructure`, `OceanRuinPieces`).

use std::sync::Arc;

use serde_json::Value;
use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, ChunkPos};
use vanillite_chunk::HeightmapKind;
use vanillite_data::block::{BlockId, ids};
use vanillite_data::fluid::FluidState;

use super::nether_fossil::ignore_structure_and_air;
use vanillite_nbt::Compound;

use crate::Error;
use crate::blocks::BlockSet;
use crate::carver::providers::{IntProvider, next_int};
use crate::feature::kinds::terrain::{FLUID_WATER, fluid_tick_kind};
use crate::feature::region::{block_entity_type, entity_tag};
use crate::feature::{WorldGenRegion, field, float_field};
use crate::structure::piece::{PieceContext, set_loot_table};
use crate::structure::structure::on_top_of_chunk_center;
use crate::structure::{
    GenerationContext, GenerationStub, PieceBase, PieceSettings, StructureParser, StructurePiece,
    StructurePiecesBuilder, StructureType, TemplatePiece,
};
use crate::template::TemplateRef;
use crate::template::rule::{BlockEntityModifier, PosRuleTest, ProcessorRule, RuleTest};
use crate::template::transform::transform;
use crate::template::{BoundingBox, Mirror, Rotation, StructureProcessor, StructureTemplate};

/// `OceanRuinStructure.Type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BiomeTemp {
    Warm,
    Cold,
}

const WARM_RUINS: [&str; 8] = [
    "minecraft:underwater_ruin/warm_1",
    "minecraft:underwater_ruin/warm_2",
    "minecraft:underwater_ruin/warm_3",
    "minecraft:underwater_ruin/warm_4",
    "minecraft:underwater_ruin/warm_5",
    "minecraft:underwater_ruin/warm_6",
    "minecraft:underwater_ruin/warm_7",
    "minecraft:underwater_ruin/warm_8",
];
const RUINS_BRICK: [&str; 8] = [
    "minecraft:underwater_ruin/brick_1",
    "minecraft:underwater_ruin/brick_2",
    "minecraft:underwater_ruin/brick_3",
    "minecraft:underwater_ruin/brick_4",
    "minecraft:underwater_ruin/brick_5",
    "minecraft:underwater_ruin/brick_6",
    "minecraft:underwater_ruin/brick_7",
    "minecraft:underwater_ruin/brick_8",
];
const RUINS_CRACKED: [&str; 8] = [
    "minecraft:underwater_ruin/cracked_1",
    "minecraft:underwater_ruin/cracked_2",
    "minecraft:underwater_ruin/cracked_3",
    "minecraft:underwater_ruin/cracked_4",
    "minecraft:underwater_ruin/cracked_5",
    "minecraft:underwater_ruin/cracked_6",
    "minecraft:underwater_ruin/cracked_7",
    "minecraft:underwater_ruin/cracked_8",
];
const RUINS_MOSSY: [&str; 8] = [
    "minecraft:underwater_ruin/mossy_1",
    "minecraft:underwater_ruin/mossy_2",
    "minecraft:underwater_ruin/mossy_3",
    "minecraft:underwater_ruin/mossy_4",
    "minecraft:underwater_ruin/mossy_5",
    "minecraft:underwater_ruin/mossy_6",
    "minecraft:underwater_ruin/mossy_7",
    "minecraft:underwater_ruin/mossy_8",
];
const BIG_RUINS_BRICK: [&str; 4] = [
    "minecraft:underwater_ruin/big_brick_1",
    "minecraft:underwater_ruin/big_brick_2",
    "minecraft:underwater_ruin/big_brick_3",
    "minecraft:underwater_ruin/big_brick_8",
];
const BIG_RUINS_MOSSY: [&str; 4] = [
    "minecraft:underwater_ruin/big_mossy_1",
    "minecraft:underwater_ruin/big_mossy_2",
    "minecraft:underwater_ruin/big_mossy_3",
    "minecraft:underwater_ruin/big_mossy_8",
];
const BIG_RUINS_CRACKED: [&str; 4] = [
    "minecraft:underwater_ruin/big_cracked_1",
    "minecraft:underwater_ruin/big_cracked_2",
    "minecraft:underwater_ruin/big_cracked_3",
    "minecraft:underwater_ruin/big_cracked_8",
];
const BIG_WARM_RUINS: [&str; 4] = [
    "minecraft:underwater_ruin/big_warm_4",
    "minecraft:underwater_ruin/big_warm_5",
    "minecraft:underwater_ruin/big_warm_6",
    "minecraft:underwater_ruin/big_warm_7",
];

/// A list of ruin templates.
type Templates = Vec<(&'static str, TemplateRef)>;

/// What every ruin piece of a structure shares.
#[derive(Debug)]
struct Shared {
    temp: BiomeTemp,
    /// `BlockTags.ICE`.
    ice: BlockSet,
    /// `WARM_SUSPICIOUS_BLOCK_PROCESSOR` / `COLD_SUSPICIOUS_BLOCK_PROCESSOR`.
    suspicious: StructureProcessor,
}

/// `OceanRuinStructure`.
#[derive(Debug)]
pub struct OceanRuinStructure {
    pub biome_temp: BiomeTemp,
    pub large_probability: f32,
    pub cluster_probability: f32,
    shared: Arc<Shared>,
    /// Warm: small and big warm ruins. Cold: bricks, cracked, mossy (small), then the same big.
    small: [Templates; 3],
    big: [Templates; 3],
}

/// `OceanRuinPieces.archyRuleProcessor(candidate, replacement, lootTable)`.
fn archy_rule_processor(
    candidate: BlockId,
    replacement: BlockId,
    loot: &str,
) -> StructureProcessor {
    StructureProcessor::Capped {
        delegate: Box::new(StructureProcessor::Rule(vec![ProcessorRule {
            input_predicate: RuleTest::BlockMatch(candidate),
            location_predicate: RuleTest::AlwaysTrue,
            position_predicate: PosRuleTest::AlwaysTrue,
            output_state: replacement.default_state(),
            block_entity_modifier: BlockEntityModifier::AppendLoot(loot.to_owned()),
        }])),
        limit: IntProvider::Constant(5),
    }
}

impl StructureType for OceanRuinStructure {
    fn parse(json: &Value, parser: &mut StructureParser<'_>) -> Result<Self, Error> {
        let biome_temp = match field(json, "biome_temp")?.as_str() {
            Some("warm") => BiomeTemp::Warm,
            Some("cold") => BiomeTemp::Cold,
            _ => return Err(Error::new("invalid ocean ruin biome_temp")),
        };
        let probability = |name: &str| -> Result<f32, Error> {
            let value = float_field(json, name)?;
            if (0.0..=1.0).contains(&value) {
                Ok(value)
            } else {
                Err(Error::new(format!("{name} {value} outside [0, 1]")))
            }
        };
        let large_probability = probability("large_probability")?;
        let cluster_probability = probability("cluster_probability")?;
        let load = |names: &[&'static str]| -> Templates {
            names.iter().map(|&id| (id, parser.template(id))).collect()
        };
        let (small, big) = match biome_temp {
            BiomeTemp::Warm => (
                [load(&WARM_RUINS), Vec::new(), Vec::new()],
                [load(&BIG_WARM_RUINS), Vec::new(), Vec::new()],
            ),
            BiomeTemp::Cold => (
                [load(&RUINS_BRICK), load(&RUINS_CRACKED), load(&RUINS_MOSSY)],
                [
                    load(&BIG_RUINS_BRICK),
                    load(&BIG_RUINS_CRACKED),
                    load(&BIG_RUINS_MOSSY),
                ],
            ),
        };
        let suspicious = match biome_temp {
            BiomeTemp::Cold => archy_rule_processor(
                ids::GRAVEL,
                ids::SUSPICIOUS_GRAVEL,
                "minecraft:archaeology/ocean_ruin_cold",
            ),
            BiomeTemp::Warm => archy_rule_processor(
                ids::SAND,
                ids::SUSPICIOUS_SAND,
                "minecraft:archaeology/ocean_ruin_warm",
            ),
        };
        Ok(Self {
            biome_temp,
            large_probability,
            cluster_probability,
            shared: Arc::new(Shared {
                temp: biome_temp,
                ice: parser.block_tag("minecraft:ice"),
                suspicious,
            }),
            small,
            big,
        })
    }

    fn find_generation_point<'s>(
        &'s self,
        ctx: &mut GenerationContext<'_>,
    ) -> Option<GenerationStub<'s>> {
        on_top_of_chunk_center(ctx, HeightmapKind::OceanFloorWg, move |builder, ctx| {
            let offset =
                BlockPos::new(ctx.chunk_pos.min_block_x(), 90, ctx.chunk_pos.min_block_z());
            let random = &mut ctx.random.random;
            let rotation = Rotation::random(random);
            // `OceanRuinPieces.addPieces`.
            let is_large = random.next_float() <= self.large_probability;
            let base_integrity = if is_large { 0.9 } else { 0.8 };
            self.add_piece(offset, rotation, builder, random, is_large, base_integrity);
            if is_large && random.next_float() <= self.cluster_probability {
                self.add_cluster_ruins(random, rotation, offset, builder);
            }
        })
    }
}

impl OceanRuinStructure {
    /// `addClusterRuins(manager, random, rotation, p, structure, builder)`.
    fn add_cluster_ruins<R: RandomSource>(
        &self,
        random: &mut R,
        rotation: Rotation,
        p: BlockPos,
        builder: &mut StructurePiecesBuilder,
    ) {
        let parent = BlockPos::new(p.x, 90, p.z);
        let corner = transform(
            BlockPos::new(15, 0, 15),
            Mirror::None,
            rotation,
            BlockPos::ZERO,
        );
        let parent_corner = corner.offset(parent.x, parent.y, parent.z);
        let parent_bb = BoundingBox::from_corners(parent, parent_corner);
        let origin = BlockPos::new(
            parent.x.min(parent_corner.x),
            parent.y,
            parent.z.min(parent_corner.z),
        );
        // `allPositions(random, origin)`.
        let mut positions = Vec::with_capacity(8);
        let mut add = |dx: i32, dz: i32| positions.push(origin.offset(dx, 0, dz));
        let (x, z) = (-16 + next_int(random, 1, 8), 16 + next_int(random, 1, 7));
        add(x, z);
        let (x, z) = (-16 + next_int(random, 1, 8), next_int(random, 1, 7));
        add(x, z);
        let (x, z) = (-16 + next_int(random, 1, 8), -16 + next_int(random, 4, 8));
        add(x, z);
        let (x, z) = (next_int(random, 1, 7), 16 + next_int(random, 1, 7));
        add(x, z);
        let (x, z) = (next_int(random, 1, 7), -16 + next_int(random, 4, 6));
        add(x, z);
        let (x, z) = (16 + next_int(random, 1, 7), 16 + next_int(random, 3, 8));
        add(x, z);
        let (x, z) = (16 + next_int(random, 1, 7), next_int(random, 1, 7));
        add(x, z);
        let (x, z) = (16 + next_int(random, 1, 7), -16 + next_int(random, 4, 8));
        add(x, z);
        let ruins = next_int(random, 4, 8);
        for _ in 0..ruins {
            if positions.is_empty() {
                continue;
            }
            let index = random.next_int_bounded(positions.len() as i32) as usize;
            let pos = positions.remove(index);
            let next_rotation = Rotation::random(random);
            let next_corner = transform(
                BlockPos::new(5, 0, 6),
                Mirror::None,
                next_rotation,
                BlockPos::ZERO,
            )
            .offset(pos.x, pos.y, pos.z);
            let next_bb = BoundingBox::from_corners(pos, next_corner);
            if !next_bb.intersects(&parent_bb) {
                self.add_piece(pos, next_rotation, builder, random, false, 0.8);
            }
        }
    }

    /// `addPiece(manager, position, rotation, builder, random, structure, isLarge,
    /// baseIntegrity)`.
    fn add_piece<R: RandomSource>(
        &self,
        position: BlockPos,
        rotation: Rotation,
        builder: &mut StructurePiecesBuilder,
        random: &mut R,
        is_large: bool,
        base_integrity: f32,
    ) {
        let lists = if is_large { &self.big } else { &self.small };
        let piece = |(name, template): &(&'static str, TemplateRef), integrity| {
            Box::new(OceanRuinPiece::new(
                name,
                template.get(),
                position,
                rotation,
                integrity,
                Arc::clone(&self.shared),
                is_large,
            ))
        };
        match self.biome_temp {
            BiomeTemp::Warm => {
                let list = &lists[0];
                let index = random.next_int_bounded(list.len() as i32) as usize;
                builder.add_piece(piece(&list[index], base_integrity));
            }
            BiomeTemp::Cold => {
                let index = random.next_int_bounded(lists[0].len() as i32) as usize;
                builder.add_piece(piece(&lists[0][index], base_integrity));
                builder.add_piece(piece(&lists[1][index], 0.7));
                builder.add_piece(piece(&lists[2][index], 0.5));
            }
        }
    }
}

/// `OceanRuinPieces.OceanRuinPiece` (`minecraft:orp`).
#[derive(Clone, Debug)]
pub struct OceanRuinPiece {
    pub template: TemplatePiece,
    pub integrity: f32,
    pub is_large: bool,
    shared: Arc<Shared>,
}

impl OceanRuinPiece {
    /// `new OceanRuinPiece(manager, location, position, rotation, integrity, biomeType,
    /// isLarge)`.
    fn new(
        name: &str,
        template: Arc<StructureTemplate>,
        position: BlockPos,
        rotation: Rotation,
        integrity: f32,
        shared: Arc<Shared>,
        is_large: bool,
    ) -> Self {
        let settings = PieceSettings {
            rotation,
            processors: vec![
                StructureProcessor::BlockRot {
                    rottable_blocks: None,
                    integrity,
                },
                ignore_structure_and_air(),
                shared.suspicious.clone(),
            ],
            ..PieceSettings::new()
        };
        Self {
            template: TemplatePiece::new(
                "minecraft:orp",
                0,
                name.to_owned(),
                template,
                settings,
                position,
            ),
            integrity,
            is_large,
            shared,
        }
    }

    /// The biome type of the ruin.
    pub fn biome_temp(&self) -> BiomeTemp {
        self.shared.temp
    }

    /// `getHeight(pos, level, corner)`: lowers the ruin onto the floor below water and ice when
    /// most of it would float.
    fn height(&self, pos: BlockPos, level: &WorldGenRegion<'_>, corner: BlockPos) -> i32 {
        let mut new_y = pos.y;
        let mut min_y = 512;
        let top_y = new_y - 1;
        let mut area = 0;
        let (min_x, max_x) = (pos.x.min(corner.x), pos.x.max(corner.x));
        let (min_z, max_z) = (pos.z.min(corner.z), pos.z.max(corner.z));
        for z in min_z..=max_z {
            for x in min_x..=max_x {
                let mut floor_y = pos.y - 1;
                let mut state = level.block(BlockPos::new(x, floor_y, z));
                let mut fluid = state.fluid_state();
                while (state.is_air() || is_water(fluid) || self.shared.ice.contains(state))
                    && floor_y > level.min_y() + 1
                {
                    floor_y -= 1;
                    state = level.block(BlockPos::new(x, floor_y, z));
                    fluid = state.fluid_state();
                }
                min_y = min_y.min(floor_y);
                if floor_y < top_y - 2 {
                    area += 1;
                }
            }
        }
        let width = (pos.x - corner.x).abs();
        if top_y - min_y > 2 && area > width - 2 {
            new_y = min_y + 1;
        }
        new_y
    }
}

/// `fluid.is(FluidTags.WATER)`.
fn is_water(fluid: FluidState) -> bool {
    fluid_tick_kind(fluid) == Some(FLUID_WATER)
}

impl StructurePiece for OceanRuinPiece {
    fn base(&self) -> &PieceBase {
        &self.template.base
    }

    fn base_mut(&mut self) -> &mut PieceBase {
        &mut self.template.base
    }

    /// `addAdditionalSaveData`.
    fn additional_save_data(&self) -> Option<Compound> {
        let biome_type = match self.shared.temp {
            BiomeTemp::Warm => "WARM",
            BiomeTemp::Cold => "COLD",
        };
        let tag = self
            .template
            .save_data()
            .with("Rot", self.template.settings.rotation.legacy_name())
            .with("Integrity", self.integrity)
            .with("BiomeType", biome_type)
            .with("IsLarge", self.is_large);
        Some(tag)
    }

    fn move_by(&mut self, dx: i32, dy: i32, dz: i32) {
        self.template.move_by(dx, dy, dz);
    }

    fn rotation(&self) -> Rotation {
        self.template.rotation()
    }

    fn post_process(
        &mut self,
        ctx: &mut PieceContext<'_, '_>,
        chunk_bb: &mut BoundingBox,
        _chunk_pos: ChunkPos,
        reference: BlockPos,
    ) {
        let p = self.template.template_position;
        let height = ctx.level.height_at(HeightmapKind::OceanFloorWg, p.x, p.z);
        let p = BlockPos::new(p.x, height, p.z);
        let size = self.template.template.size();
        let corner = transform(
            BlockPos::new(size.x - 1, 0, size.z - 1),
            Mirror::None,
            self.template.settings.rotation,
            BlockPos::ZERO,
        )
        .offset(p.x, p.y, p.z);
        let y = self.height(p, ctx.level, corner);
        self.template.template_position = BlockPos::new(p.x, y, p.z);
        let is_large = self.is_large;
        self.template
            .place(ctx, chunk_bb, reference, |marker, pos, ctx, _| {
                // `handleDataMarker`.
                match marker {
                    "chest" => {
                        let waterlogged = is_water(ctx.level.fluid(pos));
                        let chest = ids::CHEST.default_state();
                        let state = chest
                            .with("waterlogged", if waterlogged { "true" } else { "false" })
                            .unwrap_or(chest);
                        ctx.level.set_block(pos, state, 2);
                        if matches!(
                            block_entity_type(ctx.level.block(pos)),
                            Some("minecraft:chest" | "minecraft:trapped_chest")
                        ) {
                            let loot = if is_large {
                                "minecraft:chests/underwater_ruin_big"
                            } else {
                                "minecraft:chests/underwater_ruin_small"
                            };
                            let seed = ctx.random.next_long();
                            set_loot_table(ctx.level, pos, loot, seed);
                        }
                    }
                    "drowned" => {
                        // `finalizeSpawn` (which draws from the level's random) is not run, the
                        // drowned is recorded with its type and position.
                        ctx.level.add_fresh_entity(entity_tag(
                            "minecraft:drowned",
                            f64::from(pos.x) + 0.5,
                            f64::from(pos.y),
                            f64::from(pos.z) + 0.5,
                        ));
                        let state = if pos.y > ctx.level.sea_level() {
                            vanillite_data::block::BlockState::AIR
                        } else {
                            ids::WATER.default_state()
                        };
                        ctx.level.set_block(pos, state, 2);
                    }
                    _ => {}
                }
            });
    }
}
