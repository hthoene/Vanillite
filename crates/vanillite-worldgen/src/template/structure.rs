//! `StructureTemplate`: a template loaded from a data pack's `.nbt` file and its placement.

use std::sync::OnceLock;

use rustc_hash::FxHashSet;
use vanillite_base::{BlockPos, Direction, Vec3};
use vanillite_data::block::{BlockId, BlockState, flags as block_flags, ids};
use vanillite_nbt::{Compound, List, Tag};

use super::bounding_box::BoundingBox;
use super::liquid::{is_liquid_block_container, is_source, place_liquid};
use super::settings::{StructurePlaceSettings, TemplateRandom};
use super::transform::{
    Mirror, Rotation, direction_by_name, mirror_state, rotate_state, transform, transform_vec3,
};
use super::voxel::BitSetVoxelShape;
use crate::Error;
use crate::behaviour;
use crate::feature::region::{WorldGenRegion, block_entity_type};

/// `StructureTemplate.StructureBlockInfo`: a block of a template, template relative or placed.
#[derive(Clone, Debug, PartialEq)]
pub struct StructureBlockInfo {
    pub pos: BlockPos,
    pub state: BlockState,
    /// Block entity data.
    pub nbt: Option<Compound>,
}

impl StructureBlockInfo {
    /// The record's `equals`: compound tags compare as maps (entry order does not matter).
    pub fn same_as(&self, other: &Self) -> bool {
        self.pos == other.pos
            && self.state == other.state
            && match (&self.nbt, &other.nbt) {
                (None, None) => true,
                (Some(a), Some(b)) => compound_eq(a, b),
                _ => false,
            }
    }
}

fn compound_eq(a: &Compound, b: &Compound) -> bool {
    a.len() == b.len()
        && a.iter()
            .all(|(name, value)| b.get(name).is_some_and(|other| tag_eq(value, other)))
}

fn tag_eq(a: &Tag, b: &Tag) -> bool {
    match (a, b) {
        (Tag::Compound(a), Tag::Compound(b)) => compound_eq(a, b),
        (Tag::List(a), Tag::List(b)) => {
            a.0.len() == b.0.len() && a.0.iter().zip(&b.0).all(|(a, b)| tag_eq(a, b))
        }
        (Tag::Float(a), Tag::Float(b)) => a.to_bits() == b.to_bits(),
        (Tag::Double(a), Tag::Double(b)) => a.to_bits() == b.to_bits(),
        _ => a == b,
    }
}

/// `StructureTemplate.StructureEntityInfo`. Entities are loaded but not placed yet.
#[derive(Clone, Debug)]
pub struct StructureEntityInfo {
    pub pos: Vec3,
    pub block_pos: BlockPos,
    pub nbt: Compound,
}

/// `JigsawBlockEntity.JointType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JointType {
    Rollable,
    Aligned,
}

/// `StructureTemplate.JigsawBlockInfo`.
#[derive(Clone, Debug)]
pub struct JigsawBlockInfo {
    pub pos: BlockPos,
    pub state: BlockState,
    pub joint_type: JointType,
    pub name: String,
    pub pool: String,
    pub target: String,
    pub placement_priority: i32,
    pub selection_priority: i32,
}

/// `JigsawBlock.getFrontFacing(state)`: the front of the `orientation` property.
pub fn jigsaw_front(state: BlockState) -> Option<Direction> {
    state
        .value("orientation")
        .and_then(|o| o.split_once('_'))
        .and_then(|(front, _)| direction_by_name(front))
}

