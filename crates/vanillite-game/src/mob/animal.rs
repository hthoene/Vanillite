//! Players interacting with mobs (`Player.interactOn`, `Mob.interact`, `mobInteract`) and the
//! animal life cycle around it: feeding, love mode and breeding (`Animal`, `BreedGoal`),
//! babies growing up, shearing, milking, saddles and riding pigs, spawn eggs, name tags and
//! leads.

use vanillite_base::Vec3;
use vanillite_base::random::RandomSource;
use vanillite_config::GameMode;
use vanillite_protocol::SharedPacket;
use vanillite_protocol::packets::play::clientbound::{EntityEvent, SoundSource};
use vanillite_protocol::packets::play::serverbound::Hand;
use vanillite_world::entity::{EntityId, NetworkIds, Rotation};

use super::{Kind, MobContext, MobData, combat, kinds};
use crate::entities::Entities;
use crate::inventory::OFFHAND;
use crate::item::{self, ItemStack};
use crate::level::Level;
use crate::survival::player::Env;
use crate::survival::rules::Rule;
use crate::survival::{Effect, play_sound};

/// `InteractionResult`, as far as the server acts on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interaction {
    Pass,
    /// Handled; the player swings its arm.
    Success,
    /// Handled without a swing.
    Consume,
}

impl Interaction {
    /// `consumesAction`.
    pub fn consumes(self) -> bool {
        self != Interaction::Pass
    }
}

/// The breeding food item tag of an animal kind (`isFood`).
pub fn food_tag(kind: Kind) -> Option<&'static str> {
    Some(match kind {
        Kind::Cow => "minecraft:cow_food",
        Kind::Pig => "minecraft:pig_food",
        Kind::Sheep => "minecraft:sheep_food",
        Kind::Chicken => "minecraft:chicken_food",
        _ => return None,
    })
}

/// `Animal.isFood(item)`.
pub fn is_food(kind: Kind, item: u16) -> bool {
    if kind.family() != super::Family::Classic {
        return super::fam::is_food(kind, item);
    }
    food_tag(kind).is_some_and(|tag| {
        crate::player::play::game_data().is_some_and(|d| d.item_in_tag(item, tag))
    })
}

/// `isPanicking`: a panic goal runs.
pub fn is_panicking(m: &MobData) -> bool {
    m.goals
        .slots
        .iter()
        .any(|s| s.running && matches!(s.goal, super::goals::Goal::Panic { .. }))
}

/// `BreedGoal.getFreePartner`: the closest animal of the same type in love within 8 blocks.
pub fn free_partner(t: &mut super::Tick<'_, '_>) -> Option<EntityId> {
    let kind = t.mob_ref().kind;
    let area = t.aabb().inflate(8.0, 8.0, 8.0);
    let pos = t.pos();
    let mut ids = std::mem::take(&mut t.cx.ids);
    ids.clear();
    t.ents.store.for_each_in(&area, false, |id| ids.push(id));
    let mut best: Option<(EntityId, f64)> = None;
    for &id in &ids {
        if id == t.id {
            continue;
        }
        let Some(m) = t.ents.mob(id) else {
            continue;
        };
        if !m.is_alive() {
            continue;
        }
        if kind.family() == super::Family::Classic {
            if m.kind != kind || m.in_love <= 0 {
                continue;
            }
        } else if !super::fam::can_mate(t.mob_ref(), m) {
            continue;
        }
        let Some(e) = t.ents.store.get(id) else {
            continue;
        };
        let d = e.pos().distance_to_sqr(pos);
        if d > 64.0 {
            continue;
        }
        if best.is_none_or(|(_, b)| d < b) {
            best = Some((id, d));
        }
    }
    t.cx.ids = ids;
    best.map(|(id, _)| id)
}

/// `level.broadcastEntityEvent(entity, event)`.
pub fn entity_event(ents: &Entities, env: &mut Env<'_>, id: EntityId, event: i8) {
    if let Some(e) = ents.store.get(id) {
        env.out.push(Effect::Tracking {
            entity: id,
            with_self: false,
            packet: SharedPacket::of(&EntityEvent {
                entity_id: e.network_id().0,
                event,
            }),
        });
    }
}

/// A sound at an entity's position (`playSound(sound, volume, pitch)` of the entity).
pub fn sound_at(
    ents: &Entities,
    env: &mut Env<'_>,
    id: EntityId,
    sound: &str,
    source: SoundSource,
    pitch: f32,
) {
    let Some(pos) = ents.store.get(id).map(|e| e.pos()) else {
        return;
    };
    let seed = crate::survival::sound_seed();
    play_sound(env.out, None, pos, sound, source, 1.0, pitch, seed);
}

