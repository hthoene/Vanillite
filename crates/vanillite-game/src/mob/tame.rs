//! Owners and leads, shared by every tameable and leashable mob: `TamableAnimal` (taming,
//! sitting, following and protecting the owner: `SitWhenOrderedToGoal`, `FollowOwnerGoal`,
//! `OwnerHurtByTargetGoal`, `OwnerHurtTargetGoal`) and `Leashable` (leads held by players,
//! mobs or fence knots). The tameable kinds plug in through [`tame`] and the goals.

use vanillite_base::random::RandomSource;
use vanillite_base::{Aabb, BlockPos, Vec3};
use vanillite_config::GameMode;
use vanillite_protocol::SharedPacket;
use vanillite_protocol::packets::play::clientbound::{SetEntityLink, SoundSource};
use vanillite_protocol::packets::play::serverbound::Hand;
use vanillite_world::entity::{EntityId, NetworkIds, NewEntity};

use super::animal::Interaction;
use super::{MobData, Tick, nav};
use crate::entities::Entities;
use crate::item::ItemStack;
use crate::level::Level;
use crate::survival::player::Env;
use crate::survival::rules::Rule;
use crate::survival::{Effect, play_sound};

/// `TamableAnimal.tame(player)`: the player owns the mob from now on.
pub fn tame(m: &mut MobData, owner: u128) {
    m.tame = true;
    m.owner = Some(owner);
    m.persistent = true;
}

/// The owner entity of a tamed mob in this world (`getOwner`).
pub fn owner_of(ents: &Entities, m: &MobData) -> Option<EntityId> {
    ents.player_by_uuid(m.owner?)
}

/// `setInSittingPose`.
pub fn set_in_sitting_pose(t: &mut Tick<'_, '_>, on: bool) {
    if t.mob_ref().in_sitting_pose != on {
        t.mob().in_sitting_pose = on;
        t.core_mut().mark_data_dirty();
    }
}

/// `SitWhenOrderedToGoal.canUse`.
pub fn sit_can_use(t: &mut Tick<'_, '_>) -> bool {
    let m = t.mob_ref();
    if !m.tame || m.fluids.in_water() || !t.on_ground() {
        return false;
    }
    let ordered = m.order_to_sit;
    let Some(owner) = owner_of(t.ents, m) else {
        return true;
    };
    let near = t
        .ents
        .store
        .get(owner)
        .is_some_and(|e| e.pos().distance_to_sqr(t.pos()) < 144.0);
    let hurt = t
        .ents
        .player(owner)
        .and_then(|p| p.survival.last_hurt_by_mob)
        .is_some();
    if near && hurt { false } else { ordered }
}

/// `unableToMoveToOwner`.
fn unable_to_move_to_owner(t: &Tick<'_, '_>, owner: EntityId) -> bool {
    t.mob_ref().order_to_sit
        || t.ents.vehicle_of(t.id).is_some()
        || t.ents.leashes.contains_key(&t.id)
        || t.ents
            .player(owner)
            .is_some_and(|p| p.game_mode == GameMode::Spectator)
}

/// `FollowOwnerGoal.canUse`.
pub fn follow_can_use(t: &mut Tick<'_, '_>, start_distance: f32) -> bool {
    let Some(owner) = owner_of(t.ents, t.mob_ref()) else {
        return false;
    };
    if unable_to_move_to_owner(t, owner) {
        return false;
    }
    let d = f64::from(start_distance);
    t.ents
        .store
        .get(owner)
        .is_some_and(|e| e.pos().distance_to_sqr(t.pos()) >= d * d)
}

/// `FollowOwnerGoal.canContinueToUse`.
pub fn follow_can_continue(t: &mut Tick<'_, '_>, stop_distance: f32) -> bool {
    if t.mob_ref().nav.is_done() {
        return false;
    }
    let Some(owner) = owner_of(t.ents, t.mob_ref()) else {
        return false;
    };
    if unable_to_move_to_owner(t, owner) {
        return false;
    }
    let d = f64::from(stop_distance);
    t.ents
        .store
        .get(owner)
        .is_some_and(|e| e.pos().distance_to_sqr(t.pos()) > d * d)
}