impl JigsawBlockInfo {
    /// `JigsawBlockInfo.parse(info)`.
    fn parse(info: &StructureBlockInfo) -> Self {
        let empty = Compound::new();
        let nbt = info.nbt.as_ref().unwrap_or(&empty);
        let string = |name: &str, default: &str| match nbt.get(name) {
            Some(Tag::String(s)) => crate::blocks::qualify(s),
            _ => default.to_owned(),
        };
        let int = |name: &str| match nbt.get(name) {
            Some(tag) => tag.as_i64().map_or(0, |v| v as i32),
            None => 0,
        };
        let joint_type = match nbt.get("joint").and_then(Tag::as_str) {
            Some("rollable") => JointType::Rollable,
            Some("aligned") => JointType::Aligned,
            _ => match jigsaw_front(info.state) {
                Some(Direction::Up | Direction::Down) => JointType::Rollable,
                _ => JointType::Aligned,
            },
        };
        Self {
            pos: info.pos,
            state: info.state,
            joint_type,
            name: string("name", "minecraft:empty"),
            pool: string("pool", "minecraft:empty"),
            target: string("target", "minecraft:empty"),
            placement_priority: int("placement_priority"),
            selection_priority: int("selection_priority"),
        }
    }
}

/// `StructureTemplate.Palette`: the blocks of one palette, in Vanilla's order (full blocks,
/// then other blocks, then block entities, each sorted by y, x, z).
#[derive(Debug)]
pub struct Palette {
    blocks: Vec<StructureBlockInfo>,
    jigsaws: OnceLock<Vec<JigsawBlockInfo>>,
}

impl Palette {
    /// `blocks()`.
    pub fn blocks(&self) -> &[StructureBlockInfo] {
        &self.blocks
    }

    /// `blocks(block)`: the blocks of one type.
    pub fn blocks_of(&self, block: BlockId) -> impl Iterator<Item = &StructureBlockInfo> {
        self.blocks.iter().filter(move |b| b.state.block() == block)
    }

    /// `jigsaws()`.
    pub fn jigsaws(&self) -> &[JigsawBlockInfo] {
        self.jigsaws.get_or_init(|| {
            self.blocks_of(ids::JIGSAW)
                .map(JigsawBlockInfo::parse)
                .collect()
        })
    }
}

/// `StructureTemplate`.
#[derive(Debug, Default)]
pub struct StructureTemplate {
    palettes: Vec<Palette>,
    entities: Vec<StructureEntityInfo>,
    size: BlockPos,
}

/// Randomizable container block entities: `placeInWorld` draws a loot table seed for them.
pub(crate) const RANDOMIZABLE_CONTAINERS: [&str; 9] = [
    "minecraft:chest",
    "minecraft:trapped_chest",
    "minecraft:dispenser",
    "minecraft:dropper",
    "minecraft:hopper",
    "minecraft:shulker_box",
    "minecraft:barrel",
    "minecraft:decorated_pot",
    "minecraft:crafter",
];

/// `Block.isShapeFullBlock(state.getShape(level, pos))`: the outline shape is the full cube.
pub fn is_shape_full_block(state: BlockState) -> bool {
    if state.has(block_flags::OUTLINE_OFFSET) {
        // Offset shapes (plants) are never full.
        return false;
    }
    let boxes = state.outline_shape();
    if boxes.is_empty() {
        return false;
    }
    if boxes.contains(&[0.0, 0.0, 0.0, 1.0, 1.0, 1.0]) {
        return true;
    }
    // Several boxes may still cover the cube: check every cell of the grid their bounds make.
    let mut coords: [Vec<f64>; 3] = [vec![0.0, 1.0], vec![0.0, 1.0], vec![0.0, 1.0]];
    for b in boxes {
        for axis in 0..3 {
            coords[axis].push(b[axis].clamp(0.0, 1.0));
            coords[axis].push(b[axis + 3].clamp(0.0, 1.0));
        }
    }
    for c in &mut coords {
        c.sort_by(f64::total_cmp);
        c.dedup();
    }
    let mid = |c: &[f64], i: usize| (c[i] + c[i + 1]) / 2.0;
    for i in 0..coords[0].len() - 1 {
        for j in 0..coords[1].len() - 1 {
            for k in 0..coords[2].len() - 1 {
                let (x, y, z) = (mid(&coords[0], i), mid(&coords[1], j), mid(&coords[2], k));
                let covered = boxes.iter().any(|b| {
                    b[0] <= x && x <= b[3] && b[1] <= y && y <= b[4] && b[2] <= z && z <= b[5]
                });
                if !covered {
                    return false;
                }
            }
        }
    }
    true
}

