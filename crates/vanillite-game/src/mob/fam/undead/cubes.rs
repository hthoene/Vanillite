//! The cube mobs (`AbstractCubeMob`): slimes, magma cubes and sulfur cubes. They hop in a
//! direction (`CubeMobMoveControl`), hurt what they touch, and split when they die.

use vanillite_base::random::{LegacyRandom, RandomSource};
use vanillite_base::{Aabb, BlockPos, Vec3};
use vanillite_nbt::{Compound, Tag};
use vanillite_world::entity::{Dimensions, EntityId};

use super::{Goal, State, goal, sound, state, state_mut};
use crate::item::{EquipmentSlot, ItemStack};
use crate::level::Level;
use crate::mob::animal::Interaction;
use crate::mob::common::CommonGoal;
use crate::mob::equipment;
use crate::mob::fam::{Hurt, Interact, SpawnContext, SpawnReason};
use crate::mob::goals::{self, flag};
use crate::mob::nav::{self, MoveKind, NavKind, Operation};
use crate::mob::{Environment, Kind, MobData, Tick, kinds};
use crate::player::encode::DataWriter;

/// The state of a cube mob.
#[derive(Clone, Debug)]
pub struct CubeState {
    /// `ID_SIZE` (1–127).
    pub size: i32,
    pub was_on_ground: bool,
    /// `CubeMobMoveControl`: the wanted yaw, whether it attacks and the jump delay.
    pub move_yaw: f32,
    pub aggressive: bool,
    pub jump_delay: i32,
    /// `AgeableMob.AGE_LOCKED`.
    pub age_locked: bool,
    /// `SulfurCube`: from a bucket, the pickup timer, the push sound cooldown and the fuse.
    pub from_bucket: bool,
    pub pickup_timer: i32,
    pub fuse: i32,
}

impl Default for CubeState {
    fn default() -> Self {
        CubeState {
            size: 1,
            was_on_ground: false,
            move_yaw: 0.0,
            aggressive: false,
            jump_delay: 0,
            age_locked: false,
            from_bucket: false,
            pickup_timer: 0,
            fuse: -1,
        }
    }
}

fn cs_mut(m: &mut MobData) -> &mut CubeState {
    let s = state_mut(m);
    if !matches!(s, State::Cube(_)) {
        *s = State::Cube(CubeState::default());
    }
    match s {
        State::Cube(c) => c,
        _ => unreachable!("the cube state was just set"),
    }
}

fn cs(m: &MobData) -> Option<&CubeState> {
    match state(m) {
        State::Cube(c) => Some(c),
        _ => None,
    }
}

/// `getSize`.
pub fn size(m: &MobData) -> i32 {
    cs(m).map_or(1, |c| c.size)
}

fn is_tiny(m: &MobData) -> bool {
    size(m) <= 1
}

/// `setSize(size, updateHealth)`: health, speed, damage, armour and experience follow the
/// size.
pub fn set_size(m: &mut MobData, size: i32, update_health: bool) {
    let size = size.clamp(1, 127);
    cs_mut(m).size = size;
    let sulfur = m.kind == Kind::SulfurCube;
    m.attrs.max_health = if sulfur {
        f64::from(4 * size)
    } else {
        f64::from(size * size)
    };
    #[allow(clippy::cast_precision_loss)]
    {
        m.attrs.movement_speed = f64::from(0.2_f32 + 0.1_f32 * size as f32);
    }
    match m.kind {
        Kind::Slime => m.attrs.attack_damage = f64::from(size),
        Kind::MagmaCube => {
            m.attrs.attack_damage = f64::from(size);
            m.attrs.armor = f64::from(size * 3);
        }
        _ => {}
    }
    if update_health {
        m.health = m.max_health();
        if sulfur && size == 1 && !m.baby {
            m.baby = true;
            m.age = -24000;
        }
    }
}

/// `getSoundVolume`: 0.4 per size.
pub fn sound_volume(m: &MobData) -> f32 {
    #[allow(clippy::cast_precision_loss)]
    let v = 0.4 * size(m) as f32;
    v
}

// --- Properties -------------------------------------------------------------------------

pub fn ambient_sound(_m: &MobData) -> Option<&'static str> {
    None
}

pub fn hurt_sound(m: &MobData) -> &'static str {
    match (m.kind, is_tiny(m)) {
        (Kind::Slime, true) => "minecraft:entity.slime.hurt_small",
        (Kind::Slime, false) => "minecraft:entity.slime.hurt",
        (Kind::MagmaCube, true) => "minecraft:entity.magma_cube.hurt_small",
        (Kind::MagmaCube, false) => "minecraft:entity.magma_cube.hurt",
        (_, true) => "minecraft:entity.small_sulfur_cube.hurt",
        (_, false) => "minecraft:entity.sulfur_cube.hurt",
    }
}

pub fn death_sound(m: &MobData) -> &'static str {
    match (m.kind, is_tiny(m)) {
        (Kind::Slime, true) => "minecraft:entity.slime.death_small",
        (Kind::Slime, false) => "minecraft:entity.slime.death",
        (Kind::MagmaCube, true) => "minecraft:entity.magma_cube.death_small",
        (Kind::MagmaCube, false) => "minecraft:entity.magma_cube.death",
        (_, true) => "minecraft:entity.small_sulfur_cube.death",
        (_, false) => "minecraft:entity.sulfur_cube.death",
    }
}