/// `Animal.spawnChildFromBreeding` and `finalizeSpawnChildFromBreeding`.
pub fn breed(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    parent: EntityId,
    partner: EntityId,
) {
    let (Some(a), Some(b)) = (ents.mob(parent), ents.mob(partner)) else {
        return;
    };
    let kind = a.kind;
    let (va, vb, ca, cb) = (a.variant, b.variant, a.sheep_color, b.sheep_color);
    let cause = a.love_cause.or(b.love_cause);
    let Some(pos) = ents.store.get(parent).map(|e| e.pos()) else {
        return;
    };
    let seed = crate::mob::new_entity_seed();
    let child_kind = match (ents.mob(parent), ents.mob(partner)) {
        (Some(a), Some(b)) if kind.family() != super::Family::Classic => {
            super::fam::offspring_kind(a, b)
        }
        _ => kind,
    };
    let mut child = MobData::new(child_kind, seed);
    let pick = ents
        .mob_mut(parent)
        .map(|m| m.random.next_boolean())
        .unwrap_or(true);
    child.variant = if pick { va } else { vb };
    if kind == Kind::Sheep {
        child.sheep_color = offspring_color(ca, cb, pick);
    }
    if kind.family() != super::Family::Classic
        && let (Some(a), Some(b)) = (ents.mob(parent), ents.mob(partner))
    {
        let mut random = vanillite_base::random::LegacyRandom::new(env.random.next_long());
        super::fam::breed_offspring(a, b, &mut child, &mut random);
    }
    child.age = -24000;
    child.baby = true;
    let uuid = crate::mob::new_uuid();
    let child = super::spawn(
        ents,
        level.network_ids(),
        uuid,
        pos,
        Rotation::default(),
        child,
    );
    award_breeding(ents, parent, partner, Some(child));
    for id in [parent, partner] {
        if let Some(m) = ents.mob_mut(id) {
            m.age = 6000;
            m.in_love = 0;
            m.love_cause = None;
        }
    }
    let _ = cause;
    entity_event(ents, env, parent, 18);
    if env.rules.bool(Rule::MOB_DROPS) {
        let xp = ents
            .mob_mut(parent)
            .map_or(1, |m| m.random.next_int_bounded(7) + 1);
        env.out.push(Effect::Experience { pos, amount: xp });
    }
}

/// The player of a UUID in this world (`level.getPlayerByUUID`).
pub fn player_by_uuid(ents: &Entities, uuid: Option<u128>) -> Option<EntityId> {
    uuid.and_then(|u| ents.players_by_uuid.get(&u).copied())
}

/// The award of `finalizeSpawnChildFromBreeding` (and the turtle's and fox's breed goals):
/// the love cause of `parent`, else of `partner`, gets `animals_bred` and `bred_animals`.
/// Called before the love is reset.
pub fn award_breeding(
    ents: &mut Entities,
    parent: EntityId,
    partner: EntityId,
    child: Option<EntityId>,
) {
    use crate::advancements::trigger::{Event, TriggerKind};
    use crate::advancements::view;
    let cause = ents
        .mob(parent)
        .and_then(|m| m.love_cause)
        .and_then(|u| player_by_uuid(ents, Some(u)))
        .or_else(|| player_by_uuid(ents, ents.mob(partner).and_then(|m| m.love_cause)));
    let Some(player) = cause else {
        return;
    };
    if let Some(p) = ents.player_mut(player) {
        p.stats.award(crate::stats::custom::ANIMALS_BRED, 1);
    }
    if !view::listens(ents, player, TriggerKind::BRED_ANIMALS) {
        return;
    }
    if let (Some(a), Some(b)) = (view::snapshot(ents, parent), view::snapshot(ents, partner)) {
        let c = child.and_then(|c| view::snapshot(ents, c));
        if let Some(p) = ents.player_mut(player) {
            p.advancements.trigger(Event::BredAnimals(a, b, c));
        }
    }
}

/// `FILLED_BUCKET.trigger(player, bucket)`.
pub fn filled_bucket(ents: &mut Entities, player: EntityId, bucket: &ItemStack) {
    if let Some(p) = ents.player_mut(player) {
        p.advancements.trigger_item(
            crate::advancements::trigger::TriggerKind::FILLED_BUCKET,
            bucket,
        );
    }
}

/// The entity of a UUID in this world, players first (`level.getEntity(uuid)`).
pub fn entity_by_uuid(ents: &Entities, uuid: u128) -> Option<EntityId> {
    ents.players_by_uuid.get(&uuid).copied().or_else(|| {
        ents.mobs
            .iter()
            .map(|(id, _)| id)
            .find(|&id| ents.store.get(id).is_some_and(|e| e.uuid() == uuid))
    })
}