/// `addToLists` + `buildInfoList`: sorts the blocks into Vanilla's placement order.
fn build_info_list(blocks: Vec<StructureBlockInfo>) -> Vec<StructureBlockInfo> {
    let mut full = Vec::new();
    let mut other = Vec::new();
    let mut entities = Vec::new();
    for info in blocks {
        if info.nbt.is_some() {
            entities.push(info);
        } else if !info.state.block().info().dynamic_shape
            && info.state.has(block_flags::COLLISION_FULL_BLOCK)
        {
            full.push(info);
        } else {
            other.push(info);
        }
    }
    let key = |b: &StructureBlockInfo| (b.pos.y, b.pos.x, b.pos.z);
    // `List.sort` is stable, as is `sort_by_key`.
    full.sort_by_key(key);
    other.sort_by_key(key);
    entities.sort_by_key(key);
    full.extend(other);
    full.extend(entities);
    full
}

fn int_list(compound: &Compound, name: &str) -> [i32; 3] {
    let mut out = [0; 3];
    if let Some(Tag::List(list)) = compound.get(name) {
        for (slot, tag) in out.iter_mut().zip(&list.0) {
            if let Tag::Int(v) = tag {
                *slot = *v;
            }
        }
    }
    out
}

fn double_list(compound: &Compound, name: &str) -> [f64; 3] {
    let mut out = [0.0; 3];
    if let Some(Tag::List(list)) = compound.get(name) {
        for (slot, tag) in out.iter_mut().zip(&list.0) {
            if let Tag::Double(v) = tag {
                *slot = *v;
            }
        }
    }
    out
}

fn compounds(tag: Option<&Tag>) -> impl Iterator<Item = &Compound> {
    let list: &[Tag] = match tag {
        Some(Tag::List(List(items))) => items,
        _ => &[],
    };
    list.iter().filter_map(Tag::as_compound)
}

/// `NbtUtils.readBlockState`: `{id, properties}`; unknown blocks are air, unknown properties
/// and values are skipped.
fn read_block_state(tag: &Compound) -> BlockState {
    let Some(block) = tag
        .get("id")
        .and_then(Tag::as_str)
        .and_then(|name| BlockId::by_name(&crate::blocks::qualify(name)))
    else {
        return BlockState::AIR;
    };
    let mut state = block.default_state();
    if let Some(Tag::Compound(properties)) = tag.get("properties") {
        for (name, value) in properties.iter() {
            if let Some(value) = value.as_str() {
                state = state.with(name, value).unwrap_or(state);
            }
        }
    }
    state
}

impl StructureTemplate {
    /// `load(blockLookup, tag)`: a template from its (data fixed) NBT. Vanilla upgrades older
    /// data versions with DataFixerUpper first; the Vanilla pack ships current versions only.
    pub fn load(tag: &Compound) -> Self {
        let [sx, sy, sz] = int_list(tag, "size");
        let blocks: Vec<&Compound> = compounds(tag.get("blocks")).collect();
        let mut palettes = Vec::new();
        match tag.get("palettes") {
            Some(Tag::List(lists)) => {
                for palette in &lists.0 {
                    palettes.push(Self::load_palette(Some(palette), &blocks));
                }
            }
            _ => palettes.push(Self::load_palette(tag.get("palette"), &blocks)),
        }
        let entities = compounds(tag.get("entities"))
            .filter_map(|entity| {
                let [x, y, z] = double_list(entity, "pos");
                let [bx, by, bz] = int_list(entity, "blockPos");
                let Some(Tag::Compound(nbt)) = entity.get("nbt") else {
                    return None;
                };
                Some(StructureEntityInfo {
                    pos: Vec3::new(x, y, z),
                    block_pos: BlockPos::new(bx, by, bz),
                    nbt: nbt.clone(),
                })
            })
            .collect();
        Self {
            palettes,
            entities,
            size: BlockPos::new(sx, sy, sz),
        }
    }

