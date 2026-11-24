//! End cities (`EndCityStructure`, `EndCityPieces`): towers, bridges, houses and the ship,
//! grown recursively from the base house by the section generators.

use std::sync::Arc;

use rustc_hash::FxHashMap;
use serde_json::Value;
use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, ChunkPos, Direction};
use vanillite_data::block::ids;

use crate::Error;
use vanillite_nbt::{Compound, List, Tag};

use crate::feature::region::{block_entity_type, entity_tag};
use crate::structure::piece::{PieceContext, data_2d, set_loot_table};
use crate::structure::structure::lowest_y_in_5by5_box;
use crate::structure::{
    GenerationContext, GenerationStub, PieceBase, PieceSettings, StructureParser, StructurePiece,
    StructureType, TemplatePiece,
};
use crate::template::structure::RANDOMIZABLE_CONTAINERS;
use crate::template::{BoundingBox, Rotation, StructureProcessor, StructureTemplate};

/// `BuiltInLootTables.END_CITY_TREASURE`.
const LOOT_TABLE: &str = "minecraft:chests/end_city_treasure";

/// `EndCityPieces.MAX_GEN_DEPTH`.
const MAX_GEN_DEPTH: i32 = 8;

/// Every template the section generators use (`end_city/<name>`).
const TEMPLATES: [&str; 19] = [
    "base_floor",
    "base_roof",
    "bridge_end",
    "bridge_gentle_stairs",
    "bridge_piece",
    "bridge_steep_stairs",
    "fat_tower_base",
    "fat_tower_middle",
    "fat_tower_top",
    "second_floor_1",
    "second_floor_2",
    "second_roof",
    "ship",
    "third_floor_1",
    "third_floor_2",
    "third_roof",
    "tower_base",
    "tower_piece",
    "tower_top",
];

/// `EndCityPieces.TOWER_BRIDGES`.
const TOWER_BRIDGES: [(Rotation, BlockPos); 4] = [
    (Rotation::None, BlockPos::new(1, -1, 0)),
    (Rotation::Clockwise90, BlockPos::new(6, -1, 1)),
    (Rotation::Counterclockwise90, BlockPos::new(0, -1, 5)),
    (Rotation::Clockwise180, BlockPos::new(5, -1, 6)),
];

/// `EndCityPieces.FAT_TOWER_BRIDGES`.
const FAT_TOWER_BRIDGES: [(Rotation, BlockPos); 4] = [
    (Rotation::None, BlockPos::new(4, -1, 0)),
    (Rotation::Clockwise90, BlockPos::new(12, -1, 4)),
    (Rotation::Counterclockwise90, BlockPos::new(0, -1, 8)),
    (Rotation::Clockwise180, BlockPos::new(8, -1, 12)),
];

/// `EndCityStructure`.
#[derive(Debug)]
pub struct EndCityStructure {
    /// The templates by name (without the `end_city/` prefix).
    templates: FxHashMap<&'static str, Arc<StructureTemplate>>,
}

impl StructureType for EndCityStructure {
    fn parse(_json: &Value, parser: &mut StructureParser<'_>) -> Result<Self, Error> {
        let templates = TEMPLATES
            .iter()
            .map(|&name| {
                Ok((
                    name,
                    parser.template(&format!("minecraft:end_city/{name}"))?,
                ))
            })
            .collect::<Result<_, Error>>()?;
        Ok(Self { templates })
    }

    fn find_generation_point<'s>(
        &'s self,
        ctx: &mut GenerationContext<'_>,
    ) -> Option<GenerationStub<'s>> {
        let block_x = ctx.chunk_pos.min_block_x() + 7;
        let block_z = ctx.chunk_pos.min_block_z() + 7;
        if !ctx.could_valid_biome_exist_in_terrain_column(block_x, block_z) {
            return None;
        }
        let rotation = Rotation::random(&mut ctx.random.random);
        let start = lowest_y_in_5by5_box(ctx, block_x, block_z, rotation);
        if start.y < 60 {
            return None;
        }
        Some(GenerationStub::lazy(start, move |builder, ctx| {
            let mut generator = CityGenerator {
                templates: &self.templates,
                random: &mut ctx.random.random,
                ship_created: false,
            };
            for piece in generator.start_house_tower(start, rotation) {
                builder.add_piece(Box::new(piece));
            }
        }))
    }
}

