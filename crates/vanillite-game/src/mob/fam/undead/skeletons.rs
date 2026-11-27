//! The skeleton variants (`AbstractSkeleton` subclasses): strays, bogged, parched and wither
//! skeletons, and the classic skeleton's freezing into a stray.

use vanillite_base::random::{LegacyRandom, RandomSource};
use vanillite_base::{BlockPos, Vec3};
use vanillite_nbt::Compound;
use vanillite_world::entity::{Dimensions, EntityId};

use super::{Goal, Melee, State, goal, level_event, state, state_mut};
use crate::effect as effects;
use crate::effect::EffectInstance;
use crate::item::{EquipmentSlot, ItemStack};
use crate::level::Level;
use crate::mob::animal::Interaction;
use crate::mob::common::{self, CommonGoal};
use crate::mob::equipment::{self, PickupRules};
use crate::mob::fam::{Hurt, Interact, SpawnContext, SpawnReason};
use crate::mob::goals::{self, MeleeKind};
use crate::mob::{Environment, Kind, MobData, Tick, kinds};
use crate::player::encode::DataWriter;
use crate::survival::rules::Difficulty;

/// The state of a skeleton.
#[derive(Clone, Debug, Default)]
pub struct SkeletonState {
    /// The powder snow conversion of the classic skeleton (`FreezingTime`,
    /// `StrayConversionTime`, `DATA_STRAY_CONVERSION_ID`).
    pub freeze_time: i32,
    pub conversion_time: i32,
    pub converting: bool,
    /// `Bogged.isSheared`.
    pub sheared: bool,
    /// `reassessWeaponGoal` is due (after loading or picking up a weapon).
    pub reassess: bool,
}

/// The skeletons of this family.
pub fn is_skeleton(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::Stray | Kind::Bogged | Kind::Parched | Kind::WitherSkeleton
    )
}

fn ss_mut(m: &mut MobData) -> &mut SkeletonState {
    let s = state_mut(m);
    if !matches!(s, State::Skeleton(_)) {
        *s = State::Skeleton(SkeletonState::default());
    }
    match s {
        State::Skeleton(k) => k,
        _ => unreachable!("the skeleton state was just set"),
    }
}

fn ss(m: &MobData) -> Option<&SkeletonState> {
    match state(m) {
        State::Skeleton(k) => Some(k),
        _ => None,
    }
}

/// `getArrow` of a skeleton kind: the tipped arrow's effect and whether it burns.
pub fn skeleton_arrow(kind: Kind) -> (Option<EffectInstance>, bool) {
    match kind {
        Kind::Stray => (
            Some(EffectInstance::new(effects::MobEffect::SLOWNESS, 600, 0)),
            false,
        ),
        Kind::Bogged => (
            Some(EffectInstance::new(effects::MobEffect::POISON, 100, 0)),
            false,
        ),
        Kind::Parched => (
            Some(EffectInstance::new(effects::MobEffect::WEAKNESS, 600, 0)),
            false,
        ),
        Kind::WitherSkeleton => (None, true),
        _ => (None, false),
    }
}

// --- Properties -------------------------------------------------------------------------

pub fn ambient_sound(m: &MobData) -> Option<&'static str> {
    Some(m.kind.default_sound(kinds::DefaultSound::Ambient))
}

pub fn hurt_sound(m: &MobData) -> &'static str {
    m.kind.default_sound(kinds::DefaultSound::Hurt)
}

pub fn death_sound(m: &MobData) -> &'static str {
    m.kind.default_sound(kinds::DefaultSound::Death)
}

pub fn experience_reward(m: &mut MobData) -> i32 {
    super::zombies::equipment_experience(m, 5)
}

pub fn dimensions(_m: &MobData) -> Option<Dimensions> {
    None
}

pub fn check_spawn_rules(
    kind: Kind,
    level: &Level,
    env: &Environment,
    pos: BlockPos,
    random: &mut LegacyRandom,
    reason: SpawnReason,
) -> bool {
    use super::zombies::{is_spawner, monster_rules, surface_monster_rules};
    match kind {
        Kind::Stray => {
            // `checkStraySpawnRules`: the sky above the powder snow.
            let mut sky = BlockPos::new(pos.x, pos.y + 1, pos.z);
            while level.block(sky).block().name() == "minecraft:powder_snow" {
                sky = BlockPos::new(sky.x, sky.y + 1, sky.z);
            }
            monster_rules(kind, level, env, pos, random, reason)
                && (is_spawner(reason)
                    || crate::mob::can_see_sky(level, BlockPos::new(sky.x, sky.y - 1, sky.z)))
        }
        Kind::Parched => surface_monster_rules(kind, level, env, pos, random, reason),
        _ => monster_rules(kind, level, env, pos, random, reason),
    }
}

