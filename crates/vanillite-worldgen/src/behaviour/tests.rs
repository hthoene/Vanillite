//! Behaviour in a real region with the Vanilla data pack's tags.

use vanillite_base::{BlockPos, ChunkPos, Direction};
use vanillite_data::block::BlockState;

use super::*;
use crate::blocks::parse_block_state;
use crate::feature::region::{WorldGenRegion, flags as update};
use crate::proto::ProtoChunk;
use crate::random_state::RandomState;
use crate::test_support::packs;

fn state(text: &str) -> BlockState {
    let json = if text.starts_with('{') {
        serde_json::from_str(text).expect("state JSON")
    } else {
        serde_json::Value::String(text.to_owned())
    };
    parse_block_state(&json).expect("block state")
}

/// Runs `test` on an empty region around chunk 0, 0 with the given build height; dimensions
/// below y 0 have sky light (as the overworld), the others not.
fn with_region(min_y: i32, height: i32, test: impl FnOnce(&mut WorldGenRegion<'_>)) {
    init(packs());
    let random_state = RandomState::new(packs(), 0, false).expect("random state");
    let mut chunks: Vec<ProtoChunk> = (0..9)
        .map(|i| ProtoChunk::new(ChunkPos::new(i % 3 - 1, i / 3 - 1), min_y, height, 0))
        .collect();
    let mut region = WorldGenRegion::new(chunks.iter_mut().collect(), 1, 1, &random_state, 63);
    region.set_has_sky_light(min_y < 0);
    test(&mut region);
}

fn put(level: &mut WorldGenRegion<'_>, pos: BlockPos, text: &str) {
    level.set_block(pos, state(text), update::CLIENTS | update::KNOWN_SHAPE);
}

const ORIGIN: BlockPos = BlockPos::new(0, 64, 0);

#[test]
fn plants_need_their_support() {
    with_region(-64, 384, |level| {
        put(level, ORIGIN.below(), "grass_block");
        assert!(can_survive(level, state("short_grass"), ORIGIN));
        assert!(!can_survive(level, state("dead_bush"), ORIGIN.above()));
        put(level, ORIGIN.below(), "stone");
        assert!(!can_survive(level, state("short_grass"), ORIGIN));
        // Snow needs a full top face; ice is excluded by tag.
        assert!(can_survive(level, state("snow"), ORIGIN));
        put(level, ORIGIN.below(), "oak_slab");
        assert!(!can_survive(level, state("snow"), ORIGIN));
        put(level, ORIGIN.below(), "ice");
        assert!(!can_survive(level, state("snow"), ORIGIN));
        // Sugar cane next to water.
        put(level, ORIGIN.below(), "sand");
        assert!(!can_survive(level, state("sugar_cane"), ORIGIN));
        put(level, ORIGIN.below().relative(Direction::East), "water");
        assert!(can_survive(level, state("sugar_cane"), ORIGIN));
    });
}

#[test]
fn mushrooms_see_no_light_only_without_sky() {
    with_region(-64, 384, |level| {
        put(level, ORIGIN.below(), "stone");
        assert!(!can_survive(level, state("brown_mushroom"), ORIGIN));
        put(level, ORIGIN.below(), "mycelium");
        assert!(can_survive(level, state("brown_mushroom"), ORIGIN));
    });
    with_region(0, 256, |level| {
        put(level, ORIGIN.below(), "netherrack");
        assert!(can_survive(level, state("red_mushroom"), ORIGIN));
    });
}

#[test]
fn attached_blocks_break() {
    with_region(-64, 384, |level| {
        put(level, ORIGIN.relative(Direction::North), "stone");
        let vine = state(r#"{"Name":"vine","Properties":{"north":"true"}}"#);
        assert!(can_survive(level, vine, ORIGIN));
        let torch = state(r#"{"Name":"wall_torch","Properties":{"facing":"south"}}"#);
        assert!(can_survive(level, torch, ORIGIN));
        let upper = state(r#"{"Name":"tall_grass","Properties":{"half":"upper"}}"#);
        put(level, ORIGIN, "air");
        let below = level.block(ORIGIN.below());
        let updated = update_shape(level, upper, ORIGIN, Direction::Down, ORIGIN.below(), below);
        assert!(updated.is_air());
    });
}

#[test]
fn connections_follow_neighbours() {
    with_region(-64, 384, |level| {
        put(level, ORIGIN.relative(Direction::East), "stone");
        put(level, ORIGIN.relative(Direction::North), "oak_fence");
        let fence = update_from_neighbour_shapes(level, state("oak_fence"), ORIGIN);
        assert_eq!(fence.value("east"), Some("true"));
        assert_eq!(fence.value("north"), Some("true"));
        assert_eq!(fence.value("south"), Some("false"));
        assert_eq!(fence.value("west"), Some("false"));

        let wall_pos = ORIGIN.offset(5, 0, 0);
        put(
            level,
            wall_pos.relative(Direction::North),
            "cobblestone_wall",
        );
        put(
            level,
            wall_pos.relative(Direction::South),
            "cobblestone_wall",
        );
        let wall = update_from_neighbour_shapes(level, state("cobblestone_wall"), wall_pos);
        assert_eq!(wall.value("north"), Some("low"));
        assert_eq!(wall.value("south"), Some("low"));
        assert_eq!(wall.value("east"), Some("none"));
        assert_eq!(wall.value("up"), Some("false"));
        put(level, wall_pos.above(), "stone");
        let wall = update_from_neighbour_shapes(level, state("cobblestone_wall"), wall_pos);
        assert_eq!(wall.value("north"), Some("tall"));
        assert_eq!(wall.value("up"), Some("false"));

        // An outer corner: the stair behind faces west.
        let stairs_pos = ORIGIN.offset(-5, 0, 0);
        put(
            level,
            stairs_pos.relative(Direction::North),
            r#"{"Name":"oak_stairs","Properties":{"facing":"west"}}"#,
        );
        let stairs = update_from_neighbour_shapes(
            level,
            state(r#"{"Name":"oak_stairs","Properties":{"facing":"north"}}"#),
            stairs_pos,
        );
        assert_eq!(stairs.value("shape"), Some("outer_left"));
    });
}

#[test]
fn redstone_wire_connects() {
    with_region(-64, 384, |level| {
        for dx in -1..=1 {
            put(level, ORIGIN.offset(dx, -1, 0), "stone");
        }
        put(level, ORIGIN.relative(Direction::East), "redstone_wire");
        let wire = update_from_neighbour_shapes(level, state("redstone_wire"), ORIGIN);
        assert_eq!(wire.value("east"), Some("side"));
        assert_eq!(wire.value("west"), Some("side"));
        assert_eq!(wire.value("north"), Some("none"));
    });
}

#[test]
fn waterlogged_blocks_schedule_water() {
    with_region(-64, 384, |level| {
        let slab = state(r#"{"Name":"oak_slab","Properties":{"waterlogged":"true"}}"#);
        update_from_neighbour_shapes(level, slab, ORIGIN);
        // `Fluids.WATER`.
        assert!(level.chunk(ORIGIN).fluid_ticks().has(2, ORIGIN));
        put(level, ORIGIN.below(), "stone");
        let sand = state("sand");
        update_shape(
            level,
            sand,
            ORIGIN,
            Direction::Up,
            ORIGIN.above(),
            BlockState::AIR,
        );
        assert!(level.chunk(ORIGIN).block_ticks().has(sand.block(), ORIGIN));
    });
}

#[test]
fn post_processing_marks() {
    with_region(-64, 384, |level| {
        assert_eq!(
            post_process_pos(level, state("red_mushroom"), ORIGIN),
            Some(ORIGIN)
        );
        assert_eq!(
            post_process_pos(level, state("magma_block"), ORIGIN),
            Some(ORIGIN.above())
        );
        assert_eq!(post_process_pos(level, state("stone"), ORIGIN), None);
    });
}
