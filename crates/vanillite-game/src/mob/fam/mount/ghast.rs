//! Happy ghasts and ghastlings (`HappyGhast`): ghastlings think with their brain (in
//! [`super::ai`]), adults float around with goals and the ghast move control, carry up to
//! four riders with a harness and stay still while a player stands on them.

use vanillite_base::random::RandomSource;
use vanillite_base::{Aabb, BlockPos, Vec3};
use vanillite_nbt::Compound;
use vanillite_world::entity::EntityId;

use super::{Goal, State, encode_ageable, load_ageable, save_ageable};
use crate::item::EquipmentSlot;
use crate::level::Level;
use crate::mob::animal::{self, Interaction};
use crate::mob::fam::{FamGoal, Interact};
use crate::mob::goals;
use crate::mob::nav::{LookKind, MoveKind, NavKind, NavVariant, Operation};
use crate::mob::{MobData, Tick};
use crate::player::encode::DataWriter;

/// `BABY_SCALE`.
pub const BABY_SCALE: f32 = 0.2375;

/// The happy ghast part of a mob.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HappyGhast {
    pub leash_holder_time: i32,
    /// `serverStillTimeout`.
    pub still_timeout: i32,
    /// `IS_LEASH_HOLDER`.
    pub leash_holder: bool,
    /// `GhastMoveControl.floatDuration`.
    pub float_duration: i32,
}

/// A goal of the adult.
#[derive(Clone, Copy, Debug)]
pub enum GhastGoal {
    /// `HappyGhastFloatGoal`.
    Float,
    /// `TemptGoal.ForNonPathfinders(this, 1.0, ..., false, 7.0)`.
    Tempt {
        player: Option<EntityId>,
        calm_down: i32,
    },
    /// `Ghast.RandomFloatAroundGoal(this, 16)`.
    RandomFloat,
}

fn ghast(m: &MobData) -> HappyGhast {
    match super::state(m) {
        State::HappyGhast(g) => g.clone(),
        _ => HappyGhast::default(),
    }
}

fn ghast_mut(m: &mut MobData) -> &mut HappyGhast {
    let s = super::state_mut(m);
    if !matches!(s, State::HappyGhast(_)) {
        *s = State::HappyGhast(HappyGhast::default());
    }
    match s {
        State::HappyGhast(g) => g,
        _ => unreachable!("set above"),
    }
}

pub fn init(m: &mut MobData) {
    ghast_mut(m);
    // `LivingEntity`'s constructor makes the brain (its sensors draw their start delays) for
    // adults too; only ghastlings use it.
    super::ai::happy_ghast_brain(m);
    setup_for_age(m);
}

/// `EntityTypes.HAPPY_GHAST.create(level, BREEDING)` and `setBaby(true)`: a ghastling
/// (`ageBoundaryReached` runs `babyGhastSetup` on the brain made by the constructor).
pub fn new_ghastling(seed: i64) -> MobData {
    let mut m = MobData::new(crate::mob::Kind::HappyGhast, seed);
    // The constructor's brain: the adult setup dropped it, so it is made again from the
    // random as it was then (after the constructor's yaw draw), which ends where it is now.
    let mut then = vanillite_base::random::LegacyRandom::new(seed);
    let _ = then.next_float();
    let now = std::mem::replace(&mut m.random, then);
    m.brain = None;
    super::ai::happy_ghast_brain(&mut m);
    m.random = now;
    m.baby = true;
    m.age = -24000;
    setup_for_age(&mut m);
    register_goals(&mut m);
    m
}

/// `adultGhastSetup` / `babyGhastSetup`: the controls of the age.
fn setup_for_age(m: &mut MobData) {
    if m.baby {
        m.move_ctl.kind = MoveKind::Flying {
            max_turn: 180,
            hovers: true,
        };
        m.look.kind = LookKind::Default;
        m.nav.kind = NavKind::Flying;
        m.nav.variant = NavVariant::BabyHappyGhast;
        m.nav.can_float = true;
        m.nav.can_open_doors = false;
        let follow = m.attrs.follow_range;
        m.nav.set_required_path_length(48.0, follow);
        if m.brain.is_none() {
            super::ai::happy_ghast_brain(m);
        }
    } else {
        m.move_ctl.kind = MoveKind::Custom;
        m.look.kind = LookKind::Custom;
        m.nav.kind = NavKind::Flying;
        m.nav.variant = NavVariant::Default;
        let follow = m.attrs.follow_range;
        m.nav.set_required_path_length(16.0, follow);
        m.brain = None;
    }
}

