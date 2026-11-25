//! Allays (`Allay`): they hold an item a player gave them, collect matching items into their
//! one slot inventory and bring them to the player (or a liked note block), heal over time,
//! dance and duplicate with amethyst shards. Their brain is in [`super::ai`].

use vanillite_base::{BlockPos, SectionPos, Vec3};
use vanillite_nbt::{Compound, List, Tag};
use vanillite_world::entity::EntityId;

use super::State;
use crate::block::sculk_vibration::{self, Listenable, Vibration, VibrationData};
use crate::entities::Entities;
use crate::item::{EquipmentSlot, ItemStack};
use crate::level::Level;
use crate::level::game_event::{EntityListener, GameEventContext, Listener, events};
use crate::mob::animal::{self, Interaction};
use crate::mob::boss::warden::LevelAction;
use crate::mob::fam::{Hurt, Interact};
use crate::mob::nav::{MoveKind, NavKind};
use crate::mob::{Kind, MobData, Tick};
use crate::player::encode::DataWriter;
use crate::survival::Effect;
use crate::survival::player::Env;
use crate::survival::rules::Rule;

/// `VibrationUser.VIBRATION_EVENT_LISTENER_RANGE`.
const VIBRATION_RANGE: i32 = 16;
/// `DUPLICATION_COOLDOWN_TICKS`.
const DUPLICATION_COOLDOWN_TICKS: i64 = 6000;
/// `GameEvent.JUKEBOX_PLAY.notificationRadius`.
const JUKEBOX_RADIUS: f64 = 10.0;

/// The allay part of a mob.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Allay {
    /// The one slot inventory (`SimpleContainer(1)`).
    pub inventory: ItemStack,
    /// `duplicationCooldown`.
    pub duplication_cooldown: i64,
    /// `DATA_DANCING`.
    pub dancing: bool,
    pub jukebox_pos: Option<BlockPos>,
    /// `vibrationData`.
    pub vibration: VibrationData,
    /// The section its two game event listeners are registered in.
    pub listener_section: Option<SectionPos>,
}

/// The allay state of a mob.
pub fn allay(m: &MobData) -> Allay {
    match super::state(m) {
        State::Allay(a) => a.clone(),
        _ => Allay::default(),
    }
}

/// The allay state of a mob, mutably.
pub fn allay_mut(m: &mut MobData) -> &mut Allay {
    let s = super::state_mut(m);
    if !matches!(s, State::Allay(_)) {
        *s = State::Allay(Allay::default());
    }
    match s {
        State::Allay(a) => a,
        _ => unreachable!("set above"),
    }
}

pub fn init(m: &mut MobData) {
    allay_mut(m);
    // `FlyingMoveControl(this, 20, true)`, flying navigation without doors, floating.
    m.move_ctl.kind = MoveKind::Flying {
        max_turn: 20,
        hovers: true,
    };
    m.nav.kind = NavKind::Flying;
    m.nav.can_open_doors = false;
    m.nav.can_float = true;
    super::ai::allay_brain(m);
}

pub fn ambient_sound(m: &MobData) -> &'static str {
    if m.equipment.get(EquipmentSlot::MainHand).is_empty() {
        "minecraft:entity.allay.ambient_without_item"
    } else {
        "minecraft:entity.allay.ambient_with_item"
    }
}

/// `Allay.travel`: `travelFlying(input, getSpeed())`.
pub fn travel(t: &mut Tick<'_, '_>, input: Vec3) -> bool {
    let speed = t.mob_ref().speed;
    super::ghast::fly(t, input, speed);
    true
}

/// `Allay.aiStep`: heals every 10 ticks, stops dancing away from the jukebox, counts the
/// duplication cooldown down.
pub fn ai_step(t: &mut Tick<'_, '_>) {
    let m = t.mob_ref();
    if m.is_alive() && m.tick_count % 10 == 0 && m.health < m.max_health() {
        let m = t.mob();
        m.health = (m.health + 1.0).min(m.max_health());
        t.core_mut().mark_data_dirty();
    }
    let a = allay(t.mob_ref());
    if a.dancing && should_stop_dancing(t, &a) && t.mob_ref().tick_count % 20 == 0 {
        set_dancing(t.mob(), false);
        allay_mut(t.mob()).jukebox_pos = None;
        t.core_mut().mark_data_dirty();
    }
    let a = allay_mut(t.mob());
    if a.duplication_cooldown > 0 {
        a.duplication_cooldown -= 1;
        if a.duplication_cooldown == 0 {
            t.core_mut().mark_data_dirty();
        }
    }
}

