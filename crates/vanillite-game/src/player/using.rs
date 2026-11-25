//! Using an item over time (`LivingEntity.startUsingItem`, `updatingUsingItem`,
//! `releaseUsingItem`, `completeUsingItem`) for players: eating and drinking, blocking with a
//! shield, drawing a bow. The per-item behaviour is the `match` in [`on_use_tick`],
//! [`finish`] and [`release`].

use vanillite_base::Vec3;
use vanillite_base::random::RandomSource;
use vanillite_config::GameMode;
use vanillite_protocol::packets::play::clientbound::{EntityEvent, SoundSource};
use vanillite_protocol::packets::play::serverbound::Hand;
use vanillite_world::entity::EntityId;

use crate::entities::Entities;
use crate::item::{self, ItemStack};
use crate::survival::play_sound;
use crate::survival::player::Env;

/// The item a player uses (`useItem`, `useItemRemaining`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsingItem {
    pub hand: Hand,
    /// The item used; using stops when the hand holds another one.
    pub item: u16,
    /// Ticks used so far.
    pub ticks: i32,
    /// `getUseDuration`.
    pub duration: i32,
}

/// Ticks a shield needs before it blocks (`block_delay_seconds` 0.25).
const SHIELD_DELAY: i32 = 5;

fn hand_index(ents: &Entities, player: EntityId, hand: Hand) -> usize {
    match hand {
        Hand::MainHand => ents.player(player).map_or(0, |p| p.inventory.selected),
        Hand::OffHand => crate::inventory::OFFHAND,
    }
}

fn held(ents: &Entities, player: EntityId, hand: Hand) -> ItemStack {
    let i = hand_index(ents, player, hand);
    ents.player(player)
        .map(|p| p.inventory.get(i).clone())
        .unwrap_or_default()
}

/// `getUseDuration` of an item, 0 when it is not used over time.
fn use_duration(ents: &Entities, player: EntityId, stack: &ItemStack) -> i32 {
    let id = item::item_id(stack);
    let info = id.info();
    let name = id.name();
    if info.blocks_attacks || name == "minecraft:bow" {
        return 72000;
    }
    if name == "minecraft:brush" {
        return 200;
    }
    let drink = matches!(
        name,
        "minecraft:milk_bucket"
            | "minecraft:potion"
            | "minecraft:honey_bottle"
            | "minecraft:ominous_bottle"
    );
    if let Some(food) = info.food {
        // `canEat`: hungry, always edible, or invulnerable.
        let can = ents.player(player).is_some_and(|p| {
            food.can_always_eat
                || matches!(p.game_mode, GameMode::Creative | GameMode::Spectator)
                || p.survival.food.needs_food()
        });
        if !can {
            return 0;
        }
    } else if !drink {
        return 0;
    }
    match name {
        "minecraft:dried_kelp" => 16,
        "minecraft:honey_bottle" => 40,
        _ => 32,
    }
}

fn mark(ents: &mut Entities, player: EntityId) {
    if let Some(mut e) = ents.store.get_mut(player) {
        e.mark_data_dirty();
    }
}

/// `startUsingItem(hand)` from `use_item`, when the held item is used over time. A bow needs
/// arrows unless the player has infinite materials.
pub fn start(ents: &mut Entities, _env: &mut Env<'_>, player: EntityId, hand: Hand) {
    let stack = held(ents, player, hand);
    if stack.is_empty() {
        return;
    }
    let duration = use_duration(ents, player, &stack);
    if duration <= 0 {
        return;
    }
    let cooling = ents.player(player).is_some_and(|p| {
        item::item_id(&stack).info().blocks_attacks
            && p.survival.tick_count < p.survival.shield_disabled_until
    });
    if cooling {
        return;
    }
    if item::item_id(&stack).name() == "minecraft:bow" && find_arrow(ents, player).is_none() {
        let creative = ents
            .player(player)
            .is_some_and(|p| p.game_mode == GameMode::Creative);
        if !creative {
            return;
        }
    }
    if let Some(p) = ents.player_mut(player) {
        p.survival.using_item = Some(UsingItem {
            hand,
            item: stack.item,
            ticks: 0,
            duration,
        });
    }
    mark(ents, player);
}

/// `stopUsingItem`.
pub fn stop(ents: &mut Entities, player: EntityId) {
    let was = ents
        .player_mut(player)
        .and_then(|p| p.survival.using_item.take());
    if was.is_some() {
        mark(ents, player);
    }
}