/// `registerGoals` (adults).
pub fn register_goals(m: &mut MobData) {
    m.goals.slots.clear();
    if m.baby {
        return;
    }
    let g = |goal| goals::Goal::Fam(FamGoal::Mount(Goal::Ghast(goal)));
    m.goals.add(3, g(GhastGoal::Float));
    m.goals.add(
        4,
        g(GhastGoal::Tempt {
            player: None,
            calm_down: 0,
        }),
    );
    m.goals.add(5, g(GhastGoal::RandomFloat));
}

pub fn ambient_sound(m: &MobData) -> &'static str {
    if m.baby {
        "minecraft:entity.ghastling.ambient"
    } else {
        "minecraft:entity.happy_ghast.ambient"
    }
}

pub fn hurt_sound(m: &MobData) -> &'static str {
    if m.baby {
        "minecraft:entity.ghastling.hurt"
    } else {
        "minecraft:entity.happy_ghast.hurt"
    }
}

pub fn death_sound(m: &MobData) -> &'static str {
    if m.baby {
        "minecraft:entity.ghastling.death"
    } else {
        "minecraft:entity.happy_ghast.death"
    }
}

/// `getWalkTargetValue`: air with ground two blocks below is best.
pub fn walk_target_value(level: &Level, pos: BlockPos) -> f64 {
    if !level.block(pos).is_air() {
        return 0.0;
    }
    let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
    let below2 = BlockPos::new(pos.x, pos.y - 2, pos.z);
    if level.block(below).is_air() && !level.block(below2).is_air() {
        10.0
    } else {
        5.0
    }
}

/// Wears a harness.
fn harnessed(m: &MobData) -> bool {
    !m.equipment.get(EquipmentSlot::Body).is_empty()
}

/// `isOnStillTimeout`.
pub fn on_still_timeout(m: &MobData) -> bool {
    ghast(m).still_timeout > 0
}

/// `getControllingPassenger`: the first player of a harnessed adult that is not standing still.
pub fn rider_controls(m: &MobData) -> bool {
    !m.baby && harnessed(m) && !on_still_timeout(m)
}

/// `HappyGhast.travel`: flying with the flying speed (`travelFlying`).
pub fn travel(t: &mut Tick<'_, '_>, input: Vec3) -> bool {
    // `FLYING_SPEED` 0.05 × 5 / 3.
    let speed = 0.05_f32 * 5.0 / 3.0;
    fly(t, input, speed);
    true
}

/// `LivingEntity.travelFlying(input, speed)`: water and lava slow down, air keeps 91 %.
pub fn fly(t: &mut Tick<'_, '_>, input: Vec3, speed: f32) {
    let fluids = t.mob_ref().fluids;
    // `scale(0.8F)`, `scale(0.5)`, `scale(0.91F)`: the floats widened.
    let drag = if fluids.in_water() {
        f64::from(0.8_f32)
    } else if fluids.in_lava() {
        0.5
    } else {
        f64::from(0.91_f32)
    };
    crate::mob::living::move_relative(t, speed, input);
    let v = t.velocity();
    crate::mob::living::move_self(t, v);
    if !t.exists() {
        return;
    }
    let v = t.velocity();
    t.set_velocity(Vec3::new(v.x * drag, v.y * drag, v.z * drag));
}

/// `HappyGhast.tick`: the leash holder flag and the still timeout (a player standing on top).
pub fn after_tick(t: &mut Tick<'_, '_>) {
    let tick_count = t.mob_ref().tick_count;
    let player_above = player_above(t);
    let g = ghast_mut(t.mob());
    if g.leash_holder_time > 0 {
        g.leash_holder_time -= 1;
    }
    let holder = g.leash_holder_time > 0;
    let mut dirty = holder != g.leash_holder;
    g.leash_holder = holder;
    let was_still = g.still_timeout > 0;
    if g.still_timeout > 0 && tick_count > 60 {
        g.still_timeout -= 1;
    }
    if player_above {
        g.still_timeout = 10;
    }
    dirty |= was_still != (g.still_timeout > 0);
    if dirty {
        t.core_mut().mark_data_dirty();
    }
}

