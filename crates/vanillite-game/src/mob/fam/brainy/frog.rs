//! Frogs and tadpoles (`Frog`, `FrogAi`, `ShootTongue`, `Croak`, `TryLaySpawnOnFluidNearLand`,
//! `TryFindLandNearLiquid`, `FrogAttackablesSensor`, `Tadpole`, `TadpoleAi`).

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Vec3};
use vanillite_nbt::{Compound, Tag};
use vanillite_protocol::packets::play::clientbound::SoundSource;
use vanillite_world::entity::{EntityFlags, EntityId, Pose};

use super::goat::{Jumper, long_jump_specs};
use super::{SpawnContext, nbt_bool, nbt_int, sound_at, spec, state, state_mut};
use crate::mob::animal::Interaction;
use crate::mob::brain::behavior::{self as bh, Shape, Spec, for_manhattan};
use crate::mob::brain::memory::MemoryStatus as S;
use crate::mob::brain::{
    self, Activity, ActivitySpec, Brain, BrainSpec, MemValue, MemoryStatus, MemoryType as M,
    SensorKind, Tracker, WalkTarget, brain_ref, sensing,
};
use crate::mob::fam::Interact;
use crate::mob::nav::{LookKind, MoveKind, NavKind};
use crate::mob::path::{MalusTable, PathType};
use crate::mob::{Kind, MobData, Tick};
use crate::player::encode::DataWriter;

/// `Frog`: water costs 4, trapdoors are off limits.
pub const MALUS: &MalusTable = &[(PathType::Water, 4.0), (PathType::Trapdoor, -1.0)];

/// `FrogAttackablesSensor.requires`.
pub const SENSOR_MEMORIES: &[M] = &[
    M::NearestAttackable,
    M::NearestVisibleLivingEntities,
    M::UnreachableTongueTargets,
];

/// The frog variants by `minecraft:frog_variant` id.
pub const VARIANTS: [&str; 3] = ["cold", "temperate", "warm"];

/// The frog behaviours.
#[derive(Clone, Copy, Debug)]
pub enum Behavior {
    /// `Croak`.
    Croak { counter: i32 },
    /// `ShootTongue`: state 0 move to target, 1 catch, 2 eat, 3 done.
    ShootTongue {
        state: u8,
        eat_timer: i32,
        path_counter: i32,
    },
    /// `TryLaySpawnOnFluidNearLand(FROGSPAWN)`.
    LaySpawn,
    /// `TryFindLandNearLiquid.create(range, speed, FROG_TRIES_TO_FIND_LAND_NEAR)`.
    FindLandNearWater {
        range: i32,
        speed: f32,
        next_ok: i64,
    },
}

fn fr(b: Behavior) -> Spec {
    spec(super::Behavior::Frog(b))
}

fn is_player(t: &Tick<'_, '_>, id: EntityId) -> bool {
    t.ents.player(id).is_some()
}

fn not_breeding(t: &mut Tick<'_, '_>) -> bool {
    !brain_ref(t).has(M::BreedTarget)
}

fn nearest_attackable(t: &mut Tick<'_, '_>) -> Option<EntityId> {
    brain_ref(t).entity(M::NearestAttackable)
}

fn croak() -> Spec {
    fr(Behavior::Croak { counter: 0 })
}

