//! `TestInstanceBlock` and `TestInstanceBlockEntity`: a game test placed in the world, with
//! its structure in front of the block. What depends on the test definition (its rotation,
//! padding and sky access) comes in as a [`TestShape`]; the test registry and the runner are
//! the server's.

use std::sync::Arc;

use vanillite_base::{Aabb, BlockPos, ChunkPos};
use vanillite_data::block::{BlockId, ids};
use vanillite_nbt::{Compound, List, Tag};
use vanillite_protocol::packets::play::serverbound::{TestInstanceBlockAction, TestInstanceData};
use vanillite_protocol::text::{Arg, Color, Component, NamedColor};
use vanillite_worldgen::template::structure::StructureTemplate;
use vanillite_worldgen::template::transform::Rotation;

use super::Editor;
use super::templates::PlaceSettings;
use crate::block::BlockBehaviour;
use crate::level::{BlockEntity, Level, update};

/// `STRUCTURE_OFFSET`: the structure starts one block up and one block south of the block.
const STRUCTURE_OFFSET: BlockPos = BlockPos::new(0, 1, 1);
/// The flags structures are placed and cleared with (`818`).
pub const PLACE_FLAGS: u32 = update::CLIENTS
    | update::KNOWN_SHAPE
    | update::SUPPRESS_DROPS
    | update::SKIP_BLOCK_ENTITY_SIDEEFFECTS
    | update::SKIP_ON_PLACE;

/// `TestInstanceBlockEntity.Status`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Status {
    /// `cleared`.
    #[default]
    Cleared,
    /// `running`.
    Running,
    /// `finished`.
    Finished,
}

impl Status {
    const ALL: [Status; 3] = [Status::Cleared, Status::Running, Status::Finished];

    /// `getSerializedName`.
    pub fn name(self) -> &'static str {
        match self {
            Status::Cleared => "cleared",
            Status::Running => "running",
            Status::Finished => "finished",
        }
    }

    fn by_name(name: &str) -> Option<Status> {
        Status::ALL.into_iter().find(|s| s.name() == name)
    }

    fn by_id(id: u8) -> Status {
        Status::ALL
            .get(usize::from(id))
            .copied()
            .unwrap_or_default()
    }
}

/// `Rotation.getSerializedName`.
pub fn rotation_name(rotation: Rotation) -> &'static str {
    match rotation {
        Rotation::None => "none",
        Rotation::Clockwise90 => "clockwise_90",
        Rotation::Clockwise180 => "180",
        Rotation::Counterclockwise90 => "counterclockwise_90",
    }
}

/// `Rotation.BY_ID` with `OutOfBoundsStrategy.ZERO`.
pub fn rotation_by_id(id: u8) -> Rotation {
    Rotation::ALL
        .get(usize::from(id))
        .copied()
        .unwrap_or_default()
}

/// `TestInstanceBlockEntity.Data`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Data {
    /// `test`: the id of a `minecraft:test_instance` entry.
    pub test: Option<String>,
    /// `size`: the untransformed structure size.
    pub size: [i32; 3],
    /// `rotation`: on top of the test's own.
    pub rotation: Rotation,
    /// `ignore_entities`.
    pub ignore_entities: bool,
    /// `status`.
    pub status: Status,
    /// `error_message`.
    pub error_message: Option<Component>,
}

impl Data {
    /// The data of a `test_instance_block_action` packet.
    pub fn from_packet(data: TestInstanceData) -> Data {
        Data {
            test: data.test.map(|t| t.as_str().to_owned()),
            size: data.size,
            rotation: rotation_by_id(data.rotation),
            ignore_entities: data.ignore_entities,
            status: Status::by_id(data.status),
            error_message: data.error_message,
        }
    }

    /// `withStatus`: clears the error.
    pub fn with_status(&self, status: Status) -> Data {
        Data {
            status,
            error_message: None,
            ..self.clone()
        }
    }

    /// `withError`: finished with an error.
    pub fn with_error(&self, error: Component) -> Data {
        Data {
            status: Status::Finished,
            error_message: Some(error),
            ..self.clone()
        }
    }

