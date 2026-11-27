//! Goats (`Goat`, `GoatAi`) with ramming (`PrepareRamNearestTarget`, `RamTarget`) and long
//! jumps (`LongJumpToRandomPos`, `LongJumpMidJump`, shared with frogs through
//! `LongJumpToPreferredBlock`).

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Vec3};
use vanillite_nbt::Compound;
use vanillite_protocol::packets::play::clientbound::SoundSource;
use vanillite_world::entity::{EntityFlags, EntityId, Pose};

use super::{SpawnContext, entity_event, nbt_bool, spec, state, state_mut, voice_pitch};
use crate::mob::animal::Interaction;
use crate::mob::brain::behavior::{Shape, Spec};
use crate::mob::brain::memory::MemoryStatus as S;
use crate::mob::brain::{
    self, Activity, ActivitySpec, Brain, BrainSpec, MemValue, MemoryStatus, MemoryType as M,
    SensorKind, Tracker, WalkTarget, brain_ref, sensing,
};
use crate::mob::fam::Interact;
use crate::mob::path::{MalusTable, PathType};
use crate::mob::{Kind, MobData, Tick, nav};
use crate::player::encode::DataWriter;

/// `setPathfindingMalus`: powder snow is off limits.
pub const MALUS: &MalusTable = &[
    (PathType::PowderSnow, -1.0),
    (PathType::OnTopOfPowderSnow, -1.0),
];

/// Which jumper's sounds and landing rules a long jump uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Jumper {
    Goat,
    Frog,
}

/// The goat behaviours (and the long jumps frogs share).
#[derive(Clone, Copy, Debug)]
pub enum Behavior {
    /// `PrepareRamNearestTarget`.
    PrepareRam {
        candidate: Option<(BlockPos, BlockPos, EntityId)>,
        reached_at: Option<i64>,
    },
    /// `RamTarget`.
    Ram { direction: Vec3 },
    /// `LongJumpMidJump(timeBetween, landingSound)`.
    MidJump { jumper: Jumper },
    /// `LongJumpToRandomPos` / `LongJumpToPreferredBlock`.
    LongJump {
        jumper: Jumper,
        initial: Option<Vec3>,
        chosen: Option<Vec3>,
        tries: i32,
        prepare_start: i64,
        wanting_preferred: bool,
    },
}

fn goat(b: Behavior) -> Spec {
    spec(super::Behavior::Goat(b))
}

/// A long jump behaviour pair for a jumper: `(MidJump, LongJump)` specs.
pub fn long_jump_specs(jumper: Jumper) -> (Spec, Spec) {
    (
        goat(Behavior::MidJump { jumper }),
        goat(Behavior::LongJump {
            jumper,
            initial: None,
            chosen: None,
            tries: 0,
            prepare_start: 0,
            wanting_preferred: false,
        }),
    )
}

pub(super) fn brain_spec() -> BrainSpec {
    let core = ActivitySpec::new(
        Activity::Core,
        0,
        vec![
            Spec::swim(0.8),
            Spec::animal_panic(2.0),
            Spec::look_at_target_sink(45, 90),
            Spec::move_to_target_sink(),
            Spec::count_down(M::TemptationCooldownTicks),
            Spec::count_down(M::LongJumpCooldownTicks),
            Spec::count_down(M::RamCooldownTicks),
        ],
    );
    let idle = ActivitySpec::with_priorities(
        Activity::Idle,
        vec![
            (
                0,
                Spec::look_at_entity_sometimes(Some(is_player), 6.0, 30, 60),
            ),
            (0, Spec::make_love(Kind::Goat, 1.0, 2)),
            (1, Spec::follow_temptation(|_| 1.25, 2.5, false)),
            (2, Spec::baby_follow_adult(5, 16, 1.25)),
            (
                3,
                Spec::run_one(vec![
                    (Spec::stroll(1.0), 2),
                    (Spec::walk_to_look_target(1.0, 3), 2),
                    (Spec::do_nothing(30, 60), 1),
                ]),
            ),
        ],
    )
    .requires(&[
        (M::RamCooldownTicks, S::ValuePresent),
        (M::LongJumpCooldownTicks, S::ValuePresent),
    ]);
    let (mid, jump) = long_jump_specs(Jumper::Goat);
    let long_jump = ActivitySpec::with_priorities(Activity::LongJump, vec![(0, mid), (1, jump)])
        .requires(&[
            (M::TemptingPlayer, S::ValueAbsent),
            (M::BreedTarget, S::ValueAbsent),
            (M::WalkTarget, S::ValueAbsent),
            (M::LongJumpCooldownTicks, S::ValueAbsent),
        ]);
    let ram = ActivitySpec::with_priorities(
        Activity::Ram,
        vec![
            (
                0,
                goat(Behavior::Ram {
                    direction: Vec3::ZERO,
                }),
            ),
            (
                1,
                goat(Behavior::PrepareRam {
                    candidate: None,
                    reached_at: None,
                }),
            ),
        ],
    )
    .requires(&[
        (M::TemptingPlayer, S::ValueAbsent),
        (M::BreedTarget, S::ValueAbsent),
        (M::RamCooldownTicks, S::ValueAbsent),
    ]);
    BrainSpec {
        sensors: vec![
            SensorKind::NearestLivingEntities,
            SensorKind::NearestPlayers,
            SensorKind::NearestItems,
            SensorKind::NearestAdult,
            SensorKind::HurtBy,
            SensorKind::Tempting(None),
        ],
        activities: vec![core, idle, long_jump, ram],
        ..BrainSpec::default()
    }
}

