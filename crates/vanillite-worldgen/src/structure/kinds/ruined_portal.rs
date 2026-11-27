//! Ruined portals (`RuinedPortalStructure`, `RuinedPortalPiece`).

use std::sync::Arc;

use serde_json::Value;
use vanillite_base::random::{LegacyRandom, RandomSource};
use vanillite_base::{BlockPos, ChunkPos, Direction};
use vanillite_chunk::{HeightmapKind, facts};
use vanillite_data::block::{BlockId, BlockState, ids};

use super::nether_fossil::ignore_structure_and_air;
use vanillite_nbt::Compound;

use crate::Error;
use crate::behaviour;
use crate::blocks::BlockSet;
use crate::carver::providers::random_between_inclusive;
use crate::feature::{WorldGenRegion, field, float_field};
use crate::generator::{NoiseColumn, TerrainGenerator};
use crate::structure::piece::{PieceContext, random_horizontal_direction};
use crate::structure::{
    GenerationContext, GenerationStub, LevelHeight, PieceBase, PieceSettings, StructureParser,
    StructurePiece, StructureType, TemplatePiece,
};
use crate::template::TemplateRef;
use crate::template::rule::{BlockEntityModifier, PosRuleTest, ProcessorRule, RuleTest};
use crate::template::transform::direction_name;
use crate::template::{BoundingBox, Mirror, Rotation, StructureProcessor, StructureTemplate};

/// `STRUCTURE_LOCATION_PORTALS`.
const PORTALS: [&str; 10] = [
    "minecraft:ruined_portal/portal_1",
    "minecraft:ruined_portal/portal_2",
    "minecraft:ruined_portal/portal_3",
    "minecraft:ruined_portal/portal_4",
    "minecraft:ruined_portal/portal_5",
    "minecraft:ruined_portal/portal_6",
    "minecraft:ruined_portal/portal_7",
    "minecraft:ruined_portal/portal_8",
    "minecraft:ruined_portal/portal_9",
    "minecraft:ruined_portal/portal_10",
];

/// `STRUCTURE_LOCATION_GIANT_PORTALS`.
const GIANT_PORTALS: [&str; 3] = [
    "minecraft:ruined_portal/giant_portal_1",
    "minecraft:ruined_portal/giant_portal_2",
    "minecraft:ruined_portal/giant_portal_3",
];

/// `RuinedPortalPiece.VerticalPlacement`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerticalPlacement {
    OnLandSurface,
    PartlyBuried,
    OnOceanFloor,
    InMountain,
    Underground,
    InNether,
}

impl VerticalPlacement {
    fn from_name(name: &str) -> Result<Self, Error> {
        Ok(match name {
            "on_land_surface" => Self::OnLandSurface,
            "partly_buried" => Self::PartlyBuried,
            "on_ocean_floor" => Self::OnOceanFloor,
            "in_mountain" => Self::InMountain,
            "underground" => Self::Underground,
            "in_nether" => Self::InNether,
            other => return Err(Error::new(format!("unknown vertical placement {other}"))),
        })
    }

    /// The serialized name (`VerticalPlacement.CODEC`).
    pub fn name(self) -> &'static str {
        match self {
            Self::OnLandSurface => "on_land_surface",
            Self::PartlyBuried => "partly_buried",
            Self::OnOceanFloor => "on_ocean_floor",
            Self::InMountain => "in_mountain",
            Self::Underground => "underground",
            Self::InNether => "in_nether",
        }
    }

    /// `RuinedPortalPiece.getHeightMapType(placement)`.
    fn heightmap(self) -> HeightmapKind {
        if self == Self::OnOceanFloor {
            HeightmapKind::OceanFloorWg
        } else {
            HeightmapKind::WorldSurfaceWg
        }
    }
}

/// `RuinedPortalStructure.Setup`.
#[derive(Clone, Copy, Debug)]
pub struct Setup {
    pub placement: VerticalPlacement,
    pub air_pocket_probability: f32,
    pub mossiness: f32,
    pub overgrown: bool,
    pub vines: bool,
    pub can_be_cold: bool,
    pub replace_with_blackstone: bool,
    pub weight: f32,
}

