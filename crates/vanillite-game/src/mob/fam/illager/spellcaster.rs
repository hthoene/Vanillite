//! Evokers and illusioners (`SpellcasterIllager`): their spells (vexes, fangs, wololo,
//! mirror images, blindness).

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Vec3};
use vanillite_world::entity::{EntityId, Rotation};

use super::super::super::common::{self, CommonGoal};
use super::super::super::goals::{self, Goal as MobGoal};
use super::super::super::kinds::{self, Kind};
use super::super::super::missile::{self, MissileData, MissileKind};
use super::super::super::{MobData, Tick};
use super::super::{SpawnContext, SpawnReason};
use super::raider::{self, Ignore, LookFilter};
use super::{
    Goal, dirty, fam, look_at_player, sound, st, st_mut, target_mobs, target_players, with_memory,
};
use crate::effect as effects;
use crate::effect::{EffectInstance, MobEffect};
use crate::survival::rules::Rule;

/// `SpellcasterIllager.IllagerSpell` ids.
pub mod spell {
    pub const NONE: u8 = 0;
    pub const SUMMON_VEX: u8 = 1;
    pub const FANGS: u8 = 2;
    pub const WOLOLO: u8 = 3;
    pub const DISAPPEAR: u8 = 4;
    pub const BLINDNESS: u8 = 5;
}

/// `SpellcasterUseSpellGoal` with its spell.
#[derive(Clone, Copy, Debug)]
pub struct UseSpell {
    pub spell: u8,
    pub warmup: i32,
    pub next_attack_tick: i32,
    /// `IllusionerBlindnessSpellGoal.lastTargetId` (network id).
    pub last_target: i32,
}

fn use_spell(spell: u8) -> MobGoal {
    fam(Goal::Spell(UseSpell {
        spell,
        warmup: 0,
        next_attack_tick: 0,
        last_target: 0,
    }))
}

/// `Evoker.registerGoals` and `Illusioner.registerGoals`.
pub fn register_goals(m: &mut MobData) {
    let evoker = m.kind == Kind::Evoker;
    let g = &mut m.goals;
    let tg = &mut m.targets;
    g.add(4, raider::patrol());
    g.add(0, MobGoal::Float);
    g.add(1, fam(Goal::SpellCasting));
    if evoker {
        g.add(
            2,
            MobGoal::Common(CommonGoal::avoid(common::any_player, 8.0, 0.6, 1.0)),
        );
        g.add(3, super::avoid_creaking(0.6, 1.0));
        g.add(4, use_spell(spell::SUMMON_VEX));
        g.add(5, use_spell(spell::FANGS));
        g.add(6, use_spell(spell::WOLOLO));
    } else {
        g.add(3, super::avoid_creaking(1.0, 1.2));
        g.add(4, use_spell(spell::DISAPPEAR));
        g.add(5, use_spell(spell::BLINDNESS));
        g.add(6, MobGoal::bow(0.5, 20, 15.0));
    }
    g.add(8, raider::stroll(0.6, 120));
    g.add(9, look_at_player(3.0, 1.0));
    g.add(10, raider::look_at_mobs(8.0, 0.02, LookFilter::AnyMob));
    tg.add(1, raider::hurt_by(Ignore::Raiders, true));
    tg.add(2, with_memory(target_players(true), 300));
    tg.add(3, with_memory(target_mobs(false, common::villager), 300));
    let golems = target_mobs(false, common::iron_golem);
    tg.add(
        3,
        if evoker {
            golems
        } else {
            with_memory(golems, 300)
        },
    );
}

fn is_casting(m: &MobData) -> bool {
    st(m).spell_ticks > 0
}

/// `SpellcasterCastingSpellGoal.tick` (the evoker also looks at its wololo sheep).
pub fn casting_tick(t: &mut Tick<'_, '_>) {
    let look = t.mob_ref().target.or(if t.mob_ref().kind == Kind::Evoker {
        st(t.mob_ref()).wololo_target
    } else {
        None
    });
    if let Some(o) = look.and_then(|id| t.other(id)) {
        // `getMaxHeadYRot` 75, `getMaxHeadXRot` 40.
        t.mob()
            .look
            .set_look_at(Vec3::new(o.pos.x, o.eye_y, o.pos.z), 75.0, 40.0);
    }
}