fn is_player(t: &Tick<'_, '_>, id: EntityId) -> bool {
    t.ents.player(id).is_some()
}

pub fn init(m: &mut MobData) {
    m.brain = Some(Box::new(Brain::new(brain_spec(), &mut m.random)));
    m.nav.can_float = true;
    let s = state_mut(m);
    s.left_horn = true;
    s.right_horn = true;
}

/// `GoatAi.initMemories`.
fn init_memories(m: &mut MobData, random: &mut impl RandomSource) {
    let jump = random.next_int_between_inclusive(600, 1200);
    let ram = random.next_int_between_inclusive(600, 6000);
    if let Some(b) = m.brain.as_deref_mut() {
        b.set(M::LongJumpCooldownTicks, MemValue::Int(jump));
        b.set(M::RamCooldownTicks, MemValue::Int(ram));
    }
}

/// `finalizeSpawn`: cooldowns, 2 % screamers, 10 % one horn, then `AgeableMob`'s babies.
pub fn finalize_spawn(m: &mut MobData, ctx: &mut SpawnContext<'_>) {
    let mut r = m.random.clone();
    init_memories(m, &mut r);
    m.random = r;
    let screaming = m.random.next_double() < 0.02;
    state_mut(m).screaming = screaming;
    if !m.baby && m.random.next_float() < 0.1 {
        let left = m.random.next_boolean();
        let s = state_mut(m);
        if left {
            s.left_horn = false;
        } else {
            s.right_horn = false;
        }
    }
    if ctx.group_index > 0 && m.random.next_float() <= 0.05 {
        m.age = -24000;
        m.baby = true;
    }
    age_boundary(m);
}

fn age_boundary(m: &mut MobData) {
    m.attrs.attack_damage = if m.baby { 1.0 } else { 2.0 };
}

/// `getBreedOffspring`: cooldowns and screaming from a random parent or 2 %.
pub fn breed_offspring(
    parent: &MobData,
    partner: &MobData,
    child: &mut MobData,
    random: &mut vanillite_base::random::LegacyRandom,
) {
    init_memories(child, random);
    let selected = if random.next_boolean() {
        parent
    } else {
        partner
    };
    let screaming = state(selected).screaming || random.next_double() < 0.02;
    state_mut(child).screaming = screaming;
    age_boundary(child);
}

/// The kind's sounds (`goat.*` or `goat.screaming.*`).
pub fn sound(m: &MobData, what: &str) -> &'static str {
    let screaming = state(m).screaming;
    match (what, screaming) {
        ("ambient", false) => "minecraft:entity.goat.ambient",
        ("ambient", true) => "minecraft:entity.goat.screaming.ambient",
        ("hurt", false) => "minecraft:entity.goat.hurt",
        ("hurt", true) => "minecraft:entity.goat.screaming.hurt",
        ("death", false) => "minecraft:entity.goat.death",
        ("death", true) => "minecraft:entity.goat.screaming.death",
        (_, false) => "minecraft:entity.goat.ambient",
        (_, true) => "minecraft:entity.goat.screaming.ambient",
    }
}

/// `GoatAi.updateActivity`.
pub fn update_activity(t: &mut Tick<'_, '_>) {
    brain::brain(t).set_active_activity_to_first_valid(&[
        Activity::Ram,
        Activity::LongJump,
        Activity::Idle,
    ]);
}

/// `aiStep`'s lowering head counter.
pub fn ai_step(t: &mut Tick<'_, '_>) {
    let s = state_mut(t.mob());
    s.lower_head_tick = s.lower_head_tick.clamp(0, 20);
    let m = t.mob();
    let expected = if m.baby { 1.0 } else { 2.0 };
    #[allow(clippy::float_cmp)]
    if m.attrs.attack_damage != expected && !m.effects.has(crate::effect::MobEffect::STRENGTH) {
        age_boundary(m);
    }
}

/// `Goat.mobInteract`: milk, then feeding.
pub fn interact(ix: &mut Interact<'_, '_>) -> Interaction {
    let name = crate::item::item_id(ix.stack).name();
    let baby = ix.ents.mob(ix.target).is_some_and(|m| m.baby);
    if name == "minecraft:bucket" && !baby {
        let screaming = ix.ents.mob(ix.target).is_some_and(|m| state(m).screaming);
        let sound = if screaming {
            "minecraft:entity.goat.screaming.milk"
        } else {
            "minecraft:entity.goat.milk"
        };
        if let Some(pos) = ix.ents.store.get(ix.player).map(|e| e.pos()) {
            let seed = crate::survival::sound_seed();
            crate::survival::play_sound(
                ix.env.out,
                None,
                pos,
                sound,
                SoundSource::Players,
                1.0,
                1.0,
                seed,
            );
        }
        crate::mob::animal::fill_held_pub(ix, "minecraft:milk_bucket");
        return Interaction::Success;
    }
    let food = crate::mob::animal::is_food(Kind::Goat, ix.stack.item);
    let r = crate::mob::animal::animal_interact_pub(ix);
    if r.consumes() && food {
        let screaming = ix.ents.mob(ix.target).is_some_and(|m| state(m).screaming);
        let sound = if screaming {
            "minecraft:entity.goat.screaming.eat"
        } else {
            "minecraft:entity.goat.eat"
        };
        if ix.ents.store.get(ix.target).is_some() {
            let pitch = 0.8 + ix.env.random.next_float() * 0.4;
            crate::survival::play_entity_sound(
                ix.env.out,
                ix.ents,
                None,
                ix.target,
                sound,
                SoundSource::Neutral,
                1.0,
                pitch,
            );
        }
    }
    r
}

