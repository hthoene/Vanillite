//! Wandering traders (`WanderingTrader`): goals (drinking invisibility at night and milk at
//! day, trading, fleeing illagers and zombies, wandering to the spawner's target), the despawn
//! timer, sounds and NBT. The spawner (`WanderingTraderSpawner`) is [`spawner`].

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Vec3};
use vanillite_nbt::{Compound, Tag};
use vanillite_protocol::packets::play::clientbound::SoundSource;
use vanillite_world::entity::EntityId;

use super::super::{FamilyHooks, Interact, SpawnContext};
use super::{merchant, state, state_mut};
use crate::item::{EquipmentSlot, ItemStack};
use crate::mob::animal::Interaction;
use crate::mob::common::{self, CommonGoal};
use crate::mob::goals::{self, Goal as MobGoal, flag, reduced};
use crate::mob::kinds::Kind;
use crate::mob::path::{MalusTable, PathType};
use crate::mob::{MobData, Tick, nav};
use crate::player::encode::DataWriter;

pub mod spawner;

/// The family's hooks for wandering traders.
#[derive(Debug)]
pub struct Hooks;

/// The wandering trader state.
#[derive(Clone, Debug, Default)]
pub struct Trader {
    /// `despawnDelay`.
    pub despawn_delay: i32,
    /// `wanderTarget`.
    pub wander_target: Option<BlockPos>,
    /// `useItemRemaining` of the drink in hand (0: not drinking).
    pub use_remaining: i32,
}

/// What a `UseItemGoal` of the trader drinks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drink {
    /// A potion of invisibility when it gets dark.
    Invisibility,
    /// Milk when it gets bright.
    Milk,
}

/// The wandering trader goals.
#[derive(Clone, Copy, Debug)]
pub enum Goal {
    /// `UseItemGoal(mob, item, finishSound, predicate)`.
    UseItem(Drink),
    /// `TradeWithPlayerGoal`.
    TradeWithPlayer,
    /// `LookAtTradingPlayerGoal`.
    LookAtTradingPlayer {
        look_at: Option<EntityId>,
        time: i32,
    },
    /// `WanderToPositionGoal(trader, 2.0, 0.35)`.
    WanderToPosition,
    /// `InteractGoal(mob, Player.class, 3, 1)` and `LookAtPlayerGoal(mob, Mob.class, 8)`.
    LookAt {
        players: bool,
        distance: f32,
        probability: f32,
        move_flag: bool,
        look_at: Option<EntityId>,
        time: i32,
    },
}

/// `AbstractVillager`'s path malus: fire next to the path costs, fire is off limits.
const MALUS: &MalusTable = &[(PathType::FireInNeighbor, 16.0), (PathType::Fire, -1.0)];

fn fam(g: Goal) -> MobGoal {
    MobGoal::Fam(super::super::FamGoal::Village(super::Goal::Trader(g)))
}

fn zombie(t: &Tick<'_, '_>, id: EntityId) -> bool {
    t.ents.mob(id).is_some_and(|m| {
        matches!(
            m.kind,
            Kind::Zombie
                | Kind::Husk
                | Kind::Drowned
                | Kind::ZombieVillager
                | Kind::ZombifiedPiglin
        )
    })
}
fn evoker(t: &Tick<'_, '_>, id: EntityId) -> bool {
    t.ents.mob(id).is_some_and(|m| m.kind == Kind::Evoker)
}
fn vindicator(t: &Tick<'_, '_>, id: EntityId) -> bool {
    t.ents.mob(id).is_some_and(|m| m.kind == Kind::Vindicator)
}
fn vex(t: &Tick<'_, '_>, id: EntityId) -> bool {
    t.ents.mob(id).is_some_and(|m| m.kind == Kind::Vex)
}
fn pillager(t: &Tick<'_, '_>, id: EntityId) -> bool {
    t.ents.mob(id).is_some_and(|m| m.kind == Kind::Pillager)
}
fn illusioner(t: &Tick<'_, '_>, id: EntityId) -> bool {
    t.ents.mob(id).is_some_and(|m| m.kind == Kind::Illusioner)
}
fn zoglin(t: &Tick<'_, '_>, id: EntityId) -> bool {
    t.ents.mob(id).is_some_and(|m| m.kind == Kind::Zoglin)
}