fn squish_sound(m: &MobData) -> &'static str {
    match (m.kind, is_tiny(m)) {
        (Kind::Slime, true) => "minecraft:entity.slime.squish_small",
        (Kind::Slime, false) => "minecraft:entity.slime.squish",
        (Kind::MagmaCube, true) => "minecraft:entity.magma_cube.squish_small",
        (Kind::MagmaCube, false) => "minecraft:entity.magma_cube.squish",
        (_, true) => "minecraft:entity.small_sulfur_cube.squish",
        (_, false) if has_body_item(m) => "minecraft:entity.sulfur_cube.bounce",
        (_, false) => "minecraft:entity.sulfur_cube.squish",
    }
}

fn jump_sound(m: &MobData) -> &'static str {
    match (m.kind, is_tiny(m)) {
        (Kind::Slime, true) => "minecraft:entity.slime.jump_small",
        (Kind::Slime, false) => "minecraft:entity.slime.jump",
        (Kind::MagmaCube, _) => "minecraft:entity.magma_cube.jump",
        (_, true) => "minecraft:entity.small_sulfur_cube.jump",
        (_, false) => "minecraft:entity.sulfur_cube.jump",
    }
}

/// The experience: the size for slimes and magma cubes (`xpReward`), 1–2 for adult sulfur
/// cubes.
pub fn experience_reward(m: &mut MobData) -> i32 {
    if m.kind == Kind::SulfurCube {
        return if m.baby {
            0
        } else {
            1 + m.random.next_int_bounded(2)
        };
    }
    let s = size(m);
    super::zombies::equipment_experience(m, s)
}

/// `getDefaultDimensions`: the type's size times the cube size (babies at half).
pub fn dimensions(m: &MobData) -> Option<Dimensions> {
    let d = Dimensions::of(m.kind.type_id());
    #[allow(clippy::cast_precision_loss)]
    let scale = size(m) as f32 * if m.baby { 0.5 } else { 1.0 };
    Some(Dimensions {
        width: d.width * scale,
        height: d.height * scale,
    })
}

/// `WorldgenRandom.seedSlimeChunk(x, z, seed, 987234911).nextInt(10) == 0`.
pub fn is_slime_chunk(x: i32, z: i32, seed: i64) -> bool {
    let a = i64::from(x.wrapping_mul(x).wrapping_mul(4_987_142));
    let b = i64::from(x.wrapping_mul(5_947_611));
    let c = i64::from(z.wrapping_mul(z)).wrapping_mul(4_392_871);
    let d = i64::from(z.wrapping_mul(389_711));
    let s = seed
        .wrapping_add(a)
        .wrapping_add(b)
        .wrapping_add(c)
        .wrapping_add(d)
        ^ 987_234_911;
    LegacyRandom::new(s).next_int_bounded(10) == 0
}

pub fn check_spawn_rules(
    kind: Kind,
    level: &Level,
    env: &Environment,
    pos: BlockPos,
    random: &mut LegacyRandom,
    reason: SpawnReason,
) -> bool {
    use super::zombies::{biome_is, is_spawner, mob_rules};
    match kind {
        Kind::Slime => {
            if is_spawner(reason) {
                return mob_rules(kind, level, pos, reason);
            }
            if biome_is(level, pos, "minecraft:allows_surface_slime_spawns")
                && pos.y > 50
                && pos.y < 70
                && random.next_float() < env.surface_slime_chance
                && crate::mob::max_local_raw_brightness(level, env, pos)
                    <= random.next_int_bounded(8)
            {
                return mob_rules(kind, level, pos, reason);
            }
            let slime_chunk = is_slime_chunk(pos.x >> 4, pos.z >> 4, env.world_seed);
            if random.next_int_bounded(10) == 0 && slime_chunk && pos.y < 40 {
                return mob_rules(kind, level, pos, reason);
            }
            false
        }
        // `checkMagmaCubeSpawnRules` and `checkSulfurCubeSpawnRules`.
        _ => true,
    }
}

// --- Setup ------------------------------------------------------------------------------

/// `CubeMobMoveControl`'s constructor: its direction is `180 * yRot / PI` of the yaw the
/// `LivingEntity` constructor rolled (radians taken as degrees).
pub fn construct_move_yaw(m: &mut MobData, y_rot: f32) {
    if cs(m).is_some() {
        cs_mut(m).move_yaw = 180.0 * y_rot / std::f32::consts::PI;
    }
}

pub fn init(m: &mut MobData) {
    cs_mut(m);
    m.nav.kind = NavKind::Ground;
    m.move_ctl.kind = MoveKind::Custom;
    set_size(m, 1, true);
    if m.kind == Kind::SulfurCube {
        // A fresh sulfur cube is not a baby until it spawns as one.
        m.baby = false;
        m.age = 0;
    }
}

fn iron_golem(t: &Tick<'_, '_>, id: EntityId) -> bool {
    crate::mob::common::iron_golem(t, id)
}

/// The player target of slimes and magma cubes: within 4 blocks of height.
fn level_player(t: &Tick<'_, '_>, id: EntityId) -> bool {
    t.ents.player(id).is_some()
        && t.ents
            .store
            .get(id)
            .is_some_and(|e| (e.pos().y - t.pos().y).abs() <= 4.0)
}

pub fn register_goals(m: &mut MobData) {
    let g = &mut m.goals;
    let tg = &mut m.targets;
    g.add(1, goal(Goal::CubeFloat));
    g.add(
        4,
        goal(Goal::CubeRandomDirection {
            chosen: 0.0,
            next: 0,
        }),
    );
    g.add(5, goal(Goal::CubeKeepOnJumping));
    m.nav.can_float = true;
    if m.kind == Kind::SulfurCube {
        g.add(
            2,
            goal(Goal::SulfurTempt {
                player: None,
                calm_down: 0,
            }),
        );
        g.add(3, goal(Goal::SulfurSearchItems { item: None }));
    } else {
        g.add(2, goal(Goal::CubeAttack { grow_tired: 0 }));
        tg.add(
            1,
            goals::Goal::Common(CommonGoal::nearest(10, true, level_player)),
        );
        tg.add(
            3,
            goals::Goal::Common(CommonGoal::nearest(10, true, iron_golem)),
        );
    }
}

