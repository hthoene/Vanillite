//! Unit tests of the wildlife family.

use vanillite_base::Vec3;
use vanillite_nbt::Compound;

use super::*;
use crate::mob::fam::{FamilyHooks, SpawnContext, SpawnReason};
use crate::mob::goals::Goal as MobGoal;
use crate::survival::rules::Difficulty;

const KINDS: [Kind; 14] = [
    Kind::Bee,
    Kind::Turtle,
    Kind::Mooshroom,
    Kind::Strider,
    Kind::SnowGolem,
    Kind::IronGolem,
    Kind::Bat,
    Kind::Squid,
    Kind::GlowSquid,
    Kind::Dolphin,
    Kind::Cod,
    Kind::Salmon,
    Kind::Pufferfish,
    Kind::TropicalFish,
];

#[test]
fn every_kind_constructs_with_its_goals() {
    for kind in KINDS {
        assert_eq!(kind.family(), crate::mob::Family::Wildlife);
        let m = MobData::new(kind, 7);
        assert!(
            matches!(m.ext, crate::mob::fam::Ext::Wildlife(_)),
            "{kind:?}"
        );
        if kind != Kind::Bat {
            assert!(!m.goals.slots.is_empty(), "{kind:?} has goals");
        }
    }
    // Fish: panic, avoid players, swim; schooling fish also follow a leader.
    let cod = MobData::new(Kind::Cod, 1);
    assert_eq!(cod.goals.slots.len(), 4);
    let puffer = MobData::new(Kind::Pufferfish, 1);
    assert!(puffer.goals.slots.iter().any(|s| matches!(
        s.goal,
        MobGoal::Fam(crate::mob::fam::FamGoal::Wildlife(Goal::Puff))
    )));
    let golem = MobData::new(Kind::IronGolem, 1);
    assert_eq!(golem.targets.slots.len(), 4);
}

#[test]
fn bat_starts_resting_and_squid_has_a_tentacle_speed() {
    assert!(st(&MobData::new(Kind::Bat, 3)).resting);
    let s = st(&MobData::new(Kind::Squid, 3)).tentacle_speed;
    assert!(s > 0.099 && s <= 0.2, "{s}");
}

#[test]
fn finalize_spawn_rolls_variants_like_the_seed() {
    let roll = |kind, seed| {
        let mut m = MobData::new(kind, seed);
        let mut ctx = SpawnContext::simple(Difficulty::Normal, SpawnReason::Natural);
        crate::mob::kinds::finalize_spawn(&mut m, &mut ctx);
        st(&m).variant
    };
    for seed in 0..50 {
        let v = roll(Kind::Salmon, seed);
        assert!((0..=2).contains(&v));
        assert_eq!(v, roll(Kind::Salmon, seed), "deterministic per seed");
        let t = roll(Kind::TropicalFish, seed);
        assert_eq!(t, roll(Kind::TropicalFish, seed));
        // Pattern colour and base colour are dye colours.
        assert!((t >> 16 & 0xFF) < 16 && (t >> 24 & 0xFF) < 16);
    }
}

#[test]
fn tropical_fish_variants_pack_like_vanilla() {
    // Stripey (large, 1), orange (1) on gray (7).
    assert_eq!(fish::common_variant(0), 257 | 1 << 16 | 7 << 24);
    // Kob (small, 0), red (14) on white (0).
    assert_eq!(fish::common_variant(18), 14 << 16);
    assert_eq!(fish::pack_variant(1281, 15, 15), 1281 | 15 << 16 | 15 << 24);
    assert_eq!(fish::DEFAULT_TROPICAL_VARIANT, 0);
}

#[test]
fn pufferfish_puffs_up_and_deflates() {
    let mut s = State {
        inflate_counter: 1,
        ..State::default()
    };
    assert_eq!(fish::puff_transition(&mut s).map(|c| c.0), Some(1));
    s.puff_state = 1;
    for _ in 0..39 {
        assert_eq!(fish::puff_transition(&mut s), None);
    }
    assert_eq!(fish::puff_transition(&mut s).map(|c| c.0), Some(2));
    s.puff_state = 2;
    // The threat is gone: 61 ticks to half, 101 more to small.
    s.inflate_counter = 0;
    s.deflate_timer = 0;
    let mut ticks = 0;
    while fish::puff_transition(&mut s).is_none() {
        ticks += 1;
    }
    assert_eq!(ticks, 61);
    s.puff_state = 1;
    let mut more = 0;
    while fish::puff_transition(&mut s).is_none() {
        more += 1;
    }
    assert_eq!(s.deflate_timer, 102);
    assert_eq!(more, 39);
}

#[test]
fn iron_golem_flings_by_resistance() {
    let v = golem::fling(Vec3::new(0.1, 0.0, -0.2), 0.0);
    assert!((v.y - f64::from(0.4_f32)).abs() < 1e-9);
    assert_eq!(v.x, 0.1);
    let v = golem::fling(Vec3::ZERO, 1.0);
    assert_eq!(v.y, 0.0);
    assert_eq!(golem::crackiness(1.0), 0);
    assert_eq!(golem::crackiness(0.6), 1);
    assert_eq!(golem::crackiness(0.3), 2);
    assert_eq!(golem::crackiness(0.1), 3);
}

