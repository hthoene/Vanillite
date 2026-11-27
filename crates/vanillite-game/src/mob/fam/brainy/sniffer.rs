//! Sniffers (`Sniffer`, `SnifferAi`): they sniff out, walk to and dig up ancient seeds, keep
//! the last 20 dug positions and lay eggs when bred.

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Vec3};
use vanillite_world::entity::EntityId;

use super::{make_sound, spec, state, state_mut};
use crate::mob::animal::Interaction;
use crate::mob::brain::behavior::{self as bh, Shape, Spec};
use crate::mob::brain::memory::MemoryStatus as S;
use crate::mob::brain::{
    self, Activity, ActivitySpec, Brain, BrainSpec, GlobalPos, MemValue, MemoryStatus,
    MemoryType as M, SensorKind, WalkTarget, brain_ref,
};
use crate::mob::fam::Interact;
use crate::mob::path::{MalusTable, PathType};
use crate::mob::{MobData, Tick, nav, randompos};
use crate::player::encode::DataWriter;

/// Water, powder snow and damaging blocks are off limits.
pub const MALUS: &MalusTable = &[
    (PathType::Water, -1.0),
    (PathType::OnTopOfPowderSnow, -1.0),
    (PathType::DamageCautious, -1.0),
];

/// `Sniffer.State` ids.
pub const IDLING: u8 = 0;
pub const FEELING_HAPPY: u8 = 1;
pub const SCENTING: u8 = 2;
pub const SNIFFING: u8 = 3;
pub const SEARCHING: u8 = 4;
pub const DIGGING: u8 = 5;
pub const RISING: u8 = 6;

/// The sniffer behaviours.
#[derive(Clone, Copy, Debug)]
pub enum Behavior {
    Digging,
    FeelingHappy,
    FinishedDigging,
    Scenting,
    Searching,
    Sniffing,
}

fn sn(b: Behavior, min: i32, max: i32) -> Spec {
    let mut s = spec(super::Behavior::Sniffer(b));
    s.set_duration(min, max);
    s
}

fn is_player(t: &Tick<'_, '_>, id: EntityId) -> bool {
    t.ents.player(id).is_some()
}

/// `SnifferAi.resetSniffing`.
pub fn reset_sniffing(t: &mut Tick<'_, '_>) {
    let b = brain::brain(t);
    b.erase(M::SnifferDigging);
    b.erase(M::SnifferSniffingTarget);
    transition_to(t, IDLING);
}

pub(super) fn brain_spec() -> BrainSpec {
    let mut panic = Spec::animal_panic(2.0);
    panic.set_on_start(reset_sniffing);
    let core = ActivitySpec::new(
        Activity::Core,
        0,
        vec![
            Spec::swim(0.8),
            panic,
            Spec::move_to_target_sink_for(500, 700),
            Spec::count_down(M::TemptationCooldownTicks),
        ],
    );
    let mut love = Spec::make_love(crate::mob::Kind::Sniffer, 1.0, 2);
    love.set_on_start(reset_sniffing);
    let mut tempt = Spec::follow_temptation(|_| 1.25, 3.5, false);
    tempt.set_on_start(reset_sniffing);
    tempt.set_close_fn(|t| if t.mob_ref().baby { 2.5 } else { 3.5 });
    let idle = ActivitySpec::with_priorities(
        Activity::Idle,
        vec![
            (0, love),
            (1, tempt),
            (2, Spec::look_at_target_sink(45, 90)),
            (3, sn(Behavior::FeelingHappy, 40, 100)),
            (
                4,
                Spec::run_one(vec![
                    (Spec::walk_to_look_target(1.0, 3), 2),
                    (sn(Behavior::Scenting, 40, 80), 1),
                    (sn(Behavior::Sniffing, 40, 80), 1),
                    (Spec::look_at_entity(Some(is_player), 6.0), 1),
                    (Spec::stroll(1.0), 1),
                    (Spec::do_nothing(5, 20), 2),
                ]),
            ),
        ],
    )
    .requires(&[(M::SnifferDigging, S::ValueAbsent)]);
    let sniff = ActivitySpec::with_priorities(
        Activity::Sniff,
        vec![(0, sn(Behavior::Searching, 600, 600))],
    )
    .requires(&[
        (M::IsPanicking, S::ValueAbsent),
        (M::SnifferSniffingTarget, S::ValuePresent),
        (M::WalkTarget, S::ValuePresent),
    ]);
    let dig = ActivitySpec::with_priorities(
        Activity::Dig,
        vec![
            (0, sn(Behavior::Digging, 160, 180)),
            (0, sn(Behavior::FinishedDigging, 40, 40)),
        ],
    )
    .requires(&[
        (M::IsPanicking, S::ValueAbsent),
        (M::WalkTarget, S::ValueAbsent),
        (M::SnifferDigging, S::ValuePresent),
    ]);
    BrainSpec {
        memories: vec![M::SnifferExploredPositions],
        sensors: vec![
            SensorKind::NearestLivingEntities,
            SensorKind::HurtBy,
            SensorKind::NearestPlayers,
            SensorKind::Tempting(None),
        ],
        activities: vec![core, idle, sniff, dig],
        ..BrainSpec::default()
    }
}