/// `scanPlayerAboveGhast`: a player (not riding a happy ghast) stands on its top.
fn player_above(t: &Tick<'_, '_>) -> bool {
    if t.mob_ref().baby {
        return false;
    }
    let bb = t.aabb();
    let detect = Aabb::new(
        bb.min_x - 1.0,
        bb.max_y - 1.0e-5,
        bb.min_z - 1.0,
        bb.max_x + 1.0,
        bb.max_y + (bb.max_y - bb.min_y) / 2.0,
        bb.max_z + 1.0,
    );
    let mut found = false;
    t.ents.store.for_each_in(&detect, true, |id| {
        if found {
            return;
        }
        let Some(p) = t.ents.player(id) else {
            return;
        };
        if p.game_mode == vanillite_config::GameMode::Spectator {
            return;
        }
        // `getRootVehicle`.
        let mut root = id;
        while let Some(v) = t.ents.vehicle_of(root) {
            root = v;
        }
        if t.ents
            .mob(root)
            .is_some_and(|m| m.kind == crate::mob::Kind::HappyGhast)
        {
            return;
        }
        if let Some(e) = t.ents.store.get(root)
            && detect.contains(e.pos().x, e.pos().y, e.pos().z)
        {
            found = true;
        }
    });
    found
}

/// `HappyGhast.aiStep`: ghastlings never drown; then `continuousHeal`.
pub fn ai_step(t: &mut Tick<'_, '_>) {
    if t.mob_ref().baby {
        // `canBreatheUnderwater` of ghastlings.
        t.mob().air = 300;
    }
    let m = t.mob_ref();
    if m.is_alive() && m.death_time == 0 && m.health < m.max_health() {
        let p = t.pos();
        let raining = (t.env.raining_at)(BlockPos::containing(p.x, p.y, p.z));
        // `isInClouds`: at cloud height (192 to 196 in the overworld).
        let in_clouds = (192.0..=196.0).contains(&p.y);
        let interval = if in_clouds || raining { 20 } else { 600 };
        if t.mob_ref().tick_count % interval == 0 {
            let m = t.mob();
            m.health = (m.health + 1.0).min(m.max_health());
            t.core_mut().mark_data_dirty();
        }
    }
}

/// `customServerAiStep`: the ghastling brain, then `checkRestriction`.
pub fn custom_server_ai_step(t: &mut Tick<'_, '_>) {
    if t.mob_ref().baby {
        super::ai::tick(t);
    }
    let leashed = t.ents.leashes.contains_key(&t.id);
    let ridden = !t.ents.riding.passengers_of(t.id).is_empty();
    if !leashed && !ridden {
        let m = t.mob_ref();
        let radius = if !m.baby && !harnessed(m) { 64 } else { 32 };
        let here = t.block_pos();
        let m = t.mob_ref();
        let far = {
            let c = m.restrict_center;
            let r = f64::from(radius + 16);
            f64::from(c.x - here.x).powi(2)
                + f64::from(c.y - here.y).powi(2)
                + f64::from(c.z - here.z).powi(2)
                >= r * r
        };
        if !m.has_restriction() || far || m.restrict_radius != radius {
            let m = t.mob();
            m.restrict_center = here;
            m.restrict_radius = radius;
        }
    }
    // Growing up switches the controls and goals (`ageBoundaryReached`).
    let baby = t.mob_ref().baby;
    let adult_setup = t.mob_ref().move_ctl.kind == MoveKind::Custom;
    if baby == adult_setup {
        let m = t.mob();
        setup_for_age(m);
        register_goals(m);
    }
}