    fn save(&self) -> Compound {
        let mut c = Compound::new();
        if let Some(test) = &self.test {
            c.insert("test", Tag::String(test.clone()));
        }
        c.insert("size", Tag::IntArray(self.size.to_vec()));
        c.insert(
            "rotation",
            Tag::String(rotation_name(self.rotation).to_owned()),
        );
        c.insert("ignore_entities", Tag::Byte(i8::from(self.ignore_entities)));
        c.insert("status", Tag::String(self.status.name().to_owned()));
        if let Some(error) = &self.error_message {
            c.insert("error_message", error.to_nbt());
        }
        c
    }

    /// `Data.CODEC.parse`: `None` when a required field is missing or wrong.
    fn load(c: &Compound) -> Option<Data> {
        let size = match c.get("size")? {
            Tag::IntArray(v) if v.len() == 3 => [v[0], v[1], v[2]],
            Tag::List(List(v)) if v.len() == 3 => {
                let int = |t: &Tag| match t {
                    Tag::Int(i) => Some(*i),
                    _ => None,
                };
                [int(&v[0])?, int(&v[1])?, int(&v[2])?]
            }
            _ => return None,
        };
        let rotation = match c.get("rotation")? {
            Tag::String(r) => Rotation::ALL.into_iter().find(|x| rotation_name(*x) == r)?,
            _ => return None,
        };
        let ignore_entities = match c.get("ignore_entities")? {
            Tag::Byte(b) => *b != 0,
            _ => return None,
        };
        let status = match c.get("status")? {
            Tag::String(s) => Status::by_name(s)?,
            _ => return None,
        };
        let test = match c.get("test") {
            Some(Tag::String(t)) => Some(t.clone()),
            None => None,
            Some(_) => return None,
        };
        Some(Data {
            test,
            size,
            rotation,
            ignore_entities,
            status,
            error_message: c.get("error_message").map(Component::from_nbt),
        })
    }
}

/// `TestInstanceBlockEntity`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TestInstance {
    /// `data`.
    pub data: Data,
    /// `errorMarkers`: positions and texts of failed assertions.
    pub errors: Vec<(BlockPos, Component)>,
}

/// What the block needs of its test definition (`GameTestInstance`); the default when the
/// test is not in the registry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TestShape {
    /// `rotation`.
    pub rotation: Rotation,
    /// `padding`.
    pub padding: i32,
    /// `skyAccess`.
    pub sky_access: bool,
}

/// An inclusive block box (`BoundingBox`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockBox {
    /// The lowest corner.
    pub min: BlockPos,
    /// The highest corner.
    pub max: BlockPos,
}

impl BlockBox {
    /// `fromCorners`.
    pub fn from_corners(a: BlockPos, b: BlockPos) -> BlockBox {
        BlockBox {
            min: BlockPos::new(a.x.min(b.x), a.y.min(b.y), a.z.min(b.z)),
            max: BlockPos::new(a.x.max(b.x), a.y.max(b.y), a.z.max(b.z)),
        }
    }

    /// `inflatedBy(amount)`.
    pub fn inflated(self, amount: i32) -> BlockBox {
        BlockBox {
            min: BlockPos::new(
                self.min.x - amount,
                self.min.y - amount,
                self.min.z - amount,
            ),
            max: BlockPos::new(
                self.max.x + amount,
                self.max.y + amount,
                self.max.z + amount,
            ),
        }
    }

    /// `isInside(pos)`.
    pub fn contains(self, pos: BlockPos) -> bool {
        (self.min.x..=self.max.x).contains(&pos.x)
            && (self.min.y..=self.max.y).contains(&pos.y)
            && (self.min.z..=self.max.z).contains(&pos.z)
    }

    /// `AABB.of(box)`.
    pub fn aabb(self) -> Aabb {
        Aabb::new(
            f64::from(self.min.x),
            f64::from(self.min.y),
            f64::from(self.min.z),
            f64::from(self.max.x) + 1.0,
            f64::from(self.max.y) + 1.0,
            f64::from(self.max.z) + 1.0,
        )
    }

    /// `intersectingChunks`.
    pub fn chunks(self) -> impl Iterator<Item = ChunkPos> {
        let (min, max) = (self.min, self.max);
        ((min.z >> 4)..=(max.z >> 4))
            .flat_map(move |z| ((min.x >> 4)..=(max.x >> 4)).map(move |x| ChunkPos::new(x, z)))
    }