/// `FollowOwnerGoal.tick`: walk to the owner, or teleport next to it from 12 blocks on.
pub fn follow_tick(t: &mut Tick<'_, '_>, speed: f64, recalc: &mut i32) {
    let Some(owner) = owner_of(t.ents, t.mob_ref()) else {
        return;
    };
    let Some(o) = t.other(owner) else {
        return;
    };
    let teleport = t.pos().distance_to_sqr(o.pos) >= 144.0;
    if !teleport {
        t.mob()
            .look
            .set_look_at(Vec3::new(o.pos.x, o.eye_y, o.pos.z), 10.0, 40.0);
    }
    *recalc -= 1;
    if *recalc <= 0 {
        *recalc = super::goals::reduced(10);
        if teleport {
            try_to_teleport_to_owner(t, o.pos);
        } else {
            nav::move_to_entity(t, o.pos, 1, speed);
        }
    }
}

/// `tryToTeleportToOwner`: ten tries at a free spot within 3 blocks of the owner.
/// `tryToTeleportToOwner` to the owner's position (the pets' panic goal uses it too).
pub fn teleport_near_owner(t: &mut Tick<'_, '_>, owner: Vec3) {
    try_to_teleport_to_owner(t, owner);
}

fn try_to_teleport_to_owner(t: &mut Tick<'_, '_>, owner: Vec3) {
    let base = BlockPos::containing(owner.x, owner.y, owner.z);
    for _ in 0..10 {
        let m = t.mob();
        let dx = m.random.next_int_between_inclusive(-3, 3);
        let dy = m.random.next_int_between_inclusive(-1, 1);
        let dz = m.random.next_int_between_inclusive(-3, 3);
        if dx.abs() < 2 && dz.abs() < 2 {
            continue;
        }
        let pos = BlockPos::new(base.x + dx, base.y + dy, base.z + dz);
        if can_teleport_to(t, pos) {
            let p = Vec3::new(
                f64::from(pos.x) + 0.5,
                f64::from(pos.y),
                f64::from(pos.z) + 0.5,
            );
            t.core_mut().set_pos(p);
            t.mob().nav.stop();
            return;
        }
    }
}

/// `canTeleportTo`: a walkable spot, not on leaves, with room for the mob.
fn can_teleport_to(t: &mut Tick<'_, '_>, pos: BlockPos) -> bool {
    let below = t.level.block(BlockPos::new(pos.x, pos.y - 1, pos.z));
    if below.collision_shape().is_empty() || below.block().info().name.ends_with("_leaves") {
        return false;
    }
    let here = t.level.block(pos);
    if !here.collision_shape().is_empty() {
        return false;
    }
    let dims = t.core().dims();
    let half = f64::from(dims.width) / 2.0;
    let (x, y, z) = (
        f64::from(pos.x) + 0.5,
        f64::from(pos.y),
        f64::from(pos.z) + 0.5,
    );
    let b = Aabb::new(
        x - half,
        y,
        z - half,
        x + half,
        y + f64::from(dims.height),
        z + half,
    );
    super::physics::no_collision(t.level, &b, &mut t.cx.boxes)
}

/// The owner's `lastHurtByMob` (`by_owner = true`) or `lastHurtMob` with its time stamp, while
/// it is at most 100 ticks old.
fn owner_hurt_info(t: &Tick<'_, '_>, by_owner: bool) -> Option<(EntityId, i32)> {
    let owner = owner_of(t.ents, t.mob_ref())?;
    let p = t.ents.player(owner)?;
    let entry = if by_owner {
        p.survival.last_hurt_by_mob
    } else {
        p.survival.last_hurt_mob
    }?;
    (p.survival.tick_count - entry.1 <= 100).then_some(entry)
}

