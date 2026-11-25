//! Tests of the companion family.

use vanillite_base::random::{LegacyRandom, RandomSource};
use vanillite_nbt::Compound;

use super::*;
use crate::mob::fam::{FamilyHooks, SpawnContext, SpawnReason};
use crate::survival::rules::Difficulty;

const KINDS: [Kind; 15] = [
    Kind::Horse,
    Kind::Donkey,
    Kind::Mule,
    Kind::SkeletonHorse,
    Kind::ZombieHorse,
    Kind::Llama,
    Kind::TraderLlama,
    Kind::Wolf,
    Kind::Cat,
    Kind::Ocelot,
    Kind::Parrot,
    Kind::Fox,
    Kind::Panda,
    Kind::PolarBear,
    Kind::Rabbit,
];

#[test]
fn every_kind_has_goals_and_state() {
    for kind in KINDS {
        let m = MobData::new(kind, 7);
        assert_eq!(kind.family(), crate::mob::Family::Companion, "{kind:?}");
        assert!(!m.goals.slots.is_empty(), "{kind:?} has no goals");
        assert!(
            matches!(m.ext, crate::mob::fam::Ext::Companion(_)),
            "{kind:?}"
        );
        assert!(kind.ageable());
    }
}

#[test]
fn finalize_spawn_is_deterministic_per_seed() {
    for kind in KINDS {
        let run = || {
            let mut m = MobData::new(kind, 42);
            let mut ctx = SpawnContext::simple(Difficulty::Normal, SpawnReason::Natural);
            crate::mob::kinds::finalize_spawn(
                &mut m,
                &mut ctx,
                &mut vanillite_base::random::LegacyRandom::new(1),
            );
            (
                m.variant,
                m.attrs.max_health.to_bits(),
                st(&m).strength,
                st(&m).main_gene,
            )
        };
        assert_eq!(run(), run(), "{kind:?}");
    }
}

#[test]
fn horse_variant_and_attributes_are_in_range() {
    for seed in 0..50 {
        let mut m = MobData::new(Kind::Horse, seed);
        let mut ctx = SpawnContext::simple(Difficulty::Normal, SpawnReason::Natural);
        horse::finalize_spawn(&mut m, &mut ctx);
        assert!((0..7).contains(&(m.variant & 0xFF)));
        assert!((0..5).contains(&(m.variant >> 8)));
        assert!((15.0..=30.0).contains(&m.attrs.max_health));
        assert!(m.attrs.jump_strength >= 0.4 && m.attrs.jump_strength <= 1.0);
    }
}

#[test]
fn horse_and_donkey_breed_a_mule() {
    let horse = MobData::new(Kind::Horse, 1);
    let donkey = MobData::new(Kind::Donkey, 2);
    assert_eq!(horse::offspring_kind(&horse, &donkey), Kind::Mule);
    assert_eq!(horse::offspring_kind(&donkey, &horse), Kind::Mule);
    assert_eq!(horse::offspring_kind(&horse, &horse), Kind::Horse);
}

#[test]
fn foal_colours_come_from_the_parents_mostly() {
    let mut a = MobData::new(Kind::Horse, 1);
    let mut b = MobData::new(Kind::Horse, 2);
    a.variant = 2 | (1 << 8);
    b.variant = 5 | (3 << 8);
    let mut from_parent = 0;
    for seed in 0..200 {
        let mut child = MobData::new(Kind::Horse, seed);
        let mut r = LegacyRandom::new(seed);
        horse::breed_offspring(&a, &b, &mut child, &mut r);
        let v = child.variant & 0xFF;
        if v == 2 || v == 5 {
            from_parent += 1;
        }
        assert!((15.0..=30.0).contains(&child.attrs.max_health));
    }
    // Eight in nine inherit a parent's colour, plus the random hits.
    assert!(from_parent > 150, "{from_parent}");
}

#[test]
fn panda_genes_follow_the_recessive_rule() {
    use panda::gene;
    assert_eq!(panda::variant(gene::BROWN, gene::BROWN), gene::BROWN);
    assert_eq!(panda::variant(gene::BROWN, gene::LAZY), gene::NORMAL);
    assert_eq!(panda::variant(gene::LAZY, gene::BROWN), gene::LAZY);
    let mut r = LegacyRandom::new(3);
    for _ in 0..100 {
        assert!(panda::random_gene(&mut r) <= gene::AGGRESSIVE);
    }
}

#[test]
fn panda_child_genes_come_from_the_parents() {
    use panda::gene;
    let mut a = MobData::new(Kind::Panda, 1);
    let mut b = MobData::new(Kind::Panda, 2);
    st_mut(&mut a).main_gene = gene::LAZY;
    st_mut(&mut a).hidden_gene = gene::LAZY;
    st_mut(&mut b).main_gene = gene::WORRIED;
    st_mut(&mut b).hidden_gene = gene::WORRIED;
    let mut inherited = 0;
    for seed in 0..64 {
        let mut child = MobData::new(Kind::Panda, 3);
        let mut r = LegacyRandom::new(seed);
        panda::breed_offspring(&a, &b, &mut child, &mut r);
        let s = st(&child);
        if matches!(s.main_gene, gene::LAZY | gene::WORRIED)
            && matches!(s.hidden_gene, gene::LAZY | gene::WORRIED)
        {
            inherited += 1;
        }
    }
    assert!(inherited > 50, "{inherited}");
}

