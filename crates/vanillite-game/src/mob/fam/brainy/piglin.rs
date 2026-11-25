//! Piglins and piglin brutes (`AbstractPiglin`, `Piglin`, `PiglinAi`, `PiglinBrute`,
//! `PiglinBruteAi`, `PiglinSpecificSensor`, `PiglinBruteSpecificSensor` and the piglin
//! behaviours): gold admiring and bartering, anger at players without gold armour, hoglin
//! hunts, retreats, celebrations, riding baby hoglins, crossbows and spears, and the
//! zombification outside the Nether.

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Vec3};
use vanillite_config::GameMode;
use vanillite_nbt::{Compound, List, Tag};
use vanillite_protocol::packets::play::clientbound::SoundSource;
use vanillite_world::entity::EntityId;

use super::hoglin::with_other;
use super::{
    SpawnContext, SpawnReason, convert_to, make_sound, nbt_bool, nbt_int, spec, state, state_mut,
};
use crate::item::{EquipmentSlot, ItemStack};
use crate::mob::animal::Interaction;
use crate::mob::brain::behavior::{self as bh, Shape, Spec, for_manhattan};
use crate::mob::brain::memory::MemoryStatus as S;
use crate::mob::brain::{
    self, Activity, ActivitySpec, Brain, BrainSpec, GlobalPos, MemValue, MemoryStatus,
    MemoryType as M, SensorKind, Tracker, brain_ref, sensing,
};
use crate::mob::equipment::{self, PickupRules};
use crate::mob::fam::Interact;
use crate::mob::path::{MalusTable, PathType};
use crate::mob::{Kind, MobData, Tick, nav, randompos};
use crate::player::encode::DataWriter;
use crate::survival::Effect;
use crate::survival::rules::{Difficulty, Rule};

/// `AbstractPiglin`'s fire maluses.
pub const MALUS: &MalusTable = &[(PathType::FireInNeighbor, 16.0), (PathType::Fire, -1.0)];

/// `PiglinSpecificSensor.requires`.
pub const SPECIFIC_SENSOR_MEMORIES: &[M] = &[
    M::NearestVisibleLivingEntities,
    M::NearestLivingEntities,
    M::NearestVisibleNemesis,
    M::NearestTargetablePlayerNotWearingGold,
    M::NearestPlayerHoldingWantedItem,
    M::NearestVisibleHuntableHoglin,
    M::NearestVisibleBabyHoglin,
    M::NearestVisibleAdultPiglins,
    M::NearbyAdultPiglins,
    M::VisibleAdultPiglinCount,
    M::VisibleAdultHoglinCount,
    M::NearestRepellent,
];

/// `PiglinBruteSpecificSensor.requires`.
pub const BRUTE_SENSOR_MEMORIES: &[M] = &[
    M::NearestVisibleLivingEntities,
    M::NearestVisibleNemesis,
    M::NearbyAdultPiglins,
];

/// The piglin behaviours.
#[derive(Clone, Copy, Debug)]
pub enum Behavior {
    StopHoldingItemIfNoLongerAdmiring,
    StartAdmiringItemIfSeen {
        duration: i32,
    },
    StartCelebratingIfTargetDead {
        duration: i32,
    },
    StartHuntingHoglin,
    RememberIfHoglinWasKilled,
    StopAdmiringIfItemTooFarAway {
        distance: i32,
    },
    StopAdmiringIfTired {
        max: i32,
        disable: i32,
    },
    /// `babySometimesRideBabyHoglin`: `CopyMemoryWithExpiry` with its ticker.
    BabyRideBabyHoglin {
        ticks: i32,
    },
    /// `CrossbowAttack`: state 0 uncharged, 1 charging, 2 charged, 3 ready.
    CrossbowAttack {
        state: u8,
        delay: i32,
    },
    SpearApproach {
        speed: f64,
        distance_sqr: f32,
    },
    SpearAttack {
        charge: f64,
        reposition: f64,
        radius_sqr: f32,
    },
    SpearRetreat {
        speed: f64,
    },
}

fn p(b: Behavior) -> Spec {
    spec(super::Behavior::Piglin(b))
}

fn is_adult(t: &mut Tick<'_, '_>) -> bool {
    !t.mob_ref().baby
}

fn is_baby(t: &mut Tick<'_, '_>) -> bool {
    t.mob_ref().baby
}

fn is_player(t: &Tick<'_, '_>, id: EntityId) -> bool {
    t.ents.player(id).is_some()
}

fn is_piglin(t: &Tick<'_, '_>, id: EntityId) -> bool {
    t.ents.mob(id).is_some_and(|m| m.kind == Kind::Piglin)
}

fn is_brute(t: &Tick<'_, '_>, id: EntityId) -> bool {
    t.ents.mob(id).is_some_and(|m| m.kind == Kind::PiglinBrute)
}

/// `PiglinAi.isPlayerHoldingLovedItem`.
pub fn player_holding_loved(t: &Tick<'_, '_>, id: EntityId) -> bool {
    t.ents.player(id).is_some_and(|p| {
        [EquipmentSlot::MainHand, EquipmentSlot::OffHand]
            .iter()
            .any(|&s| is_loved(p.inventory.equipment(s)))
    })
}

/// `PiglinAi.isLovedItem`.
pub fn is_loved(stack: &ItemStack) -> bool {
    !stack.is_empty() && super::item_in_tag(stack.item, "minecraft:piglin_loved")
}

fn is_barter_currency(stack: &ItemStack) -> bool {
    super::is_item(stack, "minecraft:gold_ingot")
}

fn look_behaviors() -> Vec<(Spec, i32)> {
    vec![
        (Spec::look_at_entity(Some(is_player), 8.0), 1),
        (Spec::look_at_entity(Some(is_piglin), 8.0), 1),
        (Spec::look_at_entity(None, 8.0), 1),
    ]
}

fn idle_look() -> Spec {
    let mut list = look_behaviors();
    list.push((Spec::do_nothing(30, 60), 1));
    Spec::run_one(list)
}

fn idle_movement() -> Spec {
    Spec::run_one(vec![
        (Spec::stroll(0.6), 2),
        (
            Spec::of(bh::Behavior::InteractWith {
                filter: is_piglin,
                range: 8,
                memory: M::InteractionTarget,
                speed: 0.6,
                stop: 2,
            }),
            2,
        ),
        (
            Spec::trigger_if(doesnt_see_loved_holder, Spec::walk_to_look_target(0.6, 3)),
            2,
        ),
        (Spec::do_nothing(30, 60), 1),
    ])
}

fn piglin_brain() -> BrainSpec {
    let core = ActivitySpec::new(
        Activity::Core,
        0,
        vec![
            Spec::look_at_target_sink(45, 90),
            Spec::move_to_target_sink(),
            Spec::copy_with_expiry(is_baby, M::NearestVisibleNemesis, M::AvoidTarget, 100, 140),
            Spec::copy_with_expiry(
                is_near_zombified,
                M::NearestVisibleZombified,
                M::AvoidTarget,
                100,
                140,
            ),
            p(Behavior::StopHoldingItemIfNoLongerAdmiring),
            p(Behavior::StartAdmiringItemIfSeen { duration: 119 }),
            p(Behavior::StartCelebratingIfTargetDead { duration: 300 }),
            Spec::of(bh::Behavior::StopBeingAngryIfTargetDead),
        ],
    );
    let idle = ActivitySpec::new(
        Activity::Idle,
        10,
        vec![
            Spec::look_at_entity(Some(player_holding_loved), 14.0),
            Spec::start_attacking(Some(is_adult), find_attack_target),
            Spec::trigger_if(can_hunt, p(Behavior::StartHuntingHoglin)),
            Spec::walk_away_from(M::NearestRepellent, 1.0, 8, false),
            p(Behavior::BabyRideBabyHoglin { ticks: 0 }),
            idle_look(),
            idle_movement(),
            Spec::of(bh::Behavior::SetLookAndInteract {
                filter: is_player,
                range: 4,
            }),
        ],
    );
    let admire = ActivitySpec::with_memory(
        Activity::AdmireItem,
        10,
        vec![
            Spec::go_to_wanted_item(Some(not_holding_loved_in_off_hand), 1.0, true, 9),
            p(Behavior::StopAdmiringIfItemTooFarAway { distance: 9 }),
            p(Behavior::StopAdmiringIfTired {
                max: 200,
                disable: 200,
            }),
        ],
        M::AdmiringItem,
    );
    let fight = ActivitySpec::with_memory(
        Activity::Fight,
        10,
        vec![
            Spec::of(bh::Behavior::StopAttackingIfTargetInvalid {
                stop_when: Some(not_nearest_valid_target),
                on_erased: None,
                tired: true,
            }),
            Spec::trigger_if(
                has_crossbow,
                Spec::of(bh::Behavior::BackUpIfTooClose {
                    distance: 5,
                    speed: 0.75,
                }),
            ),
            Spec::walk_to_attack_target(1.0),
            p(Behavior::SpearApproach {
                speed: 1.0,
                distance_sqr: 100.0,
            }),
            p(Behavior::SpearAttack {
                charge: 1.0,
                reposition: 1.0,
                radius_sqr: 4.0,
            }),
            p(Behavior::SpearRetreat { speed: 1.0 }),
            Spec::melee(20),
            p(Behavior::CrossbowAttack { state: 0, delay: 0 }),
            p(Behavior::RememberIfHoglinWasKilled),
            Spec::erase_if(is_near_zombified, M::AttackTarget),
        ],
        M::AttackTarget,
    );
    let celebrate = ActivitySpec::with_memory(
        Activity::Celebrate,
        10,
        vec![
            Spec::walk_away_from(M::NearestRepellent, 1.0, 8, false),
            Spec::look_at_entity(Some(player_holding_loved), 14.0),
            Spec::start_attacking(Some(is_adult), find_attack_target),
            Spec::trigger_if(
                not_dancing,
                Spec::of(bh::Behavior::GoToTargetLocation {
                    memory: M::CelebrateLocation,
                    close: 2,
                    speed: 1.0,
                }),
            ),
            Spec::trigger_if(
                is_dancing,
                Spec::of(bh::Behavior::GoToTargetLocation {
                    memory: M::CelebrateLocation,
                    close: 4,
                    speed: 0.6,
                }),
            ),
            Spec::run_one(vec![
                (Spec::look_at_entity(Some(is_piglin), 8.0), 1),
                (
                    Spec::stroll_mode(0.6, 2, 1, bh::StrollMode::Land { from_water: true }),
                    1,
                ),
                (Spec::do_nothing(10, 20), 1),
            ]),
        ],
        M::CelebrateLocation,
    );
    let avoid = ActivitySpec::with_memory(
        Activity::Avoid,
        10,
        vec![
            Spec::walk_away_from(M::AvoidTarget, 1.0, 12, true),
            idle_look(),
            idle_movement(),
            Spec::erase_if(wants_to_stop_fleeing, M::AvoidTarget),
        ],
        M::AvoidTarget,
    );
    let mut ride_look = look_behaviors();
    ride_look.push((Spec::of(bh::Behavior::Check(|_| true)), 1));
    let ride = ActivitySpec::with_memory(
        Activity::Ride,
        10,
        vec![
            Spec::of(bh::Behavior::Mount { speed: 0.8 }),
            Spec::look_at_entity(Some(player_holding_loved), 8.0),
            Spec::sequence(
                Spec::of(bh::Behavior::Check(is_passenger)),
                Spec::trigger_one_shuffled(ride_look),
            ),
            Spec::of(bh::Behavior::Dismount {
                max_dist: 8,
                dont_ride_if: wants_to_stop_riding,
            }),
        ],
        M::RideTarget,
    );
    BrainSpec {
        memories: vec![
            M::UniversalAnger,
            M::AteRecently,
            M::SpearFleeingTime,
            M::SpearFleeingPosition,
            M::SpearChargePosition,
            M::SpearEngageTime,
        ],
        sensors: vec![
            SensorKind::NearestLivingEntities,
            SensorKind::NearestPlayers,
            SensorKind::NearestItems,
            SensorKind::HurtBy,
            super::sensor(super::Sensor::PiglinSpecific),
        ],
        activities: vec![core, idle, admire, fight, celebrate, avoid, ride],
        ..BrainSpec::default()
    }
}

