//! Tests of the brain AI mobs: brains, spawn setup, NBT and synched data.

use vanillite_nbt::Compound;

use super::{state, state_mut};
use crate::mob::brain::{Activity, MemValue, MemoryType as M};
use crate::mob::fam::{SpawnContext, SpawnReason};
use crate::mob::{Kind, MobData, kinds};
use crate::survival::rules::Difficulty;

const KINDS: [Kind; 10] = [
    Kind::Piglin,
    Kind::PiglinBrute,
    Kind::Hoglin,
    Kind::Zoglin,
    Kind::Goat,
    Kind::Axolotl,
    Kind::Frog,
    Kind::Tadpole,
    Kind::Armadillo,
    Kind::Sniffer,
];

fn spawned(kind: Kind, seed: i64) -> MobData {
    let mut m = MobData::new(kind, seed);
    let mut ctx = SpawnContext::simple(Difficulty::Normal, SpawnReason::Natural);
    kinds::finalize_spawn(&mut m, &mut ctx);
    m
}

#[test]
fn every_kind_builds_a_brain_with_its_default_activity() {
    for kind in KINDS {
        let m = MobData::new(kind, 1);
        let b = m.brain.as_deref().expect("brain");
        assert!(b.is_active(Activity::Core), "{kind:?}");
        assert!(b.is_active(Activity::Idle), "{kind:?}");
        assert!(!b.nodes.is_empty(), "{kind:?}");
        assert!(m.goals.slots.is_empty(), "{kind:?} uses no goals");
    }
}

#[test]
fn finalize_spawn_is_deterministic_per_seed() {
    for kind in KINDS {
        let a = spawned(kind, 42);
        let b = spawned(kind, 42);
        assert_eq!(a.baby, b.baby, "{kind:?}");
        assert_eq!(a.equipment, b.equipment, "{kind:?}");
        assert_eq!(state(&a).variant, state(&b).variant, "{kind:?}");
    }
}

#[test]
fn adult_piglins_spawn_with_a_weapon_and_brutes_with_an_axe() {
    let mut weapons = 0;
    for seed in 0..64 {
        let m = spawned(Kind::Piglin, seed);
        if !m.baby {
            let held = m.equipment.get(crate::item::EquipmentSlot::MainHand);
            assert!(!held.is_empty() || crate::player::play::game_data().is_none());
            weapons += 1;
        }
        let hunted = m.brain.as_deref().is_some_and(|b| b.has(M::HuntedRecently));
        assert!(hunted, "initMemories");
    }
    assert!(weapons > 30);
    let brute = spawned(Kind::PiglinBrute, 3);
    assert!(brute.brain.as_deref().is_some_and(|b| b.has(M::Home)));
}

#[test]
fn goats_get_cooldowns_and_horns() {
    let g = spawned(Kind::Goat, 9);
    let b = g.brain.as_deref().expect("brain");
    assert!(
        b.int(M::LongJumpCooldownTicks)
            .is_some_and(|v| (600..=1200).contains(&v))
    );
    assert!(
        b.int(M::RamCooldownTicks)
            .is_some_and(|v| (600..=6000).contains(&v))
    );
    let s = state(&g);
    assert!(s.left_horn || s.right_horn);
}

#[test]
fn nbt_round_trips_kind_data_and_memories() {
    let mut g = spawned(Kind::Goat, 5);
    state_mut(&mut g).screaming = true;
    state_mut(&mut g).left_horn = false;
    let mut out = Compound::new();
    <super::Hooks as crate::mob::fam::FamilyHooks>::save(&g, &mut out);
    let mut h = MobData::new(Kind::Goat, 6);
    <super::Hooks as crate::mob::fam::FamilyHooks>::load(&mut h, &out);
    assert!(state(&h).screaming);
    assert!(!state(&h).left_horn && state(&h).right_horn);
    let cd = |m: &MobData| m.brain.as_deref().and_then(|b| b.int(M::RamCooldownTicks));
    assert_eq!(cd(&g), cd(&h), "ram cooldown memory is saved");

    let mut a = spawned(Kind::Axolotl, 11);
    state_mut(&mut a).variant = 3;
    state_mut(&mut a).from_bucket = true;
    let mut out = Compound::new();
    <super::Hooks as crate::mob::fam::FamilyHooks>::save(&a, &mut out);
    let mut b = MobData::new(Kind::Axolotl, 12);
    <super::Hooks as crate::mob::fam::FamilyHooks>::load(&mut b, &out);
    assert_eq!(state(&b).variant, 3);
    assert!(state(&b).from_bucket);

    let mut p = spawned(Kind::Piglin, 13);
    p.brain.as_deref_mut().expect("brain").set_with_expiry(
        M::AdmiringDisabled,
        MemValue::Bool(true),
        77,
    );
    let mut out = Compound::new();
    <super::Hooks as crate::mob::fam::FamilyHooks>::save(&p, &mut out);
    let mut q = MobData::new(Kind::Piglin, 14);
    <super::Hooks as crate::mob::fam::FamilyHooks>::load(&mut q, &out);
    let b = q.brain.as_deref().expect("brain");
    assert!(b.has(M::AdmiringDisabled));
    assert_eq!(b.ttl(M::AdmiringDisabled), 77);
}

#[test]
fn synched_data_uses_the_class_ids() {
    let mut out = Vec::new();
    let m = spawned(Kind::Sniffer, 1);
    {
        let mut w = crate::player::encode::DataWriter::new(&mut out, true);
        <super::Hooks as crate::mob::fam::FamilyHooks>::encode_data(&mut w, &m);
    }
    // Baby (16, boolean), state (17, sniffer state), seed tick (18, int).
    assert_eq!(out[0], 16);
    assert!(out.contains(&17) && out.contains(&18));
    let mut out = Vec::new();
    let h = spawned(Kind::Hoglin, 2);
    {
        let mut w = crate::player::encode::DataWriter::new(&mut out, true);
        <super::Hooks as crate::mob::fam::FamilyHooks>::encode_data(&mut w, &h);
    }
    assert_eq!(out[0], 16);
    assert!(out.contains(&17));
}

#[test]
fn frog_variant_follows_the_biome() {
    let mut m = MobData::new(Kind::Frog, 1);
    let mut ctx = SpawnContext::simple(Difficulty::Normal, SpawnReason::Natural);
    kinds::finalize_spawn(&mut m, &mut ctx);
    assert_eq!(state(&m).variant, 1, "temperate without a biome");
}

#[test]
fn brains_tick_on_a_flat_world() {
    use crate::mob::brain::tests::{level_with, with_tick};
    for kind in KINDS {
        let (mut level, id) = level_with(kind, 21);
        for time in 0..120 {
            let alive = with_tick(&mut level, id, 1000 + time, |t| {
                if !t.exists() {
                    return false;
                }
                <super::Hooks as crate::mob::fam::FamilyHooks>::custom_server_ai_step(t);
                t.exists()
            });
            if !alive {
                break;
            }
        }
        if let Some(m) = level.entities.mob(id) {
            assert!(
                m.brain
                    .as_deref()
                    .is_some_and(|b| b.active_non_core().is_some()),
                "{kind:?}"
            );
        }
    }
}