/// Whether the player blocks with a shield (`isBlocking`).
pub fn is_blocking(ents: &Entities, player: EntityId) -> bool {
    ents.player(player)
        .and_then(|p| p.survival.using_item)
        .is_some_and(|u| {
            item::item_id(&ItemStack::new(u.item, 1))
                .info()
                .blocks_attacks
                && u.ticks >= SHIELD_DELAY
        })
}

/// The living entity flags of a player (`DATA_LIVING_ENTITY_FLAGS`): using an item and the
/// off hand.
pub fn living_flags(u: Option<UsingItem>) -> u8 {
    match u {
        Some(u) => 1 | if u.hand == Hand::OffHand { 2 } else { 0 },
        None => 0,
    }
}

/// `updatingUsingItem` of every player that uses an item; once per tick.
pub fn tick_all(ents: &mut Entities, env: &mut Env<'_>) {
    let users: Vec<EntityId> = ents
        .players
        .iter()
        .filter(|(_, p)| p.survival.using_item.is_some())
        .map(|(id, _)| id)
        .collect();
    for id in users {
        tick(ents, env, id);
    }
}

/// `updatingUsingItem`: stops when the hand changed, runs the per-tick behaviour and finishes
/// consumables.
pub fn tick(ents: &mut Entities, env: &mut Env<'_>, player: EntityId) {
    let Some(u) = ents.player(player).and_then(|p| p.survival.using_item) else {
        return;
    };
    let stack = held(ents, player, u.hand);
    if stack.item != u.item || stack.is_empty() {
        stop(ents, player);
        return;
    }
    let ticks = u.ticks.saturating_add(1);
    if let Some(p) = ents.player_mut(player)
        && let Some(state) = p.survival.using_item.as_mut()
    {
        state.ticks = ticks;
    }
    if ticks == SHIELD_DELAY && item::item_id(&stack).info().blocks_attacks {
        // The blocking state becomes visible (`isBlocking`).
        mark(ents, player);
    }
    let remaining = u.duration.saturating_sub(ticks);
    if is_brush(&stack) {
        // `BrushItem.onUseTick` needs the level: [`tick_brushes`]. The brush is no
        // consumable, `completeUsingItem` only stops using it.
        if remaining <= 0 {
            stop(ents, player);
        }
        return;
    }
    on_use_tick(ents, env, player, &stack, remaining);
    if remaining <= 0 && u.duration < 72000 {
        finish(ents, env, player, u);
    }
}

fn is_brush(stack: &ItemStack) -> bool {
    item::item_id(stack).name() == "minecraft:brush"
}

/// `BrushItem.onUseTick` of every player brushing (after [`tick_all`], with the entities back
/// in the level): the view ray must hit a block within the block interaction range, else the
/// player stops (`releaseUsingItem`); every tenth tick (`timeElapsed % 10 == 5`) the stroke
/// plays the brush sound and brushes a brushable block (`BrushableBlockEntity.brush`).
pub fn tick_brushes(level: &mut crate::level::Level) {
    let users: Vec<(EntityId, UsingItem)> = level
        .entities
        .players
        .iter()
        .filter_map(|(id, p)| p.survival.using_item.map(|u| (id, u)))
        .collect();
    for (player, u) in users {
        let stack = held(&level.entities, player, u.hand);
        if stack.item != u.item || !is_brush(&stack) {
            continue;
        }
        let Some(hit) = brush_hit(level, player) else {
            stop(&mut level.entities, player);
            continue;
        };
        // `timeElapsed = getUseDuration - ticksRemaining + 1` with the remaining ticks before
        // this tick's decrement: the ticks used so far.
        if u.ticks % 10 != 5 {
            continue;
        }
        let (pos, face) = hit;
        let state = level.block(pos);
        let sound = crate::block::brushable::brush_sound(state);
        let sound = crate::block::sound_id(sound.strip_prefix("minecraft:").unwrap_or(sound));
        level.play_block_sound(
            Some(player),
            pos,
            sound,
            crate::level::SOUND_BLOCKS,
            1.0,
            1.0,
        );
        if !matches!(
            level.block_entity(pos),
            Some(crate::level::BlockEntity::Brushable(_))
        ) {
            continue;
        }
        let i = hand_index(&level.entities, player, u.hand);
        let mut brush = level
            .entities
            .player_mut(player)
            .and_then(|p| p.inventory.get_mut(i).map(std::mem::take))
            .unwrap_or_default();
        let time = level.game_time();
        crate::block::brushable::brush(level, time, player, pos, face, &mut brush);
        if let Some(p) = level.entities.player_mut(player)
            && let Some(slot) = p.inventory.get_mut(i)
        {
            *slot = brush;
        }
    }
}