/// Synched data: baby (16), screaming (17), left horn (18), right horn (19).
pub fn encode(w: &mut DataWriter<'_>, m: &MobData) {
    let s = state(m);
    w.boolean(16, m.baby, false);
    w.boolean(17, s.screaming, false);
    w.boolean(18, s.left_horn, true);
    w.boolean(19, s.right_horn, true);
}

pub fn save(m: &MobData, out: &mut Compound) {
    let s = state(m);
    out.insert("IsScreamingGoat", s.screaming);
    out.insert("HasLeftHorn", s.left_horn);
    out.insert("HasRightHorn", s.right_horn);
}

pub fn load(m: &mut MobData, tag: &Compound) {
    let s = state_mut(m);
    s.screaming = nbt_bool(tag, "IsScreamingGoat", false);
    s.left_horn = nbt_bool(tag, "HasLeftHorn", true);
    s.right_horn = nbt_bool(tag, "HasRightHorn", true);
    age_boundary(m);
}

// --- Behaviours -----------------------------------------------------------------------------

pub fn duration(b: &Behavior) -> (i32, i32) {
    match b {
        Behavior::PrepareRam { .. } => (160, 160),
        Behavior::Ram { .. } | Behavior::LongJump { .. } => (200, 200),
        Behavior::MidJump { .. } => (100, 100),
    }
}

pub fn shape(_b: &Behavior) -> Shape {
    Shape::Timed
}

pub fn requirements(b: &Behavior) -> &'static [(M, MemoryStatus)] {
    match b {
        Behavior::PrepareRam { .. } => &[
            (M::LookTarget, S::Registered),
            (M::RamCooldownTicks, S::ValueAbsent),
            (M::NearestVisibleLivingEntities, S::ValuePresent),
            (M::RamTarget, S::ValueAbsent),
        ],
        Behavior::Ram { .. } => &[
            (M::RamCooldownTicks, S::ValueAbsent),
            (M::RamTarget, S::ValuePresent),
        ],
        Behavior::MidJump { .. } => &[
            (M::LookTarget, S::Registered),
            (M::LongJumpMidJump, S::ValuePresent),
        ],
        Behavior::LongJump { .. } => &[
            (M::LookTarget, S::Registered),
            (M::LongJumpCooldownTicks, S::ValueAbsent),
            (M::LongJumpMidJump, S::ValueAbsent),
        ],
    }
}

pub fn no_timeout(_b: &Behavior) -> bool {
    false
}

fn screaming(t: &Tick<'_, '_>) -> bool {
    t.mob_ref().kind == Kind::Goat && state(t.mob_ref()).screaming
}

/// `TIME_BETWEEN_RAMS` (screamers ram more often).
fn time_between_rams(t: &Tick<'_, '_>) -> (i32, i32) {
    if screaming(t) {
        (100, 300)
    } else {
        (600, 6000)
    }
}

/// `TIME_BETWEEN_LONG_JUMPS`: goats 600–1200, frogs 100–140.
fn time_between_jumps(jumper: Jumper) -> (i32, i32) {
    match jumper {
        Jumper::Goat => (600, 1200),
        Jumper::Frog => (100, 140),
    }
}

/// `RAM_TARGET_CONDITIONS`: combat targets except goats (armor stands are no living
/// entities here).
fn ram_targetable(t: &mut Tick<'_, '_>, id: EntityId) -> bool {
    if t.ents.mob(id).is_some_and(|m| m.kind == Kind::Goat) {
        return false;
    }
    sensing::test(t, id, true, -1.0, true, true)
}

pub fn check(b: &mut Behavior, t: &mut Tick<'_, '_>) -> bool {
    match b {
        Behavior::Ram { .. } => brain_ref(t).has(M::RamTarget),
        Behavior::LongJump { jumper, .. } => {
            let honey = t.level.block(t.block_pos()).block().name() == "minecraft:honey_block";
            let f = t.mob_ref().fluids;
            let ok = t.on_ground() && !f.in_water() && !f.in_lava() && !honey;
            if !ok {
                let (lo, hi) = time_between_jumps(*jumper);
                let cd = t.env.random.next_int_between_inclusive(lo, hi) / 2;
                brain::brain(t).set(M::LongJumpCooldownTicks, MemValue::Int(cd));
            }
            ok
        }
        _ => true,
    }
}