/// `Allay.tick` after `super.tick()`: the vibration ticker, a panicking allay stops dancing;
/// then its listeners follow it (`updateDynamicGameEventListener` on a section change).
pub fn after_tick(t: &mut Tick<'_, '_>) {
    tick_vibrations(t);
    if crate::mob::animal::is_panicking(t.mob_ref()) && allay(t.mob_ref()).dancing {
        set_dancing(t.mob(), false);
        t.core_mut().mark_data_dirty();
    }
    let p = t.pos();
    let eye = crate::mob::eye_height(t.mob_ref());
    let section = BlockPos::containing(p.x, p.y + eye, p.z).section();
    let id = t.id;
    let a = allay_mut(t.mob());
    if a.listener_section == Some(section) {
        return;
    }
    let old = a.listener_section.replace(section);
    for (kind, radius) in [
        (EntityListener::Vibration, VIBRATION_RANGE),
        (EntityListener::Jukebox, JUKEBOX_RADIUS as i32),
    ] {
        let listener = |section| Listener::Entity {
            id,
            section,
            radius,
            kind,
        };
        t.cx.actions
            .push(crate::mob::WorldAction::Level(LevelAction::Listener {
                remove: old.map(listener),
                add: Some(listener(section)),
            }));
    }
}

/// `VibrationSystem.Ticker.tick` of the allay: a heard note block becomes its liked one.
fn tick_vibrations(t: &mut Tick<'_, '_>) {
    let time = t.game_time();
    let a = allay_mut(t.mob());
    a.vibration.try_select(time);
    let Some(vibration) = a.vibration.current else {
        return;
    };
    a.vibration.travel_time = (a.vibration.travel_time - 1).max(0);
    if a.vibration.travel_time > 0 {
        return;
    }
    // `receiveVibration` (no adjacent chunk requirement) and `onReceiveVibration`.
    a.vibration.current = None;
    if vibration.event == events().note_block_play {
        let origin = crate::level::game_event::containing(vibration.pos);
        let dimension = t.env.dimension;
        super::ai::hear_noteblock(t.mob(), origin, dimension);
    }
}

/// The allay's game event listeners (from the level's dispatch): its vibration listener
/// (`VibrationSystem.Listener.handleGameEvent` with `Allay.VibrationUser`) and its
/// `JukeboxListener`.
pub(crate) fn handle_game_event(
    level: &mut Level,
    id: EntityId,
    kind: EntityListener,
    event: u16,
    context: GameEventContext,
    source: Vec3,
) {
    if kind == EntityListener::Jukebox {
        let e = events();
        let playing = if event == e.jukebox_play {
            true
        } else if event == e.jukebox_stop_play {
            false
        } else {
            return;
        };
        let pos = crate::level::game_event::containing(source);
        if let Some(m) = level.entities.mob_mut(id) {
            let before = allay(m).dancing;
            set_jukebox_playing(m, pos, playing);
            if allay(m).dancing != before
                && let Some(mut e) = level.entities.store.get_mut(id)
            {
                e.mark_data_dirty();
            }
        }
        return;
    }
    let Some(busy) = level
        .entities
        .mob(id)
        .map(|m| allay(m).vibration.current.is_some())
    else {
        return;
    };
    if busy || !sculk_vibration::is_valid_vibration(level, event, context, Listenable::Allay) {
        return;
    }
    let Some(destination) = level.listener_position(Listener::Entity {
        id,
        section: BlockPos::ZERO.section(),
        radius: VIBRATION_RANGE,
        kind,
    }) else {
        return;
    };
    // `canReceiveVibration`: not without AI; with a liked note block only that one within
    // 1024 blocks (chessboard distance).
    let Some(m) = level.entities.mob(id) else {
        return;
    };
    if m.no_ai {
        return;
    }
    if let Some(liked) = m
        .brain
        .as_deref()
        .and_then(|b| b.pos(crate::mob::brain::MemoryType::LikedNoteblockPosition))
    {
        let here = level
            .entities
            .store
            .get(id)
            .map(|e| {
                let p = e.pos();
                BlockPos::containing(p.x, p.y, p.z)
            })
            .unwrap_or(liked);
        let chessboard = (liked.x - here.x)
            .abs()
            .max((liked.y - here.y).abs())
            .max((liked.z - here.z).abs());
        if chessboard > 1024 || liked != crate::level::game_event::containing(source) {
            return;
        }
    }
    if sculk_vibration::is_occluded(level, source, destination) {
        return;
    }
    let time = level.game_time();
    let projectile_owner = sculk_vibration::projectile_owner(level, context.source);
    let distance = source.distance_to_sqr(destination).sqrt() as f32;
    if let Some(m) = level.entities.mob_mut(id) {
        allay_mut(m).vibration.add_candidate(
            Vibration {
                event,
                distance,
                pos: source,
                source: context.source,
                projectile_owner,
            },
            time,
        );
    }
}