fn round_trip(m: &MobData) -> MobData {
    let mut out = Compound::new();
    Hooks::save(m, &mut out);
    let mut n = MobData::new(m.kind, 99);
    Hooks::load(&mut n, &out);
    n
}

#[test]
fn nbt_round_trips() {
    let mut salmon = MobData::new(Kind::Salmon, 1);
    st_mut(&mut salmon).variant = 2;
    st_mut(&mut salmon).from_bucket = true;
    let n = round_trip(&salmon);
    assert_eq!(st(&n).variant, 2);
    assert!(st(&n).from_bucket);

    let mut fish = MobData::new(Kind::TropicalFish, 1);
    st_mut(&mut fish).variant = fish::common_variant(5);
    assert_eq!(st(&round_trip(&fish)).variant, fish::common_variant(5));

    let mut bee = MobData::new(Kind::Bee, 1);
    st_mut(&mut bee).bee_flags = 4 | 8;
    st_mut(&mut bee).flower_pos = Some(vanillite_base::BlockPos::new(1, 64, -3));
    bee.age = -100;
    let n = round_trip(&bee);
    assert_eq!(st(&n).bee_flags, 12);
    assert_eq!(
        st(&n).flower_pos,
        Some(vanillite_base::BlockPos::new(1, 64, -3))
    );
    assert!(n.baby);

    let mut turtle = MobData::new(Kind::Turtle, 1);
    st_mut(&mut turtle).home_pos = vanillite_base::BlockPos::new(10, 62, 10);
    st_mut(&mut turtle).has_egg = true;
    let n = round_trip(&turtle);
    assert_eq!(st(&n).home_pos, vanillite_base::BlockPos::new(10, 62, 10));
    assert!(st(&n).has_egg);

    let mut cow = MobData::new(Kind::Mooshroom, 1);
    st_mut(&mut cow).variant = 1;
    st_mut(&mut cow).stew_effects = Some((crate::mob::effects::id::POISON, 220));
    let n = round_trip(&cow);
    assert_eq!(st(&n).variant, 1);
    assert_eq!(
        st(&n).stew_effects,
        Some((crate::mob::effects::id::POISON, 220))
    );

    let mut golem = MobData::new(Kind::IronGolem, 1);
    st_mut(&mut golem).player_created = true;
    assert!(st(&round_trip(&golem)).player_created);

    let mut snow = MobData::new(Kind::SnowGolem, 1);
    st_mut(&mut snow).pumpkin = false;
    assert!(!st(&round_trip(&snow)).pumpkin);

    let mut bat = MobData::new(Kind::Bat, 1);
    st_mut(&mut bat).resting = false;
    assert!(!st(&round_trip(&bat)).resting);
}

#[test]
fn mooshroom_offspring_keep_a_parent_colour() {
    let mut a = MobData::new(Kind::Mooshroom, 1);
    let b = MobData::new(Kind::Mooshroom, 2);
    st_mut(&mut a).variant = 1;
    let mut random = vanillite_base::random::LegacyRandom::new(5);
    for _ in 0..20 {
        let mut child = MobData::new(Kind::Mooshroom, 3);
        mooshroom::offspring(&a, &b, &mut child, &mut random);
        assert!((0..=1).contains(&st(&child).variant));
    }
    assert_eq!(
        mooshroom::flower_effect("minecraft:allium"),
        Some((crate::mob::effects::id::FIRE_RESISTANCE, 60))
    );
    assert_eq!(mooshroom::flower_effect("minecraft:stone"), None);
}

#[test]
fn kind_properties() {
    assert!(Hooks::ageable(Kind::Dolphin));
    assert!(!Hooks::ageable(Kind::Cod));
    assert!(Hooks::breathes_underwater(Kind::Cod));
    assert!(!Hooks::breathes_underwater(Kind::Dolphin));
    assert!(Hooks::sensitive_to_water(Kind::SnowGolem));
    assert!(Hooks::fall_damage_immune(Kind::Bee));
    assert!(!Hooks::remove_when_far_away(Kind::IronGolem));
    assert!(Hooks::remove_when_far_away(Kind::Squid));
    let mut fish = MobData::new(Kind::Cod, 1);
    assert!(!Hooks::requires_custom_persistence(&fish));
    st_mut(&mut fish).from_bucket = true;
    assert!(Hooks::requires_custom_persistence(&fish));
    let mut puffer = MobData::new(Kind::Pufferfish, 1);
    let small = Hooks::dimensions(&puffer).map(|d| d.width);
    st_mut(&mut puffer).puff_state = 2;
    let big = Hooks::dimensions(&puffer).map(|d| d.width);
    assert!(small < big);
}