pub fn init(m: &mut MobData) {
    m.brain = Some(Box::new(Brain::new(brain_spec(), &mut m.random)));
    m.nav.can_float = true;
}

fn sniffer_state(m: &MobData) -> u8 {
    state(m).sniffer_state
}

/// `transitionTo`.
fn transition_to(t: &mut Tick<'_, '_>, s: u8) {
    match s {
        FEELING_HAPPY => {
            crate::mob::kinds::play_sound(t, "minecraft:entity.sniffer.happy", 1.0, 1.0)
        }
        SCENTING => {
            let pitch = if t.mob_ref().baby { 1.3 } else { 1.0 };
            crate::mob::kinds::play_sound(t, "minecraft:entity.sniffer.scenting", 1.0, pitch);
        }
        SNIFFING => crate::mob::kinds::play_sound(t, "minecraft:entity.sniffer.sniffing", 1.0, 1.0),
        DIGGING => {
            let at = t.mob_ref().tick_count + 120;
            state_mut(t.mob()).drop_seed_at_tick = at;
            super::entity_event(t, 63);
        }
        RISING => {
            crate::mob::kinds::play_sound(t, "minecraft:entity.sniffer.digging_stop", 1.0, 1.0)
        }
        _ => {}
    }
    if sniffer_state(t.mob_ref()) != s {
        state_mut(t.mob()).sniffer_state = s;
        let dims = dimensions_for(t.mob_ref());
        t.core_mut().set_dims(dims);
        t.core_mut().mark_data_dirty();
    }
}

/// The digging pose is 0.4 lower.
fn dimensions_for(m: &MobData) -> crate::mob::Dimensions {
    let d = crate::mob::kinds::dimensions(m.kind.type_id(), m.baby);
    if sniffer_state(m) == DIGGING {
        let scale = if m.baby { 0.5 } else { 1.0 };
        crate::mob::Dimensions {
            width: d.width,
            height: (3.5 - 0.4) * scale,
        }
    } else {
        d
    }
}

fn is_tempted(t: &Tick<'_, '_>) -> bool {
    brain_ref(t).has(M::TemptingPlayer)
}

/// `canSniff`.
fn can_sniff(t: &Tick<'_, '_>) -> bool {
    !is_tempted(t)
        && !brain_ref(t).has(M::IsPanicking)
        && !t.mob_ref().fluids.in_water()
        && t.mob_ref().in_love <= 0
        && t.on_ground()
        && t.ents.riding.vehicle_of(t.id).is_none()
        && crate::mob::tame::leash_holder(t.ents, t.id).is_none()
}