/// `onItemPickup(itemEntity)` of `picker` for an item entity with an owner (`thrower`):
/// `thrown_item_picked_up_by_entity` for a throwing player and, when a player picks it up,
/// `thrown_item_picked_up_by_player` (`ServerPlayer.onItemPickup`). `stack` is the item
/// entity's stack after the pickup (`getItem()`).
pub fn item_picked_up(ents: &mut Entities, picker: EntityId, thrower: u128, stack: &ItemStack) {
    use crate::advancements::trigger::{Event, TriggerKind};
    use crate::advancements::view;
    let by_entity = TriggerKind::THROWN_ITEM_PICKED_UP_BY_ENTITY;
    if let Some(owner) = ents.players_by_uuid.get(&thrower).copied() {
        view::fire(ents, owner, by_entity, |ents| {
            Event::EntityItem(by_entity, view::snapshot(ents, picker), stack.clone())
        });
    }
    let by_player = TriggerKind::THROWN_ITEM_PICKED_UP_BY_PLAYER;
    if ents.player(picker).is_some()
        && view::listens(ents, picker, by_player)
        && let Some(owner) = entity_by_uuid(ents, thrower)
    {
        view::fire(ents, picker, by_player, |ents| {
            Event::EntityItem(by_player, view::snapshot(ents, owner), stack.clone())
        });
    }
}

/// `SUMMONED_ENTITY.trigger(player, entity)` for each of `players`.
pub fn award_summoned(ents: &mut Entities, players: &[EntityId], entity: EntityId) {
    use crate::advancements::trigger::{Event, TriggerKind};
    use crate::advancements::view;
    let kind = TriggerKind::SUMMONED_ENTITY;
    let listening: Vec<EntityId> = players
        .iter()
        .copied()
        .filter(|&p| view::listens(ents, p, kind))
        .collect();
    if listening.is_empty() {
        return;
    }
    let Some(e) = view::snapshot(ents, entity) else {
        return;
    };
    for p in listening {
        if let Some(p) = ents.player_mut(p) {
            p.advancements.trigger(Event::Entity(kind, e.clone()));
        }
    }
}

/// The players whose box intersects `aabb` (`getEntitiesOfClass(ServerPlayer.class, aabb)`).
pub fn players_in(ents: &Entities, aabb: &vanillite_base::Aabb) -> Vec<EntityId> {
    ents.players
        .iter()
        .map(|(id, _)| id)
        .filter(|&id| {
            ents.store
                .get(id)
                .is_some_and(|e| e.aabb().intersects(aabb))
        })
        .collect()
}

/// `Sheep.getOffspringColor`: the dye two parent colours craft into, else one of them.
fn offspring_color(a: u8, b: u8, pick_first: bool) -> u8 {
    // `DyeColor` ids: 0 white, 1 orange, 2 magenta, 3 light blue, 4 yellow, 5 lime, 6 pink,
    // 7 gray, 8 light gray, 9 cyan, 10 purple, 11 blue, 12 brown, 13 green, 14 red, 15 black.
    let (lo, hi) = (a.min(b), a.max(b));
    let mixed = match (lo, hi) {
        (0, 15) => Some(7),
        (0, 7) => Some(8),
        (0, 14) => Some(6),
        (0, 11) => Some(3),
        (0, 13) => Some(5),
        (4, 14) => Some(1),
        (10, 6) | (6, 10) => Some(2),
        (11, 14) => Some(10),
        (11, 13) => Some(9),
        _ => None,
    };
    mixed.unwrap_or(if pick_first { a } else { b })
}

/// The inventory index of a hand.
pub fn hand_index(ents: &Entities, player: EntityId, hand: Hand) -> usize {
    match hand {
        Hand::MainHand => ents.player(player).map_or(0, |p| p.inventory.selected),
        Hand::OffHand => OFFHAND,
    }
}

/// The stack in a player's hand.
pub fn held(ents: &Entities, player: EntityId, hand: Hand) -> ItemStack {
    let i = hand_index(ents, player, hand);
    ents.player(player)
        .map(|p| p.inventory.get(i).clone())
        .unwrap_or_default()
}

pub fn infinite(ents: &Entities, player: EntityId) -> bool {
    ents.player(player)
        .is_some_and(|p| p.game_mode == GameMode::Creative)
}

/// `usePlayerItem`: one item of the hand is used up unless the player has infinite materials.
pub fn use_item(ents: &mut Entities, player: EntityId, hand: Hand) {
    if infinite(ents, player) {
        return;
    }
    let i = hand_index(ents, player, hand);
    if let Some(p) = ents.player_mut(player)
        && let Some(s) = p.inventory.get_mut(i)
    {
        s.shrink(1);
    }
}