/// `setDancing(dancing)`: a panicking allay does not start dancing.
fn set_dancing(m: &mut MobData, dancing: bool) {
    if m.no_ai || (dancing && crate::mob::animal::is_panicking(m)) {
        return;
    }
    allay_mut(m).dancing = dancing;
}

/// `shouldStopDancing`: the jukebox is gone or too far.
fn should_stop_dancing(t: &Tick<'_, '_>, a: &Allay) -> bool {
    let Some(j) = a.jukebox_pos else {
        return true;
    };
    let center = Vec3::new(
        f64::from(j.x) + 0.5,
        f64::from(j.y) + 0.5,
        f64::from(j.z) + 0.5,
    );
    center.distance_to_sqr(t.pos()) >= JUKEBOX_RADIUS * JUKEBOX_RADIUS
        || t.level.block(j).block().name() != "minecraft:jukebox"
}

/// `setJukeboxPlaying(pos, playing)` (the jukebox listener).
pub fn set_jukebox_playing(m: &mut MobData, pos: BlockPos, playing: bool) {
    let a = allay_mut(m);
    if playing {
        if !a.dancing {
            a.jukebox_pos = Some(pos);
            set_dancing(m, true);
        }
    } else if a.jukebox_pos == Some(pos) || a.jukebox_pos.is_none() {
        a.jukebox_pos = None;
        set_dancing(m, false);
    }
}

/// `customServerAiStep`: the brain, the pickup and note block cooldowns.
pub fn custom_server_ai_step(t: &mut Tick<'_, '_>) {
    super::ai::tick(t);
}

/// `hurtServer`: the liked player cannot hurt it.
pub fn hurt_filter(h: &mut Hurt<'_, '_>, amount: f32) -> Option<f32> {
    let liked = h.ents.mob(h.target).and_then(super::ai::liked_player);
    let cause = h
        .source
        .cause
        .and_then(|c| h.ents.store.get(c))
        .map(|e| e.uuid());
    if liked.is_some() && liked == cause {
        return None;
    }
    Some(amount)
}

/// `dropEquipment`: the inventory and the held item drop.
pub fn on_die(h: &mut Hurt<'_, '_>) {
    let Some(pos) = h.ents.store.get(h.target).map(|e| e.pos()) else {
        return;
    };
    let Some(m) = h.ents.mob_mut(h.target) else {
        return;
    };
    let inv = std::mem::take(&mut allay_mut(m).inventory);
    let held = m.equipment.get(EquipmentSlot::MainHand).clone();
    m.equipment
        .set(EquipmentSlot::MainHand, ItemStack::default());
    // The held item is guaranteed (`dropEquipment`), not by chance.
    m.equipment.set_drop_chance(EquipmentSlot::MainHand, 0.0);
    for stack in [inv, held] {
        drop_at(h.env, pos, stack);
    }
}