fn brute_brain() -> BrainSpec {
    let core = ActivitySpec::new(
        Activity::Core,
        0,
        vec![
            Spec::look_at_target_sink(45, 90),
            Spec::move_to_target_sink(),
            Spec::of(bh::Behavior::StopBeingAngryIfTargetDead),
        ],
    );
    let idle = ActivitySpec::new(
        Activity::Idle,
        10,
        vec![
            Spec::start_attacking(None, brute_attack_target),
            Spec::run_one(vec![
                (Spec::look_at_entity(Some(is_player), 8.0), 1),
                (Spec::look_at_entity(Some(is_piglin), 8.0), 1),
                (Spec::look_at_entity(Some(is_brute), 8.0), 1),
                (Spec::look_at_entity(None, 8.0), 1),
                (Spec::do_nothing(30, 60), 1),
            ]),
            Spec::run_one(vec![
                (Spec::stroll(0.6), 2),
                (
                    Spec::of(bh::Behavior::InteractWith {
                        filter: is_piglin,
                        range: 8,
                        memory: M::InteractionTarget,
                        speed: 0.6,
                        stop: 2,
                    }),
                    2,
                ),
                (
                    Spec::of(bh::Behavior::InteractWith {
                        filter: is_brute,
                        range: 8,
                        memory: M::InteractionTarget,
                        speed: 0.6,
                        stop: 2,
                    }),
                    2,
                ),
                (
                    Spec::of(bh::Behavior::StrollToPoi {
                        memory: M::Home,
                        speed: 0.6,
                        close: 2,
                        max_dist: 100,
                        next_ok: 0,
                    }),
                    2,
                ),
                (
                    Spec::of(bh::Behavior::StrollAroundPoi {
                        memory: M::Home,
                        speed: 0.6,
                        max_dist: 5,
                        next_ok: 0,
                    }),
                    2,
                ),
                (Spec::do_nothing(30, 60), 1),
            ]),
            Spec::of(bh::Behavior::SetLookAndInteract {
                filter: is_player,
                range: 4,
            }),
        ],
    );
    let fight = ActivitySpec::with_memory(
        Activity::Fight,
        10,
        vec![
            Spec::of(bh::Behavior::StopAttackingIfTargetInvalid {
                stop_when: Some(brute_not_nearest_valid_target),
                on_erased: None,
                tired: true,
            }),
            Spec::walk_to_attack_target(1.0),
            Spec::melee(20),
        ],
        M::AttackTarget,
    );
    BrainSpec {
        memories: vec![M::NearestVisibleAdultPiglins],
        sensors: vec![
            SensorKind::NearestLivingEntities,
            SensorKind::NearestPlayers,
            SensorKind::NearestItems,
            SensorKind::HurtBy,
            super::sensor(super::Sensor::PiglinBruteSpecific),
        ],
        activities: vec![core, idle, fight],
        ..BrainSpec::default()
    }
}

pub fn init(m: &mut MobData) {
    let spec = if m.kind == Kind::Piglin {
        piglin_brain()
    } else {
        brute_brain()
    };
    m.brain = Some(Box::new(Brain::new(spec, &mut m.random)));
    m.can_pick_up_loot = true;
    m.nav.can_open_doors = true;
    m.nav.can_float = true;
    if m.kind == Kind::Piglin {
        state_mut(m).inventory = vec![ItemStack::default(); 8];
    }
}

/// The base movement speed with the baby modifier (`SPEED_MODIFIER_BABY`, +20 % of base).
fn apply_baby_speed(m: &mut MobData) {
    let base = crate::mob::Attrs::of(m.kind.type_id()).movement_speed;
    m.attrs.movement_speed = if m.baby { base * 1.2 } else { base };
}

/// `finalizeSpawn`.
pub fn finalize_spawn(m: &mut MobData, ctx: &mut SpawnContext<'_>) {
    if m.kind == Kind::PiglinBrute {
        // `PiglinBruteAi.initMemories`: home is the spawn position.
        let dimension = ctx.level.map_or_else(
            || "minecraft:the_nether".into(),
            |l| l.chunks.info().dimension.clone().into(),
        );
        if let Some(b) = m.brain.as_deref_mut() {
            b.set(
                M::Home,
                MemValue::GlobalPos(GlobalPos {
                    dimension,
                    pos: ctx.pos,
                }),
            );
        }
        m.equipment.set(
            EquipmentSlot::MainHand,
            equipment::stack("minecraft:golden_axe"),
        );
        return;
    }
    if ctx.reason != SpawnReason::Structure {
        if m.random.next_float() < 0.2 {
            m.baby = true;
        } else {
            let weapon = if m.random.next_float() < 0.5 {
                "minecraft:crossbow"
            } else if m.random.next_int_bounded(10) == 0 {
                "minecraft:golden_spear"
            } else {
                "minecraft:golden_sword"
            };
            m.equipment
                .set(EquipmentSlot::MainHand, equipment::stack(weapon));
        }
    }
    // `PiglinAi.initMemories`.
    let hunt = m.random.next_int_between_inclusive(600, 2400);
    if let Some(b) = m.brain.as_deref_mut() {
        b.set_with_expiry(M::HuntedRecently, MemValue::Bool(true), i64::from(hunt));
    }
    if !m.baby {
        for (slot, item) in [
            (EquipmentSlot::Head, "minecraft:golden_helmet"),
            (EquipmentSlot::Chest, "minecraft:golden_chestplate"),
            (EquipmentSlot::Legs, "minecraft:golden_leggings"),
            (EquipmentSlot::Feet, "minecraft:golden_boots"),
        ] {
            if m.random.next_float() < 0.1 {
                m.equipment.set(slot, equipment::stack(item));
            }
        }
    }
    equipment::populate_default_enchantments(m, ctx.special_multiplier);
    apply_baby_speed(m);
}

// --- Targets and predicates -------------------------------------------------------------------

fn can_hunt(t: &mut Tick<'_, '_>) -> bool {
    !state(t.mob_ref()).cannot_hunt
}

fn is_dancing(t: &mut Tick<'_, '_>) -> bool {
    state(t.mob_ref()).dancing
}

fn not_dancing(t: &mut Tick<'_, '_>) -> bool {
    !state(t.mob_ref()).dancing
}

fn is_passenger(t: &mut Tick<'_, '_>) -> bool {
    t.ents.riding.vehicle_of(t.id).is_some()
}

fn has_crossbow(t: &mut Tick<'_, '_>) -> bool {
    [EquipmentSlot::MainHand, EquipmentSlot::OffHand]
        .iter()
        .any(|&s| super::is_item(t.mob_ref().equipment.get(s), "minecraft:crossbow"))
}