/// `ItemUtils.createFilledResult`: the hand's stack turns into `filled` (one item of it).
pub fn fill_held(
    ents: &mut Entities,
    env: &mut Env<'_>,
    player: EntityId,
    hand: Hand,
    filled: ItemStack,
) {
    let creative = infinite(ents, player);
    let i = hand_index(ents, player, hand);
    let pos = ents.store.get(player).map_or(Vec3::ZERO, |e| e.pos());
    let Some(p) = ents.player_mut(player) else {
        return;
    };
    let mut filled = filled;
    if creative {
        if p.inventory.find_matching(&filled).is_none() {
            p.inventory.add(None, &mut filled, true);
        }
        return;
    }
    let Some(s) = p.inventory.get_mut(i) else {
        return;
    };
    s.shrink(1);
    if s.is_empty() {
        *s = filled;
    } else if !p.inventory.add(None, &mut filled, false) {
        // `drop(filled, false)`: it falls in front of the player.
        let (uuid, y_rot, _) = crate::mob::new_item_entity();
        env.out.push(Effect::DropItem {
            uuid,
            pos: pos + Vec3::new(0.0, 1.32, 0.0),
            velocity: Vec3::new(0.0, 0.1, 0.0),
            y_rot,
            pickup_delay: 40,
            stack: filled,
        });
    }
}

/// `hurtAndBreak(amount, player, hand)` of the stack in a hand.
pub fn hurt_held(
    ents: &mut Entities,
    env: &mut Env<'_>,
    player: EntityId,
    hand: Hand,
    amount: i32,
) {
    if infinite(ents, player) {
        return;
    }
    let unbreaking = crate::player::play::game_data().map_or(0, |d| {
        d.enchantment_level(&held(ents, player, hand), "minecraft:unbreaking")
    });
    let mut n = 0;
    for _ in 0..amount {
        #[allow(clippy::cast_precision_loss)]
        let chance = 1.0 / (unbreaking.max(0) as f32 + 1.0);
        if unbreaking <= 0 || env.random.next_float() < chance {
            n += 1;
        }
    }
    let i = hand_index(ents, player, hand);
    let pos = ents.store.get(player).map(|e| e.pos());
    let Some(p) = ents.player_mut(player) else {
        return;
    };
    let crate::player::PlayerData {
        inventory,
        advancements,
        stats,
        ..
    } = p;
    let Some(s) = inventory.get_mut(i) else {
        return;
    };
    if n == 0 || !item::is_damageable(s) {
        return;
    }
    let d = item::damage(s) + n;
    if crate::advancements::view::apply_damage(Some(advancements), Some(stats), s, d)
        && let Some(pos) = pos
    {
        let seed = crate::survival::sound_seed();
        play_sound(
            env.out,
            None,
            pos,
            "minecraft:entity.item.break",
            SoundSource::Players,
            0.8,
            0.8 + env.random.next_float() * 0.4,
            seed,
        );
    }
}

/// `Player.interactOn(entity, hand, location)` for a mob: the mob's own interaction, then the
/// held item's (`interactLivingEntity`).
#[allow(clippy::too_many_arguments)]
pub fn interact_on(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    player: EntityId,
    target: EntityId,
    hand: Hand,
    sneaking: bool,
) -> Interaction {
    if ents
        .player(player)
        .is_none_or(|p| p.game_mode == GameMode::Spectator)
    {
        return Interaction::Pass;
    }
    if ents.boat(target).is_some() {
        // `AbstractBoat.interact`: leads first, then getting in.
        let r = super::tame::leash_interact(ents, env, player, target, hand, sneaking);
        if r.consumes() {
            return r;
        }
        let out_of_control = ents
            .boat(target)
            .is_some_and(|b| b.out_of_control_ticks >= 60.0);
        if !sneaking
            && !out_of_control
            && crate::riding::start_riding(ents, env, player, target, false)
        {
            return Interaction::Success;
        }
        // `AbstractChestBoat.interact`: sneaking, or a full boat, opens the chest.
        if let Some(b) = ents.boat(target).filter(|b| b.kind.chest) {
            let room = ents.riding.passengers_of(target).len() < b.kind.max_passengers();
            if room && !sneaking {
                return Interaction::Pass;
            }
            crate::minecart::open_container(ents, env, cx, player, target);
            return Interaction::Success;
        }
        return Interaction::Pass;
    }
    if ents.minecart(target).is_some() {
        return crate::minecart::interact(ents, env, cx, player, target, hand, sneaking);
    }
    if !ents.mob(target).is_some_and(MobData::is_alive) {
        return Interaction::Pass;
    }
    let stack = held(ents, player, hand);
    let before = stack.count;
    let r = mob_interact(level, ents, env, cx, player, target, hand, sneaking, &stack);
    if r.consumes() {
        // Creative players keep their items.
        if infinite(ents, player) {
            let i = hand_index(ents, player, hand);
            if let Some(p) = ents.player_mut(player)
                && let Some(s) = p.inventory.get_mut(i)
                && s.item == stack.item
                && s.count < before
            {
                s.count = before;
            }
        }
        return r;
    }
    if !stack.is_empty() {
        return item_interact(ents, env, player, target, hand, &stack);
    }
    Interaction::Pass
}

