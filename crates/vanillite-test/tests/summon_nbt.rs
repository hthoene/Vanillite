//! Entity NBT through `/summon`, `/data merge` and a save and load: the common `Entity` keys,
//! `attributes`, numbers of any tag type, `DeathLootTable`, `Team` and the enderman's block.
//! Each key is checked by what it does (flags and synched data, damage, health, drops), not
//! only by what is saved.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use vanillite_data::entity::{EntityTypeId, ids};
use vanillite_game::effect::living::mob_pairing_attributes;
use vanillite_game::player::encode::all_entity_data;
use vanillite_nbt::{Compound, Tag};
use vanillite_test::game_world::GameWorld;
use vanillite_test::scenario::Scenario;
use vanillite_test::world::ScenarioWorld;
use vanillite_world::entity::{EntityFlags, EntityId};

fn world() -> GameWorld {
    let scenario = Scenario::parse(
        "summon_nbt",
        r#"{"region": [[0, -61, 0], [15, -55, 15]], "ticks": 1}"#,
    )
    .unwrap();
    GameWorld::new(&scenario.world).unwrap()
}

fn run(world: &mut GameWorld, line: &str) {
    world
        .command(line)
        .unwrap_or_else(|e| panic!("{line}: {e}"));
}

fn all_of(world: &mut GameWorld, kind: EntityTypeId) -> Vec<EntityId> {
    let store = &world.level().entities.store;
    store
        .columns()
        .id
        .iter()
        .copied()
        .filter(|&id| store.get(id).is_some_and(|e| e.kind() == kind))
        .collect()
}

fn one(world: &mut GameWorld, kind: EntityTypeId) -> EntityId {
    let found = all_of(world, kind);
    assert_eq!(found.len(), 1, "{}", kind.name());
    found[0]
}

/// Summons one `kind` at (8, -60, 8) with `nbt`.
fn summon(world: &mut GameWorld, kind: EntityTypeId, nbt: &str) -> EntityId {
    run(world, &format!("summon {} 8.5 -60 8.5 {nbt}", kind.name()));
    one(world, kind)
}

fn saved(world: &mut GameWorld, id: EntityId) -> Compound {
    world.level().save_entity(id).unwrap()
}

/// The entity saved, removed and loaded again, as with its chunk.
fn reload(world: &mut GameWorld, id: EntityId) -> EntityId {
    let nbt = saved(world, id);
    let kind = world.level().entities.store.get(id).unwrap().kind();
    world.level().entities.despawn(id).unwrap();
    assert!(world.level().load_entity(nbt));
    one(world, kind)
}

fn health(world: &mut GameWorld, id: EntityId) -> f32 {
    world.level().entities.mob(id).unwrap().health
}

fn has_flag(world: &mut GameWorld, id: EntityId, flag: EntityFlags) -> bool {
    let e = world.level().entities.store.get(id).unwrap();
    e.flags().contains(flag)
}

/// The shared flags byte and whether the boolean data item `index` is true, as clients get
/// them.
fn synched(world: &mut GameWorld, id: EntityId, index: u8) -> (u8, bool) {
    let data = all_entity_data(&world.level().entities, id).unwrap();
    assert_eq!(&data[..2], &[0, 0]);
    let on = data.windows(3).any(|w| w == [index, 8, 1]);
    (data[2], on)
}