pub fn can_still_use(b: &mut Behavior, t: &mut Tick<'_, '_>, _now: i64) -> bool {
    match b {
        Behavior::PrepareRam { candidate, .. } => {
            candidate.is_some_and(|(_, _, target)| crate::mob::brain::behavior::is_alive(t, target))
        }
        Behavior::Ram { .. } => brain_ref(t).has(M::RamTarget),
        Behavior::MidJump { .. } => !t.on_ground(),
        Behavior::LongJump {
            jumper,
            initial,
            chosen,
            tries,
            ..
        } => {
            let has_candidates = !state(t.mob_ref()).jump_candidates.is_empty();
            let valid = initial.is_some_and(|p| p == t.pos())
                && *tries > 0
                && !t.mob_ref().fluids.in_water()
                && (chosen.is_some() || has_candidates);
            if !valid && !brain_ref(t).has(M::LongJumpMidJump) {
                let (lo, hi) = time_between_jumps(*jumper);
                let cd = t.env.random.next_int_between_inclusive(lo, hi) / 2;
                let b = brain::brain(t);
                b.set(M::LongJumpCooldownTicks, MemValue::Int(cd));
                b.erase(M::LookTarget);
            }
            valid
        }
    }
}

pub fn start(b: &mut Behavior, t: &mut Tick<'_, '_>, _now: i64) {
    match b {
        Behavior::PrepareRam {
            candidate,
            reached_at,
        } => {
            // `findClosest(ramTargeting)`: the targeting test needs the mutable tick.
            let mut chosen = None;
            let list = brain_ref(t)
                .entities(M::NearestVisibleLivingEntities)
                .to_vec();
            for id in list {
                if ram_targetable(t, id) && sensing::visible_contains(t, id) {
                    chosen = Some(id);
                    break;
                }
            }
            if let Some(target) = chosen {
                choose_ram_position(t, target, candidate, reached_at);
            }
        }
        Behavior::Ram { direction } => {
            let cur = t.block_pos();
            let Some(target) = brain_ref(t).vec3(M::RamTarget) else {
                return;
            };
            let d = Vec3::new(
                f64::from(cur.x) - target.x,
                0.0,
                f64::from(cur.z) - target.z,
            );
            *direction = d.normalize();
            brain::brain(t).set(
                M::WalkTarget,
                MemValue::Walk(WalkTarget::at(target, 3.0, 0)),
            );
        }
        Behavior::MidJump { .. } => {
            t.mob().discard_friction = true;
            t.core_mut().set_pose(Pose::LongJumping);
        }
        Behavior::LongJump {
            jumper,
            initial,
            chosen,
            tries,
            wanting_preferred,
            ..
        } => {
            *chosen = None;
            *tries = 20;
            *initial = Some(t.pos());
            let me = t.block_pos();
            // `maxLongJumpWidth`, `maxLongJumpHeight`.
            let (w, h) = match *jumper {
                Jumper::Goat => (5, 5),
                Jumper::Frog => (4, 2),
            };
            let s = state_mut(t.mob());
            s.jump_candidates.clear();
            s.not_preferred.clear();
            // `BlockPos.betweenClosedStream` order: x runs first, then y, then z.
            for z in me.z - w..=me.z + w {
                for y in me.y - h..=me.y + h {
                    for x in me.x - w..=me.x + w {
                        let p = BlockPos::new(x, y, z);
                        if p != me {
                            #[allow(clippy::cast_possible_truncation)]
                            let weight = me.dist_sqr(p).ceil() as i32;
                            s.jump_candidates.push((p, weight));
                        }
                    }
                }
            }
            if *jumper == Jumper::Frog {
                // `LongJumpToPreferredBlock.start`.
                *wanting_preferred = t.mob().random.next_float() < 0.5;
            }
        }
    }
}

pub fn tick(b: &mut Behavior, t: &mut Tick<'_, '_>, now: i64) {
    match b {
        Behavior::PrepareRam {
            candidate,
            reached_at,
        } => prepare_ram_tick(t, candidate, reached_at, now),
        Behavior::Ram { direction } => ram_tick(t, *direction),
        Behavior::MidJump { .. } => {}
        Behavior::LongJump {
            jumper,
            chosen,
            tries,
            prepare_start,
            wanting_preferred,
            ..
        } => {
            if let Some(jump) = *chosen {
                if now - *prepare_start >= 40 {
                    let body = t.mob_ref().y_body_rot;
                    let mut rot = t.core().rot();
                    rot.y_rot = body;
                    t.core_mut().set_rot(rot);
                    t.mob().discard_friction = true;
                    let len = jump.length();
                    let boost = t
                        .mob_ref()
                        .effects
                        .amplifier(crate::effect::MobEffect::JUMP_BOOST)
                        .map_or(0.0, |a| 0.1 * (f64::from(a) + 1.0));
                    let v = jump * ((len + boost) / len);
                    t.set_velocity(v);
                    t.core_mut().set_flags(EntityFlags::HAS_IMPULSE, true);
                    brain::brain(t).set(M::LongJumpMidJump, MemValue::Bool(true));
                    let sound = match *jumper {
                        Jumper::Goat if screaming(t) => "minecraft:entity.goat.screaming.long_jump",
                        Jumper::Goat => "minecraft:entity.goat.long_jump",
                        Jumper::Frog => "minecraft:entity.frog.long_jump",
                    };
                    super::entity_sound_at(t, sound, SoundSource::Neutral, 1.0, 1.0);
                }
            } else {
                *tries -= 1;
                pick_candidate(t, *jumper, *wanting_preferred, chosen, prepare_start, now);
            }
        }
    }
}