impl Setup {
    fn parse(json: &Value) -> Result<Self, Error> {
        let unit = |name: &str| -> Result<f32, Error> {
            let value = float_field(json, name)?;
            if (0.0..=1.0).contains(&value) {
                Ok(value)
            } else {
                Err(Error::new(format!("{name} {value} outside [0, 1]")))
            }
        };
        let flag = |name: &str| -> Result<bool, Error> {
            field(json, name)?
                .as_bool()
                .ok_or_else(|| Error::new(format!("{name} is not a boolean")))
        };
        let weight = float_field(json, "weight")?;
        if weight <= 0.0 {
            return Err(Error::new("setup weight must be positive"));
        }
        Ok(Self {
            placement: VerticalPlacement::from_name(
                field(json, "placement")?
                    .as_str()
                    .ok_or_else(|| Error::new("placement is not a string"))?,
            )?,
            air_pocket_probability: unit("air_pocket_probability")?,
            mossiness: unit("mossiness")?,
            overgrown: flag("overgrown")?,
            vines: flag("vines")?,
            can_be_cold: flag("can_be_cold")?,
            replace_with_blackstone: flag("replace_with_blackstone")?,
            weight,
        })
    }
}

/// `RuinedPortalPiece.Properties`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Properties {
    pub cold: bool,
    pub mossiness: f32,
    pub air_pocket: bool,
    pub overgrown: bool,
    pub vines: bool,
    pub replace_with_blackstone: bool,
}

/// Block tags the portal processors and pieces test, resolved once.
#[derive(Debug)]
struct Tags {
    stairs: BlockSet,
    slabs: BlockSet,
    walls: BlockSet,
    /// `BlockTags.FEATURES_CANNOT_REPLACE`.
    features_cannot_replace: BlockSet,
}

/// `RuinedPortalStructure`.
#[derive(Debug)]
pub struct RuinedPortalStructure {
    pub setups: Vec<Setup>,
    portals: Vec<TemplateRef>,
    giant_portals: Vec<TemplateRef>,
    tags: Arc<Tags>,
}