/// `EndCityPieces.SectionGenerator`: the four generators of the city's sections.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Section {
    /// `HOUSE_TOWER_GENERATOR`.
    HouseTower,
    /// `TOWER_GENERATOR`.
    Tower,
    /// `TOWER_BRIDGE_GENERATOR`.
    TowerBridge,
    /// `FAT_TOWER_GENERATOR`.
    FatTower,
}

/// The state of one city's generation: the templates, the structure random and the static
/// `shipCreated` flag of `TOWER_BRIDGE_GENERATOR` (reset by `init()` for every city).
struct CityGenerator<'a, R> {
    templates: &'a FxHashMap<&'static str, Arc<StructureTemplate>>,
    random: &'a mut R,
    ship_created: bool,
}

impl<R: RandomSource> CityGenerator<'_, R> {
    /// `new EndCityPiece(manager, templateName, position, rotation, overwrite)`.
    fn piece(
        &self,
        name: &'static str,
        position: BlockPos,
        rotation: Rotation,
        overwrite: bool,
    ) -> EndCityPiece {
        let template = Arc::clone(&self.templates[name]);
        EndCityPiece::new(name, template, position, rotation, overwrite)
    }

    /// `addPiece(manager, parent, offset, templateName, rotation, overwrite)`: a piece connected
    /// to `parent` at `offset`.
    fn add_piece(
        &self,
        parent: &EndCityPiece,
        offset: BlockPos,
        name: &'static str,
        rotation: Rotation,
        overwrite: bool,
    ) -> EndCityPiece {
        let mut child = self.piece(name, parent.template.template_position, rotation, overwrite);
        let origin = StructureTemplate::calculate_connected_position(
            &parent.template.settings.place_settings(),
            offset,
            &child.template.settings.place_settings(),
            BlockPos::ZERO,
        );
        child.template.move_by(origin.x, origin.y, origin.z);
        child
    }

    /// `startHouseTower(manager, origin, rotation, pieces, random)`.
    fn start_house_tower(&mut self, origin: BlockPos, rotation: Rotation) -> Vec<EndCityPiece> {
        self.ship_created = false;
        let mut pieces = Vec::new();
        let last = add_helper(
            &mut pieces,
            self.piece("base_floor", origin, rotation, true),
        );
        let next = self.add_piece(
            &pieces[last],
            BlockPos::new(-1, 0, -1),
            "second_floor_1",
            rotation,
            false,
        );
        let last = add_helper(&mut pieces, next);
        let next = self.add_piece(
            &pieces[last],
            BlockPos::new(-1, 4, -1),
            "third_floor_1",
            rotation,
            false,
        );
        let last = add_helper(&mut pieces, next);
        let next = self.add_piece(
            &pieces[last],
            BlockPos::new(-1, 8, -1),
            "third_roof",
            rotation,
            true,
        );
        let last = add_helper(&mut pieces, next);
        self.recursive_children(Section::Tower, 1, &mut pieces, last, None);
        pieces
    }

    /// `recursiveChildren(manager, generator, genDepth, parent, offset, pieces, random)`: runs a
    /// section generator on a fresh list and keeps its pieces unless one collides with a piece
    /// of `pieces` from another generation (`parent` is an index into `pieces`).
    fn recursive_children(
        &mut self,
        section: Section,
        gen_depth: i32,
        pieces: &mut Vec<EndCityPiece>,
        parent: usize,
        offset: Option<BlockPos>,
    ) -> bool {
        if gen_depth > MAX_GEN_DEPTH {
            return false;
        }
        let mut children = Vec::new();
        if !self.generate(section, gen_depth, &pieces[parent], offset, &mut children) {
            return false;
        }
        let child_tag = self.random.next_int();
        let parent_depth = pieces[parent].base().gen_depth;
        for child in &mut children {
            child.base_mut().gen_depth = child_tag;
            if let Some(index) = find_collision_piece_in(pieces, child.bounding_box())
                && pieces[index].base().gen_depth != parent_depth
            {
                return false;
            }
        }
        pieces.append(&mut children);
        true
    }

    /// `SectionGenerator.generate(manager, genDepth, parent, offset, pieces, random)`.
    fn generate(
        &mut self,
        section: Section,
        gen_depth: i32,
        parent: &EndCityPiece,
        offset: Option<BlockPos>,
        pieces: &mut Vec<EndCityPiece>,
    ) -> bool {
        match section {
            Section::HouseTower => self.house_tower(gen_depth, parent, offset, pieces),
            Section::Tower => self.tower(gen_depth, parent, pieces),
            Section::TowerBridge => self.tower_bridge(gen_depth, parent, pieces),
            Section::FatTower => self.fat_tower(gen_depth, parent, pieces),
        }
    }

    /// `HOUSE_TOWER_GENERATOR`.
    fn house_tower(
        &mut self,
        gen_depth: i32,
        parent: &EndCityPiece,
        offset: Option<BlockPos>,
        pieces: &mut Vec<EndCityPiece>,
    ) -> bool {
        if gen_depth > MAX_GEN_DEPTH {
            return false;
        }
        let rotation = parent.template.settings.rotation;
        // Vanilla passes the (never null here) offset straight to `addPiece`.
        let offset = offset.unwrap_or(BlockPos::ZERO);
        let base = self.add_piece(parent, offset, "base_floor", rotation, true);
        let mut last = add_helper(pieces, base);
        let floors = self.random.next_int_bounded(3);
        let parts: &[(BlockPos, &'static str, bool)] = match floors {
            0 => &[(BlockPos::new(-1, 4, -1), "base_roof", true)],
            1 => &[
                (BlockPos::new(-1, 0, -1), "second_floor_2", false),
                (BlockPos::new(-1, 8, -1), "second_roof", false),
            ],
            _ => &[
                (BlockPos::new(-1, 0, -1), "second_floor_2", false),
                (BlockPos::new(-1, 4, -1), "third_floor_2", false),
                (BlockPos::new(-1, 8, -1), "third_roof", true),
            ],
        };
        for &(offset, name, overwrite) in parts {
            let next = self.add_piece(&pieces[last], offset, name, rotation, overwrite);
            last = add_helper(pieces, next);
        }
        if floors != 0 {
            self.recursive_children(Section::Tower, gen_depth + 1, pieces, last, None);
        }
        true
    }

    /// `TOWER_GENERATOR`.
    fn tower(
        &mut self,
        gen_depth: i32,
        parent: &EndCityPiece,
        pieces: &mut Vec<EndCityPiece>,
    ) -> bool {
        let rotation = parent.template.settings.rotation;
        let x = 3 + self.random.next_int_bounded(2);
        let z = 3 + self.random.next_int_bounded(2);
        let base = self.add_piece(
            parent,
            BlockPos::new(x, -3, z),
            "tower_base",
            rotation,
            true,
        );
        let mut last = add_helper(pieces, base);
        let next = self.add_piece(
            &pieces[last],
            BlockPos::new(0, 7, 0),
            "tower_piece",
            rotation,
            true,
        );
        last = add_helper(pieces, next);
        let mut bridge_piece = (self.random.next_int_bounded(3) == 0).then_some(last);
        let tower_height = 1 + self.random.next_int_bounded(3);
        for i in 0..tower_height {
            let next = self.add_piece(
                &pieces[last],
                BlockPos::new(0, 4, 0),
                "tower_piece",
                rotation,
                true,
            );
            last = add_helper(pieces, next);
            if i < tower_height - 1 && self.random.next_boolean() {
                bridge_piece = Some(last);
            }
        }
        if let Some(bridge_piece) = bridge_piece {
            for (bridge_rotation, offset) in TOWER_BRIDGES {
                if self.random.next_boolean() {
                    let start = self.add_piece(
                        &pieces[bridge_piece],
                        offset,
                        "bridge_end",
                        rotation.rotated(bridge_rotation),
                        true,
                    );
                    let start = add_helper(pieces, start);
                    self.recursive_children(
                        Section::TowerBridge,
                        gen_depth + 1,
                        pieces,
                        start,
                        None,
                    );
                }
            }
        } else if gen_depth != 7 {
            return self.recursive_children(Section::FatTower, gen_depth + 1, pieces, last, None);
        }
        let top = self.add_piece(
            &pieces[last],
            BlockPos::new(-1, 4, -1),
            "tower_top",
            rotation,
            true,
        );
        add_helper(pieces, top);
        true
    }

    /// `TOWER_BRIDGE_GENERATOR`.
    fn tower_bridge(
        &mut self,
        gen_depth: i32,
        parent: &EndCityPiece,
        pieces: &mut Vec<EndCityPiece>,
    ) -> bool {
        let rotation = parent.template.settings.rotation;
        let bridge_length = self.random.next_int_bounded(4) + 1;
        let first = self.add_piece(
            parent,
            BlockPos::new(0, 0, -4),
            "bridge_piece",
            rotation,
            true,
        );
        let mut last = add_helper(pieces, first);
        pieces[last].base_mut().gen_depth = -1;
        let mut next_y = 0;
        for _ in 0..bridge_length {
            let next = if self.random.next_boolean() {
                let piece = self.add_piece(
                    &pieces[last],
                    BlockPos::new(0, next_y, -4),
                    "bridge_piece",
                    rotation,
                    true,
                );
                next_y = 0;
                piece
            } else {
                let piece = if self.random.next_boolean() {
                    self.add_piece(
                        &pieces[last],
                        BlockPos::new(0, next_y, -4),
                        "bridge_steep_stairs",
                        rotation,
                        true,
                    )
                } else {
                    self.add_piece(
                        &pieces[last],
                        BlockPos::new(0, next_y, -8),
                        "bridge_gentle_stairs",
                        rotation,
                        true,
                    )
                };
                next_y = 4;
                piece
            };
            last = add_helper(pieces, next);
        }
        if !self.ship_created && self.random.next_int_bounded(10 - gen_depth) == 0 {
            let x = -8 + self.random.next_int_bounded(8);
            let z = -70 + self.random.next_int_bounded(10);
            let ship = self.add_piece(
                &pieces[last],
                BlockPos::new(x, next_y, z),
                "ship",
                rotation,
                true,
            );
            add_helper(pieces, ship);
            self.ship_created = true;
        } else if !self.recursive_children(
            Section::HouseTower,
            gen_depth + 1,
            pieces,
            last,
            Some(BlockPos::new(-3, next_y + 1, -11)),
        ) {
            return false;
        }
        let end = self.add_piece(
            &pieces[last],
            BlockPos::new(4, next_y, 0),
            "bridge_end",
            rotation.rotated(Rotation::Clockwise180),
            true,
        );
        let end = add_helper(pieces, end);
        pieces[end].base_mut().gen_depth = -1;
        true
    }

    /// `FAT_TOWER_GENERATOR`.
    fn fat_tower(
        &mut self,
        gen_depth: i32,
        parent: &EndCityPiece,
        pieces: &mut Vec<EndCityPiece>,
    ) -> bool {
        let rotation = parent.template.settings.rotation;
        let base = self.add_piece(
            parent,
            BlockPos::new(-3, 4, -3),
            "fat_tower_base",
            rotation,
            true,
        );
        let mut last = add_helper(pieces, base);
        let next = self.add_piece(
            &pieces[last],
            BlockPos::new(0, 4, 0),
            "fat_tower_middle",
            rotation,
            true,
        );
        last = add_helper(pieces, next);
        let mut i = 0;
        while i < 2 && self.random.next_int_bounded(3) != 0 {
            let next = self.add_piece(
                &pieces[last],
                BlockPos::new(0, 8, 0),
                "fat_tower_middle",
                rotation,
                true,
            );
            last = add_helper(pieces, next);
            for (bridge_rotation, offset) in FAT_TOWER_BRIDGES {
                if self.random.next_boolean() {
                    let start = self.add_piece(
                        &pieces[last],
                        offset,
                        "bridge_end",
                        rotation.rotated(bridge_rotation),
                        true,
                    );
                    let start = add_helper(pieces, start);
                    self.recursive_children(
                        Section::TowerBridge,
                        gen_depth + 1,
                        pieces,
                        start,
                        None,
                    );
                }
            }
            i += 1;
        }
        let top = self.add_piece(
            &pieces[last],
            BlockPos::new(-2, 8, -2),
            "fat_tower_top",
            rotation,
            true,
        );
        add_helper(pieces, top);
        true
    }
}