/// `AbstractCubeMob.finalizeSpawn`: the babies of sulfur cube groups, then the spawn size.
pub fn finalize_spawn(m: &mut MobData, ctx: &mut SpawnContext<'_>) {
    if m.kind == Kind::SulfurCube {
        // `AgeableMob.finalizeSpawn` (5 % babies after the first of a group).
        if ctx.group_index > 0 && m.random.next_float() <= 0.05 {
            m.age = -24000;
            m.baby = true;
        }
        let size = if m.baby { 1 } else { 2 };
        set_size(m, size, true);
        return;
    }
    // `setSpawnSize`: 1, 2 or 4, larger on harder difficulties.
    let mut scale = m.random.next_int_bounded(3);
    if scale < 2 && m.random.next_float() < 0.5 * ctx.special_multiplier {
        scale += 1;
    }
    set_size(m, 1 << scale, true);
}

// --- Tick -------------------------------------------------------------------------------

pub fn before_tick(t: &mut Tick<'_, '_>) {
    // The split happens when the dead cube is removed (`remove`), the tick its death time
    // reaches 20.
    let m = t.mob_ref();
    if m.is_dead_or_dying() && m.death_time == 19 && size(m) > 1 {
        split(t);
    }
}

/// `AbstractCubeMob.tick` after `LivingEntity.tick`: landing squishes.
pub fn after_tick(t: &mut Tick<'_, '_>) {
    let on_ground = t.on_ground();
    let was = cs(t.mob_ref()).is_some_and(|c| c.was_on_ground);
    if on_ground && !was {
        // The landing particles roll their positions on the server too.
        let width = t.core().dims().width * 2.0;
        let mut i = 0.0_f32;
        while i < width * 16.0 {
            let r = &mut t.mob().random;
            let _ = (r.next_float(), r.next_float());
            i += 1.0;
        }
        let volume = sound_volume(t.mob_ref());
        let r = &mut t.mob().random;
        let pitch = ((r.next_float() - r.next_float()) * 0.2 + 1.0) / 0.8;
        let s = squish_sound(t.mob_ref());
        sound(t, s, volume, pitch);
    }
    cs_mut(t.mob()).was_on_ground = on_ground;
}

pub fn ai_step(t: &mut Tick<'_, '_>) {
    let kind = t.mob_ref().kind;
    if kind == Kind::SulfurCube {
        let m = t.mob();
        if cs(m).is_some_and(|c| c.age_locked) && m.age < 0 {
            // `canAgeUp`: an age locked baby stays a baby.
            m.age -= 1;
        }
        // `ageBoundaryReached`: grown up cubes are of size 2.
        if !m.baby && size(m) == 1 && m.is_alive() {
            set_size(m, 2, true);
            let dims = kinds::mob_dimensions(t.mob_ref());
            t.core_mut().set_dims(dims);
            t.core_mut().mark_data_dirty();
        }
        return;
    }
    touch_damage(t);
}

/// `isDealsDamage` of a living cube.
fn deals_damage(m: &MobData) -> bool {
    let can_deal = if m.kind == Kind::MagmaCube {
        !m.no_ai
    } else {
        !is_tiny(m) && !m.no_ai
    };
    can_deal && m.is_alive()
}

/// `Player.touch` → `AbstractCubeMob.playerTouch` for a player the server moves itself
/// (`Survival::simulate_movement`), at the end of its `aiStep`: the cubes in its box grown by
/// (1, 0.5, 1) that deal damage hurt it. The cubes hurt the other players in their own tick
/// ([`touch_damage`]).
pub fn player_touch(
    level: &Level,
    ents: &mut crate::entities::Entities,
    env: &mut crate::survival::player::Env<'_>,
    cx: &mut crate::mob::MobContext,
    player: EntityId,
) {
    let Some(area) = ents
        .store
        .get(player)
        .map(|e| e.aabb().inflate(1.0, 0.5, 1.0))
    else {
        return;
    };
    let mut cubes = Vec::new();
    ents.store.for_each_in(&area, false, |id| {
        if ents
            .mob(id)
            .is_some_and(|m| matches!(m.kind, Kind::Slime | Kind::MagmaCube))
        {
            cubes.push(id);
        }
    });
    for cube in cubes {
        let mut t = Tick {
            level,
            ents: &mut *ents,
            env: &mut *env,
            cx: &mut *cx,
            id: cube,
        };
        if t.exists() && deals_damage(t.mob_ref()) {
            deal_damage(&mut t, player);
        }
    }
}

/// `playerTouch` and `push(IronGolem)`: players and iron golems touching a cube that can
/// deal damage get hurt (`dealDamage`).
fn touch_damage(t: &mut Tick<'_, '_>) {
    if !deals_damage(t.mob_ref()) {
        return;
    }
    let aabb = t.aabb();
    let area = aabb.inflate(1.0, 0.5, 1.0);
    let mut found = std::mem::take(&mut t.cx.ids);
    found.clear();
    t.ents.store.for_each_in(&area, false, |id| found.push(id));
    for &id in &found {
        if id == t.id {
            continue;
        }
        let Some(e) = t.ents.store.get(id) else {
            continue;
        };
        let touches = if let Some(p) = t.ents.player(id) {
            if p.survival.simulate_movement {
                // It touches the cube in its own tick ([`player_touch`]).
                continue;
            }
            // `Player.touch` uses the player's box grown by (1, 0.5, 1).
            e.aabb().inflate(1.0, 0.5, 1.0).intersects(&aabb)
        } else {
            t.ents.mob(id).is_some_and(|o| o.kind == Kind::IronGolem) && e.aabb().intersects(&aabb)
        };
        if touches {
            deal_damage(t, id);
        }
        if !t.exists() {
            break;
        }
    }
    t.cx.ids = found;
}

