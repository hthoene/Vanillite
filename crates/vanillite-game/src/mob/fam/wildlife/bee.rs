//! Bees (`Bee`): neutral flyers that pollinate flowers, grow crops on the way home, sting
//! once and die after stinging.
//!
//! Beehives and bee nests have no block entity yet: bees cannot enter, leave or find a hive
//! (`BeeEnterHiveGoal`, `BeeLocateHiveGoal`, `BeeGoToHiveGoal` never start, `isHiveValid` is
//! false), so they do not grow crops either (`BeeGrowCropGoal` needs a valid hive). They wander,
//! pollinate, breed, get angry and sting like Vanilla.

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Vec3};
use vanillite_nbt::Compound;

use super::{Goal, block_tags, st, st_mut};
use crate::effect::MobEffect;
use crate::mob::animal::{Interaction, animal_interact, use_item};
use crate::mob::common::{self, CommonGoal};
use crate::mob::fam::{FamGoal, Interact};
use crate::mob::goals::{Goal as MobGoal, MeleeKind, TemptItems, reduced};
use crate::mob::kinds::{self, Kind};
use crate::mob::{MobData, Tick, WorldAction, combat, living, nav, randompos};
use crate::survival::damage::DamageSource;
use crate::survival::rules::Difficulty;
use vanillite_world::entity::EntityId;

const FLAG_ROLL: u8 = 2;
const FLAG_HAS_STUNG: u8 = 4;
const FLAG_HAS_NECTAR: u8 = 8;

fn fam(g: Goal) -> MobGoal {
    MobGoal::Fam(FamGoal::Wildlife(g))
}

/// `BeeBecomeAngryTargetGoal`'s player filter: angry, not stung, angry at the player.
fn angry_target(t: &crate::mob::Tick<'_, '_>, id: EntityId) -> bool {
    let m = t.mob_ref();
    common::is_angry(m, t.env.game_time)
        && st(m).bee_flags & FLAG_HAS_STUNG == 0
        && common::angry_at(t, id)
}

/// `Bee.registerGoals`. The constructors of `ValidateHiveGoal` and `ValidateFlowerGoal` roll
/// their cooldowns, then the bee's own field initializer rolls the flower cooldown.
pub(super) fn register_goals(m: &mut MobData) {
    m.goals.add(0, MobGoal::melee(MeleeKind::Plain, 1.4, true));
    m.goals.add(2, MobGoal::breed(1.0));
    m.goals.add(3, MobGoal::tempt(1.25, TemptItems::Food));
    // `ValidateHiveGoal`: there are no hives to validate; its cooldown roll stays.
    let _hive_cooldown = m.random.next_int_between_inclusive(20, 40);
    let cooldown = m.random.next_int_between_inclusive(20, 40);
    m.goals.add(
        3,
        fam(Goal::BeeValidateFlower {
            cooldown,
            last_validate: -1,
        }),
    );
    m.goals.add(
        4,
        fam(Goal::BeePollinate {
            successful: 0,
            last_sound: 0,
            hover: Vec3::ZERO,
            has_hover: false,
            ticks: 0,
        }),
    );
    m.goals.add(
        5,
        MobGoal::FollowParent {
            speed: 1.25,
            parent: None,
            recalc: 0,
        },
    );
    m.goals
        .add(6, fam(Goal::BeeGoToKnownFlower { travelling: 0 }));
    m.goals.add(7, fam(Goal::BeeGrowCrop));
    m.goals.add(8, fam(Goal::BeeWander));
    m.goals.add(9, MobGoal::Float);
    m.targets.add(1, MobGoal::hurt_by(true));
    m.targets.add(
        2,
        MobGoal::Common(CommonGoal::nearest_player(true, angry_target)),
    );
    m.targets.add(
        3,
        MobGoal::Common(CommonGoal::ResetUniversalAnger {
            alert_same_type: true,
            last_hurt: 0,
        }),
    );
    // `remainingCooldownBeforeLocatingNewFlower = Mth.nextInt(random, 20, 60)`.
    let flower = m.random.next_int_between_inclusive(20, 60);
    super::st_mut(m).cooldown_flower = flower;
}

/// `isAngry`.
pub(super) fn is_angry(t: &Tick<'_, '_>) -> bool {
    common::is_angry(t.mob_ref(), t.env.game_time)
}

fn flag(m: &MobData, f: u8) -> bool {
    st(m).bee_flags & f != 0
}