/// `entity.gameEvent(event, source)`: at the entity's position.
pub fn entity_game_event(
    ents: &Entities,
    env: &mut Env<'_>,
    entity: EntityId,
    event: u16,
    source: Option<EntityId>,
) {
    if let Some(pos) = ents.store.get(entity).map(|e| e.pos()) {
        crate::survival::game_event(env.out, event, pos, source);
    }
}

/// `Mob.interact`: name tags and spawn eggs, leads, then `mobInteract`; the important
/// interactions and `mobInteract` post `ENTITY_INTERACT` when they consume the action.
#[allow(clippy::too_many_arguments)]
fn mob_interact(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    _cx: &mut MobContext,
    player: EntityId,
    target: EntityId,
    hand: Hand,
    sneaking: bool,
    stack: &ItemStack,
) -> Interaction {
    let name = item::item_id(stack).name();
    // `checkAndHandleImportantInteractions`.
    let interacted = |ents: &Entities, env: &mut Env<'_>| {
        let event = crate::level::game_event::events().entity_interact;
        entity_game_event(ents, env, target, event, Some(player));
    };
    if name == "minecraft:name_tag" {
        let r = name_tag(ents, player, target, hand, stack);
        if r.consumes() {
            interacted(ents, env);
            return r;
        }
    }
    if let Some(egg_kind) = spawn_egg_kind(stack) {
        let own = ents.mob(target).map(|m| m.kind);
        if own == Some(egg_kind) && ents.mob(target).is_some_and(|m| m.kind.ageable()) {
            spawn_offspring(level, ents, player, target, hand, egg_kind);
            interacted(ents, env);
            return Interaction::Success;
        }
        return Interaction::Pass;
    }
    // `Entity.interact`: leads (with their own game events).
    let r = super::tame::leash_interact(ents, env, player, target, hand, sneaking);
    if r != Interaction::Pass {
        return r;
    }
    // `mobInteract`.
    let r = own_interact(level, ents, env, _cx, player, target, hand, sneaking, stack);
    if r.consumes() {
        interacted(ents, env);
    }
    r
}

/// `mobInteract` of the mob kinds.
#[allow(clippy::too_many_arguments)]
fn own_interact(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    _cx: &mut MobContext,
    player: EntityId,
    target: EntityId,
    hand: Hand,
    sneaking: bool,
    stack: &ItemStack,
) -> Interaction {
    let name = item::item_id(stack).name();
    let kind = ents.mob(target).map_or(Kind::Zombie, |m| m.kind);
    if kind.family() != super::Family::Classic {
        let mut ix = super::fam::Interact {
            level,
            ents,
            env,
            cx: _cx,
            player,
            target,
            hand,
            sneaking,
            stack,
        };
        return super::fam::interact(&mut ix, kind);
    }
    match kind {
        Kind::Cow if name == "minecraft:bucket" && !ents.mob(target).is_some_and(|m| m.baby) => {
            // `AbstractCow.mobInteract`: the milk sound is the player's own.
            if let Some(pos) = ents.store.get(player).map(|e| e.pos()) {
                let seed = crate::survival::sound_seed();
                play_sound(
                    env.out,
                    None,
                    pos,
                    "minecraft:entity.cow.milk",
                    SoundSource::Players,
                    1.0,
                    1.0,
                    seed,
                );
            }
            if let Some(milk) = vanillite_data::registry::ITEM.id("minecraft:milk_bucket") {
                fill_held(ents, env, player, hand, ItemStack::new(milk, 1));
            }
            return Interaction::Success;
        }
        Kind::Sheep if name == "minecraft:shears" => {
            let ready = ents
                .mob(target)
                .is_some_and(|m| m.is_alive() && !m.sheared && !m.baby);
            if ready {
                shear_sheep(ents, env, target);
                let event = crate::level::game_event::events().shear;
                entity_game_event(ents, env, target, event, Some(player));
                hurt_held(ents, env, player, hand, 1);
                return Interaction::Success;
            }
            return Interaction::Consume;
        }
        Kind::Pig => {
            let food = is_food(kind, stack.item);
            let ridden = !ents.riding.passengers_of(target).is_empty();
            let saddled = ents.mob(target).is_some_and(|m| m.saddled);
            if !food && saddled && !ridden && !sneaking {
                crate::riding::start_riding(ents, env, player, target, false);
                return Interaction::Success;
            }
            let r = animal_interact(ents, env, player, target, hand, stack);
            if r.consumes() {
                return r;
            }
            if name == "minecraft:saddle" {
                return equip_saddle(ents, env, player, target, hand);
            }
            return Interaction::Pass;
        }
        _ => {}
    }
    if name == "minecraft:shears" && !sneaking && ents.mob(target).is_some_and(|m| m.saddled) {
        // `attemptToShearEquipment`: the saddle comes off.
        unequip_saddle(ents, env, target);
        let event = crate::level::game_event::events().shear;
        entity_game_event(ents, env, target, event, Some(player));
        hurt_held(ents, env, player, hand, 1);
        return Interaction::Success;
    }
    if kind.ageable() {
        return animal_interact(ents, env, player, target, hand, stack);
    }
    Interaction::Pass
}