fn doesnt_see_loved_holder(t: &mut Tick<'_, '_>) -> bool {
    !brain_ref(t).has(M::NearestPlayerHoldingWantedItem)
}

fn not_holding_loved_in_off_hand(t: &mut Tick<'_, '_>) -> bool {
    let off = t.mob_ref().equipment.get(EquipmentSlot::OffHand);
    off.is_empty() || !is_loved(off)
}

/// `isNearZombified`: a visible zombified piglin or zoglin within 6 blocks.
fn is_near_zombified(t: &mut Tick<'_, '_>) -> bool {
    brain_ref(t)
        .entity(M::NearestVisibleZombified)
        .is_some_and(|z| bh::closer_than(t, z, 6.0))
}

fn is_zombified(t: &Tick<'_, '_>, id: EntityId) -> bool {
    t.ents
        .mob(id)
        .is_some_and(|m| matches!(m.kind, Kind::ZombifiedPiglin | Kind::Zoglin))
}

/// `PiglinAi.findNearestValidAttackTarget`.
fn find_attack_target(t: &mut Tick<'_, '_>) -> Option<EntityId> {
    if is_near_zombified(t) {
        return None;
    }
    if let Some(angry) = angry_at(t)
        && sensing::is_attackable_ignoring_los(t, angry)
    {
        return Some(angry);
    }
    let b = brain_ref(t);
    if b.has(M::UniversalAnger)
        && let Some(p) = b.entity(M::NearestVisibleAttackablePlayer)
    {
        return Some(p);
    }
    if let Some(n) = b.entity(M::NearestVisibleNemesis) {
        return Some(n);
    }
    let p = b.entity(M::NearestTargetablePlayerNotWearingGold)?;
    sensing::is_attackable(t, p).then_some(p)
}

fn not_nearest_valid_target(t: &mut Tick<'_, '_>, target: EntityId) -> bool {
    find_attack_target(t) != Some(target)
}

/// `PiglinBruteAi.findNearestValidAttackTarget`.
fn brute_attack_target(t: &mut Tick<'_, '_>) -> Option<EntityId> {
    if let Some(angry) = angry_at(t)
        && sensing::is_attackable_ignoring_los(t, angry)
    {
        return Some(angry);
    }
    let b = brain_ref(t);
    b.entity(M::NearestVisibleAttackablePlayer)
        .or_else(|| b.entity(M::NearestVisibleNemesis))
}

fn brute_not_nearest_valid_target(t: &mut Tick<'_, '_>, target: EntityId) -> bool {
    brute_attack_target(t) != Some(target)
}

/// `getLivingEntityFromUUIDMemory(ANGRY_AT)`.
fn angry_at(t: &mut Tick<'_, '_>) -> Option<EntityId> {
    let uuid = brain_ref(t).uuid(M::AngryAt)?;
    bh::living_from_uuid(t, uuid)
}

fn wants_to_stop_fleeing(t: &mut Tick<'_, '_>) -> bool {
    let Some(avoid) = brain_ref(t).entity(M::AvoidTarget) else {
        return true;
    };
    match t.ents.mob(avoid).map(|m| m.kind) {
        Some(Kind::Hoglin) => !hoglins_outnumber_piglins(t),
        _ if is_zombified(t, avoid) => !brain_ref(t).is_entity(M::NearestVisibleZombified, avoid),
        _ => false,
    }
}

fn hoglins_outnumber_piglins(t: &Tick<'_, '_>) -> bool {
    let b = brain_ref(t);
    let piglins = b.int(M::VisibleAdultPiglinCount).unwrap_or(0) + 1;
    let hoglins = b.int(M::VisibleAdultHoglinCount).unwrap_or(0);
    hoglins > piglins
}

fn wants_to_stop_riding(t: &mut Tick<'_, '_>, vehicle: EntityId) -> bool {
    let Some(v) = t.ents.mob(vehicle) else {
        return false;
    };
    let hurt = |b: Option<&Brain>| b.is_some_and(|b| b.has(M::HurtBy));
    !v.baby
        || !v.is_alive()
        || hurt(t.mob_ref().brain.as_deref())
        || hurt(v.brain.as_deref())
        || (v.kind == Kind::Piglin && t.ents.riding.vehicle_of(vehicle).is_none())
}

/// `PiglinAi.isWearingSafeArmor` of a player: `#piglin_safe_armor` in an armour slot.
fn wears_safe_armor(t: &Tick<'_, '_>, id: EntityId) -> bool {
    t.ents.player(id).is_some_and(|p| {
        [
            EquipmentSlot::Head,
            EquipmentSlot::Chest,
            EquipmentSlot::Legs,
            EquipmentSlot::Feet,
        ]
        .iter()
        .any(|&s| {
            let st = p.inventory.equipment(s);
            !st.is_empty() && super::item_in_tag(st.item, "minecraft:piglin_safe_armor")
        })
    })
}

// --- Sensors -----------------------------------------------------------------------------------

/// The nearest `#piglin_repellents` (lit soul campfires only).
fn nearest_repellent(t: &Tick<'_, '_>) -> Option<BlockPos> {
    let tags = crate::block::block_tags();
    let mut found = None;
    for_manhattan(t.block_pos(), 8, 4, |p| {
        let state = t.level.block(p);
        let mut ok = tags.piglin_repellents.contains(state);
        if ok && state.block().name() == "minecraft:soul_campfire" {
            ok = state.value("lit") == Some("true");
        }
        if ok {
            found = Some(p);
        }
        ok
    });
    found
}

/// `PiglinAi.findNearbyAdultPiglins`.
fn nearby_adult_piglins(t: &Tick<'_, '_>) -> Vec<EntityId> {
    brain_ref(t)
        .entities(M::NearestLivingEntities)
        .iter()
        .copied()
        .filter(|&id| {
            t.ents
                .mob(id)
                .is_some_and(|m| matches!(m.kind, Kind::Piglin | Kind::PiglinBrute) && !m.baby)
        })
        .collect()
}

/// `PiglinSpecificSensor`.
pub fn specific_sensor(t: &mut Tick<'_, '_>) {
    let repellent = nearest_repellent(t);
    brain::brain(t).set_opt(M::NearestRepellent, repellent.map(MemValue::Pos));
    let mut all = Vec::new();
    sensing::visible_find_all(t, |_, _| true, &mut all);
    let (mut nemesis, mut huntable, mut baby_hoglin, mut baby_piglin) = (None, None, None, None);
    let (mut zombified, mut no_gold, mut holding) = (None, None, None);
    let mut hoglins = 0;
    let mut adults = Vec::new();
    for id in all {
        if let Some(m) = t.ents.mob(id) {
            match m.kind {
                Kind::Hoglin => {
                    if m.baby && baby_hoglin.is_none() {
                        baby_hoglin = Some(id);
                    } else if !m.baby {
                        hoglins += 1;
                        if huntable.is_none() && super::hoglin::can_be_hunted(m) {
                            huntable = Some(id);
                        }
                    }
                }
                Kind::PiglinBrute => adults.push(id),
                Kind::Piglin => {
                    if m.baby && baby_piglin.is_none() {
                        baby_piglin = Some(id);
                    } else if !m.baby {
                        adults.push(id);
                    }
                }
                Kind::WitherSkeleton if nemesis.is_none() => nemesis = Some(id),
                _ => {
                    if zombified.is_none() && is_zombified(t, id) {
                        zombified = Some(id);
                    }
                }
            }
        } else if t.ents.player(id).is_some() {
            if no_gold.is_none() && !wears_safe_armor(t, id) && bh::can_attack(t, id) {
                no_gold = Some(id);
            }
            let spectator = t
                .ents
                .player(id)
                .is_some_and(|p| p.game_mode == GameMode::Spectator);
            if holding.is_none() && !spectator && player_holding_loved(t, id) {
                holding = Some(id);
            }
        }
    }
    let nearby = nearby_adult_piglins(t);
    let b = brain::brain(t);
    b.set_opt(M::NearestVisibleNemesis, nemesis.map(MemValue::Entity));
    b.set_opt(
        M::NearestVisibleHuntableHoglin,
        huntable.map(MemValue::Entity),
    );
    b.set_opt(
        M::NearestVisibleBabyHoglin,
        baby_hoglin.map(MemValue::Entity),
    );
    b.set_opt(M::NearestVisibleZombified, zombified.map(MemValue::Entity));
    b.set_opt(
        M::NearestTargetablePlayerNotWearingGold,
        no_gold.map(MemValue::Entity),
    );
    b.set_opt(
        M::NearestPlayerHoldingWantedItem,
        holding.map(MemValue::Entity),
    );
    b.set_entities(M::NearbyAdultPiglins, nearby.into_iter());
    let count = i32::try_from(adults.len()).unwrap_or(i32::MAX);
    b.set_entities(M::NearestVisibleAdultPiglins, adults.into_iter());
    b.set(M::VisibleAdultPiglinCount, MemValue::Int(count));
    b.set(M::VisibleAdultHoglinCount, MemValue::Int(hoglins));
}

/// `PiglinBruteSpecificSensor`.
pub fn brute_sensor(t: &mut Tick<'_, '_>) {
    let nemesis = sensing::visible_find_closest(t, |t, id| {
        t.ents
            .mob(id)
            .is_some_and(|m| m.kind == Kind::WitherSkeleton)
    });
    let nearby = nearby_adult_piglins(t);
    let b = brain::brain(t);
    b.set_opt(M::NearestVisibleNemesis, nemesis.map(MemValue::Entity));
    b.set_entities(M::NearbyAdultPiglins, nearby.into_iter());
}