impl StructureType for RuinedPortalStructure {
    fn parse(json: &Value, parser: &mut StructureParser<'_>) -> Result<Self, Error> {
        let setups = field(json, "setups")?
            .as_array()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::new("setups is not a non-empty list"))?
            .iter()
            .map(Setup::parse)
            .collect::<Result<_, _>>()?;
        let load =
            |names: &[&str]| -> Vec<_> { names.iter().map(|id| parser.template(id)).collect() };
        Ok(Self {
            setups,
            portals: load(&PORTALS),
            giant_portals: load(&GIANT_PORTALS),
            tags: Arc::new(Tags {
                stairs: parser.block_tag("minecraft:stairs"),
                slabs: parser.block_tag("minecraft:slabs"),
                walls: parser.block_tag("minecraft:walls"),
                features_cannot_replace: parser.block_tag("minecraft:features_cannot_replace"),
            }),
        })
    }

    fn find_generation_point<'s>(
        &'s self,
        ctx: &mut GenerationContext<'_>,
    ) -> Option<GenerationStub<'s>> {
        let random = &mut ctx.random.random;
        let setup = if self.setups.len() > 1 {
            let total: f32 = self.setups.iter().map(|s| s.weight).sum();
            let mut pick = random.next_float();
            let mut chosen = None;
            for s in &self.setups {
                pick -= s.weight / total;
                if pick < 0.0 {
                    chosen = Some(*s);
                    break;
                }
            }
            // Vanilla throws when rounding leaves no setup chosen.
            chosen?
        } else {
            self.setups[0]
        };
        let air_pocket = sample(random, setup.air_pocket_probability);
        let (name, template) = if random.next_float() < 0.05 {
            let index = random.next_int_bounded(GIANT_PORTALS.len() as i32) as usize;
            (GIANT_PORTALS[index], self.giant_portals[index].get())
        } else {
            let index = random.next_int_bounded(PORTALS.len() as i32) as usize;
            (PORTALS[index], self.portals[index].get())
        };
        let rotation = Rotation::random(random);
        let mirror = if random.next_float() < 0.5 {
            Mirror::None
        } else {
            Mirror::FrontBack
        };
        let size = template.size();
        let pivot = BlockPos::new(size.x / 2, 0, size.z / 2);
        let base = BlockPos::new(ctx.chunk_pos.min_block_x(), 0, ctx.chunk_pos.min_block_z());
        let bounding_box = template.bounding_box_with(base, rotation, pivot, mirror);
        let center = bounding_box.center();
        let generator = ctx.generator;
        let surface_y =
            generator.base_height(center.x, center.z, setup.placement.heightmap(), ctx.height) - 1;
        let projected_y = find_suitable_y(
            random,
            generator,
            ctx.height,
            setup.placement,
            air_pocket,
            surface_y,
            bounding_box.y_span(),
            &bounding_box,
        );
        let origin = BlockPos::new(base.x, projected_y, base.z);
        Some(GenerationStub::lazy(origin, move |builder, ctx| {
            let cold = setup.can_be_cold && {
                let biome = ctx.noise_biome(origin.x >> 2, origin.y >> 2, origin.z >> 2);
                ctx.generator.biomes().get(biome).cold_enough_to_snow(
                    origin.x,
                    origin.y,
                    origin.z,
                    ctx.generator.sea_level(),
                )
            };
            let properties = Properties {
                cold,
                mossiness: setup.mossiness,
                air_pocket,
                overgrown: setup.overgrown,
                vines: setup.vines,
                replace_with_blackstone: setup.replace_with_blackstone,
            };
            builder.add_piece(Box::new(RuinedPortalPiece::new(
                origin,
                setup.placement,
                properties,
                name,
                template,
                rotation,
                mirror,
                pivot,
                Arc::clone(&self.tags),
            )));
        }))
    }
}

/// `RuinedPortalStructure.sample(random, limit)`.
fn sample(random: &mut impl RandomSource, limit: f32) -> bool {
    if limit == 0.0 {
        false
    } else if limit == 1.0 {
        true
    } else {
        random.next_float() < limit
    }
}

/// `getRandomWithinInterval(random, minPreferred, max)`.
fn random_within_interval(random: &mut impl RandomSource, min_preferred: i32, max: i32) -> i32 {
    if min_preferred < max {
        random_between_inclusive(random, min_preferred, max)
    } else {
        max
    }
}

/// `findSuitableY(random, generator, placement, airPocket, surfaceYAtCenter, ySpan,
/// boundingBox, heightAccessor, randomState)`.
#[allow(clippy::too_many_arguments)]
fn find_suitable_y(
    random: &mut LegacyRandom,
    generator: &TerrainGenerator,
    level: LevelHeight,
    placement: VerticalPlacement,
    air_pocket: bool,
    surface_y: i32,
    y_span: i32,
    bounding_box: &BoundingBox,
) -> i32 {
    let min_y = level.min_y + 15;
    let new_y = match placement {
        VerticalPlacement::InNether => {
            if air_pocket {
                random_between_inclusive(random, 32, 100)
            } else if random.next_float() < 0.5 {
                random_between_inclusive(random, 27, 29)
            } else {
                random_between_inclusive(random, 29, 100)
            }
        }
        VerticalPlacement::InMountain => random_within_interval(random, 70, surface_y - y_span),
        VerticalPlacement::Underground => random_within_interval(random, min_y, surface_y - y_span),
        VerticalPlacement::PartlyBuried => {
            surface_y - y_span + random_between_inclusive(random, 2, 8)
        }
        _ => surface_y,
    };
    let corners = [
        (bounding_box.min_x(), bounding_box.min_z()),
        (bounding_box.max_x(), bounding_box.min_z()),
        (bounding_box.min_x(), bounding_box.max_z()),
        (bounding_box.max_x(), bounding_box.max_z()),
    ];
    let columns: Vec<NoiseColumn> = corners
        .iter()
        .map(|&(x, z)| generator.base_column(x, z, level))
        .collect();
    let heightmap = placement.heightmap();
    let mut projected_y = new_y;
    while projected_y > min_y {
        let mut on_solid_ground = 0;
        for column in &columns {
            if heightmap.matches(facts::block_flags(column.block(projected_y).0)) {
                on_solid_ground += 1;
                if on_solid_ground == 3 {
                    return projected_y;
                }
            }
        }
        projected_y -= 1;
    }
    projected_y
}