/// `addHelper(pieces, piece)`: appends the piece and returns its index.
fn add_helper(pieces: &mut Vec<EndCityPiece>, piece: EndCityPiece) -> usize {
    pieces.push(piece);
    pieces.len() - 1
}

/// `StructurePiece.findCollisionPiece(pieces, box)` over end city pieces.
fn find_collision_piece_in(pieces: &[EndCityPiece], bx: &BoundingBox) -> Option<usize> {
    pieces.iter().position(|p| p.bounding_box().intersects(bx))
}

/// `EndCityPieces.EndCityPiece` (`minecraft:ecp`).
#[derive(Clone, Debug)]
pub struct EndCityPiece {
    pub template: TemplatePiece,
    /// `OW`: whether the piece replaces air (`BlockIgnoreProcessor.STRUCTURE_BLOCK`) instead
    /// of skipping it (`STRUCTURE_AND_AIR`).
    pub overwrite: bool,
}

impl EndCityPiece {
    /// `new EndCityPiece(manager, templateName, position, rotation, overwrite)`.
    pub fn new(
        name: &str,
        template: Arc<StructureTemplate>,
        position: BlockPos,
        rotation: Rotation,
        overwrite: bool,
    ) -> Self {
        // `makeSettings(overwrite, rotation)`.
        let ignored = if overwrite {
            vec![ids::STRUCTURE_BLOCK]
        } else {
            vec![ids::STRUCTURE_BLOCK, ids::AIR]
        };
        let settings = PieceSettings {
            rotation,
            ignore_entities: true,
            processors: vec![StructureProcessor::BlockIgnore(ignored)],
            ..PieceSettings::new()
        };
        Self {
            template: TemplatePiece::new(
                "minecraft:ecp",
                0,
                name.to_owned(),
                template,
                settings,
                position,
            ),
            overwrite,
        }
    }
}

