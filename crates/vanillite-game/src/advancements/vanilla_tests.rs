//! The Vanilla advancements run through scripted trigger events: each event completes the
//! criteria Vanilla completes with it, and only those.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use vanillite_base::{BlockPos, Vec3};
use vanillite_data::block::{BlockId, BlockState};
use vanillite_data::registry;

use super::predicate::{DamageEvent, DamageView, EntityView, World};
use super::trigger::{Event, TriggerContext, TriggerKind};
use super::{Advancements, PlayerAdvancements};
use crate::inventory::PlayerInventory;
use crate::item::ItemStack;
use crate::survival::damage::tag;

fn pack() -> PathBuf {
    let cache = std::env::var_os("VANILLITE_CACHE").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/vanilla-cache"),
        PathBuf::from,
    );
    let jar = std::env::var_os("VANILLITE_SERVER_JAR").map(PathBuf::from);
    vanillite_datapack::vanilla::ensure_vanilla_pack(&cache, jar.as_deref())
        .expect("Vanilla data pack (set VANILLITE_SERVER_JAR for an offline copy)")
}

fn json_files(dir: &Path) -> BTreeMap<String, String> {
    let mut files = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().map(Result::unwrap) {
            let path = e.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|x| x == "json") {
                let rel = path.strip_prefix(dir).unwrap().with_extension("");
                let id = format!("minecraft:{}", rel.to_string_lossy());
                files.insert(id, std::fs::read_to_string(&path).unwrap());
            }
        }
    }
    files
}

/// The Vanilla tags of the registries the advancements use, flattened.
struct Tags(HashMap<(String, String), HashSet<String>>);

impl Tags {
    fn load(pack: &Path) -> Tags {
        let mut raw: HashMap<(String, String), Vec<String>> = HashMap::new();
        for (registry, dir) in [
            ("minecraft:entity_type", "entity_type"),
            ("minecraft:damage_type", "damage_type"),
            ("minecraft:item", "item"),
            ("minecraft:block", "block"),
        ] {
            for (id, text) in json_files(&pack.join("data/minecraft/tags").join(dir)) {
                let v: serde_json::Value = serde_json::from_str(&text).unwrap();
                let values = v["values"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|e| e.as_str().or_else(|| e["id"].as_str()))
                    .map(str::to_owned)
                    .collect();
                raw.insert((registry.to_owned(), id), values);
            }
        }
        fn flatten(
            raw: &HashMap<(String, String), Vec<String>>,
            key: &(String, String),
            out: &mut HashSet<String>,
        ) {
            for v in raw.get(key).into_iter().flatten() {
                match v.strip_prefix('#') {
                    Some(t) => flatten(raw, &(key.0.clone(), t.to_owned()), out),
                    None => {
                        out.insert(v.clone());
                    }
                }
            }
        }
        let mut tags = HashMap::new();
        for key in raw.keys() {
            let mut out = HashSet::new();
            flatten(&raw, key, &mut out);
            tags.insert(key.clone(), out);
        }
        Tags(tags)
    }
}

struct Overworld<'a>(&'a Tags);

impl World for Overworld<'_> {
    fn dimension(&self) -> &str {
        "minecraft:overworld"
    }
    fn block(&self, _: BlockPos) -> Option<BlockState> {
        None
    }
    fn biome(&self, _: BlockPos) -> Option<String> {
        None
    }
    fn in_tag(&self, registry: &str, tag: &str, id: &str) -> bool {
        self.0
            .0
            .get(&(registry.to_owned(), tag.to_owned()))
            .is_some_and(|s| s.contains(id))
    }
}

fn stack(name: &str) -> ItemStack {
    ItemStack::new(registry::ITEM.id(name).unwrap(), 1)
}