// --- Setup ------------------------------------------------------------------------------

pub fn init(m: &mut MobData) {
    ss_mut(m);
}

fn wolf(t: &Tick<'_, '_>, id: EntityId) -> bool {
    common::mob_of(t, id, Kind::Wolf)
}

fn piglin(t: &Tick<'_, '_>, id: EntityId) -> bool {
    t.ents
        .mob(id)
        .is_some_and(|m| matches!(m.kind, Kind::Piglin | Kind::PiglinBrute))
}

/// `AbstractSkeleton.registerGoals` and the weapon goal the constructor adds (melee until a
/// bow is in hand).
pub fn register_goals(m: &mut MobData) {
    let g = &mut m.goals;
    let tg = &mut m.targets;
    if m.kind == Kind::WitherSkeleton {
        tg.add(
            3,
            goals::Goal::Common(CommonGoal::nearest(10, true, piglin)),
        );
    }
    g.add(2, goals::Goal::RestrictSun);
    g.add(
        3,
        goals::Goal::FleeSun {
            speed: 1.0,
            wanted: Vec3::ZERO,
        },
    );
    g.add(
        3,
        goals::Goal::Common(CommonGoal::avoid(wolf, 6.0, 1.0, 1.2)),
    );
    g.add(5, goals::Goal::stroll(1.0));
    g.add(6, goals::Goal::look_at_player(8.0));
    g.add(6, goals::Goal::random_look());
    tg.add(1, goals::Goal::hurt_by(false));
    tg.add(2, goals::Goal::nearest_player(true));
    tg.add(
        3,
        goals::Goal::Common(CommonGoal::nearest(10, true, common::iron_golem)),
    );
    tg.add(
        3,
        goals::Goal::Common(CommonGoal::nearest(10, true, common::baby_turtle_on_land)),
    );
    g.add(
        4,
        goal(Goal::SkeletonMelee(Melee::new(
            MeleeKind::Plain,
            1.2,
            false,
        ))),
    );
    m.nav.can_float = false;
}

/// The melee goal of `reassessWeaponGoal` without a bow (the classic skeleton's start goal).
pub fn melee_goal() -> goals::Goal {
    goal(Goal::SkeletonMelee(Melee::new(
        MeleeKind::Plain,
        1.2,
        false,
    )))
}

/// `getHardAttackInterval` / `getAttackInterval`.
fn attack_interval(kind: Kind, difficulty: Difficulty) -> i32 {
    let slow = matches!(kind, Kind::Bogged | Kind::Parched);
    match (difficulty == Difficulty::Hard, slow) {
        (true, true) => 50,
        (true, false) => 20,
        (false, true) => 70,
        (false, false) => 40,
    }
}

/// `reassessWeaponGoal`: the bow goal with a bow in hand, else the melee goal; the weapon
/// slot is replaced in place (a running goal stops first).
pub fn reassess_weapon_goal(m: &mut MobData, difficulty: Difficulty) {
    let bow = vanillite_data::item::ids::BOW.0;
    let holds_bow = m.equipment.holds(bow) || {
        let off = m.equipment.get(EquipmentSlot::OffHand);
        !off.is_empty() && off.item == bow
    };
    let new = if holds_bow {
        goals::Goal::bow(1.0, attack_interval(m.kind, difficulty), 15.0)
    } else {
        goal(Goal::SkeletonMelee(Melee::new(
            MeleeKind::Plain,
            1.2,
            false,
        )))
    };
    let slot = m.goals.slots.iter_mut().find(|s| {
        matches!(
            s.goal,
            goals::Goal::Bow { .. }
                | goals::Goal::Fam(crate::mob::fam::FamGoal::Undead(Goal::SkeletonMelee(_)))
        )
    });
    if let Some(s) = slot {
        s.goal = new;
        s.running = false;
    }
    m.aggressive = false;
    m.using_item = -1;
}