/// `dealDamage`: within melee range and in sight.
fn deal_damage(t: &mut Tick<'_, '_>, target: EntityId) {
    let Some(o) = t.other(target) else {
        return;
    };
    let reach = f64::from(2.04_f32).sqrt() - f64::from(0.6_f32);
    if !t.aabb().inflate(reach, 0.0, reach).intersects(&o.aabb) {
        return;
    }
    if !crate::mob::living::has_line_of_sight(t, target) {
        return;
    }
    if let Some(p) = t.ents.player(target)
        && matches!(
            p.game_mode,
            vanillite_config::GameMode::Creative | vanillite_config::GameMode::Spectator
        )
    {
        return;
    }
    #[allow(clippy::cast_possible_truncation)]
    let mut damage = t.mob_ref().attrs.attack_damage as f32;
    if t.mob_ref().kind == Kind::MagmaCube {
        damage += 2.0;
    }
    let source = crate::mob::combat::entity_source(t.ents, t.env.types.mob_attack, t.id, t.id);
    if crate::mob::combat::hurt(t.level, t.ents, t.env, t.cx, target, source, damage) && t.exists()
    {
        let r = &mut t.mob().random;
        let pitch = (r.next_float() - r.next_float()) * 0.2 + 1.0;
        sound(t, "minecraft:entity.slime.attack", 1.0, pitch);
    }
}

/// The split when a large cube dies: 2–4 cubes of half the size (sulfur cubes: 2 babies).
fn split(t: &mut Tick<'_, '_>) {
    let m = t.mob_ref();
    let size = size(m);
    let kind = m.kind;
    let width = t.core().dims().width;
    let offset = width / 2.0;
    let half = size / 2;
    let count = if kind == Kind::SulfurCube {
        if cs(m).is_some_and(|c| c.fuse >= 0) {
            0
        } else {
            2
        }
    } else {
        2 + t.mob().random.next_int_bounded(3)
    };
    let pos = t.pos();
    for i in 0..count {
        #[allow(clippy::cast_precision_loss)]
        let xd = ((i % 2) as f32 - 0.5) * offset;
        #[allow(clippy::cast_precision_loss)]
        let zd = ((i / 2) as f32 - 0.5) * offset;
        let seed = crate::mob::new_entity_seed();
        let old = t.mob_ref();
        let mut new = MobData::new(kind, seed);
        for e in old.effects.iter() {
            crate::effect::living::add_to_mob(&mut new, e.clone());
        }
        new.left_handed = old.left_handed;
        new.no_ai = old.no_ai;
        new.persistent = old.persistent;
        new.silent = old.silent;
        new.custom_name.clone_from(&old.custom_name);
        if kind == Kind::SulfurCube {
            new.baby = true;
            new.age = -24000;
        }
        set_size(&mut new, half, true);
        let y_rot = t.mob().random.next_float() * 360.0;
        new.y_body_rot = y_rot;
        let at = Vec3::new(pos.x + f64::from(xd), pos.y + 0.5, pos.z + f64::from(zd));
        let uuid = crate::mob::new_uuid();
        crate::mob::spawn(
            t.ents,
            t.level.network_ids(),
            uuid,
            at,
            super::rotation(y_rot),
            new,
        );
    }
}

pub fn custom_server_ai_step(t: &mut Tick<'_, '_>) {
    if t.mob_ref().kind == Kind::SulfurCube {
        let c = cs_mut(t.mob());
        if c.pickup_timer > 0 {
            c.pickup_timer -= 1;
        }
    }
}

/// `setSpeed`: the speed and the forward input.
fn set_speed(m: &mut MobData, speed: f32) {
    m.speed = speed;
    m.zza = speed;
}

/// `CubeMobMoveControl.tick` (sulfur cubes holding a block do not hop).
pub fn move_control(t: &mut Tick<'_, '_>) -> bool {
    if t.mob_ref().kind == Kind::SulfurCube && has_body_item(t.mob_ref()) {
        return true;
    }
    let yaw = cs(t.mob_ref()).map_or(0.0, |c| c.move_yaw);
    let mut rot = t.core().rot();
    rot.y_rot = nav::rotlerp(rot.y_rot, yaw, 90.0);
    rot.y_head_rot = rot.y_rot;
    t.core_mut().set_rot(rot);
    t.mob().y_body_rot = rot.y_rot;
    let ctl = t.mob_ref().move_ctl;
    if ctl.operation != Operation::MoveTo {
        t.mob().zza = 0.0;
        return true;
    }
    t.mob().move_ctl.operation = Operation::Wait;
    #[allow(clippy::cast_possible_truncation)]
    let speed = (ctl.speed_modifier * t.mob_ref().attrs.movement_speed) as f32;
    if !t.on_ground() {
        set_speed(t.mob(), speed);
        return true;
    }
    set_speed(t.mob(), speed);
    let delay = cs(t.mob_ref()).map_or(0, |c| c.jump_delay);
    cs_mut(t.mob()).jump_delay = delay - 1;
    if delay <= 0 {
        let mut next = t.mob().random.next_int_bounded(20) + 10;
        if t.mob_ref().kind == Kind::MagmaCube {
            next *= 4;
        }
        if cs(t.mob_ref()).is_some_and(|c| c.aggressive) {
            next /= 3;
        }
        cs_mut(t.mob()).jump_delay = next;
        t.mob().jump = true;
        if size(t.mob_ref()) > 0 {
            let volume = sound_volume(t.mob_ref());
            let tiny = is_tiny(t.mob_ref());
            let r = &mut t.mob().random;
            let pitch =
                ((r.next_float() - r.next_float()) * 0.2 + 1.0) * if tiny { 1.4 } else { 0.8 };
            let s = jump_sound(t.mob_ref());
            sound(t, s, volume, pitch);
        }
    } else {
        let m = t.mob();
        m.xxa = 0.0;
        m.zza = 0.0;
        set_speed(m, 0.0);
    }
    true
}

