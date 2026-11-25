use vanillite_base::{BlockPos, Vec3};
use vanillite_nbt::Compound;

use super::super::super::MobData;
use super::super::super::kinds::Kind;
use super::super::super::missile::{MissileData, MissileKind};
use super::super::{FamGoal, FamilyHooks, SpawnContext, SpawnReason};
use super::{Goal, Hooks, st, st_mut};
use crate::item::EquipmentSlot;
use crate::player::encode::DataWriter;
use crate::survival::damage::{
    DamageSource, DamageType, DamageTypes, DeathMessageType, Effects, Scaling,
};
use crate::survival::rules::Difficulty;

const KINDS: [Kind; 10] = [
    Kind::Witch,
    Kind::Pillager,
    Kind::Vindicator,
    Kind::Evoker,
    Kind::Illusioner,
    Kind::Vex,
    Kind::Ravager,
    Kind::Ghast,
    Kind::Blaze,
    Kind::Shulker,
];

fn has_goal(m: &MobData, f: impl Fn(&Goal) -> bool) -> bool {
    m.goals.slots.iter().chain(m.targets.slots.iter()).any(
        |s| matches!(s.goal, super::super::super::goals::Goal::Fam(FamGoal::Illager(g)) if f(&g)),
    )
}

#[test]
fn every_kind_has_goals_and_state() {
    for kind in KINDS {
        let m = MobData::new(kind, 7);
        assert!(!m.goals.slots.is_empty(), "{kind:?} has goals");
        assert!(
            matches!(m.ext, super::super::Ext::Illager(_)),
            "{kind:?} state"
        );
    }
    assert!(has_goal(&MobData::new(Kind::Pillager, 1), |g| matches!(
        g,
        Goal::Crossbow(_)
    )));
    assert!(has_goal(&MobData::new(Kind::Evoker, 1), |g| matches!(
        g,
        Goal::Spell(_)
    )));
    assert!(has_goal(&MobData::new(Kind::Ghast, 1), |g| matches!(
        g,
        Goal::GhastShoot { .. }
    )));
    assert!(has_goal(&MobData::new(Kind::Blaze, 1), |g| matches!(
        g,
        Goal::BlazeAttack(_)
    )));
    assert!(has_goal(&MobData::new(Kind::Shulker, 1), |g| matches!(
        g,
        Goal::ShulkerPeek { .. }
    )));
    // Every raider patrols.
    for kind in [
        Kind::Witch,
        Kind::Pillager,
        Kind::Vindicator,
        Kind::Evoker,
        Kind::Ravager,
    ] {
        assert!(
            has_goal(&MobData::new(kind, 2), |g| matches!(g, Goal::Patrol(_))),
            "{kind:?}"
        );
    }
}

#[test]
fn finalize_spawn_equips_weapons() {
    let mut ctx = SpawnContext::simple(Difficulty::Normal, SpawnReason::Natural);
    let mut p = MobData::new(Kind::Pillager, 1);
    super::super::super::kinds::finalize_spawn(
        &mut p,
        &mut ctx,
        &mut vanillite_base::random::LegacyRandom::new(1),
    );
    let main = p.equipment.get(EquipmentSlot::MainHand);
    assert_eq!(crate::item::item_id(main).name(), "minecraft:crossbow");

    let mut v = MobData::new(Kind::Vindicator, 2);
    super::super::super::kinds::finalize_spawn(
        &mut v,
        &mut ctx,
        &mut vanillite_base::random::LegacyRandom::new(1),
    );
    let main = v.equipment.get(EquipmentSlot::MainHand);
    assert_eq!(crate::item::item_id(main).name(), "minecraft:iron_axe");
    assert!(v.nav.can_open_doors);

    let mut x = MobData::new(Kind::Vex, 3);
    super::super::super::kinds::finalize_spawn(
        &mut x,
        &mut ctx,
        &mut vanillite_base::random::LegacyRandom::new(1),
    );
    assert_eq!(x.equipment.drop_chance(EquipmentSlot::MainHand), 0.0);

    let mut i = MobData::new(Kind::Illusioner, 4);
    super::super::super::kinds::finalize_spawn(
        &mut i,
        &mut ctx,
        &mut vanillite_base::random::LegacyRandom::new(1),
    );
    let main = i.equipment.get(EquipmentSlot::MainHand);
    assert_eq!(crate::item::item_id(main).name(), "minecraft:bow");
}