pub fn finalize_spawn(m: &mut MobData, ctx: &mut SpawnContext<'_>) {
    let special = ctx.special_multiplier;
    if m.kind == Kind::WitherSkeleton {
        // `populateDefaultEquipmentSlots`: a stone sword; no enchantments.
        m.equipment.set(
            EquipmentSlot::MainHand,
            equipment::stack("minecraft:stone_sword"),
        );
    } else {
        equipment::populate_default_armor(m, ctx.difficulty, special);
        m.equipment
            .set(EquipmentSlot::MainHand, equipment::stack("minecraft:bow"));
        equipment::populate_default_enchantments(m, special);
    }
    reassess_weapon_goal(m, ctx.difficulty);
    m.can_pick_up_loot = m.random.next_float() < 0.55 * special;
    kinds::halloween_head(m);
    if m.kind == Kind::WitherSkeleton {
        m.attrs.attack_damage = 4.0;
        reassess_weapon_goal(m, ctx.difficulty);
    }
    ss_mut(m).reassess = false;
}

// --- Tick -------------------------------------------------------------------------------

/// A due `reassessWeaponGoal` (after loading or a new weapon).
pub fn reassess_if_due(t: &mut Tick<'_, '_>) {
    if ss(t.mob_ref()).is_some_and(|s| s.reassess) {
        let difficulty = t.env.difficulty;
        let m = t.mob();
        reassess_weapon_goal(m, difficulty);
        ss_mut(m).reassess = false;
    }
}

pub fn before_tick(t: &mut Tick<'_, '_>) {
    reassess_if_due(t);
    // `LivingEntity.updatingUsingItem`: the drawn bow counts on.
    let m = t.mob();
    if m.using_item >= 0 {
        m.using_item += 1;
    }
}

pub fn after_tick(_t: &mut Tick<'_, '_>) {}

pub fn ai_step(_t: &mut Tick<'_, '_>) {}

pub fn custom_server_ai_step(_t: &mut Tick<'_, '_>) {}

pub fn move_control(_t: &mut Tick<'_, '_>) -> bool {
    false
}

pub fn travel(_t: &mut Tick<'_, '_>, _input: Vec3) -> bool {
    false
}

/// `isInPowderSnow`: a powder snow block in the box.
fn in_powder_snow(t: &Tick<'_, '_>) -> bool {
    let b = t.aabb().inflate(-1.0e-5, -1.0e-5, -1.0e-5);
    let (x0, y0, z0) = (
        vanillite_base::math::floor(b.min_x),
        vanillite_base::math::floor(b.min_y),
        vanillite_base::math::floor(b.min_z),
    );
    let (x1, y1, z1) = (
        vanillite_base::math::floor(b.max_x),
        vanillite_base::math::floor(b.max_y),
        vanillite_base::math::floor(b.max_z),
    );
    for x in x0..=x1 {
        for y in y0..=y1 {
            for z in z0..=z1 {
                if t.level.block(BlockPos::new(x, y, z)).block().name() == "minecraft:powder_snow" {
                    return true;
                }
            }
        }
    }
    false
}

/// `Skeleton.tick`'s freezing conversion (`ConversionTracker`, before `Mob.tick`).
pub fn freezing_tick(t: &mut Tick<'_, '_>) {
    if !t.mob_ref().is_alive() || t.mob_ref().no_ai {
        return;
    }
    let afflicted = in_powder_snow(t);
    let s = ss_mut(t.mob());
    if afflicted {
        if s.converting {
            s.conversion_time -= 1;
            if s.conversion_time < 0
                && let Some(id) = super::convert_to(
                    t,
                    Kind::Stray,
                    true,
                    true,
                    vanillite_abi::transform_reason::FROZEN,
                    |n| {
                        ss_mut(n).reassess = true;
                    },
                )
                && !t.ents.mob(id).is_some_and(|m| m.silent)
                && let Some(p) = t.ents.store.get(id).map(|e| e.pos())
            {
                level_event(t.cx, 1048, BlockPos::containing(p.x, p.y, p.z), 0);
            }
        } else {
            s.freeze_time += 1;
            if s.freeze_time >= 140 {
                s.conversion_time = 300;
                s.converting = true;
                t.core_mut().mark_data_dirty();
            }
        }
    } else {
        s.freeze_time = -1;
        if s.converting {
            s.converting = false;
            t.core_mut().mark_data_dirty();
        }
    }
}

/// Whether the classic skeleton converts (its synched data 16).
pub fn is_converting(m: &MobData) -> bool {
    ss(m).is_some_and(|s| s.converting)
}

/// The conversion data of the classic skeleton.
pub fn save_conversion(m: &MobData, out: &mut Compound) {
    let s = ss(m).cloned().unwrap_or_default();
    out.insert(
        "StrayConversionTime",
        if s.converting { s.conversion_time } else { -1 },
    );
    out.insert("FreezingTime", s.freeze_time);
}