// --- Activity, anger and retreat -------------------------------------------------------------

/// `PiglinAi.updateActivity` / `PiglinBruteAi.updateActivity`, then `AbstractPiglin`'s
/// conversion.
pub fn update_activity(t: &mut Tick<'_, '_>) {
    let old = brain_ref(t).active_non_core();
    if t.mob_ref().kind == Kind::PiglinBrute {
        brain::brain(t).set_active_activity_to_first_valid(&[Activity::Fight, Activity::Idle]);
        let new = brain_ref(t).active_non_core();
        if old != new && new == Some(Activity::Fight) {
            make_sound(t, "minecraft:entity.piglin_brute.angry");
        }
        let attacking = brain_ref(t).has(M::AttackTarget);
        super::set_aggressive(t, attacking);
        // `maybePlayActivitySound`.
        if t.env.random.next_float() < 0.0125
            && brain_ref(t).active_non_core() == Some(Activity::Fight)
        {
            make_sound(t, "minecraft:entity.piglin_brute.angry");
        }
    } else {
        brain::brain(t).set_active_activity_to_first_valid(&[
            Activity::AdmireItem,
            Activity::Fight,
            Activity::Avoid,
            Activity::Celebrate,
            Activity::Ride,
            Activity::Idle,
        ]);
        let new = brain_ref(t).active_non_core();
        if old != new
            && let Some(sound) = activity_sound(t.mob_ref(), Some(t))
        {
            make_sound(t, sound);
        }
        let attacking = brain_ref(t).has(M::AttackTarget);
        super::set_aggressive(t, attacking);
        if !brain_ref(t).has(M::RideTarget) && is_baby_riding_baby(t) {
            let me = t.id;
            t.ents.riding.remove(me);
        }
        if !brain_ref(t).has(M::CelebrateLocation) {
            brain::brain(t).erase(M::Dancing);
        }
        let dancing = brain_ref(t).has(M::Dancing);
        if state(t.mob_ref()).dancing != dancing {
            state_mut(t.mob()).dancing = dancing;
            t.core_mut().mark_data_dirty();
        }
    }
    // `AbstractPiglin.customServerAiStep`.
    if is_converting(t) {
        state_mut(t.mob()).time_in_overworld += 1;
    } else {
        state_mut(t.mob()).time_in_overworld = 0;
    }
    if state(t.mob_ref()).time_in_overworld > 300 {
        if t.env.difficulty != Difficulty::Peaceful {
            let sound = if t.mob_ref().kind == Kind::Piglin {
                "minecraft:entity.piglin.converted_to_zombified"
            } else {
                "minecraft:entity.piglin_brute.converted_to_zombified"
            };
            make_sound(t, sound);
        }
        if t.mob_ref().kind == Kind::Piglin {
            cancel_admiring(t);
            drop_inventory(t);
        }
        if let Some(z) = convert_to(t, Kind::ZombifiedPiglin, true, true)
            && let Some(m) = t.ents.mob_mut(z)
        {
            crate::effect::add(
                m,
                crate::effect::EffectInstance::new(crate::effect::id::NAUSEA, 200, 0),
            );
        }
    }
}

fn is_converting(t: &Tick<'_, '_>) -> bool {
    !state(t.mob_ref()).immune_to_zombification && !t.mob_ref().no_ai && super::zombifies(t)
}

fn is_baby_riding_baby(t: &Tick<'_, '_>) -> bool {
    if !t.mob_ref().baby {
        return false;
    }
    t.ents
        .riding
        .vehicle_of(t.id)
        .and_then(|v| t.ents.mob(v))
        .is_some_and(|v| v.baby && matches!(v.kind, Kind::Piglin | Kind::Hoglin))
}

/// `PiglinAi.getSoundForActivity` (the ambient sound of piglins).
fn activity_sound(m: &MobData, t: Option<&Tick<'_, '_>>) -> Option<&'static str> {
    let b = m.brain.as_deref()?;
    let activity = b.active_non_core()?;
    let converting = t.is_some_and(is_converting);
    let near_avoid = t.is_some_and(|t| {
        b.entity(M::AvoidTarget)
            .is_some_and(|a| bh::closer_than(t, a, 12.0))
    });
    Some(if activity == Activity::Fight {
        "minecraft:entity.piglin.angry"
    } else if converting || (activity == Activity::Avoid && near_avoid) {
        "minecraft:entity.piglin.retreat"
    } else if activity == Activity::AdmireItem {
        "minecraft:entity.piglin.admiring_item"
    } else if activity == Activity::Celebrate {
        "minecraft:entity.piglin.celebrate"
    } else if b.has(M::NearestPlayerHoldingWantedItem) {
        "minecraft:entity.piglin.jealous"
    } else if b.has(M::NearestRepellent) {
        "minecraft:entity.piglin.retreat"
    } else {
        "minecraft:entity.piglin.ambient"
    })
}

/// The ambient sound: only while idle (`AbstractPiglin.playAmbientSound`).
pub fn ambient_sound(m: &MobData) -> Option<&'static str> {
    if !m
        .brain
        .as_deref()
        .is_some_and(|b| b.is_active(Activity::Idle))
    {
        return None;
    }
    activity_sound(m, None)
}

pub fn brute_ambient_sound(m: &MobData) -> Option<&'static str> {
    m.brain
        .as_deref()
        .is_some_and(|b| b.is_active(Activity::Idle))
        .then_some("minecraft:entity.piglin_brute.ambient")
}

/// `PiglinAi.setAngerTarget`.
fn set_anger_target(t: &mut Tick<'_, '_>, target: EntityId) {
    if !sensing::is_attackable_ignoring_los(t, target) {
        return;
    }
    let Some(uuid) = t.ents.store.get(target).map(|e| e.uuid()) else {
        return;
    };
    {
        let b = brain::brain(t);
        b.erase(M::CantReachWalkTargetSince);
        b.set_with_expiry(M::AngryAt, MemValue::Uuid(uuid), 600);
    }
    let is_hoglin = t.ents.mob(target).is_some_and(|m| m.kind == Kind::Hoglin);
    if is_hoglin && t.mob_ref().kind == Kind::Piglin && !state(t.mob_ref()).cannot_hunt {
        dont_hunt_for_a_while(t);
    }
    if t.ents.player(target).is_some() && t.env.rules.bool(Rule::UNIVERSAL_ANGER) {
        brain::brain(t).set_with_expiry(M::UniversalAnger, MemValue::Bool(true), 600);
    }
}

fn dont_hunt_for_a_while(t: &mut Tick<'_, '_>) {
    let ttl = t.env.random.next_int_between_inclusive(600, 2400);
    brain::brain(t).set_with_expiry(M::HuntedRecently, MemValue::Bool(true), i64::from(ttl));
}

fn nearest_visible_targetable_player(t: &Tick<'_, '_>) -> Option<EntityId> {
    brain_ref(t).entity(M::NearestVisibleAttackablePlayer)
}

fn set_anger_to_nearest_player_or(t: &mut Tick<'_, '_>, fallback: EntityId) {
    let target = nearest_visible_targetable_player(t).unwrap_or(fallback);
    set_anger_target(t, target);
}

/// `PiglinAi.broadcastAngerTarget`.
fn broadcast_anger_target(t: &mut Tick<'_, '_>, target: EntityId) {
    let hoglin = t
        .ents
        .mob(target)
        .filter(|m| m.kind == Kind::Hoglin)
        .map(super::hoglin::can_be_hunted);
    for other in brain_ref(t).entities(M::NearbyAdultPiglins).to_vec() {
        with_other(t, other, |t| {
            let can_hunt = t.mob_ref().kind == Kind::Piglin && !state(t.mob_ref()).cannot_hunt;
            if let Some(huntable) = hoglin
                && (!can_hunt || !huntable)
            {
                return;
            }
            // `setAngerTargetIfCloserThanCurrent`.
            let current = angry_at(t);
            let nearest = bh::nearest_target(t, current, target);
            if current != Some(nearest) {
                set_anger_target(t, nearest);
            }
        });
    }
}

fn broadcast_universal_anger(t: &mut Tick<'_, '_>) {
    for other in brain_ref(t).entities(M::NearbyAdultPiglins).to_vec() {
        with_other(t, other, |t| {
            if let Some(p) = nearest_visible_targetable_player(t) {
                set_anger_target(t, p);
            }
        });
    }
}

/// `PiglinAi.maybeRetaliate`.
pub fn maybe_retaliate(t: &mut Tick<'_, '_>, attacker: EntityId) {
    if brain_ref(t).is_active(Activity::Avoid) {
        return;
    }
    if !sensing::is_attackable_ignoring_los(t, attacker)
        || bh::much_further_than_target(t, attacker, 4.0)
    {
        return;
    }
    if t.ents.player(attacker).is_some() && t.env.rules.bool(Rule::UNIVERSAL_ANGER) {
        set_anger_to_nearest_player_or(t, attacker);
        broadcast_universal_anger(t);
    } else {
        set_anger_target(t, attacker);
        broadcast_anger_target(t, attacker);
    }
}