/// `GhastMoveControl(this, careful = true, isOnStillTimeout)`.
pub fn move_control(t: &mut Tick<'_, '_>) -> bool {
    if on_still_timeout(t.mob_ref()) {
        t.mob().move_ctl.operation = Operation::Wait;
        // `stopInPlace`.
        t.mob().nav.stop();
        t.set_velocity(Vec3::ZERO);
    }
    if t.mob_ref().move_ctl.operation != Operation::MoveTo {
        return true;
    }
    let duration = ghast(t.mob_ref()).float_duration;
    ghast_mut(t.mob()).float_duration = duration - 1;
    if duration > 0 {
        return true;
    }
    let add = t.mob().random.next_int_bounded(5) + 2;
    ghast_mut(t.mob()).float_duration += add;
    let wanted = t.mob_ref().move_ctl.wanted;
    let p = t.pos();
    let travel = Vec3::new(wanted.x - p.x, wanted.y - p.y, wanted.z - p.z);
    if can_reach(t, travel) {
        // `FLYING_SPEED` 0.05 × 5 / 3.
        let v = t.velocity() + travel.normalize() * (0.05 * 5.0 / 3.0);
        t.set_velocity(v);
    } else {
        t.mob().move_ctl.operation = Operation::Wait;
    }
    true
}

/// `GhastMoveControl.canReach` (careful): no avoided, colliding or fluid blocks around the
/// destination and along the way. The way is checked block by block over the swept box.
fn can_reach(t: &Tick<'_, '_>, travel: Vec3) -> bool {
    let aabb = t.aabb();
    let dest = Aabb::new(
        aabb.min_x + travel.x,
        aabb.min_y + travel.y,
        aabb.min_z + travel.z,
        aabb.max_x + travel.x,
        aabb.max_y + travel.y,
        aabb.max_z + travel.z,
    );
    let in_water = t.mob_ref().fluids.in_water();
    let in_lava = t.mob_ref().fluids.in_lava();
    let check = |pos: BlockPos, water: bool, lava: bool| -> bool {
        let state = t.level.block(pos);
        if state.is_air() {
            return true;
        }
        if crate::mob::spawn::in_block_tag(state, "minecraft:happy_ghast_avoids") {
            return false;
        }
        if crate::mob::is_water(state) {
            return water;
        }
        if crate::mob::is_lava(state) {
            return lava;
        }
        state.collision_shape().is_empty()
    };
    let around = dest.inflate(1.0, 1.0, 1.0);
    if !for_each_block(around, |pos| check(pos, false, false)) {
        return false;
    }
    let swept = Aabb::new(
        aabb.min_x.min(dest.min_x),
        aabb.min_y.min(dest.min_y),
        aabb.min_z.min(dest.min_z),
        aabb.max_x.max(dest.max_x),
        aabb.max_y.max(dest.max_y),
        aabb.max_z.max(dest.max_z),
    );
    for_each_block(swept, |pos| {
        let block_box = Aabb::new(
            f64::from(pos.x),
            f64::from(pos.y),
            f64::from(pos.z),
            f64::from(pos.x) + 1.0,
            f64::from(pos.y) + 1.0,
            f64::from(pos.z) + 1.0,
        );
        aabb.intersects(&block_box) || check(pos, in_water, in_lava)
    })
}

/// Calls `f` for every block of a box (`BlockPos.betweenClosed`); stops at the first `false`.
fn for_each_block(b: Aabb, mut f: impl FnMut(BlockPos) -> bool) -> bool {
    let (x0, y0, z0) = (
        b.min_x.floor() as i32,
        b.min_y.floor() as i32,
        b.min_z.floor() as i32,
    );
    let (x1, y1, z1) = (
        b.max_x.floor() as i32,
        b.max_y.floor() as i32,
        b.max_z.floor() as i32,
    );
    for x in x0..=x1 {
        for y in y0..=y1 {
            for z in z0..=z1 {
                if !f(BlockPos::new(x, y, z)) {
                    return false;
                }
            }
        }
    }
    true
}

/// `HappyGhastLookControl`: square to the grid while standing still, else face the look
/// target or the movement (`Ghast.faceMovementDirection`).
pub fn look_control(t: &mut Tick<'_, '_>) -> bool {
    let mut rot = t.core().rot();
    if on_still_timeout(t.mob_ref()) {
        let close = wrap_degrees_90(rot.y_rot);
        rot.y_rot -= close;
        rot.y_head_rot = rot.y_rot;
        t.core_mut().set_rot(rot);
        return true;
    }
    if t.mob_ref().look.cooldown > 0 {
        t.mob().look.cooldown -= 1;
        let w = t.mob_ref().look.wanted;
        let p = t.pos();
        set_y_rot(t, crate::mob::direction_yaw(w.x - p.x, w.z - p.z));
        return true;
    }
    // `Ghast.faceMovementDirection`: the yaw and body, not the head.
    let p = t.pos();
    let towards = match t.mob_ref().target.and_then(|id| t.other(id)) {
        None => Some(t.velocity()),
        Some(o) if o.pos.distance_to_sqr(p) < 4096.0 => Some(o.pos - p),
        Some(_) => None,
    };
    if let Some(d) = towards {
        rot.y_rot = crate::mob::direction_yaw(d.x, d.z);
        t.core_mut().set_rot(rot);
        t.mob().y_body_rot = rot.y_rot;
    }
    true
}