fn target_alive(t: &Tick<'_, '_>) -> Option<EntityId> {
    t.mob_ref().target.filter(|&id| t.other(id).is_some())
}

fn base_can_use(t: &Tick<'_, '_>, s: &UseSpell) -> bool {
    target_alive(t).is_some()
        && !is_casting(t.mob_ref())
        && t.mob_ref().tick_count >= s.next_attack_tick
}

/// The vexes within 16 blocks (`getNearbyEntities(Vex.class, ...)`).
fn nearby_vexes(t: &mut Tick<'_, '_>) -> i32 {
    let area = t.aabb().inflate(16.0, 16.0, 16.0);
    let p = t.pos();
    let mut n = 0;
    let ents = &*t.ents;
    ents.store.for_each_in(&area, false, |id| {
        if ents
            .mob(id)
            .is_some_and(|m| m.kind == Kind::Vex && m.is_alive())
            && ents
                .store
                .get(id)
                .is_some_and(|e| e.pos().distance_to_sqr(p) <= 256.0)
        {
            n += 1;
        }
    });
    n
}

pub fn can_use(t: &mut Tick<'_, '_>, s: &mut UseSpell) -> bool {
    match s.spell {
        spell::SUMMON_VEX => {
            if !base_can_use(t, s) {
                return false;
            }
            let vexes = nearby_vexes(t);
            t.mob().random.next_int_bounded(8) + 1 > vexes
        }
        spell::WOLOLO => wololo_can_use(t, s),
        spell::DISAPPEAR => base_can_use(t, s) && !t.mob_ref().effects.has(MobEffect::INVISIBILITY),
        spell::BLINDNESS => {
            if !base_can_use(t, s) {
                return false;
            }
            let Some(target) = t.mob_ref().target else {
                return false;
            };
            let net = t.ents.store.get(target).map_or(0, |e| e.network_id().0);
            // `getCurrentDifficultyAt(blockPosition()).isHarderThan(NORMAL.ordinal())`.
            net != s.last_target
                && crate::level::difficulty::at(t.level, t.block_pos()).is_harder_than(2.0)
        }
        _ => base_can_use(t, s),
    }
}

pub fn can_continue(t: &mut Tick<'_, '_>, s: &mut UseSpell) -> bool {
    if s.spell == spell::WOLOLO {
        return st(t.mob_ref()).wololo_target.is_some() && s.warmup > 0;
    }
    target_alive(t).is_some() && s.warmup > 0
}

/// (warmup, casting time, interval, prepare sound) of a spell.
fn spell_timing(spell: u8) -> (i32, i32, i32, Option<&'static str>) {
    match spell {
        spell::SUMMON_VEX => (20, 100, 340, Some("minecraft:entity.evoker.prepare_summon")),
        spell::FANGS => (20, 40, 100, Some("minecraft:entity.evoker.prepare_attack")),
        spell::WOLOLO => (40, 60, 140, Some("minecraft:entity.evoker.prepare_wololo")),
        spell::DISAPPEAR => (
            20,
            20,
            340,
            Some("minecraft:entity.illusioner.prepare_mirror"),
        ),
        _ => (
            20,
            20,
            180,
            Some("minecraft:entity.illusioner.prepare_blindness"),
        ),
    }
}

pub fn start(t: &mut Tick<'_, '_>, s: &mut UseSpell) {
    let (warmup, casting, interval, prepare) = spell_timing(s.spell);
    s.warmup = goals::reduced(warmup);
    let tick = t.mob_ref().tick_count;
    st_mut(t.mob()).spell_ticks = casting;
    s.next_attack_tick = tick + interval;
    if let Some(prepare) = prepare {
        sound(t, prepare, 1.0, 1.0);
    }
    st_mut(t.mob()).spell = s.spell;
    dirty(t);
    if s.spell == spell::BLINDNESS
        && let Some(target) = t.mob_ref().target
    {
        s.last_target = t.ents.store.get(target).map_or(0, |e| e.network_id().0);
    }
}

