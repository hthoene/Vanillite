//! `TemplateStructurePiece`: a piece that places a structure template.

use std::sync::Arc;

use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockState, ids};
use vanillite_nbt::{Compound, Tag};

use super::piece::{PieceBase, PieceContext};
use crate::template::{
    BoundingBox, LiquidSettings, Mirror, Rotation, StructurePlaceSettings, StructureProcessor,
    StructureTemplate, parse_state_string,
};

/// An owned `StructurePlaceSettings` as a piece keeps it (processors by value instead of
/// borrowed from their lists). The random is always the positional one (`setRandom` is never
/// called by pieces).
#[derive(Clone, Debug, Default)]
pub struct PieceSettings {
    pub mirror: Mirror,
    pub rotation: Rotation,
    pub rotation_pivot: BlockPos,
    pub ignore_entities: bool,
    pub bounding_box: Option<BoundingBox>,
    pub liquid_settings: LiquidSettings,
    pub processors: Vec<StructureProcessor>,
    pub known_shape: bool,
    pub finalize_entities: bool,
}

impl PieceSettings {
    /// `new StructurePlaceSettings()`.
    pub fn new() -> Self {
        Self::default()
    }

    /// The borrowed settings template placement takes.
    pub fn place_settings(&self) -> StructurePlaceSettings<'_> {
        let mut settings = StructurePlaceSettings::new()
            .with_rotation(self.rotation)
            .with_mirror(self.mirror);
        settings.rotation_pivot = self.rotation_pivot;
        settings.ignore_entities = self.ignore_entities;
        settings.bounding_box = self.bounding_box;
        settings.liquid_settings = self.liquid_settings;
        settings.known_shape = self.known_shape;
        settings.finalize_entities = self.finalize_entities;
        settings.add_processors(&self.processors);
        settings
    }
}

/// The state of Vanilla's `TemplateStructurePiece`: embed it in a piece and delegate
/// `base`/`base_mut`/`move_by`/`rotation` to it; `post_process` calls [`Self::place`] with the
/// piece's `handleDataMarker`.
#[derive(Clone, Debug)]
pub struct TemplatePiece {
    pub base: PieceBase,
    /// `templateName` (e.g. `minecraft:igloo/top`).
    pub template_name: String,
    pub template: Arc<StructureTemplate>,
    /// `placeSettings`.
    pub settings: PieceSettings,
    /// `templatePosition`.
    pub template_position: BlockPos,
}

impl TemplatePiece {
    /// `new TemplateStructurePiece(type, genDepth, manager, location, name, settings, position)`:
    /// the box of the template at `position`, orientation north.
    pub fn new(
        kind: &'static str,
        gen_depth: i32,
        template_name: String,
        template: Arc<StructureTemplate>,
        settings: PieceSettings,
        position: BlockPos,
    ) -> Self {
        let bounding_box = template.bounding_box(&settings.place_settings(), position);
        let mut base = PieceBase::new(kind, gen_depth, bounding_box);
        base.set_orientation(Some(Direction::North));
        Self {
            base,
            template_name,
            template,
            settings,
            template_position: position,
        }
    }

    /// `TemplateStructurePiece.addAdditionalSaveData`: `TPX`, `TPY`, `TPZ`, `Template`.
    pub fn save_data(&self) -> Compound {
        Compound::new()
            .with("TPX", self.template_position.x)
            .with("TPY", self.template_position.y)
            .with("TPZ", self.template_position.z)
            .with("Template", self.template_name.as_str())
    }

    /// `move(dx, dy, dz)`: the box and the template position.
    pub fn move_by(&mut self, dx: i32, dy: i32, dz: i32) {
        self.base.bounding_box.translate(dx, dy, dz);
        self.template_position = self.template_position.offset(dx, dy, dz);
    }

    /// `getRotation()`: the placement rotation.
    pub fn rotation(&self) -> Rotation {
        self.settings.rotation
    }

    /// `TemplateStructurePiece.postProcess`: places the template clipped to `chunk_bb`, then
    /// calls `handle_data_marker(metadata, pos, ctx, chunk_bb)` for every data structure block
    /// and turns jigsaw blocks into their final state.
    pub fn place(
        &mut self,
        ctx: &mut PieceContext<'_, '_>,
        chunk_bb: &BoundingBox,
        reference: BlockPos,
        mut handle_data_marker: impl FnMut(&str, BlockPos, &mut PieceContext<'_, '_>, &BoundingBox),
    ) {
        self.settings.bounding_box = Some(*chunk_bb);
        let settings = self.settings.place_settings();
        self.base.bounding_box = self
            .template
            .bounding_box(&settings, self.template_position);
        let placed = self.template.place_in_world(
            ctx.level,
            self.template_position,
            reference,
            &settings,
            &mut *ctx.random,
            2,
        );
        if !placed {
            return;
        }
        let markers = self.template.filter_blocks(
            self.template_position,
            &settings,
            ids::STRUCTURE_BLOCK,
            true,
            &mut *ctx.random,
        );
        for marker in markers {
            let Some(nbt) = &marker.nbt else {
                continue;
            };
            let is_data = matches!(nbt.get("mode"), Some(Tag::String(mode)) if mode.eq_ignore_ascii_case("data"));
            if is_data {
                let metadata = match nbt.get("metadata") {
                    Some(Tag::String(m)) => m.as_str(),
                    _ => "",
                };
                handle_data_marker(metadata, marker.pos, ctx, chunk_bb);
            }
        }
        let jigsaws = self.template.filter_blocks(
            self.template_position,
            &settings,
            ids::JIGSAW,
            true,
            &mut *ctx.random,
        );
        for jigsaw in jigsaws {
            let Some(nbt) = &jigsaw.nbt else {
                continue;
            };
            let final_state = match nbt.get("final_state") {
                Some(Tag::String(s)) => s.as_str(),
                _ => "minecraft:air",
            };
            let state = parse_state_string(final_state).unwrap_or(BlockState::AIR);
            ctx.level.set_block_and_update(jigsaw.pos, state);
        }
    }
}