/// `Mth.wrapDegrees90`.
fn wrap_degrees_90(deg: f32) -> f32 {
    let mut r = deg % 90.0;
    if r >= 45.0 {
        r -= 90.0;
    }
    if r < -45.0 {
        r += 90.0;
    }
    r
}

fn set_y_rot(t: &mut Tick<'_, '_>, y: f32) {
    let mut rot = t.core().rot();
    rot.y_rot = y;
    rot.y_head_rot = y;
    t.core_mut().set_rot(rot);
    t.mob().y_body_rot = y;
}

/// `HappyGhast.mobInteract`: adults with a harness are ridden; harnesses go on with
/// `Equippable`.
pub fn interact(ix: &mut Interact<'_, '_>) -> Interaction {
    let Some(m) = ix.ents.mob(ix.target) else {
        return Interaction::Pass;
    };
    if m.baby {
        return animal::animal_interact(ix.ents, ix.env, ix.player, ix.target, ix.hand, ix.stack);
    }
    let name = crate::item::item_id(ix.stack).name();
    if name.ends_with("_harness") && !harnessed(m) {
        if let Some(m) = ix.ents.mob_mut(ix.target) {
            m.equipment.set(EquipmentSlot::Body, ix.stack.with_count(1));
        }
        animal::use_item(ix.ents, ix.player, ix.hand);
        crate::mob::combat::mob_sound(
            ix.ents,
            ix.env,
            ix.target,
            "minecraft:entity.happy_ghast.equip",
        );
        return Interaction::Success;
    }
    if harnessed(m) && !ix.sneaking {
        let first = ix.ents.riding.passengers_of(ix.target).is_empty();
        if crate::riding::start_riding(ix.ents, ix.env, ix.player, ix.target, false) && first {
            crate::mob::combat::mob_sound(
                ix.ents,
                ix.env,
                ix.target,
                "minecraft:entity.happy_ghast.harness_goggles_down",
            );
        }
        return Interaction::Success;
    }
    animal::animal_interact(ix.ents, ix.env, ix.player, ix.target, ix.hand, ix.stack)
}

/// The synched data: `AgeableMob`, leash holder 18, stays still 19.
pub fn encode(w: &mut DataWriter<'_>, m: &MobData) {
    encode_ageable(w, m);
    let g = ghast(m);
    w.boolean(18, g.leash_holder, false);
    w.boolean(19, g.still_timeout > 0, false);
}

pub fn save(m: &MobData, out: &mut Compound) {
    save_ageable(m, out, true);
    out.insert("still_timeout", ghast(m).still_timeout);
}

pub fn load(m: &mut MobData, tag: &Compound) {
    load_ageable(m, tag, true);
    let still = super::int(tag, "still_timeout").unwrap_or(0);
    ghast_mut(m).still_timeout = still;
    setup_for_age(m);
    register_goals(m);
}

// --- Goals ------------------------------------------------------------------------------

pub fn goal_flags(g: &GhastGoal) -> u8 {
    match g {
        GhastGoal::Float => goals::flag::JUMP,
        GhastGoal::Tempt { .. } => goals::flag::MOVE | goals::flag::LOOK,
        GhastGoal::RandomFloat => goals::flag::MOVE,
    }
}

pub fn goal_every_tick(g: &GhastGoal) -> bool {
    matches!(g, GhastGoal::Float)
}

/// The items that tempt a happy ghast: `#happy_ghast_tempt_items` for adults without a
/// harness, else the food.
fn tempts(m: &MobData, item: u16) -> bool {
    let tag = if !harnessed(m) && !m.baby {
        "minecraft:happy_ghast_tempt_items"
    } else {
        "minecraft:happy_ghast_food"
    };
    crate::player::play::game_data().is_some_and(|d| d.item_in_tag(item, tag))
}