#[test]
fn no_ai_sheep_with_a_name_stays() {
    // An SNBT string is the text component itself (since 1.21.5 no JSON), so the single
    // quoted form keeps its quotes.
    for (nbt, name) in [
        (r#"{NoAI:1b,CustomName:'"Dinnerbone"'}"#, "\"Dinnerbone\""),
        (r#"{NoAI:1b,CustomName:"Dinnerbone"}"#, "Dinnerbone"),
    ] {
        let mut world = world();
        let id = summon(&mut world, ids::SHEEP, nbt);
        let m = world.level().entities.mob(id).unwrap();
        assert!(m.no_ai);
        let expected = vanillite_game::mob::nbt::tag_component(&Tag::String(name.into())).unwrap();
        assert_eq!(m.custom_name.as_deref(), Some(&*expected));
        let start = world.level().entities.store.get(id).unwrap().pos();
        let rot = world.level().entities.store.get(id).unwrap().rot();
        for _ in 0..600 {
            world.step();
        }
        let e = world.level().entities.store.get(id).unwrap();
        assert_eq!((e.pos(), e.rot()), (start, rot), "{nbt}");
    }
}

#[test]
fn entity_flags_act_and_survive_a_reload() {
    let mut world = world();
    let mut id = summon(
        &mut world,
        ids::SHEEP,
        "{NoAI:1b,NoGravity:1b,Glowing:1b,CustomNameVisible:1b,Invulnerable:1b,\
         HasVisualFire:1b,PortalCooldown:50,data:{a:1}}",
    );
    for round in 0..2 {
        for flag in [
            EntityFlags::NO_GRAVITY,
            EntityFlags::GLOWING,
            EntityFlags::GLOWING_TAG,
            EntityFlags::NAME_VISIBLE,
            EntityFlags::INVULNERABLE,
            EntityFlags::VISUAL_FIRE,
        ] {
            assert!(has_flag(&mut world, id, flag), "round {round}: {flag:?}");
        }
        // On fire (bit 0) and glowing (bit 6) for the client; name visible and no gravity.
        assert_eq!(synched(&mut world, id, 3), (0x41, true), "round {round}");
        assert!(synched(&mut world, id, 5).1);
        // The server does not burn it.
        assert!(!has_flag(&mut world, id, EntityFlags::ON_FIRE));
        assert!(world.level().entities.portals[&id].cooldown > 0);
        run(&mut world, "damage @e[type=sheep,limit=1] 5");
        assert_eq!(health(&mut world, id), 8.0, "round {round}");
        let nbt = saved(&mut world, id);
        for key in [
            "NoGravity",
            "Glowing",
            "CustomNameVisible",
            "Invulnerable",
            "HasVisualFire",
        ] {
            assert_eq!(nbt.get(key).and_then(Tag::as_i64), Some(1), "{key}");
        }
        assert!(matches!(nbt.get("data"), Some(Tag::Compound(c)) if c.get("a").is_some()));
        id = reload(&mut world, id);
    }
    // `data merge` clears them again.
    run(
        &mut world,
        "data merge entity @e[type=sheep,limit=1] {NoGravity:0b,Glowing:0b,\
         CustomNameVisible:0b,Invulnerable:0b,HasVisualFire:0b,PortalCooldown:0}",
    );
    assert_eq!(synched(&mut world, id, 3), (0, false));
    assert!(!synched(&mut world, id, 5).1);
    run(&mut world, "damage @e[type=sheep,limit=1] 5");
    assert_eq!(health(&mut world, id), 3.0);
    assert_eq!(
        world
            .level()
            .entities
            .portals
            .get(&id)
            .map_or(0, |p| p.cooldown),
        0
    );
}

#[test]
fn invulnerable_is_bypassed_by_kill() {
    let mut world = world();
    let id = summon(&mut world, ids::PIG, "{NoAI:1b,Invulnerable:1b}");
    run(&mut world, "kill @e[type=pig]");
    assert!(
        world
            .level()
            .entities
            .mob(id)
            .is_none_or(|m| m.health <= 0.0)
    );
}

#[test]
fn glowing_tag_outlasts_the_effect() {
    let mut world = world();
    let id = summon(&mut world, ids::PIG, "{NoAI:1b,Glowing:1b}");
    run(&mut world, "effect give @e[type=pig] minecraft:glowing 1");
    for _ in 0..40 {
        world.step();
    }
    assert!(has_flag(&mut world, id, EntityFlags::GLOWING));
}

#[test]
fn non_mobs_keep_the_common_keys() {
    let mut world = world();
    for kind in [ids::OAK_BOAT, ids::MINECART, ids::ITEM] {
        let item = if kind == ids::ITEM {
            "Item:{id:\"minecraft:stone\",count:1},"
        } else {
            ""
        };
        let id = summon(
            &mut world,
            kind,
            &format!("{{{item}NoGravity:1b,Glowing:1b,Invulnerable:1b,data:{{a:1}}}}"),
        );
        let id = reload(&mut world, id);
        assert!(has_flag(&mut world, id, EntityFlags::NO_GRAVITY));
        assert_eq!(synched(&mut world, id, 5), (0x40, true), "{}", kind.name());
        let nbt = saved(&mut world, id);
        assert_eq!(nbt.get("Invulnerable").and_then(Tag::as_i64), Some(1));
        assert!(nbt.get("data").is_some());
        if kind != ids::ITEM {
            run(
                &mut world,
                &format!("damage @e[type={},limit=1] 100", kind.name()),
            );
            assert_eq!(all_of(&mut world, kind), [id]);
        }
    }
}

#[test]
fn attributes_apply_before_health() {
    for nbt in [
        "{Health:40f,attributes:[{id:\"minecraft:max_health\",base:40}]}",
        "{attributes:[{id:\"max_health\",base:40.0d}]}",
        "{Health:40,attributes:[{id:\"minecraft:max_health\",base:20,modifiers:\
         [{id:\"test:more\",amount:1,operation:\"add_multiplied_base\"}]}]}",
    ] {
        let mut world = world();
        let mut id = summon(&mut world, ids::SHEEP, nbt);
        for round in 0..2 {
            assert_eq!(health(&mut world, id), 40.0, "{nbt} round {round}");
            let m = world.level().entities.mob(id).unwrap();
            assert_eq!(m.max_health(), 40.0);
            let given = &world.level().entities.extra(id).unwrap().attributes.clone();
            let m = world.level().entities.mob(id).unwrap();
            assert!(mob_pairing_attributes(1, m, given).is_some());
            for _ in 0..5 {
                world.step();
            }
            id = reload(&mut world, id);
        }
        run(
            &mut world,
            "data merge entity @e[type=sheep,limit=1] {Air:100}",
        );
        assert_eq!(health(&mut world, id), 40.0, "{nbt} merged");
    }
}

#[test]
fn horse_attributes_take_any_number() {
    let mut world = world();
    let id = summon(
        &mut world,
        ids::HORSE,
        "{attributes:[{id:\"minecraft:max_health\",base:30},\
         {id:\"minecraft:jump_strength\",base:1.5f}]}",
    );
    let id = reload(&mut world, id);
    let m = world.level().entities.mob(id).unwrap();
    assert_eq!((m.health, m.max_health()), (30.0, 30.0));
    assert_eq!(m.attrs.jump_strength, 1.5);
}

#[test]
fn a_tame_wolf_keeps_its_loaded_max_health() {
    let mut world = world();
    let id = summon(
        &mut world,
        ids::WOLF,
        "{Owner:[I;1,2,3,4],attributes:[{id:\"minecraft:max_health\",base:25}]}",
    );
    let id = reload(&mut world, id);
    let m = world.level().entities.mob(id).unwrap();
    assert_eq!((m.health, m.max_health()), (25.0, 25.0));
}

#[test]
fn slime_size_comes_before_its_attributes() {
    let mut world = world();
    let id = summon(&mut world, ids::SLIME, "{Size:3,NoAI:1b}");
    let m = world.level().entities.mob(id).unwrap();
    assert_eq!((m.health, m.max_health()), (16.0, 16.0));
}

#[test]
fn any_numeric_tag_type_is_read() {
    let mut world = world();
    let id = summon(
        &mut world,
        ids::SHEEP,
        "{Health:3,NoAI:1,AgeLocked:1,Age:-100.5d,Rotation:[90,0],NoGravity:1,Motion:[0,0,0]}",
    );
    assert_eq!(health(&mut world, id), 3.0);
    let m = world.level().entities.mob(id).unwrap();
    assert!(m.no_ai && m.age_locked);
    // A floating point value read as an integer is floored.
    assert_eq!(m.age, -101);
    assert_eq!(
        world.level().entities.store.get(id).unwrap().rot().y_rot,
        90.0
    );
    assert!(has_flag(&mut world, id, EntityFlags::NO_GRAVITY));
    let mut nbt = saved(&mut world, id);
    nbt.insert(
        "Pos",
        Tag::List(vanillite_nbt::List(vec![
            Tag::Int(4),
            Tag::Int(-60),
            Tag::Int(4),
        ])),
    );
    nbt.insert("Health", Tag::Byte(2));
    world.level().entities.despawn(id).unwrap();
    assert!(world.level().load_entity(nbt));
    let id = one(&mut world, ids::SHEEP);
    assert_eq!(health(&mut world, id), 2.0);
    assert_eq!(world.level().entities.store.get(id).unwrap().pos().x, 4.0);
}

#[test]
fn no_gravity_mob_does_not_fall_after_a_reload() {
    let mut world = world();
    run(
        &mut world,
        "summon minecraft:pig 8.5 -57 8.5 {NoGravity:1b}",
    );
    let id = one(&mut world, ids::PIG);
    let id = reload(&mut world, id);
    for _ in 0..40 {
        world.step();
    }
    let y = world.level().entities.store.get(id).unwrap().pos().y;
    assert!(y > -57.5, "fell to {y}");
}

#[test]
fn enderman_carries_its_block() {
    let mut world = world();
    let id = summon(
        &mut world,
        ids::ENDERMAN,
        "{NoAI:1b,carriedBlockState:{Name:\"minecraft:dirt\"}}",
    );
    let id = reload(&mut world, id);
    let carried = |world: &mut GameWorld| {
        let m = world.level().entities.mob(id).unwrap();
        vanillite_game::mob::fam::classic::enderman(m).carried
    };
    assert_eq!(
        carried(&mut world),
        Some(vanillite_data::block::ids::DIRT.default_state())
    );
    assert!(saved(&mut world, id).get("carriedBlockState").is_some());
}

/// The item ids lying around after the pig summoned with `nbt` is killed.
fn pig_drops(nbt: &str) -> Vec<String> {
    let mut world = world();
    summon(&mut world, ids::PIG, nbt);
    run(&mut world, "kill @e[type=pig]");
    for _ in 0..3 {
        world.step();
    }
    let items = all_of(&mut world, ids::ITEM);
    items
        .into_iter()
        .map(|id| {
            let d = world.level().entities.item(id).unwrap();
            vanillite_game::item::item_id(&d.stack).name().to_owned()
        })
        .collect()
}

#[test]
fn death_loot_table_replaces_the_types_table() {
    assert!(pig_drops("{NoAI:1b}").contains(&"minecraft:porkchop".to_owned()));
    assert!(pig_drops("{NoAI:1b,DeathLootTable:\"minecraft:empty\"}").is_empty());
    for seed in ["", ",DeathLootTableSeed:42L"] {
        let drops = pig_drops(&format!(
            "{{NoAI:1b,DeathLootTable:\"minecraft:entities/cow\"{seed}}}"
        ));
        assert!(drops.contains(&"minecraft:beef".to_owned()), "{drops:?}");
        assert!(!drops.contains(&"minecraft:porkchop".to_owned()));
    }
}

#[test]
fn team_joins_the_entity() {
    let mut world = world();
    run(&mut world, "team add nbt_team");
    summon(&mut world, ids::PIG, "{NoAI:1b,Team:\"nbt_team\"}");
    run(&mut world, "kill @e[type=pig,team=nbt_team]");
    run(&mut world, "team remove nbt_team");
}