#[test]
fn nbt_round_trips() {
    for kind in KINDS {
        let mut m = MobData::new(kind, 11);
        let mut ctx = SpawnContext::simple(Difficulty::Normal, SpawnReason::Natural);
        crate::mob::kinds::finalize_spawn(
            &mut m,
            &mut ctx,
            &mut vanillite_base::random::LegacyRandom::new(1),
        );
        m.tame = matches!(kind, Kind::Horse | Kind::Wolf);
        st_mut(&mut m).temper = 17;
        st_mut(&mut m).collar = 3;
        let mut out = Compound::new();
        <Hooks as FamilyHooks>::save(&m, &mut out);
        let mut n = MobData::new(kind, 12);
        <Hooks as FamilyHooks>::load(&mut n, &out);
        assert_eq!(n.variant, m.variant, "{kind:?}");
        if is_horse(kind) {
            assert_eq!(st(&n).temper, 17);
            assert_eq!(n.tame, m.tame);
        }
        if matches!(kind, Kind::Wolf | Kind::Cat) {
            assert_eq!(st(&n).collar, 3);
            assert_eq!(st(&n).sound_variant, st(&m).sound_variant);
        }
        if kind == Kind::Panda {
            assert_eq!(st(&n).main_gene, st(&m).main_gene);
            assert_eq!(st(&n).hidden_gene, st(&m).hidden_gene);
        }
    }
}

#[test]
fn encode_starts_with_the_baby_flag_and_writes_the_kind_ids() {
    for (kind, last) in [
        (Kind::Horse, 19u8),
        (Kind::Llama, 21),
        (Kind::Wolf, 24),
        (Kind::Cat, 24),
        (Kind::Fox, 21),
        (Kind::Panda, 23),
        (Kind::Rabbit, 18),
        (Kind::Parrot, 20),
    ] {
        let m = MobData::new(kind, 5);
        let mut out = Vec::new();
        let mut w = crate::player::encode::DataWriter::all_for_test(&mut out);
        <Hooks as FamilyHooks>::encode_data(&mut w, &m);
        assert_eq!(out.first(), Some(&16), "{kind:?}");
        assert!(out.contains(&last), "{kind:?} lacks {last}");
    }
}

#[test]
fn wolf_variant_follows_the_biome() {
    let forest = crate::mob::spawn::BiomeSpawns {
        name: "minecraft:forest".into(),
        ..Default::default()
    };
    let mut ctx = SpawnContext::simple(Difficulty::Normal, SpawnReason::Natural);
    ctx.biome = Some(&forest);
    assert_eq!(wolf::variant_for(&ctx), 8);
    let savanna = crate::mob::spawn::BiomeSpawns {
        name: "minecraft:savanna".into(),
        tags: vec!["minecraft:is_savanna".into()],
        ..Default::default()
    };
    let mut ctx = SpawnContext::simple(Difficulty::Normal, SpawnReason::Natural);
    ctx.biome = Some(&savanna);
    assert_eq!(wolf::variant_for(&ctx), 6);
    let ctx = SpawnContext::simple(Difficulty::Normal, SpawnReason::Natural);
    assert_eq!(wolf::variant_for(&ctx), wolf::VARIANT_PALE);
}

#[test]
fn snow_foxes_spawn_in_snowy_biomes() {
    let snowy = crate::mob::spawn::BiomeSpawns {
        name: "minecraft:snowy_taiga".into(),
        tags: vec!["minecraft:spawns_snow_foxes".into()],
        ..Default::default()
    };
    let mut ctx = SpawnContext::simple(Difficulty::Normal, SpawnReason::Natural);
    ctx.biome = Some(&snowy);
    let mut m = MobData::new(Kind::Fox, 1);
    fox::finalize_spawn(&mut m, &mut ctx);
    assert_eq!(m.variant, 1);
}

#[test]
fn cat_variant_pick() {
    let mut r = LegacyRandom::new(1);
    assert_eq!(feline::pick_cat_variant(&mut r, true, false), 0);
    for _ in 0..100 {
        let v = feline::pick_cat_variant(&mut r, false, false);
        assert!((1..=10).contains(&v));
    }
}

#[test]
fn taming_rolls_one_in_three() {
    let mut r = LegacyRandom::new(1234);
    let tamed = (0..3000).filter(|_| r.next_int_bounded(3) == 0).count();
    assert!((900..1100).contains(&tamed), "{tamed}");
}

#[test]
fn offspring_attribute_stays_in_range() {
    let mut r = LegacyRandom::new(5);
    for _ in 0..500 {
        let v = horse::offspring_attribute(15.0, 30.0, 15.0, 30.0, &mut r);
        assert!((15.0..=30.0).contains(&v));
    }
}

#[test]
fn wolf_taming_side_effects() {
    let mut m = MobData::new(Kind::Wolf, 1);
    m.tame = true;
    wolf::apply_taming_side_effects(&mut m);
    assert!((m.health - 40.0).abs() < f32::EPSILON);
}