/// `Animal.mobInteract`: food makes adults fall in love and babies grow faster.
/// `ItemUtils.createFilledResult` for the family kinds: the held item becomes `item`.
pub fn fill_held_pub(ix: &mut super::fam::Interact<'_, '_>, item: &str) {
    if let Some(id) = vanillite_data::registry::ITEM.id(item) {
        fill_held(ix.ents, ix.env, ix.player, ix.hand, ItemStack::new(id, 1));
    }
}

/// `stack.consume(1, player)` for the family kinds (creative players keep it).
pub fn use_held_pub(ix: &mut super::fam::Interact<'_, '_>) {
    use_item(ix.ents, ix.player, ix.hand);
}

/// `hurtAndBreak(amount, player, hand)` of the held item for the family kinds.
pub fn hurt_held_pub(ix: &mut super::fam::Interact<'_, '_>, amount: i32) {
    hurt_held(ix.ents, ix.env, ix.player, ix.hand, amount);
}

/// Whether the interacting player keeps its items (creative).
pub fn infinite_pub(ents: &Entities, player: EntityId) -> bool {
    infinite(ents, player)
}

/// `Animal.mobInteract` for the family kinds: feeding, love mode and growing babies.
pub fn animal_interact_pub(ix: &mut super::fam::Interact<'_, '_>) -> Interaction {
    animal_interact(ix.ents, ix.env, ix.player, ix.target, ix.hand, ix.stack)
}

pub fn animal_interact(
    ents: &mut Entities,
    env: &mut Env<'_>,
    player: EntityId,
    target: EntityId,
    hand: Hand,
    stack: &ItemStack,
) -> Interaction {
    let Some(m) = ents.mob(target) else {
        return Interaction::Pass;
    };
    if !is_food(m.kind, stack.item) {
        return Interaction::Pass;
    }
    let age = m.age;
    let uuid = ents.store.get(player).map(|e| e.uuid());
    if age == 0 && m.in_love <= 0 {
        use_item(ents, player, hand);
        if let Some(m) = ents.mob_mut(target) {
            m.in_love = 600;
            m.love_cause = uuid;
        }
        entity_event(ents, env, target, 18);
        eating_sound(ents, env, target);
        return Interaction::Success;
    }
    if m.baby {
        use_item(ents, player, hand);
        // `ageUp(getSpeedUpSecondsWhenFeeding(-age), true)`.
        #[allow(clippy::cast_possible_truncation)]
        let seconds = ((-age / 20) as f32 * 0.1) as i32;
        if let Some(m) = ents.mob_mut(target) {
            let new_age = (m.age + seconds * 20).min(0);
            let delta = new_age - m.age;
            m.age = new_age;
            m.forced_age += delta;
            if m.forced_age_timer == 0 {
                m.forced_age_timer = 40;
            }
        }
        eating_sound(ents, env, target);
        return Interaction::Success;
    }
    Interaction::Consume
}

/// `playEatingSound`: sheep and pigs have one.
pub fn eating_sound(ents: &mut Entities, env: &mut Env<'_>, target: EntityId) {
    let sound = match ents.mob(target).map(|m| m.kind) {
        Some(Kind::Sheep) => "minecraft:entity.sheep.eat",
        Some(Kind::Pig) => "minecraft:entity.pig.eat",
        _ => return,
    };
    combat::mob_sound(ents, env, target, sound);
}