pub fn stop(b: &mut Behavior, t: &mut Tick<'_, '_>, _now: i64) {
    match b {
        Behavior::PrepareRam { .. } => {
            if !brain_ref(t).has(M::RamTarget) {
                entity_event(t, 59);
                let cd = time_between_rams(t).0;
                brain::brain(t).set(M::RamCooldownTicks, MemValue::Int(cd));
            }
        }
        Behavior::Ram { .. } => {}
        Behavior::MidJump { jumper } => {
            if t.on_ground() {
                let v = t.velocity();
                t.set_velocity(v.multiply(f64::from(0.1_f32), 1.0, f64::from(0.1_f32)));
                let sound = match *jumper {
                    Jumper::Goat => "minecraft:entity.goat.step",
                    Jumper::Frog => "minecraft:entity.frog.step",
                };
                super::entity_sound_at(t, sound, SoundSource::Neutral, 2.0, 1.0);
            }
            t.mob().discard_friction = false;
            t.core_mut().set_pose(Pose::Standing);
            let (lo, hi) = time_between_jumps(*jumper);
            let cd = t.env.random.next_int_between_inclusive(lo, hi);
            let b = brain::brain(t);
            b.erase(M::LongJumpMidJump);
            b.set(M::LongJumpCooldownTicks, MemValue::Int(cd));
        }
        Behavior::LongJump { .. } => {}
    }
}

pub fn trigger(_b: &mut Behavior, _t: &mut Tick<'_, '_>, _now: i64) -> bool {
    false
}

/// `PrepareRamNearestTarget.isWalkableBlock`.
fn walkable(t: &Tick<'_, '_>, pos: BlockPos) -> bool {
    nav::is_stable_destination(t, pos)
        && crate::mob::path::malus_of(
            t.mob_ref().kind.malus(),
            crate::mob::path::path_type_static(t.level, t.level.min_y(), pos.x, pos.y, pos.z),
        ) == 0.0
}

/// `chooseRamPosition` with `calculateRammingStartPosition`.
fn choose_ram_position(
    t: &mut Tick<'_, '_>,
    target: EntityId,
    candidate: &mut Option<(BlockPos, BlockPos, EntityId)>,
    reached_at: &mut Option<i64>,
) {
    *reached_at = None;
    *candidate = None;
    let Some(tp) = crate::mob::brain::behavior::entity_pos(t, target) else {
        return;
    };
    let target_pos = BlockPos::containing(tp.x, tp.y, tp.z);
    if !walkable(t, target_pos) {
        return;
    }
    let mut possible = Vec::new();
    // `Direction.Plane.HORIZONTAL`: north, east, south, west.
    for (dx, dz) in [(0, -1), (1, 0), (0, 1), (-1, 0)] {
        let mut p = target_pos;
        for _ in 0..7 {
            let next = BlockPos::new(p.x + dx, p.y, p.z + dz);
            if !walkable(t, next) {
                break;
            }
            p = next;
        }
        // The loop moved one past the last walkable block and back.
        let manhattan = (p.x - target_pos.x).abs() + (p.z - target_pos.z).abs();
        if manhattan >= 4 {
            possible.push(p);
        }
    }
    let me = t.block_pos();
    possible.sort_by(|a, b| me.dist_sqr(*a).total_cmp(&me.dist_sqr(*b)));
    for p in possible {
        let path = nav::create_path_to(t, p, 0);
        if path.is_some_and(|p| p.reached) {
            *candidate = Some((p, target_pos, target));
            return;
        }
    }
}

fn prepare_ram_tick(
    t: &mut Tick<'_, '_>,
    candidate: &mut Option<(BlockPos, BlockPos, EntityId)>,
    reached_at: &mut Option<i64>,
    now: i64,
) {
    let Some((start, target_pos, target)) = *candidate else {
        return;
    };
    {
        let b = brain::brain(t);
        b.set(
            M::WalkTarget,
            MemValue::Walk(WalkTarget::block(start, 1.25, 0)),
        );
        b.set(M::LookTarget, MemValue::Look(Tracker::entity(target, true)));
    }
    let now_target = crate::mob::brain::behavior::entity_pos(t, target)
        .map(|p| BlockPos::containing(p.x, p.y, p.z));
    if now_target != Some(target_pos) {
        entity_event(t, 59);
        t.nav().stop();
        choose_ram_position(t, target, candidate, reached_at);
    } else if t.block_pos() == start {
        entity_event(t, 58);
        let reached = *reached_at.get_or_insert(now);
        if now - reached >= 20 {
            let sx = f64::from((target_pos.x - start.x).signum()) * 0.5;
            let sz = f64::from((target_pos.z - start.z).signum()) * 0.5;
            let c = super::bottom_center(target_pos);
            brain::brain(t).set(
                M::RamTarget,
                MemValue::Vec3(Vec3::new(c.x + sx, c.y, c.z + sz)),
            );
            let sound = if screaming(t) {
                "minecraft:entity.goat.screaming.prepare_ram"
            } else {
                "minecraft:entity.goat.prepare_ram"
            };
            let pitch = voice_pitch(t.mob());
            super::entity_sound_at(t, sound, SoundSource::Neutral, 1.0, pitch);
            *candidate = None;
        }
    }
}