fn frog_brain() -> BrainSpec {
    let core = ActivitySpec::new(
        Activity::Core,
        0,
        vec![
            Spec::animal_panic(2.0),
            Spec::look_at_target_sink(45, 90),
            Spec::move_to_target_sink(),
            Spec::count_down(M::TemptationCooldownTicks),
            Spec::count_down(M::LongJumpCooldownTicks),
        ],
    );
    let idle = ActivitySpec::with_priorities(
        Activity::Idle,
        vec![
            (
                0,
                Spec::look_at_entity_sometimes(Some(is_player), 6.0, 30, 60),
            ),
            (0, Spec::make_love(Kind::Frog, 1.0, 2)),
            (1, Spec::follow_temptation(|_| 1.25, 2.5, false)),
            (
                2,
                Spec::start_attacking(Some(not_breeding), nearest_attackable),
            ),
            (3, Spec::try_find_land(6, 1.0)),
            (
                4,
                Spec::run_one_if(
                    &[(M::WalkTarget, S::ValueAbsent)],
                    vec![
                        (Spec::stroll(1.0), 1),
                        (Spec::walk_to_look_target(1.0, 3), 1),
                        (croak(), 3),
                        (Spec::of(bh::Behavior::Check(|t| t.on_ground())), 2),
                    ],
                ),
            ),
        ],
    )
    .requires(&[
        (M::LongJumpMidJump, S::ValueAbsent),
        (M::IsInWater, S::ValueAbsent),
    ]);
    let swim = ActivitySpec::with_priorities(
        Activity::Swim,
        vec![
            (
                0,
                Spec::look_at_entity_sometimes(Some(is_player), 6.0, 30, 60),
            ),
            (1, Spec::follow_temptation(|_| 1.25, 2.5, false)),
            (
                2,
                Spec::start_attacking(Some(not_breeding), nearest_attackable),
            ),
            (3, Spec::try_find_land(8, 1.5)),
            (
                5,
                Spec::gate(
                    &[(M::WalkTarget, S::ValueAbsent)],
                    &[],
                    false,
                    false,
                    vec![
                        (Spec::stroll_mode(0.75, 10, 7, bh::StrollMode::Swim), 1),
                        (Spec::stroll(1.0), 1),
                        (Spec::walk_to_look_target(1.0, 3), 1),
                        (
                            Spec::of(bh::Behavior::Check(|t| t.mob_ref().fluids.in_water())),
                            5,
                        ),
                    ],
                ),
            ),
        ],
    )
    .requires(&[
        (M::LongJumpMidJump, S::ValueAbsent),
        (M::IsInWater, S::ValuePresent),
    ]);
    let lay_spawn = ActivitySpec::with_priorities(
        Activity::LaySpawn,
        vec![
            (
                0,
                Spec::look_at_entity_sometimes(Some(is_player), 6.0, 30, 60),
            ),
            (
                1,
                Spec::start_attacking(Some(not_breeding), nearest_attackable),
            ),
            (
                2,
                fr(Behavior::FindLandNearWater {
                    range: 8,
                    speed: 1.0,
                    next_ok: 0,
                }),
            ),
            (3, fr(Behavior::LaySpawn)),
            (
                4,
                Spec::run_one(vec![
                    (Spec::stroll(1.0), 2),
                    (Spec::walk_to_look_target(1.0, 3), 1),
                    (croak(), 2),
                    (Spec::of(bh::Behavior::Check(|t| t.on_ground())), 1),
                ]),
            ),
        ],
    )
    .requires(&[
        (M::LongJumpMidJump, S::ValueAbsent),
        (M::IsPregnant, S::ValuePresent),
    ]);
    let tongue = ActivitySpec::with_memory(
        Activity::Tongue,
        0,
        vec![
            Spec::stop_attacking_if_invalid(),
            fr(Behavior::ShootTongue {
                state: 3,
                eat_timer: 0,
                path_counter: 0,
            }),
        ],
        M::AttackTarget,
    );
    let (mid, jump) = long_jump_specs(Jumper::Frog);
    let long_jump = ActivitySpec::with_priorities(Activity::LongJump, vec![(0, mid), (1, jump)])
        .requires(&[
            (M::TemptingPlayer, S::ValueAbsent),
            (M::BreedTarget, S::ValueAbsent),
            (M::LongJumpCooldownTicks, S::ValueAbsent),
            (M::IsInWater, S::ValueAbsent),
        ]);
    BrainSpec {
        sensors: vec![
            SensorKind::NearestLivingEntities,
            SensorKind::HurtBy,
            super::sensor(super::Sensor::FrogAttackables),
            SensorKind::Tempting(Some("minecraft:frog_food")),
            SensorKind::IsInWater,
        ],
        activities: vec![core, idle, swim, lay_spawn, tongue, long_jump],
        ..BrainSpec::default()
    }
}