    fn load_palette(palette: Option<&Tag>, blocks: &[&Compound]) -> Palette {
        let states: Vec<BlockState> = compounds(palette).map(read_block_state).collect();
        let infos = blocks
            .iter()
            .map(|block| {
                let [x, y, z] = int_list(block, "pos");
                let index = match block.get("state") {
                    Some(Tag::Int(i)) => *i,
                    _ => 0,
                };
                StructureBlockInfo {
                    pos: BlockPos::new(x, y, z),
                    // `SimplePalette.stateFor`: unknown ids are air.
                    state: usize::try_from(index)
                        .ok()
                        .and_then(|i| states.get(i).copied())
                        .unwrap_or(BlockState::AIR),
                    nbt: match block.get("nbt") {
                        Some(Tag::Compound(nbt)) => Some(nbt.clone()),
                        _ => None,
                    },
                }
            })
            .collect();
        Palette {
            blocks: build_info_list(infos),
            jigsaws: OnceLock::new(),
        }
    }

    /// Reads a gzip compressed template file (`NbtIo.readCompressed` + [`Self::load`]).
    pub fn from_gzip(bytes: &[u8]) -> Result<Self, Error> {
        let data = gunzip(bytes)?;
        let mut budget = vanillite_nbt::Budget::file();
        let tag = vanillite_nbt::read::read_named(&mut data.as_slice(), &mut budget)
            .map_err(|e| Error::new(format!("invalid structure NBT: {e}")))?;
        Ok(Self::load(&tag))
    }

    /// `getSize()`.
    pub fn size(&self) -> BlockPos {
        self.size
    }

    /// `getSize(rotation)`: x and z swap for quarter turns.
    pub fn size_rotated(&self, rotation: Rotation) -> BlockPos {
        match rotation {
            Rotation::Clockwise90 | Rotation::Counterclockwise90 => {
                BlockPos::new(self.size.z, self.size.y, self.size.x)
            }
            _ => self.size,
        }
    }

    /// The palettes.
    pub fn palettes(&self) -> &[Palette] {
        &self.palettes
    }

    /// The entities (not placed yet).
    pub fn entities(&self) -> &[StructureEntityInfo] {
        &self.entities
    }

    /// `calculateRelativePosition(settings, pos)`.
    pub fn calculate_relative_position(
        settings: &StructurePlaceSettings<'_>,
        pos: BlockPos,
    ) -> BlockPos {
        transform(
            pos,
            settings.mirror,
            settings.rotation,
            settings.rotation_pivot,
        )
    }

    /// `calculateConnectedPosition(settings1, connection1, settings2, connection2)`.
    pub fn calculate_connected_position(
        settings1: &StructurePlaceSettings<'_>,
        connection1: BlockPos,
        settings2: &StructurePlaceSettings<'_>,
        connection2: BlockPos,
    ) -> BlockPos {
        let a = Self::calculate_relative_position(settings1, connection1);
        let b = Self::calculate_relative_position(settings2, connection2);
        BlockPos::new(a.x - b.x, a.y - b.y, a.z - b.z)
    }

    /// `getZeroPositionWithTransform(zeroPos, mirror, rotation)`.
    pub fn zero_position_with_transform(
        &self,
        zero: BlockPos,
        mirror: Mirror,
        rotation: Rotation,
    ) -> BlockPos {
        zero_position_with_transform(zero, mirror, rotation, self.size.x, self.size.z)
    }

    /// `getBoundingBox(settings, position)`.
    pub fn bounding_box(
        &self,
        settings: &StructurePlaceSettings<'_>,
        position: BlockPos,
    ) -> BoundingBox {
        self.bounding_box_with(
            position,
            settings.rotation,
            settings.rotation_pivot,
            settings.mirror,
        )
    }

    /// `getBoundingBox(position, rotation, pivot, mirror)`.
    pub fn bounding_box_with(
        &self,
        position: BlockPos,
        rotation: Rotation,
        pivot: BlockPos,
        mirror: Mirror,
    ) -> BoundingBox {
        let delta = self.size.offset(-1, -1, -1);
        let a = transform(BlockPos::ZERO, mirror, rotation, pivot);
        let b = transform(delta, mirror, rotation, pivot);
        BoundingBox::from_corners(a, b).moved(position.x, position.y, position.z)
    }