fn entity(kind: &'static str, pos: Vec3) -> Box<EntityView<'static>> {
    Box::new(EntityView::new(kind, pos))
}

fn damage(kind: &str, tags: u32, cause: Option<Box<EntityView<'static>>>) -> DamageView {
    DamageView {
        kind: kind.to_owned(),
        tags,
        direct: cause.clone(),
        cause,
        is_direct: true,
        position: None,
    }
}

struct Run {
    set: Arc<Advancements>,
    progress: PlayerAdvancements,
    tags: Tags,
    inventory: PlayerInventory,
}

impl Run {
    /// Fires `event` for a player at `pos` (with `vehicle`) and returns the completed criteria
    /// as `advancement#criterion`.
    fn fire(
        &mut self,
        pos: Vec3,
        vehicle: Option<Box<EntityView<'static>>>,
        event: Event,
    ) -> Vec<String> {
        let before = self.done();
        let mut player = EntityView::new("minecraft:player", pos);
        player.vehicle = vehicle;
        let world = Overworld(&self.tags);
        assert!(
            self.progress.listens(event.kind()),
            "nothing listens for {:?}",
            event.kind()
        );
        self.progress.trigger(event);
        self.progress
            .process([], &TriggerContext::new(&world, &player, &self.inventory));
        let mut new: Vec<String> = self.done().difference(&before).cloned().collect();
        new.sort();
        new
    }

    fn done(&self) -> HashSet<String> {
        let mut out = HashSet::new();
        for (index, a) in self.set.iter() {
            for (c, criterion) in (0u16..).zip(&a.criteria) {
                if self.progress.criterion_done(index, c) {
                    out.insert(format!("{}#{}", a.id, criterion.name));
                }
            }
        }
        out
    }
}

#[test]
fn vanilla_advancements_through_scripted_events() {
    let pack = pack();
    let files = json_files(&pack.join("data/minecraft/advancement"));
    let set = Arc::new(Advancements::load(&files));
    assert_eq!(set.len(), files.len(), "every Vanilla advancement parses");
    let mut run = Run {
        progress: PlayerAdvancements::load(Arc::clone(&set), None),
        set,
        tags: Tags::load(&pack),
        inventory: PlayerInventory::default(),
    };
    let at = Vec3::new(0.5, 64.0, 0.5);
    let player = || Some(entity("minecraft:player", at));

    // Combat.
    let zombie = entity("minecraft:zombie", Vec3::new(3.0, 64.0, 0.0));
    let hit = damage("minecraft:player_attack", 0, player());
    let killed = run.fire(
        at,
        None,
        Event::Killed(TriggerKind::PLAYER_KILLED_ENTITY, zombie, Box::new(hit)),
    );
    assert!(killed.contains(&"minecraft:adventure/kill_a_mob#minecraft:zombie".to_owned()));
    assert!(killed.contains(&"minecraft:adventure/kill_all_mobs#minecraft:zombie".to_owned()));
    assert!(!killed.iter().any(|c| c.contains("#minecraft:skeleton")));
    let arrow = damage(
        "minecraft:arrow",
        tag::IS_PROJECTILE,
        Some(entity("minecraft:skeleton", at)),
    );
    let blocked = run.fire(
        at,
        None,
        Event::EntityHurtPlayer(Box::new(DamageEvent {
            source: arrow,
            dealt: 4.0,
            taken: 0.0,
            blocked: true,
        })),
    );
    assert_eq!(
        blocked,
        ["minecraft:story/deflect_arrow#deflected_projectile"]
    );

    // Mob interactions.
    let golem = entity("minecraft:iron_golem", at);
    let summoned = run.fire(at, None, Event::Entity(TriggerKind::SUMMONED_ENTITY, golem));
    assert_eq!(
        summoned,
        ["minecraft:adventure/summon_iron_golem#summoned_golem"]
    );
    let cow = || entity("minecraft:cow", at);
    let bred = run.fire(at, None, Event::BredAnimals(cow(), cow(), Some(cow())));
    assert!(bred.contains(&"minecraft:husbandry/breed_an_animal#bred".to_owned()));
    assert!(bred.contains(&"minecraft:husbandry/bred_all_animals#minecraft:cow".to_owned()));
    let mut cat = entity("minecraft:cat", at);
    cat.components
        .push(("minecraft:cat/variant", "minecraft:black".to_owned()));
    let tamed = run.fire(at, None, Event::Entity(TriggerKind::TAME_ANIMAL, cat));
    assert!(tamed.contains(&"minecraft:husbandry/tame_an_animal#tamed_animal".to_owned()));
    assert!(tamed.contains(&"minecraft:husbandry/complete_catalogue#minecraft:black".to_owned()));
    let mut frog = entity("minecraft:frog", at);
    frog.components
        .push(("minecraft:frog/variant", "minecraft:cold".to_owned()));
    let leashed = run.fire(
        at,
        None,
        Event::EntityItem(
            TriggerKind::PLAYER_INTERACTED_WITH_ENTITY,
            Some(frog),
            stack("minecraft:lead"),
        ),
    );
    assert_eq!(
        leashed,
        ["minecraft:husbandry/leash_all_frog_variants#minecraft:cold"]
    );
    let mut boat = entity("minecraft:oak_boat", at);
    boat.passengers = vec![
        EntityView::new("minecraft:player", at),
        EntityView::new("minecraft:goat", at),
    ];
    let riding = run.fire(at, Some(boat), Event::Player(TriggerKind::STARTED_RIDING));
    assert_eq!(
        riding,
        ["minecraft:husbandry/ride_a_boat_with_a_goat#ride_a_boat_with_a_goat"]
    );
    let mut bolt = entity("minecraft:lightning_bolt", Vec3::new(10.0, 64.0, 0.0));
    bolt.lightning_fires = Some(0);
    let struck = run.fire(
        at,
        None,
        Event::LightningStrike(bolt, vec![EntityView::new("minecraft:villager", at)]),
    );
    assert_eq!(
        struck,
        [
            "minecraft:adventure/lightning_rod_with_villager_no_fire#lightning_rod_with_villager_no_fire"
        ]
    );

    // Player state.
    let far = Vec3::new(7000.5, 64.0, 0.5);
    let travelled = run.fire(far, None, Event::Distance(TriggerKind::NETHER_TRAVEL, at));
    assert_eq!(travelled, ["minecraft:nether/fast_travel#travelled"]);
    let up = Vec3::new(0.5, 115.0, 0.5);
    let levitated = run.fire(up, None, Event::Levitation(at, 100));
    assert_eq!(levitated, ["minecraft:end/levitate#levitated"]);
    let bottom = Vec3::new(0.5, -60.0, 0.5);
    let fell = run.fire(
        bottom,
        None,
        Event::Distance(TriggerKind::FALL_FROM_HEIGHT, Vec3::new(0.5, 320.0, 0.5)),
    );
    assert_eq!(
        fell,
        ["minecraft:adventure/fall_from_world_height#fall_from_world_height"]
    );
    let strider = entity("minecraft:strider", at);
    let lava = run.fire(
        Vec3::new(60.5, 64.0, 0.5),
        Some(strider),
        Event::Distance(TriggerKind::RIDE_ENTITY_IN_LAVA, at),
    );
    assert_eq!(
        lava,
        ["minecraft:nether/ride_strider_in_overworld_lava#ride_entity_distance"]
    );
    let totem = run.fire(
        at,
        None,
        Event::Item(TriggerKind::USED_TOTEM, stack("minecraft:totem_of_undying")),
    );
    assert_eq!(totem, ["minecraft:adventure/totem_of_undying#used_totem"]);
    let avoided = run.fire(at, None, Event::Player(TriggerKind::AVOID_VIBRATION));
    assert_eq!(
        avoided,
        ["minecraft:adventure/avoid_vibration#avoid_vibration"]
    );

    // Blocks and menus.
    let beacon = run.fire(at, None, Event::ConstructBeacon(4));
    assert!(beacon.contains(&"minecraft:nether/create_beacon#beacon".to_owned()));
    assert!(beacon.contains(&"minecraft:nether/create_full_beacon#beacon".to_owned()));
    let brewed = run.fire(
        at,
        None,
        Event::BrewedPotion(Some("minecraft:awkward".to_owned())),
    );
    assert_eq!(brewed, ["minecraft:nether/brew_potion#potion"]);
    let enchanted = run.fire(
        at,
        None,
        Event::EnchantedItem(stack("minecraft:iron_sword"), 1),
    );
    assert_eq!(enchanted, ["minecraft:story/enchant_item#enchanted_item"]);
    let near = entity("minecraft:arrow", Vec3::new(5.0, 64.0, 0.0));
    assert!(run.fire(at, None, Event::TargetHit(near, 15)).is_empty());
    let shot = entity("minecraft:arrow", Vec3::new(40.0, 64.0, 0.0));
    let bullseye = run.fire(at, None, Event::TargetHit(shot, 15));
    assert_eq!(bullseye, ["minecraft:adventure/bullseye#bullseye"]);
    let honey = BlockId::by_name("minecraft:honey_block")
        .unwrap()
        .default_state();
    let slid = run.fire(at, None, Event::SlideDownBlock(honey));
    assert_eq!(
        slid,
        ["minecraft:adventure/honey_block_slide#honey_block_slide"]
    );
    let looted = run.fire(
        at,
        None,
        Event::GenerateLoot("minecraft:chests/bastion_bridge".to_owned()),
    );
    assert_eq!(
        looted,
        ["minecraft:nether/loot_bastion#loot_bastion_bridge"]
    );

    // Villages, raids and weapons.
    let villager = || entity("minecraft:villager", at);
    let traded = run.fire(
        at,
        None,
        Event::EntityItem(
            TriggerKind::VILLAGER_TRADE,
            Some(villager()),
            stack("minecraft:emerald"),
        ),
    );
    assert_eq!(traded, ["minecraft:adventure/trade#traded"]);
    let cured = run.fire(
        at,
        None,
        Event::CuredZombieVillager(entity("minecraft:zombie_villager", at), villager()),
    );
    assert_eq!(cured, ["minecraft:story/cure_zombie_villager#cured_zombie"]);
    let struck = run.fire(at, None, Event::ChanneledLightning(vec![*villager()]));
    assert_eq!(
        struck,
        ["minecraft:adventure/very_very_frightening#struck_villager"]
    );
    assert!(run.fire(at, None, Event::SpearMobs(4)).is_empty());
    let speared = run.fire(at, None, Event::SpearMobs(5));
    assert_eq!(
        speared,
        ["minecraft:adventure/spear_many_mobs#spear_many_mobs"]
    );
    let nest = BlockId::by_name("minecraft:bee_nest")
        .unwrap()
        .default_state();
    let no_silk_touch = Event::BeeNestDestroyed(nest, stack("minecraft:iron_axe"), 3);
    assert!(run.fire(at, None, no_silk_touch).is_empty());
    let hero = run.fire(at, None, Event::Player(TriggerKind::HERO_OF_THE_VILLAGE));
    assert_eq!(
        hero,
        ["minecraft:adventure/hero_of_the_village#hero_of_the_village"]
    );
    let shot = run.fire(
        at,
        None,
        Event::Item(TriggerKind::SHOT_CROSSBOW, stack("minecraft:crossbow")),
    );
    assert!(shot.contains(&"minecraft:adventure/ol_betsy#shot_crossbow".to_owned()));
}