fn tadpole_brain() -> BrainSpec {
    let core = ActivitySpec::new(
        Activity::Core,
        0,
        vec![
            Spec::animal_panic(2.0),
            Spec::look_at_target_sink(45, 90),
            Spec::move_to_target_sink(),
            Spec::count_down(M::TemptationCooldownTicks),
        ],
    );
    let idle = ActivitySpec::with_priorities(
        Activity::Idle,
        vec![
            (
                0,
                Spec::look_at_entity_sometimes(Some(is_player), 6.0, 30, 60),
            ),
            (1, Spec::follow_temptation(|_| 1.25, 2.5, false)),
            (
                2,
                Spec::gate(
                    &[(M::WalkTarget, S::ValueAbsent)],
                    &[],
                    false,
                    false,
                    vec![
                        (Spec::stroll_mode(0.5, 10, 7, bh::StrollMode::Swim), 2),
                        (Spec::walk_to_look_target(0.5, 3), 3),
                        (
                            Spec::of(bh::Behavior::Check(|t| t.mob_ref().fluids.in_water())),
                            5,
                        ),
                    ],
                ),
            ),
        ],
    );
    BrainSpec {
        sensors: vec![
            SensorKind::NearestLivingEntities,
            SensorKind::NearestPlayers,
            SensorKind::HurtBy,
            SensorKind::Tempting(Some("minecraft:frog_food")),
        ],
        activities: vec![core, idle],
        ..BrainSpec::default()
    }
}

pub fn init(m: &mut MobData) {
    let tadpole = m.kind == Kind::Tadpole;
    let spec = if tadpole {
        tadpole_brain()
    } else {
        frog_brain()
    };
    m.brain = Some(Box::new(Brain::new(spec, &mut m.random)));
    m.move_ctl.kind = MoveKind::SmoothSwimming {
        max_turn_x: 85,
        max_turn_y: 10,
        in_water_speed: 0.02,
        outside_water_speed: 0.1,
        apply_gravity: true,
    };
    if tadpole {
        m.nav.kind = NavKind::Water { breaching: false };
        m.look.kind = LookKind::SmoothSwimming { max_y_rot: 10 };
        state_mut(m).from_bucket = true;
    } else {
        m.nav.kind = NavKind::Amphibious;
        state_mut(m).variant = 1;
    }
}

/// `FrogAi.initMemories`.
fn init_memories(m: &mut MobData, random: &mut impl RandomSource) {
    let cd = random.next_int_between_inclusive(100, 140);
    if let Some(b) = m.brain.as_deref_mut() {
        b.set(M::LongJumpCooldownTicks, MemValue::Int(cd));
    }
}

/// `finalizeSpawn`: the variant of the biome, then the jump cooldown.
pub fn finalize_spawn(m: &mut MobData, ctx: &mut SpawnContext<'_>) {
    let variant = if ctx.biome_is("minecraft:spawns_warm_variant_frogs") {
        2
    } else if ctx.biome_is("minecraft:spawns_cold_variant_frogs") {
        0
    } else {
        1
    };
    state_mut(m).variant = variant;
    let mut r = m.random.clone();
    init_memories(m, &mut r);
    m.random = r;
}

/// `FrogAi.updateActivity` / `TadpoleAi.updateActivity`.
pub fn update_activity(t: &mut Tick<'_, '_>) {
    if t.mob_ref().kind == Kind::Tadpole {
        brain::brain(t).set_active_activity_to_first_valid(&[Activity::Idle]);
        return;
    }
    brain::brain(t).set_active_activity_to_first_valid(&[
        Activity::Tongue,
        Activity::LaySpawn,
        Activity::LongJump,
        Activity::Swim,
        Activity::Idle,
    ]);
}

/// `Frog.canEat`: `#frog_food`, cubes only at size 1.
pub fn can_eat(t: &Tick<'_, '_>, id: EntityId) -> bool {
    if t.ents.mob(id).is_some_and(|m| {
        matches!(m.kind, Kind::Slime | Kind::MagmaCube | Kind::SulfurCube) && slime_size(m) != 1
    }) {
        return false;
    }
    brain::sensor::in_entity_tag(t, id, "minecraft:frog_food")
}