    /// `filterBlocks(position, settings, block, absolute)`: the blocks of one type of the
    /// randomly chosen palette, with transformed positions (when `absolute`) and rotated states.
    pub fn filter_blocks(
        &self,
        position: BlockPos,
        settings: &StructurePlaceSettings<'_>,
        block: BlockId,
        absolute: bool,
        random: &mut dyn TemplateRandom,
    ) -> Vec<StructureBlockInfo> {
        let Some(palette) = settings.random_palette(&self.palettes, position, random) else {
            return Vec::new();
        };
        palette
            .blocks_of(block)
            .filter_map(|info| {
                let pos = if absolute {
                    let rel = Self::calculate_relative_position(settings, info.pos);
                    rel.offset(position.x, position.y, position.z)
                } else {
                    info.pos
                };
                settings
                    .bounding_box
                    .is_none_or(|bb| bb.is_inside(pos))
                    .then(|| StructureBlockInfo {
                        pos,
                        state: rotate_state(info.state, settings.rotation),
                        nbt: info.nbt.clone(),
                    })
            })
            .collect()
    }

    /// `getJigsaws(position, rotation)`: the jigsaw blocks of the palette chosen with a
    /// positional random, placed and rotated.
    pub fn jigsaws(&self, position: BlockPos, rotation: Rotation) -> Vec<JigsawBlockInfo> {
        let settings = StructurePlaceSettings::new().with_rotation(rotation);
        // The settings have no random: the palette comes from `Mth.getSeed(position)`.
        let mut unused = vanillite_base::random::LegacyRandom::new(0);
        let Some(palette) = settings.random_palette(&self.palettes, position, &mut unused) else {
            return Vec::new();
        };
        palette
            .jigsaws()
            .iter()
            .map(|jigsaw| {
                let rel = Self::calculate_relative_position(&settings, jigsaw.pos);
                JigsawBlockInfo {
                    pos: rel.offset(position.x, position.y, position.z),
                    state: rotate_state(jigsaw.state, rotation),
                    ..jigsaw.clone()
                }
            })
            .collect()
    }