    /// `BlockPos.betweenClosed`: x fastest, then y, then z.
    pub fn positions(self) -> impl Iterator<Item = BlockPos> {
        let (min, max) = (self.min, self.max);
        (min.z..=max.z).flat_map(move |z| {
            (min.y..=max.y).flat_map(move |y| (min.x..=max.x).map(move |x| BlockPos::new(x, y, z)))
        })
    }
}

impl TestInstance {
    /// `loadAdditional`.
    pub fn load(&mut self, nbt: &Compound) {
        if let Some(Tag::Compound(c)) = nbt.get("data")
            && let Some(data) = Data::load(c)
        {
            self.data = data;
        }
        self.errors.clear();
        if let Some(Tag::List(List(list))) = nbt.get("errors") {
            for marker in list {
                let Tag::Compound(m) = marker else {
                    continue;
                };
                let (Some(Tag::IntArray(p)), Some(text)) = (m.get("pos"), m.get("text")) else {
                    continue;
                };
                if p.len() == 3 {
                    self.errors
                        .push((BlockPos::new(p[0], p[1], p[2]), Component::from_nbt(text)));
                }
            }
        }
    }

    /// `saveAdditional`.
    pub fn save(&self, nbt: &mut Compound) {
        nbt.insert("data", Tag::Compound(self.data.save()));
        if !self.errors.is_empty() {
            let list = self
                .errors
                .iter()
                .map(|(pos, text)| {
                    let mut m = Compound::new();
                    m.insert("pos", Tag::IntArray(vec![pos.x, pos.y, pos.z]));
                    m.insert("text", text.to_nbt());
                    Tag::Compound(m)
                })
                .collect();
            nbt.insert("errors", Tag::List(List(list)));
        }
    }

    /// `getUpdateTag`: `saveCustomOnly`.
    pub fn update_tag(&self) -> Option<Compound> {
        let mut nbt = Compound::new();
        self.save(&mut nbt);
        Some(nbt)
    }

    /// `getRotation`: the test's rotation turned by the block's.
    pub fn rotation(&self, shape: TestShape) -> Rotation {
        shape.rotation.rotated(self.data.rotation)
    }

    /// `getTransformedSize`.
    fn transformed_size(&self, shape: TestShape) -> [i32; 3] {
        let [x, y, z] = self.data.size;
        match self.rotation(shape) {
            Rotation::Clockwise90 | Rotation::Counterclockwise90 => [z, y, x],
            _ => [x, y, z],
        }
    }

    /// `getStructurePos`: the north west corner of the structure.
    pub fn structure_pos(&self, pos: BlockPos, shape: TestShape) -> BlockPos {
        let p = shape.padding;
        BlockPos::new(
            pos.x + p + STRUCTURE_OFFSET.x,
            pos.y + p + STRUCTURE_OFFSET.y,
            pos.z + p + STRUCTURE_OFFSET.z,
        )
    }

    /// `getStructureBoundingBox`.
    pub fn structure_box(&self, pos: BlockPos, shape: TestShape) -> BlockBox {
        let a = self.structure_pos(pos, shape);
        let [x, y, z] = self.transformed_size(shape);
        BlockBox::from_corners(a, BlockPos::new(a.x + x - 1, a.y + y - 1, a.z + z - 1))
    }

    /// `getTestBoundingBox`: with the padding around.
    pub fn test_box(&self, pos: BlockPos, shape: TestShape) -> BlockBox {
        self.structure_box(pos, shape).inflated(shape.padding)
    }

    /// `getStartCorner`: where the rotated structure's origin lies.
    pub fn start_corner(&self, pos: BlockPos, shape: TestShape) -> BlockPos {
        let [sx, _, sz] = self.data.size;
        let c = self.structure_pos(pos, shape);
        match self.rotation(shape) {
            Rotation::None => c,
            Rotation::Clockwise90 => BlockPos::new(c.x + sz - 1, c.y, c.z),
            Rotation::Clockwise180 => BlockPos::new(c.x + sx - 1, c.y, c.z + sz - 1),
            Rotation::Counterclockwise90 => BlockPos::new(c.x, c.y, c.z + sx - 1),
        }
    }

    /// `getTestName`: the test id, else `test_instance_block.invalid_test`.
    pub fn test_name(&self) -> Component {
        match &self.data.test {
            Some(id) => Component::text(id.clone()),
            None => Component::translatable("test_instance_block.invalid_test", vec![]),
        }
    }
}