/// `OwnerHurtByTargetGoal.canUse` (`by_owner`) and `OwnerHurtTargetGoal.canUse`.
pub fn owner_hurt_by_can_use(
    t: &mut Tick<'_, '_>,
    timestamp: i32,
    found: &mut Option<EntityId>,
    by_owner: bool,
) -> bool {
    let m = t.mob_ref();
    if !m.tame || m.order_to_sit {
        return false;
    }
    let Some((target, stamp)) = owner_hurt_info(t, by_owner) else {
        return false;
    };
    if stamp == timestamp || target == t.id {
        return false;
    }
    let alive = t.ents.mob(target).is_some_and(MobData::is_alive)
        || t.ents.player(target).is_some_and(|p| {
            !p.survival.is_dead_or_dying()
                && matches!(p.game_mode, GameMode::Survival | GameMode::Adventure)
        });
    // `wantsToAttack`: never the owner's other pets.
    let same_owner = t
        .ents
        .mob(target)
        .is_some_and(|o| o.tame && o.owner == t.mob_ref().owner);
    if !alive || same_owner || owner_of(t.ents, t.mob_ref()) == Some(target) {
        return false;
    }
    *found = Some(target);
    true
}

/// The time stamp an owner goal remembers when it starts.
pub fn owner_hurt_stamp(t: &Tick<'_, '_>, by_owner: bool) -> i32 {
    owner_hurt_info(t, by_owner).map_or(0, |(_, s)| s)
}

// Leads.

/// `canBeLeashed`: animals, not monsters.
fn can_be_leashed(m: &MobData) -> bool {
    !m.kind.is_monster()
}

/// The holder of `mob`'s lead.
pub fn leash_holder(ents: &Entities, mob: EntityId) -> Option<EntityId> {
    ents.leashes.get(&mob).copied()
}

fn link_packet(ents: &Entities, env: &mut Env<'_>, mob: EntityId, holder: Option<EntityId>) {
    let Some(e) = ents.store.get(mob) else {
        return;
    };
    let dest = holder
        .and_then(|h| ents.store.get(h))
        .map_or(0, |h| h.network_id().0);
    env.out.push(Effect::Tracking {
        entity: mob,
        with_self: false,
        packet: SharedPacket::of(&SetEntityLink {
            source_id: e.network_id().0,
            dest_id: dest,
        }),
    });
}

/// The pairing packet of a leashed mob.
pub fn link_pairing_packet(ents: &Entities, mob: EntityId) -> Option<SetEntityLink> {
    let holder = leash_holder(ents, mob)?;
    Some(SetEntityLink {
        source_id: ents.store.get(mob)?.network_id().0,
        dest_id: ents.store.get(holder)?.network_id().0,
    })
}

/// `setLeashedTo(holder, true)`.
pub fn set_leashed_to(ents: &mut Entities, env: &mut Env<'_>, mob: EntityId, holder: EntityId) {
    ents.leashes.insert(mob, holder);
    if let Some(m) = ents.mob_mut(mob) {
        m.persistent = true;
        m.pending_leash = None;
    }
    link_packet(ents, env, mob, Some(holder));
    if ents.vehicle_of(mob).is_some() {
        crate::riding::stop_riding(ents, env, mob);
    }
}

/// `dropLeash` (`drop_item`) or `removeLeash`.
pub fn drop_leash(ents: &mut Entities, env: &mut Env<'_>, mob: EntityId, drop_item: bool) {
    if ents.leashes.remove(&mob).is_none() {
        return;
    }
    link_packet(ents, env, mob, None);
    if drop_item
        && let Some(pos) = ents.store.get(mob).map(|e| e.pos())
        && let Some(lead) = vanillite_data::registry::ITEM.id("minecraft:lead")
    {
        let (uuid, y_rot, velocity) = crate::mob::new_item_entity();
        env.out.push(Effect::DropItem {
            uuid,
            pos,
            velocity,
            y_rot,
            pickup_delay: 10,
            stack: ItemStack::new(lead, 1),
        });
    }
}

