//! Dispenser behaviour tests on the flat test world (grass at Y -61).

use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::ids;
use vanillite_data::item::ids as items;
use vanillite_worldgen::behaviour::{PosExt, StateExt};

use super::{Behaviour, behaviour_of, tick};
use crate::block::container;
use crate::item::{self, ItemStack};
use crate::level::tests::flat_level;
use crate::level::{Level, update};

const DISPENSER: BlockPos = BlockPos::new(0, -60, 0);
/// The block in front (the dispenser faces east).
const TARGET: BlockPos = BlockPos::new(1, -60, 0);

fn stack(item: vanillite_data::item::ItemId, count: i32) -> ItemStack {
    ItemStack::new(item.0, count)
}

/// A flat level with an east-facing dispenser holding `contents` from slot 0 on.
fn level_with(contents: &[ItemStack]) -> Level {
    let mut level = flat_level();
    let state = ids::DISPENSER
        .default_state()
        .set_direction("facing", Direction::East);
    assert!(level.set_block(DISPENSER, state, update::ALL));
    let items = container::items_mut(&mut level, DISPENSER).expect("dispenser block entity");
    for (slot, stack) in items.iter_mut().zip(contents) {
        *slot = stack.clone();
    }
    level
}

/// Runs one dispense.
fn fire(level: &mut Level) {
    let state = level.block(DISPENSER);
    tick(level, state, DISPENSER);
}

fn contents(level: &Level) -> Vec<ItemStack> {
    container::items(level, DISPENSER)
        .expect("dispenser block entity")
        .iter()
        .filter(|s| !s.is_empty())
        .cloned()
        .collect()
}

fn events(level: &mut Level) -> Vec<i32> {
    level
        .output()
        .level_events
        .iter()
        .map(|e| e.event)
        .collect()
}

fn items_on_ground(level: &Level) -> usize {
    level.entities.items.iter().count()
}

#[test]
fn registry_matches_bootstrap() {
    assert_eq!(behaviour_of(&stack(items::ARROW, 1)), Behaviour::Projectile);
    assert_eq!(
        behaviour_of(&stack(items::FIRE_CHARGE, 1)),
        Behaviour::Projectile
    );
    assert_eq!(behaviour_of(&stack(items::OAK_BOAT, 1)), Behaviour::Boat);
    assert_eq!(
        behaviour_of(&stack(items::HOPPER_MINECART, 1)),
        Behaviour::Minecart
    );
    assert_eq!(
        behaviour_of(&stack(items::COD_BUCKET, 1)),
        Behaviour::FilledBucket
    );
    assert_eq!(
        behaviour_of(&stack(items::PIG_SPAWN_EGG, 1)),
        Behaviour::SpawnEgg
    );
    assert_eq!(
        behaviour_of(&stack(items::WATER_BUCKET, 1)),
        Behaviour::FilledBucket
    );
    assert_eq!(
        behaviour_of(&stack(items::RED_SHULKER_BOX, 1)),
        Behaviour::ShulkerBox
    );
    assert_eq!(
        behaviour_of(&stack(items::IRON_HELMET, 1)),
        Behaviour::Equipment
    );
    assert_eq!(behaviour_of(&stack(items::SADDLE, 1)), Behaviour::Equipment);
    assert_eq!(behaviour_of(&stack(items::STONE, 1)), Behaviour::Default);
}

#[test]
fn fallback_items_drop() {
    let mut level = level_with(&[stack(items::OAK_BOAT, 2)]);
    fire(&mut level);
    assert_eq!(contents(&level), vec![stack(items::OAK_BOAT, 1)]);
    assert_eq!(items_on_ground(&level), 1);
    // `BoatDispenseItemBehavior` without water: the nested default dispense plays its sound
    // and smoke, then the boat behaviour its own.
    assert_eq!(events(&mut level), vec![1000, 2000, 1000, 2000]);
}

#[test]
fn water_bucket_places_water_and_keeps_the_bucket() {
    let mut level = level_with(&[stack(items::WATER_BUCKET, 1)]);
    fire(&mut level);
    assert!(level.block(TARGET).is(ids::WATER));
    assert_eq!(contents(&level), vec![stack(items::BUCKET, 1)]);
}

