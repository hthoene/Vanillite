//! `StructureBlock`: save, load, corner and data modes over structure templates.

use std::sync::Arc;

use vanillite_base::{BlockPos, ChunkPos};
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_nbt::{Compound, Tag};
use vanillite_protocol::packets::play::serverbound::SetStructureBlock;
use vanillite_protocol::text::Component;
use vanillite_worldgen::behaviour::StateExt;
use vanillite_worldgen::template::structure::StructureTemplate;
use vanillite_worldgen::template::transform::{Mirror, Rotation};

use super::Editor;
use super::templates::PlaceSettings;
use crate::block::BlockBehaviour;
use crate::level::{BlockEntity, Level};

/// `MAX_OFFSET_PER_AXIS`.
const MAX_OFFSET: i32 = 48;
/// `MAX_SIZE_PER_AXIS`.
const MAX_SIZE: i32 = 48;
/// `SCAN_CORNER_BLOCKS_RANGE`.
const SCAN_RANGE: i32 = 80;

/// `StructureMode`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    Save,
    #[default]
    Load,
    Corner,
    Data,
}

impl Mode {
    const ALL: [Mode; 4] = [Mode::Save, Mode::Load, Mode::Corner, Mode::Data];

    /// The value of the `mode` block state property.
    pub fn name(self) -> &'static str {
        match self {
            Mode::Save => "save",
            Mode::Load => "load",
            Mode::Corner => "corner",
            Mode::Data => "data",
        }
    }

    /// `StructureMode.LEGACY_CODEC` (the enum constant's name).
    fn legacy_name(self) -> &'static str {
        match self {
            Mode::Save => "SAVE",
            Mode::Load => "LOAD",
            Mode::Corner => "CORNER",
            Mode::Data => "DATA",
        }
    }

    /// The mode of the network id (`readEnum`).
    pub fn by_id(id: u8) -> Mode {
        Mode::ALL.get(usize::from(id)).copied().unwrap_or_default()
    }

    /// The mode of a structure block state.
    pub fn of(state: BlockState) -> Mode {
        let value = state.value("mode");
        Mode::ALL
            .into_iter()
            .find(|m| Some(m.name()) == value)
            .unwrap_or_default()
    }
}

/// `Rotation.LEGACY_CODEC` names.
const ROTATIONS: [(Rotation, &str); 4] = [
    (Rotation::None, "NONE"),
    (Rotation::Clockwise90, "CLOCKWISE_90"),
    (Rotation::Clockwise180, "CLOCKWISE_180"),
    (Rotation::Counterclockwise90, "COUNTERCLOCKWISE_90"),
];

/// `Mirror.LEGACY_CODEC` names.
const MIRRORS: [(Mirror, &str); 3] = [
    (Mirror::None, "NONE"),
    (Mirror::LeftRight, "LEFT_RIGHT"),
    (Mirror::FrontBack, "FRONT_BACK"),
];

/// The rotation of a network id (`Rotation.STREAM_CODEC`, unknown ids are none).
pub fn rotation_by_id(id: u8) -> Rotation {
    ROTATIONS
        .get(usize::from(id))
        .map_or(Rotation::None, |(r, _)| *r)
}

/// The mirror of a network id (`readEnum`).
pub fn mirror_by_id(id: u8) -> Mirror {
    MIRRORS
        .get(usize::from(id))
        .map_or(Mirror::None, |(m, _)| *m)
}

/// `ValueInput.getStringOr`.
pub(super) fn string_or<'a>(nbt: &'a Compound, key: &str, default: &'a str) -> &'a str {
    nbt.get(key).and_then(Tag::as_str).unwrap_or(default)
}

/// `ValueInput.getIntOr`: any number, narrowed like Java's casts.
#[allow(clippy::as_conversions, clippy::cast_possible_truncation)]
pub(super) fn int_or(nbt: &Compound, key: &str, default: i32) -> i32 {
    match nbt.get(key) {
        Some(Tag::Float(v)) => *v as i32,
        Some(Tag::Double(v)) => *v as i32,
        Some(tag) => tag.as_i64().map_or(default, |v| v as i32),
        None => default,
    }
}

/// `ValueInput.getLongOr`.
fn long_or(nbt: &Compound, key: &str, default: i64) -> i64 {
    nbt.get(key).and_then(Tag::as_i64).unwrap_or(default)
}