pub fn travel(_t: &mut Tick<'_, '_>, _input: Vec3) -> bool {
    false
}

/// `MagmaCube.jumpInLiquid` in lava: a jump by the size.
pub fn jump_in_liquid(t: &mut Tick<'_, '_>) -> bool {
    if t.mob_ref().kind != Kind::MagmaCube || !t.mob_ref().fluids.in_lava() {
        return false;
    }
    #[allow(clippy::cast_precision_loss)]
    let y = 0.22_f32 + size(t.mob_ref()) as f32 * 0.05;
    let v = t.velocity();
    t.set_velocity(Vec3::new(v.x, f64::from(y), v.z));
    true
}

/// `setDirection(yRot, aggressive)`.
fn set_direction(m: &mut MobData, yaw: f32, aggressive: bool) {
    let c = cs_mut(m);
    c.move_yaw = yaw;
    c.aggressive = aggressive;
}

/// `setWantedMovement(speed)`.
fn set_wanted_movement(m: &mut MobData, speed: f64) {
    m.move_ctl.speed_modifier = speed;
    m.move_ctl.operation = Operation::MoveTo;
}

// --- Combat ---------------------------------------------------------------------------------

pub fn do_hurt_target(_t: &mut Tick<'_, '_>, _target: EntityId) -> Option<bool> {
    None
}

pub fn hurt_filter(_h: &mut Hurt<'_, '_>, amount: f32) -> Option<f32> {
    Some(amount)
}

pub fn after_hurt(_h: &mut Hurt<'_, '_>, _amount: f32) {}

pub fn on_die(_h: &mut Hurt<'_, '_>) {}

// --- Sulfur cubes -------------------------------------------------------------------------

fn has_body_item(m: &MobData) -> bool {
    !m.equipment.get(EquipmentSlot::Body).is_empty()
}

/// `requiresCustomPersistence`: a block inside or from a bucket.
pub fn sulfur_persistent(m: &MobData) -> bool {
    has_body_item(m) || cs(m).is_some_and(|c| c.from_bucket)
}

fn in_tag(item: u16, tag: &str) -> bool {
    crate::player::play::game_data().is_some_and(|d| d.item_in_tag(item, tag))
}

/// `#sulfur_cube_food` (what babies eat).
pub fn sulfur_is_food(item: u16) -> bool {
    in_tag(item, "minecraft:sulfur_cube_food")
}

/// `isSwallowableItem`.
fn swallowable(s: &ItemStack) -> bool {
    !s.is_empty() && in_tag(s.item, "minecraft:sulfur_cube_swallowable")
}

/// `SulfurCube.mobInteract`: feeding babies, shearing out the block, swallowing a held
/// block, bucketing.
pub fn interact(ix: &mut Interact<'_, '_>) -> Interaction {
    let Some(m) = ix.ents.mob(ix.target) else {
        return Interaction::Pass;
    };
    if m.kind != Kind::SulfurCube {
        return Interaction::Pass;
    }
    let name = crate::item::item_id(ix.stack).name();
    if m.baby {
        let locked = cs(m).is_some_and(|c| c.age_locked);
        if sulfur_is_food(ix.stack.item) && !locked {
            crate::mob::animal::use_item(ix.ents, ix.player, ix.hand);
            if let Some(m) = ix.ents.mob_mut(ix.target) {
                // `ageUp(getSpeedUpSecondsWhenFeeding(-age), true)`.
                let secs = (-m.age / 20) / 10;
                m.age = (m.age + secs * 20).min(0);
                m.forced_age_timer = 40;
            }
            let pos = ix.ents.store.get(ix.target).map_or(Vec3::ZERO, |e| e.pos());
            let seed = crate::survival::sound_seed();
            crate::survival::play_sound(
                ix.env.out,
                None,
                pos,
                "minecraft:entity.small_sulfur_cube.eat",
                vanillite_protocol::packets::play::clientbound::SoundSource::Neutral,
                1.0,
                1.0,
                seed,
            );
            return Interaction::Success;
        }
        return Interaction::Pass;
    }
    if cs(m).is_some_and(|c| c.fuse >= 0) {
        return Interaction::Pass;
    }
    let pos = ix.ents.store.get(ix.target).map_or(Vec3::ZERO, |e| e.pos());
    let height = ix
        .ents
        .store
        .get(ix.target)
        .map_or(0.0, |e| f64::from(e.dims().height));
    if name == "minecraft:shears" && has_body_item(m) {
        // `shear`: the block pops out at the passenger point.
        let Some(m) = ix.ents.mob_mut(ix.target) else {
            return Interaction::Pass;
        };
        let block =
            std::mem::take(&mut m.equipment.items[usize::from(EquipmentSlot::Body.ordinal())]);
        cs_mut(m).pickup_timer = 100;
        let sheared = crate::advancements::trigger::TriggerKind::PLAYER_SHEARED_EQUIPMENT;
        let award =
            crate::advancements::view::listens(ix.ents, ix.player, sheared).then(|| block.clone());
        super::drop_item(ix.env, pos + Vec3::new(0.0, height, 0.0), block);
        play(ix, pos, "minecraft:entity.sulfur_cube.eject");
        crate::mob::animal::hurt_held(ix.ents, ix.env, ix.player, ix.hand, 1);
        if let Some(stack) = award {
            let target = ix.target;
            crate::advancements::view::fire(ix.ents, ix.player, sheared, |ents| {
                crate::advancements::trigger::Event::EntityItem(
                    sheared,
                    crate::advancements::view::snapshot(ents, target),
                    stack,
                )
            });
        }
        if let Some(mut e) = ix.ents.store.get_mut(ix.target) {
            e.mark_data_dirty();
        }
        return Interaction::Success;
    }
    if swallowable(ix.stack) {
        // `equipItem`.
        if m.equipment.get(EquipmentSlot::Body).item == ix.stack.item && has_body_item(m) {
            return Interaction::Pass;
        }
        let old = m.equipment.get(EquipmentSlot::Body).clone();
        super::drop_item(ix.env, pos + Vec3::new(0.0, height, 0.0), old);
        if let Some(m) = ix.ents.mob_mut(ix.target) {
            m.equipment.set(EquipmentSlot::Body, ix.stack.with_count(1));
            m.equipment.set_guaranteed_drop(EquipmentSlot::Body);
        }
        crate::mob::animal::use_item(ix.ents, ix.player, ix.hand);
        play(ix, pos, "minecraft:entity.sulfur_cube.absorb");
        return Interaction::Success;
    }
    if name == "minecraft:bucket" {
        return bucket(ix, pos);
    }
    Interaction::Pass
}