/// The size of a cube mob (its width / 0.52).
fn slime_size(m: &MobData) -> i32 {
    #[allow(clippy::cast_possible_truncation)]
    let size = (f64::from(crate::mob::kinds::mob_dimensions(m).width) / 0.52).round() as i32;
    size.max(1)
}

/// `FrogAttackablesSensor`.
pub fn attackables_sensor(t: &mut Tick<'_, '_>) {
    let unreachable: Vec<u128> = match brain_ref(t).get(M::UnreachableTongueTargets) {
        Some(MemValue::Uuids(v)) => v.clone(),
        _ => Vec::new(),
    };
    let mut candidates = Vec::new();
    sensing::visible_find_all(t, |_, _| true, &mut candidates);
    let mut found = None;
    for id in candidates {
        if !sensing::is_attackable(t, id) || !can_eat(t, id) {
            continue;
        }
        let uuid = t.ents.store.get(id).map(|e| e.uuid());
        if uuid.is_some_and(|u| unreachable.contains(&u)) {
            continue;
        }
        if bh::closer_than(t, id, 10.0) {
            found = Some(id);
            break;
        }
    }
    brain::brain(t).set_opt(M::NearestAttackable, found.map(MemValue::Entity));
}

/// `FrogAi.isAcceptableLandingSpot`.
pub fn acceptable_landing(t: &Tick<'_, '_>, pos: BlockPos) -> bool {
    let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
    let above = BlockPos::new(pos.x, pos.y + 1, pos.z);
    let fluid = |p| !t.level.block(p).fluid_state().is_empty();
    if fluid(pos) || fluid(below) || fluid(above) {
        return false;
    }
    let tags = crate::block::block_tags();
    let (bs, bs_below) = (t.level.block(pos), t.level.block(below));
    if tags.frog_prefer_jump_to.contains(bs) || tags.frog_prefer_jump_to.contains(bs_below) {
        return true;
    }
    let pt = crate::mob::path::path_type_static(t.level, t.level.min_y(), pos.x, pos.y, pos.z);
    let pt_below =
        crate::mob::path::path_type_static(t.level, t.level.min_y(), below.x, below.y, below.z);
    if pt != PathType::Trapdoor && (!bs.is_air() || pt_below != PathType::Trapdoor) {
        super::goat::default_landing(t, pos)
    } else {
        true
    }
}

/// `Frog.spawnChildFromBreeding`: no baby; the frog becomes pregnant.
pub fn spawn_child_from_breeding(t: &mut Tick<'_, '_>, partner: EntityId) {
    super::finalize_child_from_breeding(t, partner);
    brain::brain(t).set(M::IsPregnant, MemValue::Unit);
}

/// The tadpole's ageing (`aiStep`) and fish flopping on land.
pub fn tadpole_ai_step(t: &mut Tick<'_, '_>) {
    // `AbstractFish.aiStep`: flopping out of water.
    if !t.mob_ref().fluids.in_water() && t.on_ground() && t.mob_ref().vertical_collision {
        let r = &mut t.mob().random;
        let dx = f64::from((r.next_float() * 2.0 - 1.0) * 0.05);
        let dz = f64::from((r.next_float() * 2.0 - 1.0) * 0.05);
        let v = t.velocity();
        t.set_velocity(Vec3::new(v.x + dx, v.y + f64::from(0.4_f32), v.z + dz));
        t.core_mut().set_flags(EntityFlags::ON_GROUND, false);
        t.core_mut().set_flags(EntityFlags::HAS_IMPULSE, true);
        super::make_sound(t, "minecraft:entity.tadpole.flop");
    }
    // `WaterAnimal.handleAirSupply`.
    if t.mob_ref().is_alive() && !t.mob_ref().fluids.in_water() {
        t.mob().air -= 1;
        if t.mob_ref().air <= -20 {
            t.mob().air = 0;
            let kind = t.env.types.drown;
            let id = t.id;
            crate::mob::combat::hurt(
                t.level,
                t.ents,
                t.env,
                t.cx,
                id,
                crate::survival::damage::DamageSource::of(kind),
                2.0,
            );
            if !t.exists() {
                return;
            }
        }
    } else {
        t.mob().air = 300;
    }
    if !state(t.mob_ref()).playing_dead {
        // `playing_dead` is the tadpole's `AGE_LOCKED`.
        let age = state(t.mob_ref()).tadpole_age + 1;
        set_tadpole_age(t, age);
    }
}