/// The test instance block entity at `pos`.
pub fn get(level: &Level, pos: BlockPos) -> Option<&TestInstance> {
    match level.block_entity(pos) {
        Some(BlockEntity::TestInstance(t)) => Some(t),
        _ => None,
    }
}

fn get_mut(level: &mut Level, pos: BlockPos) -> Option<&mut TestInstance> {
    match level.block_entity_mut(pos) {
        Some(BlockEntity::TestInstance(t)) => Some(t),
        _ => None,
    }
}

/// `setChanged`: saved and sent to the clients.
fn changed(level: &mut Level, pos: BlockPos) {
    level.block_entities.touch(pos.chunk());
    level.send_block_entity_updated(pos);
}

/// `set(data)`.
pub fn set(level: &mut Level, pos: BlockPos, data: Data) {
    if let Some(t) = get_mut(level, pos) {
        t.data = data;
        changed(level, pos);
    }
}

/// `setSuccess`, `setRunning` (`withStatus`).
pub fn set_status(level: &mut Level, pos: BlockPos, status: Status) {
    if let Some(t) = get(level, pos) {
        let data = t.data.with_status(status);
        set(level, pos, data);
    }
}

/// `setErrorMessage`.
pub fn set_error_message(level: &mut Level, pos: BlockPos, error: Component) {
    if let Some(t) = get(level, pos) {
        let data = t.data.with_error(error);
        set(level, pos, data);
    }
}

/// `markError`.
pub fn mark_error(level: &mut Level, pos: BlockPos, at: BlockPos, text: Component) {
    if let Some(t) = get_mut(level, pos) {
        t.errors.push((at, text));
        changed(level, pos);
    }
}

/// `clearErrorMarkers`.
pub fn clear_error_markers(level: &mut Level, pos: BlockPos) {
    if let Some(t) = get_mut(level, pos)
        && !t.errors.is_empty()
    {
        t.errors.clear();
        changed(level, pos);
    }
}

/// `processStructureBoundary`: the walls and floor around the structure, and the ceiling
/// unless the test has sky access.
fn structure_boundary(t: &TestInstance, pos: BlockPos, shape: TestShape) -> Vec<BlockPos> {
    let b = t.structure_box(pos, shape);
    let low = BlockPos::new(b.min.x - 1, b.min.y - 1, b.min.z - 1);
    let high = BlockPos::new(b.max.x + 1, b.max.y + 1, b.max.z + 1);
    let ceiling = !shape.sky_access;
    BlockBox {
        min: low,
        max: high,
    }
    .positions()
    .filter(|p| {
        let wall = p.x == low.x || p.x == high.x || p.z == low.z || p.z == high.z || p.y == low.y;
        wall || (p.y == high.y && ceiling)
    })
    .collect()
}

/// `encaseStructure`: barriers around the structure.
pub fn encase_structure(level: &mut Level, pos: BlockPos, shape: TestShape) {
    let Some(t) = get(level, pos) else {
        return;
    };
    for p in structure_boundary(t, pos, shape) {
        if level.block(p).block() != ids::TEST_INSTANCE_BLOCK {
            level.set_block(p, ids::BARRIER.default_state(), update::ALL);
        }
    }
}

/// `removeBarriers`.
pub fn remove_barriers(level: &mut Level, pos: BlockPos, shape: TestShape) {
    let Some(t) = get(level, pos) else {
        return;
    };
    for p in structure_boundary(t, pos, shape) {
        if level.block(p).block() == ids::BARRIER {
            level.set_block(p, vanillite_data::block::BlockState::AIR, update::ALL);
        }
    }
}

/// Discards the entities other than players touching `aabb`.
fn discard_entities(level: &mut Level, aabb: &Aabb) {
    let mut found = Vec::new();
    level
        .entities
        .store
        .for_each_non_player_in(aabb, |id| found.push(id));
    for id in found {
        if level.entities.player(id).is_none() {
            level.entities.despawn(id);
        }
    }
}