/// `WanderingTrader.registerGoals`.
fn register_goals(m: &mut MobData) {
    let g = &mut m.goals;
    g.add(0, MobGoal::Float);
    g.add(0, fam(Goal::UseItem(Drink::Invisibility)));
    g.add(0, fam(Goal::UseItem(Drink::Milk)));
    g.add(1, fam(Goal::TradeWithPlayer));
    let avoid =
        |f: common::TargetFilter, d: f32| MobGoal::Common(CommonGoal::avoid(f, d, 0.5, 0.5));
    g.add(1, avoid(zombie, 8.0));
    g.add(1, avoid(evoker, 12.0));
    g.add(1, avoid(vindicator, 8.0));
    g.add(1, avoid(vex, 8.0));
    g.add(1, avoid(pillager, 15.0));
    g.add(1, avoid(illusioner, 12.0));
    g.add(1, avoid(zoglin, 10.0));
    g.add(1, MobGoal::panic(0.5));
    g.add(
        1,
        fam(Goal::LookAtTradingPlayer {
            look_at: None,
            time: 0,
        }),
    );
    g.add(2, fam(Goal::WanderToPosition));
    g.add(
        4,
        MobGoal::Common(CommonGoal::MoveTowardsRestriction {
            speed: 0.35,
            wanted: Vec3::ZERO,
        }),
    );
    g.add(8, MobGoal::stroll(0.35));
    g.add(
        9,
        fam(Goal::LookAt {
            players: true,
            distance: 3.0,
            probability: 1.0,
            move_flag: true,
            look_at: None,
            time: 0,
        }),
    );
    g.add(
        10,
        fam(Goal::LookAt {
            players: false,
            distance: 8.0,
            probability: 0.02,
            move_flag: false,
            look_at: None,
            time: 0,
        }),
    );
}

fn trading(m: &MobData) -> bool {
    state(m).merchant.trading_player.is_some()
}

fn invisible(m: &MobData) -> bool {
    m.effects.has(crate::effect::MobEffect::INVISIBILITY)
}

/// The drink's stack.
fn drink_stack(drink: Drink) -> ItemStack {
    match drink {
        Drink::Milk => ItemStack::new(vanillite_data::item::ids::MILK_BUCKET.0, 1),
        Drink::Invisibility => {
            let mut stack = ItemStack::new(vanillite_data::item::ids::POTION.0, 1);
            let id = vanillite_data::registry::POTION
                .id("minecraft:invisibility")
                .map_or(0, i32::from);
            let mut out = vec![1];
            vanillite_protocol::codec::write_varint(&mut out, id);
            out.extend_from_slice(&[0, 0, 0]);
            stack.patch_mut().set(
                vanillite_protocol::components::ids::POTION_CONTENTS,
                out.into_boxed_slice(),
            );
            stack
        }
    }
}

fn random_float(t: &mut Tick<'_, '_>) -> f32 {
    t.mob().random.next_float()
}

/// `LookAtPlayerGoal.canUse` for players or any mob.
fn find_look_target(t: &mut Tick<'_, '_>, players: bool, distance: f32) -> Option<EntityId> {
    // The target Vanilla looks at first is always replaced by the search.
    let d = f64::from(distance);
    if players {
        goals::nearest_player(t, false, d, true)
    } else {
        // `getNearestEntity(getEntitiesOfClass(Mob, box.inflate(d, 3, d)), conditions, ...)`.
        let area = t.aabb().inflate(d, 3.0, d);
        let mut candidates = std::mem::take(&mut t.cx.ids);
        candidates.clear();
        t.ents
            .store
            .for_each_in(&area, true, |id| candidates.push(id));
        let p = t.pos();
        let at = Vec3::new(p.x, t.eye_y(), p.z);
        let mut best: Option<(EntityId, f64)> = None;
        for &id in &candidates {
            if id == t.id || !t.ents.mob(id).is_some_and(MobData::is_alive) {
                continue;
            }
            let Some(pos) = t.ents.store.get(id).map(|e| e.pos()) else {
                continue;
            };
            if p.distance_to_sqr(pos) > d.max(2.0) * d.max(2.0) {
                continue;
            }
            if !crate::mob::living::has_line_of_sight(t, id) {
                continue;
            }
            let dist = pos.distance_to_sqr(at);
            if best.is_none_or(|(_, b)| dist < b) {
                best = Some((id, dist));
            }
        }
        t.cx.ids = candidates;
        best.map(|(id, _)| id)
    }
}