fn play(ix: &mut Interact<'_, '_>, pos: Vec3, s: &str) {
    let seed = crate::survival::sound_seed();
    crate::survival::play_sound(
        ix.env.out,
        None,
        pos,
        s,
        vanillite_protocol::packets::play::clientbound::SoundSource::Neutral,
        1.0,
        1.0,
        seed,
    );
}

/// `Bucketable.bucketMobPickup`: the cube goes into a sulfur cube bucket.
fn bucket(ix: &mut Interact<'_, '_>, pos: Vec3) -> Interaction {
    let Some(m) = ix.ents.mob(ix.target) else {
        return Interaction::Pass;
    };
    if !m.is_alive() {
        return Interaction::Pass;
    }
    let Some(item) = vanillite_data::registry::ITEM.id("minecraft:sulfur_cube_bucket") else {
        return Interaction::Pass;
    };
    let mut data = Compound::new();
    if let Some(name) = m.custom_name.as_deref()
        && let Some(tag) = crate::mob::nbt::component_tag(name)
    {
        data.insert("CustomName", tag);
    }
    if m.no_ai {
        data.insert("NoAI", true);
    }
    if m.silent {
        data.insert("Silent", true);
    }
    data.insert("Health", m.health);
    data.insert("age", m.age);
    data.insert("age_locked", cs(m).is_some_and(|c| c.age_locked));
    let mut stack = ItemStack::new(item, 1);
    if let Some(id) =
        vanillite_data::registry::DATA_COMPONENT_TYPE.id("minecraft:bucket_entity_data")
        && let Some(bytes) = crate::mob::nbt::tag_component(&Tag::Compound(data))
    {
        stack.patch_mut().set(id, bytes);
    }
    play(ix, pos, "minecraft:item.bucket.fill_sulfur_cube");
    crate::mob::animal::filled_bucket(ix.ents, ix.player, &stack);
    crate::mob::animal::fill_held(ix.ents, ix.env, ix.player, ix.hand, stack);
    if let Some(m) = ix.ents.mob_mut(ix.target) {
        m.removed = true;
    }
    ix.cx.discarded.push(ix.target);
    Interaction::Success
}

/// Sulfur cubes swallow blocks lying around (`pickUpItem`); the others pick nothing up.
pub fn pick_up_items(t: &mut Tick<'_, '_>) {
    let m = t.mob_ref();
    if m.kind != Kind::SulfurCube || has_body_item(m) || m.baby || !m.is_alive() {
        return;
    }
    if cs(m).is_some_and(|c| c.pickup_timer > 0) {
        return;
    }
    if !t.env.rules.bool(crate::survival::rules::Rule::MOB_GRIEFING) {
        return;
    }
    let area = t.aabb().inflate(1.0, 0.0, 1.0);
    let mut found = std::mem::take(&mut t.cx.ids);
    found.clear();
    t.ents.store.for_each_in(&area, false, |id| found.push(id));
    for &id in &found {
        let Some(d) = t.ents.item(id) else {
            continue;
        };
        if d.pickup_delay > 0 || !swallowable(&d.stack) || t.cx.discarded.contains(&id) {
            continue;
        }
        let one = d.stack.with_count(1);
        let m = t.mob();
        m.equipment.set(EquipmentSlot::Body, one);
        m.equipment.set_guaranteed_drop(EquipmentSlot::Body);
        sound(t, "minecraft:entity.sulfur_cube.absorb", 1.0, 1.0);
        equipment::take(t, id, 1);
        equipment::broadcast(t);
        break;
    }
    t.cx.ids = found;
}

// --- Client data and storage ------------------------------------------------------------

pub fn encode_data(w: &mut DataWriter<'_>, m: &MobData) {
    let c = cs(m).cloned().unwrap_or_default();
    w.boolean(16, m.baby, false);
    w.boolean(17, c.age_locked, false);
    w.int(18, c.size, 1);
    if m.kind == Kind::SulfurCube {
        w.int(19, c.fuse, -1);
        w.boolean(20, c.from_bucket, false);
    }
}