/// `ProjectileUtil.getHitResultOnViewVector(player, CAN_BE_PICKED, blockInteractionRange)`:
/// the block and face the player looks at, `None` when nothing or an entity is hit first.
fn brush_hit(
    level: &crate::level::Level,
    player: EntityId,
) -> Option<(vanillite_base::BlockPos, vanillite_base::Direction)> {
    /// The `block_interaction_range` attribute of players.
    const RANGE: f64 = 4.5;
    let e = level.entities.store.get(player)?;
    let info = e.kind().info();
    let eye_height =
        f64::from(info.eye_height) * f64::from(e.dims().height / info.height.max(0.01));
    let rot = e.rot();
    let eye = e.pos() + Vec3::new(0.0, eye_height, 0.0);
    let view = crate::mob::view_vector(rot.x_rot, rot.y_rot);
    let to = eye + view * RANGE;
    let (pos, at) = crate::mob::physics::clip(level, eye, to)?;
    // An entity on the way is hit first (`getEntityHitResult` with `isPickable`).
    let area =
        vanillite_base::Aabb::new(eye.x, eye.y, eye.z, at.x, at.y, at.z).inflate(1.0, 1.0, 1.0);
    let mut entity_first = false;
    let store = &level.entities.store;
    store.for_each_in(&area, false, |other| {
        if other == player || entity_first {
            return;
        }
        let Some(o) = store.get(other) else {
            return;
        };
        if !pickable(level, other, o.kind()) {
            return;
        }
        if let Some(t) = ray_box(eye, at - eye, &o.aabb())
            && t < 1.0
        {
            entity_first = true;
        }
    });
    if entity_first {
        return None;
    }
    Some((pos, hit_face(pos, at, view)))
}

/// `Entity.isPickable` of the entities a view ray can hit: living entities that are not
/// spectators, boats, minecarts and other solid entities, not items, orbs or projectiles.
fn pickable(
    level: &crate::level::Level,
    id: EntityId,
    kind: vanillite_data::entity::EntityTypeId,
) -> bool {
    use vanillite_data::entity::ids;
    if let Some(p) = level.entities.player(id) {
        return p.game_mode != GameMode::Spectator && !p.survival.is_dead_or_dying();
    }
    if let Some(m) = level.entities.mob(id) {
        return m.is_alive();
    }
    crate::vehicle::BoatKind::of(kind).is_some()
        || crate::minecart::MinecartKind::of(kind).is_some()
        || kind == ids::END_CRYSTAL
        || kind == ids::ARMOR_STAND
}

/// The entry fraction of the segment `from + t * delta` (`t` in 0..=1) into the box.
fn ray_box(from: Vec3, delta: Vec3, b: &vanillite_base::Aabb) -> Option<f64> {
    let mut t0 = 0.0_f64;
    let mut t1 = 1.0_f64;
    for (o, d, lo, hi) in [
        (from.x, delta.x, b.min_x, b.max_x),
        (from.y, delta.y, b.min_y, b.max_y),
        (from.z, delta.z, b.min_z, b.max_z),
    ] {
        if d.abs() < 1e-12 {
            if o < lo || o > hi {
                return None;
            }
            continue;
        }
        let (a, c) = ((lo - o) / d, (hi - o) / d);
        let (a, c) = if a < c { (a, c) } else { (c, a) };
        t0 = t0.max(a);
        t1 = t1.min(c);
        if t0 > t1 {
            return None;
        }
    }
    Some(t0)
}