/// `ValueInput.getFloatOr`.
#[allow(clippy::as_conversions, clippy::cast_possible_truncation)]
fn float_or(nbt: &Compound, key: &str, default: f32) -> f32 {
    match nbt.get(key) {
        Some(Tag::Float(v)) => *v,
        Some(Tag::Double(v)) => *v as f32,
        Some(tag) => tag.as_i64().map_or(default, |v| v as f32),
        None => default,
    }
}

/// `ValueInput.getBooleanOr`.
fn bool_or(nbt: &Compound, key: &str, default: bool) -> bool {
    match nbt.get(key) {
        Some(Tag::Float(v)) => *v as i8 != 0,
        Some(Tag::Double(v)) => *v as i8 != 0,
        Some(tag) => tag.as_i64().map_or(default, |v| v as i8 != 0),
        None => default,
    }
}

/// `Identifier.tryParse`: `minecraft` for a missing namespace, `None` for invalid text.
pub(super) fn try_parse_identifier(text: &str) -> Option<String> {
    vanillite_protocol::types::Identifier::new(text)
        .ok()
        .map(|id| id.as_str().to_owned())
}

/// `StructureBlockEntity`.
#[derive(Clone, Debug, PartialEq)]
pub struct StructureBlock {
    /// `structureName`.
    pub name: Option<String>,
    /// `author`.
    pub author: String,
    /// `metaData`.
    pub metadata: String,
    /// `structurePos`: the offset of the area from the block.
    pub offset: BlockPos,
    /// `structureSize`.
    pub size: BlockPos,
    /// `mirror`.
    pub mirror: Mirror,
    /// `rotation`.
    pub rotation: Rotation,
    /// `mode`.
    pub mode: Mode,
    /// `ignoreEntities`.
    pub ignore_entities: bool,
    /// `strict`.
    pub strict: bool,
    /// `powered`.
    pub powered: bool,
    /// `showAir`.
    pub show_air: bool,
    /// `showBoundingBox`.
    pub show_bounding_box: bool,
    /// `integrity`.
    pub integrity: f32,
    /// `seed`.
    pub seed: i64,
}

impl Default for StructureBlock {
    fn default() -> Self {
        Self {
            name: None,
            author: String::new(),
            metadata: String::new(),
            offset: BlockPos::new(0, 1, 0),
            size: BlockPos::ZERO,
            mirror: Mirror::None,
            rotation: Rotation::None,
            mode: Mode::Load,
            ignore_entities: true,
            strict: false,
            powered: false,
            show_air: false,
            show_bounding_box: true,
            integrity: 1.0,
            seed: 0,
        }
    }
}

impl StructureBlock {
    /// `new StructureBlockEntity(pos, state)`: the mode of the block state.
    pub fn new(state: BlockState) -> Self {
        Self {
            mode: Mode::of(state),
            ..Self::default()
        }
    }

    /// `getStructureName`.
    pub fn name(&self) -> &str {
        self.name.as_deref().unwrap_or("")
    }

    /// `setStructureName(String)`: empty or invalid names clear it.
    pub fn set_name(&mut self, name: &str) {
        self.name = if name.is_empty() {
            None
        } else {
            try_parse_identifier(name)
        };
    }

    /// `loadAdditional` (the mode of the block follows in [`loaded`]).
    pub fn load(&mut self, nbt: &Compound) {
        self.set_name(string_or(nbt, "name", ""));
        self.author = string_or(nbt, "author", "").to_owned();
        self.metadata = string_or(nbt, "metadata", "").to_owned();
        let offset = |key, default| int_or(nbt, key, default).clamp(-MAX_OFFSET, MAX_OFFSET);
        self.offset = BlockPos::new(offset("posX", 0), offset("posY", 1), offset("posZ", 0));
        let size = |key| int_or(nbt, key, 0).clamp(0, MAX_SIZE);
        self.size = BlockPos::new(size("sizeX"), size("sizeY"), size("sizeZ"));
        let legacy = |key| nbt.get(key).and_then(Tag::as_str);
        self.rotation = ROTATIONS
            .iter()
            .find(|(_, n)| Some(*n) == legacy("rotation"))
            .map_or(Rotation::None, |(r, _)| *r);
        self.mirror = MIRRORS
            .iter()
            .find(|(_, n)| Some(*n) == legacy("mirror"))
            .map_or(Mirror::None, |(m, _)| *m);
        self.mode = Mode::ALL
            .into_iter()
            .find(|m| Some(m.legacy_name()) == legacy("mode"))
            .unwrap_or(Mode::Data);
        self.ignore_entities = bool_or(nbt, "ignoreEntities", true);
        self.strict = bool_or(nbt, "strict", false);
        self.powered = bool_or(nbt, "powered", false);
        self.show_air = bool_or(nbt, "showair", false);
        self.show_bounding_box = bool_or(nbt, "showboundingbox", true);
        self.integrity = float_or(nbt, "integrity", 1.0);
        self.seed = long_or(nbt, "seed", 0);
    }

