//! `Rabbit`: hopping movement (`RabbitMoveControl`, `RabbitJumpControl`), biome variants,
//! carrot raids and the killer bunny.

use vanillite_base::random::{LegacyRandom, RandomSource};
use vanillite_base::{BlockPos, Vec3};
use vanillite_world::entity::EntityId;

use super::{G, Goal as FamGoal, MeleeKind, SpawnContext, TemptItems, common, fam, st, st_mut};
use crate::level::Level;
use crate::mob::common::{CommonGoal, MoveToBlock, TargetFilter};
use crate::mob::fam::SpawnReason;
use crate::mob::goals::flag;
use crate::mob::kinds::Kind;
use crate::mob::nav::MoveKind;
use crate::mob::{MobData, Tick};
use crate::survival::rules::Rule;

/// `Rabbit.Variant.EVIL`, the killer bunny.
pub const EVIL: i32 = 99;

/// The rabbit goals.
#[derive(Clone, Copy, Debug)]
pub enum Goal {
    /// `RabbitPanicGoal`.
    Panic { pos: Vec3 },
    /// `RabbitAvoidEntityGoal` (not for killer bunnies).
    Avoid(CommonGoal),
    /// `RaidGardenGoal`.
    RaidGarden {
        base: MoveToBlock,
        wants_to_raid: bool,
        can_raid: bool,
    },
}

pub fn flags(g: &Goal) -> u8 {
    match g {
        Goal::Panic { .. } | Goal::Avoid(_) => flag::MOVE,
        Goal::RaidGarden { .. } => flag::MOVE | flag::JUMP,
    }
}

pub fn every_tick(g: &Goal) -> bool {
    matches!(g, Goal::RaidGarden { .. })
}

/// The stored type, clamped to the known ones.
pub fn clamp_type(v: i32) -> i32 {
    if v == EVIL || (0..=5).contains(&v) {
        v
    } else {
        0
    }
}

fn wolf(t: &Tick<'_, '_>, id: EntityId) -> bool {
    crate::mob::common::mob_of(t, id, Kind::Wolf)
}

fn player(t: &Tick<'_, '_>, id: EntityId) -> bool {
    t.ents
        .player(id)
        .is_some_and(|p| p.game_mode != vanillite_config::GameMode::Spectator)
}

/// `registerGoals` of the rabbit.
pub fn register_goals(m: &mut MobData) {
    // The constructor's `RabbitMoveControl` and `setSpeedModifier(0.0)`: the move control
    // wants (0, 0, 0).
    m.move_ctl.rabbit = Some(crate::mob::nav::RabbitMove::default());
    set_speed_modifier(m, 0.0);
    let g = &mut m.goals;
    g.add(1, G::Float);
    g.add(1, fam(FamGoal::Rabbit(Goal::Panic { pos: Vec3::ZERO })));
    g.add(2, G::breed(0.8));
    g.add(3, G::tempt(1.0, TemptItems::Tag("minecraft:rabbit_food")));
    let players: TargetFilter = player;
    let wolves: TargetFilter = wolf;
    let monsters: TargetFilter = crate::mob::common::enemy;
    g.add(
        4,
        fam(FamGoal::Rabbit(Goal::Avoid(CommonGoal::avoid(
            players, 8.0, 2.2, 2.2,
        )))),
    );
    g.add(
        4,
        fam(FamGoal::Rabbit(Goal::Avoid(CommonGoal::avoid(
            wolves, 10.0, 2.2, 2.2,
        )))),
    );
    g.add(
        4,
        fam(FamGoal::Rabbit(Goal::Avoid(CommonGoal::avoid(
            monsters, 4.0, 2.2, 2.2,
        )))),
    );
    g.add(
        5,
        fam(FamGoal::Rabbit(Goal::RaidGarden {
            base: MoveToBlock::new(f64::from(0.7_f32), 16, 1),
            wants_to_raid: false,
            can_raid: false,
        })),
    );
    g.add(6, G::stroll(0.6));
    g.add(11, G::look_at_player(10.0));
    m.move_ctl.kind = MoveKind::Custom;
    m.nav.can_float = true;
}