/// `getHeadBlock`: 2.25 ahead at the feet + 0.2.
fn head_block(t: &Tick<'_, '_>) -> BlockPos {
    let y_rot = t.core().rot().y_rot;
    let rad = f64::from(y_rot) * std::f64::consts::PI / 180.0;
    let (sin, cos) = (rad.sin(), rad.cos());
    let p = t.pos();
    BlockPos::containing(p.x - sin * 2.25, p.y + f64::from(0.2_f32), p.z + cos * 2.25)
}

fn explored(t: &Tick<'_, '_>) -> Vec<GlobalPos> {
    match brain_ref(t).get(M::SnifferExploredPositions) {
        Some(MemValue::GlobalPositions(v)) => v.clone(),
        _ => Vec::new(),
    }
}

/// `canDig(position)`.
fn can_dig_at(t: &mut Tick<'_, '_>, pos: BlockPos) -> bool {
    if !crate::block::block_tags()
        .sniffer_diggable_block
        .contains(t.level.block(pos))
    {
        return false;
    }
    let dim = t.env.dimension;
    if explored(t)
        .iter()
        .any(|g| *g.dimension == *dim && g.pos == pos)
    {
        return false;
    }
    nav::create_path_to(t, pos, 1).is_some_and(|p| p.reached)
}

/// `canDig()`.
fn can_dig(t: &mut Tick<'_, '_>) -> bool {
    if brain_ref(t).has(M::IsPanicking)
        || is_tempted(t)
        || t.mob_ref().baby
        || t.mob_ref().fluids.in_water()
        || !t.on_ground()
        || t.ents.riding.vehicle_of(t.id).is_some()
    {
        return false;
    }
    let head = head_block(t);
    can_dig_at(t, BlockPos::new(head.x, head.y - 1, head.z))
}

/// `calculateDigPosition`.
fn dig_position(t: &mut Tick<'_, '_>) -> Option<BlockPos> {
    for idx in 0..5 {
        let Some(p) = randompos::land_pos(t, 10 + 2 * idx, 3) else {
            continue;
        };
        let b = BlockPos::containing(p.x, p.y, p.z);
        let below = BlockPos::new(b.x, b.y - 1, b.z);
        if can_dig_at(t, below) {
            return Some(below);
        }
    }
    None
}

/// `onDiggingComplete`: remembers the dug position (up to 20 + the new one).
fn on_digging_complete(t: &mut Tick<'_, '_>, success: bool) {
    if !success {
        return;
    }
    let p = t.pos();
    let on = BlockPos::containing(p.x, p.y - 0.2, p.z);
    let mut list: Vec<GlobalPos> = explored(t).into_iter().take(20).collect();
    list.insert(
        0,
        GlobalPos {
            dimension: t.env.dimension.into(),
            pos: on,
        },
    );
    brain::brain(t).set(M::SnifferExploredPositions, MemValue::GlobalPositions(list));
}

/// `tick`: digging drops the seed when its tick comes.
pub fn before_tick(t: &mut Tick<'_, '_>) {
    if sniffer_state(t.mob_ref()) == DIGGING
        && state(t.mob_ref()).drop_seed_at_tick == t.mob_ref().tick_count
    {
        let head = head_block(t);
        let items = super::gift_loot(t, "minecraft:gameplay/sniffer_digging");
        for stack in items {
            let (uuid, y_rot, velocity, item_random) = crate::mob::new_item_entity();
            t.env.out.push(crate::survival::Effect::DropItem {
                random: item_random,
                uuid,
                pos: Vec3::new(f64::from(head.x), f64::from(head.y), f64::from(head.z)),
                velocity,
                y_rot,
                pickup_delay: 10,
                stack,
            });
        }
        crate::mob::kinds::play_sound(t, "minecraft:entity.sniffer.drop_seed", 1.0, 1.0);
    }
}

/// `SnifferAi.updateActivity`.
pub fn update_activity(t: &mut Tick<'_, '_>) {
    brain::brain(t).set_active_activity_to_first_valid(&[
        Activity::Dig,
        Activity::Sniff,
        Activity::Idle,
    ]);
}

