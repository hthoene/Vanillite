//! Shulkers: attached to a block face, peeking, shooting bullets, teleporting.

use vanillite_base::random::RandomSource;
use vanillite_base::{Aabb, BlockPos, Vec3};
use vanillite_world::entity::{EntityId, Rotation};

use super::super::super::common::{self, CommonGoal};
use super::super::super::goals::{self, Goal as MobGoal};
use super::super::super::kinds::Kind;
use super::super::super::missile::{self, MissileData, MissileKind};
use super::super::super::{MobData, Tick, WorldAction, combat, physics};
use super::super::Hurt;
use super::raider::{self, Ignore};
use super::{Gate, Goal, dirty, fam, look_at_player, st, st_mut};
use crate::entities::Entities;
use crate::level::Level;
use crate::survival::rules::Difficulty;

/// The unit step of a `Direction` 3D id (down, up, north, south, west, east).
pub fn step(face: u8) -> (i32, i32, i32) {
    match face {
        0 => (0, -1, 0),
        1 => (0, 1, 0),
        2 => (0, 0, -1),
        3 => (0, 0, 1),
        4 => (-1, 0, 0),
        _ => (1, 0, 0),
    }
}

/// The opposite face.
pub fn opposite(face: u8) -> u8 {
    face ^ 1
}

/// The axis of a face: 0 y, 1 z, 2 x.
pub fn axis(face: u8) -> u8 {
    face / 2
}

fn relative(p: BlockPos, face: u8) -> BlockPos {
    let (x, y, z) = step(face);
    BlockPos::new(p.x + x, p.y + y, p.z + z)
}

/// `Shulker.registerGoals`.
pub fn register_goals(m: &mut MobData) {
    let g = &mut m.goals;
    let tg = &mut m.targets;
    g.add(1, look_at_player(8.0, 0.02));
    g.add(4, fam(Goal::ShulkerAttack { attack_time: 0 }));
    g.add(7, fam(Goal::ShulkerPeek { peek_time: 0 }));
    g.add(8, MobGoal::random_look());
    tg.add(1, raider::hurt_by(Ignore::Kind(Kind::Shulker), true));
    tg.add(
        2,
        fam(Goal::Target {
            inner: CommonGoal::nearest_player(true, common::any_player),
            gate: Gate::NotPeaceful,
        }),
    );
    tg.add(
        3,
        fam(Goal::Target {
            inner: CommonGoal::nearest(10, true, defense_target),
            gate: Gate::InTeam,
        }),
    );
}

/// The scoreboard team of an entity that is no player (`getTeam`).
pub(super) fn team_of(t: &Tick<'_, '_>, id: EntityId) -> Option<Box<str>> {
    let uuid = t.ents.store.get(id)?.uuid();
    (t.env.entity_team)(uuid)
}

/// `ShulkerDefenseAttackGoal`'s targets: an `Enemy` that is not in the shulker's team
/// (`isAlliedTo` of `TargetingConditions`).
fn defense_target(t: &Tick<'_, '_>, id: EntityId) -> bool {
    common::enemy(t, id) && team_of(t, id) != team_of(t, t.id)
}

/// `getTargetSearchArea` of the shulker's target goals: the follow distance across the face
/// it sits on, 4 blocks along its axis.
pub fn target_search_area(t: &Tick<'_, '_>, follow: f64) -> Aabb {
    match axis(st(t.mob_ref()).attach_face) {
        2 => t.aabb().inflate(4.0, follow, follow),
        1 => t.aabb().inflate(follow, follow, 4.0),
        _ => t.aabb().inflate(follow, 4.0, follow),
    }
}

/// `getPhysicalPeek`.
fn physical_peek(amount: f32) -> f32 {
    0.5 - vanillite_base::math::sin(f64::from((0.5 + amount) * std::f32::consts::PI)) * 0.5
}

/// `getProgressAabb(1, face.opposite, progress, bottom center of pos)`: the box of a shulker
/// attached to `face`, its lid opened to `progress`.
pub fn progress_aabb(face: u8, progress: f32, at: Vec3) -> Aabb {
    let dir = opposite(face);
    let (sx, sy, sz) = step(dir);
    let p = f64::from(progress);
    let mut b = Aabb::new(
        at.x - 0.5,
        at.y,
        at.z - 0.5,
        at.x + 0.5,
        at.y + 1.0,
        at.z + 0.5,
    );
    // `expandTowards(dir * progress)`.
    let (dx, dy, dz) = (f64::from(sx) * p, f64::from(sy) * p, f64::from(sz) * p);
    if dx < 0.0 {
        b.min_x += dx
    } else {
        b.max_x += dx
    }
    if dy < 0.0 {
        b.min_y += dy
    } else {
        b.max_y += dy
    }
    if dz < 0.0 {
        b.min_z += dz
    } else {
        b.max_z += dz
    }
    b
}