fn drop_at(env: &mut Env<'_>, pos: Vec3, stack: ItemStack) {
    if stack.is_empty() {
        return;
    }
    let (uuid, y_rot, velocity) = crate::mob::new_item_entity();
    env.out.push(Effect::DropItem {
        uuid,
        pos,
        velocity,
        y_rot,
        pickup_delay: 10,
        stack,
    });
}

/// `BehaviorUtils.throwItem(thrower, item, target, velocity, handYDistanceFromEye)`.
pub fn throw_item(
    ents: &Entities,
    env: &mut Env<'_>,
    thrower: EntityId,
    stack: ItemStack,
    target: Vec3,
    velocity: Vec3,
    hand_y: f32,
) {
    if stack.is_empty() {
        return;
    }
    let Some(e) = ents.store.get(thrower) else {
        return;
    };
    let pos = e.pos();
    let eye = ents.mob(thrower).map_or(0.0, crate::mob::eye_height);
    let from = Vec3::new(pos.x, pos.y + eye - f64::from(hand_y), pos.z);
    // `new ItemEntity(...)`: its random motion is replaced by the throw.
    let dir = (target - pos).normalize();
    let v = Vec3::new(dir.x * velocity.x, dir.y * velocity.y, dir.z * velocity.z);
    let (uuid, y_rot, _) = crate::mob::new_item_entity();
    env.out.push(Effect::DropItem {
        uuid,
        pos: from,
        velocity: v,
        y_rot,
        pickup_delay: 10,
        stack,
    });
}

/// `allayConsidersItemEqual`: the same item and the same potion contents.
pub fn considers_equal(a: &ItemStack, b: &ItemStack) -> bool {
    let id = vanillite_protocol::components::ids::POTION_CONTENTS;
    a.item == b.item && a.component(id) == b.component(id)
}

/// `wantsToPickUp`: holding an item, mob griefing, room in the inventory, the same item.
pub fn wants_to_pick_up(m: &MobData, rules: bool, stack: &ItemStack) -> bool {
    let held = m.equipment.get(EquipmentSlot::MainHand);
    if held.is_empty() || !rules || !considers_equal(held, stack) {
        return false;
    }
    let inv = &allay(m).inventory;
    inv.is_empty()
        || (crate::item::matches(inv, stack) && inv.count < crate::item::max_stack_size(inv))
}

/// `Mob.aiStep`'s looting with `Allay.canPickUpLoot` (no pickup cooldown, an item in hand),
/// reach 1, 1, 1 and `InventoryCarrier.pickUpItem`.
pub fn pick_up_items(t: &mut Tick<'_, '_>) {
    let m = t.mob_ref();
    if super::ai::on_pickup_cooldown(m)
        || m.equipment.get(EquipmentSlot::MainHand).is_empty()
        || !m.is_alive()
        || m.dead
    {
        return;
    }
    let griefing = t.env.rules.bool(Rule::MOB_GRIEFING);
    if !griefing {
        return;
    }
    let area = t.aabb().inflate(1.0, 1.0, 1.0);
    let mut found = std::mem::take(&mut t.cx.ids);
    found.clear();
    t.ents.store.for_each_in(&area, false, |id| found.push(id));
    for &id in &found {
        let Some(data) = t.ents.item(id) else {
            continue;
        };
        if data.stack.is_empty() || data.pickup_delay > 0 || t.cx.discarded.contains(&id) {
            continue;
        }
        let stack = data.stack.clone();
        if !wants_to_pick_up(t.mob_ref(), griefing, &stack) {
            continue;
        }
        // `SimpleContainer.addItem`.
        let a = allay_mut(t.mob());
        let added = if a.inventory.is_empty() {
            a.inventory = stack.clone();
            stack.count
        } else {
            let max = crate::item::max_stack_size(&a.inventory);
            let n = (max - a.inventory.count).min(stack.count).max(0);
            a.inventory.count += n;
            n
        };
        if added > 0 {
            crate::mob::equipment::take(t, id, added);
        }
    }
    t.cx.ids = found;
}