pub fn ambient_sound(m: &MobData) -> Option<&'static str> {
    (!matches!(sniffer_state(m), DIGGING | SEARCHING)).then_some("minecraft:entity.sniffer.idle")
}

/// `Sniffer.spawnChildFromBreeding`: a sniffer egg instead of a baby.
pub fn spawn_child_from_breeding(t: &mut Tick<'_, '_>, partner: EntityId) {
    super::finalize_child_from_breeding(t, partner);
    let r = &mut t.mob().random;
    let pitch = (r.next_float() - r.next_float()) * 0.2 + 0.5;
    crate::mob::kinds::play_sound(t, "minecraft:entity.sniffer.egg_plop", 1.0, pitch);
    let (uuid, y_rot, velocity, item_random) = crate::mob::new_item_entity();
    let pos = t.pos();
    t.env.out.push(crate::survival::Effect::DropItem {
        random: item_random,
        uuid,
        pos,
        velocity,
        y_rot,
        pickup_delay: 10,
        stack: crate::mob::equipment::stack("minecraft:sniffer_egg"),
    });
}

/// `canMate`: only while idling, scenting or happy.
pub fn can_mate(m: &MobData) -> bool {
    matches!(sniffer_state(m), IDLING | SCENTING | FEELING_HAPPY)
}

/// `mobInteract`: feeding with the eating sound.
pub fn interact(ix: &mut Interact<'_, '_>) -> Interaction {
    let food = crate::mob::animal::is_food(crate::mob::Kind::Sniffer, ix.stack.item);
    let r = crate::mob::animal::animal_interact_pub(ix);
    if r.consumes()
        && food
        && let Some(pos) = ix.ents.store.get(ix.target).map(|e| e.pos())
    {
        let pitch = 0.8 + ix.env.random.next_float() * 0.4;
        let seed = crate::survival::sound_seed();
        crate::survival::play_sound(
            ix.env.out,
            None,
            pos,
            "minecraft:entity.sniffer.eat",
            vanillite_protocol::packets::play::clientbound::SoundSource::Neutral,
            1.0,
            pitch,
            seed,
        );
    }
    r
}

/// Synched data: baby (16), state (17), seed drop tick (18).
pub fn encode(w: &mut DataWriter<'_>, m: &MobData) {
    w.boolean(16, m.baby, false);
    w.varint(
        17,
        crate::player::encode::serializer::SNIFFER_STATE,
        i32::from(sniffer_state(m)),
        0,
    );
    w.int(18, state(m).drop_seed_at_tick, 0);
}

// --- Behaviours -----------------------------------------------------------------------------

pub fn duration(_b: &Behavior) -> (i32, i32) {
    (60, 60)
}

pub fn shape(_b: &Behavior) -> Shape {
    Shape::Timed
}

pub fn requirements(b: &Behavior) -> &'static [(M, MemoryStatus)] {
    match b {
        Behavior::Digging => &[
            (M::IsPanicking, S::ValueAbsent),
            (M::WalkTarget, S::ValueAbsent),
            (M::SnifferDigging, S::ValuePresent),
            (M::SniffCooldown, S::ValueAbsent),
        ],
        Behavior::FeelingHappy => &[(M::SnifferHappy, S::ValuePresent)],
        Behavior::FinishedDigging => &[
            (M::IsPanicking, S::ValueAbsent),
            (M::WalkTarget, S::ValueAbsent),
            (M::SnifferDigging, S::ValuePresent),
            (M::SniffCooldown, S::ValuePresent),
        ],
        Behavior::Scenting => &[
            (M::IsPanicking, S::ValueAbsent),
            (M::SnifferDigging, S::ValueAbsent),
            (M::SnifferSniffingTarget, S::ValueAbsent),
            (M::SnifferHappy, S::ValueAbsent),
            (M::BreedTarget, S::ValueAbsent),
        ],
        Behavior::Searching => &[
            (M::WalkTarget, S::ValuePresent),
            (M::IsPanicking, S::ValueAbsent),
            (M::SnifferSniffingTarget, S::ValuePresent),
        ],
        Behavior::Sniffing => &[
            (M::WalkTarget, S::ValueAbsent),
            (M::SnifferSniffingTarget, S::ValueAbsent),
            (M::SniffCooldown, S::ValueAbsent),
        ],
    }
}