/// `Tadpole.setAge`: a frog at 24000 ticks.
fn set_tadpole_age(t: &mut Tick<'_, '_>, age: i32) {
    state_mut(t.mob()).tadpole_age = age;
    if age >= 24000 {
        // `ageUp`: a persistent frog with `finalizeSpawn(CONVERSION)`.
        let Some(frog) = super::convert_to(t, Kind::Frog, false, false) else {
            return;
        };
        let pos = t.ents.store.get(frog).map(|e| e.pos());
        if let Some(m) = t.ents.mob_mut(frog) {
            let pos = pos.map_or(BlockPos::new(0, 0, 0), |p| {
                BlockPos::containing(p.x, p.y, p.z)
            });
            let mut ctx = SpawnContext::at(t.level, pos, super::SpawnReason::Conversion);
            crate::mob::kinds::finalize_spawn(m, &mut ctx, t.env.random);
            m.persistent = true;
            m.baby = false;
            m.age = 0;
        }
        sound_at(
            t,
            "minecraft:entity.tadpole.grow_up",
            SoundSource::Neutral,
            0.15,
            1.0,
        );
    }
}

/// `Tadpole.feed`: frog food speeds up its growth.
pub fn feed_tadpole(ix: &mut Interact<'_, '_>) -> Interaction {
    let locked = ix
        .ents
        .mob(ix.target)
        .is_some_and(|m| state(m).playing_dead);
    if locked {
        return Interaction::Pass;
    }
    crate::mob::animal::use_held_pub(ix);
    let target = ix.target;
    let mut t = Tick {
        level: ix.level,
        ents: &mut *ix.ents,
        env: &mut *ix.env,
        cx: &mut *ix.cx,
        id: target,
    };
    let age = state(t.mob_ref()).tadpole_age;
    let left = (24000 - age).max(0);
    // `AgeableMob.getSpeedUpSecondsWhenFeeding`: a tenth of the remaining seconds.
    let seconds = (left / 20) / 10;
    set_tadpole_age(&mut t, age + seconds * 20);
    Interaction::Success
}

/// Synched data: frogs baby (16), variant (17), tongue target (18); tadpoles from bucket (16),
/// age locked (17).
pub fn encode(w: &mut DataWriter<'_>, m: &MobData) {
    let s = state(m);
    if m.kind == Kind::Tadpole {
        w.boolean(16, s.from_bucket, false);
        w.boolean(17, s.playing_dead, false);
        return;
    }
    w.boolean(16, m.baby, false);
    w.varint(
        17,
        crate::player::encode::serializer::FROG_VARIANT,
        s.variant,
        1,
    );
    // `OPTIONAL_UNSIGNED_INT`: 0 for empty, else the network id + 1.
    let tongue = s.tongue_target.map_or(0, |_| 1);
    w.raw(
        18,
        crate::player::encode::serializer::OPTIONAL_UNSIGNED_INT,
        tongue == 0,
        |o| {
            vanillite_protocol::codec::write_varint(o, tongue_net_id(s));
        },
    );
}

/// The tongue target as `OptionalInt` wire value (network id + 1, 0 for none).
fn tongue_net_id(s: &super::State) -> i32 {
    s.tongue_net_id
}

pub fn save(m: &MobData, out: &mut Compound) {
    let s = state(m);
    if m.kind == Kind::Tadpole {
        out.insert("Age", s.tadpole_age);
        out.insert("AgeLocked", s.playing_dead);
        out.insert("FromBucket", s.from_bucket);
        return;
    }
    let v = usize::try_from(s.variant)
        .ok()
        .and_then(|i| VARIANTS.get(i))
        .copied()
        .unwrap_or("temperate");
    out.insert("variant", format!("minecraft:{v}"));
}