pub fn save(m: &MobData, out: &mut Compound) {
    let c = cs(m).cloned().unwrap_or_default();
    out.insert("Size", c.size - 1);
    out.insert("wasOnGround", c.was_on_ground);
    if m.kind == Kind::SulfurCube {
        out.insert("Age", m.age);
        out.insert("ForcedAge", m.forced_age);
        out.insert("AgeLocked", c.age_locked);
        out.insert("pickup_timer", c.pickup_timer);
        out.insert("from_bucket", c.from_bucket);
        out.insert("fuse", c.fuse);
    }
}

pub fn load(m: &mut MobData, tag: &Compound) {
    let int = |k: &str| match tag.get(k) {
        Some(Tag::Int(v)) => Some(*v),
        _ => None,
    };
    let boolean = |k: &str| matches!(tag.get(k), Some(Tag::Byte(b)) if *b != 0);
    let health = m.health;
    set_size(m, int("Size").unwrap_or(0) + 1, false);
    m.health = health.min(m.max_health());
    cs_mut(m).was_on_ground = boolean("wasOnGround");
    if m.kind == Kind::SulfurCube {
        m.age = int("Age").unwrap_or(0);
        m.baby = m.age < 0;
        m.forced_age = int("ForcedAge").unwrap_or(0);
        let c = cs_mut(m);
        c.age_locked = boolean("AgeLocked");
        c.pickup_timer = int("pickup_timer").unwrap_or(0);
        c.from_bucket = boolean("from_bucket");
        c.fuse = int("fuse").unwrap_or(-1);
    }
}

// --- Goals --------------------------------------------------------------------------------

pub fn goal_flags(g: &Goal) -> u8 {
    match g {
        Goal::CubeFloat | Goal::CubeKeepOnJumping => flag::JUMP | flag::MOVE,
        Goal::CubeAttack { .. }
        | Goal::CubeRandomDirection { .. }
        | Goal::SulfurTempt { .. }
        | Goal::SulfurSearchItems { .. } => flag::LOOK,
        _ => 0,
    }
}

pub fn goal_every_tick(g: &Goal) -> bool {
    matches!(g, Goal::CubeFloat | Goal::CubeAttack { .. })
}

/// `TemptGoal.canUse` of sulfur cubes (tempt range 8).
fn tempting_player(t: &mut Tick<'_, '_>) -> Option<EntityId> {
    let range = 8.0;
    let baby = t.mob_ref().baby;
    let p = t.pos();
    let at = Vec3::new(p.x, t.eye_y(), p.z);
    let area = Aabb::new(
        p.x - range,
        p.y - range,
        p.z - range,
        p.x + range,
        p.y + range,
        p.z + range,
    );
    let mut found = std::mem::take(&mut t.cx.ids);
    found.clear();
    t.ents.store.for_each_in(&area, true, |id| found.push(id));
    let mut best: Option<(EntityId, f64)> = None;
    for &id in &found {
        let Some(pd) = t.ents.player(id) else {
            continue;
        };
        if pd.survival.is_dead_or_dying() || pd.game_mode == vanillite_config::GameMode::Spectator {
            continue;
        }
        let holds = [EquipmentSlot::MainHand, EquipmentSlot::OffHand]
            .iter()
            .any(|&s| {
                let stack = pd.inventory.equipment(s);
                if baby {
                    !stack.is_empty() && sulfur_is_food(stack.item)
                } else {
                    swallowable(stack)
                }
            });
        let Some(e) = t.ents.store.get(id) else {
            continue;
        };
        let d = e.pos().distance_to_sqr(at);
        if holds && d <= range * range && best.is_none_or(|(_, b)| d < b) {
            best = Some((id, d));
        }
    }
    t.cx.ids = found;
    best.map(|(id, _)| id)
}

pub fn goal_can_use(t: &mut Tick<'_, '_>, g: &mut Goal) -> bool {
    match g {
        Goal::CubeFloat => {
            let f = t.mob_ref().fluids;
            f.in_water() || f.in_lava()
        }
        Goal::CubeAttack { .. } => t
            .mob_ref()
            .target
            .is_some_and(|id| goals::can_attack(t, id)),
        Goal::CubeRandomDirection { .. } => {
            let m = t.mob_ref();
            m.target.is_none()
                && (t.on_ground()
                    || m.fluids.in_water()
                    || m.fluids.in_lava()
                    || m.effects.has(crate::effect::MobEffect::LEVITATION))
        }
        Goal::CubeKeepOnJumping => t.ents.vehicle_of(t.id).is_none(),
        Goal::SulfurTempt { player, calm_down } => {
            if *calm_down > 0 {
                *calm_down -= 1;
                return false;
            }
            *player = tempting_player(t);
            player.is_some()
        }
        Goal::SulfurSearchItems { item } => {
            let m = t.mob_ref();
            if m.baby || cs(m).is_some_and(|c| c.pickup_timer > 0) {
                return false;
            }
            let area = t.aabb().inflate(8.0, 8.0, 8.0);
            let p = t.pos();
            let mut found = std::mem::take(&mut t.cx.ids);
            found.clear();
            t.ents.store.for_each_in(&area, false, |id| found.push(id));
            let mut best: Option<(EntityId, f64)> = None;
            for &id in &found {
                let (Some(d), Some(e)) = (t.ents.item(id), t.ents.store.get(id)) else {
                    continue;
                };
                if d.pickup_delay > 0 || !swallowable(&d.stack) {
                    continue;
                }
                let dist = e.pos().distance_to_sqr(p);
                if best.is_none_or(|(_, b)| dist < b) {
                    best = Some((id, dist));
                }
            }
            t.cx.ids = found;
            *item = best.map(|(id, _)| id);
            item.is_some()
        }
        _ => false,
    }
}