/// `setVariant(EVIL)`: armour, a stronger bite, hostility and the name.
pub fn apply_evil(m: &mut MobData) {
    m.attrs.armor = 8.0;
    m.attrs.attack_damage += 5.0;
    m.goals.add(4, G::melee(MeleeKind::Plain, 1.4, true));
    m.targets.add(1, G::hurt_by(true));
    let players: TargetFilter = crate::mob::common::any_player;
    let wolves: TargetFilter = wolf;
    m.targets
        .add(2, common(CommonGoal::nearest_player(true, players)));
    m.targets
        .add(2, common(CommonGoal::nearest(10, true, wolves)));
    if m.custom_name.is_none() {
        let mut v = Vec::new();
        let tag = vanillite_nbt::Tag::Compound({
            let mut c = vanillite_nbt::Compound::new();
            c.insert("translate", "entity.minecraft.killer_bunny");
            c
        });
        if let Some(enc) = crate::mob::nbt::tag_component(&tag) {
            v.extend_from_slice(&enc);
            m.custom_name = Some(v.into());
        }
    }
}

/// `getRandomRabbitVariant`: white in snowy biomes, gold in deserts, else brown, salt or
/// black.
pub fn random_variant(ctx: &SpawnContext<'_>, r: &mut LegacyRandom) -> i32 {
    let roll = r.next_int_bounded(100);
    if ctx.biome_is("minecraft:spawns_white_rabbits") {
        if roll < 80 { 1 } else { 3 }
    } else if ctx.biome_is("minecraft:spawns_gold_rabbits") {
        4
    } else if roll < 50 {
        0
    } else if roll < 90 {
        5
    } else {
        2
    }
}

/// `finalizeSpawn`: the biome variant, kits after the first of a group.
pub fn finalize_spawn(m: &mut MobData, ctx: &mut SpawnContext<'_>) {
    m.variant = random_variant(ctx, &mut m.random);
    super::ageable_group(m, ctx, true, 1.0);
}

/// `getBreedOffspring`: usually a parent's type.
pub fn breed_offspring(
    parent: &MobData,
    partner: &MobData,
    child: &mut MobData,
    r: &mut LegacyRandom,
) {
    let ctx = SpawnContext::simple(
        crate::survival::rules::Difficulty::Normal,
        SpawnReason::Breeding,
    );
    let mut v = random_variant(&ctx, r);
    if r.next_int_bounded(20) != 0 {
        v = if r.next_boolean() {
            partner.variant
        } else {
            parent.variant
        };
    }
    child.variant = v;
    if v == EVIL {
        apply_evil(child);
    }
}

/// `startJumping`.
fn start_jumping(t: &mut Tick<'_, '_>) {
    t.mob().jumping = true;
    let pitch = {
        let r = &mut t.mob().random;
        ((r.next_float() - r.next_float()) * 0.2 + 1.0) * 0.8
    };
    crate::mob::kinds::play_sound(t, "minecraft:entity.rabbit.jump", 0.6, pitch);
    let s = st_mut(t.mob());
    s.jump_duration = 15;
    s.jump_ticks = 0;
}

/// `Rabbit.getJumpPower`: the multiplier of the jump strength, 0.3 (0.2 slowly, 0.5 up a
/// step) of the 0.42 base.
pub fn jump_multiplier(t: &Tick<'_, '_>) -> f32 {
    let m = t.mob_ref();
    let y = t.pos().y;
    let mut base: f32 = 0.3;
    if m.move_ctl.speed_modifier <= 0.6 {
        base = 0.2;
    }
    if let Some(path) = &m.nav.path
        && !path.is_done()
        && path.next_entity_pos(t.core().dims().width).y > y + 0.5
    {
        base = 0.5;
    }
    if m.horizontal_collision || m.jumping && m.move_ctl.wanted.y > y + 0.5 {
        base = 0.5;
    }
    base / 0.42
}

/// `Rabbit.jumpFromGround` after the jump: a push forwards from standing, the hop event.
pub fn after_jump(t: &mut Tick<'_, '_>) {
    if t.mob_ref().move_ctl.speed_modifier > 0.0 {
        let v = t.velocity();
        if v.x * v.x + v.z * v.z < 0.01 {
            let up = if t.mob_ref().baby { 0.5 } else { 1.5 };
            crate::mob::living::move_relative(t, 0.1, Vec3::new(0.0, up, 1.0));
        }
    }
    let id = t.id;
    crate::mob::living::entity_event(t, id, 1);
}

/// `face_point`.
fn face_point(t: &mut Tick<'_, '_>, x: f64, z: f64) {
    let p = t.pos();
    #[allow(clippy::cast_possible_truncation)]
    let y = (crate::mob::atan2_deg(z - p.z, x - p.x) as f32) - 90.0;
    let mut rot = t.core().rot();
    rot.y_rot = y;
    t.core_mut().set_rot(rot);
}