pub fn load(m: &mut MobData, tag: &Compound) {
    if m.kind == Kind::Tadpole {
        let s = state_mut(m);
        s.tadpole_age = nbt_int(tag, "Age", 0);
        s.playing_dead = nbt_bool(tag, "AgeLocked", false);
        return;
    }
    if let Some(Tag::String(v)) = tag.get("variant") {
        let name = v.strip_prefix("minecraft:").unwrap_or(v);
        if let Some(i) = VARIANTS.iter().position(|n| *n == name) {
            state_mut(m).variant = i32::try_from(i).unwrap_or(1);
        }
    }
}

// --- Behaviours -----------------------------------------------------------------------------

pub fn duration(b: &Behavior) -> (i32, i32) {
    match b {
        Behavior::Croak { .. } | Behavior::ShootTongue { .. } => (100, 100),
        _ => (60, 60),
    }
}

pub fn shape(b: &Behavior) -> Shape {
    match b {
        Behavior::Croak { .. } | Behavior::ShootTongue { .. } => Shape::Timed,
        _ => Shape::OneShot,
    }
}

pub fn requirements(b: &Behavior) -> &'static [(M, MemoryStatus)] {
    match b {
        Behavior::Croak { .. } => &[(M::WalkTarget, S::ValueAbsent)],
        Behavior::ShootTongue { .. } => &[
            (M::WalkTarget, S::ValueAbsent),
            (M::LookTarget, S::Registered),
            (M::AttackTarget, S::ValuePresent),
            (M::IsPanicking, S::ValueAbsent),
        ],
        Behavior::LaySpawn => &[
            (M::AttackTarget, S::ValueAbsent),
            (M::WalkTarget, S::ValuePresent),
            (M::IsPregnant, S::ValuePresent),
        ],
        Behavior::FindLandNearWater { .. } => &[
            (M::AttackTarget, S::ValueAbsent),
            (M::WalkTarget, S::ValueAbsent),
            (M::LookTarget, S::Registered),
        ],
    }
}

pub fn no_timeout(_b: &Behavior) -> bool {
    false
}

fn pose(t: &Tick<'_, '_>) -> Pose {
    t.core().pose()
}

pub fn check(b: &mut Behavior, t: &mut Tick<'_, '_>) -> bool {
    match b {
        Behavior::Croak { .. } => pose(t) == Pose::Standing,
        Behavior::ShootTongue { .. } => {
            let Some(target) = brain_ref(t).entity(M::AttackTarget) else {
                return false;
            };
            let reachable = bh::entity_pos(t, target).is_some_and(|p| {
                crate::mob::nav::create_path_to(t, BlockPos::containing(p.x, p.y, p.z), 0)
                    .is_some_and(|path| path.dist_to_target < 1.75)
            });
            if !reachable {
                brain::brain(t).erase(M::AttackTarget);
                add_unreachable(t, target);
            }
            reachable && pose(t) != Pose::Croaking && can_eat(t, target)
        }
        _ => true,
    }
}

/// `addUnreachableTargetToMemory`: up to 5 for 100 ticks.
fn add_unreachable(t: &mut Tick<'_, '_>, target: EntityId) {
    let Some(uuid) = t.ents.store.get(target).map(|e| e.uuid()) else {
        return;
    };
    let mut list = match brain_ref(t).get(M::UnreachableTongueTargets) {
        Some(MemValue::Uuids(v)) => v.clone(),
        _ => Vec::new(),
    };
    let add = !list.contains(&uuid);
    if list.len() == 5 && add {
        list.remove(0);
    }
    if add {
        list.push(uuid);
    }
    brain::brain(t).set_with_expiry(M::UnreachableTongueTargets, MemValue::Uuids(list), 100);
}

pub fn can_still_use(b: &mut Behavior, t: &mut Tick<'_, '_>, _now: i64) -> bool {
    match b {
        Behavior::Croak { counter } => *counter < 60,
        Behavior::ShootTongue { state, .. } => {
            let br = brain_ref(t);
            br.has(M::AttackTarget) && *state != 3 && !br.has(M::IsPanicking)
        }
        _ => false,
    }
}