    /// `placeInWorld(level, position, referencePos, settings, random, updateMode)`.
    ///
    /// Entities are not placed yet (they count for the return value as in Vanilla).
    pub fn place_in_world(
        &self,
        level: &mut WorldGenRegion<'_>,
        position: BlockPos,
        reference: BlockPos,
        settings: &StructurePlaceSettings<'_>,
        random: &mut dyn TemplateRandom,
        update_mode: i32,
    ) -> bool {
        let Some(palette) = settings.random_palette(&self.palettes, position, random) else {
            return false;
        };
        let blocks = palette.blocks();
        if (blocks.is_empty() && (settings.ignore_entities || self.entities.is_empty()))
            || self.size.x < 1
            || self.size.y < 1
            || self.size.z < 1
        {
            return false;
        }
        let bounding_box = settings.bounding_box;
        let waterlogging = settings.should_apply_waterlogging();
        let mut to_fill: Vec<BlockPos> = Vec::new();
        let mut locked_fluids: FxHashSet<BlockPos> = FxHashSet::default();
        let mut placed: Vec<(BlockPos, bool)> = Vec::with_capacity(blocks.len());
        let (mut min_x, mut min_y, mut min_z) = (i32::MAX, i32::MAX, i32::MAX);
        let (mut max_x, mut max_y, mut max_z) = (i32::MIN, i32::MIN, i32::MIN);
        let processed =
            process_block_infos(level, position, reference, settings, blocks, &mut *random);

        for info in processed {
            let pos = info.pos;
            // Outside the write zone both `setBlock` calls fail and nothing else happens; skipping
            // early avoids reading chunks the region does not hold.
            if bounding_box.is_some_and(|bb| !bb.is_inside(pos)) || !level.is_within_write_zone(pos)
            {
                continue;
            }
            let previous_fluid = waterlogging.then(|| level.fluid(pos));
            let state = rotate_state(mirror_state(info.state, settings.mirror), settings.rotation);
            if info.nbt.is_some() {
                level.set_block(pos, ids::BARRIER.default_state(), 820);
            }
            if !level.set_block(pos, state, update_mode) {
                continue;
            }
            min_x = min_x.min(pos.x);
            min_y = min_y.min(pos.y);
            min_z = min_z.min(pos.z);
            max_x = max_x.max(pos.x);
            max_y = max_y.max(pos.y);
            max_z = max_z.max(pos.z);
            placed.push((pos, info.nbt.is_some()));
            if let Some(nbt) = info.nbt {
                load_block_entity(level, pos, nbt, random);
            }
            if let Some(previous) = previous_fluid {
                if is_source(state.fluid_state()) {
                    locked_fluids.insert(pos);
                } else if is_liquid_block_container(state) {
                    place_liquid(level, pos, state, previous);
                    if !is_source(previous) {
                        to_fill.push(pos);
                    }
                }
            }
        }

        const FILL_DIRECTIONS: [Direction; 5] = [
            Direction::Up,
            Direction::North,
            Direction::East,
            Direction::South,
            Direction::West,
        ];
        let mut filled = true;
        while filled && !to_fill.is_empty() {
            filled = false;
            to_fill.retain(|&pos| {
                let mut to_place = level.fluid(pos);
                let mut i = 0;
                while i < FILL_DIRECTIONS.len() && !is_source(to_place) {
                    let neighbour_pos = pos.relative(FILL_DIRECTIONS[i]);
                    // Neighbours outside the region's chunks are not read (see
                    // `neighbours_in_region`).
                    let neighbour = if level.has_chunk(neighbour_pos.x >> 4, neighbour_pos.z >> 4) {
                        level.fluid(neighbour_pos)
                    } else {
                        vanillite_data::fluid::FluidState::EMPTY
                    };
                    if is_source(neighbour) && !locked_fluids.contains(&neighbour_pos) {
                        to_place = neighbour;
                    }
                    i += 1;
                }
                if is_source(to_place) {
                    let state = level.block(pos);
                    if is_liquid_block_container(state) {
                        place_liquid(level, pos, state, to_place);
                        filled = true;
                        return false;
                    }
                }
                true
            });
        }

        if min_x <= max_x {
            if !settings.known_shape {
                let size = |min: i32, max: i32| (i64::from(max) - i64::from(min) + 1) as usize;
                let mut shape = BitSetVoxelShape::new(
                    size(min_x, max_x),
                    size(min_y, max_y),
                    size(min_z, max_z),
                );
                for &(pos, _) in &placed {
                    shape.fill(
                        (pos.x - min_x) as usize,
                        (pos.y - min_y) as usize,
                        (pos.z - min_z) as usize,
                    );
                }
                update_shape_at_edge(level, update_mode, &shape, min_x, min_y, min_z);
            }
            for &(pos, _) in &placed {
                if !settings.known_shape && neighbours_in_region(level, pos) {
                    let state = level.block(pos);
                    let new_state = behaviour::update_from_neighbour_shapes(level, state, pos);
                    if state != new_state {
                        level.set_block(
                            pos,
                            new_state,
                            (update_mode & !1) | crate::feature::region::flags::KNOWN_SHAPE,
                        );
                    }
                    // `updateNeighborsAt` is a no-op on a `WorldGenRegion`.
                }
                // `blockEntity.setChanged()` has no effect during generation.
            }
        }
        if !settings.ignore_entities {
            self.place_entities(level, position, settings);
        }
        true
    }