#[test]
fn empty_bucket_picks_up_a_source_into_a_free_slot() {
    let mut level = level_with(&[stack(items::BUCKET, 2)]);
    level.set_block(TARGET, ids::WATER.default_state(), update::ALL);
    fire(&mut level);
    assert!(level.block(TARGET).is_air());
    assert_eq!(
        contents(&level),
        vec![stack(items::BUCKET, 1), stack(items::WATER_BUCKET, 1)]
    );

    // Nothing to pick up: the bucket drops.
    let mut level = level_with(&[stack(items::BUCKET, 1)]);
    fire(&mut level);
    assert_eq!(items_on_ground(&level), 1);
    assert!(contents(&level).is_empty());
}

#[test]
fn tnt_is_primed_in_front() {
    let mut level = level_with(&[stack(items::TNT, 1)]);
    fire(&mut level);
    assert!(contents(&level).is_empty());
    assert_eq!(level.entities.primed_tnts.iter().count(), 1);

    let mut level = level_with(&[stack(items::TNT, 1)]);
    level.settings.tnt_explodes = false;
    fire(&mut level);
    assert_eq!(contents(&level), vec![stack(items::TNT, 1)]);
    assert_eq!(events(&mut level), vec![1001, 2000]);
}

#[test]
fn bone_meal_grows_crops_and_fails_on_stone() {
    let mut level = level_with(&[stack(items::BONE_MEAL, 2)]);
    level.set_block(TARGET.below(), ids::FARMLAND.default_state(), update::ALL);
    level.set_block(TARGET, ids::WHEAT.default_state(), update::ALL);
    fire(&mut level);
    let age = level.block(TARGET).int("age").unwrap_or(0);
    assert!((2..=5).contains(&age), "age {age}");
    assert_eq!(contents(&level), vec![stack(items::BONE_MEAL, 1)]);
    assert!(events(&mut level).contains(&1505));

    level.set_block(TARGET, ids::STONE.default_state(), update::ALL);
    level.output().level_events.clear();
    fire(&mut level);
    assert_eq!(contents(&level), vec![stack(items::BONE_MEAL, 1)]);
    assert_eq!(events(&mut level), vec![1001, 2000]);
}

#[test]
fn bone_meal_turns_grass_into_tall_grass() {
    let mut level = level_with(&[stack(items::BONE_MEAL, 1)]);
    level.set_block(TARGET, ids::SHORT_GRASS.default_state(), update::ALL);
    fire(&mut level);
    assert!(level.block(TARGET).is(ids::TALL_GRASS));
    assert!(level.block(TARGET.above()).is(ids::TALL_GRASS));
    assert_eq!(level.block(TARGET.above()).value("half"), Some("upper"));
}

#[test]
fn flint_and_steel_lights_fire_and_wears_out() {
    let mut level = level_with(&[stack(items::FLINT_AND_STEEL, 1)]);
    fire(&mut level);
    assert!(level.block(TARGET).is(ids::FIRE));
    let left = contents(&level);
    assert_eq!(item::damage(&left[0]), 1);
}

#[test]
fn shears_harvest_a_full_beehive() {
    let mut level = level_with(&[stack(items::SHEARS, 1)]);
    let hive = ids::BEEHIVE.default_state().set_int("honey_level", 5);
    level.set_block(TARGET, hive, update::ALL);
    fire(&mut level);
    assert_eq!(level.block(TARGET).int("honey_level"), Some(0));
    assert_eq!(items_on_ground(&level), 1);
    assert_eq!(item::damage(&contents(&level)[0]), 1);
    // An empty hive: the shears fail and keep their durability.
    level.output().level_events.clear();
    fire(&mut level);
    assert_eq!(item::damage(&contents(&level)[0]), 1);
    assert_eq!(events(&mut level), vec![1001, 2000]);
}