/// `Rabbit.customServerAiStep`: carrot appetite, landing delay and the next hop.
pub fn custom_server_ai_step(t: &mut Tick<'_, '_>) {
    {
        let s = st_mut(t.mob());
        if s.jump_delay_ticks > 0 {
            s.jump_delay_ticks -= 1;
        }
    }
    if st(t.mob_ref()).more_carrot_ticks > 0 {
        let d = t.mob().random.next_int_bounded(3);
        let s = st_mut(t.mob());
        s.more_carrot_ticks = (s.more_carrot_ticks - d).max(0);
    }
    if t.on_ground() {
        if !st(t.mob_ref()).was_on_ground {
            t.mob().jumping = false;
            let speed = t.mob_ref().move_ctl.speed_modifier;
            st_mut(t.mob()).jump_delay_ticks = if speed < 2.2 { 10 } else { 3 };
        }
        if t.mob_ref().variant == EVIL
            && st(t.mob_ref()).jump_delay_ticks == 0
            && let Some(o) = t.mob_ref().target.and_then(|id| t.other(id))
            && o.pos.distance_to_sqr(t.pos()) < 16.0
        {
            face_point(t, o.pos.x, o.pos.z);
            let speed = t.mob_ref().move_ctl.speed_modifier;
            t.mob().move_ctl.set_wanted_position(o.pos, speed);
            start_jumping(t);
            st_mut(t.mob()).was_on_ground = true;
        }
        if !t.mob_ref().jump
            && t.mob_ref().move_ctl.has_wanted()
            && st(t.mob_ref()).jump_delay_ticks == 0
        {
            let width = t.core().dims().width;
            let pos = match &t.mob_ref().nav.path {
                Some(path) if !path.is_done() => path.next_entity_pos(width),
                _ => t.mob_ref().move_ctl.wanted,
            };
            face_point(t, pos.x, pos.z);
            start_jumping(t);
        }
    }
    st_mut(t.mob()).was_on_ground = t.on_ground();
}

/// `RabbitJumpControl.tick`: a wanted jump starts a hop.
pub fn jump_control(t: &mut Tick<'_, '_>) {
    if t.mob_ref().jump {
        start_jumping(t);
        t.mob().jump = false;
    }
}

/// `Rabbit.aiStep`: the hop timer.
pub fn ai_step(t: &mut Tick<'_, '_>) {
    let s = st_mut(t.mob());
    if s.jump_ticks != s.jump_duration {
        s.jump_ticks += 1;
    } else if s.jump_duration != 0 {
        s.jump_ticks = 0;
        s.jump_duration = 0;
        t.mob().jumping = false;
    }
}

/// `Rabbit.setSpeedModifier`: the navigation's speed, and the move control wants its last
/// position again with it.
fn set_speed_modifier(m: &mut MobData, speed: f64) {
    m.nav.speed_modifier = speed;
    let wanted = m.move_ctl.wanted;
    m.move_ctl.set_wanted_position(wanted, speed);
}

/// `RabbitMoveControl.tick`: no speed on the ground between hops.
pub fn move_control(t: &mut Tick<'_, '_>) -> bool {
    let on_ground = t.on_ground();
    let m = t.mob();
    if on_ground && !m.jumping && !m.jump {
        set_speed_modifier(m, 0.0);
    } else if m.move_ctl.has_wanted() || m.move_ctl.operation == crate::mob::nav::Operation::Jumping
    {
        let next = m.move_ctl.rabbit.map_or(0.0, |r| r.next_jump_speed);
        set_speed_modifier(m, next);
    }
    t.mob().move_ctl.kind = MoveKind::Default;
    crate::mob::nav::tick_move_control(t);
    t.mob().move_ctl.kind = MoveKind::Custom;
    true
}

/// The killer bunny's attack sound.
pub fn do_hurt_target(t: &mut Tick<'_, '_>, target: EntityId) -> Option<bool> {
    let r = crate::mob::combat::base_mob_attack(t, target);
    if r && t.mob_ref().variant == EVIL {
        let pitch = {
            let r = &mut t.mob().random;
            (r.next_float() - r.next_float()) * 0.2 + 1.0
        };
        crate::mob::kinds::play_sound(t, "minecraft:entity.rabbit.attack", 1.0, pitch);
    }
    Some(r)
}

/// `checkRabbitSpawnRules`: `#rabbits_spawnable_on` below and bright enough.
pub fn spawn_rules(level: &Level, pos: BlockPos, reason: SpawnReason) -> bool {
    super::below_is(
        level,
        pos,
        &[
            "minecraft:grass_block",
            "minecraft:snow",
            "minecraft:snow_block",
            "minecraft:sand",
        ],
    ) && super::bright_enough(level, pos, reason)
}

// --- Goals --------------------------------------------------------------------------------