    /// `placeEntities(level, position, mirror, rotation, pivot, boundingBox, finalizeEntities)`:
    /// the template's entities whose block position is inside the box, with their `Pos` moved
    /// into the world (`UUID` dropped). `finalizeSpawn` of mobs (with `finalizeEntities`) is not
    /// run, so its draws from the level's random are not made either.
    fn place_entities(
        &self,
        level: &mut WorldGenRegion<'_>,
        position: BlockPos,
        settings: &StructurePlaceSettings<'_>,
    ) {
        let (mirror, rotation, pivot) =
            (settings.mirror, settings.rotation, settings.rotation_pivot);
        for info in &self.entities {
            let block_pos = transform(info.block_pos, mirror, rotation, pivot)
                .offset(position.x, position.y, position.z);
            if settings
                .bounding_box
                .is_some_and(|bb| !bb.is_inside(block_pos))
            {
                continue;
            }
            // `EntityType.create(tag)` fails without a known type.
            if !matches!(info.nbt.get("id"), Some(Tag::String(_))) {
                continue;
            }
            let relative = transform_vec3(info.pos, mirror, rotation, pivot);
            let pos = Vec3::new(
                relative.x + f64::from(position.x),
                relative.y + f64::from(position.y),
                relative.z + f64::from(position.z),
            );
            let mut tag = info.nbt.clone();
            tag.insert(
                "Pos",
                List(vec![
                    Tag::Double(pos.x),
                    Tag::Double(pos.y),
                    Tag::Double(pos.z),
                ]),
            );
            tag.remove("UUID");
            if tag.get("block_pos").is_some() {
                tag.insert(
                    "block_pos",
                    Tag::IntArray(vec![block_pos.x, block_pos.y, block_pos.z]),
                );
            }
            level.add_fresh_entity(tag);
        }
    }
}

/// `blockEntity.loadWithComponents(nbt)` after the `LootTableSeed` draw of randomizable
/// containers: the template's data replaces the block entity's (its position and type stay).
fn load_block_entity(
    level: &mut WorldGenRegion<'_>,
    pos: BlockPos,
    mut nbt: Compound,
    random: &mut dyn TemplateRandom,
) {
    let state = level.block(pos);
    let Some(kind) = block_entity_type(state) else {
        return;
    };
    let Some(entity) = level.block_entity_mut(pos) else {
        return;
    };
    if RANDOMIZABLE_CONTAINERS.contains(&kind) {
        let seed = random.next_long();
        // The seed is only kept (and saved) together with a loot table.
        if nbt.get("LootTable").is_some() {
            nbt.insert("LootTableSeed", Tag::Long(seed));
        }
    }
    for (name, value) in nbt.iter() {
        if !matches!(name, "x" | "y" | "z" | "id") {
            entity.insert(name, value.clone());
        }
    }
    super::block_entity::normalize_loaded(kind, entity);
}

/// Whether the horizontal neighbours of `pos` are in chunks the region holds. Vanilla's region
/// also reads chunks beyond its write radius; ours holds only those, so shape updates that
/// would read further are skipped instead of panicking.
fn neighbours_in_region(level: &WorldGenRegion<'_>, pos: BlockPos) -> bool {
    level.has_chunk((pos.x - 1) >> 4, pos.z >> 4)
        && level.has_chunk((pos.x + 1) >> 4, pos.z >> 4)
        && level.has_chunk(pos.x >> 4, (pos.z - 1) >> 4)
        && level.has_chunk(pos.x >> 4, (pos.z + 1) >> 4)
}

/// `getZeroPositionWithTransform(zeroPos, mirror, rotation, sizeX, sizeZ)`.
pub fn zero_position_with_transform(
    zero: BlockPos,
    mirror: Mirror,
    rotation: Rotation,
    size_x: i32,
    size_z: i32,
) -> BlockPos {
    let size_x = size_x - 1;
    let size_z = size_z - 1;
    let mirror_dx = if mirror == Mirror::FrontBack {
        size_x
    } else {
        0
    };
    let mirror_dz = if mirror == Mirror::LeftRight {
        size_z
    } else {
        0
    };
    match rotation {
        Rotation::Counterclockwise90 => zero.offset(mirror_dz, 0, size_x - mirror_dx),
        Rotation::Clockwise90 => zero.offset(size_z - mirror_dz, 0, mirror_dx),
        Rotation::Clockwise180 => zero.offset(size_x - mirror_dx, 0, size_z - mirror_dz),
        Rotation::None => zero.offset(mirror_dx, 0, mirror_dz),
    }
}