/// `canStayAt(pos, face)`: free, a sturdy face to hold on and room to open.
fn can_stay_at(
    level: &Level,
    scratch: &mut Vec<Aabb>,
    pos: BlockPos,
    face: u8,
    own: BlockPos,
) -> bool {
    let state = level.block(pos);
    let blocked =
        !state.is_air() && !(state.block().name() == "minecraft:moving_piston" && pos == own);
    if blocked {
        return false;
    }
    let support = relative(pos, face);
    if !level.is_loaded(support) {
        return false;
    }
    // `isFaceSturdy(opposite face)`: bit `face id` of `sturdy_full`.
    let towards = opposite(face);
    if level.block(support).misc().sturdy_full & (1 << towards) == 0 {
        return false;
    }
    let at = Vec3::new(
        f64::from(pos.x) + 0.5,
        f64::from(pos.y),
        f64::from(pos.z) + 0.5,
    );
    let open = progress_aabb(face, 1.0, at).inflate(-1.0e-6, -1.0e-6, -1.0e-6);
    physics::no_collision(&level.view(), &open, scratch)
}

/// `findAttachableSurface`.
fn find_attachable(
    level: &Level,
    scratch: &mut Vec<Aabb>,
    pos: BlockPos,
    own: BlockPos,
) -> Option<u8> {
    (0..6).find(|&f| can_stay_at(level, scratch, pos, f, own))
}

/// `Shulker.tick` before the living tick: no motion of its own.
pub fn before_tick(t: &mut Tick<'_, '_>) {
    t.set_velocity(Vec3::ZERO);
}

/// `Shulker.tick` after `super.tick()`: a new hold when the block went away, the lid.
pub fn after_tick(t: &mut Tick<'_, '_>) {
    t.set_velocity(Vec3::ZERO);
    let pos = t.block_pos();
    let face = st(t.mob_ref()).attach_face;
    let passenger = t.ents.riding.vehicle_of(t.id).is_some();
    let mut scratch = std::mem::take(&mut t.cx.boxes);
    if !passenger && !can_stay_at(t.level, &mut scratch, pos, face, pos) {
        match find_attachable(t.level, &mut scratch, pos, pos) {
            Some(f) => {
                st_mut(t.mob()).attach_face = f;
                dirty(t);
            }
            None => {
                t.cx.boxes = scratch;
                let id = t.id;
                teleport_somewhere(t.level, t.ents, t.env, t.cx, id);
                scratch = std::mem::take(&mut t.cx.boxes);
            }
        }
    }
    t.cx.boxes = scratch;
    // `updatePeekAmount`.
    let s = st_mut(t.mob());
    s.old_peek = s.current_peek;
    let target = f32::from(s.peek) * 0.01;
    #[allow(clippy::float_cmp)]
    if s.current_peek != target {
        s.current_peek = if s.current_peek > target {
            (s.current_peek - 0.05).clamp(target, 1.0)
        } else {
            (s.current_peek + 0.05).clamp(0.0, target)
        };
        on_peek_change(t);
    }
}

/// `onPeekAmountChange`: the box grows and pushes what is in the way.
fn on_peek_change(t: &mut Tick<'_, '_>) {
    let dims = super::super::super::kinds::mob_dimensions(t.mob_ref());
    t.core_mut().set_dims(dims);
    let s = st(t.mob_ref());
    let (new, old) = (physical_peek(s.current_peek), physical_peek(s.old_peek));
    let push = new - old;
    if push <= 0.0 {
        return;
    }
    let dir = opposite(s.attach_face);
    let (sx, sy, sz) = step(dir);
    let p = t.pos();
    let area = {
        let from = progress_aabb(s.attach_face, old, p);
        let to = progress_aabb(s.attach_face, new, p);
        Aabb::new(
            from.min_x.min(to.min_x),
            from.min_y.min(to.min_y),
            from.min_z.min(to.min_z),
            from.max_x.max(to.max_x),
            from.max_y.max(to.max_y),
            from.max_z.max(to.max_z),
        )
    };
    let mut ids = std::mem::take(&mut t.cx.ids);
    ids.clear();
    t.ents.store.for_each_in(&area, false, |id| ids.push(id));
    let delta = Vec3::new(
        f64::from(push * f32::from(i8::try_from(sx).unwrap_or(0))),
        f64::from(push * f32::from(i8::try_from(sy).unwrap_or(0))),
        f64::from(push * f32::from(i8::try_from(sz).unwrap_or(0))),
    );
    for &id in &ids {
        // Not other shulkers, nor what has `noPhysics` (its own bullets).
        if id == t.id
            || t.ents.mob(id).is_some_and(|m| m.kind == Kind::Shulker)
            || t.ents
                .missile(id)
                .is_some_and(|d| d.kind == MissileKind::ShulkerBullet)
        {
            continue;
        }
        if let Some(mut e) = t.ents.store.get_mut(id) {
            let ep = e.as_ref().pos();
            e.set_pos(ep + delta);
        }
    }
    t.cx.ids = ids;
}