/// Reads the conversion data of the classic skeleton.
pub fn load_conversion(m: &mut MobData, tag: &Compound) {
    let int = |k: &str| crate::mob::entity_nbt::get_int(tag, k);
    let s = ss_mut(m);
    s.freeze_time = int("FreezingTime").unwrap_or(0);
    match int("StrayConversionTime") {
        Some(t) if t != -1 => {
            s.conversion_time = t;
            s.converting = true;
        }
        _ => s.converting = false,
    }
    s.reassess = true;
}

// --- Combat ---------------------------------------------------------------------------------

pub fn do_hurt_target(t: &mut Tick<'_, '_>, target: EntityId) -> Option<bool> {
    if t.mob_ref().kind != Kind::WitherSkeleton {
        return None;
    }
    let hit = crate::mob::combat::base_mob_attack(t, target);
    if hit {
        let me = t.id;
        super::add_effect(
            t.ents,
            target,
            EffectInstance::new(effects::MobEffect::WITHER, 200, 0),
            Some(me),
        );
    }
    Some(hit)
}

pub fn hurt_filter(_h: &mut Hurt<'_, '_>, amount: f32) -> Option<f32> {
    Some(amount)
}

pub fn after_hurt(_h: &mut Hurt<'_, '_>, _amount: f32) {}

pub fn on_die(_h: &mut Hurt<'_, '_>) {}

// --- Players ----------------------------------------------------------------------------

/// `Bogged.mobInteract`: shears take the mushrooms.
pub fn interact(ix: &mut Interact<'_, '_>) -> Interaction {
    let Some(m) = ix.ents.mob(ix.target) else {
        return Interaction::Pass;
    };
    if m.kind != Kind::Bogged || crate::item::item_id(ix.stack).name() != "minecraft:shears" {
        return Interaction::Pass;
    }
    if ss(m).is_some_and(|s| s.sheared) {
        return Interaction::Pass;
    }
    shear(
        ix.level,
        ix.ents,
        ix.env,
        ix.target,
        vanillite_protocol::packets::play::clientbound::SoundSource::Players,
    );
    crate::mob::animal::entity_game_event(
        ix.ents,
        ix.env,
        ix.target,
        crate::level::game_event::events().shear,
        Some(ix.player),
    );
    crate::mob::animal::hurt_held(ix.ents, ix.env, ix.player, ix.hand, 1);
    Interaction::Success
}

/// `Bogged.readyForShearing`.
pub(crate) fn ready_for_shearing(m: &MobData) -> bool {
    m.kind == Kind::Bogged && !ss(m).is_some_and(|s| s.sheared)
}

/// `Bogged.shear`: the sound, `shearing/bogged` dropped at the top, then sheared.
pub(crate) fn shear(
    level: &crate::level::Level,
    ents: &mut crate::entities::Entities,
    env: &mut crate::survival::player::Env<'_>,
    target: vanillite_world::entity::EntityId,
    source: vanillite_protocol::packets::play::clientbound::SoundSource,
) {
    let Some(e) = ents.store.get(target) else {
        return;
    };
    let pos = e.pos();
    let height = f64::from(e.dims().height);
    crate::survival::play_entity_sound(
        env.out,
        ents,
        None,
        target,
        "minecraft:entity.bogged.shear",
        source,
        1.0,
        1.0,
    );
    let drops = super::simple_table_loot(level, "minecraft:shearing/bogged", env.random);
    let Some(m) = ents.mob_mut(target) else {
        return;
    };
    ss_mut(m).sheared = true;
    for stack in drops {
        super::drop_item(env, pos + Vec3::new(0.0, height, 0.0), stack);
    }
    if let Some(mut e) = ents.store.get_mut(target) {
        e.mark_data_dirty();
    }
}

pub fn pick_up_items(t: &mut Tick<'_, '_>) {
    let rules = if t.mob_ref().kind == Kind::WitherSkeleton {
        PickupRules {
            wants: |_, s| !in_tag(s, "minecraft:wither_skeleton_disliked_weapons"),
            preferred_weapon: None,
        }
    } else {
        PickupRules {
            wants: |_, s| !in_tag(s, "minecraft:spears"),
            preferred_weapon: Some("minecraft:skeleton_preferred_weapons"),
        }
    };
    let before = t.mob_ref().equipment.get(EquipmentSlot::MainHand).clone();
    equipment::pick_up_items(t, &rules, None, (1.0, 0.0, 1.0));
    // `onEquipItem` reassesses the weapon goal.
    if t.exists() && *t.mob_ref().equipment.get(EquipmentSlot::MainHand) != before {
        ss_mut(t.mob()).reassess = true;
    }
}

