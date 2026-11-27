//! Exploit tests of the block handlers: fast break, reach and hit position checks.

#![allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use vanillite_base::{BlockPos, Vec3};
use vanillite_config::GameMode;
use vanillite_data::block::{BlockState, ids as blocks};
use vanillite_protocol::SharedPacket;
use vanillite_protocol::packets::play::serverbound::{DigAction, Hand, UseItemOn};
use vanillite_protocol::types::BlockPos as WirePos;

use super::{Actor, Digging, GameData, PlayContext};
use crate::block::place::util::StateExt;
use crate::inventory::PlayerInventory;
use crate::item::ItemStack;
use crate::player::sink::PacketSink;
use crate::player::world::{PlayLevel, SoundSource};

/// A stone floor at y = -1 and air above.
#[derive(Default)]
struct TestLevel {
    changed: HashMap<BlockPos, BlockState>,
    sounds: Vec<u16>,
}

impl PlayLevel for TestLevel {
    fn block(&self, pos: BlockPos) -> BlockState {
        self.changed.get(&pos).copied().unwrap_or(if pos.y == -1 {
            blocks::STONE.default_state()
        } else {
            BlockState::AIR
        })
    }
    fn is_loaded(&self, _: BlockPos) -> bool {
        true
    }
    fn min_y(&self) -> i32 {
        -64
    }
    fn max_y(&self) -> i32 {
        319
    }
    fn set_block(&mut self, pos: BlockPos, state: BlockState, _: u32) -> bool {
        self.changed.insert(pos, state);
        true
    }
    fn level_event(&mut self, _: Option<i32>, _: i32, _: BlockPos, _: i32) {}
    fn destroy_progress(&mut self, _: i32, _: BlockPos, _: i32) {}
    fn play_sound(&mut self, _: Option<i32>, _: Vec3, sound: u16, _: SoundSource, _: f32, _: f32) {
        self.sounds.push(sound);
    }
    fn spawn_item(&mut self, _: Vec3, _: ItemStack, _: Vec3, _: i16, _: Option<u128>) {}
    fn is_unobstructed(&self, _: BlockState, _: BlockPos) -> bool {
        true
    }
    fn random_float(&mut self) -> f32 {
        0.5
    }
    fn random_double(&mut self) -> f64 {
        0.5
    }
    fn random_boolean(&mut self) -> bool {
        false
    }
    fn random_int(&mut self, _: i32) -> i32 {
        0
    }

    fn lights_portal(&self, _: BlockPos, _: vanillite_base::Direction) -> bool {
        false
    }

    fn end_portal_frame_filled(&mut self, _: BlockPos) {}
}

#[derive(Default)]
struct Sink(Mutex<Vec<Vec<u8>>>);

impl PacketSink for Sink {
    fn send_body(&self, body: Vec<u8>) -> bool {
        self.0.lock().unwrap().push(body);
        true
    }
    fn send_shared(&self, _: &Arc<SharedPacket>) -> bool {
        true
    }
    fn close(&self) {}
    fn is_closed(&self) -> bool {
        false
    }
    fn latency_ms(&self) -> u32 {
        0
    }
}

fn actor(game_mode: GameMode) -> Actor {
    Actor {
        entity: None,
        network_id: 1,
        uuid: 1,
        pos: Vec3::new(0.5, 0.0, 0.5),
        eye_height: f64::from(1.62f32),
        y_rot: 0.0,
        x_rot: 90.0,
        on_ground: true,
        sneaking: false,
        game_mode,
        permission_level: 0,
        eyes_in_water: false,
        dig_speed: None,
        mining_fatigue: None,
    }
}

