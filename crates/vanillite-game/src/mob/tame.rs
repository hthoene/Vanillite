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
use super::kinds::Kind;
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

/// The plugins' tame event (`vanillite_abi::event::ENTITY_TAME`) of `animal` by the player
/// `owner` (its UUID): whether it may (`false`: the attempt fails). Without listeners one
/// branch.
pub fn tame_event(ents: &Entities, animal: EntityId, owner: u128) -> bool {
    let Some(hooks) = ents.hooks_for(vanillite_abi::event::ENTITY_TAME) else {
        return true;
    };
    let Some(e) = ents.store.get(animal) else {
        return true;
    };
    let mut ev = crate::hooks::Event {
        entity: e.uuid(),
        entity_type: e.kind().name().to_owned(),
        other: owner,
        player: owner,
        ..crate::hooks::Event::new(vanillite_abi::event::ENTITY_TAME)
    };
    hooks.event(&mut ev);
    !ev.cancelled
}

/// The `tame_animal` trigger of `TamableAnimal.tame(player)` and
/// `AbstractHorse.tameWithName(player)`.
pub fn award_tame(ents: &mut Entities, animal: EntityId, player: EntityId) {
    use crate::advancements::trigger::{Event, TriggerKind};
    use crate::advancements::view;
    if !view::listens(ents, player, TriggerKind::TAME_ANIMAL) {
        return;
    }
    if let Some(e) = view::snapshot(ents, animal)
        && let Some(p) = ents.player_mut(player)
    {
        p.advancements
            .trigger(Event::Entity(TriggerKind::TAME_ANIMAL, e));
    }
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
    // An untamed mob ordered to sit (`Sitting` loaded or merged) sits too.
    if (!m.order_to_sit && !m.tame) || m.fluids.in_water() || !t.on_ground() {
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
    super::physics::no_collision(&t.level.view(), &b, &mut t.cx.boxes)
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
    // `canAttack(target, TargetingConditions.DEFAULT)` (never the owner).
    let owner = owner_of(t.ents, t.mob_ref());
    if !crate::mob::goals::can_attack(t, target) || owner == Some(target) {
        return false;
    }
    // `Wolf.wantsToAttack`: no creepers, ghasts, armour stands, tamed horses or tamed pets
    // (other owners' wolves are fair game); other pets want to attack anything.
    if t.mob_ref().kind == Kind::Wolf
        && let Some(o) = t.ents.mob(target)
    {
        let wanted = match o.kind {
            Kind::Creeper | Kind::Ghast | Kind::ArmorStand => false,
            Kind::Wolf => !o.tame || o.owner != t.mob_ref().owner,
            k if crate::mob::fam::companion::is_horse(k)
                || matches!(k, Kind::Camel | Kind::CamelHusk) =>
            {
                !o.tame
            }
            k if crate::mob::fam::companion::is_tamable(k)
                || matches!(k, Kind::Nautilus | Kind::ZombieNautilus) =>
            {
                !o.tame
            }
            _ => true,
        };
        if !wanted {
            return false;
        }
    }
    // `Wolf.wantsToAttack`: a player's wolf attacks no player when `canHarmPlayer` fails (the
    // pvp rule).
    if t.ents.player(target).is_some()
        && owner.is_some_and(|o| t.ents.player(o).is_some())
        && !t.env.rules.bool(crate::survival::rules::Rule::PVP)
    {
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

/// `canBeLeashed` with its overrides: tamables, squids, dolphins, axolotls and hoglins yes;
/// fish, bats, villagers, pandas, turtles and enemies no; angry wolves, aggravated nautiluses
/// and mob-ridden zombie mounts no; sulfur cubes only with a body item.
fn can_be_leashed(ents: &Entities, id: EntityId, m: &MobData) -> bool {
    use crate::mob::brain::memory::MemoryType;
    let mob_controlled = || {
        ents.riding
            .passengers_of(id)
            .first()
            .is_some_and(|&p| ents.mob(p).is_some())
    };
    let aggravated = || {
        m.brain
            .as_ref()
            .is_some_and(|b| b.has(MemoryType::AngryAt) || b.has(MemoryType::AttackTarget))
    };
    match m.kind {
        Kind::Wolf => !crate::mob::common::is_angry(m, ents.game_time),
        Kind::Squid
        | Kind::GlowSquid
        | Kind::Dolphin
        | Kind::Axolotl
        | Kind::Hoglin
        | Kind::Zoglin => true,
        Kind::Cod
        | Kind::Salmon
        | Kind::Pufferfish
        | Kind::TropicalFish
        | Kind::Tadpole
        | Kind::Bat
        | Kind::Villager
        | Kind::WanderingTrader
        | Kind::Panda
        | Kind::Turtle => false,
        Kind::CamelHusk => !mob_controlled(),
        Kind::ZombieHorse => m.tame || !mob_controlled(),
        Kind::Nautilus => !aggravated(),
        Kind::ZombieNautilus => !aggravated() && !mob_controlled(),
        Kind::SulfurCube => crate::mob::fam::undead::cubes::has_body_item(m),
        _ if crate::mob::fam::companion::is_tamable(m.kind) => true,
        k => !k.is_monster(),
    }
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

/// `dropAllLeashConnections` of a player: the mobs on its leads go free and drop their leads;
/// `true` when there were any.
pub fn drop_leashes_of(ents: &mut Entities, env: &mut Env<'_>, holder: EntityId) -> bool {
    let mut mine: Vec<EntityId> = ents
        .leashes
        .iter()
        .filter(|(_, h)| **h == holder)
        .map(|(m, _)| *m)
        .collect();
    mine.sort_unstable();
    for &m in &mine {
        drop_leash(ents, env, m, true);
    }
    !mine.is_empty()
}

/// `Entity.shearOffAllLeashConnections(null)` (a dispenser's shears): the entity's own lead
/// and the leads tied to it drop, `SHEAR` at the entity, the snip sound at its block in its
/// sound source; `true` when there was a lead.
pub fn shear_off_all_leash_connections(
    ents: &mut Entities,
    env: &mut Env<'_>,
    id: EntityId,
) -> bool {
    let mut tied: Vec<EntityId> = ents
        .leashes
        .iter()
        .filter(|(_, h)| **h == id)
        .map(|(m, _)| *m)
        .collect();
    tied.sort_unstable();
    let own = leash_holder(ents, id).is_some();
    if !own && tied.is_empty() {
        return false;
    }
    if own {
        drop_leash(ents, env, id, true);
    }
    for m in tied {
        drop_leash(ents, env, m, true);
    }
    let Some(pos) = ents.store.get(id).map(|e| e.pos()) else {
        return true;
    };
    let event = crate::level::game_event::events().shear;
    super::animal::entity_game_event(ents, env, id, event, None);
    let source = ents
        .mob(id)
        .map_or(SoundSource::Neutral, |m| m.kind.sound_source());
    let block = BlockPos::containing(pos.x, pos.y, pos.z);
    let at = Vec3::new(
        f64::from(block.x) + 0.5,
        f64::from(block.y) + 0.5,
        f64::from(block.z) + 0.5,
    );
    let seed = crate::survival::sound_seed();
    play_sound(
        env.out,
        None,
        at,
        "minecraft:item.shears.snip",
        source,
        1.0,
        1.0,
        seed,
    );
    true
}

/// `dropLeash` (`drop_item`) or `removeLeash`.
pub fn drop_leash(ents: &mut Entities, env: &mut Env<'_>, mob: EntityId, drop_item: bool) {
    if ents.leashes.remove(&mob).is_none() {
        return;
    }
    // `Mob.onLeashRemoved`: `clearHome`.
    if let Some(m) = ents.mob_mut(mob) {
        m.restrict_center = BlockPos::ZERO;
        m.restrict_radius = -1;
    }
    link_packet(ents, env, mob, None);
    if drop_item
        && let Some(pos) = ents.store.get(mob).map(|e| e.pos())
        && let Some(lead) = vanillite_data::registry::ITEM.id("minecraft:lead")
    {
        let (uuid, y_rot, velocity, item_random) = crate::mob::new_item_entity();
        env.out.push(Effect::DropItem {
            random: item_random,
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

/// `Equippable.shearingSound` of the items with `canBeSheared` (saddles, harnesses, carpets,
/// horse, wolf and nautilus armour).
fn shearing_sound(name: &str) -> Option<&'static str> {
    Some(match name {
        "minecraft:saddle" => "minecraft:item.saddle.unequip",
        "minecraft:wolf_armor" => "minecraft:item.armor.unequip_wolf",
        n if n.ends_with("_harness") => "minecraft:entity.happy_ghast.unequip",
        n if n.ends_with("_carpet") => "minecraft:item.llama_carpet.unequip",
        n if n.ends_with("_horse_armor") => "minecraft:item.horse_armor.unequip",
        n if n.ends_with("_nautilus_armor") => "minecraft:item.armor.unequip_nautilus",
        _ => return None,
    })
}

/// `Mob.canShearEquipment` and `attemptToShearEquipment`: shears take the first shearable item
/// off (a wolf's only for its owner, others only without passengers), drop it at the passenger
/// attachment, play its shearing sound and post `SHEAR`.
fn shear_equipment(
    ents: &mut Entities,
    env: &mut Env<'_>,
    player: EntityId,
    target: EntityId,
    hand: Hand,
) -> bool {
    let Some(m) = ents.mob(target) else {
        return false;
    };
    let allowed = if m.kind == Kind::Wolf {
        ents.store.get(player).map(|e| e.uuid()) == m.owner
    } else {
        ents.riding.passengers_of(target).is_empty()
    };
    if !allowed {
        return false;
    }
    let creative = ents
        .player(player)
        .is_some_and(|p| p.game_mode == GameMode::Creative);
    let found = (0..8)
        .filter_map(crate::item::EquipmentSlot::from_ordinal)
        .find_map(|slot| {
            let s = m.equipment.get(slot);
            let sound = shearing_sound(crate::item::item_id(s).name())?;
            // `PREVENT_ARMOR_CHANGE` (Curse of Binding) keeps it, unless creative.
            let bound = crate::player::play::game_data()
                .is_some_and(|d| d.enchantment_level(s, "minecraft:binding_curse") > 0);
            (!s.is_empty() && (!bound || creative)).then(|| (slot, s.clone(), sound))
        });
    let Some((slot, stack, sound)) = found else {
        return false;
    };
    let kind = m.kind;
    super::animal::hurt_held(ents, env, player, hand, 1);
    let height = ents.store.get(target).map_or(0.0, |e| e.dims().height);
    if let Some(m) = ents.mob_mut(target) {
        m.equipment.set(slot, ItemStack::default());
        if slot == crate::item::EquipmentSlot::Saddle {
            m.saddled = false;
        }
    }
    let event = crate::level::game_event::events().shear;
    super::animal::entity_game_event(ents, env, target, event, Some(player));
    if let Some(pos) = ents.store.get(target).map(|e| e.pos()) {
        // `getAttachments().getAverage(PASSENGER)`: the type's passenger points, unrotated.
        let offset = match kind {
            Kind::Wolf => Vec3::new(0.0, 0.818_75, -0.0625),
            Kind::Llama | Kind::TraderLlama => Vec3::new(0.0, 1.37, -0.3),
            Kind::Camel | Kind::CamelHusk | Kind::Strider => Vec3::new(0.0, f64::from(height), 0.0),
            _ => Vec3::new(0.0, crate::riding::mob_passenger_height(kind, height), 0.0),
        };
        let (uuid, y_rot, velocity, item_random) = crate::mob::new_item_entity();
        env.out.push(crate::survival::Effect::DropItem {
            random: item_random,
            uuid,
            pos: pos + offset,
            velocity,
            y_rot,
            pickup_delay: 10,
            stack,
        });
    }
    if let Some(update) = super::equipment::tracking_update(ents, target) {
        env.out.push(update);
    }
    if let Some(mut e) = ents.store.get_mut(target) {
        e.mark_data_dirty();
    }
    // `playSound(shearingSound)`: the mob's sound source, its random draws no pitch.
    if let Some(m) = ents.mob(target)
        && !m.silent
        && let Some(pos) = ents.store.get(target).map(|e| e.pos())
    {
        let seed = crate::survival::sound_seed();
        play_sound(
            env.out,
            None,
            pos,
            sound,
            m.kind.sound_source(),
            1.0,
            1.0,
            seed,
        );
    }
    true
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
        .is_some_and(|m| m.is_alive() && can_be_leashed(ents, target, m))
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
            // `gameEvent(ENTITY_ACTION, blockPosition(), Context.of(player))`.
            if let Some(p) = ents.store.get(target).map(|e| e.pos()) {
                let b = BlockPos::containing(p.x, p.y, p.z);
                crate::survival::game_event(
                    env.out,
                    crate::level::game_event::events().entity_action,
                    Vec3::new(
                        f64::from(b.x) + 0.5,
                        f64::from(b.y) + 0.5,
                        f64::from(b.z) + 0.5,
                    ),
                    Some(player),
                );
            }
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
        // `dropAllLeashConnections`.
        let event = crate::level::game_event::events().shear;
        super::animal::entity_game_event(ents, env, target, event, Some(player));
        entity_sound(ents, env, target, "minecraft:item.shears.snip");
        return Interaction::Success;
    }
    if name == "minecraft:shears" && !sneaking && shear_equipment(ents, env, player, target, hand) {
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
        let event = crate::level::game_event::events().entity_interact;
        super::animal::entity_game_event(ents, env, target, event, Some(player));
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

/// `LeadItem.useOn` on a fence and `FenceBlock.useWithoutItem` (`LeadItem.bindPlayerMobs`): the
/// player's leashed mobs around the fence get tied to its knot.
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
    bind_player_mobs(ents, env, level.network_ids(), player, pos)
}

/// `LeadItem.bindPlayerMobs`: whether any mob got tied (`SUCCESS_SERVER`, else `PASS`).
pub fn bind_player_mobs(
    ents: &mut Entities,
    env: &mut Env<'_>,
    network_ids: &NetworkIds,
    player: EntityId,
    pos: BlockPos,
) -> bool {
    let center = Vec3::new(
        f64::from(pos.x) + 0.5,
        f64::from(pos.y) + 0.5,
        f64::from(pos.z) + 0.5,
    );
    // `Leashable.leashableInArea`: a 32 block cube around the block centre.
    let area = Aabb::new(
        center.x - 16.0,
        center.y - 16.0,
        center.z - 16.0,
        center.x + 16.0,
        center.y + 16.0,
        center.z + 16.0,
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
    // `canHaveALeashAttachedTo(knot)`: the knot's box centre within `leashSnapDistance`.
    let knot_center = Vec3::new(
        f64::from(pos.x) + 0.5,
        f64::from(pos.y) + 0.625,
        f64::from(pos.z) + 0.5,
    );
    mine.retain(|&m| {
        let Some(e) = ents.store.get(m) else {
            return false;
        };
        let snap = if e.kind() == vanillite_data::entity::ids::GHAST
            || e.kind() == vanillite_data::entity::ids::HAPPY_GHAST
        {
            16.0
        } else {
            12.0
        };
        let b = e.aabb();
        let c = Vec3::new(
            (b.min_x + b.max_x) / 2.0,
            (b.min_y + b.max_y) / 2.0,
            (b.min_z + b.max_z) / 2.0,
        );
        c.distance_to_sqr(knot_center).sqrt() <= snap
            && ents.mob(m).is_none_or(|mob| can_be_leashed(ents, m, mob))
    });
    // Without a mob to tie, Vanilla discards the knot it just created.
    if mine.is_empty() {
        return false;
    }
    let uuid = crate::mob::new_entity_uuid();
    let knots = ents.knots.len();
    let knot = knot_at(ents, network_ids, pos, uuid);
    if ents.knots.len() > knots && !ents.spawned(knot, vanillite_abi::spawn_reason::DEFAULT, 0) {
        return false;
    }
    for m in mine {
        set_leashed_to(ents, env, m, knot);
    }
    entity_sound(ents, env, knot, "minecraft:item.lead.tied");
    crate::survival::game_event(
        env.out,
        crate::level::game_event::events().block_attach,
        center,
        Some(player),
    );
    true
}

/// `Leashable.leashableLeashedTo(holder)` (a 32 block cube around the holder's box centre)
/// filtered by `canHaveALeashAttachedTo(other)`: within the snap distance of `other` and
/// leashable.
fn leashed_to_attachable(ents: &Entities, holder: EntityId, other: EntityId) -> Vec<EntityId> {
    let center = |id: EntityId| {
        ents.store.get(id).map(|e| {
            let b = e.aabb();
            Vec3::new(
                (b.min_x + b.max_x) / 2.0,
                (b.min_y + b.max_y) / 2.0,
                (b.min_z + b.max_z) / 2.0,
            )
        })
    };
    let (Some(hc), Some(oc)) = (center(holder), center(other)) else {
        return Vec::new();
    };
    let area = Aabb::new(
        hc.x - 16.0,
        hc.y - 16.0,
        hc.z - 16.0,
        hc.x + 16.0,
        hc.y + 16.0,
        hc.z + 16.0,
    );
    let mut out = Vec::new();
    ents.store.for_each_in(&area, false, |id| {
        if ents.leashes.get(&id) == Some(&holder) {
            out.push(id);
        }
    });
    out.retain(|&m| {
        if m == other {
            return false;
        }
        let snap = match ents.store.get(m).map(|e| e.kind()) {
            Some(k)
                if k == vanillite_data::entity::ids::GHAST
                    || k == vanillite_data::entity::ids::HAPPY_GHAST =>
            {
                16.0
            }
            _ => 12.0,
        };
        center(m).is_some_and(|c| c.distance_to_sqr(oc).sqrt() <= snap)
            && ents.mob(m).is_none_or(|mob| can_be_leashed(ents, m, mob))
    });
    out
}

/// `LeashFenceKnotEntity.interact`: shears cut the knot's leads; the player's own leashed mobs
/// get tied to the knot, else (not sneaking) the knot's mobs go onto the player's lead
/// (`BLOCK_ATTACH`, the tie sound). `Pass` when nothing changed; an emptied knot is discarded
/// (`notifyLeasheeRemoved`).
pub fn knot_interact(
    ents: &mut Entities,
    env: &mut Env<'_>,
    discarded: &mut Vec<EntityId>,
    player: EntityId,
    knot: EntityId,
    hand: Hand,
    sneaking: bool,
) -> Interaction {
    let stack = super::animal::held(ents, player, hand);
    if vanillite_data::registry::ITEM.name(stack.item) == Some("minecraft:shears")
        && shear_off_all_leash_connections(ents, env, knot)
    {
        super::animal::hurt_held(ents, env, player, hand, 1);
        discard_if_empty(ents, knot, discarded);
        return Interaction::Success;
    }
    let mine = leashed_to_attachable(ents, player, knot);
    let attached = !mine.is_empty();
    for m in mine {
        set_leashed_to(ents, env, m, knot);
    }
    let mut moved = false;
    if !attached && !sneaking {
        for m in leashed_to_attachable(ents, knot, player) {
            set_leashed_to(ents, env, m, player);
            moved = true;
        }
    }
    if !attached && !moved {
        return Interaction::Pass;
    }
    if let Some(pos) = ents.store.get(knot).map(|e| e.pos()) {
        let event = crate::level::game_event::events().block_attach;
        crate::survival::game_event(env.out, event, pos, Some(player));
    }
    entity_sound(ents, env, knot, "minecraft:item.lead.tied");
    discard_if_empty(ents, knot, discarded);
    Interaction::Success
}

/// `LeashFenceKnotEntity.notifyLeasheeRemoved`: a knot nothing is tied to goes.
fn discard_if_empty(ents: &Entities, knot: EntityId, discarded: &mut Vec<EntityId>) {
    if !ents.leashes.values().any(|&h| h == knot) {
        discarded.push(knot);
    }
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
    // `PathfinderMob.whenLeashedTo`: home at the holder, `leashElasticDistance - 1` around it.
    {
        let m = t.mob();
        m.restrict_center = BlockPos::containing(hpos.x, hpos.y, hpos.z);
        m.restrict_radius = 5;
    }
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