fn entity_sound(ents: &Entities, env: &mut Env<'_>, id: EntityId, sound: &str) {
    let Some(pos) = ents.store.get(id).map(|e| e.pos()) else {
        return;
    };
    let seed = crate::survival::sound_seed();
    play_sound(
        env.out,
        None,
        pos,
        sound,
        SoundSource::Neutral,
        1.0,
        1.0,
        seed,
    );
}

/// The lead part of `Entity.interact`: tie, untie or pass on leads.
pub fn leash_interact(
    ents: &mut Entities,
    env: &mut Env<'_>,
    player: EntityId,
    target: EntityId,
    hand: Hand,
    sneaking: bool,
) -> Interaction {
    let leashable = ents
        .mob(target)
        .is_some_and(|m| m.is_alive() && can_be_leashed(m))
        || ents.boat(target).is_some();
    let baby = ents.mob(target).is_some_and(|m| m.baby);
    if sneaking && leashable && !baby {
        // The player's leashed mobs nearby get tied to the target.
        let mine = leashed_by(ents, player, target, 10.0);
        let mut any = false;
        for m in mine {
            if m != target {
                set_leashed_to(ents, env, m, target);
                any = true;
            }
        }
        if any {
            entity_sound(ents, env, target, "minecraft:item.lead.tied");
            return Interaction::Success;
        }
    }
    let stack = {
        let i = match hand {
            Hand::MainHand => ents.player(player).map_or(0, |p| p.inventory.selected),
            Hand::OffHand => crate::inventory::OFFHAND,
        };
        ents.player(player)
            .map(|p| p.inventory.get(i).clone())
            .unwrap_or_default()
    };
    let name = crate::item::item_id(&stack).name();
    if name == "minecraft:shears" && leash_holder(ents, target).is_some() && !sneaking {
        drop_leash(ents, env, target, true);
        entity_sound(ents, env, target, "minecraft:item.shears.snip");
        return Interaction::Success;
    }
    if !leashable {
        return Interaction::Pass;
    }
    if leash_holder(ents, target) == Some(player) {
        let creative = ents
            .player(player)
            .is_some_and(|p| p.game_mode == GameMode::Creative);
        drop_leash(ents, env, target, !creative);
        entity_sound(ents, env, target, "minecraft:item.lead.untied");
        return Interaction::Success;
    }
    let held_by_player = leash_holder(ents, target).is_some_and(|h| ents.player(h).is_some());
    if name == "minecraft:lead" && !held_by_player {
        let close = match (ents.store.get(target), ents.store.get(player)) {
            (Some(a), Some(b)) => a.pos().distance_to_sqr(b.pos()) <= 144.0,
            _ => false,
        };
        if close {
            if leash_holder(ents, target).is_some() {
                drop_leash(ents, env, target, true);
            }
            set_leashed_to(ents, env, target, player);
            entity_sound(ents, env, target, "minecraft:item.lead.tied");
            let creative = ents
                .player(player)
                .is_some_and(|p| p.game_mode == GameMode::Creative);
            if !creative {
                let i = match hand {
                    Hand::MainHand => ents.player(player).map_or(0, |p| p.inventory.selected),
                    Hand::OffHand => crate::inventory::OFFHAND,
                };
                if let Some(p) = ents.player_mut(player)
                    && let Some(s) = p.inventory.get_mut(i)
                {
                    s.shrink(1);
                }
            }
            return Interaction::Success;
        }
    }
    Interaction::Pass
}

/// The mobs within `radius` of `around` whose leads `holder` holds (`leashableInArea`).
fn leashed_by(ents: &Entities, holder: EntityId, around: EntityId, radius: f64) -> Vec<EntityId> {
    let Some(c) = ents.store.get(around).map(|e| e.pos()) else {
        return Vec::new();
    };
    let area = Aabb::new(
        c.x - radius,
        c.y - radius,
        c.z - radius,
        c.x + radius,
        c.y + radius,
        c.z + radius,
    );
    let mut out = Vec::new();
    ents.store.for_each_in(&area, false, |id| {
        if ents.leashes.get(&id) == Some(&holder) {
            out.push(id);
        }
    });
    out
}