/// Runs `f` with a play context over `level` for a player in `game_mode` holding `held`.
fn with_ctx(
    level: &mut TestLevel,
    game_mode: GameMode,
    held: ItemStack,
    f: impl FnOnce(&mut PlayContext<'_>),
) -> PlayerInventory {
    let mut inventory = PlayerInventory::default();
    inventory.items[0] = held;
    let data = GameData::default();
    let mut drops = Vec::new();
    let mut swings = Vec::new();
    let mut ctx = PlayContext {
        hooks: &crate::hooks::NoHooks,
        xp: None,
        food: None,
        recipe_book: None,
        level,
        inventory: &mut inventory,
        actor: actor(game_mode),
        data: &data,
        spawn_protected: &|_| false,
        drops: &mut drops,
        swings: &mut swings,
        stats: None,
        advancements: None,
    };
    f(&mut ctx);
    inventory
}

const FLOOR: BlockPos = BlockPos::new(1, -1, 0);

/// Fast break (instant-mine hacks): a survival player that stops digging stone by hand at once
/// does not break it; the block only goes after the real breaking time.
#[test]
fn survival_stone_does_not_break_instantly() {
    let mut level = TestLevel::default();
    let sink = Sink::default();
    let mut digging = Digging::new();
    with_ctx(&mut level, GameMode::Survival, ItemStack::EMPTY, |ctx| {
        digging.handle_action(ctx, DigAction::StartDestroyBlock, FLOOR, 1, 1, &sink);
        digging.handle_action(ctx, DigAction::StopDestroyBlock, FLOOR, 1, 2, &sink);
    });
    assert!(level.block(FLOOR).is(blocks::STONE));
    // Stone by hand: 1.5 hardness, wrong tool → 1 / 1.5 / 100 per tick, 150 ticks.
    for _ in 0..140 {
        with_ctx(&mut level, GameMode::Survival, ItemStack::EMPTY, |ctx| {
            digging.tick(ctx, &sink);
        });
    }
    assert!(level.block(FLOOR).is(blocks::STONE), "broken too early");
    for _ in 0..20 {
        with_ctx(&mut level, GameMode::Survival, ItemStack::EMPTY, |ctx| {
            digging.tick(ctx, &sink);
        });
    }
    assert!(
        level.block(FLOOR).is_air(),
        "delayed destroy did not finish"
    );
}

/// Reach hacks: blocks beyond the interaction range can be neither broken nor used.
#[test]
fn out_of_reach_blocks_are_untouched() {
    let far = BlockPos::new(9, -1, 0);
    let mut level = TestLevel::default();
    let sink = Sink::default();
    let mut digging = Digging::new();
    with_ctx(&mut level, GameMode::Creative, ItemStack::EMPTY, |ctx| {
        digging.handle_action(ctx, DigAction::StartDestroyBlock, far, 1, 1, &sink);
    });
    assert!(level.block(far).is(blocks::STONE));
    let stone = ItemStack::new(vanillite_data::item::ids::STONE.0, 1);
    with_ctx(&mut level, GameMode::Creative, stone, |ctx| {
        digging.use_item_on(ctx, &use_on(far, 0.5), &sink);
    });
    assert!(level.block(BlockPos::new(9, 0, 0)).is_air());
    // In range it works.
    let mut digging = Digging::new();
    with_ctx(&mut level, GameMode::Creative, ItemStack::EMPTY, |ctx| {
        digging.handle_action(ctx, DigAction::StartDestroyBlock, FLOOR, 1, 1, &sink);
    });
    assert!(level.block(FLOOR).is_air());
}

fn use_on(pos: BlockPos, cursor_y: f32) -> UseItemOn {
    UseItemOn {
        hand: Hand::MainHand,
        pos: WirePos {
            x: pos.x,
            y: pos.y,
            z: pos.z,
        },
        face: 1,
        cursor_x: 0.5,
        cursor_y,
        cursor_z: 0.5,
        inside: false,
        world_border_hit: false,
        sequence: 3,
    }
}

/// Placing through walls with a forged hit location: a hit far outside the clicked block (or
/// a NaN one) is rejected; a real hit places the block and survival players lose the item.
#[test]
fn forged_hit_locations_are_rejected() {
    let stone = ItemStack::new(vanillite_data::item::ids::STONE.0, 2);
    let above = BlockPos::new(1, 0, 0);
    for cursor_y in [3.0, f32::NAN, f32::INFINITY] {
        let mut level = TestLevel::default();
        let sink = Sink::default();
        let mut digging = Digging::new();
        with_ctx(&mut level, GameMode::Survival, stone.clone(), |ctx| {
            digging.use_item_on(ctx, &use_on(FLOOR, cursor_y), &sink);
        });
        assert!(level.block(above).is_air(), "{cursor_y}");
    }
    let mut level = TestLevel::default();
    let sink = Sink::default();
    let mut digging = Digging::new();
    let inventory = with_ctx(&mut level, GameMode::Survival, stone, |ctx| {
        digging.use_item_on(ctx, &use_on(FLOOR, 1.0), &sink);
    });
    assert!(level.block(above).is(blocks::STONE));
    assert_eq!(inventory.items[0].count, 1);
}

/// Creative stacks above the item's stack size are refused (`handleSetCreativeModeSlot`).
#[test]
fn oversized_creative_stacks_are_refused() {
    let mut level = TestLevel::default();
    let mut digging = Digging::new();
    let inventory = with_ctx(&mut level, GameMode::Creative, ItemStack::EMPTY, |ctx| {
        // Stone stacks to 64; a diamond sword to 1.
        digging.set_creative_slot(
            ctx,
            36,
            ItemStack::new(vanillite_data::item::ids::STONE.0, 65),
        );
        digging.set_creative_slot(
            ctx,
            37,
            ItemStack::new(vanillite_data::item::ids::DIAMOND_SWORD.0, 2),
        );
        digging.set_creative_slot(
            ctx,
            38,
            ItemStack::new(vanillite_data::item::ids::STONE.0, 64),
        );
    });
    assert!(inventory.items[0].is_empty());
    assert!(inventory.items[1].is_empty());
    assert_eq!(inventory.items[2].count, 64);
}

/// Creative slots are refused in survival (creative inventory packets from hacked clients).
#[test]
fn creative_slots_need_creative_mode() {
    let mut level = TestLevel::default();
    let mut digging = Digging::new();
    let inventory = with_ctx(&mut level, GameMode::Survival, ItemStack::EMPTY, |ctx| {
        digging.set_creative_slot(
            ctx,
            36,
            ItemStack::new(vanillite_data::item::ids::STONE.0, 64),
        );
    });
    assert!(inventory.items[0].is_empty());
}

/// `SolidBucketItem`: placing powder snow leaves an empty bucket (the full one in creative)
/// and plays `item.bucket.empty_powder_snow`, not the block's place sound (#119).
#[test]
fn powder_snow_bucket_leaves_an_empty_bucket() {
    let bucket = ItemStack::new(vanillite_data::item::ids::POWDER_SNOW_BUCKET.0, 1);
    let above = BlockPos::new(1, 0, 0);
    for (mode, left) in [
        (GameMode::Survival, vanillite_data::item::ids::BUCKET),
        (
            GameMode::Creative,
            vanillite_data::item::ids::POWDER_SNOW_BUCKET,
        ),
    ] {
        let mut level = TestLevel::default();
        let sink = Sink::default();
        let mut digging = Digging::new();
        let inventory = with_ctx(&mut level, mode, bucket.clone(), |ctx| {
            digging.use_item_on(ctx, &use_on(FLOOR, 1.0), &sink);
        });
        assert!(level.block(above).is(blocks::POWDER_SNOW), "{mode:?}");
        assert_eq!(inventory.items[0].item, left.0, "{mode:?}");
        assert_eq!(inventory.items[0].count, 1, "{mode:?}");
        assert_eq!(
            level.sounds,
            vec![crate::block::sound_id("item.bucket.empty_powder_snow")],
            "{mode:?}"
        );
    }
}

/// `GameMasterBlockItem`: command, structure, jigsaw and test blocks are only placed by
/// creative players with permission level 2 (#119).
#[test]
fn game_master_blocks_need_creative_and_op() {
    use vanillite_data::item::ids as items;
    let above = BlockPos::new(1, 0, 0);
    for item in [
        items::COMMAND_BLOCK,
        items::CHAIN_COMMAND_BLOCK,
        items::REPEATING_COMMAND_BLOCK,
        items::STRUCTURE_BLOCK,
        items::JIGSAW,
        items::TEST_BLOCK,
        items::TEST_INSTANCE_BLOCK,
    ] {
        for (mode, permission, placed) in [
            (GameMode::Survival, 4, false),
            (GameMode::Creative, 1, false),
            (GameMode::Creative, 2, true),
        ] {
            let mut level = TestLevel::default();
            let sink = Sink::default();
            let mut digging = Digging::new();
            let inventory = with_ctx(&mut level, mode, ItemStack::new(item.0, 1), |ctx| {
                ctx.actor.permission_level = permission;
                digging.use_item_on(ctx, &use_on(FLOOR, 1.0), &sink);
            });
            let what = format!("{} {mode:?} {permission}", item.name());
            assert_eq!(!level.block(above).is_air(), placed, "{what}");
            assert_eq!(inventory.items[0].count, 1, "{what}");
        }
    }
}