impl StructurePiece for EndCityPiece {
    fn base(&self) -> &PieceBase {
        &self.template.base
    }

    fn base_mut(&mut self) -> &mut PieceBase {
        &mut self.template.base
    }

    fn move_by(&mut self, dx: i32, dy: i32, dz: i32) {
        self.template.move_by(dx, dy, dz);
    }

    fn rotation(&self) -> Rotation {
        self.template.rotation()
    }

    /// `addAdditionalSaveData`: the template fields, `Rot` and `OW`.
    fn additional_save_data(&self) -> Option<Compound> {
        Some(
            self.template
                .save_data()
                .with("Rot", self.template.settings.rotation.legacy_name())
                .with("OW", self.overwrite),
        )
    }

    fn post_process(
        &mut self,
        ctx: &mut PieceContext<'_, '_>,
        chunk_bb: &mut BoundingBox,
        _chunk_pos: ChunkPos,
        reference: BlockPos,
    ) {
        let rotation = self.template.settings.rotation;
        self.template
            .place(ctx, chunk_bb, reference, |marker, pos, ctx, chunk_bb| {
                handle_data_marker(rotation, marker, pos, ctx, chunk_bb);
            });
    }
}

/// `EndCityPiece.handleDataMarker(markerId, position, level, random, chunkBB)`: chests get their
/// loot table, `Sentry` markers a shulker and `Elytra` markers the ship's item frame.
fn handle_data_marker(
    rotation: Rotation,
    marker: &str,
    pos: BlockPos,
    ctx: &mut PieceContext<'_, '_>,
    chunk_bb: &BoundingBox,
) {
    if marker.starts_with("Chest") {
        let chest = pos.relative(Direction::Down);
        if chunk_bb.is_inside(chest) {
            set_block_entity_loot_table(ctx, chest, LOOT_TABLE);
        }
    } else if chunk_bb.is_inside(pos) && is_in_spawnable_bounds(pos) {
        if marker.starts_with("Sentry") {
            let (x, y, z) = (
                f64::from(pos.x) + 0.5,
                f64::from(pos.y),
                f64::from(pos.z) + 0.5,
            );
            ctx.level
                .add_fresh_entity(entity_tag("minecraft:shulker", x, y, z));
        } else if marker.starts_with("Elytra") {
            ctx.level
                .add_fresh_entity(elytra_item_frame(pos, rotation.rotate(Direction::South)));
        }
    }
}

