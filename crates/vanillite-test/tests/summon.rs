//! `/summon` of the entities with kind data of their own: every summoned entity gets its own
//! row in its kind store (none aliases another entity's data), and removing a thrown potion
//! leaves the other stores alone.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use vanillite_data::entity::{EntityTypeId, ids};
use vanillite_game::mob::boss::special::is_special_kind;
use vanillite_game::mob::missile::is_missile_kind;
use vanillite_game::mob::projectile::is_arrow_kind;
use vanillite_test::game_world::GameWorld;
use vanillite_test::scenario::Scenario;
use vanillite_test::world::ScenarioWorld;

const KINDS: &[(&str, &str)] = &[
    ("oak_boat", ""),
    ("oak_chest_boat", ""),
    ("minecart", ""),
    ("chest_minecart", ""),
    ("hopper_minecart", ""),
    ("tnt_minecart", ""),
    ("arrow", ""),
    ("spectral_arrow", ""),
    ("trident", ""),
    ("snowball", ""),
    ("ender_pearl", ""),
    ("fireball", ""),
    ("small_fireball", ""),
    ("shulker_bullet", ""),
    ("llama_spit", ""),
    ("wind_charge", ""),
    ("breeze_wind_charge", ""),
    ("evoker_fangs", ""),
    ("experience_orb", "Value:3s,"),
    ("area_effect_cloud", "Duration:100,"),
    (
        "splash_potion",
        "Item:{id:\"minecraft:splash_potion\",count:1},",
    ),
    (
        "lingering_potion",
        "Item:{id:\"minecraft:lingering_potion\",count:1},",
    ),
    ("wither_skull", ""),
    ("dragon_fireball", ""),
];

/// The kind store an entity type keeps its data in.
fn store(kind: EntityTypeId) -> &'static str {
    if vanillite_game::vehicle::BoatKind::of(kind).is_some() {
        "boats"
    } else if vanillite_game::minecart::MinecartKind::of(kind).is_some() {
        "minecarts"
    } else if is_arrow_kind(kind) {
        "arrows"
    } else if is_missile_kind(kind) {
        "missiles"
    } else if is_special_kind(kind) {
        "specials"
    } else if kind == ids::EXPERIENCE_ORB {
        "orbs"
    } else if kind == ids::AREA_EFFECT_CLOUD {
        "clouds"
    } else {
        "none"
    }
}

/// Every entity has a row of its own in its store and saves as its own kind.
fn check_rows(world: &mut GameWorld) -> usize {
    let level = world.level();
    let ids: Vec<_> = level.entities.store.columns().id.to_vec();
    let mut rows = BTreeSet::new();
    for &id in &ids {
        let e = level.entities.store.get(id).unwrap();
        let kind = e.kind();
        let store = store(kind);
        assert_ne!(store, "none", "{} has no kind store", kind.name());
        assert!(
            rows.insert((store, e.kind_row())),
            "{} shares row {} of the {store} store",
            kind.name(),
            e.kind_row()
        );
        let saved = level
            .save_entity(id)
            .unwrap_or_else(|| panic!("{} does not save", kind.name()));
        assert_eq!(
            saved.get("id").and_then(|t| t.as_str()),
            Some(kind.name()),
            "{} saves as another kind",
            kind.name()
        );
    }
    ids.len()
}

#[test]
fn summoned_entities_get_their_own_rows() {
    let scenario = Scenario::parse(
        "summon_rows",
        r#"{"region": [[0, -61, 0], [15, -55, 15]], "ticks": 1}"#,
    )
    .unwrap();
    let mut world = GameWorld::new(&scenario.world).unwrap();
    // Two of each, the kinds interleaved, so later rows of a store are used as well.
    for round in 0..2 {
        for (i, (kind, nbt)) in KINDS.iter().enumerate() {
            let line = format!(
                "summon minecraft:{kind} {} -59 {} {{{nbt}NoGravity:1b}}",
                i % 16,
                round * 8
            );
            world
                .command(&line)
                .unwrap_or_else(|e| panic!("{line}: {e}"));
        }
    }
    assert_eq!(check_rows(&mut world), KINDS.len() * 2);
    // A removed thrown potion takes its own row with it, not one of another store.
    world
        .command("kill @e[type=minecraft:splash_potion]")
        .unwrap();
    assert_eq!(check_rows(&mut world), KINDS.len() * 2 - 2);
}