/// The nearest player within the tempt range (16) holding a tempting item.
fn tempting_player(t: &Tick<'_, '_>) -> Option<EntityId> {
    let range = 16.0;
    let p = t.pos();
    let eye = Vec3::new(p.x, t.eye_y(), p.z);
    let area = t.aabb().inflate(range, range, range);
    let mut best: Option<(EntityId, f64)> = None;
    t.ents.store.for_each_in(&area, true, |id| {
        let Some(pd) = t.ents.player(id) else {
            return;
        };
        if pd.game_mode == vanillite_config::GameMode::Spectator || pd.survival.is_dead_or_dying() {
            return;
        }
        let holds = [EquipmentSlot::MainHand, EquipmentSlot::OffHand]
            .iter()
            .any(|&s| {
                let st = pd.inventory.equipment(s);
                !st.is_empty() && tempts(t.mob_ref(), st.item)
            });
        if !holds {
            return;
        }
        let Some(e) = t.ents.store.get(id) else {
            return;
        };
        let d = e.pos().distance_to_sqr(eye);
        if d <= range * range && best.is_none_or(|(_, b)| d < b) {
            best = Some((id, d));
        }
    });
    best.map(|(id, _)| id)
}

pub fn goal_can_use(t: &mut Tick<'_, '_>, g: &mut GhastGoal) -> bool {
    match g {
        GhastGoal::Float => {
            // `FloatGoal.canUse` unless standing still.
            let f = t.mob_ref().fluids;
            !on_still_timeout(t.mob_ref()) && (f.water_height > 0.4 || f.in_lava())
        }
        GhastGoal::Tempt { player, calm_down } => {
            if *calm_down > 0 {
                *calm_down -= 1;
                return false;
            }
            *player = tempting_player(t);
            player.is_some()
        }
        GhastGoal::RandomFloat => {
            let ctl = t.mob_ref().move_ctl;
            if !ctl.has_wanted() {
                return true;
            }
            let d = ctl.wanted.distance_to_sqr(t.pos());
            !(1.0..=3600.0).contains(&d)
        }
    }
}

pub fn goal_can_continue(t: &mut Tick<'_, '_>, g: &mut GhastGoal) -> bool {
    match g {
        GhastGoal::RandomFloat => false,
        GhastGoal::Tempt { player, .. } => {
            let still = tempting_player(t);
            if still.is_some() {
                *player = still;
            }
            still.is_some()
        }
        GhastGoal::Float => goal_can_use(t, g),
    }
}

pub fn goal_start(t: &mut Tick<'_, '_>, g: &mut GhastGoal) {
    if let GhastGoal::RandomFloat = g {
        let target = suitable_fly_to_position(t, 16);
        t.mob().move_ctl.set_wanted_position(target, 1.0);
    }
}

pub fn goal_stop(t: &mut Tick<'_, '_>, g: &mut GhastGoal) {
    if let GhastGoal::Tempt { player, calm_down } = g {
        *player = None;
        t.mob().move_ctl.operation = Operation::Wait;
        *calm_down = goals::reduced(100);
    }
}

pub fn goal_tick(t: &mut Tick<'_, '_>, g: &mut GhastGoal) {
    match g {
        GhastGoal::Float => {
            if t.mob().random.next_float() < 0.8 {
                t.mob().jumping = true;
            }
        }
        GhastGoal::Tempt { player, .. } => {
            let Some(o) = player.and_then(|id| t.other(id)) else {
                return;
            };
            let eye = Vec3::new(o.pos.x, o.eye_y, o.pos.z);
            t.mob().look.set_look_at(eye, 30.0, 40.0);
            if t.pos().distance_to_sqr(o.pos) < 49.0 {
                t.mob().move_ctl.operation = Operation::Wait;
            } else {
                // `navigateTowards`: a random point on the way to the eyes.
                let p = t.pos();
                let f = t.mob().random.next_double();
                let target = p + (eye - p) * f;
                t.mob().move_ctl.set_wanted_position(target, 1.0);
            }
        }
        GhastGoal::RandomFloat => {}
    }
}