fn ram_tick(t: &mut Tick<'_, '_>, direction: Vec3) {
    let aabb = t.aabb();
    let mut hits = Vec::new();
    t.ents.store.for_each_in(&aabb, false, |id| hits.push(id));
    let mut target = None;
    for id in hits {
        if id != t.id
            && (t.ents.player(id).is_some() || t.ents.mob(id).is_some())
            && t.ents
                .store
                .get(id)
                .is_some_and(|e| e.aabb().intersects(&aabb))
            && ram_targetable(t, id)
        {
            target = Some(id);
            break;
        }
    }
    let impact = if screaming(t) {
        "minecraft:entity.goat.screaming.ram_impact"
    } else {
        "minecraft:entity.goat.ram_impact"
    };
    if let Some(target) = target {
        #[allow(clippy::cast_possible_truncation)]
        let damage = t.mob_ref().attrs.attack_damage as f32;
        // `noAggroMobAttack`.
        let kind = t.env.types.named("minecraft:mob_attack_no_aggro");
        let source = crate::mob::combat::entity_source(t.ents, kind, t.id, t.id);
        crate::mob::combat::hurt(t.level, t.ents, t.env, t.cx, target, source, damage);
        if !t.exists() {
            return;
        }
        let fx = &t.mob_ref().effects;
        let speed_level = fx
            .amplifier(crate::effect::MobEffect::SPEED)
            .map_or(0, |a| a + 1);
        let slow_level = fx
            .amplifier(crate::effect::MobEffect::SLOWNESS)
            .map_or(0, |a| a + 1);
        #[allow(clippy::cast_precision_loss)]
        let boost = 0.25 * (speed_level - slow_level) as f32;
        let factor = (t.mob_ref().speed * 1.65).clamp(0.2, 3.0) + boost;
        let force = if t.mob_ref().baby { 1.0 } else { 2.5 };
        crate::mob::combat::knock_living(
            t.ents,
            t.env,
            target,
            f64::from(factor) * force,
            direction.x,
            direction.z,
            None,
        );
        finish_ram(t);
        super::entity_sound_at(t, impact, SoundSource::Neutral, 1.0, 1.0);
    } else if rammed_horn_breaking_block(t) {
        super::entity_sound_at(t, impact, SoundSource::Neutral, 1.0, 1.0);
        if drop_horn(t) {
            super::entity_sound_at(
                t,
                "minecraft:entity.goat.horn_break",
                SoundSource::Neutral,
                1.0,
                1.0,
            );
        }
        finish_ram(t);
    } else {
        let b = brain_ref(t);
        let walk = b.walk_target();
        let ram = b.vec3(M::RamTarget);
        let lost = match (walk, ram) {
            (Some(w), Some(r)) => crate::mob::brain::behavior::tracker_pos(t, &w.target)
                .is_none_or(|p| p.distance_to_sqr(r) < 0.25 * 0.25),
            _ => true,
        };
        if lost {
            finish_ram(t);
        }
    }
}

fn finish_ram(t: &mut Tick<'_, '_>) {
    entity_event(t, 59);
    let (lo, hi) = time_between_rams(t);
    let cd = t.env.random.next_int_between_inclusive(lo, hi);
    let b = brain::brain(t);
    b.set(M::RamCooldownTicks, MemValue::Int(cd));
    b.erase(M::RamTarget);
}

/// `hasRammedHornBreakingBlock`: `#snaps_goat_horn` ahead or above.
fn rammed_horn_breaking_block(t: &Tick<'_, '_>) -> bool {
    let v = t.velocity();
    let h = Vec3::new(v.x, 0.0, v.z);
    let len = h.length();
    let n = if len < 1.0e-4 {
        Vec3::ZERO
    } else {
        h * (1.0 / len)
    };
    let p = t.pos();
    let ahead = BlockPos::containing(p.x + n.x, p.y + n.y, p.z + n.z);
    let tags = crate::block::block_tags();
    let snaps = |pos: BlockPos| tags.snaps_goat_horn.contains(t.level.block(pos));
    snaps(ahead) || snaps(BlockPos::new(ahead.x, ahead.y + 1, ahead.z))
}

/// `dropHorn`: a random remaining horn falls off as a goat horn item.
fn drop_horn(t: &mut Tick<'_, '_>) -> bool {
    if t.mob_ref().baby {
        return false;
    }
    let s = state(t.mob_ref());
    let (left, right) = (s.left_horn, s.right_horn);
    if !left && !right {
        return false;
    }
    let drop_left = if !left {
        false
    } else if !right {
        true
    } else {
        t.mob().random.next_boolean()
    };
    let s = state_mut(t.mob());
    if drop_left {
        s.left_horn = false;
    } else {
        s.right_horn = false;
    }
    t.core_mut().mark_data_dirty();
    let r = &mut t.mob().random;
    let dx = f64::from(-0.2 + r.next_float() * 0.4);
    let dy = f64::from(0.3 + r.next_float() * 0.4);
    let dz = f64::from(-0.2 + r.next_float() * 0.4);
    let pos = t.pos();
    let (uuid, y_rot, item_random) = crate::mob::new_item_entity_with_motion();
    let stack = create_horn(t.core().uuid(), state(t.mob_ref()).screaming);
    t.env.out.push(crate::survival::Effect::DropItem {
        random: item_random,
        uuid,
        pos,
        velocity: Vec3::new(dx, dy, dz),
        y_rot,
        pickup_delay: 0,
        stack,
    });
    true
}