/// `updateShapeAtEdge(level, updateMode, shape, startX, startY, startZ)`: updates the shapes on
/// both sides of every outer face of the placed blocks.
///
/// Neighbours in chunks the region does not hold are skipped (Vanilla's larger region would read
/// them; ours would panic).
pub fn update_shape_at_edge(
    level: &mut WorldGenRegion<'_>,
    update_mode: i32,
    shape: &BitSetVoxelShape,
    start_x: i32,
    start_y: i32,
    start_z: i32,
) {
    let flags = update_mode & !1;
    shape.for_all_faces(|direction, x, y, z| {
        let pos = BlockPos::new(start_x + x as i32, start_y + y as i32, start_z + z as i32);
        let neighbour_pos = pos.relative(direction);
        if !level.has_chunk(neighbour_pos.x >> 4, neighbour_pos.z >> 4) {
            return;
        }
        let state = level.block(pos);
        let neighbour = level.block(neighbour_pos);
        let new_state =
            behaviour::update_shape(level, state, pos, direction, neighbour_pos, neighbour);
        if state != new_state {
            level.set_block(pos, new_state, flags);
        }
        let new_neighbour = behaviour::update_shape(
            level,
            neighbour,
            neighbour_pos,
            direction.opposite(),
            pos,
            new_state,
        );
        if neighbour != new_neighbour {
            level.set_block(neighbour_pos, new_neighbour, flags);
        }
    });
}

/// `processBlockInfos(level, position, referencePos, settings, blockInfoList)`: transforms
/// the template blocks to world positions and runs the processors on them.
pub fn process_block_infos(
    level: &WorldGenRegion<'_>,
    position: BlockPos,
    reference: BlockPos,
    settings: &StructurePlaceSettings<'_>,
    blocks: &[StructureBlockInfo],
    random: &mut dyn TemplateRandom,
) -> Vec<StructureBlockInfo> {
    let only_in_chunk = !settings
        .processors
        .iter()
        .any(|p| p.evaluates_entire_piece_state());
    let chunk_bb = settings.bounding_box;
    let mut originals: Vec<BlockPos> = Vec::new();
    let mut processed: Vec<StructureBlockInfo> = Vec::with_capacity(blocks.len());
    for info in blocks {
        let pos = StructureTemplate::calculate_relative_position(settings, info.pos)
            .offset(position.x, position.y, position.z);
        if only_in_chunk && chunk_bb.is_some_and(|bb| !bb.is_inside(pos)) {
            continue;
        }
        let mut current = Some(StructureBlockInfo {
            pos,
            state: info.state,
            nbt: info.nbt.clone(),
        });
        for processor in &settings.processors {
            let Some(block) = current else { break };
            current = processor.process_block(
                level, position, reference, info.pos, block, settings, random,
            );
        }
        if let Some(block) = current {
            processed.push(block);
            originals.push(info.pos);
        }
    }
    for processor in &settings.processors {
        processor.finalize_processing(
            level,
            position,
            reference,
            &originals,
            &mut processed,
            settings,
            random,
        );
    }
    processed
}

/// Gunzips a whole `.nbt` file.
fn gunzip(bytes: &[u8]) -> Result<Vec<u8>, Error> {
    let trailer: [u8; 4] = bytes
        .len()
        .checked_sub(4)
        .and_then(|start| bytes.get(start..))
        .and_then(|t| t.try_into().ok())
        .ok_or_else(|| Error::new("truncated gzip data"))?;
    let size = u32::from_le_bytes(trailer) as usize;
    if size as u64 > vanillite_nbt::read::FILE_QUOTA {
        return Err(Error::new("structure file too large"));
    }
    let mut out = vec![0; size];
    let written = libdeflater::Decompressor::new()
        .gzip_decompress(bytes, &mut out)
        .map_err(|e| Error::new(format!("invalid gzip data: {e:?}")))?;
    out.truncate(written);
    Ok(out)
}