/// `StructureUtils.clearSpaceForStructure`: air (stone below the floor), no ticks, block
/// events or entities other than players.
pub fn clear_space(level: &mut Level, area: BlockBox) {
    let ground = area.min.y - 1;
    for p in area.positions() {
        let state = if p.y < ground {
            ids::STONE.default_state()
        } else {
            vanillite_data::block::BlockState::AIR
        };
        level.set_block(p, state, PLACE_FLAGS);
        level.update_neighbors_at(p, state.block());
    }
    level.chunks.clear_block_ticks(area.min, area.max);
    level.clear_block_events(area.min, area.max);
    discard_entities(level, &area.aabb());
}

/// `placeStructure(level, template)`: clears the test area and places the structure at the
/// start corner (strict, the block's rotation and entity setting).
pub fn place_structure(
    level: &mut Level,
    pos: BlockPos,
    shape: TestShape,
    template: &StructureTemplate,
) {
    let Some(t) = get(level, pos) else {
        return;
    };
    let settings = PlaceSettings {
        rotation: t.rotation(shape),
        ignore_entities: t.data.ignore_entities,
        strict: true,
        ..PlaceSettings::default()
    };
    let corner = t.start_corner(pos, shape);
    let structure = t.structure_box(pos, shape);
    let test = t.test_box(pos, shape);
    // `forceLoadChunks`.
    {
        let Level { chunks, forced, .. } = &mut *level;
        for chunk in structure.chunks() {
            forced.set(chunks, chunk, true);
        }
    }
    clear_space(level, test);
    discard_entities(level, &test.aabb());
    level.place_structure(template, corner, &settings);
}

/// `resetTest`: barriers and markers go, the structure is placed again.
pub fn reset_test(
    level: &mut Level,
    pos: BlockPos,
    shape: TestShape,
    template: Option<&Arc<StructureTemplate>>,
    feedback: &mut dyn FnMut(Component),
) {
    remove_barriers(level, pos, shape);
    clear_error_markers(level, pos);
    if let Some(template) = template {
        place_structure(level, pos, shape, template);
        if let Some(t) = get(level, pos) {
            feedback(
                Component::translatable("test_instance_block.reset_success", vec![t.test_name()])
                    .color(Color::Named(NamedColor::Green)),
            );
        }
    }
    set_status(level, pos, Status::Cleared);
}

/// `saveTest`: the structure (`structure` of the test, else the test id) is saved from the
/// world without air. Returns the structure id.
pub fn save_test(
    level: &mut Level,
    pos: BlockPos,
    shape: TestShape,
    structure: Option<String>,
    feedback: &mut dyn FnMut(Component),
) -> Option<String> {
    let t = get(level, pos)?;
    let Some(id) = structure.or_else(|| t.data.test.clone()) else {
        feedback(
            Component::translatable_args(
                "test_instance_block.error.unable_to_save",
                vec![Arg::Int(pos.x), Arg::Int(pos.y), Arg::Int(pos.z)],
            )
            .color(Color::Named(NamedColor::Red)),
        );
        return None;
    };
    let origin = t.structure_pos(pos, shape);
    let [x, y, z] = t.data.size;
    let include_entities = !t.data.ignore_entities;
    let template = level.capture_structure(
        origin,
        BlockPos::new(x, y, z),
        include_entities,
        &[ids::AIR],
    );
    level.save_structure_template(&id, template);
    Some(id)
}

/// A `test_instance_block_action` of a player, handled by the server's game tests.
#[derive(Clone, Debug, PartialEq)]
pub struct Action {
    /// The player's UUID.
    pub player: u128,
    /// The test instance block.
    pub pos: BlockPos,
    /// Init, query, set, reset, save, export, run.
    pub action: u8,
    /// The block's data as the player edited it.
    pub data: Data,
}

/// The actions of `test_instance_block_action`.
pub mod action {
    /// `INIT`: the status of the test the screen shows.
    pub const INIT: u8 = 0;
    /// `QUERY`: the status and the structure size.
    pub const QUERY: u8 = 1;
    /// `SET`.
    pub const SET: u8 = 2;
    /// `RESET`.
    pub const RESET: u8 = 3;
    /// `SAVE`.
    pub const SAVE: u8 = 4;
    /// `EXPORT`.
    pub const EXPORT: u8 = 5;
    /// `RUN`.
    pub const RUN: u8 = 6;
}

/// The behaviour of the block (no ticks, no redstone).
pub(super) fn fill(_block: BlockId, _b: &mut BlockBehaviour) {}