pub fn no_timeout(_b: &Behavior) -> bool {
    false
}

pub fn check(b: &mut Behavior, t: &mut Tick<'_, '_>) -> bool {
    match b {
        Behavior::Digging | Behavior::Searching => can_sniff(t),
        Behavior::Scenting => !is_tempted(t),
        Behavior::Sniffing => !t.mob_ref().baby && can_sniff(t),
        Behavior::FeelingHappy | Behavior::FinishedDigging => true,
    }
}

pub fn can_still_use(b: &mut Behavior, t: &mut Tick<'_, '_>, _now: i64) -> bool {
    match b {
        Behavior::Digging => {
            brain_ref(t).has(M::SnifferDigging) && can_dig(t) && t.mob_ref().in_love <= 0
        }
        Behavior::FeelingHappy | Behavior::Scenting => true,
        Behavior::FinishedDigging => brain_ref(t).has(M::SnifferDigging),
        Behavior::Searching => {
            if !can_sniff(t) {
                transition_to(t, IDLING);
                return false;
            }
            let walk = brain_ref(t)
                .walk_target()
                .and_then(|w| bh::tracker_block(t, &w.target));
            let target = brain_ref(t).pos(M::SnifferSniffingTarget);
            matches!((walk, target), (Some(a), Some(b)) if a == b)
        }
        Behavior::Sniffing => can_sniff(t),
    }
}

pub fn start(b: &mut Behavior, t: &mut Tick<'_, '_>, _now: i64) {
    let s = match b {
        Behavior::Digging => DIGGING,
        Behavior::FeelingHappy => FEELING_HAPPY,
        Behavior::FinishedDigging => RISING,
        Behavior::Scenting => SCENTING,
        Behavior::Searching => SEARCHING,
        Behavior::Sniffing => SNIFFING,
    };
    transition_to(t, s);
}

pub fn tick(_b: &mut Behavior, _t: &mut Tick<'_, '_>, _now: i64) {}

pub fn stop(b: &mut Behavior, t: &mut Tick<'_, '_>, _now: i64) {
    let finished = brain_ref(t).stop_timed_out;
    match b {
        Behavior::Digging => {
            if finished {
                brain::brain(t).set_with_expiry(M::SniffCooldown, MemValue::Unit, 9600);
            } else {
                reset_sniffing(t);
            }
        }
        Behavior::FeelingHappy => {
            transition_to(t, IDLING);
            brain::brain(t).erase(M::SnifferHappy);
        }
        Behavior::FinishedDigging => {
            transition_to(t, IDLING);
            on_digging_complete(t, finished);
            let b = brain::brain(t);
            b.erase(M::SnifferDigging);
            b.set(M::SnifferHappy, MemValue::Bool(true));
        }
        Behavior::Scenting => transition_to(t, IDLING),
        Behavior::Searching => {
            if can_dig(t) && can_sniff(t) {
                brain::brain(t).set(M::SnifferDigging, MemValue::Bool(true));
            }
            let b = brain::brain(t);
            b.erase(M::WalkTarget);
            b.erase(M::SnifferSniffingTarget);
        }
        Behavior::Sniffing => {
            transition_to(t, IDLING);
            if finished && let Some(p) = dig_position(t) {
                let b = brain::brain(t);
                b.set(M::SnifferSniffingTarget, MemValue::Pos(p));
                b.set(M::WalkTarget, MemValue::Walk(WalkTarget::block(p, 1.25, 0)));
            }
        }
    }
}

pub fn trigger(_b: &mut Behavior, _t: &mut Tick<'_, '_>, _now: i64) -> bool {
    false
}

#[allow(dead_code)]
fn _sound(t: &mut Tick<'_, '_>) {
    make_sound(t, "minecraft:entity.sniffer.idle");
}