fn set_pose(t: &mut Tick<'_, '_>, pose: Pose) {
    if t.core().pose() != pose {
        t.core_mut().set_pose(pose);
    }
}

fn set_tongue_target(t: &mut Tick<'_, '_>, target: Option<EntityId>) {
    let net = target
        .and_then(|id| t.ents.store.get(id))
        .map_or(0, |e| e.network_id().0 + 1);
    let s = state_mut(t.mob());
    if s.tongue_target != target || s.tongue_net_id != net {
        s.tongue_target = target;
        s.tongue_net_id = net;
        t.core_mut().mark_data_dirty();
    }
}

pub fn start(b: &mut Behavior, t: &mut Tick<'_, '_>, _now: i64) {
    match b {
        Behavior::Croak { counter } => {
            let f = t.mob_ref().fluids;
            if !(f.in_water() || f.in_lava()) {
                set_pose(t, Pose::Croaking);
                *counter = 0;
            }
        }
        Behavior::ShootTongue {
            state,
            path_counter,
            ..
        } => {
            let Some(target) = brain_ref(t).entity(M::AttackTarget) else {
                return;
            };
            brain::brain(t).set(M::LookTarget, MemValue::Look(Tracker::entity(target, true)));
            set_tongue_target(t, Some(target));
            if let Some(p) = bh::entity_pos(t, target) {
                brain::brain(t).set(M::WalkTarget, MemValue::Walk(WalkTarget::at(p, 2.0, 0)));
            }
            *path_counter = 10;
            *state = 0;
        }
        _ => {}
    }
}