/// `handleTestInstanceBlockAction`: operators in creative act on a test instance block; the
/// server's game tests do the rest (they know the test registry).
pub fn handle(level: &mut Level, packet: TestInstanceBlockAction, editor: &Editor<'_>) {
    let pos = BlockPos::new(packet.pos.x, packet.pos.y, packet.pos.z);
    if !editor.allowed || get(level, pos).is_none() {
        return;
    }
    level.test_instance_actions.push(Action {
        player: editor.uuid,
        pos,
        action: packet.action,
        data: Data::from_packet(packet.data),
    });
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn test_instance_nbt_round_trip() {
        let block = TestInstance {
            data: Data {
                test: Some("minecraft:always_pass".into()),
                size: [3, 4, 5],
                rotation: Rotation::Clockwise180,
                ignore_entities: true,
                status: Status::Finished,
                error_message: Some(Component::text("boom")),
            },
            errors: vec![(BlockPos::new(1, 2, 3), Component::text("here"))],
        };
        let mut nbt = Compound::new();
        block.save(&mut nbt);
        let Some(Tag::Compound(data)) = nbt.get("data") else {
            panic!("no data");
        };
        assert_eq!(data.get("size"), Some(&Tag::IntArray(vec![3, 4, 5])));
        assert_eq!(data.get("rotation"), Some(&Tag::String("180".into())));
        assert_eq!(data.get("status"), Some(&Tag::String("finished".into())));
        let mut loaded = TestInstance::default();
        loaded.load(&nbt);
        assert_eq!(loaded, block);
    }

    #[test]
    fn test_instance_geometry() {
        let mut t = TestInstance::default();
        t.data.size = [3, 2, 5];
        let pos = BlockPos::new(0, 64, 0);
        let shape = TestShape {
            padding: 1,
            ..TestShape::default()
        };
        assert_eq!(t.structure_pos(pos, shape), BlockPos::new(1, 66, 2));
        let b = t.structure_box(pos, shape);
        assert_eq!(b.max, BlockPos::new(3, 67, 6));
        t.data.rotation = Rotation::Clockwise90;
        let b = t.structure_box(pos, shape);
        assert_eq!(b.max, BlockPos::new(5, 67, 4));
        assert_eq!(t.start_corner(pos, shape), BlockPos::new(5, 66, 2));
        assert_eq!(t.test_box(pos, shape).min, BlockPos::new(0, 65, 1));
    }

    #[test]
    fn test_instance_barriers_and_clearing() {
        let mut level = crate::level::tests::flat_level();
        let pos = BlockPos::new(0, 0, 0);
        level.set_block(pos, ids::TEST_INSTANCE_BLOCK.default_state(), update::ALL);
        let mut data = Data {
            size: [2, 2, 2],
            ..Data::default()
        };
        data.test = Some("minecraft:t".into());
        set(&mut level, pos, data);
        let shape = TestShape::default();
        let inside = BlockPos::new(1, 1, 1);
        level.set_block(inside, ids::STONE.default_state(), update::ALL);
        encase_structure(&mut level, pos, shape);
        // Floor, walls and ceiling one block around the 2x2x2 structure at (0, 1, 1).
        for p in [
            BlockPos::new(-1, 0, 0),
            BlockPos::new(2, 2, 3),
            BlockPos::new(0, 3, 1),
        ] {
            assert_eq!(level.block(p).block(), ids::BARRIER, "{p:?}");
        }
        assert_eq!(level.block(pos).block(), ids::TEST_INSTANCE_BLOCK);
        assert_eq!(level.block(inside).block(), ids::STONE);
        let area = get(&level, pos)
            .map(|t| t.test_box(pos, shape))
            .expect("test instance");
        clear_space(&mut level, area);
        assert!(level.block(inside).is_air());
        remove_barriers(
            &mut level,
            pos,
            TestShape {
                sky_access: true,
                ..shape
            },
        );
        assert!(level.block(BlockPos::new(-1, 0, 0)).is_air());
        assert_eq!(level.block(BlockPos::new(0, 3, 1)).block(), ids::BARRIER);
        set_status(&mut level, pos, Status::Running);
        assert_eq!(
            get(&level, pos).map(|t| t.data.status),
            Some(Status::Running)
        );
    }
}