/// Whether a block holds leash knots (`#minecraft:leash_knots`: the fences).
pub fn is_knot_block(state: vanillite_data::block::BlockState) -> bool {
    state.block().info().name.ends_with("_fence")
}

/// The knot on a fence, created when missing (`LeashFenceKnotEntity.getOrCreateKnot`).
pub fn knot_at(
    ents: &mut Entities,
    network_ids: &NetworkIds,
    pos: BlockPos,
    uuid: u128,
) -> EntityId {
    let center = Vec3::new(
        f64::from(pos.x) + 0.5,
        f64::from(pos.y) + 0.375,
        f64::from(pos.z) + 0.5,
    );
    let area = Aabb::of_block(pos).inflate(0.1, 0.1, 0.1);
    let mut found = None;
    ents.store.for_each_in(&area, false, |id| {
        if ents
            .store
            .get(id)
            .is_some_and(|e| e.kind() == vanillite_data::entity::ids::LEASH_KNOT)
        {
            found = Some(id);
        }
    });
    if let Some(id) = found {
        return id;
    }
    let new = NewEntity::new(
        vanillite_data::entity::ids::LEASH_KNOT,
        network_ids.next(),
        uuid,
        center,
    );
    let network_id = new.network_id.0;
    let knots = &mut ents.knots;
    let id = ents.store.spawn(new, |id| knots.push(id, pos));
    ents.by_network_id.insert(network_id, id);
    id
}

/// `LeadItem.useOn` on a fence: the player's leashed mobs within 7 blocks get tied to it.
pub fn use_lead_on_fence(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    player: EntityId,
    pos: BlockPos,
) -> bool {
    if !is_knot_block(level.block(pos)) {
        return false;
    }
    let center = Vec3::new(
        f64::from(pos.x) + 0.5,
        f64::from(pos.y) + 0.5,
        f64::from(pos.z) + 0.5,
    );
    let area = Aabb::new(
        center.x - 7.0,
        center.y - 7.0,
        center.z - 7.0,
        center.x + 7.0,
        center.y + 7.0,
        center.z + 7.0,
    );
    let mut mine = Vec::new();
    ents.store.for_each_in(&area, false, |id| {
        if ents.leashes.get(&id) == Some(&player) {
            mine.push(id);
        }
    });
    if mine.is_empty() {
        return false;
    }
    let uuid = crate::mob::new_entity_uuid();
    let knot = knot_at(ents, level.network_ids(), pos, uuid);
    for m in mine {
        set_leashed_to(ents, env, m, knot);
    }
    entity_sound(ents, env, knot, "minecraft:item.lead.tied");
    true
}

/// A player hit or used a knot: it breaks and drops the leads tied to it.
pub fn break_knot(
    ents: &mut Entities,
    env: &mut Env<'_>,
    knot: EntityId,
    discarded: &mut Vec<EntityId>,
) {
    let tied: Vec<EntityId> = ents
        .leashes
        .iter()
        .filter(|&(_, &h)| h == knot)
        .map(|(&m, _)| m)
        .collect();
    for m in tied {
        drop_leash(ents, env, m, true);
    }
    entity_sound(ents, env, knot, "minecraft:item.lead.untied");
    discarded.push(knot);
}