#[test]
fn captains_carry_the_banner() {
    // Some seed rolls the 6 % captain chance.
    let mut found = false;
    for seed in 0..400 {
        let mut p = MobData::new(Kind::Vindicator, seed);
        let mut ctx = SpawnContext::simple(Difficulty::Normal, SpawnReason::Natural);
        super::super::super::kinds::finalize_spawn(
            &mut p,
            &mut ctx,
            &mut vanillite_base::random::LegacyRandom::new(1),
        );
        if st(&p).patrol_leader {
            let head = p.equipment.get(EquipmentSlot::Head);
            assert_eq!(crate::item::item_id(head).name(), "minecraft:white_banner");
            assert_eq!(p.equipment.drop_chance(EquipmentSlot::Head), 2.0);
            found = true;
            break;
        }
    }
    assert!(found);
    // Witches and ravagers never lead.
    for seed in 0..200 {
        let mut w = MobData::new(Kind::Ravager, seed);
        let mut ctx = SpawnContext::simple(Difficulty::Normal, SpawnReason::Natural);
        super::super::super::kinds::finalize_spawn(
            &mut w,
            &mut ctx,
            &mut vanillite_base::random::LegacyRandom::new(1),
        );
        assert!(!st(&w).patrol_leader);
    }
}

#[test]
fn patrol_spawns_make_patrollers_and_witches_stay_out_of_raids() {
    let mut p = MobData::new(Kind::Pillager, 4);
    let mut ctx = SpawnContext::simple(Difficulty::Hard, SpawnReason::Patrol);
    super::super::super::kinds::finalize_spawn(
        &mut p,
        &mut ctx,
        &mut vanillite_base::random::LegacyRandom::new(1),
    );
    assert!(st(&p).patrolling);
    assert!(st(&p).can_join_raid);
    let mut w = MobData::new(Kind::Witch, 5);
    let mut ctx = SpawnContext::simple(Difficulty::Hard, SpawnReason::Natural);
    super::super::super::kinds::finalize_spawn(
        &mut w,
        &mut ctx,
        &mut vanillite_base::random::LegacyRandom::new(1),
    );
    assert!(!st(&w).can_join_raid);
}

#[test]
fn nbt_round_trip() {
    let mut s = MobData::new(Kind::Shulker, 9);
    {
        let x = st_mut(&mut s);
        x.attach_face = 3;
        x.peek = 30;
        x.color = 5;
    }
    let mut out = Compound::new();
    Hooks::save(&s, &mut out);
    let mut l = MobData::new(Kind::Shulker, 10);
    Hooks::load(&mut l, &out);
    assert_eq!(st(&l).attach_face, 3);
    assert_eq!(st(&l).peek, 30);
    assert_eq!(st(&l).color, 5);

    let mut r = MobData::new(Kind::Ravager, 11);
    st_mut(&mut r).roar_tick = 12;
    let mut out = Compound::new();
    Hooks::save(&r, &mut out);
    let mut l = MobData::new(Kind::Ravager, 12);
    Hooks::load(&mut l, &out);
    assert_eq!(st(&l).roar_tick, 12);
    assert!(Hooks::is_immobile(&l));

    let mut v = MobData::new(Kind::Vex, 13);
    {
        let x = st_mut(&mut v);
        x.owner = Some(0x0123_4567_89ab_cdef_0011_2233_4455_6677);
        x.bound_origin = Some(BlockPos::new(1, 2, 3));
        x.life_ticks = Some(600);
    }
    let mut out = Compound::new();
    Hooks::save(&v, &mut out);
    let mut l = MobData::new(Kind::Vex, 14);
    Hooks::load(&mut l, &out);
    assert_eq!(st(&l).owner, st(&v).owner);
    assert_eq!(st(&l).bound_origin, Some(BlockPos::new(1, 2, 3)));
    assert_eq!(st(&l).life_ticks, Some(600));

    let mut p = MobData::new(Kind::Pillager, 15);
    {
        let x = st_mut(&mut p);
        x.patrol_target = Some(BlockPos::new(-5, 70, 9));
        x.patrol_leader = true;
        x.patrolling = true;
    }
    let mut out = Compound::new();
    Hooks::save(&p, &mut out);
    let mut l = MobData::new(Kind::Pillager, 16);
    Hooks::load(&mut l, &out);
    assert_eq!(st(&l).patrol_target, Some(BlockPos::new(-5, 70, 9)));
    assert!(st(&l).patrol_leader && st(&l).patrolling);
    assert!(l.can_pick_up_loot);
}

fn written_ids(m: &MobData, all: bool) -> Vec<u8> {
    let mut out = Vec::new();
    Hooks::encode_data(&mut DataWriter::new(&mut out, all), m);
    // Each item: id, var int serializer (all ids here are below 128), value.
    let mut ids = Vec::new();
    let mut i = 0;
    while i < out.len() {
        ids.push(out[i]);
        let serializer = out[i + 1];
        i += 2;
        i += match serializer {
            0 | 8 => 1,
            1 | 12 => {
                let mut n = 0;
                while out[i + n] & 0x80 != 0 {
                    n += 1;
                }
                n + 1
            }
            _ => panic!("unexpected serializer {serializer}"),
        };
    }
    ids
}