pub fn stop(t: &mut Tick<'_, '_>, s: &mut UseSpell) {
    if s.spell == spell::WOLOLO {
        st_mut(t.mob()).wololo_target = None;
    }
}

pub fn tick(t: &mut Tick<'_, '_>, s: &mut UseSpell) {
    s.warmup -= 1;
    if s.warmup == 0 {
        match s.spell {
            spell::SUMMON_VEX => summon_vexes(t),
            spell::FANGS => cast_fangs(t),
            spell::WOLOLO => wololo(t),
            spell::DISAPPEAR => {
                effects::living::add_to_mob(
                    t.mob(),
                    EffectInstance::new(MobEffect::INVISIBILITY, 1200, 0),
                );
                dirty(t);
            }
            _ => {
                if let Some(target) = t.mob_ref().target
                    && let Some(m) = t.ents.mob_mut(target)
                {
                    // Players get the blindness with the effect system (#74).
                    effects::living::add_to_mob(
                        m,
                        EffectInstance::new(MobEffect::BLINDNESS, 400, 0),
                    );
                }
            }
        }
        let cast = if t.mob_ref().kind == Kind::Evoker {
            "minecraft:entity.evoker.cast_spell"
        } else {
            "minecraft:entity.illusioner.cast_spell"
        };
        sound(t, cast, 1.0, 1.0);
    }
}

/// `EvokerSummonSpellGoal.performSpellCasting`: three vexes bound to where they appear.
fn summon_vexes(t: &mut Tick<'_, '_>) {
    let here = t.block_pos();
    let owner_uuid = t.ents.store.get(t.id).map(|e| e.uuid());
    let owner_id = t.id;
    for _ in 0..3 {
        let r = &mut t.mob().random;
        let (dx, dz) = (-2 + r.next_int_bounded(5), -2 + r.next_int_bounded(5));
        let pos = BlockPos::new(here.x + dx, here.y + 1, here.z + dz);
        let seed = crate::mob::new_entity_seed();
        let mut vex = MobData::new(Kind::Vex, seed);
        let mut ctx = SpawnContext::at(t.level, pos, SpawnReason::MobSummoned);
        kinds::finalize_spawn(&mut vex, &mut ctx, t.env.random);
        let life = 20 * (30 + t.mob().random.next_int_bounded(90));
        let s = st_mut(&mut vex);
        s.owner = owner_uuid;
        s.owner_id = Some(owner_id);
        s.bound_origin = Some(pos);
        s.life_ticks = Some(life);
        let uuid = crate::mob::new_uuid();
        // `snapTo(pos)`: the bottom centre of the block.
        let at = Vec3::new(
            f64::from(pos.x) + 0.5,
            f64::from(pos.y),
            f64::from(pos.z) + 0.5,
        );
        super::super::super::spawn(
            t.ents,
            t.level.network_ids(),
            uuid,
            at,
            // `snapTo(pos, 0, 0)` keeps the constructor's head yaw.
            Rotation {
                y_head_rot: crate::mob::constructor_y_rot(seed),
                ..Rotation::default()
            },
            vex,
        );
    }
}

/// `EvokerAttackSpellGoal.performSpellCasting`: a line of fangs, or two rings up close.
fn cast_fangs(t: &mut Tick<'_, '_>) {
    let Some(target) = t.mob_ref().target else {
        return;
    };
    let Some(o) = t.other(target) else {
        return;
    };
    let p = t.pos();
    let min_y = o.pos.y.min(p.y);
    let max_y = o.pos.y.max(p.y) + 1.0;
    #[allow(clippy::cast_possible_truncation)]
    let angle = vanillite_base::math::atan2(o.pos.z - p.z, o.pos.x - p.x) as f32;
    let pi = std::f32::consts::PI;
    let cos = |a: f32| f64::from(vanillite_base::math::cos(f64::from(a)));
    let sin = |a: f32| f64::from(vanillite_base::math::sin(f64::from(a)));
    if p.distance_to_sqr(o.pos) < 9.0 {
        for i in 0..5 {
            #[allow(clippy::cast_precision_loss)]
            let a = angle + i as f32 * pi * 0.4;
            fang(
                t,
                p.x + cos(a) * 1.5,
                p.z + sin(a) * 1.5,
                min_y,
                max_y,
                a,
                0,
            );
        }
        for i in 0..8 {
            #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
            let a = angle + i as f32 * pi * 2.0 / 8.0 + (std::f64::consts::PI * 2.0 / 5.0) as f32;
            fang(
                t,
                p.x + cos(a) * 2.5,
                p.z + sin(a) * 2.5,
                min_y,
                max_y,
                a,
                3,
            );
        }
    } else {
        for i in 0..16 {
            let reach = 1.25 * f64::from(i + 1);
            fang(
                t,
                p.x + cos(angle) * reach,
                p.z + sin(angle) * reach,
                min_y,
                max_y,
                angle,
                i,
            );
        }
    }
}