/// `Leashable.tickLeash` of every leashed mob, and knots on fences that are gone. Runs once per
/// world tick after the entities.
pub fn tick_leashes(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    discarded: &mut Vec<EntityId>,
) {
    // Leashes read from NBT find their holder.
    let pending: Vec<(EntityId, super::LeashRef)> = ents
        .mobs
        .iter()
        .filter_map(|(id, m)| m.pending_leash.map(|l| (id, l)))
        .collect();
    for (id, l) in pending {
        let holder = match l {
            super::LeashRef::Entity(uuid) => ents.player_by_uuid(uuid),
            super::LeashRef::Knot(pos) => {
                if level.is_loaded(pos) && is_knot_block(level.block(pos)) {
                    let uuid = crate::mob::new_entity_uuid();
                    Some(knot_at(ents, level.network_ids(), pos, uuid))
                } else {
                    None
                }
            }
        };
        if let Some(h) = holder {
            set_leashed_to(ents, env, id, h);
        }
    }
    if !ents.knots.is_empty() {
        let gone: Vec<EntityId> = ents
            .knots
            .iter()
            .filter(|(_, pos)| level.is_loaded(**pos) && !is_knot_block(level.block(**pos)))
            .map(|(id, _)| id)
            .collect();
        for k in gone {
            break_knot(ents, env, k, discarded);
        }
    }
    // Holders that left the world.
    let orphans: Vec<EntityId> = ents
        .leashes
        .iter()
        .filter(|&(_, h)| !ents.store.contains(*h))
        .map(|(&m, _)| m)
        .collect();
    for mob in orphans {
        if ents.store.contains(mob) {
            let drops = env.rules.bool(Rule::ENTITY_DROPS);
            drop_leash(ents, env, mob, drops);
        } else {
            ents.leashes.remove(&mob);
        }
    }
}

/// `Leashable.tickLeash` of the ticking mob (end of `Mob.tick`).
pub fn tick_leash(t: &mut Tick<'_, '_>) {
    let Some(holder) = leash_holder(t.ents, t.id) else {
        return;
    };
    let mob = t.id;
    let Some(he) = t.ents.store.get(holder) else {
        return;
    };
    let alive = t.mob_ref().is_alive();
    let holder_ok = t
        .ents
        .player(holder)
        .is_none_or(|p| !p.survival.is_dead_or_dying() && p.game_mode != GameMode::Spectator);
    let (hpos, hw, hh) = (
        he.pos(),
        f64::from(he.dims().width),
        f64::from(he.dims().height),
    );
    let holder_motion = if t.ents.player(holder).is_some() {
        he.pos() - he.old_pos()
    } else {
        he.velocity()
    };
    if !alive || !holder_ok {
        let drops = t.env.rules.bool(Rule::ENTITY_DROPS);
        drop_leash(t.ents, t.env, mob, drops);
        return;
    }
    let me = t.core();
    let (mpos, mw, mh) = (
        me.pos(),
        f64::from(me.dims().width),
        f64::from(me.dims().height),
    );
    let y_rot = me.rot().y_rot;
    let velocity = me.velocity();
    let distance = mpos.distance_to_sqr(hpos).sqrt();
    // `PathfinderMob.whenLeashedTo`: home at the holder.
    if distance > 12.0 {
        let seed = crate::survival::sound_seed();
        play_sound(
            t.env.out,
            None,
            hpos,
            "minecraft:item.lead.break",
            SoundSource::Neutral,
            1.0,
            1.0,
            seed,
        );
        drop_leash(t.ents, t.env, mob, true);
    } else if distance > 6.0 - hw - mw {
        // `checkElasticInteractions` with one attachment point each.
        let a = y_rot * std::f32::consts::PI / 180.0;
        let (s, c) = (f64::from(a.sin()), f64::from(a.cos()));
        // `(0, 0.5, 0.5) * dims` turned by the yaw.
        let arm = Vec3::new(-0.5 * mw * s, 0.5 * mh, 0.5 * mw * c);
        let at = mpos + arm;
        let pivot = hpos + Vec3::new(0.0, 0.5 * hh, 0.0);
        let d = at.distance_to_sqr(pivot).sqrt();
        if d >= 6.0 {
            let mut force = (pivot - at).normalize() * (d - 6.0);
            if velocity.dot(force) >= 0.0 {
                force = force * 0.3;
            }
            let rel = holder_motion - velocity;
            let add = force.multiply(0.8, 0.2, 0.8) + rel * 0.11;
            t.set_velocity(velocity + add);
        }
    } else if !super::animal::is_panicking(t.mob_ref()) {
        // `PathfinderMob.closeRangeLeashBehaviour`: stay within 2 blocks of the holder.
        let to = hpos - mpos;
        let len = to.length();
        let target = mpos + to.normalize() * (len - 2.0).max(0.0);
        nav::move_to_pos(t, target, 1.0);
    }
}
