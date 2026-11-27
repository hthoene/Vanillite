//! The synched entity data of every summonable entity type against Vanilla's layout
//! (`data/vanilla/entity_data.json`): every item the server sends must have an index the
//! type defines, with that index's serializer. The client drops the connection on any
//! mismatch (`SynchedEntityData.assignValue`), so a wrong index kicks every player that
//! comes near such an entity.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};

use vanillite_protocol::codec::Reader;
use vanillite_test::game_world::GameWorld;
use vanillite_test::scenario::Scenario;
use vanillite_test::world::ScenarioWorld;

/// Per entity type: index -> serializer name; and the serializer names by id.
type Layouts = (BTreeMap<String, BTreeMap<u8, String>>, Vec<String>);

fn vanilla_layouts() -> Layouts {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../data/vanilla/entity_data.json"
    );
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let serializers = json["serializers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_owned())
        .collect();
    let types = json["types"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(name, items)| {
            let items = items
                .as_array()
                .unwrap()
                .iter()
                .map(|i| {
                    (
                        u8::try_from(i[0].as_u64().unwrap()).unwrap(),
                        i[1].as_str().unwrap().to_owned(),
                    )
                })
                .collect();
            (name.clone(), items)
        })
        .collect();
    (types, serializers)
}

/// Skips one particle (`ParticleTypes.STREAM_CODEC`) of the types entities send.
fn skip_particle(r: &mut Reader<'_>) -> Result<(), String> {
    let id = r.varint().map_err(|e| e.to_string())?;
    let name = vanillite_data::registry::PARTICLE_TYPE
        .name(u16::try_from(id).map_err(|e| e.to_string())?)
        .ok_or("unknown particle")?;
    let len = match name {
        // `ColorParticleOption`: an ARGB int; `PowerParticleOption`: a float.
        "minecraft:entity_effect" | "minecraft:dragon_breath" => 4,
        _ => return Err(format!("cannot measure particle {name}")),
    };
    r.skip(len).map_err(|e| e.to_string())
}

/// Skips one value of `serializer`.
fn skip_value(r: &mut Reader<'_>, serializer: &str) -> Result<(), String> {
    let e = |e: vanillite_protocol::Error| e.to_string();
    let fixed = match serializer {
        "byte" | "boolean" => 1,
        "float" => 4,
        "block_pos" => 8,
        "rotations" | "vector3" => 12,
        "quaternion" => 16,
        "int"
        | "direction"
        | "block_state"
        | "optional_block_state"
        | "optional_unsigned_int"
        | "pose"
        | "cat_variant"
        | "cat_sound_variant"
        | "cow_variant"
        | "cow_sound_variant"
        | "wolf_variant"
        | "wolf_sound_variant"
        | "frog_variant"
        | "pig_variant"
        | "pig_sound_variant"
        | "chicken_variant"
        | "chicken_sound_variant"
        | "zombie_nautilus_variant"
        | "sniffer_state"
        | "armadillo_state"
        | "copper_golem_state"
        | "weathering_copper_state"
        | "humanoid_arm"
        | "dye_color" => {
            r.varint().map_err(e)?;
            0
        }
        "painting_variant" => {
            // `holder`: 0 is an inline variant, else the registry id + 1.
            if r.varint().map_err(e)? == 0 {
                return Err("inline painting variant".into());
            }
            0
        }
        "long" => {
            r.varlong().map_err(e)?;
            0
        }
        "villager_data" => {
            for _ in 0..3 {
                r.varint().map_err(e)?;
            }
            0
        }
        "string" => {
            let n = r.varint().map_err(e)?;
            usize::try_from(n).map_err(|e| e.to_string())?
        }
        "component" | "optional_component" => {
            if serializer == "optional_component" && r.u8().map_err(e)? == 0 {
                return Ok(());
            }
            vanillite_nbt::read::skip_network(r.rest(), &mut vanillite_nbt::read::Budget::network())
                .map_err(|e| e.to_string())?
        }
        "optional_block_pos" => {
            if r.u8().map_err(e)? == 0 {
                0
            } else {
                8
            }
        }
        "optional_living_entity_reference" => {
            if r.u8().map_err(e)? == 0 {
                0
            } else {
                16
            }
        }
        "optional_global_pos" => {
            if r.u8().map_err(e)? != 0 {
                let n = r.varint().map_err(e)?;
                r.skip(usize::try_from(n).map_err(|e| e.to_string())?)
                    .map_err(e)?;
                8
            } else {
                0
            }
        }
        "item_stack" => {
            vanillite_protocol::item::ItemStack::decode_plain(r).map_err(e)?;
            0
        }
        "particle" => {
            skip_particle(r)?;
            0
        }
        "particles" => {
            let n = r.varint().map_err(e)?;
            for _ in 0..n {
                skip_particle(r)?;
            }
            0
        }
        "resolvable_profile" => vanillite_protocol::components::value_len(
            vanillite_protocol::components::ids::PROFILE,
            r.rest(),
        )
        .map_err(e)?,
        other => return Err(format!("cannot measure serializer {other}")),
    };
    r.skip(fixed).map_err(e)
}