/// `Sheep.shear`: 1 to 3 wool of its colour pop out.
fn shear_sheep(ents: &mut Entities, env: &mut Env<'_>, target: EntityId) {
    sound_at(
        ents,
        env,
        target,
        "minecraft:entity.sheep.shear",
        SoundSource::Players,
        1.0,
    );
    let Some(pos) = ents.store.get(target).map(|e| e.pos()) else {
        return;
    };
    let Some(m) = ents.mob_mut(target) else {
        return;
    };
    m.sheared = true;
    let color = usize::from(m.sheep_color & 15);
    let count = m.random.next_int_between_inclusive(1, 3);
    let wool = kinds::SHEEP_WOOL[color];
    let Some(item) = vanillite_data::registry::ITEM.id(wool) else {
        return;
    };
    for _ in 0..count {
        let Some(m) = ents.mob_mut(target) else {
            return;
        };
        let r = &mut m.random;
        let extra = Vec3::new(
            f64::from((r.next_float() - r.next_float()) * 0.1),
            f64::from(r.next_float() * 0.05),
            f64::from((r.next_float() - r.next_float()) * 0.1),
        );
        // `spawnAtLocation(stack, 1.0F)`: an item one block up with the default motion.
        let (uuid, y_rot, velocity) = crate::mob::new_item_entity();
        env.out.push(Effect::DropItem {
            uuid,
            pos: pos + Vec3::new(0.0, 1.0, 0.0),
            velocity: velocity + extra,
            y_rot,
            pickup_delay: 10,
            stack: ItemStack::new(item, 1),
        });
    }
    if let Some(mut e) = ents.store.get_mut(target) {
        e.mark_data_dirty();
    }
}

/// Saddles a pig (the saddle's `equippable` component with `equip_on_interact`).
fn equip_saddle(
    ents: &mut Entities,
    env: &mut Env<'_>,
    player: EntityId,
    target: EntityId,
    hand: Hand,
) -> Interaction {
    let ok = ents
        .mob(target)
        .is_some_and(|m| m.is_alive() && !m.saddled && !m.baby);
    if !ok {
        return Interaction::Pass;
    }
    if let Some(m) = ents.mob_mut(target) {
        m.saddled = true;
        m.persistent = true;
    }
    use_item(ents, player, hand);
    sound_at(
        ents,
        env,
        target,
        "minecraft:entity.pig.saddle",
        SoundSource::Neutral,
        1.0,
    );
    send_saddle(ents, env, target);
    Interaction::Success
}

/// Takes the saddle off and drops it (`attemptToShearEquipment`).
fn unequip_saddle(ents: &mut Entities, env: &mut Env<'_>, target: EntityId) {
    if let Some(m) = ents.mob_mut(target) {
        m.saddled = false;
    }
    sound_at(
        ents,
        env,
        target,
        "minecraft:item.saddle.unequip",
        SoundSource::Players,
        1.0,
    );
    send_saddle(ents, env, target);
    let Some(pos) = ents.store.get(target).map(|e| e.pos()) else {
        return;
    };
    if let Some(saddle) = vanillite_data::registry::ITEM.id("minecraft:saddle") {
        let (uuid, y_rot, velocity) = crate::mob::new_item_entity();
        env.out.push(Effect::DropItem {
            uuid,
            pos: pos + Vec3::new(0.0, 1.0, 0.0),
            velocity,
            y_rot,
            pickup_delay: 10,
            stack: ItemStack::new(saddle, 1),
        });
    }
}

/// `set_equipment` with the saddle slot of a mob.
fn send_saddle(ents: &Entities, env: &mut Env<'_>, target: EntityId) {
    let Some(e) = ents.store.get(target) else {
        return;
    };
    let stack = if ents.mob(target).is_some_and(|m| m.saddled) {
        vanillite_data::registry::ITEM
            .id("minecraft:saddle")
            .map_or_else(ItemStack::default, |i| ItemStack::new(i, 1))
    } else {
        ItemStack::default()
    };
    env.out.push(Effect::Tracking {
        entity: target,
        with_self: false,
        packet: SharedPacket::of(
            &vanillite_protocol::packets::play::clientbound::SetEquipment {
                entity_id: e.network_id().0,
                slots: vec![(item::EquipmentSlot::Saddle.ordinal(), stack)],
            },
        ),
    });
}

/// `NameTagItem.interactLivingEntity`: a named tag names the mob and keeps it from
/// despawning.
fn name_tag(
    ents: &mut Entities,
    player: EntityId,
    target: EntityId,
    hand: Hand,
    stack: &ItemStack,
) -> Interaction {
    let Some(name) = stack.component(vanillite_protocol::components::ids::CUSTOM_NAME) else {
        return Interaction::Pass;
    };
    let name: Box<[u8]> = name.into();
    if let Some(m) = ents.mob_mut(target) {
        m.custom_name = Some(name);
        m.persistent = true;
    }
    if let Some(mut e) = ents.store.get_mut(target) {
        e.mark_data_dirty();
    }
    use_item(ents, player, hand);
    Interaction::Success
}

/// The mob kind a spawn egg spawns (`entity_data` default `id`), by its item name.
pub fn spawn_egg_kind(stack: &ItemStack) -> Option<Kind> {
    let name = item::item_id(stack).name();
    let base = name.strip_suffix("_spawn_egg")?;
    let ty = vanillite_data::entity::EntityTypeId::by_name(base)?;
    Kind::of(ty)
}