fn set_flag(t: &mut Tick<'_, '_>, f: u8, on: bool) {
    let s = st_mut(t.mob());
    let old = s.bee_flags;
    if on {
        s.bee_flags |= f;
    } else {
        s.bee_flags &= !f;
    }
    if s.bee_flags != old {
        t.core_mut().mark_data_dirty();
    }
}

/// `setHasNectar`.
fn set_has_nectar(t: &mut Tick<'_, '_>, on: bool) {
    if on {
        st_mut(t.mob()).ticks_without_nectar = 0;
    }
    set_flag(t, FLAG_HAS_NECTAR, on);
}

/// `dropFlower`.
fn drop_flower(t: &mut Tick<'_, '_>) {
    let r = t.mob().random.next_int_between_inclusive(20, 60);
    let s = st_mut(t.mob());
    s.flower_pos = None;
    s.cooldown_flower = r;
}

/// `Bee.attractsBees(state)`.
pub(crate) fn attracts_bees(state: vanillite_data::block::BlockState) -> bool {
    if !block_tags().bee_attractive.contains(state) {
        return false;
    }
    if state.value("waterlogged") == Some("true") {
        return false;
    }
    state.block() != vanillite_data::block::ids::SUNFLOWER || state.value("half") == Some("upper")
}

/// `Bee.aiStep` after `super.aiStep()`: cooldowns and the roll animation flag.
pub(super) fn ai_step(t: &mut Tick<'_, '_>) {
    {
        let s = st_mut(t.mob());
        if s.stay_out_of_hive > 0 {
            s.stay_out_of_hive -= 1;
        }
        if s.cooldown_hive > 0 {
            s.cooldown_hive -= 1;
        }
        if s.cooldown_flower > 0 {
            s.cooldown_flower -= 1;
        }
    }
    let roll = is_angry(t)
        && !flag(t.mob_ref(), FLAG_HAS_STUNG)
        && t.mob_ref()
            .target
            .and_then(|id| t.other(id))
            .is_some_and(|o| o.pos.distance_to_sqr(t.pos()) < 4.0);
    set_flag(t, FLAG_ROLL, roll);
    // `isHiveValid` is false without hive block entities.
    if t.mob_ref().tick_count % 20 == 0 {
        st_mut(t.mob()).hive_pos = None;
    }
}

/// `Bee.customServerAiStep`: drowning, the death after stinging and the anger.
pub(super) fn custom_server_ai_step(t: &mut Tick<'_, '_>) {
    let stung = flag(t.mob_ref(), FLAG_HAS_STUNG);
    let in_water = t.mob_ref().fluids.in_water();
    {
        let s = st_mut(t.mob());
        if in_water {
            s.underwater_ticks += 1;
        } else {
            s.underwater_ticks = 0;
        }
    }
    if st(t.mob_ref()).underwater_ticks > 20 {
        let source = DamageSource::of(t.env.types.drown);
        living::hurt_self(t, source, 1.0);
        if !t.exists() {
            return;
        }
    }
    if stung {
        st_mut(t.mob()).time_since_sting += 1;
        let since = st(t.mob_ref()).time_since_sting;
        if since % 5 == 0
            && t.mob()
                .random
                .next_int_bounded((1200 - since).clamp(1, 1200))
                == 0
        {
            let health = t.mob_ref().health;
            let source = DamageSource::of(t.env.types.generic);
            living::hurt_self(t, source, health);
            if !t.exists() {
                return;
            }
        }
    }
    if !flag(t.mob_ref(), FLAG_HAS_NECTAR) {
        st_mut(t.mob()).ticks_without_nectar += 1;
    }
    let range = crate::mob::fam::anger_range(Kind::Bee);
    common::update_persistent_anger(t, false, range);
}

/// `Bee.doHurtTarget`: a sting with poison on normal and hard, then the bee is done.
pub(super) fn sting(t: &mut Tick<'_, '_>, target: EntityId) -> bool {
    let id = t.id;
    let source = combat::entity_source(t.ents, t.env.types.sting, id, id);
    #[allow(clippy::cast_possible_truncation)]
    let damage = f32::from(t.mob_ref().attrs.attack_damage as i16);
    let hurt = combat::hurt(t.level, t.ents, t.env, t.cx, target, source, damage);
    if hurt {
        let seconds = match t.env.difficulty {
            Difficulty::Normal => 10,
            Difficulty::Hard => 18,
            _ => 0,
        };
        if seconds > 0 {
            super::add_effect(t.ents, target, MobEffect::POISON, seconds * 20, 0);
        }
        if !t.exists() {
            return hurt;
        }
        set_flag(t, FLAG_HAS_STUNG, true);
        common::stop_being_angry(t.mob());
        kinds::play_sound(t, "minecraft:entity.bee.sting", 1.0, 1.0);
        t.mob().last_hurt_mob = Some(target);
    }
    hurt
}