/// The problems of one entity's data against its type's layout.
fn check(
    kind: &str,
    data: &[u8],
    layout: &BTreeMap<u8, String>,
    serializers: &[String],
) -> Vec<String> {
    let mut problems = Vec::new();
    let mut seen = BTreeSet::new();
    let mut r = Reader::new(data);
    loop {
        let Ok(index) = r.u8() else {
            problems.push(format!("{kind}: no 0xFF terminator"));
            break;
        };
        if index == 0xFF {
            if !r.rest().is_empty() {
                problems.push(format!("{kind}: bytes after the terminator"));
            }
            break;
        }
        let Some(name) = r
            .varint()
            .ok()
            .and_then(|s| usize::try_from(s).ok())
            .and_then(|s| serializers.get(s))
        else {
            problems.push(format!("{kind}: item {index} has an unknown serializer"));
            break;
        };
        match layout.get(&index) {
            None => problems.push(format!(
                "{kind}: item {index} ({name}) is not defined (Vanilla: {layout:?})"
            )),
            Some(expected) if expected != name => problems.push(format!(
                "{kind}: item {index} is {name}, Vanilla {expected} (Vanilla: {layout:?})"
            )),
            Some(_) => {}
        }
        if !seen.insert(index) {
            problems.push(format!("{kind}: item {index} sent twice"));
        }
        if let Err(e) = skip_value(&mut r, name) {
            problems.push(format!("{kind}: item {index} ({name}): {e}"));
            break;
        }
    }
    problems
}

#[test]
fn every_entity_type_sends_vanilla_entity_data() {
    let (layouts, serializers) = vanilla_layouts();
    let scenario = Scenario::parse(
        "entity_data_layout",
        r#"{"region": [[0, -61, 0], [31, -40, 31]], "ticks": 1,
            "entities": [{"name": "p", "type": "minecraft:player", "pos": [8.5, -60.0, 8.5]}]}"#,
    )
    .unwrap();
    let mut world = GameWorld::new(&scenario.world).unwrap();
    // Players cannot be summoned.
    world.add_player(&scenario.entities[0], 0).unwrap();
    let mut problems = Vec::new();
    let mut checked = BTreeSet::new();
    let mut not_summoned = Vec::new();
    for info in &vanillite_data::entity::ENTITY_TYPES {
        if !info.summonable {
            continue;
        }
        let nbt = match info.name {
            "minecraft:item" => "{Item:{id:\"minecraft:stone\",count:1},NoGravity:1b}",
            "minecraft:falling_block" => "{BlockState:{Name:\"minecraft:sand\"},NoGravity:1b}",
            "minecraft:splash_potion" | "minecraft:lingering_potion" => {
                "{Item:{id:\"minecraft:splash_potion\",count:1},NoGravity:1b}"
            }
            _ => "{NoGravity:1b}",
        };
        let line = format!("summon {} 16 -50 16 {nbt}", info.name);
        if let Err(e) = world.command(&line) {
            not_summoned.push(format!("{}: {e}", info.name));
        }
    }
    let level = world.level();
    let ids: Vec<_> = level.entities.store.columns().id.to_vec();
    for id in ids {
        let kind = level.entities.store.get(id).unwrap().kind().name();
        let Some(layout) = layouts.get(kind) else {
            problems.push(format!("{kind}: no Vanilla layout"));
            continue;
        };
        checked.insert(kind);
        let Some(data) = vanillite_game::player::encode::all_entity_data(&level.entities, id)
        else {
            problems.push(format!("{kind}: no entity data"));
            continue;
        };
        problems.extend(check(kind, &data, layout, &serializers));
    }
    problems.sort();
    problems.dedup();
    assert!(
        problems.is_empty(),
        "{} problems (checked {} types; not summoned: {not_summoned:?}):\n{}",
        problems.len(),
        checked.len(),
        problems.join("\n")
    );
    eprintln!(
        "checked {} entity types; not summoned: {not_summoned:?}",
        checked.len()
    );
}