/// `RuinedPortalPiece` (`minecraft:rupo`).
#[derive(Clone, Debug)]
pub struct RuinedPortalPiece {
    pub template: TemplatePiece,
    pub vertical_placement: VerticalPlacement,
    pub properties: Properties,
    tags: Arc<Tags>,
}

/// `getBlockReplaceRule(source, probability, target)`.
fn random_replace_rule(source: BlockId, probability: f32, target: BlockId) -> ProcessorRule {
    replace_rule(
        RuleTest::RandomBlockMatch {
            block: source,
            probability,
        },
        target,
    )
}

/// `getBlockReplaceRule(source, target)` (and the shared rule shape).
fn replace_rule(input: RuleTest, target: BlockId) -> ProcessorRule {
    ProcessorRule {
        input_predicate: input,
        location_predicate: RuleTest::AlwaysTrue,
        position_predicate: PosRuleTest::AlwaysTrue,
        output_state: target.default_state(),
        block_entity_modifier: BlockEntityModifier::Passthrough,
    }
}

impl RuinedPortalPiece {
    /// `new RuinedPortalPiece(registries, manager, position, placement, properties, location,
    /// template, rotation, mirror, pivot)`.
    #[allow(clippy::too_many_arguments)]
    fn new(
        position: BlockPos,
        vertical_placement: VerticalPlacement,
        properties: Properties,
        name: &str,
        template: Arc<StructureTemplate>,
        rotation: Rotation,
        mirror: Mirror,
        pivot: BlockPos,
        tags: Arc<Tags>,
    ) -> Self {
        let settings = Self::settings(
            mirror,
            rotation,
            vertical_placement,
            pivot,
            properties,
            &tags,
        );
        Self {
            template: TemplatePiece::new(
                "minecraft:rupo",
                0,
                name.to_owned(),
                template,
                settings,
                position,
            ),
            vertical_placement,
            properties,
            tags,
        }
    }

    /// `makeSettings(registries, mirror, rotation, placement, pivot, properties)`.
    fn settings(
        mirror: Mirror,
        rotation: Rotation,
        placement: VerticalPlacement,
        pivot: BlockPos,
        properties: Properties,
        tags: &Tags,
    ) -> PieceSettings {
        let ignore = if properties.air_pocket {
            StructureProcessor::BlockIgnore(vec![ids::STRUCTURE_BLOCK])
        } else {
            ignore_structure_and_air()
        };
        let mut rules = vec![random_replace_rule(ids::GOLD_BLOCK, 0.3, ids::AIR)];
        // `getLavaProcessorRule(placement, properties)`.
        rules.push(if placement == VerticalPlacement::OnOceanFloor {
            replace_rule(RuleTest::BlockMatch(ids::LAVA), ids::MAGMA_BLOCK)
        } else if properties.cold {
            replace_rule(RuleTest::BlockMatch(ids::LAVA), ids::NETHERRACK)
        } else {
            random_replace_rule(ids::LAVA, 0.2, ids::MAGMA_BLOCK)
        });
        if !properties.cold {
            rules.push(random_replace_rule(ids::NETHERRACK, 0.07, ids::MAGMA_BLOCK));
        }
        let mut processors = vec![
            ignore,
            StructureProcessor::Rule(rules),
            StructureProcessor::BlockAge {
                mossiness: properties.mossiness,
                stairs: tags.stairs.clone(),
                slabs: tags.slabs.clone(),
                walls: tags.walls.clone(),
            },
            StructureProcessor::ProtectedBlocks(tags.features_cannot_replace.clone()),
            StructureProcessor::LavaSubmergedBlock,
        ];
        if properties.replace_with_blackstone {
            processors.push(StructureProcessor::BlackstoneReplace);
        }
        PieceSettings {
            rotation,
            mirror,
            rotation_pivot: pivot,
            processors,
            ..PieceSettings::new()
        }
    }