/// `Allay.mobInteract`: duplicating while dancing, taking a given item, giving it back.
pub fn interact(ix: &mut Interact<'_, '_>) -> Interaction {
    let Some(m) = ix.ents.mob(ix.target) else {
        return Interaction::Pass;
    };
    let a = allay(m);
    let held = m.equipment.get(EquipmentSlot::MainHand).clone();
    let duplicates = crate::player::play::game_data()
        .is_some_and(|d| d.item_in_tag(ix.stack.item, "minecraft:duplicates_allays"));
    if a.dancing && duplicates && a.duplication_cooldown == 0 {
        duplicate(ix);
        animal::entity_event(ix.ents, ix.env, ix.target, 18);
        sound_for_player(ix, "minecraft:block.amethyst_block.chime");
        animal::use_item(ix.ents, ix.player, ix.hand);
        return Interaction::Success;
    }
    if held.is_empty() && !ix.stack.is_empty() {
        let uuid = ix.ents.store.get(ix.player).map(|e| e.uuid());
        if let Some(m) = ix.ents.mob_mut(ix.target) {
            m.equipment
                .set(EquipmentSlot::MainHand, ix.stack.with_count(1));
            super::ai::set_liked_player(m, uuid);
        }
        animal::use_item(ix.ents, ix.player, ix.hand);
        sound_for_player(ix, "minecraft:entity.allay.item_given");
        broadcast_equipment(ix.ents, ix.env, ix.target);
        return Interaction::Success;
    }
    let main = matches!(
        ix.hand,
        vanillite_protocol::packets::play::serverbound::Hand::MainHand
    );
    if !held.is_empty() && main && ix.stack.is_empty() {
        let player_pos = ix.ents.store.get(ix.player).map(|e| e.pos());
        let inv = ix.ents.mob_mut(ix.target).map(|m| {
            m.equipment
                .set(EquipmentSlot::MainHand, ItemStack::default());
            super::ai::set_liked_player(m, None);
            std::mem::take(&mut allay_mut(m).inventory)
        });
        sound_for_player(ix, "minecraft:entity.allay.item_taken");
        if let (Some(inv), Some(p)) = (inv, player_pos) {
            throw_item(
                ix.ents,
                ix.env,
                ix.target,
                inv,
                p,
                Vec3::new(0.3, 0.3, 0.3),
                0.3,
            );
        }
        // `player.addItem(itemInHand)`.
        let mut give = held;
        if let Some(p) = ix.ents.player_mut(ix.player)
            && !p.inventory.add(None, &mut give, false)
            && let Some(pos) = player_pos
        {
            drop_at(ix.env, pos, give);
        }
        broadcast_equipment(ix.ents, ix.env, ix.target);
        return Interaction::Success;
    }
    Interaction::Pass
}

/// `level.playSound(player, this, sound, NEUTRAL, 2, 1)`.
fn sound_for_player(ix: &mut Interact<'_, '_>, sound: &str) {
    let Some(pos) = ix.ents.store.get(ix.target).map(|e| e.pos()) else {
        return;
    };
    let seed = crate::survival::sound_seed();
    crate::survival::play_sound(
        ix.env.out,
        Some(ix.player),
        pos,
        sound,
        vanillite_protocol::packets::play::clientbound::SoundSource::Neutral,
        2.0,
        1.0,
        seed,
    );
}

/// Sends the allay's equipment after an interaction changed it.
fn broadcast_equipment(ents: &Entities, env: &mut Env<'_>, id: EntityId) {
    let (Some(e), Some(m)) = (ents.store.get(id), ents.mob(id)) else {
        return;
    };
    let slots = crate::mob::equipment::SLOTS
        .iter()
        .map(|&s| (s.ordinal(), m.equipment.get(s).clone()))
        .collect();
    env.out.push(Effect::Tracking {
        entity: id,
        with_self: false,
        packet: vanillite_protocol::SharedPacket::of(
            &vanillite_protocol::packets::play::clientbound::SetEquipment {
                entity_id: e.network_id().0,
                slots,
            },
        ),
    });
}