pub fn goal_can_continue(t: &mut Tick<'_, '_>, g: &mut Goal) -> bool {
    match g {
        Goal::CubeAttack { grow_tired } => {
            let ok = t
                .mob_ref()
                .target
                .is_some_and(|id| goals::can_attack(t, id));
            if !ok {
                return false;
            }
            *grow_tired -= 1;
            *grow_tired > 0
        }
        Goal::SulfurTempt { player, .. } => {
            *player = tempting_player(t);
            player.is_some()
        }
        _ => goal_can_use(t, g),
    }
}

pub fn goal_start(_t: &mut Tick<'_, '_>, g: &mut Goal) {
    if let Goal::CubeAttack { grow_tired } = g {
        *grow_tired = goals::reduced(300);
    }
}

pub fn goal_stop(t: &mut Tick<'_, '_>, g: &mut Goal) {
    if let Goal::SulfurTempt { player, calm_down } = g {
        *player = None;
        set_wanted_movement(t.mob(), 0.0);
        *calm_down = goals::reduced(100);
    }
}

/// `Mob.lookAt(entity, 10, 10)` towards a point.
fn look_at(t: &mut Tick<'_, '_>, id: EntityId) {
    if let Some(o) = t.other(id) {
        kinds::look_at_entity(t, o, 10.0, 10.0);
    } else if let Some(e) = t.ents.store.get(id) {
        let o = crate::mob::Other {
            pos: e.pos(),
            eye_y: e.pos().y + f64::from(e.dims().height) / 2.0,
            aabb: e.aabb(),
            height: e.dims().height,
            is_player: false,
        };
        kinds::look_at_entity(t, o, 10.0, 10.0);
    }
}

pub fn goal_tick(t: &mut Tick<'_, '_>, g: &mut Goal) {
    match g {
        Goal::CubeFloat => {
            if t.mob().random.next_float() < 0.8 {
                t.mob().jump = true;
            }
            set_wanted_movement(t.mob(), 1.2);
        }
        Goal::CubeAttack { .. } => {
            if let Some(target) = t.mob_ref().target {
                look_at(t, target);
            }
            let yaw = t.core().rot().y_rot;
            let deal = {
                let m = t.mob_ref();
                if m.kind == Kind::MagmaCube {
                    !m.no_ai
                } else {
                    !is_tiny(m) && !m.no_ai
                }
            };
            set_direction(t.mob(), yaw, deal);
        }
        Goal::CubeRandomDirection { chosen, next } => {
            *next -= 1;
            if *next <= 0 {
                let r = &mut t.mob().random;
                *next = goals::reduced(40 + r.next_int_bounded(60));
                #[allow(clippy::cast_precision_loss)]
                {
                    *chosen = r.next_int_bounded(360) as f32;
                }
            }
            let yaw = *chosen;
            set_direction(t.mob(), yaw, false);
        }
        Goal::CubeKeepOnJumping => set_wanted_movement(t.mob(), 1.0),
        Goal::SulfurTempt { player, .. } => {
            let Some(p) = *player else {
                return;
            };
            let Some(o) = t.other(p) else {
                return;
            };
            if t.pos().distance_to_sqr(o.pos) < 1.0 {
                set_wanted_movement(t.mob(), 0.0);
            } else {
                look_at(t, p);
                let yaw = t.core().rot().y_rot;
                set_direction(t.mob(), yaw, true);
            }
        }
        Goal::SulfurSearchItems { item: Some(id) } => {
            let id = *id;
            look_at(t, id);
            let yaw = t.core().rot().y_rot;
            set_direction(t.mob(), yaw, true);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::survival::rules::Difficulty;

    #[test]
    fn sizes_set_health_and_dimensions() {
        let mut m = MobData::new(Kind::Slime, 1);
        set_size(&mut m, 4, true);
        assert_eq!(m.health, 16.0);
        assert_eq!(m.attrs.attack_damage, 4.0);
        let d = dimensions(&m).expect("cube dimensions");
        assert!((d.width - 0.52 * 4.0).abs() < 1e-6);
        let mut magma = MobData::new(Kind::MagmaCube, 1);
        set_size(&mut magma, 2, true);
        assert_eq!(magma.attrs.armor, 6.0);
    }

    #[test]
    fn spawn_sizes_are_powers_of_two() {
        for seed in 0..32 {
            let mut m = MobData::new(Kind::Slime, seed);
            let mut ctx = SpawnContext::simple(Difficulty::Hard, SpawnReason::Command);
            kinds::finalize_spawn(
                &mut m,
                &mut ctx,
                &mut vanillite_base::random::LegacyRandom::new(1),
            );
            assert!(matches!(size(&m), 1 | 2 | 4), "{}", size(&m));
        }
    }

    #[test]
    fn size_round_trip() {
        let mut m = MobData::new(Kind::MagmaCube, 1);
        set_size(&mut m, 4, true);
        let mut out = Compound::new();
        save(&m, &mut out);
        let mut n = MobData::new(Kind::MagmaCube, 2);
        load(&mut n, &out);
        assert_eq!(size(&n), 4);
    }

    #[test]
    fn slime_chunks_are_one_in_ten() {
        let n = (0..100)
            .flat_map(|x| (0..100).map(move |z| (x, z)))
            .filter(|&(x, z)| is_slime_chunk(x, z, 12345))
            .count();
        assert!((800..1200).contains(&n), "{n}");
    }

    #[test]
    fn sulfur_cubes_are_size_two_adults() {
        let mut m = MobData::new(Kind::SulfurCube, 1);
        let mut ctx = SpawnContext::simple(Difficulty::Normal, SpawnReason::Command);
        kinds::finalize_spawn(
            &mut m,
            &mut ctx,
            &mut vanillite_base::random::LegacyRandom::new(1),
        );
        assert_eq!(size(&m), 2);
        assert!(!m.baby);
        assert_eq!(m.health, 8.0);
    }
}