fn set_avoid_target_and_dont_hunt(t: &mut Tick<'_, '_>, target: EntityId) {
    let ttl = t.env.random.next_int_between_inclusive(100, 400);
    {
        let b = brain::brain(t);
        b.erase(M::AngryAt);
        b.erase(M::AttackTarget);
        b.erase(M::WalkTarget);
        b.set_with_expiry(M::AvoidTarget, MemValue::Entity(target), i64::from(ttl));
    }
    dont_hunt_for_a_while(t);
}

fn broadcast_retreat(t: &mut Tick<'_, '_>, target: EntityId) {
    for other in brain_ref(t)
        .entities(M::NearestVisibleAdultPiglins)
        .to_vec()
    {
        with_other(t, other, |t| {
            if t.mob_ref().kind != Kind::Piglin {
                return;
            }
            let b = brain_ref(t);
            let (avoid, attack) = (b.entity(M::AvoidTarget), b.entity(M::AttackTarget));
            let nearest = bh::nearest_target(t, avoid, target);
            let nearest = bh::nearest_target(t, attack, nearest);
            set_avoid_target_and_dont_hunt(t, nearest);
        });
    }
}

/// `PiglinAi.wasHurtBy`.
pub fn was_hurt_by(t: &mut Tick<'_, '_>, attacker: EntityId) {
    if t.ents.mob(attacker).is_some_and(|m| m.kind == Kind::Piglin) {
        return;
    }
    if !t.mob_ref().equipment.get(EquipmentSlot::OffHand).is_empty() {
        stop_holding_off_hand(t, false);
    }
    {
        let b = brain::brain(t);
        b.erase(M::CelebrateLocation);
        b.erase(M::Dancing);
        b.erase(M::AdmiringItem);
    }
    if t.ents.player(attacker).is_some() {
        brain::brain(t).set_with_expiry(M::AdmiringDisabled, MemValue::Bool(true), 400);
    }
    if let Some(avoid) = brain_ref(t).entity(M::AvoidTarget) {
        let kind_of = |id| t.ents.store.get(id).map(|e| e.kind());
        if kind_of(avoid) != kind_of(attacker) {
            brain::brain(t).erase(M::AvoidTarget);
        }
    }
    if t.mob_ref().baby {
        brain::brain(t).set_with_expiry(M::AvoidTarget, MemValue::Entity(attacker), 100);
        if sensing::is_attackable_ignoring_los(t, attacker) {
            broadcast_anger_target(t, attacker);
        }
    } else if t.ents.mob(attacker).is_some_and(|m| m.kind == Kind::Hoglin)
        && hoglins_outnumber_piglins(t)
    {
        set_avoid_target_and_dont_hunt(t, attacker);
        broadcast_retreat(t, attacker);
    } else {
        maybe_retaliate(t, attacker);
    }
}

/// `PiglinBruteAi.wasHurtBy`.
pub fn brute_was_hurt_by(t: &mut Tick<'_, '_>, attacker: EntityId) {
    if t.ents
        .mob(attacker)
        .is_some_and(|m| matches!(m.kind, Kind::Piglin | Kind::PiglinBrute))
    {
        return;
    }
    maybe_retaliate(t, attacker);
}

// --- Items -----------------------------------------------------------------------------------

/// `SimpleContainer.canAddItem`.
fn can_add_to_inventory(m: &MobData, stack: &ItemStack) -> bool {
    state(m).inventory.iter().any(|s| {
        s.is_empty() || (crate::item::matches(s, stack) && s.count < crate::item::max_stack_size(s))
    })
}

/// `SimpleContainer.addItem`: merges, then fills an empty slot; returns what did not fit.
fn add_to_inventory(m: &mut MobData, stack: ItemStack) -> ItemStack {
    let mut stack = stack;
    let inv = &mut state_mut(m).inventory;
    if inv.len() < 8 {
        inv.resize(8, ItemStack::default());
    }
    for s in inv.iter_mut() {
        if stack.is_empty() {
            break;
        }
        if !s.is_empty() && crate::item::matches(s, &stack) {
            let room = crate::item::max_stack_size(s) - s.count;
            let n = room.min(stack.count).max(0);
            s.count += n;
            stack.count -= n;
        }
    }
    if !stack.is_empty()
        && let Some(slot) = inv.iter_mut().find(|s| s.is_empty())
    {
        *slot = stack;
        return ItemStack::default();
    }
    if stack.count <= 0 {
        ItemStack::default()
    } else {
        stack
    }
}

/// `PiglinAi.wantsToPickup` (with `Piglin.wantsToPickUp`'s rules and loot flag).
pub fn wants_to_pick_up(t: &Tick<'_, '_>, stack: &ItemStack) -> bool {
    let m = t.mob_ref();
    if !t.env.rules.bool(Rule::MOB_GRIEFING) || !m.can_pick_up_loot {
        return false;
    }
    wants_item(m, stack)
}

fn wants_item(m: &MobData, stack: &ItemStack) -> bool {
    if m.baby && super::item_in_tag(stack.item, "minecraft:ignored_by_piglin_babies") {
        return false;
    }
    if super::item_in_tag(stack.item, "minecraft:piglin_repellents") {
        return false;
    }
    let b = m.brain.as_deref();
    if b.is_some_and(|b| b.has(M::AdmiringDisabled) && b.has(M::AttackTarget)) {
        return false;
    }
    let off = m.equipment.get(EquipmentSlot::OffHand);
    let not_holding_loved = off.is_empty() || !is_loved(off);
    if is_barter_currency(stack) {
        return not_holding_loved;
    }
    let space = can_add_to_inventory(m, stack);
    if super::is_item(stack, "minecraft:gold_nugget") {
        return space;
    }
    if super::item_in_tag(stack.item, "minecraft:piglin_food") {
        return !b.is_some_and(|b| b.has(M::AteRecently)) && space;
    }
    if !is_loved(stack) {
        let slot = crate::item::equipment_slot(stack);
        return can_replace(m, stack, m.equipment.get(slot), slot);
    }
    not_holding_loved && space
}

/// `Piglin.canReplaceCurrentItem`: loved items and the preferred weapons first.
fn can_replace(m: &MobData, new: &ItemStack, current: &ItemStack, slot: EquipmentSlot) -> bool {
    let preferred = (!m.baby).then_some("minecraft:piglin_preferred_weapons");
    let wanted = |s: &ItemStack| {
        is_loved(s) || preferred.is_some_and(|tag| !s.is_empty() && super::item_in_tag(s.item, tag))
    };
    let (n, c) = (wanted(new), wanted(current));
    if n && !c {
        return true;
    }
    if !n && c {
        return false;
    }
    equipment::can_replace_current_item(m, new, current, slot, preferred)
}

/// The piglin's looting (`Mob.aiStep` with `Piglin.pickUpItem`).
pub fn pick_up_items(t: &mut Tick<'_, '_>) {
    let rules = PickupRules {
        wants: |m, s| wants_item(m, s),
        preferred_weapon: None,
    };
    let griefing = t.env.rules.bool(Rule::MOB_GRIEFING);
    if !griefing {
        return;
    }
    equipment::pick_up_items(t, &rules, Some(pick_up_item), (1.0, 0.0, 1.0));
}

/// `PiglinAi.pickUpItem`.
fn pick_up_item(t: &mut Tick<'_, '_>, item: EntityId) -> bool {
    brain::brain(t).erase(M::WalkTarget);
    t.mob().nav.stop();
    let Some(stack) = t.ents.item(item).map(|d| d.stack.clone()) else {
        return false;
    };
    let taken = if super::is_item(&stack, "minecraft:gold_nugget") {
        equipment::take(t, item, stack.count);
        stack
    } else {
        equipment::take(t, item, 1);
        stack.with_count(1)
    };
    if is_loved(&taken) {
        brain::brain(t).erase(M::TimeTryingToReachAdmireItem);
        hold_in_off_hand(t, taken);
        admire(t);
    } else if super::item_in_tag(taken.item, "minecraft:piglin_food")
        && !brain_ref(t).has(M::AteRecently)
    {
        brain::brain(t).set_with_expiry(M::AteRecently, MemValue::Bool(true), 200);
    } else {
        let rules = PickupRules {
            wants: |_, _| true,
            preferred_weapon: None,
        };
        let equipped = equipment::equip_item_if_possible(t, &taken, &rules);
        if equipped.is_empty() {
            put_in_inventory(t, taken);
        }
    }
    true
}

fn admire(t: &mut Tick<'_, '_>) {
    brain::brain(t).set_with_expiry(M::AdmiringItem, MemValue::Bool(true), 119);
}

/// `holdInOffhand`: a held off hand item drops first.
fn hold_in_off_hand(t: &mut Tick<'_, '_>, stack: ItemStack) {
    let off = t.mob_ref().equipment.get(EquipmentSlot::OffHand).clone();
    if !off.is_empty() {
        equipment::spawn_at_location(t, off, 0.0);
    }
    let currency = is_barter_currency(&stack);
    let m = t.mob();
    m.equipment.set(EquipmentSlot::OffHand, stack);
    m.equipment.set_guaranteed_drop(EquipmentSlot::OffHand);
    if !currency {
        m.persistent = true;
    }
    equipment::broadcast(t);
}

fn put_in_inventory(t: &mut Tick<'_, '_>, stack: ItemStack) {
    let rest = add_to_inventory(t.mob(), stack);
    if !rest.is_empty() {
        let target = randompos::land_pos(t, 4, 2).unwrap_or_else(|| t.pos());
        throw_items(t, vec![rest], target);
    }
}