    /// `canBlockBeReplacedByNetherrackOrMagma(state)`.
    fn can_be_replaced_by_netherrack(&self, state: BlockState) -> bool {
        let block = state.block();
        block != ids::AIR
            && block != ids::OBSIDIAN
            && !self.tags.features_cannot_replace.contains(state)
            && (self.vertical_placement == VerticalPlacement::InNether || block != ids::LAVA)
    }

    /// `placeNetherrackOrMagma(random, level, pos)`.
    fn place_netherrack_or_magma(
        &self,
        random: &mut impl RandomSource,
        level: &mut WorldGenRegion<'_>,
        pos: BlockPos,
    ) {
        if !self.can_be_replaced_by_netherrack(level.block(pos)) {
            return;
        }
        let state = if !self.properties.cold && random.next_float() < 0.07 {
            ids::MAGMA_BLOCK.default_state()
        } else {
            ids::NETHERRACK.default_state()
        };
        level.set_block_and_update(pos, state);
    }

    /// `addNetherrackDripColumn(random, level, pos)`.
    fn add_netherrack_drip_column(
        &self,
        random: &mut impl RandomSource,
        level: &mut WorldGenRegion<'_>,
        pos: BlockPos,
    ) {
        let mut current = pos;
        self.place_netherrack_or_magma(random, level, current);
        let mut remaining = 8;
        while remaining > 0 && random.next_float() < 0.5 {
            current = current.relative(Direction::Down);
            remaining -= 1;
            self.place_netherrack_or_magma(random, level, current);
        }
    }

    /// `spreadNetherrack(random, level)`.
    fn spread_netherrack(&self, random: &mut impl RandomSource, level: &mut WorldGenRegion<'_>) {
        const PROBABILITY_BY_DISTANCE: [f32; 14] = [
            1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 0.9, 0.9, 0.8, 0.7, 0.6, 0.4, 0.2,
        ];
        let follow_ground = matches!(
            self.vertical_placement,
            VerticalPlacement::OnLandSurface | VerticalPlacement::OnOceanFloor
        );
        let bb = self.template.base.bounding_box;
        let center = bb.center();
        let max_distance = PROBABILITY_BY_DISTANCE.len() as i32;
        let average_width = (bb.x_span() + bb.z_span()) / 2;
        let adjustment = random.next_int_bounded(1.max(8 - average_width / 2));
        let heightmap = self.vertical_placement.heightmap();
        for x in center.x - max_distance..=center.x + max_distance {
            for z in center.z - max_distance..=center.z + max_distance {
                let distance = (x - center.x).abs() + (z - center.z).abs();
                let adjusted = 0.max(distance + adjustment);
                if adjusted >= max_distance {
                    continue;
                }
                let probability = PROBABILITY_BY_DISTANCE[adjusted as usize];
                if random.next_double() >= f64::from(probability) {
                    continue;
                }
                let surface_y = level.height_at(heightmap, x, z) - 1;
                let y = if follow_ground {
                    surface_y
                } else {
                    bb.min_y().min(surface_y)
                };
                let pos = BlockPos::new(x, y, z);
                if (y - bb.min_y()).abs() <= 3
                    && self.can_be_replaced_by_netherrack(level.block(pos))
                {
                    self.place_netherrack_or_magma(random, level, pos);
                    if self.properties.overgrown {
                        self.maybe_add_leaves_above(random, level, pos);
                    }
                    self.add_netherrack_drip_column(random, level, pos.relative(Direction::Down));
                }
            }
        }
    }