/// `setRawPeekAmount`: closed shulkers get 20 armour.
pub fn set_raw_peek(t: &mut Tick<'_, '_>, amount: u8) {
    let covered = st(t.mob_ref()).covered;
    if covered {
        t.mob().attrs.armor -= 20.0;
        st_mut(t.mob()).covered = false;
    }
    let events = crate::level::game_event::events();
    let at = t.pos();
    if amount == 0 {
        t.mob().attrs.armor += 20.0;
        st_mut(t.mob()).covered = true;
        super::sound(t, "minecraft:entity.shulker.close", 1.0, 1.0);
        crate::mob::living::post_game_event(t, events.container_close, at, None);
    } else {
        super::sound(t, "minecraft:entity.shulker.open", 1.0, 1.0);
        crate::mob::living::post_game_event(t, events.container_open, at, None);
    }
    if st(t.mob_ref()).peek != amount {
        st_mut(t.mob()).peek = amount;
        dirty(t);
    }
}

pub fn attack_can_use(t: &mut Tick<'_, '_>) -> bool {
    t.mob_ref().target.is_some_and(|id| t.other(id).is_some())
        && t.env.difficulty != Difficulty::Peaceful
}

/// `ShulkerAttackGoal.tick`: a bullet every one to five seconds within 20 blocks.
pub fn attack_tick(t: &mut Tick<'_, '_>, attack_time: &mut i32) {
    if t.env.difficulty == Difficulty::Peaceful {
        return;
    }
    *attack_time -= 1;
    let Some(target) = t.mob_ref().target else {
        return;
    };
    let Some(o) = t.other(target) else {
        return;
    };
    t.mob()
        .look
        .set_look_at(Vec3::new(o.pos.x, o.eye_y, o.pos.z), 180.0, 180.0);
    if t.pos().distance_to_sqr(o.pos) < 400.0 {
        if *attack_time <= 0 {
            *attack_time = 20 + t.mob().random.next_int_bounded(10) * 20 / 2;
            let face_axis = axis(st(t.mob_ref()).attach_face);
            shoot_bullet(t, target, face_axis);
            let r = &mut t.mob().random;
            let pitch = (r.next_float() - r.next_float()) * 0.2 + 1.0;
            super::sound(t, "minecraft:entity.shulker.shoot", 2.0, pitch);
        }
    } else {
        t.mob().target = None;
    }
}

/// `new ShulkerBullet(level, shulker, target, axis)`: from the box centre, first step off the
/// shulker's axis.
fn shoot_bullet(t: &mut Tick<'_, '_>, target: EntityId, avoid_axis: u8) {
    // `makeBoundingBox`: the box of the lid as far as it is open now.
    let s = st(t.mob_ref());
    let b = progress_aabb(s.attach_face, physical_peek(s.current_peek), t.pos());
    let start = Vec3::new(
        (b.min_x + b.max_x) / 2.0,
        (b.min_y + b.max_y) / 2.0,
        (b.min_z + b.max_z) / 2.0,
    );
    let mut d = MissileData::new(MissileKind::ShulkerBullet);
    d.random_state = missile::new_projectile_random().state();
    d.target = Some(target);
    d.target_uuid = t.ents.store.get(target).map(|e| e.uuid());
    // ints: [move direction (6 = none), steps, -, -]; vec: target deltas.
    d.ints[0] = 1;
    let id = missile::spawn_shot(t, start, Vec3::ZERO, Rotation::default(), d);
    super::missiles::select_next_move_direction(t.level, t.ents, id, Some(avoid_axis));
}

