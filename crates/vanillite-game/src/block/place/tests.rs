//! Placement tests over a `HashMap` level.

use std::collections::HashMap;

use vanillite_base::{BlockPos, Direction, Vec3};
use vanillite_data::block::{BlockId, BlockState};
use vanillite_data::item::ItemId;

use super::util::StateExt;
use super::{BlockReader, PlaceContext};

/// A level of explicitly set blocks; everything else is air. Build height -64..320.
#[derive(Debug, Default)]
pub(super) struct TestLevel {
    pub(super) blocks: HashMap<BlockPos, BlockState>,
}

impl TestLevel {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn set(&mut self, pos: BlockPos, state: BlockState) -> &mut Self {
        self.blocks.insert(pos, state);
        self
    }

    /// Sets the default state of the block named `name` (without namespace).
    pub(super) fn set_block(&mut self, pos: BlockPos, name: &str) -> &mut Self {
        self.set(pos, block(name).default_state())
    }
}

impl BlockReader for TestLevel {
    fn state(&self, pos: BlockPos) -> BlockState {
        self.blocks.get(&pos).copied().unwrap_or(BlockState::AIR)
    }

    fn min_y(&self) -> i32 {
        -64
    }

    fn max_y(&self) -> i32 {
        320
    }
}

/// The block named `name` (without namespace).
pub(super) fn block(name: &str) -> BlockId {
    BlockId::by_name(&format!("minecraft:{name}")).unwrap_or_else(|| panic!("no block {name}"))
}

/// The item named `name` (without namespace).
pub(super) fn item(name: &str) -> ItemId {
    ItemId::by_name(&format!("minecraft:{name}")).unwrap_or_else(|| panic!("no item {name}"))
}

/// A context for using `item` on the `face` of `clicked` at the face's centre, offset on the face
/// by `(du, dv)` is not needed by most tests: `hit` is given absolute.
#[allow(clippy::too_many_arguments)]
pub(super) fn ctx_at(
    level: &TestLevel,
    item_name: &str,
    clicked: BlockPos,
    face: Direction,
    hit: Vec3,
    y_rot: f32,
    x_rot: f32,
    sneaking: bool,
) -> PlaceContext {
    PlaceContext::new(
        clicked,
        face,
        hit,
        false,
        y_rot,
        x_rot,
        sneaking,
        item(item_name),
        level,
    )
}

/// A context for clicking the centre of the `face` of `clicked`.
pub(super) fn ctx(
    level: &TestLevel,
    item_name: &str,
    clicked: BlockPos,
    face: Direction,
    y_rot: f32,
    x_rot: f32,
) -> PlaceContext {
    let n = face.normal();
    let hit = Vec3::new(
        f64::from(clicked.x) + 0.5 + f64::from(n[0]) * 0.5,
        f64::from(clicked.y) + 0.5 + f64::from(n[1]) * 0.5,
        f64::from(clicked.z) + 0.5 + f64::from(n[2]) * 0.5,
    );
    ctx_at(level, item_name, clicked, face, hit, y_rot, x_rot, false)
}

/// `name[prop=value,…]` of a state, for readable assertions.
pub(super) fn describe(state: BlockState) -> String {
    let props: Vec<String> = state
        .properties()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    format!(
        "{}[{}]",
        state.block().name().trim_start_matches("minecraft:"),
        props.join(",")
    )
}

/// Asserts that `state` is `name` and has the given property values.
#[track_caller]
pub(super) fn assert_state(state: Option<BlockState>, name: &str, props: &[(&str, &str)]) {
    let state = state.unwrap_or_else(|| panic!("expected {name}, got None"));
    assert_eq!(state.block(), block(name), "block of {}", describe(state));
    for (k, v) in props {
        assert_eq!(state.get(k), Some(*v), "{k} of {}", describe(state));
    }
}

#[test]
fn stone_goes_next_to_the_clicked_face() {
    let mut level = TestLevel::new();
    let ground = BlockPos::new(0, 64, 0);
    level.set_block(ground, "stone");
    let c = ctx(&level, "stone", ground, Direction::Up, 0.0, 45.0);
    assert_eq!(c.pos, BlockPos::new(0, 65, 0));
    assert!(!c.replace_clicked);
    assert_state(super::state_for_placement(&c, &level), "stone", &[]);
    // Short grass is replaceable: the clicked block itself is replaced.
    level.set_block(BlockPos::new(0, 65, 0), "short_grass");
    let c = ctx(
        &level,
        "stone",
        BlockPos::new(0, 65, 0),
        Direction::Up,
        0.0,
        45.0,
    );
    assert!(c.replace_clicked);
    assert_eq!(c.pos, BlockPos::new(0, 65, 0));
    let _ = ctx_at(
        &level,
        "stone",
        ground,
        Direction::Up,
        Vec3::ZERO,
        0.0,
        0.0,
        true,
    );
}

#[test]
fn wall_variants_follow_the_items() {
    for (standing, wall) in [
        ("torch", "wall_torch"),
        ("soul_torch", "soul_wall_torch"),
        ("redstone_torch", "redstone_wall_torch"),
        ("oak_sign", "oak_wall_sign"),
        ("dark_oak_hanging_sign", "dark_oak_wall_hanging_sign"),
        ("white_banner", "white_wall_banner"),
        ("skeleton_skull", "skeleton_wall_skull"),
        ("wither_skeleton_skull", "wither_skeleton_wall_skull"),
        ("player_head", "player_wall_head"),
        ("piglin_head", "piglin_wall_head"),
        ("dead_tube_coral_fan", "dead_tube_coral_wall_fan"),
    ] {
        assert_eq!(
            super::wall_variant(block(standing)),
            Some(block(wall)),
            "{standing}"
        );
    }
    assert_eq!(super::wall_variant(block("stone")), None);
}