/// `RandomFloatAroundGoal.getSuitableFlyToPosition(mob, distanceToBlocks)`.
fn suitable_fly_to_position(t: &mut Tick<'_, '_>, distance_to_blocks: i32) -> Vec3 {
    let center = t.pos();
    let mut result = None;
    for _ in 0..64 {
        let r = random_position(t, center);
        let within = t
            .mob_ref()
            .is_within_restriction(BlockPos::containing(r.x, r.y, r.z));
        let candidate = (!t.mob_ref().has_restriction() || within).then_some(r);
        result = candidate;
        if let Some(r) = candidate
            && good_target(t.level, r, distance_to_blocks)
        {
            return r;
        }
    }
    let mut result = result.unwrap_or_else(|| random_position(t, center));
    let pos = BlockPos::containing(result.x, result.y, result.z);
    let height = surface(t.level, pos.x, pos.z);
    if height < pos.y && height > t.level.min_y() {
        let y = t.pos().y;
        result = Vec3::new(result.x, y - (y - result.y).abs(), result.z);
    }
    result
}

/// The `MOTION_BLOCKING` height at a column.
fn surface(level: &Level, x: i32, z: i32) -> i32 {
    let Some(c) = level
        .chunks
        .chunk(vanillite_base::ChunkPos::new(x >> 4, z >> 4))
    else {
        return level.min_y();
    };
    let Some(h) = c
        .chunk
        .heightmaps()
        .get(vanillite_chunk::HeightmapKind::MotionBlocking)
    else {
        return level.min_y();
    };
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_wrap)]
    let top = h.get((x & 15) as u32, (z & 15) as u32) as i32 + c.chunk.min_y();
    top
}

fn random_position(t: &mut Tick<'_, '_>, center: Vec3) -> Vec3 {
    let r = &mut t.mob().random;
    let x = center.x + f64::from((r.next_float() * 2.0 - 1.0) * 16.0);
    let y = center.y + f64::from((r.next_float() * 2.0 - 1.0) * 16.0);
    let z = center.z + f64::from((r.next_float() * 2.0 - 1.0) * 16.0);
    Vec3::new(x, y, z)
}

/// `isGoodTarget`: an air block with a block within `distance_to_blocks` in some direction.
fn good_target(level: &Level, target: Vec3, distance_to_blocks: i32) -> bool {
    if distance_to_blocks <= 0 {
        return true;
    }
    let pos = BlockPos::containing(target.x, target.y, target.z);
    if !level.block(pos).is_air() {
        return false;
    }
    const DIRS: [(i32, i32, i32); 6] = [
        (0, -1, 0),
        (0, 1, 0),
        (0, 0, -1),
        (0, 0, 1),
        (-1, 0, 0),
        (1, 0, 0),
    ];
    for (dx, dy, dz) in DIRS {
        for i in 1..distance_to_blocks {
            let p = BlockPos::new(pos.x + dx * i, pos.y + dy * i, pos.z + dz * i);
            if !level.block(p).is_air() {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mob::Kind;

    #[test]
    fn adult_has_goals_and_baby_a_brain() {
        let m = MobData::new(Kind::HappyGhast, 1);
        assert_eq!(m.goals.slots.len(), 3);
        assert_eq!(m.move_ctl.kind, MoveKind::Custom);
        let mut b = MobData::new(Kind::HappyGhast, 1);
        b.baby = true;
        b.age = -24000;
        setup_for_age(&mut b);
        register_goals(&mut b);
        assert!(b.goals.slots.is_empty());
    }

    #[test]
    fn still_timeout_round_trip() {
        let mut m = MobData::new(Kind::HappyGhast, 1);
        ghast_mut(&mut m).still_timeout = 7;
        let mut out = Compound::new();
        save(&m, &mut out);
        let mut n = MobData::new(Kind::HappyGhast, 2);
        load(&mut n, &out);
        assert_eq!(ghast(&n).still_timeout, 7);
        assert!(on_still_timeout(&n));
    }

    #[test]
    fn wrap_90() {
        assert!((wrap_degrees_90(100.0) - 10.0).abs() < 1e-5);
        assert!((wrap_degrees_90(50.0) + 40.0).abs() < 1e-5);
    }
}