    /// `saveAdditional`.
    pub fn save(&self, nbt: &mut Compound) {
        nbt.insert("name", Tag::String(self.name().to_owned()));
        nbt.insert("author", Tag::String(self.author.clone()));
        nbt.insert("metadata", Tag::String(self.metadata.clone()));
        nbt.insert("posX", Tag::Int(self.offset.x));
        nbt.insert("posY", Tag::Int(self.offset.y));
        nbt.insert("posZ", Tag::Int(self.offset.z));
        nbt.insert("sizeX", Tag::Int(self.size.x));
        nbt.insert("sizeY", Tag::Int(self.size.y));
        nbt.insert("sizeZ", Tag::Int(self.size.z));
        let rotation = ROTATIONS
            .iter()
            .find(|(r, _)| *r == self.rotation)
            .map_or("NONE", |(_, n)| n);
        let mirror = MIRRORS
            .iter()
            .find(|(m, _)| *m == self.mirror)
            .map_or("NONE", |(_, n)| n);
        nbt.insert("rotation", Tag::String(rotation.to_owned()));
        nbt.insert("mirror", Tag::String(mirror.to_owned()));
        nbt.insert("mode", Tag::String(self.mode.legacy_name().to_owned()));
        nbt.insert("ignoreEntities", Tag::Byte(i8::from(self.ignore_entities)));
        nbt.insert("strict", Tag::Byte(i8::from(self.strict)));
        nbt.insert("powered", Tag::Byte(i8::from(self.powered)));
        nbt.insert("showair", Tag::Byte(i8::from(self.show_air)));
        nbt.insert(
            "showboundingbox",
            Tag::Byte(i8::from(self.show_bounding_box)),
        );
        nbt.insert("integrity", Tag::Float(self.integrity));
        nbt.insert("seed", Tag::Long(self.seed));
    }

    /// `getUpdateTag`: `saveCustomOnly`.
    pub fn update_tag(&self) -> Option<Compound> {
        let mut nbt = Compound::new();
        self.save(&mut nbt);
        nbt.sort_java_hash_order();
        Some(nbt)
    }

    /// `loadStructureInfo(template)`: the template's author and size.
    fn load_info(&mut self, template: &StructureTemplate) {
        self.author = template.author().to_owned();
        self.size = template.size();
    }

    /// The `StructurePlaceSettings` of `placeStructure`.
    fn place_settings(&self) -> PlaceSettings {
        PlaceSettings {
            mirror: self.mirror,
            rotation: self.rotation,
            ignore_entities: self.ignore_entities,
            integrity: self.integrity,
            seed: self.seed,
            strict: self.strict,
        }
    }
}

fn get(level: &Level, pos: BlockPos) -> Option<&StructureBlock> {
    match level.block_entity(pos) {
        Some(BlockEntity::StructureBlock(b)) => Some(b),
        _ => None,
    }
}

fn get_mut(level: &mut Level, pos: BlockPos) -> Option<&mut StructureBlock> {
    match level.block_entity_mut(pos) {
        Some(BlockEntity::StructureBlock(b)) => Some(b),
        _ => None,
    }
}

/// `updateBlockState` after `loadAdditional` on a live block entity (data commands, template
/// placement): the block takes the loaded mode.
pub(crate) fn loaded(level: &mut Level, pos: BlockPos) {
    if let Some(mode) = get(level, pos).map(|b| b.mode) {
        set_block_mode(level, pos, mode);
    }
}

/// `setMode`'s block change: the `mode` property of the structure block, `UPDATE_CLIENTS`.
fn set_block_mode(level: &mut Level, pos: BlockPos, mode: Mode) {
    let state = level.block(pos);
    if state.is(ids::STRUCTURE_BLOCK)
        && let Some(new) = state.with("mode", mode.name())
        && new != state
    {
        level.set_block(pos, new, 2);
    }
}