/// `throwItemsTowardPos`: the off hand swings and the items fly at the position.
fn throw_items(t: &mut Tick<'_, '_>, items: Vec<ItemStack>, target: Vec3) {
    if items.is_empty() {
        return;
    }
    bh::swing(t, true);
    for stack in items {
        if stack.is_empty() {
            continue;
        }
        // `BehaviorUtils.throwItem`.
        let p = t.pos();
        let start = Vec3::new(p.x, t.eye_y() - 0.3, p.z);
        let aim = Vec3::new(target.x, target.y + 1.0, target.z);
        let d = Vec3::new(aim.x - p.x, aim.y - p.y, aim.z - p.z).normalize();
        let v = f64::from(0.3_f32);
        let velocity = Vec3::new(d.x * v, d.y * v, d.z * v);
        let (uuid, y_rot, _) = crate::mob::new_item_entity();
        t.env.out.push(Effect::DropItem {
            uuid,
            pos: start,
            velocity,
            y_rot,
            pickup_delay: 10,
            stack,
        });
    }
}

/// `throwItems`: towards the nearest visible player, else a random spot.
fn throw_items_to_player_or_random(t: &mut Tick<'_, '_>, items: Vec<ItemStack>) {
    let target = match brain_ref(t)
        .entity(M::NearestVisiblePlayer)
        .and_then(|p| bh::entity_pos(t, p))
    {
        Some(p) => p,
        None => randompos::land_pos(t, 4, 2).unwrap_or_else(|| t.pos()),
    };
    throw_items(t, items, target);
}

/// The bartering loot (`gameplay/piglin_bartering`).
fn barter_items(t: &mut Tick<'_, '_>) -> Vec<ItemStack> {
    let Some(data) = crate::player::play::game_data() else {
        return Vec::new();
    };
    let table = "minecraft:gameplay/piglin_bartering";
    let Some(this) = crate::mob::attack::facts(t.ents, t.id) else {
        return Vec::new();
    };
    let none_level = |_: &str| 0;
    let entity = crate::loot::EntityLootContext {
        this,
        attacker: None,
        direct_attacker: None,
        last_damage_player: None,
        source: None,
        types: t.env.types,
        tags: data,
        attacker_enchantment_level: &none_level,
    };
    let none = |_: &str| 0;
    let in_tag = |item: u16, tag: &str| data.item_in_tag(item, tag);
    let empty = ItemStack::default();
    let ctx = crate::loot::BlockLootContext {
        state: vanillite_data::block::BlockState::AIR,
        tool: &empty,
        enchantment_level: &none,
        item_in_tag: &in_tag,
        explosion_radius: None,
        entity: Some(&entity),
        block_entity: None,
    };
    let mut out = Vec::new();
    // The table's random sequence, else the level random.
    data.loot
        .roll_table(t.level, table, &ctx, t.env.random, &mut out);
    out
}

/// `PiglinAi.stopHoldingOffHandItem`.
fn stop_holding_off_hand(t: &mut Tick<'_, '_>, bartering: bool) {
    let stack = t.mob_ref().equipment.get(EquipmentSlot::OffHand).clone();
    t.mob()
        .equipment
        .set(EquipmentSlot::OffHand, ItemStack::default());
    equipment::broadcast(t);
    let rules = PickupRules {
        wants: |_, _| true,
        preferred_weapon: None,
    };
    if !t.mob_ref().baby {
        let currency = is_barter_currency(&stack);
        if bartering && currency {
            let items = barter_items(t);
            throw_items_to_player_or_random(t, items);
        } else if !currency {
            let equipped = equipment::equip_item_if_possible(t, &stack, &rules);
            if equipped.is_empty() {
                put_in_inventory(t, stack);
            }
        }
    } else {
        let equipped = equipment::equip_item_if_possible(t, &stack, &rules);
        if equipped.is_empty() {
            let main = t.mob_ref().equipment.get(EquipmentSlot::MainHand).clone();
            if is_loved(&main) {
                put_in_inventory(t, main);
            } else {
                throw_items_to_player_or_random(t, vec![main]);
            }
            let m = t.mob();
            m.equipment.set(EquipmentSlot::MainHand, stack);
            m.equipment.set_guaranteed_drop(EquipmentSlot::MainHand);
            m.persistent = true;
            equipment::broadcast(t);
        }
    }
}

/// `PiglinAi.cancelAdmiring`.
fn cancel_admiring(t: &mut Tick<'_, '_>) {
    if brain_ref(t).has(M::AdmiringItem) {
        let off = t.mob_ref().equipment.get(EquipmentSlot::OffHand).clone();
        if !off.is_empty() {
            equipment::spawn_at_location(t, off, 0.0);
            t.mob()
                .equipment
                .set(EquipmentSlot::OffHand, ItemStack::default());
        }
    }
}

/// `dropCustomDeathLoot` / conversion: the inventory falls out.
pub fn drop_inventory(t: &mut Tick<'_, '_>) {
    let items = std::mem::take(&mut state_mut(t.mob()).inventory);
    for s in items {
        if !s.is_empty() {
            equipment::spawn_at_location(t, s, 0.0);
        }
    }
    state_mut(t.mob()).inventory = vec![ItemStack::default(); 8];
}

/// `PiglinAi.mobInteract`: an adult takes a gold ingot to admire.
pub fn interact(ix: &mut Interact<'_, '_>) -> Interaction {
    let Some(m) = ix.ents.mob(ix.target) else {
        return Interaction::Pass;
    };
    let b = m.brain.as_deref();
    let can_admire = !b.is_some_and(|b| b.has(M::AdmiringDisabled) || b.has(M::AdmiringItem))
        && !m.baby
        && is_barter_currency(ix.stack);
    if !can_admire {
        return Interaction::Pass;
    }
    let taken = ix.stack.with_count(1);
    crate::mob::animal::use_held_pub(ix);
    let target = ix.target;
    let mut t = Tick {
        level: ix.level,
        ents: &mut *ix.ents,
        env: &mut *ix.env,
        cx: &mut *ix.cx,
        id: target,
    };
    hold_in_off_hand(&mut t, taken);
    admire(&mut t);
    brain::brain(&mut t).erase(M::WalkTarget);
    t.mob().nav.stop();
    Interaction::Success
}

// --- Ranged weapons ----------------------------------------------------------------------

/// `performRangedAttack` → `performCrossbowAttack(body, 1.6)`: one arrow at the target.
pub fn shoot_crossbow(t: &mut Tick<'_, '_>, target: EntityId, _power: f32) {
    let Some(o) = t.other(target) else {
        return;
    };
    let p = t.pos();
    let start = Vec3::new(p.x, t.eye_y() - f64::from(0.1_f32), p.z);
    let (xd, zd) = (o.pos.x - p.x, o.pos.z - p.z);
    let dist = (xd * xd + zd * zd).sqrt();
    let target_y = o.pos.y + f64::from(o.height) * 0.333_333_333_333_333_3;
    let yd = target_y - start.y + dist * f64::from(0.2_f32);
    #[allow(clippy::cast_precision_loss)]
    let uncertainty = (14 - t.env.difficulty as i32 * 4) as f32;
    let weapon = t.mob_ref().equipment.get(EquipmentSlot::MainHand).clone();
    let id = t.id;
    let network_ids = t.level.network_ids();
    let mut random = t.mob_ref().random.clone();
    crate::mob::projectile::shoot_arrow(
        t.ents,
        network_ids,
        &mut random,
        start,
        Vec3::new(xd, yd, zd),
        1.6,
        uncertainty,
        Some(id),
        Some(&weapon),
        false,
        0,
    );
    t.mob().random = random;
    super::sound_at(
        t,
        "minecraft:item.crossbow.shoot",
        SoundSource::Hostile,
        1.0,
        1.0,
    );
    t.mob().no_action_time = 0;
}

/// `LivingEntity.updatingUsingItem`: the crossbow or spear use counts up.
pub fn before_tick(t: &mut Tick<'_, '_>) {
    let m = t.mob();
    if m.using_item >= 0 {
        m.using_item += 1;
    }
}

fn holds_spear(t: &Tick<'_, '_>) -> bool {
    let held = t.mob_ref().equipment.get(EquipmentSlot::MainHand);
    !held.is_empty() && crate::item::item_id(held).name().ends_with("_spear")
}

fn spear_target(t: &Tick<'_, '_>) -> Option<EntityId> {
    brain_ref(t).entity(M::AttackTarget)
}

fn spear_able(t: &Tick<'_, '_>) -> bool {
    spear_target(t).is_some() && holds_spear(t)
}

// --- Behaviours -------------------------------------------------------------------------------

pub fn duration(b: &Behavior) -> (i32, i32) {
    match b {
        Behavior::CrossbowAttack { .. } => (1200, 1200),
        Behavior::SpearRetreat { .. } => (100, 100),
        _ => (60, 60),
    }
}

pub fn shape(b: &Behavior) -> Shape {
    match b {
        Behavior::CrossbowAttack { .. }
        | Behavior::SpearApproach { .. }
        | Behavior::SpearAttack { .. }
        | Behavior::SpearRetreat { .. } => Shape::Timed,
        _ => Shape::OneShot,
    }
}