/// `getJumpCandidate` (with `LongJumpToPreferredBlock`'s preference for frogs).
fn jump_candidate(
    t: &mut Tick<'_, '_>,
    jumper: Jumper,
    wanting_preferred: bool,
) -> Option<BlockPos> {
    if jumper != Jumper::Frog || !wanting_preferred {
        return weighted_pick(t);
    }
    let tags = crate::block::block_tags();
    while !state(t.mob_ref()).jump_candidates.is_empty() {
        if let Some(p) = weighted_pick(t) {
            if tags
                .frog_prefer_jump_to
                .contains(t.level.block(BlockPos::new(p.x, p.y - 1, p.z)))
            {
                return Some(p);
            }
            state_mut(t.mob()).not_preferred.push((p, 0));
        }
    }
    let s = state_mut(t.mob());
    (!s.not_preferred.is_empty()).then(|| s.not_preferred.remove(0).0)
}

/// `WeightedRandom.getRandomItem` with the level random; removes the pick.
fn weighted_pick(t: &mut Tick<'_, '_>) -> Option<BlockPos> {
    let total: i64 = state(t.mob_ref())
        .jump_candidates
        .iter()
        .map(|c| i64::from(c.1))
        .sum();
    if total <= 0 {
        state_mut(t.mob()).jump_candidates.clear();
        return None;
    }
    let mut roll = i64::from(
        t.env
            .random
            .next_int_bounded(i32::try_from(total).unwrap_or(i32::MAX)),
    );
    let s = state_mut(t.mob());
    let mut index = 0;
    for (i, c) in s.jump_candidates.iter().enumerate() {
        roll -= i64::from(c.1);
        if roll < 0 {
            index = i;
            break;
        }
    }
    Some(s.jump_candidates.remove(index).0)
}

/// `LongJumpToRandomPos.defaultAcceptableLandingSpot` (frogs also accept water and lily
/// pads).
fn acceptable_landing(t: &Tick<'_, '_>, jumper: Jumper, pos: BlockPos) -> bool {
    let me = t.block_pos();
    if me.x == pos.x && me.z == pos.z {
        return false;
    }
    if jumper == Jumper::Frog {
        return super::frog::acceptable_landing(t, pos);
    }
    default_landing(t, pos)
}

/// `defaultAcceptableLandingSpot`.
pub fn default_landing(t: &Tick<'_, '_>, pos: BlockPos) -> bool {
    t.level
        .block(BlockPos::new(pos.x, pos.y - 1, pos.z))
        .has(vanillite_data::block::flags::SOLID_RENDER)
        && crate::mob::path::malus_of(
            t.mob_ref().kind.malus(),
            crate::mob::path::path_type_static(t.level, t.level.min_y(), pos.x, pos.y, pos.z),
        ) == 0.0
}

fn pick_candidate(
    t: &mut Tick<'_, '_>,
    jumper: Jumper,
    wanting_preferred: bool,
    chosen: &mut Option<Vec3>,
    prepare_start: &mut i64,
    now: i64,
) {
    while !state(t.mob_ref()).jump_candidates.is_empty() {
        let Some(pos) = jump_candidate(t, jumper, wanting_preferred) else {
            continue;
        };
        if !acceptable_landing(t, jumper, pos) {
            continue;
        }

        let target = Vec3::new(
            f64::from(pos.x) + 0.5,
            f64::from(pos.y) + 0.5,
            f64::from(pos.z) + 0.5,
        );
        let Some(jump) = optimal_jump(t, target) else {
            continue;
        };

        brain::brain(t).set(M::LookTarget, MemValue::Look(Tracker::block(pos)));
        let path = nav::create_path_within(t, pos, 0, 8.0);
        if path.is_none_or(|p| !p.reached) {
            *chosen = Some(jump);
            *prepare_start = now;
            return;
        }
    }
}

/// `calculateOptimalJumpVector`: the angles 65–80 in random order. `Collections.shuffle(list)`
/// draws from a random of its own, not from the level's or the mob's.
fn optimal_jump(t: &mut Tick<'_, '_>, target: Vec3) -> Option<Vec3> {
    let mut angles = [65, 70, 75, 80];
    let mut random = vanillite_base::random::LegacyRandom::new(crate::mob::new_shuffle_seed());
    for i in (1..angles.len()).rev() {
        let j = usize::try_from(random.next_int_bounded(i32::try_from(i + 1).unwrap_or(1)))
            .unwrap_or(0);
        angles.swap(i, j);
    }
    #[allow(clippy::cast_possible_truncation)]
    let max_v = (t.mob_ref().attrs.jump_strength * f64::from(3.571_428_8_f32)) as f32;
    angles
        .into_iter()
        .find_map(|a| jump_vector_for_angle(t, target, max_v, a, true))
}