/// `SpawnEggItem.spawnOffspringFromSpawnEgg`: a baby of the same kind.
fn spawn_offspring(
    level: &Level,
    ents: &mut Entities,
    player: EntityId,
    target: EntityId,
    hand: Hand,
    kind: Kind,
) {
    let Some(pos) = ents.store.get(target).map(|e| e.pos()) else {
        return;
    };
    let seed = crate::mob::new_entity_seed();
    let mut child = MobData::new(kind, seed);
    child.age = -24000;
    child.baby = true;
    if let Some(m) = ents.mob(target) {
        child.variant = m.variant;
        child.sheep_color = m.sheep_color;
    }
    let stack = held(ents, player, hand);
    if let Some(name) = stack.component(vanillite_protocol::components::ids::CUSTOM_NAME) {
        child.custom_name = Some(name.into());
    }
    let uuid = crate::mob::new_uuid();
    super::spawn(
        ents,
        level.network_ids(),
        uuid,
        pos,
        Rotation::default(),
        child,
    );
    use_item(ents, player, hand);
}

/// `ItemStack.interactLivingEntity` of items not handled by the mob: saddles on other
/// mobs, leads are in `Entity.interact`.
fn item_interact(
    _ents: &mut Entities,
    _env: &mut Env<'_>,
    _player: EntityId,
    _target: EntityId,
    _hand: Hand,
    _stack: &ItemStack,
) -> Interaction {
    Interaction::Pass
}

/// `SpawnEggItem.useOn`: spawns the egg's mob on the clicked block face.
#[allow(clippy::too_many_arguments)]
pub fn use_spawn_egg_on(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    network_ids: &NetworkIds,
    kind: Kind,
    pos: vanillite_base::BlockPos,
    face: vanillite_base::Direction,
    stack: &mut ItemStack,
    infinite: bool,
) -> bool {
    let state = level.block(pos);
    // An empty collision shape: spawn inside, else on the face.
    let spawn_pos = if state.collision_shape().is_empty() {
        pos
    } else {
        pos.relative(face)
    };
    let below_face = spawn_pos != pos && face == vanillite_base::Direction::Up;
    let seed = crate::mob::new_entity_seed();
    let mut m = MobData::new(kind, seed);
    let mut ctx =
        super::fam::SpawnContext::at(level, spawn_pos, super::fam::SpawnReason::SpawnItemUse);
    kinds::finalize_spawn(&mut m, &mut ctx, env.random);
    if let Some(name) = stack.component(vanillite_protocol::components::ids::CUSTOM_NAME) {
        m.custom_name = Some(name.into());
        m.persistent = true;
    }
    // `EntityType.spawn(..., shouldOffsetY, shouldOffsetYMore)`: above the block's shape.
    let y_off = spawn_y_offset(level, spawn_pos, below_face, kind);
    let p = Vec3::new(
        f64::from(spawn_pos.x) + 0.5,
        f64::from(spawn_pos.y) + y_off,
        f64::from(spawn_pos.z) + 0.5,
    );
    let yaw = vanillite_base::math::wrap_degrees_f(env.random.next_float() * 360.0);
    let rot = Rotation {
        y_rot: yaw,
        x_rot: 0.0,
        y_head_rot: yaw,
    };
    let uuid = crate::mob::new_uuid();
    m.y_body_rot = yaw;
    super::spawn(ents, network_ids, uuid, p, rot, m);
    if !infinite {
        stack.shrink(1);
    }
    true
}

/// `EntityType.getYOffset`: how far up from the block the new mob stands.
fn spawn_y_offset(
    level: &Level,
    spawn_pos: vanillite_base::BlockPos,
    moved_up: bool,
    kind: Kind,
) -> f64 {
    use vanillite_base::Aabb;
    let mut area = Aabb::of_block(spawn_pos);
    if moved_up {
        area = area.expand_towards(0.0, -1.0, 0.0);
    }
    let mut boxes = Vec::new();
    super::physics::block_boxes(level, &area, &mut boxes);
    let info = kind.type_id().info();
    let half = f64::from(info.width) / 2.0;
    let (x, y, z) = (
        f64::from(spawn_pos.x) + 0.5,
        f64::from(spawn_pos.y) + 1.0,
        f64::from(spawn_pos.z) + 0.5,
    );
    let entity = Aabb::new(
        x - half,
        y,
        z - half,
        x + half,
        y + f64::from(info.height),
        z + half,
    );
    1.0 + super::physics::collide_axis(&boxes, &entity, 1, if moved_up { -2.0 } else { -1.0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sheep_colours_mix_like_dyes() {
        // White and black make gray, red and yellow orange; others keep a parent's colour.
        assert_eq!(offspring_color(0, 15, true), 7);
        assert_eq!(offspring_color(14, 4, false), 1);
        assert_eq!(offspring_color(1, 3, true), 1);
        assert_eq!(offspring_color(1, 3, false), 3);
    }
}