fn in_tag(s: &ItemStack, tag: &str) -> bool {
    crate::player::play::game_data().is_some_and(|d| d.item_in_tag(s.item, tag))
}

// --- Client data and storage ------------------------------------------------------------

pub fn encode_data(w: &mut DataWriter<'_>, m: &MobData) {
    if m.kind == Kind::Bogged {
        w.boolean(16, ss(m).is_some_and(|s| s.sheared), false);
    }
}

pub fn save(m: &MobData, out: &mut Compound) {
    if m.kind == Kind::Bogged {
        out.insert("sheared", ss(m).is_some_and(|s| s.sheared));
    }
}

pub fn load(m: &mut MobData, tag: &Compound) {
    let s = ss_mut(m);
    s.reassess = true;
    if m.kind == Kind::Bogged {
        ss_mut(m).sheared = crate::mob::entity_nbt::get_bool(tag, "sheared").unwrap_or(false);
    }
}

// --- Goals --------------------------------------------------------------------------------

pub fn goal_flags(g: &Goal) -> u8 {
    use crate::mob::goals::flag::{LOOK, MOVE};
    match g {
        Goal::SkeletonMelee(_) => MOVE | LOOK,
        _ => 0,
    }
}

pub fn goal_every_tick(g: &Goal) -> bool {
    matches!(g, Goal::SkeletonMelee(_))
}

pub fn goal_can_use(t: &mut Tick<'_, '_>, g: &mut Goal) -> bool {
    match g {
        Goal::SkeletonMelee(melee) => melee.with(t, goals::can_use),
        _ => false,
    }
}

pub fn goal_can_continue(t: &mut Tick<'_, '_>, g: &mut Goal) -> bool {
    match g {
        Goal::SkeletonMelee(melee) => melee.with(t, goals::can_continue),
        _ => false,
    }
}

pub fn goal_start(t: &mut Tick<'_, '_>, g: &mut Goal) {
    if let Goal::SkeletonMelee(melee) = g {
        melee.with(t, goals::start);
        goals::set_aggressive(t, true);
    }
}

pub fn goal_stop(t: &mut Tick<'_, '_>, g: &mut Goal) {
    if let Goal::SkeletonMelee(melee) = g {
        melee.with(t, goals::stop);
        goals::set_aggressive(t, false);
    }
}

pub fn goal_tick(t: &mut Tick<'_, '_>, g: &mut Goal) {
    if let Goal::SkeletonMelee(melee) = g {
        melee.with(t, goals::tick);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variants_get_a_bow_and_their_goal() {
        for kind in [Kind::Stray, Kind::Bogged, Kind::Parched] {
            let mut m = MobData::new(kind, 11);
            let mut ctx = SpawnContext::simple(Difficulty::Hard, SpawnReason::Command);
            kinds::finalize_spawn(
                &mut m,
                &mut ctx,
                &mut vanillite_base::random::LegacyRandom::new(1),
            );
            assert!(m.equipment.holds(vanillite_data::item::ids::BOW.0));
            let interval = m.goals.slots.iter().find_map(|s| match s.goal {
                goals::Goal::Bow { interval_min, .. } => Some(interval_min),
                _ => None,
            });
            let want = if kind == Kind::Stray { 20 } else { 50 };
            assert_eq!(interval, Some(want), "{kind:?}");
        }
    }

    #[test]
    fn wither_skeletons_fight_with_swords() {
        let mut m = MobData::new(Kind::WitherSkeleton, 5);
        let mut ctx = SpawnContext::simple(Difficulty::Normal, SpawnReason::Command);
        kinds::finalize_spawn(
            &mut m,
            &mut ctx,
            &mut vanillite_base::random::LegacyRandom::new(1),
        );
        assert_eq!(m.attrs.attack_damage, 4.0);
        assert!(
            !m.goals
                .slots
                .iter()
                .any(|s| matches!(s.goal, goals::Goal::Bow { .. }))
        );
        assert_eq!(skeleton_arrow(Kind::WitherSkeleton), (None, true));
    }

    #[test]
    fn bogged_sheared_round_trip() {
        let mut m = MobData::new(Kind::Bogged, 1);
        ss_mut(&mut m).sheared = true;
        let mut out = Compound::new();
        save(&m, &mut out);
        let mut n = MobData::new(Kind::Bogged, 2);
        load(&mut n, &out);
        assert!(ss(&n).is_some_and(|s| s.sheared));
    }
}