/// `LongJumpUtil.calculateJumpVectorForAngle`.
pub fn jump_vector_for_angle(
    t: &Tick<'_, '_>,
    target: Vec3,
    max_v: f32,
    angle: i32,
    check_collision: bool,
) -> Option<Vec3> {
    let mob = t.pos();
    let plane = Vec3::new(target.x - mob.x, 0.0, target.z - mob.z);
    let plen = plane.length();
    let plane = if plen < 1.0e-4 {
        Vec3::ZERO
    } else {
        plane * (0.5 / plen)
    };
    let tp = Vec3::new(target.x - plane.x, target.y - plane.y, target.z - plane.z);
    let dir = Vec3::new(tp.x - mob.x, tp.y - mob.y, tp.z - mob.z);
    #[allow(clippy::cast_precision_loss)]
    let angrad = f64::from(angle as f32 * std::f32::consts::PI / 180.0);
    let xz = dir.z.atan2(dir.x);
    let r2 = dir.x * dir.x + dir.z * dir.z;
    let r = r2.sqrt();
    let y = dir.y;
    let g = t.mob_ref().attrs.gravity;
    let sin2 = (2.0 * angrad).sin();
    let cossq = angrad.cos().powi(2);
    let (sina, cosa) = (angrad.sin(), angrad.cos());
    let (sinxz, cosxz) = (xz.sin(), xz.cos());
    let v0sq = r2 * g / (r * sin2 - 2.0 * y * cossq);
    if v0sq < 0.0 {
        return None;
    }
    let v0 = v0sq.sqrt();
    if v0 > f64::from(max_v) {
        return None;
    }
    let v0r = v0 * cosa;
    let v0y = v0 * sina;
    if check_collision {
        #[allow(clippy::cast_possible_truncation)]
        let samples = (r / v0r).ceil() as i32 * 2;
        let mut ri = 0.0;
        let mut prev: Option<Vec3> = None;
        let dims = crate::mob::kinds::mob_dimensions(t.mob_ref());
        let dims = if t.mob_ref().kind == Kind::Goat {
            crate::mob::Dimensions {
                width: dims.width * 0.7,
                height: dims.height * 0.7,
            }
        } else {
            dims
        };
        for _ in 0..samples - 1 {
            ri += r / f64::from(samples);
            let yi = sina / cosa * ri - ri * ri * g / (2.0 * v0sq * cosa * cosa);
            let sample = Vec3::new(mob.x + ri * cosxz, mob.y + yi, mob.z + ri * sinxz);
            if let Some(p) = prev
                && !clear_transition(t, dims, p, sample)
            {
                return None;
            }
            prev = Some(sample);
        }
    }
    Some(Vec3::new(v0r * cosxz, v0y, v0r * sinxz) * f64::from(0.95_f32))
}

/// `LongJumpUtil.isClearTransition`.
fn clear_transition(t: &Tick<'_, '_>, dims: crate::mob::Dimensions, a: Vec3, b: Vec3) -> bool {
    let d = Vec3::new(b.x - a.x, b.y - a.y, b.z - a.z);
    let min = f64::from(dims.width.min(dims.height));
    #[allow(clippy::cast_possible_truncation)]
    let checks = (d.length() / min).ceil() as i32;
    let n = d.normalize();
    let mut p = a;
    let mut scratch = Vec::new();
    for i in 0..checks {
        p = if i == checks - 1 {
            b
        } else {
            p + n * (min * f64::from(0.9_f32))
        };
        if !crate::mob::physics::no_collision(&t.level.view(), &dims.bounding_box(p), &mut scratch)
        {
            return false;
        }
    }
    true
}

/// `Goat.createHorn`: a goat horn with an instrument of `#regular_goat_horns` or
/// `#screaming_goat_horns`, picked by a random seeded from the goat's UUID hash.
fn create_horn(uuid: u128, screaming: bool) -> crate::item::ItemStack {
    let mut stack = crate::mob::equipment::stack("minecraft:goat_horn");
    let tag = if screaming {
        "#minecraft:screaming_goat_horns"
    } else {
        "#minecraft:regular_goat_horns"
    };
    let Some(ids) =
        crate::player::play::game_data().map(|d| d.holder_set("minecraft:instrument", tag))
    else {
        return stack;
    };
    if ids.is_empty() {
        return stack;
    }
    // `UUID.hashCode`.
    #[allow(clippy::cast_possible_truncation, clippy::as_conversions)]
    let hash = {
        let hilo = ((uuid >> 64) as u64) ^ (uuid as u64);
        ((hilo >> 32) as i32) ^ (hilo as i32)
    };
    let mut random = vanillite_base::random::LegacyRandom::new(i64::from(hash));
    let len = i32::try_from(ids.len()).unwrap_or(i32::MAX);
    let pick = usize::try_from(vanillite_base::random::RandomSource::next_int_bounded(
        &mut random,
        len,
    ))
    .unwrap_or(0);
    if let Some(&id) = ids.get(pick) {
        let mut out = Vec::new();
        vanillite_protocol::codec::write_varint(
            &mut out,
            i32::try_from(id).unwrap_or(-1).wrapping_add(1),
        );
        stack.patch_mut().set(
            vanillite_protocol::components::ids::INSTRUMENT,
            out.into_boxed_slice(),
        );
    }
    stack
}
