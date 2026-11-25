//! Simulation tests of mob spawners and trial spawners on the flat test level.

use std::sync::{Arc, Mutex};

use vanillite_base::{BlockPos, ChunkPos, Vec3};
use vanillite_config::GameMode;
use vanillite_data::block::ids;
use vanillite_nbt::{Compound, Tag};
use vanillite_protocol::SharedPacket;
use vanillite_world::entity::{EntityId, Rotation};

use super::spawner::{MobSpawner, SpawnData, SpawnPotentials};
use super::trial_spawner::{ConfigRef, TrialConfig, TrialSpawner, TrialState};
use crate::level::block_entity::BlockEntity;
use crate::level::tests::{flat_level, tick};
use crate::level::{Level, update};
use crate::mob::Kind;
use crate::player::sink::PacketSink;
use crate::player::{ChunkView, ClientInformation, PlayerData};

struct NullSink;

impl PacketSink for NullSink {
    fn send_body(&self, _: Vec<u8>) -> bool {
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

fn add_player(level: &mut Level, pos: Vec3, game_mode: GameMode) -> EntityId {
    let (_tx, inbound) = vanillite_protocol::inbound_queue(16);
    let data = PlayerData {
        profile: crate::player::GameProfile::default(),
        game_mode,
        previous_game_mode: None,
        client: ClientInformation::default(),
        chunk_view: ChunkView {
            center: ChunkPos::new(0, 0),
            view_distance: 2,
        },
        connection: crate::player::connection::Connection::new(
            Arc::new(NullSink),
            inbound,
            pos,
            true,
            true,
            &crate::player::chat::ChatSettings::default(),
            Arc::new(Mutex::new(crate::player::chat::ChatInbox::default())),
        ),
        inventory: crate::inventory::PlayerInventory::default(),
        recipe_book: crate::recipe::book::PlayerRecipeBook::default(),
        last_equipment: std::array::from_fn(|_| crate::item::ItemStack::EMPTY),
        survival: crate::survival::player::Survival::default(),
    };
    let network_ids = Arc::clone(level.network_ids_arc());
    level
        .entities
        .spawn_player(&network_ids, 0xABCD, pos, Rotation::default(), data)
}

fn zombies(level: &Level) -> usize {
    level
        .entities
        .mobs
        .iter()
        .filter(|(_, m)| m.kind == Kind::Zombie && m.is_alive())
        .count()
}

const SPAWNER: BlockPos = BlockPos::new(0, -60, 0);

fn place_spawner(level: &mut Level, spawner: MobSpawner) {
    assert!(level.set_block(SPAWNER, ids::SPAWNER.default_state(), update::ALL));
    level.set_block_entity(SPAWNER, BlockEntity::Spawner(Box::new(spawner)));
}

fn zombie_spawner() -> MobSpawner {
    // The flat test level is lit by the sky: custom rules that allow any light.
    let mut data = SpawnData::of_type("minecraft:zombie");
    let mut rules = Compound::new();
    rules.insert(
        "sky_light_limit",
        Compound::new()
            .with("min_inclusive", 0)
            .with("max_inclusive", 15),
    );
    data.0.insert("custom_spawn_rules", rules);
    MobSpawner {
        spawn_delay: 0,
        spawn_potentials: SpawnPotentials(vec![(data.clone(), 1)]),
        next_spawn_data: Some(data),
        ..MobSpawner::default()
    }
}

fn spawner_delay(level: &Level) -> i32 {
    match level.block_entity(SPAWNER) {
        Some(BlockEntity::Spawner(s)) => s.spawn_delay,
        other => panic!("no spawner: {other:?}"),
    }
}

#[test]
fn spawner_spawns_up_to_its_count_and_draws_a_new_delay() {
    let mut level = flat_level();
    place_spawner(&mut level, zombie_spawner());
    add_player(&mut level, Vec3::new(8.5, -60.0, 0.5), GameMode::Survival);
    level.tick_block_entities();
    let spawned = zombies(&level);
    assert!((1..=4).contains(&spawned), "spawned {spawned}");
    let delay = spawner_delay(&level);
    assert!((200..800).contains(&delay), "delay {delay}");
    // The next tick only counts down.
    level.tick_block_entities();
    assert_eq!(spawner_delay(&level), delay - 1);
}

#[test]
fn spawner_waits_for_a_player_in_range() {
    let mut level = flat_level();
    place_spawner(&mut level, zombie_spawner());
    // 16 blocks is the required range: a player 20 blocks away does not count, a spectator
    // never does.
    add_player(&mut level, Vec3::new(20.5, -60.0, 0.5), GameMode::Survival);
    add_player(&mut level, Vec3::new(1.5, -60.0, 0.5), GameMode::Spectator);
    for _ in 0..5 {
        level.tick_block_entities();
    }
    assert_eq!(zombies(&level), 0);
    assert_eq!(spawner_delay(&level), 0);
}

#[test]
fn spawner_stops_at_max_nearby_entities() {
    let mut level = flat_level();
    place_spawner(&mut level, zombie_spawner());
    add_player(&mut level, Vec3::new(8.5, -60.0, 0.5), GameMode::Survival);
    for i in 0..6_i32 {
        let mob = crate::mob::MobData::new(Kind::Zombie, i64::from(i));
        crate::mob::spawn::add_mob(
            &mut level,
            mob,
            Vec3::new(f64::from(i) - 2.5, -60.0, 2.5),
            0.0,
        );
    }
    level.tick_block_entities();
    assert_eq!(zombies(&level), 6);
    assert!(spawner_delay(&level) >= 200);
}

#[test]
fn spawner_nbt_round_trip_keeps_spawn_data() {
    let mut c = Compound::new();
    let mut entity = Compound::new();
    entity.insert("id", Tag::String("minecraft:skeleton".into()));
    let mut data = Compound::new();
    data.insert("entity", entity);
    c.insert("SpawnData", data);
    c.insert("Delay", 20_i16);
    let s = MobSpawner::load(&c);
    assert_eq!(
        s.next_spawn_data.as_ref().and_then(SpawnData::entity_name),
        Some("minecraft:skeleton")
    );
    assert_eq!(s.spawn_potentials.0.len(), 1);
    let mut out = Compound::new();
    s.save(&mut out, true);
    assert_eq!(MobSpawner::load(&out), s);
}

fn trial_state(level: &Level) -> TrialState {
    TrialState::of(level.block(SPAWNER))
}

fn trial_spawner(level: &Level) -> &TrialSpawner {
    match level.block_entity(SPAWNER) {
        Some(BlockEntity::TrialSpawner(s)) => s,
        other => panic!("no trial spawner: {other:?}"),
    }
}

fn despawn_zombies(level: &mut Level) {
    let ids: Vec<EntityId> = level
        .entities
        .mobs
        .iter()
        .filter(|(_, m)| m.kind == Kind::Zombie)
        .map(|(id, _)| id)
        .collect();
    for id in ids {
        level.entities.despawn(id);
    }
}

#[test]
fn trial_spawner_runs_through_its_states() {
    let mut level = flat_level();
    assert!(level.set_block(SPAWNER, ids::TRIAL_SPAWNER.default_state(), update::ALL));
    let config = TrialConfig {
        spawn_potentials: SpawnPotentials(vec![(SpawnData::of_type("minecraft:zombie"), 1)]),
        ..TrialConfig::default()
    };
    let spawner = TrialSpawner {
        normal_config: ConfigRef::Inline(Box::new(config), Compound::new()),
        ..TrialSpawner::default()
    };
    level.set_block_entity(SPAWNER, BlockEntity::TrialSpawner(Box::new(spawner)));
    add_player(&mut level, Vec3::new(4.5, -60.0, 0.5), GameMode::Survival);
    // Inactive → waiting for players → active once the throttled scan sees the player.
    let mut ticks = 0;
    while trial_state(&level) != TrialState::Active {
        tick(&mut level);
        ticks += 1;
        assert!(ticks < 60, "stuck in {:?}", trial_state(&level));
    }
    assert_eq!(trial_spawner(&level).detected_players, vec![0xABCD]);
    // One player: 6 mobs in total, at most 2 at once.
    let mut killed = 0;
    while trial_state(&level) == TrialState::Active {
        tick(&mut level);
        assert!(zombies(&level) <= 2);
        if zombies(&level) == 2 || trial_spawner(&level).total_mobs_spawned == 6 {
            killed += zombies(&level);
            despawn_zombies(&mut level);
        }
        ticks += 1;
        assert!(ticks < 2000, "never finished: {:?}", trial_spawner(&level));
    }
    assert_eq!(killed, 6);
    assert_eq!(trial_state(&level), TrialState::WaitingForRewardEjection);
    // The shutter opens after 40 ticks, the one detected player gets one ejection, then the
    // spawner cools down.
    for _ in 0..100 {
        tick(&mut level);
    }
    assert_eq!(trial_state(&level), TrialState::Cooldown);
    assert!(trial_spawner(&level).detected_players.is_empty());
}