pub fn peek_can_use(t: &mut Tick<'_, '_>) -> bool {
    if t.mob_ref().target.is_some() {
        return false;
    }
    if t.mob().random.next_int_bounded(goals::reduced(40)) != 0 {
        return false;
    }
    let pos = t.block_pos();
    let face = st(t.mob_ref()).attach_face;
    let mut scratch = std::mem::take(&mut t.cx.boxes);
    let ok = can_stay_at(t.level, &mut scratch, pos, face, pos);
    t.cx.boxes = scratch;
    ok
}

/// `ShulkerLookControl`: the head turns in the plane of its face, never pitches.
pub fn look_control(t: &mut Tick<'_, '_>) {
    let look = t.mob_ref().look;
    let mut rot = t.core().rot();
    if look.cooldown > 0 {
        t.mob().look.cooldown -= 1;
        let face = opposite(st(t.mob_ref()).attach_face);
        // `face.getRotation()` applied to south.
        let forward = match face {
            1 => (0.0, 0.0, 1.0),
            0 => (0.0, 0.0, -1.0),
            _ => (0.0, -1.0, 0.0),
        };
        let (ux, uy, uz) = step(face);
        let up = (f64::from(ux), f64::from(uy), f64::from(uz));
        let right = (
            up.1 * forward.2 - up.2 * forward.1,
            up.2 * forward.0 - up.0 * forward.2,
            up.0 * forward.1 - up.1 * forward.0,
        );
        let p = t.pos();
        let out = (
            look.wanted.x - p.x,
            look.wanted.y - t.eye_y(),
            look.wanted.z - p.z,
        );
        let dot = |a: (f64, f64, f64)| a.0 * out.0 + a.1 * out.1 + a.2 * out.2;
        let (dr, df) = (dot(right), dot(forward));
        if dr.abs() > 1.0e-5 || df.abs() > 1.0e-5 {
            #[allow(clippy::cast_possible_truncation)]
            let y_rot_d = crate::mob::atan2_deg(-dr, df) as f32;
            rot.y_head_rot = rotate_towards(rot.y_head_rot, y_rot_d, look.y_max_rot_speed);
        }
        rot.x_rot = rotate_towards(rot.x_rot, 0.0, look.x_max_rot_angle);
    } else {
        let body = t.mob_ref().y_body_rot;
        rot.y_head_rot = rotate_towards(rot.y_head_rot, body, 10.0);
    }
    t.core_mut().set_rot(rot);
}

fn rotate_towards(from: f32, to: f32, max: f32) -> f32 {
    let diff = super::super::super::nav::degrees_difference(from, to);
    from + diff.clamp(-max, max)
}

/// `teleportSomewhere`: up to five tries within eight blocks.
pub fn teleport_somewhere(
    level: &Level,
    ents: &mut Entities,
    env: &mut crate::survival::player::Env<'_>,
    cx: &mut super::super::super::MobContext,
    id: EntityId,
) -> bool {
    let Some(m) = ents.mob(id) else {
        return false;
    };
    if m.no_ai || !m.is_alive() {
        return false;
    }
    let Some(old) = ents.store.get(id).map(|e| {
        let p = e.pos();
        BlockPos::containing(p.x, p.y, p.z)
    }) else {
        return false;
    };
    let mut scratch = std::mem::take(&mut cx.boxes);
    let mut done = false;
    for _ in 0..5 {
        let Some(m) = ents.mob_mut(id) else {
            break;
        };
        let r = &mut m.random;
        let dx = r.next_int_bounded(17) - 8;
        let dy = r.next_int_bounded(17) - 8;
        let dz = r.next_int_bounded(17) - 8;
        let target = BlockPos::new(old.x + dx, old.y + dy, old.z + dz);
        if target.y <= level.min_y()
            || !level.block(target).is_air()
            || !level.border.is_block_within_bounds(target)
        {
            continue;
        }
        let unit = Aabb::new(
            f64::from(target.x),
            f64::from(target.y),
            f64::from(target.z),
            f64::from(target.x) + 1.0,
            f64::from(target.y) + 1.0,
            f64::from(target.z) + 1.0,
        )
        .inflate(-1.0e-6, -1.0e-6, -1.0e-6);
        if !physics::no_collision(&level.view(), &unit, &mut scratch) {
            continue;
        }
        let Some(face) = find_attachable(level, &mut scratch, target, old) else {
            continue;
        };
        // `#shulker_does_not_teleport_to` (bedrock): not onto those blocks.
        let surface = level.block(relative(target, face));
        if crate::mob::fam::wildlife::block_tags()
            .shulker_does_not_teleport_to
            .contains(surface)
        {
            continue;
        }
        ents.riding.remove(id);
        if let Some(m) = ents.mob_mut(id) {
            let s = st_mut(m);
            s.attach_face = face;
            s.peek = 0;
            m.target = None;
        }
        combat::mob_sound(ents, env, id, "minecraft:entity.shulker.teleport");
        if let Some(mut e) = ents.store.get_mut(id) {
            e.set_pos(Vec3::new(
                f64::from(target.x) + 0.5,
                f64::from(target.y),
                f64::from(target.z) + 0.5,
            ));
            e.mark_data_dirty();
        }
        crate::survival::game_event(
            env.out,
            crate::level::game_event::events().teleport,
            Vec3::new(
                f64::from(old.x) + 0.5,
                f64::from(old.y) + 0.5,
                f64::from(old.z) + 0.5,
            ),
            Some(id),
        );
        let packed = ((old.x - target.x + 8) & 0xFF) << 16
            | ((old.y - target.y + 8) & 0xFF) << 8
            | ((old.z - target.z + 8) & 0xFF);
        cx.actions.push(WorldAction::LevelEvent(2016, old, packed));
        done = true;
        break;
    }
    cx.boxes = scratch;
    done
}