/// `duplicateAllay`: a new persistent allay at the same place; both wait 5 minutes.
fn duplicate(ix: &mut Interact<'_, '_>) {
    let Some(pos) = ix.ents.store.get(ix.target).map(|e| e.pos()) else {
        return;
    };
    let seed = crate::mob::new_entity_seed();
    let mut child = MobData::new(Kind::Allay, seed);
    child.persistent = true;
    allay_mut(&mut child).duplication_cooldown = DUPLICATION_COOLDOWN_TICKS;
    if let Some(m) = ix.ents.mob_mut(ix.target) {
        allay_mut(m).duplication_cooldown = DUPLICATION_COOLDOWN_TICKS;
    }
    if let Some(mut e) = ix.ents.store.get_mut(ix.target) {
        e.mark_data_dirty();
    }
    let uuid = crate::mob::new_uuid();
    crate::mob::spawn(
        ix.ents,
        ix.level.network_ids(),
        uuid,
        pos,
        vanillite_world::entity::Rotation::default(),
        child,
    );
}

/// The synched data: dancing 16, can duplicate 17.
pub fn encode(w: &mut DataWriter<'_>, m: &MobData) {
    let a = allay(m);
    w.boolean(16, a.dancing, false);
    w.boolean(17, a.duplication_cooldown == 0, true);
}

pub fn save(m: &MobData, out: &mut Compound) {
    let a = allay(m);
    // `InventoryCarrier.writeInventoryToTag`.
    let list = if a.inventory.is_empty() {
        Vec::new()
    } else {
        let mut c = crate::inventory::stack_to_nbt(&a.inventory);
        c.insert("Slot", 0_i8);
        vec![Tag::Compound(c)]
    };
    out.insert("Inventory", List(list));
    out.insert("listener", Tag::Compound(a.vibration.save(|_| None)));
    out.insert("DuplicationCooldown", a.duplication_cooldown);
    super::ai::save_brain(m, out);
}

pub fn load(m: &mut MobData, tag: &Compound) {
    let inv = match tag.get("Inventory") {
        Some(Tag::List(l)) => l.0.iter().find_map(|t| match t {
            Tag::Compound(c) => Some(crate::inventory::stack_from_nbt(c)),
            _ => None,
        }),
        _ => None,
    };
    let cooldown = super::long(tag, "DuplicationCooldown").unwrap_or(0);
    let a = allay_mut(m);
    if let Some(Tag::Compound(l)) = tag.get("listener") {
        a.vibration = VibrationData::load(l, |_| None);
    }
    a.inventory = inv.unwrap_or_default();
    a.duplication_cooldown = cooldown;
    super::ai::load_brain(m, tag);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack(name: &str, n: i32) -> ItemStack {
        ItemStack::new(vanillite_data::registry::ITEM.id(name).expect("item"), n)
    }

    #[test]
    fn wants_only_its_item() {
        let mut m = MobData::new(Kind::Allay, 1);
        assert!(!wants_to_pick_up(&m, true, &stack("minecraft:stone", 1)));
        m.equipment
            .set(EquipmentSlot::MainHand, stack("minecraft:stone", 1));
        assert!(wants_to_pick_up(&m, true, &stack("minecraft:stone", 3)));
        assert!(!wants_to_pick_up(&m, true, &stack("minecraft:dirt", 3)));
        assert!(!wants_to_pick_up(&m, false, &stack("minecraft:stone", 3)));
    }

    #[test]
    fn inventory_round_trip() {
        let mut m = MobData::new(Kind::Allay, 1);
        allay_mut(&mut m).inventory = stack("minecraft:stone", 12);
        allay_mut(&mut m).duplication_cooldown = 55;
        let mut out = Compound::new();
        save(&m, &mut out);
        let mut n = MobData::new(Kind::Allay, 2);
        load(&mut n, &out);
        assert_eq!(allay(&n).inventory, allay(&m).inventory);
        assert_eq!(allay(&n).duplication_cooldown, 55);
    }
}