pub fn requirements(b: &Behavior) -> &'static [(M, MemoryStatus)] {
    match b {
        Behavior::StopHoldingItemIfNoLongerAdmiring => &[(M::AdmiringItem, S::ValueAbsent)],
        Behavior::StartAdmiringItemIfSeen { .. } => &[
            (M::NearestVisibleWantedItem, S::ValuePresent),
            (M::AdmiringItem, S::ValueAbsent),
            (M::AdmiringDisabled, S::ValueAbsent),
            (M::DisableWalkToAdmireItem, S::ValueAbsent),
        ],
        Behavior::StartCelebratingIfTargetDead { .. } => &[
            (M::AttackTarget, S::ValuePresent),
            (M::AngryAt, S::Registered),
            (M::CelebrateLocation, S::ValueAbsent),
            (M::Dancing, S::Registered),
        ],
        Behavior::StartHuntingHoglin => &[
            (M::NearestVisibleHuntableHoglin, S::ValuePresent),
            (M::AngryAt, S::ValueAbsent),
            (M::HuntedRecently, S::ValueAbsent),
            (M::NearestVisibleAdultPiglins, S::Registered),
        ],
        Behavior::RememberIfHoglinWasKilled => &[
            (M::AttackTarget, S::ValuePresent),
            (M::HuntedRecently, S::Registered),
        ],
        Behavior::StopAdmiringIfItemTooFarAway { .. } => &[
            (M::AdmiringItem, S::ValuePresent),
            (M::NearestVisibleWantedItem, S::Registered),
        ],
        Behavior::StopAdmiringIfTired { .. } => &[
            (M::AdmiringItem, S::ValuePresent),
            (M::NearestVisibleWantedItem, S::ValuePresent),
            (M::TimeTryingToReachAdmireItem, S::Registered),
            (M::DisableWalkToAdmireItem, S::Registered),
        ],
        Behavior::BabyRideBabyHoglin { .. } => &[
            (M::NearestVisibleBabyHoglin, S::ValuePresent),
            (M::RideTarget, S::ValueAbsent),
        ],
        Behavior::CrossbowAttack { .. } => &[
            (M::LookTarget, S::Registered),
            (M::AttackTarget, S::ValuePresent),
        ],
        Behavior::SpearApproach { .. } => &[(M::SpearStatus, S::ValueAbsent)],
        Behavior::SpearAttack { .. } | Behavior::SpearRetreat { .. } => {
            &[(M::SpearStatus, S::ValuePresent)]
        }
    }
}

pub fn no_timeout(b: &Behavior) -> bool {
    matches!(
        b,
        Behavior::SpearApproach { .. } | Behavior::SpearAttack { .. }
    )
}

/// `SpearAttack.SpearStatus` values in `SPEAR_STATUS`.
const APPROACH: i32 = 0;
const CHARGING: i32 = 1;
const RETREAT: i32 = 2;

pub fn check(b: &mut Behavior, t: &mut Tick<'_, '_>) -> bool {
    let using = t.mob_ref().using_item >= 0;
    match b {
        Behavior::CrossbowAttack { .. } => crossbow_usable(t),
        Behavior::SpearApproach { .. } => spear_able(t) && !using,
        Behavior::SpearAttack { .. } => {
            brain_ref(t).int(M::SpearStatus).unwrap_or(APPROACH) == CHARGING
                && spear_able(t)
                && !using
        }
        Behavior::SpearRetreat { .. } => {
            if !spear_able(t) || using {
                return false;
            }
            if brain_ref(t).int(M::SpearStatus).unwrap_or(APPROACH) != RETREAT {
                return false;
            }
            let Some(target) = spear_target(t) else {
                return false;
            };
            let Some(tp) = bh::entity_pos(t, target) else {
                return false;
            };
            let mount = if t.ents.riding.vehicle_of(t.id).is_some() {
                2.0
            } else {
                0.0
            };
            let d = bh::dist_sqr(t, target).sqrt();
            let away = randompos::land_pos_away(
                t,
                (9.0 + mount - d).max(0.0),
                (11.0 + mount - d).max(1.0),
                7,
                tp,
            );
            match away {
                Some(a) => {
                    brain::brain(t).set(M::SpearFleeingPosition, MemValue::Vec3(a));
                    true
                }
                None => false,
            }
        }
        _ => true,
    }
}

/// `CrossbowAttack.checkExtraStartConditions`.
fn crossbow_usable(t: &mut Tick<'_, '_>) -> bool {
    let Some(target) = brain_ref(t).entity(M::AttackTarget) else {
        return false;
    };
    has_crossbow(t)
        && brain_ref(t).has(M::NearestVisibleLivingEntities)
        && sensing::visible_contains(t, target)
        && bh::within_attack_range(t, target, 0)
}

pub fn can_still_use(b: &mut Behavior, t: &mut Tick<'_, '_>, _now: i64) -> bool {
    match b {
        Behavior::CrossbowAttack { .. } => brain_ref(t).has(M::AttackTarget) && crossbow_usable(t),
        Behavior::SpearApproach { distance_sqr, .. } => {
            spear_able(t)
                && spear_target(t).is_some_and(|tg| bh::dist_sqr(t, tg) > f64::from(*distance_sqr))
        }
        Behavior::SpearAttack { .. } => {
            brain_ref(t).int(M::SpearEngageTime).unwrap_or(0) > 0 && spear_able(t)
        }
        Behavior::SpearRetreat { .. } => {
            let b = brain_ref(t);
            b.int(M::SpearFleeingTime).unwrap_or(100) < 100
                && b.has(M::SpearFleeingPosition)
                && !t.mob_ref().nav.is_done()
                && spear_able(t)
        }
        _ => false,
    }
}

pub fn start(b: &mut Behavior, t: &mut Tick<'_, '_>, _now: i64) {
    match b {
        Behavior::SpearApproach { .. } => {
            super::set_aggressive(t, true);
            brain::brain(t).set(M::SpearStatus, MemValue::Int(APPROACH));
        }
        Behavior::SpearAttack { .. } => {
            super::set_aggressive(t, true);
            // `KineticWeapon.computeDamageUseDuration` of the golden spear.
            let b = brain::brain(t);
            b.set(M::SpearEngageTime, MemValue::Int(SPEAR_USE_DURATION));
            b.erase(M::SpearChargePosition);
            crate::mob::projectile::start_using_item(t);
        }
        Behavior::SpearRetreat { .. } => {
            super::set_aggressive(t, true);
            brain::brain(t).set(M::SpearFleeingTime, MemValue::Int(0));
        }
        _ => {}
    }
}

/// The use duration of a golden spear's kinetic attack in ticks.
const SPEAR_USE_DURATION: i32 = 30;

pub fn tick(b: &mut Behavior, t: &mut Tick<'_, '_>, _now: i64) {
    match b {
        Behavior::CrossbowAttack { state, delay } => {
            let Some(target) = brain_ref(t).entity(M::AttackTarget) else {
                return;
            };
            brain::brain(t).set(M::LookTarget, MemValue::Look(Tracker::entity(target, true)));
            match *state {
                0 => {
                    crate::mob::projectile::start_using_item(t);
                    *state = 1;
                    set_charging(t, true);
                }
                1 => {
                    if t.mob_ref().using_item < 0 {
                        *state = 0;
                    }
                    // `CrossbowItem.getChargeDuration`: 1.25 s.
                    if t.mob_ref().using_item >= 25 {
                        crate::mob::projectile::stop_using_item(t);
                        *state = 2;
                        *delay = 20 + t.mob().random.next_int_bounded(20);
                        set_charging(t, false);
                    }
                }
                2 => {
                    *delay -= 1;
                    if *delay == 0 {
                        *state = 3;
                    }
                }
                _ => {
                    crate::mob::fam::perform_ranged_attack(t, target, 1.0);
                    *state = 0;
                }
            }
        }
        Behavior::SpearApproach { speed, .. } => {
            let Some(target) = spear_target(t) else {
                return;
            };
            brain::brain(t).set(M::LookTarget, MemValue::Look(Tracker::entity(target, true)));
            if let Some(tp) = bh::entity_pos(t, target) {
                nav::move_to_entity(t, tp, 0, *speed);
            }
        }
        Behavior::SpearAttack {
            charge,
            reposition,
            radius_sqr,
        } => {
            let Some(target) = spear_target(t) else {
                return;
            };
            let d_sqr = bh::dist_sqr(t, target);
            let mount = if t.ents.riding.vehicle_of(t.id).is_some() {
                2.0
            } else {
                0.0
            };
            let engage = brain_ref(t).int(M::SpearEngageTime).unwrap_or(0) - 1;
            {
                let b = brain::brain(t);
                b.set(M::LookTarget, MemValue::Look(Tracker::entity(target, true)));
                b.set(M::SpearEngageTime, MemValue::Int(engage));
            }
            if let Some(away) = brain_ref(t).vec3(M::SpearChargePosition) {
                nav::move_to_pos(t, away, *reposition);
                if t.mob_ref().nav.is_done() {
                    brain::brain(t).erase(M::SpearChargePosition);
                }
            } else if let Some(tp) = bh::entity_pos(t, target) {
                nav::move_to_entity(t, tp, 0, *charge);
                if d_sqr < f64::from(*radius_sqr) || t.mob_ref().nav.is_done() {
                    let d = d_sqr.sqrt();
                    let away = randompos::land_pos_away(t, 6.0 + mount - d, 7.0 + mount - d, 7, tp);
                    brain::brain(t).set_opt(M::SpearChargePosition, away.map(MemValue::Vec3));
                }
            }
        }
        Behavior::SpearRetreat { speed } => {
            let Some(target) = spear_target(t) else {
                return;
            };
            let time = brain_ref(t).int(M::SpearFleeingTime).unwrap_or(0) + 1;
            {
                let b = brain::brain(t);
                b.set(M::LookTarget, MemValue::Look(Tracker::entity(target, true)));
                b.set(M::SpearFleeingTime, MemValue::Int(time));
            }
            if let Some(flee) = brain_ref(t).vec3(M::SpearFleeingPosition) {
                nav::move_to_pos(t, flee, *speed);
            }
        }
        _ => {}
    }
}