/// The behaviour of the block.
pub(super) fn fill(block: BlockId, b: &mut BlockBehaviour) {
    if block == ids::STRUCTURE_BLOCK {
        b.neighbor_changed = Some(neighbor_changed);
    }
}

/// `StructureBlock.neighborChanged`: a rising signal triggers the block.
fn neighbor_changed(level: &mut Level, _state: BlockState, pos: BlockPos, _: BlockId, _: bool) {
    let signal = level.has_neighbor_signal(pos);
    let Some(block) = get_mut(level, pos) else {
        return;
    };
    if signal && !block.powered {
        block.powered = true;
        trigger(level, pos);
    } else if !signal && block.powered {
        block.powered = false;
    }
}

/// `StructureBlock.trigger`.
fn trigger(level: &mut Level, pos: BlockPos) {
    let Some(block) = get(level, pos) else {
        return;
    };
    match block.mode {
        Mode::Save => {
            save_structure(level, pos, false);
        }
        Mode::Load => {
            if let Some(name) = block.name.clone()
                && let Some(template) = level.structure_template(&name)
            {
                place_structure(level, pos, &template);
            }
        }
        Mode::Corner => {
            if let Some(name) = &block.name {
                level.forget_structure_template(name);
            }
        }
        Mode::Data => {}
    }
}

/// `setPlacedBy`: the placer becomes the author.
pub fn set_placed_by(
    level: &mut Level,
    pos: BlockPos,
    placer: Option<vanillite_world::entity::EntityId>,
) {
    let Some(name) = placer
        .and_then(|id| level.entities.player(id))
        .map(|p| p.profile.name.clone())
    else {
        return;
    };
    if let Some(block) = get_mut(level, pos) {
        block.author = name;
    }
}

/// `saveStructure(saveToDisk)`: the area into the template of the block's name (written to
/// `generated/` when `to_disk`); `false` without a name.
fn save_structure(level: &mut Level, pos: BlockPos, to_disk: bool) -> bool {
    let Some(block) = get(level, pos) else {
        return false;
    };
    let Some(name) = block.name.clone() else {
        return false;
    };
    let origin = pos.offset(block.offset.x, block.offset.y, block.offset.z);
    let (size, include_entities, author) =
        (block.size, !block.ignore_entities, block.author.clone());
    // `getOrCreate` + `fillFromWorld`: an area without volume keeps the template as it is.
    let mut template = if size.x >= 1 && size.y >= 1 && size.z >= 1 {
        level.capture_structure(origin, size, include_entities, &[ids::STRUCTURE_VOID])
    } else {
        level
            .structure_template(&name)
            .map_or_else(StructureTemplate::default, |t| (*t).clone())
    };
    template.set_author(author);
    if to_disk {
        level.save_structure_template(&name, template)
    } else {
        level.set_structure_template(&name, template);
        true
    }
}

/// `placeStructure(level, template)`.
fn place_structure(level: &mut Level, pos: BlockPos, template: &Arc<StructureTemplate>) {
    let Some(block) = get_mut(level, pos) else {
        return;
    };
    block.load_info(template);
    let settings = block.place_settings();
    let origin = pos.offset(block.offset.x, block.offset.y, block.offset.z);
    level.block_entity_changed(pos);
    level.place_structure(template, origin, &settings);
}