fn look_continue(
    t: &mut Tick<'_, '_>,
    look_at: Option<EntityId>,
    distance: f32,
    time: i32,
) -> bool {
    let Some(o) = look_at.and_then(|id| t.other(id)) else {
        return false;
    };
    let d = f64::from(distance);
    t.pos().distance_to_sqr(o.pos) <= d * d && time > 0
}

/// `UseItemGoal` and `completeUsingItem` of the drink (`LivingEntity.updatingUsingItem` with
/// the consume sounds every 4 ticks).
fn update_drinking(t: &mut Tick<'_, '_>) {
    let remaining = state(t.mob_ref()).trader.use_remaining;
    if remaining <= 0 {
        return;
    }
    let held = t.mob_ref().equipment.get(EquipmentSlot::MainHand).clone();
    if held.is_empty() {
        state_mut(t.mob()).trader.use_remaining = 0;
        crate::mob::projectile::stop_using_item(t);
        return;
    }
    // `Consumable.shouldEmitParticlesAndSounds`: 32 ticks, the first 7 quiet.
    if 32 - remaining > 7 && remaining % 4 == 0 {
        drink_sound(t, &held);
    }
    let remaining = remaining - 1;
    state_mut(t.mob()).trader.use_remaining = remaining;
    if remaining == 0 {
        drink_sound(t, &held);
        let milk = held.item == vanillite_data::item::ids::MILK_BUCKET.0;
        if milk {
            t.mob().effects.clear();
        } else {
            crate::effect::living::add_to_mob(
                t.mob(),
                crate::effect::EffectInstance::new(crate::effect::MobEffect::INVISIBILITY, 3600, 0),
            );
        }
        let rest = if milk {
            ItemStack::new(vanillite_data::item::ids::BUCKET.0, 1)
        } else {
            ItemStack::new(vanillite_data::item::ids::GLASS_BOTTLE.0, 1)
        };
        t.mob().equipment.set(EquipmentSlot::MainHand, rest);
        crate::mob::equipment::broadcast(t);
        crate::mob::projectile::stop_using_item(t);
    }
}

/// `Consumable.emitParticlesAndSounds` of a drink: half volume, pitch 0.9 to 1.
fn drink_sound(t: &mut Tick<'_, '_>, held: &ItemStack) {
    let m = t.mob();
    let _eat_volume = m.random.next_boolean();
    let _eat_pitch = m.random.next_float() - m.random.next_float();
    let pitch = m.random.next_float() * 0.1 + 0.9;
    let sound = if held.item == vanillite_data::item::ids::MILK_BUCKET.0 {
        "minecraft:entity.wandering_trader.drink_milk"
    } else {
        "minecraft:entity.wandering_trader.drink_potion"
    };
    crate::mob::fam::brainy::sound_at(t, sound, SoundSource::Neutral, 0.5, pitch);
}

fn uuid_tag(tag: &Compound, key: &str) -> Option<BlockPos> {
    match tag.get(key) {
        Some(Tag::IntArray(v)) if v.len() == 3 => Some(BlockPos::new(v[0], v[1], v[2])),
        _ => None,
    }
}

impl FamilyHooks for Hooks {
    type Goal = Goal;