/// `Bee.mobInteract`: flowers with a bee effect give it to the bee.
pub(super) fn interact(ix: &mut Interact<'_, '_>) -> Interaction {
    let name = crate::item::item_id(ix.stack).name();
    let effect = match name {
        "minecraft:open_eyeblossom" | "minecraft:closed_eyeblossom" => {
            Some((MobEffect::POISON, 25))
        }
        "minecraft:wither_rose" => Some((MobEffect::WITHER, 40)),
        _ => None,
    };
    if let Some((e, duration)) = effect
        && crate::mob::fam::is_food(Kind::Bee, ix.stack.item)
    {
        use_item(ix.ents, ix.player, ix.hand);
        super::add_effect(ix.ents, ix.target, e, duration, 0);
        return Interaction::Success;
    }
    animal_interact(ix.ents, ix.env, ix.player, ix.target, ix.hand, ix.stack)
}

/// `pathfindRandomlyTowards(target)`.
fn pathfind_randomly_towards(t: &mut Tick<'_, '_>, target: BlockPos) {
    let tv = randompos::bottom_center(target);
    let b = t.block_pos();
    let y_delta = vanillite_base::math::floor(tv.y) - b.y;
    let y_adjust = if y_delta > 2 {
        4
    } else if y_delta < -2 {
        -4
    } else {
        0
    };
    let dist = (b.x - target.x).abs() + (b.y - target.y).abs() + (b.z - target.z).abs();
    let (xz, y) = if dist < 15 {
        (dist / 2, dist / 2)
    } else {
        (6, 8)
    };
    let pi10 = f64::from(std::f32::consts::PI / 10.0);
    if let Some(next) = randompos::air_pos_towards(t, xz, y, y_adjust, tv, pi10) {
        nav::move_to_pos(t, next, 1.0);
    }
}

/// `BeePollinateGoal.findNearbyFlower`: the first reachable flower by Manhattan distance.
fn find_nearby_flower(t: &mut Tick<'_, '_>) -> Option<BlockPos> {
    let c = t.block_pos();
    for d in 0..=15 {
        for dy in -5_i32..=5 {
            for dx in -5_i32..=5 {
                let rest = d - dy.abs() - dx.abs();
                if !(0..=5).contains(&rest) {
                    continue;
                }
                for dz in if rest == 0 {
                    vec![0]
                } else {
                    vec![-rest, rest]
                } {
                    let p = BlockPos::new(c.x + dx, c.y + dy, c.z + dz);
                    if !attracts_bees(t.level.block(p)) {
                        continue;
                    }
                    let path = nav::create_path_to(t, p, 1);
                    if path.is_some_and(|p| p.reached) {
                        return Some(p);
                    }
                }
            }
        }
    }
    None
}

pub(super) fn can_use(t: &mut Tick<'_, '_>, g: &mut Goal) -> bool {
    if is_angry(t) && !matches!(g, Goal::BeeWander) {
        return false;
    }
    let s = st(t.mob_ref()).clone();
    match g {
        Goal::BeeValidateFlower {
            cooldown,
            last_validate,
        } => t.env.game_time > *last_validate + i64::from(*cooldown),
        Goal::BeePollinate { .. } => {
            if s.cooldown_flower > 0 || s.bee_flags & FLAG_HAS_NECTAR != 0 || t.cx.env.raining {
                return false;
            }
            match find_nearby_flower(t) {
                Some(p) => {
                    st_mut(t.mob()).flower_pos = Some(p);
                    let at = Vec3::new(
                        f64::from(p.x) + 0.5,
                        f64::from(p.y) + 0.5,
                        f64::from(p.z) + 0.5,
                    );
                    nav::move_to_pos(t, at, f64::from(1.2_f32));
                    true
                }
                None => {
                    let r = t.mob().random.next_int_between_inclusive(20, 60);
                    st_mut(t.mob()).cooldown_flower = r;
                    false
                }
            }
        }
        Goal::BeeGoToKnownFlower { .. } => s.flower_pos.is_some_and(|f| {
            !t.mob_ref().has_restriction()
                && s.ticks_without_nectar > 600
                && !close_to(t.block_pos(), f, 2)
        }),
        Goal::BeeGrowCrop => {
            if s.crops_grown >= 10 {
                return false;
            }
            if t.mob().random.next_float() < 0.3 {
                return false;
            }
            // `hasNectar() && isHiveValid()`: there are no hives yet.
            false
        }
        Goal::BeeWander => t.mob_ref().nav.is_done() && t.mob().random.next_int_bounded(10) == 0,
        _ => false,
    }
}