    /// `addNetherrackDripColumnsBelowPortal(random, level)`.
    fn add_netherrack_drip_columns_below_portal(
        &self,
        random: &mut impl RandomSource,
        level: &mut WorldGenRegion<'_>,
    ) {
        let bb = self.template.base.bounding_box;
        for x in bb.min_x() + 1..bb.max_x() {
            for z in bb.min_z() + 1..bb.max_z() {
                let pos = BlockPos::new(x, bb.min_y(), z);
                if level.block(pos).block() == ids::NETHERRACK {
                    self.add_netherrack_drip_column(random, level, pos.relative(Direction::Down));
                }
            }
        }
    }

    /// `maybeAddVines(random, level, pos)`.
    fn maybe_add_vines(
        &self,
        random: &mut impl RandomSource,
        level: &mut WorldGenRegion<'_>,
        pos: BlockPos,
    ) {
        let state = level.block(pos);
        if state.is_air() || state.block() == ids::VINE {
            return;
        }
        let direction = random_horizontal_direction(random);
        let neighbour = pos.relative(direction);
        if !level.block(neighbour).is_air() {
            return;
        }
        if behaviour::is_collision_face_full(state, direction) {
            let vine = ids::VINE.default_state();
            let vine = vine
                .with(direction_name(direction.opposite()), "true")
                .unwrap_or(vine);
            level.set_block_and_update(neighbour, vine);
        }
    }

    /// `maybeAddLeavesAbove(random, level, pos)`.
    fn maybe_add_leaves_above(
        &self,
        random: &mut impl RandomSource,
        level: &mut WorldGenRegion<'_>,
        pos: BlockPos,
    ) {
        let above = pos.relative(Direction::Up);
        if random.next_float() < 0.5
            && level.block(pos).block() == ids::NETHERRACK
            && level.block(above).is_air()
        {
            let leaves = ids::JUNGLE_LEAVES.default_state();
            let leaves = leaves.with("persistent", "true").unwrap_or(leaves);
            level.set_block_and_update(above, leaves);
        }
    }
}

impl StructurePiece for RuinedPortalPiece {
    fn base(&self) -> &PieceBase {
        &self.template.base
    }

    fn base_mut(&mut self) -> &mut PieceBase {
        &mut self.template.base
    }

    /// `addAdditionalSaveData`.
    fn additional_save_data(&self) -> Option<Compound> {
        let p = &self.properties;
        let properties = Compound::new()
            .with("cold", p.cold)
            .with("mossiness", p.mossiness)
            .with("air_pocket", p.air_pocket)
            .with("overgrown", p.overgrown)
            .with("vines", p.vines)
            .with("replace_with_blackstone", p.replace_with_blackstone);
        let tag = self
            .template
            .save_data()
            .with("Rotation", self.template.settings.rotation.legacy_name())
            .with("Mirror", self.template.settings.mirror.legacy_name())
            .with("VerticalPlacement", self.vertical_placement.name())
            .with("Properties", properties);
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
        let t = &self.template;
        let bounding_box = t
            .template
            .bounding_box(&t.settings.place_settings(), t.template_position);
        if !chunk_bb.is_inside(bounding_box.center()) {
            return;
        }
        chunk_bb.encapsulate(&bounding_box);
        self.template
            .place(ctx, chunk_bb, reference, |_, _, _, _| {});
        let (level, random) = (&mut *ctx.level, &mut *ctx.random);
        self.spread_netherrack(random, level);
        self.add_netherrack_drip_columns_below_portal(random, level);
        if self.properties.vines || self.properties.overgrown {
            // `BlockPos.betweenClosedStream(boundingBox)`: x fastest, then y, then z.
            let bb = self.template.base.bounding_box;
            for z in bb.min_z()..=bb.max_z() {
                for y in bb.min_y()..=bb.max_y() {
                    for x in bb.min_x()..=bb.max_x() {
                        let pos = BlockPos::new(x, y, z);
                        if self.properties.vines {
                            self.maybe_add_vines(random, level, pos);
                        }
                        if self.properties.overgrown {
                            self.maybe_add_leaves_above(random, level, pos);
                        }
                    }
                }
            }
        }
    }
}