/// `createSpellEntity`: a fang on the first sturdy floor going down from `max_y`.
fn fang(t: &mut Tick<'_, '_>, x: f64, z: f64, min_y: f64, max_y: f64, angle: f32, delay: i32) {
    let mut pos = BlockPos::containing(x, max_y, z);
    let floor_min = vanillite_base::math::floor(min_y) - 1;
    let mut found = false;
    let mut top = 0.0;
    loop {
        let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
        let below_state = t.level.block(below);
        // `isFaceSturdy(UP)`: bit 1 of `sturdy_full`.
        if below_state.misc().sturdy_full & 2 != 0 {
            let state = t.level.block(pos);
            if !state.is_air() {
                top = state
                    .collision_shape()
                    .iter()
                    .map(|b| b[4])
                    .fold(0.0_f64, f64::max);
            }
            found = true;
            break;
        }
        pos = below;
        if pos.y < floor_min {
            break;
        }
    }
    if !found {
        return;
    }
    let mut d = MissileData::new(MissileKind::EvokerFangs);
    d.ints[0] = delay;
    // `lifeTicks` and the spike event flag.
    d.ints[1] = 22;
    d.ints[2] = 0;
    let rot = Rotation {
        y_rot: angle * (180.0 / std::f32::consts::PI),
        x_rot: 0.0,
        y_head_rot: 0.0,
    };
    let at = Vec3::new(x, f64::from(pos.y) + top, z);
    missile::spawn_from(t, at, Vec3::ZERO, rot, d);
}

/// `EvokerWololoSpellGoal.canUse`: a blue sheep within 16 blocks.
fn wololo_can_use(t: &mut Tick<'_, '_>, s: &UseSpell) -> bool {
    if t.mob_ref().target.is_some() || is_casting(t.mob_ref()) {
        return false;
    }
    if t.mob_ref().tick_count < s.next_attack_tick {
        return false;
    }
    if !t.env.rules.bool(Rule::MOB_GRIEFING) {
        return false;
    }
    let area = t.aabb().inflate(16.0, 4.0, 16.0);
    let p = t.pos();
    let mut sheep = Vec::new();
    {
        let ents = &*t.ents;
        ents.store.for_each_in(&area, false, |id| {
            if ents
                .mob(id)
                .is_some_and(|m| m.kind == Kind::Sheep && m.is_alive() && m.sheep_color == 11)
                && ents
                    .store
                    .get(id)
                    .is_some_and(|e| e.pos().distance_to_sqr(p) <= 256.0)
            {
                sheep.push(id);
            }
        });
    }
    // `forNonCombat` checks line of sight too.
    sheep.retain(|&id| super::super::super::living::has_line_of_sight(t, id));
    if sheep.is_empty() {
        return false;
    }
    let n = i32::try_from(sheep.len()).unwrap_or(i32::MAX);
    let pick = usize::try_from(t.mob().random.next_int_bounded(n)).unwrap_or(0);
    st_mut(t.mob()).wololo_target = sheep.get(pick).copied();
    true
}

/// `performSpellCasting` of wololo: the blue sheep turns red.
fn wololo(t: &mut Tick<'_, '_>) {
    let Some(sheep) = st(t.mob_ref()).wololo_target else {
        return;
    };
    if let Some(m) = t.ents.mob_mut(sheep)
        && m.is_alive()
    {
        m.sheep_color = 14;
        if let Some(mut e) = t.ents.store.get_mut(sheep) {
            e.mark_data_dirty();
        }
    }
}