/// `detectSize`: the box of the corner blocks with the same name within 80 blocks
/// horizontally becomes the area.
fn detect_size(level: &mut Level, pos: BlockPos) -> bool {
    let Some(block) = get(level, pos) else {
        return false;
    };
    if block.mode != Mode::Save {
        return false;
    }
    let name = block.name.clone();
    // `getEnclosingBoundingBox`: structure blocks are found through their block entities.
    let (min_x, max_x) = (pos.x - SCAN_RANGE, pos.x + SCAN_RANGE);
    let (min_z, max_z) = (pos.z - SCAN_RANGE, pos.z + SCAN_RANGE);
    let mut count = 0;
    let mut bounds: Option<(BlockPos, BlockPos)> = None;
    for cx in (min_x >> 4)..=(max_x >> 4) {
        for cz in (min_z >> 4)..=(max_z >> 4) {
            for (p, entity) in level.block_entities.in_chunk(ChunkPos::new(cx, cz)) {
                let BlockEntity::StructureBlock(corner) = entity else {
                    continue;
                };
                if !(min_x..=max_x).contains(&p.x)
                    || !(min_z..=max_z).contains(&p.z)
                    || corner.mode != Mode::Corner
                    || corner.name != name
                    || !level.block(p).is(ids::STRUCTURE_BLOCK)
                {
                    continue;
                }
                count += 1;
                bounds = Some(encapsulate(bounds, p));
            }
        }
    }
    let Some((lo, hi)) = (match count {
        0 => None,
        1 => bounds.map(|b| encapsulate(Some(b), pos)),
        _ => bounds,
    }) else {
        return false;
    };
    let (dx, dy, dz) = (hi.x - lo.x, hi.y - lo.y, hi.z - lo.z);
    if dx <= 1 || dy <= 1 || dz <= 1 {
        return false;
    }
    let Some(block) = get_mut(level, pos) else {
        return false;
    };
    block.offset = BlockPos::new(lo.x - pos.x + 1, lo.y - pos.y + 1, lo.z - pos.z + 1);
    block.size = BlockPos::new(dx - 1, dy - 1, dz - 1);
    level.block_entity_changed(pos);
    level.send_block_updated(pos);
    true
}

fn encapsulate(bounds: Option<(BlockPos, BlockPos)>, p: BlockPos) -> (BlockPos, BlockPos) {
    match bounds {
        None => (p, p),
        Some((lo, hi)) => (
            BlockPos::new(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z)),
            BlockPos::new(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z)),
        ),
    }
}

/// `handleSetStructureBlock`.
pub fn handle(level: &mut Level, p: SetStructureBlock, editor: &Editor<'_>) {
    if !editor.allowed {
        return;
    }
    let pos = BlockPos::new(p.pos.x, p.pos.y, p.pos.z);
    let mode = Mode::by_id(p.mode);
    let Some(block) = get_mut(level, pos) else {
        return;
    };
    block.mode = mode;
    block.set_name(&p.name);
    block.offset = BlockPos::new(p.offset[0], p.offset[1], p.offset[2]);
    block.size = BlockPos::new(p.size[0], p.size[1], p.size[2]);
    block.mirror = mirror_by_id(p.mirror);
    block.rotation = rotation_by_id(p.rotation);
    block.metadata = p.data;
    block.ignore_entities = p.ignore_entities;
    block.strict = p.strict;
    block.show_air = p.show_air;
    block.show_bounding_box = p.show_bounding_box;
    block.integrity = p.integrity;
    block.seed = p.seed;
    let name = block.name.clone();
    set_block_mode(level, pos, mode);
    let message = |key: &str, name: &str| {
        Component::translatable(key, vec![Component::text(name.to_owned())])
    };
    match name {
        Some(name) => match p.update_type {
            // `SAVE_AREA`.
            1 => {
                let saved = mode == Mode::Save && save_structure(level, pos, true);
                editor.message(message(
                    if saved {
                        "structure_block.save_success"
                    } else {
                        "structure_block.save_failure"
                    },
                    &name,
                ));
            }
            // `LOAD_AREA`.
            2 => {
                let template = (mode == Mode::Load)
                    .then(|| level.structure_template(&name))
                    .flatten();
                let key = match template {
                    None => "structure_block.load_not_found",
                    // `placeStructureIfSameSize`.
                    Some(template) => {
                        let same = get(level, pos).is_some_and(|b| b.size == template.size());
                        if same {
                            place_structure(level, pos, &template);
                            "structure_block.load_success"
                        } else {
                            if let Some(block) = get_mut(level, pos) {
                                block.load_info(&template);
                            }
                            "structure_block.load_prepare"
                        }
                    }
                };
                editor.message(message(key, &name));
            }
            // `SCAN_AREA`.
            3 => {
                if detect_size(level, pos) {
                    editor.message(message("structure_block.size_success", &name));
                } else {
                    editor.message(Component::translatable(
                        "structure_block.size_failure",
                        vec![],
                    ));
                }
            }
            _ => {}
        },
        None => editor.message(message("structure_block.invalid_structure_name", &p.name)),
    }
    level.block_entity_changed(pos);
    level.send_block_updated(pos);
}