/// The face of the block the ray entered at `at`: the side whose plane the hit point lies on,
/// facing against the ray.
fn hit_face(pos: vanillite_base::BlockPos, at: Vec3, view: Vec3) -> vanillite_base::Direction {
    use vanillite_base::Direction;
    let local = at - Vec3::new(f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
    let candidates = [
        (local.x, view.x, Direction::West, Direction::East),
        (local.y, view.y, Direction::Down, Direction::Up),
        (local.z, view.z, Direction::North, Direction::South),
    ];
    let mut best = (f64::MAX, Direction::Up);
    for (c, d, low, high) in candidates {
        if d > 0.0 && c.abs() < best.0 {
            best = (c.abs(), low);
        } else if d < 0.0 && (1.0 - c).abs() < best.0 {
            best = ((1.0 - c).abs(), high);
        }
    }
    best.1
}

/// `Item.onUseTick`: eating and drinking sounds. Items with their own use tick (the brush of
/// #62) get an arm here.
fn on_use_tick(
    ents: &mut Entities,
    env: &mut Env<'_>,
    player: EntityId,
    stack: &ItemStack,
    remaining: i32,
) {
    let info = item::item_id(stack).info();
    if info.food.is_none() {
        return;
    }
    // `Consumable.shouldEmitParticlesAndSounds`.
    let total = use_duration(ents, player, stack).max(1);
    // `(int)(consumeTicks * 0.21875F)`.
    let threshold = total.saturating_mul(7) / 32;
    if total.saturating_sub(remaining) > threshold && remaining % 4 == 0 {
        let Some(pos) = ents.store.get(player).map(|e| e.pos()) else {
            return;
        };
        let volume = if env.random.next_int_bounded(2) == 1 {
            1.0
        } else {
            0.5
        };
        let pitch = (env.random.next_float() - env.random.next_float()) * 0.2 + 1.0;
        let seed = crate::survival::sound_seed();
        play_sound(
            env.out,
            Some(player),
            pos,
            "minecraft:entity.generic.eat",
            SoundSource::Players,
            volume,
            pitch,
            seed,
        );
    }
}

/// `completeUsingItem`: food feeds the player, the stack shrinks (or turns into its
/// remainder), the client is told (`entity_event` 9).
fn finish(ents: &mut Entities, env: &mut Env<'_>, player: EntityId, u: UsingItem) {
    stop(ents, player);
    let stack = held(ents, player, u.hand);
    let id = item::item_id(&stack);
    let name = id.name();
    let pos = ents.store.get(player).map_or(Vec3::ZERO, |e| e.pos());
    let creative = ents
        .player(player)
        .is_some_and(|p| p.game_mode == GameMode::Creative);
    if let Some(food) = id.info().food
        && let Some(p) = ents.player_mut(player)
    {
        p.survival.food.eat(&crate::survival::food::FoodProperties {
            nutrition: food.nutrition,
            saturation: food.saturation,
            can_always_eat: food.can_always_eat,
        });
        let pitch = env.random.next_float() * 0.1 + 0.9;
        let seed = crate::survival::sound_seed();
        play_sound(
            env.out,
            None,
            pos,
            "minecraft:entity.player.burp",
            SoundSource::Players,
            0.5,
            pitch,
            seed,
        );
    }
    if name == "minecraft:milk_bucket" || name == "minecraft:honey_bottle" {
        let seed = crate::survival::sound_seed();
        play_sound(
            env.out,
            Some(player),
            pos,
            "minecraft:entity.generic.drink",
            SoundSource::Players,
            0.5,
            env.random.next_float() * 0.1 + 0.9,
            seed,
        );
    }
    let remainder = match name {
        "minecraft:mushroom_stew"
        | "minecraft:rabbit_stew"
        | "minecraft:beetroot_soup"
        | "minecraft:suspicious_stew" => Some("minecraft:bowl"),
        "minecraft:honey_bottle" | "minecraft:potion" => Some("minecraft:glass_bottle"),
        "minecraft:milk_bucket" => Some("minecraft:bucket"),
        _ => None,
    };
    if !creative {
        let i = hand_index(ents, player, u.hand);
        if let Some(p) = ents.player_mut(player) {
            let mut extra = None;
            if let Some(s) = p.inventory.get_mut(i) {
                s.shrink(1);
                if let Some(r) = remainder.and_then(|r| vanillite_data::registry::ITEM.id(r)) {
                    let rest = ItemStack::new(r, 1);
                    if s.is_empty() {
                        *s = rest;
                    } else {
                        extra = Some(rest);
                    }
                }
            }
            if let Some(mut rest) = extra {
                p.inventory.add(None, &mut rest, false);
            }
        }
    }
    if let Some(e) = ents.store.get(player)
        && let Some(p) = ents.player(player)
    {
        p.connection.sink.send(&EntityEvent {
            entity_id: e.network_id().0,
            event: 9,
        });
    }
}

/// The first arrow in the inventory (`getProjectile`): off hand, main hand, then the slots.
fn find_arrow(ents: &Entities, player: EntityId) -> Option<usize> {
    let p = ents.player(player)?;
    let arrow = vanillite_data::registry::ITEM.id("minecraft:arrow")?;
    let is_arrow = |s: &ItemStack| s.item == arrow && !s.is_empty();
    if is_arrow(p.inventory.get(crate::inventory::OFFHAND)) {
        return Some(crate::inventory::OFFHAND);
    }
    if is_arrow(p.inventory.selected_item()) {
        return Some(p.inventory.selected);
    }
    (0..36).find(|&i| is_arrow(p.inventory.get(i)))
}

/// `BowItem.getPowerForTime`.
pub fn bow_power(ticks: i32) -> f32 {
    let f = f32::from(i16::try_from(ticks.clamp(0, 1000)).unwrap_or(0)) / 20.0;
    ((f * f + f * 2.0) / 3.0).min(1.0)
}

/// `releaseUsingItem` (`player_action` `RELEASE_USE_ITEM`): a drawn bow shoots.
pub fn release(
    ents: &mut Entities,
    env: &mut Env<'_>,
    network_ids: &vanillite_world::entity::NetworkIds,
    player: EntityId,
) {
    let Some(u) = ents.player(player).and_then(|p| p.survival.using_item) else {
        return;
    };
    stop(ents, player);
    let stack = held(ents, player, u.hand);
    if stack.item != u.item || item::item_id(&stack).name() != "minecraft:bow" {
        return;
    }
    let power = bow_power(u.ticks);
    if power < 0.1 {
        return;
    }
    let creative = ents
        .player(player)
        .is_some_and(|p| p.game_mode == GameMode::Creative);
    let infinity = crate::player::play::game_data()
        .is_some_and(|d| d.enchantment_level(&stack, "minecraft:infinity") > 0);
    let arrow_slot = find_arrow(ents, player);
    if arrow_slot.is_none() && !creative {
        return;
    }
    let Some(e) = ents.store.get(player) else {
        return;
    };
    let rot = e.rot();
    let p = e.pos();
    let eye = Vec3::new(
        p.x,
        p.y + 1.62 * f64::from(e.dims().height / 1.8) - 0.1,
        p.z,
    );
    let (yr, xr) = (
        f64::from(rot.y_rot) * std::f64::consts::PI / 180.0,
        f64::from(rot.x_rot) * std::f64::consts::PI / 180.0,
    );
    let dir = Vec3::new(-yr.sin() * xr.cos(), -xr.sin(), yr.cos() * xr.cos());
    // Arrows that cost nothing can only be picked up in creative mode.
    let free = creative || infinity;
    let pickup = if free { 2 } else { 1 };
    crate::mob::projectile::shoot_arrow(
        ents,
        network_ids,
        env.random,
        eye,
        dir,
        power * 3.0,
        1.0,
        Some(player),
        Some(&stack),
        power >= 1.0,
        pickup,
    );
    let pos = ents.store.get(player).map_or(Vec3::ZERO, |e| e.pos());
    let pitch = 1.0 / (env.random.next_float() * 0.4 + 1.2) + power * 0.5;
    let seed = crate::survival::sound_seed();
    play_sound(
        env.out,
        None,
        pos,
        "minecraft:entity.arrow.shoot",
        SoundSource::Players,
        1.0,
        pitch,
        seed,
    );
    if !free
        && let Some(i) = arrow_slot
        && let Some(p) = ents.player_mut(player)
        && let Some(s) = p.inventory.get_mut(i)
    {
        s.shrink(1);
    }
    if !creative {
        // The bow loses one durability (with Unbreaking).
        let unbreaking = crate::player::play::game_data()
            .map_or(0, |d| d.enchantment_level(&stack, "minecraft:unbreaking"));
        let level = f32::from(i16::try_from(unbreaking.clamp(0, 255)).unwrap_or(0));
        let keeps = unbreaking > 0 && env.random.next_float() >= 1.0 / (level + 1.0);
        let i = hand_index(ents, player, u.hand);
        if !keeps
            && let Some(p) = ents.player_mut(player)
            && let Some(s) = p.inventory.get_mut(i)
            && item::is_damageable(s)
        {
            let d = item::damage(s).saturating_add(1);
            if d >= item::max_damage(s) {
                s.shrink(1);
            } else {
                item::set_damage(s, d);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bow_power_curve() {
        assert!(bow_power(0) == 0.0);
        assert!((bow_power(10) - (0.25 + 1.0) / 3.0).abs() < 1e-6);
        assert!((bow_power(20) - 1.0).abs() < 1e-6);
        assert!((bow_power(100) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn living_flags_of_hands() {
        let u = UsingItem {
            hand: Hand::OffHand,
            item: 1,
            ticks: 0,
            duration: 32,
        };
        assert_eq!(living_flags(Some(u)), 3);
        assert_eq!(living_flags(None), 0);
    }
}