/// `BlockPos.closerThan(other, distance)`.
fn close_to(a: BlockPos, b: BlockPos, distance: i32) -> bool {
    let (dx, dy, dz) = (
        f64::from(a.x - b.x),
        f64::from(a.y - b.y),
        f64::from(a.z - b.z),
    );
    dx * dx + dy * dy + dz * dz < f64::from(distance * distance)
}

pub(super) fn can_continue(t: &mut Tick<'_, '_>, g: &mut Goal) -> bool {
    if is_angry(t) && !matches!(g, Goal::BeeWander) {
        return false;
    }
    match g {
        Goal::BeePollinate { successful, .. } => {
            let s = st(t.mob_ref());
            if !s.pollinating || s.flower_pos.is_none() || t.cx.env.raining {
                return false;
            }
            if *successful > 400 {
                t.mob().random.next_float() < 0.2
            } else {
                true
            }
        }
        Goal::BeeWander => !t.mob_ref().nav.is_done(),
        Goal::BeeValidateFlower { .. } => false,
        _ => can_use(t, g),
    }
}

pub(super) fn start(t: &mut Tick<'_, '_>, g: &mut Goal) {
    match g {
        Goal::BeeValidateFlower { last_validate, .. } => {
            if let Some(f) = st(t.mob_ref()).flower_pos
                && t.level.is_loaded(f)
                && !attracts_bees(t.level.block(f))
            {
                drop_flower(t);
            }
            *last_validate = t.env.game_time;
        }
        Goal::BeePollinate {
            successful,
            last_sound,
            ticks,
            ..
        } => {
            *successful = 0;
            *ticks = 0;
            *last_sound = 0;
            let s = st_mut(t.mob());
            s.pollinating = true;
            s.ticks_without_nectar = 0;
        }
        Goal::BeeGoToKnownFlower { travelling } => *travelling = 0,
        Goal::BeeWander => {
            // `findPos`: hover near the ground in the view direction (hives are not there yet,
            // so never towards the hive), else in the air or water.
            let rad = f64::from(t.core().rot().y_rot).to_radians();
            let (dx, dz) = (-rad.sin(), rad.cos());
            let half = randompos::half_pi();
            #[allow(clippy::cast_possible_truncation)]
            let found = randompos::hover_pos(t, 8, 7, dx, dz, half as f32, 3, 1)
                .or_else(|| randompos::air_and_water_pos(t, 8, 4, -2, dx, dz, half));
            if let Some(p) = found {
                let path = nav::create_path_to(t, BlockPos::containing(p.x, p.y, p.z), 1);
                nav::move_to_path(t, path, 1.0);
            }
        }
        _ => {}
    }
}

pub(super) fn stop(t: &mut Tick<'_, '_>, g: &mut Goal) {
    match g {
        Goal::BeePollinate { successful, .. } => {
            if *successful > 400 {
                set_has_nectar(t, true);
            }
            let s = st_mut(t.mob());
            s.pollinating = false;
            s.cooldown_flower = 200;
            t.mob().nav.stop();
        }
        Goal::BeeGoToKnownFlower { travelling } => {
            *travelling = 0;
            t.mob().nav.stop();
        }
        _ => {}
    }
}