#[test]
fn honeycomb_waxes_copper_and_glowstone_charges_anchors() {
    let mut level = level_with(&[stack(items::HONEYCOMB, 1)]);
    let stairs = ids::EXPOSED_CUT_COPPER_STAIRS
        .default_state()
        .set_direction("facing", Direction::South);
    level.set_block(TARGET, stairs, update::ALL);
    fire(&mut level);
    assert!(contents(&level).is_empty());
    let waxed = level.block(TARGET);
    assert!(waxed.is(ids::WAXED_EXPOSED_CUT_COPPER_STAIRS));
    assert_eq!(waxed.direction("facing"), Some(Direction::South));

    let mut level = level_with(&[stack(items::GLOWSTONE, 1)]);
    level.set_block(TARGET, ids::RESPAWN_ANCHOR.default_state(), update::ALL);
    fire(&mut level);
    assert_eq!(level.block(TARGET).int("charges"), Some(1));
    assert!(contents(&level).is_empty());
}

#[test]
fn glass_bottle_fills_with_water() {
    let mut level = level_with(&[stack(items::GLASS_BOTTLE, 2)]);
    level.set_block(TARGET, ids::WATER.default_state(), update::ALL);
    fire(&mut level);
    let left = contents(&level);
    assert_eq!(left.len(), 2);
    assert_eq!(left[0], stack(items::GLASS_BOTTLE, 1));
    assert_eq!(item::item_id(&left[1]), items::POTION);
    assert_eq!(
        crate::recipe::potion_of(&left[1]),
        vanillite_data::registry::POTION.id("minecraft:water")
    );
    // The water stays.
    assert!(level.block(TARGET).is(ids::WATER));
}

#[test]
fn shulker_box_is_placed_with_its_name() {
    let mut boxed = stack(items::BLUE_SHULKER_BOX, 1);
    let name = vanillite_protocol::text::Component::text("Loot");
    let mut bytes = Vec::new();
    vanillite_protocol::codec::Encode::encode(&name, &mut bytes);
    boxed.patch_mut().set(
        vanillite_protocol::components::ids::CUSTOM_NAME,
        bytes.into(),
    );
    let mut level = level_with(&[boxed]);
    fire(&mut level);
    let placed = level.block(TARGET);
    assert!(placed.is(ids::BLUE_SHULKER_BOX));
    // The block below the target is not empty: the box is placed on the up face.
    assert_eq!(placed.direction("facing"), Some(Direction::Up));
    assert!(contents(&level).is_empty());
    match level.block_entity(TARGET) {
        Some(crate::level::BlockEntity::Container(c)) => assert!(c.custom_name.is_some()),
        other => panic!("no shulker box block entity: {other:?}"),
    }
    // Occupied target: the next box fails and stays.
    container::items_mut(&mut level, DISPENSER).unwrap()[0] = stack(items::BLUE_SHULKER_BOX, 1);
    level.output().level_events.clear();
    fire(&mut level);
    assert_eq!(contents(&level).len(), 1);
    assert_eq!(events(&mut level), vec![1001, 2000]);
}

#[test]
fn water_potion_makes_mud() {
    let mut potion = stack(items::POTION, 1);
    let water = vanillite_data::registry::POTION
        .id("minecraft:water")
        .unwrap();
    potion.patch_mut().set(
        vanillite_protocol::components::ids::POTION_CONTENTS,
        crate::recipe::potion_contents(water),
    );
    let mut level = level_with(&[potion]);
    level.set_block(TARGET, ids::DIRT.default_state(), update::ALL);
    fire(&mut level);
    assert!(level.block(TARGET).is(ids::MUD));
    assert_eq!(contents(&level), vec![stack(items::GLASS_BOTTLE, 1)]);
}

#[test]
fn copper_chests_keep_their_contents_when_waxed() {
    let mut level = flat_level();
    let pos = BlockPos::new(3, -60, 3);
    level.set_block(pos, ids::COPPER_CHEST.default_state(), update::ALL);
    container::items_mut(&mut level, pos).unwrap()[0] = stack(items::DIAMOND, 3);
    let waxed = super::waxed(level.block(pos)).unwrap();
    level.set_block_and_update(pos, waxed);
    assert!(level.block(pos).is(ids::WAXED_COPPER_CHEST));
    assert_eq!(
        container::items(&level, pos).unwrap()[0],
        stack(items::DIAMOND, 3)
    );
    assert_eq!(items_on_ground(&level), 0);
}
