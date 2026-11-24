//! Nether fossils (`NetherFossilStructure`, `NetherFossilPieces`).

use std::sync::Arc;

use serde_json::Value;
use vanillite_base::random::{LegacyRandom, PositionalRandomFactory, RandomSource};
use vanillite_base::{BlockPos, ChunkPos, Direction};
use vanillite_data::block::ids;

use vanillite_nbt::Compound;

use crate::Error;
use crate::behaviour::{self, SupportType};
use crate::carver::providers::HeightProvider;
use crate::feature::field;
use crate::structure::piece::PieceContext;
use crate::structure::{
    GenerationContext, GenerationStub, PieceBase, PieceSettings, StructureParser, StructurePiece,
    StructureType, TemplatePiece,
};
use crate::template::transform::rotate_state;
use crate::template::{BoundingBox, Rotation, StructureProcessor, StructureTemplate};

/// `NetherFossilPieces.FOSSILS`.
const FOSSILS: [&str; 14] = [
    "minecraft:nether_fossils/fossil_1",
    "minecraft:nether_fossils/fossil_2",
    "minecraft:nether_fossils/fossil_3",
    "minecraft:nether_fossils/fossil_4",
    "minecraft:nether_fossils/fossil_5",
    "minecraft:nether_fossils/fossil_6",
    "minecraft:nether_fossils/fossil_7",
    "minecraft:nether_fossils/fossil_8",
    "minecraft:nether_fossils/fossil_9",
    "minecraft:nether_fossils/fossil_10",
    "minecraft:nether_fossils/fossil_11",
    "minecraft:nether_fossils/fossil_12",
    "minecraft:nether_fossils/fossil_13",
    "minecraft:nether_fossils/fossil_14",
];

/// `BlockIgnoreProcessor.STRUCTURE_AND_AIR`.
pub(crate) fn ignore_structure_and_air() -> StructureProcessor {
    StructureProcessor::BlockIgnore(vec![ids::STRUCTURE_BLOCK, ids::AIR])
}

/// `NetherFossilStructure`.
#[derive(Debug)]
pub struct NetherFossilStructure {
    pub height: HeightProvider,
    templates: Vec<Arc<StructureTemplate>>,
}

impl StructureType for NetherFossilStructure {
    fn parse(json: &Value, parser: &mut StructureParser<'_>) -> Result<Self, Error> {
        Ok(Self {
            height: HeightProvider::from_json(field(json, "height")?)?,
            templates: FOSSILS
                .iter()
                .map(|id| parser.template(id))
                .collect::<Result<_, _>>()?,
        })
    }

    fn find_generation_point<'s>(
        &'s self,
        ctx: &mut GenerationContext<'_>,
    ) -> Option<GenerationStub<'s>> {
        let random = &mut ctx.random.random;
        let block_x = ctx.chunk_pos.min_block_x() + random.next_int_bounded(16);
        let block_z = ctx.chunk_pos.min_block_z() + random.next_int_bounded(16);
        let sea_level = ctx.generator.sea_level();
        let generation = ctx.generator.generation_context(ctx.height);
        let mut y = self.height.sample(random, &generation);
        let column = ctx.generator.base_column(block_x, block_z, ctx.height);
        while y > sea_level {
            let current = column.block(y);
            y -= 1;
            let below = column.block(y);
            if current.is_air()
                && (below.block() == ids::SOUL_SAND
                    || behaviour::is_face_sturdy(below, Direction::Up, SupportType::Full))
            {
                break;
            }
        }
        if y <= sea_level {
            return None;
        }
        let position = BlockPos::new(block_x, y, block_z);
        Some(GenerationStub::lazy(position, move |builder, ctx| {
            // `NetherFossilPieces.addPieces`.
            let random = &mut ctx.random.random;
            let rotation = Rotation::random(random);
            let index = random.next_int_bounded(FOSSILS.len() as i32) as usize;
            builder.add_piece(Box::new(NetherFossilPiece::new(
                FOSSILS[index],
                Arc::clone(&self.templates[index]),
                position,
                rotation,
            )));
        }))
    }
}

/// `NetherFossilPieces.NetherFossilPiece` (`minecraft:nefos`).
#[derive(Clone, Debug)]
pub struct NetherFossilPiece {
    pub template: TemplatePiece,
}

impl NetherFossilPiece {
    /// `new NetherFossilPiece(manager, location, position, rotation)`.
    pub fn new(
        name: &str,
        template: Arc<StructureTemplate>,
        position: BlockPos,
        rotation: Rotation,
    ) -> Self {
        let settings = PieceSettings {
            rotation,
            processors: vec![ignore_structure_and_air()],
            ..PieceSettings::new()
        };
        Self {
            template: TemplatePiece::new(
                "minecraft:nefos",
                0,
                name.to_owned(),
                template,
                settings,
                position,
            ),
        }
    }
}

impl StructurePiece for NetherFossilPiece {
    fn base(&self) -> &PieceBase {
        &self.template.base
    }

    fn base_mut(&mut self) -> &mut PieceBase {
        &mut self.template.base
    }

    /// `addAdditionalSaveData`.
    fn additional_save_data(&self) -> Option<Compound> {
        Some(
            self.template
                .save_data()
                .with("Rot", self.template.settings.rotation.legacy_name()),
        )
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
        let fossil_bb = t
            .template
            .bounding_box(&t.settings.place_settings(), t.template_position);
        // Vanilla grows the chunk box it was given (`chunkBB.encapsulate(fossilBB)`).
        chunk_bb.encapsulate(&fossil_bb);
        self.template
            .place(ctx, chunk_bb, reference, |_, _, _, _| {});
        // `placeDriedGhast`.
        let center = fossil_bb.center();
        let mut random = LegacyRandom::new(ctx.level.seed())
            .fork_positional()
            .at(center.x, center.y, center.z);
        if random.next_float() < 0.5 {
            let x = fossil_bb.min_x() + random.next_int_bounded(fossil_bb.x_span());
            let y = fossil_bb.min_y();
            let z = fossil_bb.min_z() + random.next_int_bounded(fossil_bb.z_span());
            let pos = BlockPos::new(x, y, z);
            if ctx.level.block(pos).is_air() && chunk_bb.is_inside(pos) {
                let state = rotate_state(
                    ids::DRIED_GHAST.default_state(),
                    Rotation::random(&mut random),
                );
                ctx.level.set_block(pos, state, 2);
            }
        }
    }
}