/// `RaidGardenGoal.isValidTarget`: farmland under ripe carrots.
fn raid_target(t: &Tick<'_, '_>, pos: BlockPos) -> bool {
    let below = t.level.block(pos);
    if below.block().name() != "minecraft:farmland" {
        return false;
    }
    let crop = t.level.block(BlockPos::new(pos.x, pos.y + 1, pos.z));
    crop.block().name() == "minecraft:carrots" && crop.value("age") == Some("7")
}

pub fn can_use(t: &mut Tick<'_, '_>, g: &mut Goal) -> bool {
    match g {
        Goal::Panic { pos } => {
            let panics = t
                .mob_ref()
                .last_damage_source(t.game_time())
                .is_some_and(|k| {
                    t.env
                        .types
                        .is(k, crate::survival::damage::tag::PANIC_CAUSES)
                });
            if !panics {
                return false;
            }
            match crate::mob::randompos::default_pos(t, 5, 4) {
                Some(p) => {
                    *pos = p;
                    true
                }
                None => false,
            }
        }
        Goal::Avoid(cg) => t.mob_ref().variant != EVIL && crate::mob::common::can_use(t, cg),
        Goal::RaidGarden {
            base,
            wants_to_raid,
            can_raid,
        } => {
            if base.next_start_tick <= 0 {
                if !t.env.rules.bool(Rule::MOB_GRIEFING) {
                    return false;
                }
                *can_raid = false;
                *wants_to_raid = st(t.mob_ref()).more_carrot_ticks <= 0;
            }
            if !*wants_to_raid {
                // `isValidTarget` needs the wish to raid.
                if base.next_start_tick > 0 {
                    base.next_start_tick -= 1;
                    return false;
                }
                base.next_start_tick = MoveToBlock::default_next_start_tick(t);
                return false;
            }
            let ok = base.can_use(t, raid_target, MoveToBlock::default_next_start_tick);
            if ok {
                *can_raid = true;
            }
            ok
        }
    }
}

pub fn can_continue(t: &mut Tick<'_, '_>, g: &mut Goal) -> bool {
    match g {
        Goal::Panic { .. } => !t.mob_ref().nav.is_done(),
        Goal::Avoid(cg) => crate::mob::common::can_continue(t, cg),
        Goal::RaidGarden { base, can_raid, .. } => *can_raid && base.can_continue(t, raid_target),
    }
}

pub fn start(t: &mut Tick<'_, '_>, g: &mut Goal) {
    match g {
        Goal::Panic { pos } => {
            crate::mob::nav::move_to_pos(t, *pos, 2.2);
        }
        Goal::Avoid(cg) => crate::mob::common::start(t, cg),
        Goal::RaidGarden { base, .. } => base.start(t),
    }
}

pub fn stop(t: &mut Tick<'_, '_>, g: &mut Goal) {
    match g {
        Goal::Avoid(cg) => crate::mob::common::stop(t, cg),
        Goal::Panic { .. } => t.mob().nav.stop(),
        Goal::RaidGarden { .. } => {}
    }
}

pub fn tick(t: &mut Tick<'_, '_>, g: &mut Goal) {
    match g {
        Goal::Panic { .. } => set_speed_modifier(t.mob(), 2.2),
        Goal::Avoid(cg) => crate::mob::common::tick(t, cg),
        Goal::RaidGarden { base, can_raid, .. } => {
            let target = BlockPos::new(base.block_pos.x, base.block_pos.y + 1, base.block_pos.z);
            base.tick(t, target, 1.0);
            let look = Vec3::new(
                f64::from(base.block_pos.x) + 0.5,
                f64::from(base.block_pos.y) + 1.0,
                f64::from(base.block_pos.z) + 0.5,
            );
            t.mob().look.set_look_at(look, 10.0, 40.0);
            if base.reached_target {
                let crop = t.level.block(target);
                if *can_raid && crop.block().name() == "minecraft:carrots" {
                    let age: i32 = crop.value("age").and_then(|a| a.parse().ok()).unwrap_or(0);
                    if age == 0 {
                        t.cx.actions
                            .push(crate::mob::WorldAction::DestroyBlock(target, true));
                    } else if let Some(s) = crop.with("age", &(age - 1).to_string()) {
                        t.cx.actions
                            .push(crate::mob::WorldAction::SetBlock(target, s, 2));
                        t.cx.actions.push(crate::mob::WorldAction::LevelEvent(
                            2001,
                            target,
                            i32::from(crop.0),
                        ));
                    }
                    st_mut(t.mob()).more_carrot_ticks = 40;
                }
                *can_raid = false;
                base.next_start_tick = 10;
            }
        }
    }
}