    fn is_monster(_kind: Kind) -> bool {
        false
    }
    fn ageable(_kind: Kind) -> bool {
        true
    }
    fn remove_when_far_away(_kind: Kind) -> bool {
        false
    }
    fn malus(_kind: Kind) -> &'static MalusTable {
        MALUS
    }
    fn sound_source(_kind: Kind) -> SoundSource {
        SoundSource::Neutral
    }
    fn ambient_sound(m: &mut MobData) -> Option<&'static str> {
        Some(merchant::ambient_sound(m))
    }
    fn hurt_sound(_m: &MobData) -> &'static str {
        "minecraft:entity.wandering_trader.hurt"
    }
    fn death_sound(_m: &MobData) -> &'static str {
        "minecraft:entity.wandering_trader.death"
    }
    fn experience_reward(_m: &mut MobData) -> i32 {
        0
    }
    fn register_goals(m: &mut MobData) {
        register_goals(m);
    }
    /// `AbstractVillager.finalizeSpawn`: no babies (`AgeableMobGroupData(false)`).
    fn finalize_spawn(_m: &mut MobData, _ctx: &mut SpawnContext<'_>) {}
    fn after_base_tick(t: &mut Tick<'_, '_>) {
        update_drinking(t);
    }
    /// `aiStep`: `maybeDespawn`.
    fn ai_step(t: &mut Tick<'_, '_>) {
        let m = t.mob();
        let busy = trading(m);
        let tr = &mut state_mut(m).trader;
        if tr.despawn_delay > 0 && !busy {
            tr.despawn_delay -= 1;
            if tr.despawn_delay == 0 {
                t.mob().removed = true;
            }
        }
    }
    fn interact(ix: &mut Interact<'_, '_>) -> Interaction {
        merchant::interact(ix)
    }
    fn can_mate(_me: &MobData, _partner: &MobData) -> bool {
        false
    }
    fn encode_data(w: &mut DataWriter<'_>, m: &MobData) {
        w.boolean(16, m.baby, false);
        w.boolean(17, false, false);
        w.int(18, state(m).merchant.unhappy_counter, 0);
    }
    fn save(m: &MobData, out: &mut Compound) {
        out.insert("Age", m.age);
        out.insert("ForcedAge", m.forced_age);
        out.insert("AgeLocked", false);
        merchant::save(&state(m).merchant, out);
        let tr = &state(m).trader;
        out.insert("DespawnDelay", tr.despawn_delay);
        if let Some(p) = tr.wander_target {
            out.insert("wander_target", Tag::IntArray(vec![p.x, p.y, p.z]));
        }
    }
    fn load(m: &mut MobData, tag: &Compound) {
        let int = |key: &str| {
            tag.get(key)
                .and_then(Tag::as_i64)
                .and_then(|v| i32::try_from(v).ok())
                .unwrap_or(0)
        };
        // `setAge(max(0, age))`.
        m.age = int("Age").max(0);
        m.forced_age = int("ForcedAge");
        m.baby = false;
        let s = state_mut(m);
        merchant::load(&mut s.merchant, tag);
        s.trader.despawn_delay = int("DespawnDelay");
        s.trader.wander_target = uuid_tag(tag, "wander_target");
    }
    fn pick_up_items(_t: &mut Tick<'_, '_>) {}

    fn goal_flags(g: &Goal) -> u8 {
        match g {
            Goal::UseItem(_) => 0,
            Goal::TradeWithPlayer => flag::JUMP | flag::MOVE,
            Goal::LookAtTradingPlayer { .. } => flag::LOOK,
            Goal::WanderToPosition => flag::MOVE,
            Goal::LookAt { move_flag, .. } => {
                if *move_flag {
                    flag::LOOK | flag::MOVE
                } else {
                    flag::LOOK
                }
            }
        }
    }

    fn goal_can_use(t: &mut Tick<'_, '_>, g: &mut Goal) -> bool {
        match g {
            Goal::UseItem(drink) => {
                let m = t.mob_ref();
                match drink {
                    Drink::Invisibility => !t.cx.env.is_bright_outside() && !invisible(m),
                    Drink::Milk => t.cx.env.is_bright_outside() && invisible(m),
                }
            }
            Goal::TradeWithPlayer => {
                let m = t.mob_ref();
                if !m.is_alive() || m.fluids.in_water() || !t.on_ground() {
                    return false;
                }
                let Some(player) = state(m).merchant.trading_player else {
                    return false;
                };
                t.other(player)
                    .is_some_and(|o| t.pos().distance_to_sqr(o.pos) <= 16.0)
            }
            Goal::LookAtTradingPlayer { look_at, .. } => {
                let player = state(t.mob_ref()).merchant.trading_player;
                if player.is_some() {
                    *look_at = player;
                    true
                } else {
                    false
                }
            }
            Goal::WanderToPosition => state(t.mob_ref())
                .trader
                .wander_target
                .is_some_and(|p| !crate::mob::fam::wildlife::closer_to_center(p, t.pos(), 2.0)),
            Goal::LookAt {
                players,
                distance,
                probability,
                look_at,
                ..
            } => {
                if random_float(t) >= *probability {
                    return false;
                }
                *look_at = find_look_target(t, *players, *distance);
                look_at.is_some()
            }
        }
    }

    fn goal_can_continue(t: &mut Tick<'_, '_>, g: &mut Goal) -> bool {
        match g {
            Goal::UseItem(_) => t.mob_ref().using_item >= 0,
            Goal::LookAtTradingPlayer { look_at, time } => look_continue(t, *look_at, 8.0, *time),
            Goal::LookAt {
                look_at,
                distance,
                time,
                ..
            } => look_continue(t, *look_at, *distance, *time),
            _ => Self::goal_can_use(t, g),
        }
    }

    fn goal_start(t: &mut Tick<'_, '_>, g: &mut Goal) {
        match g {
            Goal::UseItem(drink) => {
                t.mob()
                    .equipment
                    .set(EquipmentSlot::MainHand, drink_stack(*drink));
                crate::mob::equipment::broadcast(t);
                state_mut(t.mob()).trader.use_remaining = 32;
                crate::mob::projectile::start_using_item(t);
            }
            Goal::TradeWithPlayer => t.mob().nav.stop(),
            Goal::LookAtTradingPlayer { time, .. } | Goal::LookAt { time, .. } => {
                let r = t.mob().random.next_int_bounded(40);
                *time = reduced(40 + r);
            }
            Goal::WanderToPosition => {}
        }
    }

    fn goal_stop(t: &mut Tick<'_, '_>, g: &mut Goal) {
        match g {
            Goal::UseItem(drink) => {
                t.mob()
                    .equipment
                    .set(EquipmentSlot::MainHand, ItemStack::EMPTY);
                crate::mob::equipment::broadcast(t);
                state_mut(t.mob()).trader.use_remaining = 0;
                let sound = match drink {
                    Drink::Invisibility => "minecraft:entity.wandering_trader.disappeared",
                    Drink::Milk => "minecraft:entity.wandering_trader.reappeared",
                };
                let pitch = t.mob().random.next_float() * 0.2 + 0.9;
                crate::mob::fam::brainy::sound_at(t, sound, SoundSource::Neutral, 1.0, pitch);
            }
            Goal::TradeWithPlayer => merchant::stop_trading(t),
            Goal::LookAtTradingPlayer { look_at, .. } | Goal::LookAt { look_at, .. } => {
                *look_at = None;
            }
            Goal::WanderToPosition => {
                state_mut(t.mob()).trader.wander_target = None;
                t.mob().nav.stop();
            }
        }
    }

    fn goal_tick(t: &mut Tick<'_, '_>, g: &mut Goal) {
        match g {
            Goal::LookAtTradingPlayer { look_at, time } | Goal::LookAt { look_at, time, .. } => {
                if let Some(o) = look_at.and_then(|id| t.other(id)) {
                    nav::look_at(t, Vec3::new(o.pos.x, o.eye_y, o.pos.z));
                    *time -= 1;
                }
            }
            Goal::WanderToPosition => {
                let Some(target) = state(t.mob_ref()).trader.wander_target else {
                    return;
                };
                if !t.mob_ref().nav.is_done() {
                    return;
                }
                let p = t.pos();
                if !crate::mob::fam::wildlife::closer_to_center(target, p, 10.0) {
                    let dir = Vec3::new(
                        f64::from(target.x) - p.x,
                        f64::from(target.y) - p.y,
                        f64::from(target.z) - p.z,
                    )
                    .normalize();
                    let to = dir * 10.0 + p;
                    nav::move_to_pos(t, to, 0.35);
                } else {
                    let to = Vec3::new(
                        f64::from(target.x),
                        f64::from(target.y),
                        f64::from(target.z),
                    );
                    nav::move_to_pos(t, to, 0.35);
                }
            }
            _ => {}
        }
    }
}