pub fn tick(b: &mut Behavior, t: &mut Tick<'_, '_>, _now: i64) {
    match b {
        Behavior::Croak { counter } => *counter += 1,
        Behavior::ShootTongue {
            state,
            eat_timer,
            path_counter,
        } => {
            let Some(target) = brain_ref(t).entity(M::AttackTarget) else {
                return;
            };
            set_tongue_target(t, Some(target));
            let Some(tp) = bh::entity_pos(t, target) else {
                return;
            };
            match *state {
                0 => {
                    if bh::dist_sqr(t, target).sqrt() < 1.75 {
                        sound_at(
                            t,
                            "minecraft:entity.frog.tongue",
                            SoundSource::Neutral,
                            2.0,
                            1.0,
                        );
                        set_pose(t, Pose::UsingTongue);
                        let me = t.pos();
                        let d = Vec3::new(me.x - tp.x, me.y - tp.y, me.z - tp.z).normalize() * 0.75;
                        if let Some(mut e) = t.ents.store.get_mut(target) {
                            e.set_velocity(d);
                            e.set_flags(EntityFlags::HAS_IMPULSE, true);
                        }
                        *eat_timer = 0;
                        *state = 1;
                    } else if *path_counter <= 0 {
                        brain::brain(t)
                            .set(M::WalkTarget, MemValue::Walk(WalkTarget::at(tp, 2.0, 0)));
                        *path_counter = 10;
                    } else {
                        *path_counter -= 1;
                    }
                }
                1 => {
                    let timer = *eat_timer;
                    *eat_timer += 1;
                    if timer >= 6 {
                        *state = 2;
                        eat_entity(t);
                    }
                }
                2 => {
                    if *eat_timer >= 10 {
                        *state = 3;
                    } else {
                        *eat_timer += 1;
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
}

/// `ShootTongue.eatEntity`: the frog bites; a killed slime or magma cube drops a froglight.
fn eat_entity(t: &mut Tick<'_, '_>) {
    sound_at(
        t,
        "minecraft:entity.frog.eat",
        SoundSource::Neutral,
        2.0,
        1.0,
    );
    let Some(target) = state(t.mob_ref()).tongue_target else {
        return;
    };
    if !bh::is_alive(t, target) {
        return;
    }
    let target_kind = t.ents.mob(target).map(|m| m.kind);
    crate::mob::combat::mob_attack(t, target);
    if !t.exists() {
        return;
    }
    if !bh::is_alive(t, target) {
        // `MagmaCube`/`Slime` killed by a frog drop a froglight by the frog's variant.
        if target_kind == Some(Kind::MagmaCube) {
            let light = match state(t.mob_ref()).variant {
                0 => "minecraft:verdant_froglight",
                2 => "minecraft:pearlescent_froglight",
                _ => "minecraft:ochre_froglight",
            };
            if let Some(p) = bh::entity_pos(t, target) {
                let (uuid, y_rot, velocity) = crate::mob::new_item_entity();
                t.env.out.push(crate::survival::Effect::DropItem {
                    uuid,
                    pos: p,
                    velocity,
                    y_rot,
                    pickup_delay: 10,
                    stack: crate::mob::equipment::stack(light),
                });
            }
        }
        if let Some(m) = t.ents.mob_mut(target) {
            m.removed = true;
        }
        t.cx.discarded.push(target);
    }
}

pub fn stop(b: &mut Behavior, t: &mut Tick<'_, '_>, _now: i64) {
    match b {
        Behavior::Croak { .. } => set_pose(t, Pose::Standing),
        Behavior::ShootTongue { .. } => {
            brain::brain(t).erase(M::AttackTarget);
            set_tongue_target(t, None);
            set_pose(t, Pose::Standing);
        }
        _ => {}
    }
}

pub fn trigger(b: &mut Behavior, t: &mut Tick<'_, '_>, now: i64) -> bool {
    match b {
        Behavior::LaySpawn => {
            if t.mob_ref().fluids.in_water() || !t.on_ground() {
                return false;
            }
            let below = BlockPos::new(t.block_pos().x, t.block_pos().y - 1, t.block_pos().z);
            let tags = crate::block::block_tags();
            for (dx, dz) in [(0, -1), (1, 0), (0, 1), (-1, 0)] {
                let rel = BlockPos::new(below.x + dx, below.y, below.z + dz);
                let state = t.level.block(rel);
                let supports =
                    crate::mob::is_water(state) || tags.supports_frogspawn.contains(state);
                if state.collision_shape().is_empty() && supports {
                    let spawn = BlockPos::new(rel.x, rel.y + 1, rel.z);
                    if t.level.block(spawn).is_air()
                        && let Some(id) = vanillite_data::registry::BLOCK.id("minecraft:frogspawn")
                    {
                        let block = vanillite_data::block::BlockId(id).default_state();
                        t.cx.actions
                            .push(crate::mob::WorldAction::SetBlock(spawn, block, 3));
                        sound_at(
                            t,
                            "minecraft:entity.frog.lay_spawn",
                            SoundSource::Blocks,
                            1.0,
                            1.0,
                        );
                        brain::brain(t).erase(M::IsPregnant);
                        return true;
                    }
                }
            }
            true
        }
        Behavior::FindLandNearWater {
            range,
            speed,
            next_ok,
        } => {
            if crate::mob::is_water(t.level.block(t.block_pos())) {
                return false;
            }
            if now < *next_ok {
                *next_ok = now + 40;
                return true;
            }
            let me = t.block_pos();
            let mut found = None;
            for_manhattan(me, *range, *range, |p| {
                if p.x == me.x && p.z == me.z {
                    return false;
                }
                if !t.level.block(p).collision_shape().is_empty() {
                    return false;
                }
                if t.level
                    .block(BlockPos::new(p.x, p.y - 1, p.z))
                    .collision_shape()
                    .is_empty()
                {
                    return false;
                }
                for (dx, dz) in [(0, -1), (1, 0), (0, 1), (-1, 0)] {
                    let side = BlockPos::new(p.x + dx, p.y, p.z + dz);
                    let below = BlockPos::new(side.x, side.y - 1, side.z);
                    if t.level.block(side).is_air() && crate::mob::is_water(t.level.block(below)) {
                        found = Some(p);
                        return true;
                    }
                }
                false
            });
            if let Some(p) = found {
                let br = brain::brain(t);
                br.set(M::LookTarget, MemValue::Look(Tracker::block(p)));
                br.set(
                    M::WalkTarget,
                    MemValue::Walk(WalkTarget::block(p, *speed, 0)),
                );
            }
            *next_ok = now + 40;
            true
        }
        _ => false,
    }
}