/// `Level.isInSpawnableBounds(pos)`.
fn is_in_spawnable_bounds(pos: BlockPos) -> bool {
    const MAX_HORIZONTAL: i32 = 30_000_000;
    const MAX_VERTICAL: i32 = 20_000_000;
    (-MAX_VERTICAL..MAX_VERTICAL).contains(&pos.y)
        && (-MAX_HORIZONTAL..MAX_HORIZONTAL).contains(&pos.x)
        && (-MAX_HORIZONTAL..MAX_HORIZONTAL).contains(&pos.z)
}

/// `new ItemFrame(level, pos, direction)` holding an elytra (`setItem(new ItemStack(ELYTRA))`).
///
/// The frame's position is the centre of its bounding box (`ItemFrame.createBoundingBox`): the
/// block's centre moved 0.46875 against `direction`, towards the wall it hangs on.
fn elytra_item_frame(pos: BlockPos, direction: Direction) -> Compound {
    const SHIFT_TO_BLOCK_WALL: f64 = 0.46875;
    let [dx, _, dz] = direction.normal();
    let x = f64::from(pos.x) + 0.5 - SHIFT_TO_BLOCK_WALL * f64::from(dx);
    let y = f64::from(pos.y) + 0.5;
    let z = f64::from(pos.z) + 0.5 - SHIFT_TO_BLOCK_WALL * f64::from(dz);
    let item = Compound::new()
        .with("id", "minecraft:elytra")
        .with("count", 1);
    entity_tag("minecraft:item_frame", x, y, z)
        .with("Facing", Tag::Byte(direction as i8))
        .with("Item", item)
        .with(
            "Rotation",
            List(vec![
                Tag::Float((data_2d(direction) * 90) as f32),
                Tag::Float(0.0),
            ]),
        )
}

/// `RandomizableContainer.setBlockEntityLootTable(level, random, pos, lootTable)`: the loot
/// table and a seed from the decoration random, if the block entity is a container.
pub(crate) fn set_block_entity_loot_table(
    ctx: &mut PieceContext<'_, '_>,
    pos: BlockPos,
    loot: &str,
) {
    if block_entity_type(ctx.level.block(pos))
        .is_some_and(|kind| RANDOMIZABLE_CONTAINERS.contains(&kind))
    {
        let seed = ctx.random.next_long();
        set_loot_table(ctx.level, pos, loot, seed);
    }
}