pub(super) fn tick(t: &mut Tick<'_, '_>, g: &mut Goal) {
    match g {
        Goal::BeePollinate {
            successful,
            last_sound,
            hover,
            has_hover,
            ticks,
        } => {
            let Some(f) = st(t.mob_ref()).flower_pos else {
                return;
            };
            *ticks += 1;
            if *ticks > 600 {
                drop_flower(t);
                let s = st_mut(t.mob());
                s.pollinating = false;
                s.cooldown_flower = 200;
                return;
            }
            let flower = randompos::bottom_center(f) + Vec3::new(0.0, f64::from(0.6_f32), 0.0);
            if flower.distance_to_sqr(t.pos()) > 1.0 {
                *hover = flower;
                *has_hover = true;
                t.mob()
                    .move_ctl
                    .set_wanted_position(*hover, f64::from(0.35_f32));
                return;
            }
            if !*has_hover {
                *hover = flower;
                *has_hover = true;
            }
            let arrived = t.pos().distance_to_sqr(*hover) <= 0.01;
            let mut set_wanted = true;
            if !arrived && *ticks > 600 {
                drop_flower(t);
                return;
            }
            if arrived {
                if t.mob().random.next_int_bounded(25) == 0 {
                    let r = &mut t.mob().random;
                    let ox = f64::from((r.next_float() * 2.0 - 1.0) * 0.333_333_34);
                    let oz = f64::from((r.next_float() * 2.0 - 1.0) * 0.333_333_34);
                    *hover = Vec3::new(flower.x + ox, flower.y, flower.z + oz);
                    t.mob().nav.stop();
                } else {
                    set_wanted = false;
                }
                crate::mob::nav::look_at(t, flower);
            }
            if set_wanted {
                t.mob()
                    .move_ctl
                    .set_wanted_position(*hover, f64::from(0.35_f32));
            }
            *successful += 1;
            if t.mob().random.next_float() < 0.05 && *successful > *last_sound + 60 {
                *last_sound = *successful;
                kinds::play_sound(t, "minecraft:entity.bee.pollinate", 1.0, 1.0);
            }
        }
        Goal::BeeGoToKnownFlower { travelling } => {
            let Some(f) = st(t.mob_ref()).flower_pos else {
                return;
            };
            *travelling += 1;
            if *travelling > reduced(2400) {
                drop_flower(t);
            } else if t.mob_ref().nav.is_done() {
                if close_to(t.block_pos(), f, 48) {
                    pathfind_randomly_towards(t, f);
                } else {
                    drop_flower(t);
                }
            }
        }
        Goal::BeeGrowCrop => grow_crop(t),
        _ => {}
    }
}

/// `BeeGrowCropGoal.tick`: crops, stems, berries and cave vines below the bee grow a stage.
fn grow_crop(t: &mut Tick<'_, '_>) {
    if t.mob().random.next_int_bounded(30) != 0 {
        return;
    }
    let b = t.block_pos();
    for i in 1..=2 {
        let pos = BlockPos::new(b.x, b.y - i, b.z);
        let state = t.level.block(pos);
        if !block_tags().bee_growables.contains(state) {
            continue;
        }
        let grown = ["age"].iter().find_map(|prop| {
            let age: i32 = state.value(prop)?.parse().ok()?;
            let max = match state.block().name() {
                "minecraft:sweet_berry_bush" => 3,
                "minecraft:beetroots" | "minecraft:torchflower_crop" => 3,
                "minecraft:cave_vines" | "minecraft:cave_vines_plant" => return None,
                _ => 7,
            };
            (age < max).then(|| state.with(prop, &(age + 1).to_string()))?
        });
        if let Some(g) = grown {
            t.cx.actions.push(WorldAction::LevelEvent(2011, pos, 15));
            t.cx.actions.push(WorldAction::SetBlock(pos, g, 3));
            st_mut(t.mob()).crops_grown += 1;
        }
    }
}

/// `Bee.addAdditionalSaveData`.
pub(super) fn save(m: &MobData, out: &mut Compound) {
    let s = st(m);
    if let Some(p) = s.hive_pos {
        super::put_block_pos(out, "hive_pos", p);
    }
    if let Some(p) = s.flower_pos {
        super::put_block_pos(out, "flower_pos", p);
    }
    out.insert("HasNectar", s.bee_flags & FLAG_HAS_NECTAR != 0);
    out.insert("HasStung", s.bee_flags & FLAG_HAS_STUNG != 0);
    out.insert("TicksSincePollination", s.ticks_without_nectar);
    out.insert("CannotEnterHiveTicks", s.stay_out_of_hive);
    out.insert("CropsGrownSincePollination", s.crops_grown);
    common::save_anger(m, out);
}

/// `Bee.readAdditionalSaveData`.
pub(super) fn load(m: &mut MobData, tag: &Compound) {
    common::load_anger(m, tag, 0);
    let s = st_mut(m);
    let mut flags = 0;
    if super::boolean(tag, "HasNectar", false) {
        flags |= FLAG_HAS_NECTAR;
    }
    if super::boolean(tag, "HasStung", false) {
        flags |= FLAG_HAS_STUNG;
    }
    s.bee_flags = flags;
    s.ticks_without_nectar = super::int(tag, "TicksSincePollination").unwrap_or(0);
    s.stay_out_of_hive = super::int(tag, "CannotEnterHiveTicks").unwrap_or(0);
    s.crops_grown = super::int(tag, "CropsGrownSincePollination").unwrap_or(0);
    s.hive_pos = super::block_pos(tag, "hive_pos");
    s.flower_pos = super::block_pos(tag, "flower_pos");
}