/// `Shulker.hurtServer` before the damage: closed shells stop arrows.
pub fn hurt_filter(h: &mut Hurt<'_, '_>, amount: f32) -> Option<f32> {
    let closed = h.ents.mob(h.target).is_some_and(|m| st(m).peek == 0);
    // `directEntity instanceof AbstractArrow`: arrows, spectral arrows and tridents.
    let arrow = |d| {
        h.ents.store.get(d).is_some_and(|e| {
            crate::mob::projectile::is_arrow_kind(e.kind())
                || e.kind() == vanillite_data::entity::ids::TRIDENT
        })
    };
    if closed && h.source.direct.is_some_and(arrow) {
        return None;
    }
    Some(amount)
}

/// `Shulker.hurtServer` after the damage: flee when hurt badly, split when a bullet hits.
pub fn after_hurt(h: &mut Hurt<'_, '_>, _amount: f32) {
    let Some(m) = h.ents.mob_mut(h.target) else {
        return;
    };
    let low = m.health < m.max_health() * 0.5;
    if low && m.random.next_int_bounded(4) == 0 {
        teleport_somewhere(h.level, h.ents, h.env, h.cx, h.target);
        return;
    }
    let by_bullet = h
        .env
        .types
        .is(h.source.kind, crate::survival::damage::tag::IS_PROJECTILE)
        && h.source.direct.is_some_and(|d| {
            h.ents
                .missile(d)
                .is_some_and(|x| x.kind == MissileKind::ShulkerBullet)
        });
    if by_bullet {
        hit_by_bullet(h);
    }
}

/// `hitByShulkerBullet`: an open shulker teleports and may leave a new one behind.
fn hit_by_bullet(h: &mut Hurt<'_, '_>) {
    let Some(m) = h.ents.mob(h.target) else {
        return;
    };
    let closed = st(m).peek == 0;
    let color = st(m).color;
    let Some((old_pos, old_aabb)) = h.ents.store.get(h.target).map(|e| (e.pos(), e.aabb())) else {
        return;
    };
    if closed || !teleport_somewhere(h.level, h.ents, h.env, h.cx, h.target) {
        return;
    }
    let area = old_aabb.inflate(8.0, 8.0, 8.0);
    let mut count = 0;
    {
        let ents = &*h.ents;
        ents.store.for_each_in(&area, false, |id| {
            if ents
                .mob(id)
                .is_some_and(|m| m.kind == Kind::Shulker && m.is_alive())
            {
                count += 1;
            }
        });
    }
    #[allow(clippy::cast_precision_loss)]
    let failure = (count - 1) as f32 / 5.0;
    if h.env.random.next_float() < failure {
        return;
    }
    let mut baby = MobData::new(Kind::Shulker, crate::mob::new_entity_seed());
    st_mut(&mut baby).color = color;
    let uuid = crate::mob::new_uuid();
    let id = super::super::super::spawn(
        h.ents,
        h.level.network_ids(),
        uuid,
        old_pos,
        Rotation::default(),
        baby,
    );
    let parent = h.ents.store.get(h.target).map_or(0, |e| e.uuid());
    h.ents
        .spawned(id, vanillite_abi::spawn_reason::BREEDING, parent);
}