pub fn stop(b: &mut Behavior, t: &mut Tick<'_, '_>, _now: i64) {
    match b {
        Behavior::CrossbowAttack { state, .. } => {
            crate::mob::projectile::stop_using_item(t);
            if has_crossbow(t) {
                set_charging(t, false);
            }
            *state = 0;
        }
        Behavior::SpearApproach { .. } => {
            t.mob().nav.stop();
            brain::brain(t).set(M::SpearStatus, MemValue::Int(CHARGING));
        }
        Behavior::SpearAttack { .. } => {
            t.mob().nav.stop();
            crate::mob::projectile::stop_using_item(t);
            let b = brain::brain(t);
            b.erase(M::SpearChargePosition);
            b.erase(M::SpearEngageTime);
            b.set(M::SpearStatus, MemValue::Int(RETREAT));
        }
        Behavior::SpearRetreat { .. } => {
            t.mob().nav.stop();
            super::set_aggressive(t, false);
            crate::mob::projectile::stop_using_item(t);
            let b = brain::brain(t);
            b.erase(M::SpearFleeingTime);
            b.erase(M::SpearFleeingPosition);
            b.erase(M::SpearStatus);
        }
        _ => {}
    }
}

fn set_charging(t: &mut Tick<'_, '_>, on: bool) {
    if state(t.mob_ref()).charging_crossbow != on {
        state_mut(t.mob()).charging_crossbow = on;
        t.core_mut().mark_data_dirty();
    }
}

pub fn trigger(b: &mut Behavior, t: &mut Tick<'_, '_>, _now: i64) -> bool {
    match b {
        Behavior::StopHoldingItemIfNoLongerAdmiring => {
            let off = t.mob_ref().equipment.get(EquipmentSlot::OffHand);
            if off.is_empty() || super::is_item(off, "minecraft:shield") {
                return false;
            }
            stop_holding_off_hand(t, true);
            true
        }
        Behavior::StartAdmiringItemIfSeen { duration } => {
            let Some(item) = brain_ref(t).entity(M::NearestVisibleWantedItem) else {
                return false;
            };
            if !t.ents.item(item).is_some_and(|d| is_loved(&d.stack)) {
                return false;
            }
            brain::brain(t).set_with_expiry(
                M::AdmiringItem,
                MemValue::Bool(true),
                i64::from(*duration),
            );
            true
        }
        Behavior::StartCelebratingIfTargetDead { duration } => {
            let Some(target) = brain_ref(t).entity(M::AttackTarget) else {
                return false;
            };
            let dead = t
                .ents
                .mob(target)
                .map(MobData::is_dead_or_dying)
                .or_else(|| t.ents.player(target).map(|p| p.survival.is_dead_or_dying()))
                .unwrap_or(true);
            if !dead {
                return false;
            }
            let hoglin = t.ents.mob(target).is_some_and(|m| m.kind == Kind::Hoglin);
            // `wantsToDance`: a thread local random seeded with the game time.
            let dance = hoglin && {
                let mut r = vanillite_base::random::LegacyRandom::new(t.game_time());
                r.next_float() < 0.1
            };
            let pos = bh::entity_pos(t, target).map(|p| BlockPos::containing(p.x, p.y, p.z));
            let is_player = t.ents.player(target).is_some();
            let forgive = t.env.rules.bool(Rule::FORGIVE_DEAD_PLAYERS);
            let b = brain::brain(t);
            if dance {
                b.set_with_expiry(M::Dancing, MemValue::Bool(true), i64::from(*duration));
            }
            if let Some(pos) = pos {
                b.set_with_expiry(
                    M::CelebrateLocation,
                    MemValue::Pos(pos),
                    i64::from(*duration),
                );
            }
            if !is_player || forgive {
                b.erase(M::AttackTarget);
                b.erase(M::AngryAt);
            }
            true
        }
        Behavior::StartHuntingHoglin => {
            if t.mob_ref().baby {
                return false;
            }
            let others = brain_ref(t)
                .entities(M::NearestVisibleAdultPiglins)
                .to_vec();
            let any_hunted = others.iter().any(|&o| {
                t.ents
                    .mob(o)
                    .and_then(|m| m.brain.as_deref())
                    .is_some_and(|b| b.has(M::HuntedRecently))
            });
            if any_hunted {
                return false;
            }
            let Some(target) = brain_ref(t).entity(M::NearestVisibleHuntableHoglin) else {
                return false;
            };
            set_anger_target(t, target);
            dont_hunt_for_a_while(t);
            broadcast_anger_target(t, target);
            for o in others {
                with_other(t, o, dont_hunt_for_a_while);
            }
            true
        }
        Behavior::RememberIfHoglinWasKilled => {
            let Some(target) = brain_ref(t).entity(M::AttackTarget) else {
                return false;
            };
            if t.ents
                .mob(target)
                .is_some_and(|m| m.kind == Kind::Hoglin && m.is_dead_or_dying())
            {
                dont_hunt_for_a_while(t);
            }
            true
        }
        Behavior::StopAdmiringIfItemTooFarAway { distance } => {
            if !t.mob_ref().equipment.get(EquipmentSlot::OffHand).is_empty() {
                return false;
            }
            let near = brain_ref(t)
                .entity(M::NearestVisibleWantedItem)
                .is_some_and(|i| bh::closer_than(t, i, f64::from(*distance)));
            if near {
                return false;
            }
            brain::brain(t).erase(M::AdmiringItem);
            true
        }
        Behavior::StopAdmiringIfTired { max, disable } => {
            if !t.mob_ref().equipment.get(EquipmentSlot::OffHand).is_empty() {
                return false;
            }
            let b = brain::brain(t);
            match b.int(M::TimeTryingToReachAdmireItem) {
                None => b.set(M::TimeTryingToReachAdmireItem, MemValue::Int(0)),
                Some(time) if time > *max => {
                    b.erase(M::AdmiringItem);
                    b.erase(M::TimeTryingToReachAdmireItem);
                    b.set_with_expiry(
                        M::DisableWalkToAdmireItem,
                        MemValue::Bool(true),
                        i64::from(*disable),
                    );
                }
                Some(time) => b.set(M::TimeTryingToReachAdmireItem, MemValue::Int(time + 1)),
            }
            true
        }
        Behavior::BabyRideBabyHoglin { ticks } => {
            if !t.mob_ref().baby {
                return false;
            }
            // `Ticker(RIDE_START_INTERVAL).tickDownAndCheck` with the level random.
            let ready = if *ticks == 0 {
                *ticks = t.env.random.next_int_between_inclusive(200, 800) - 1;
                false
            } else {
                *ticks -= 1;
                *ticks == 0
            };
            if !ready {
                return false;
            }
            let Some(hoglin) = brain_ref(t).entity(M::NearestVisibleBabyHoglin) else {
                return false;
            };
            let ttl = t.env.random.next_int_between_inclusive(200, 600);
            brain::brain(t).set_with_expiry(
                M::RideTarget,
                MemValue::Entity(hoglin),
                i64::from(ttl),
            );
            true
        }
        _ => false,
    }
}

// --- Data ---------------------------------------------------------------------------------

/// Synched data: immunity (16); piglins also baby (17), charging (18), dancing (19).
pub fn encode(w: &mut DataWriter<'_>, m: &MobData) {
    let s = state(m);
    w.boolean(16, s.immune_to_zombification, false);
    if m.kind == Kind::Piglin {
        w.boolean(17, m.baby, false);
        w.boolean(18, s.charging_crossbow, false);
        w.boolean(19, s.dancing, false);
    }
}

pub fn save(m: &MobData, out: &mut Compound) {
    let s = state(m);
    out.insert("IsImmuneToZombification", s.immune_to_zombification);
    out.insert("TimeInOverworld", s.time_in_overworld);
    if m.kind == Kind::Piglin {
        out.insert("IsBaby", m.baby);
        out.insert("CannotHunt", s.cannot_hunt);
        let items: Vec<Tag> = s
            .inventory
            .iter()
            .filter(|i| !i.is_empty())
            .map(|i| Tag::Compound(crate::inventory::stack_to_nbt(i)))
            .collect();
        out.insert("Inventory", List(items));
    }
}

pub fn load(m: &mut MobData, tag: &Compound) {
    m.can_pick_up_loot = nbt_bool(tag, "CanPickUpLoot", true);
    let baby = nbt_bool(tag, "IsBaby", false);
    {
        let s = state_mut(m);
        s.immune_to_zombification = nbt_bool(tag, "IsImmuneToZombification", false);
        s.time_in_overworld = nbt_int(tag, "TimeInOverworld", 0);
        s.cannot_hunt = nbt_bool(tag, "CannotHunt", false);
        s.inventory = vec![ItemStack::default(); 8];
    }
    if m.kind == Kind::Piglin {
        m.baby = baby;
        if let Some(Tag::List(l)) = tag.get("Inventory") {
            for t in &l.0 {
                if let Tag::Compound(c) = t {
                    let stack = crate::inventory::stack_from_nbt(c);
                    let _ = add_to_inventory(m, stack);
                }
            }
        }
        apply_baby_speed(m);
    }
}