#[test]
fn synched_data_ids() {
    assert_eq!(
        written_ids(&MobData::new(Kind::Witch, 1), true),
        vec![16, 17]
    );
    assert_eq!(
        written_ids(&MobData::new(Kind::Pillager, 1), true),
        vec![16, 17]
    );
    assert_eq!(
        written_ids(&MobData::new(Kind::Evoker, 1), true),
        vec![16, 17]
    );
    assert_eq!(written_ids(&MobData::new(Kind::Ravager, 1), true), vec![16]);
    assert_eq!(written_ids(&MobData::new(Kind::Vex, 1), true), vec![16]);
    assert_eq!(written_ids(&MobData::new(Kind::Ghast, 1), true), vec![16]);
    assert_eq!(written_ids(&MobData::new(Kind::Blaze, 1), true), vec![16]);
    assert_eq!(
        written_ids(&MobData::new(Kind::Shulker, 1), true),
        vec![16, 17, 18]
    );
    // Pairing skips the defaults.
    assert!(written_ids(&MobData::new(Kind::Shulker, 1), false).is_empty());
}

fn types_with_magic() -> DamageTypes {
    let t = |id: &str| DamageType {
        id: id.to_owned(),
        msg_id: "x".to_owned(),
        scaling: Scaling::Never,
        exhaustion: 0.0,
        effects: Effects::Hurt,
        death_message_type: DeathMessageType::Default,
        tags: 0,
    };
    DamageTypes::new(vec![
        t("minecraft:generic"),
        t("minecraft:magic"),
        t("minecraft:thorns"),
    ])
}

#[test]
fn witch_resists_magic() {
    let types = types_with_magic();
    let magic = DamageSource::of(types.magic);
    let generic = DamageSource::of(types.generic);
    assert!((super::witch::absorb(&magic, &types, 10.0) - 1.5).abs() < 1.0e-5);
    assert!((super::witch::absorb(&generic, &types, 10.0) - 10.0).abs() < 1.0e-5);
}

#[test]
fn fireball_flight_and_bullet_shapes() {
    let v = super::missiles::fireball_inertia(Vec3::new(0.1, 0.0, 0.0), 0.1, false);
    assert!((v.x - 0.19).abs() < 1.0e-6);
    let still = super::missiles::fireball_inertia(Vec3::ZERO, 0.1, false);
    assert_eq!(still, Vec3::ZERO);
    let wet = super::missiles::fireball_inertia(Vec3::new(0.0, 0.0, 0.1), 0.1, true);
    assert!((wet.z - 0.16).abs() < 1.0e-6);
    // A shulker on the floor opens upwards.
    let b = super::shulker::progress_aabb(0, 1.0, Vec3::new(0.5, 64.0, 0.5));
    assert!((b.max_y - 66.0).abs() < 1.0e-9 && (b.min_y - 64.0).abs() < 1.0e-9);
    // On a wall it opens away from it.
    let b = super::shulker::progress_aabb(4, 0.5, Vec3::new(0.5, 64.0, 0.5));
    assert!((b.max_x - 1.5).abs() < 1.0e-9 && (b.min_x - 0.0).abs() < 1.0e-9);
    let d = MissileData::new(MissileKind::Fireball);
    let mut out = Compound::new();
    super::missiles::save(&d, &mut out);
    let mut l = MissileData::new(MissileKind::Fireball);
    super::missiles::load(&mut l, &out);
    assert_eq!(l.explosion_power, 1);
    assert!((l.acceleration - 0.1).abs() < 1.0e-12);
}

#[test]
fn kind_properties() {
    let g = MobData::new(Kind::Ghast, 13);
    assert_eq!(Hooks::sound_volume(Kind::Ghast), 5.0);
    assert_eq!(st(&g).explosion_power, 1);
    assert!(Hooks::fall_damage_immune(Kind::Blaze));
    assert!(Hooks::sensitive_to_water(Kind::Blaze));
    let shulker = MobData::new(Kind::Shulker, 1);
    assert_eq!(Hooks::ambient_sound(&shulker), None);
    assert_eq!(
        Hooks::hurt_sound(&shulker),
        "minecraft:entity.shulker.hurt_closed"
    );
    let mut evoker = MobData::new(Kind::Evoker, 2);
    let xp = Hooks::experience_reward(&mut evoker);
    assert_eq!(xp, 10);
}
