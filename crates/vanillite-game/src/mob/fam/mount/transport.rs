//! The copper golem's `TransportItemsBetweenContainers` with `CopperGolemAi`'s callbacks: an
//! empty handed golem walks to the nearest copper chest it has not visited, takes up to 16 items
//! of its first stack and carries them to the nearest chest or trapped chest that is empty or
//! holds the same item. At a chest it opens the lid for 60 ticks; chests another user has open
//! make it queue. Ten visited or fifty unreachable chests without success send it into a 140
//! tick cooldown.
//!
//! The tick reads the level; the container changes (lids, stacks) are queued as world actions.
//! Deviations: chests whose loot table was not rolled yet are seen without their loot; a chest
//! the golem opened is closed as soon as it walks off (Vanilla's opener recheck closes it within
//! five ticks); cats on chests do not block them; inline (unregistered) container locks are not
//! modelled.

use vanillite_base::{Aabb, BlockPos, ChunkPos, Direction, Vec3};
use vanillite_data::block::BlockState;

use super::copper_golem::{GolemState, golem_mut};
use crate::block::container::{ContainerRef, Kind as ContainerKind, chest_blocked, kind_of};
use crate::item::{EquipmentSlot, ItemStack};
use crate::level::BlockEntity;
use crate::mob::brain::{self, GlobalPos, MemValue, MemoryType as M, Tracker, brain_ref};
use crate::mob::{Tick, WorldAction};

/// `TARGET_INTERACTION_TIME`.
const TARGET_INTERACTION_TIME: i32 = 60;
/// `VISITED_POSITIONS_MEMORY_TIME`.
const VISITED_POSITIONS_MEMORY_TIME: i64 = 6000;
/// `TRANSPORTED_ITEM_MAX_STACK_SIZE`.
const MAX_STACK: i32 = 16;
/// `MAX_VISITED_POSITIONS`.
const MAX_VISITED: usize = 10;
/// `MAX_UNREACHABLE_POSITIONS`.
const MAX_UNREACHABLE: usize = 50;
/// `IDLE_COOLDOWN`.
const IDLE_COOLDOWN: i32 = 140;
/// `CopperGolemAi.TRANSPORT_ITEM_HORIZONTAL_SEARCH_RADIUS` and `_VERTICAL_`.
const SEARCH: (i32, i32) = (32, 8);
/// `CopperGolemAi.SPEED_MULTIPLIER_WHEN_IDLING`.
const SPEED: f32 = 1.0;

/// `TransportItemState`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Travelling,
    Queuing,
    Interacting,
}

/// `ContainerInteractionState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reached {
    PickupItem,
    PickupNoItem,
    PlaceItem,
    PlaceNoItem,
}

/// The behaviour's own state (it survives stops, like the Java instance).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Transport {
    /// `target`: the container's position and the state it had when chosen.
    pub target: Option<(BlockPos, BlockState)>,
    /// `state`.
    pub phase: Phase,
    /// `interactionState`.
    pub interaction: Option<Reached>,
    /// `ticksSinceReachingTarget`.
    pub ticks: i32,
}

/// `checkExtraStartConditions`: not leashed.
pub fn check(t: &Tick<'_, '_>) -> bool {
    !t.ents.leashes.contains_key(&t.id)
}

/// `canStillUse`.
pub fn can_still_use(t: &Tick<'_, '_>) -> bool {
    let b = brain_ref(t);
    !b.has(M::TransportItemsCooldownTicks)
        && !b.has(M::IsPanicking)
        && !t.ents.leashes.contains_key(&t.id)
}

/// `start`: the golem may path to targets below the surface.
pub fn start(t: &mut Tick<'_, '_>) {
    if t.mob_ref().nav.is_ground() {
        t.mob().nav.can_path_below_surface = true;
    }
}

/// `stop`.
pub fn stop(t: &mut Tick<'_, '_>, s: &mut Transport) {
    on_start_travelling(t, s);
    if t.mob_ref().nav.is_ground() {
        t.mob().nav.can_path_below_surface = false;
    }
}

/// `tick`.
pub fn tick(t: &mut Tick<'_, '_>, s: &mut Transport) {
    let updated = update_invalid_target(t, s);
    let Some((pos, state)) = s.target else {
        stop(t, s);
        return;
    };
    if updated {
        return;
    }
    if s.phase == Phase::Queuing && !another_mob_interacting(t, pos, state) {
        s.phase = Phase::Travelling;
        walk_towards(t, s);
    }
    if s.phase == Phase::Travelling {
        let center = center_pos(t, t.pos());
        if within_target_distance(t, 3.0, pos, state, center)
            && another_mob_interacting(t, pos, state)
        {
            stop_in_place(t);
            s.phase = Phase::Queuing;
        } else if within_target_distance(t, interaction_range(t), pos, state, center) {
            // `startOnReachedTargetInteraction`.
            let container = target_container(t, pos, state);
            s.interaction = Some(if picking_up(t) {
                if container.is_some_and(|c| !c.is_empty(t.level)) {
                    Reached::PickupItem
                } else {
                    Reached::PickupNoItem
                }
            } else if container.is_some_and(|c| leaving_requirement(t, c)) {
                Reached::PlaceItem
            } else {
                Reached::PlaceNoItem
            });
            s.phase = Phase::Interacting;
        } else {
            walk_towards(t, s);
        }
    }
    if s.phase == Phase::Interacting {
        on_reached_target(t, s, pos, state);
    }
}

/// `updateInvalidTarget`: a new target when the current one is gone; `true` if it changed.
fn update_invalid_target(t: &mut Tick<'_, '_>, s: &mut Transport) -> bool {
    if has_valid_target(t, s) {
        return false;
    }
    stop_targeting(t, s);
    match transport_target(t) {
        Some(target) => {
            s.target = Some(target);
            on_start_travelling(t, s);
            set_visited(t, s, target.0);
        }
        None => enter_cooldown(t, s),
    }
    true
}

/// `hasValidTarget`.
fn has_valid_target(t: &mut Tick<'_, '_>, s: &mut Transport) -> bool {
    let Some((pos, state)) = s.target else {
        return false;
    };
    // `targetHasNotChanged`: the chest's block entity is still there.
    let unchanged = t.level.block(pos).block() == state.block()
        && matches!(t.level.block_entity(pos), Some(BlockEntity::Container(_)));
    if !(wanted_block(t, state) && unchanged) || chest_blocked(t.level, pos) {
        return false;
    }
    if s.phase != Phase::Travelling || valid_travelling_path(t, pos, state) {
        return true;
    }
    mark_unreachable(t, s, pos);
    false
}

/// `hasValidTravellingPath`.
fn valid_travelling_path(t: &mut Tick<'_, '_>, pos: BlockPos, state: BlockState) -> bool {
    let path = match t.mob_ref().nav.path.clone() {
        Some(p) => Some(p),
        None => crate::mob::nav::create_path_to(t, pos, 0),
    };
    // `getPositionToReachTargetFrom`.
    let from = match path.as_ref().and_then(|p| p.end()) {
        Some(end) => Vec3::new(
            f64::from(end.x) + 0.5,
            f64::from(end.y),
            f64::from(end.z) + 0.5,
        ),
        None => t.pos(),
    };
    let from = center_pos(t, from);
    let can_reach = within_target_distance(t, interaction_range(t), pos, state, from);
    (path.is_none() && !can_reach) || (can_reach && can_see_any_side(t, pos, from))
}

/// `canSeeAnyTargetSide`: a line to the middle of one of the block's faces hits the block.
fn can_see_any_side(t: &Tick<'_, '_>, pos: BlockPos, from: Vec3) -> bool {
    let center = crate::level::game_event::center(pos);
    [
        Direction::Down,
        Direction::Up,
        Direction::North,
        Direction::South,
        Direction::West,
        Direction::East,
    ]
    .iter()
    .any(|d| {
        let [x, y, z] = d.normal();
        let to = Vec3::new(
            center.x + 0.5 * f64::from(x),
            center.y + 0.5 * f64::from(y),
            center.z + 0.5 * f64::from(z),
        );
        crate::mob::physics::clip(t.level as &dyn crate::mob::physics::Blocks, from, to)
            .is_some_and(|(hit, _)| hit == pos)
    })
}

/// `setMiddleYPosition`: half the golem's height up.
fn center_pos(t: &Tick<'_, '_>, pos: Vec3) -> Vec3 {
    let bb = t.aabb();
    Vec3::new(pos.x, pos.y + (bb.max_y - bb.min_y) / 2.0, pos.z)
}

/// `getInteractionRange`: 1 once the path is done, else 0.5.
fn interaction_range(t: &Tick<'_, '_>) -> f64 {
    if t.mob_ref().nav.path.as_ref().is_some_and(|p| p.is_done()) {
        1.0
    } else {
        0.5
    }
}

/// `isWithinTargetDistance`: the golem's box at `from` touches the target's collision bounds
/// grown by `distance` (0.5 vertically).
fn within_target_distance(
    t: &Tick<'_, '_>,
    distance: f64,
    pos: BlockPos,
    state: BlockState,
    from: Vec3,
) -> bool {
    let shape = state.collision_shape();
    let Some(first) = shape.first() else {
        return false;
    };
    let mut b = *first;
    for s in shape {
        for i in 0..3 {
            b[i] = b[i].min(s[i]);
            b[i + 3] = b[i + 3].max(s[i + 3]);
        }
    }
    let bounds = Aabb::new(b[0], b[1], b[2], b[3], b[4], b[5])
        .inflate(distance, 0.5, distance)
        .offset(f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
    let bb = t.aabb();
    let (hx, hy, hz) = (
        (bb.max_x - bb.min_x) / 2.0,
        (bb.max_y - bb.min_y) / 2.0,
        (bb.max_z - bb.min_z) / 2.0,
    );
    let moved = Aabb::new(
        from.x - hx,
        from.y - hy,
        from.z - hz,
        from.x + hx,
        from.y + hy,
        from.z + hz,
    );
    bounds.intersects(&moved)
}

/// `onReachedTarget`.
fn on_reached_target(t: &mut Tick<'_, '_>, s: &mut Transport, pos: BlockPos, state: BlockState) {
    let center = center_pos(t, t.pos());
    if !within_target_distance(t, 2.0, pos, state, center) {
        on_start_travelling(t, s);
        return;
    }
    s.ticks += 1;
    // `onTargetInteraction`.
    brain::brain(t).set(M::LookTarget, MemValue::Look(Tracker::block(pos)));
    stop_in_place(t);
    if let Some(reached) = s.interaction {
        on_reached_interaction(t, reached, pos, state, s.ticks);
    }
    if s.ticks >= TARGET_INTERACTION_TIME {
        let container = target_container(t, pos, state);
        match container {
            Some(c) if picking_up(t) => {
                if !c.is_empty(t.level) {
                    pick_up_items(t, s, c);
                } else {
                    stop_targeting(t, s);
                }
            }
            Some(c) if leaving_requirement(t, c) => put_down_item(t, s, c),
            _ => stop_targeting(t, s),
        }
        on_start_travelling(t, s);
    }
}

/// `CopperGolemAi.onReachedTargetInteraction`: the lid opens at tick 1, the sound plays at 9,
/// the lid closes at 60.
fn on_reached_interaction(
    t: &mut Tick<'_, '_>,
    reached: Reached,
    pos: BlockPos,
    state: BlockState,
    ticks: i32,
) {
    let (golem_state, sound) = match reached {
        Reached::PickupItem => (
            GolemState::GettingItem,
            "minecraft:entity.copper_golem.no_item_get",
        ),
        Reached::PickupNoItem => (
            GolemState::GettingNoItem,
            "minecraft:entity.copper_golem.no_item_no_get",
        ),
        Reached::PlaceItem => (
            GolemState::DroppingItem,
            "minecraft:entity.copper_golem.item_drop",
        ),
        Reached::PlaceNoItem => (
            GolemState::DroppingNoItem,
            "minecraft:entity.copper_golem.item_no_drop",
        ),
    };
    if ticks == 1 {
        clear_opened_chest(t);
        if let Some(c) = target_container(t, pos, state) {
            t.cx.actions.push(WorldAction::ContainerOpen(c, true, t.id));
            golem_mut(t.mob()).opened_chest = Some((pos, c));
        }
        set_state(t, golem_state);
    }
    if ticks == 9 {
        crate::mob::kinds::play_sound(t, sound, 1.0, 1.0);
    }
    if ticks == TARGET_INTERACTION_TIME {
        clear_opened_chest(t);
    }
}

/// `CopperGolemAi.onTravelling` and the behaviour's own part of `onStartTravelling`.
fn on_start_travelling(t: &mut Tick<'_, '_>, s: &mut Transport) {
    clear_opened_chest(t);
    set_state(t, GolemState::Idle);
    s.phase = Phase::Travelling;
    s.interaction = None;
    s.ticks = 0;
}

/// `clearOpenedChestPos` (with the lid it opened closing).
pub fn clear_opened_chest(t: &mut Tick<'_, '_>) {
    if let Some((_, c)) = golem_mut(t.mob()).opened_chest.take() {
        t.cx.actions
            .push(WorldAction::ContainerOpen(c, false, t.id));
    }
}

fn set_state(t: &mut Tick<'_, '_>, state: GolemState) {
    let g = golem_mut(t.mob());
    if g.state != state {
        g.state = state;
        t.core_mut().mark_data_dirty();
    }
}

/// `walkTowardsTarget`.
fn walk_towards(t: &mut Tick<'_, '_>, s: &Transport) {
    if let Some((pos, _)) = s.target {
        crate::mob::brain::behavior::set_walk_and_look(t, Tracker::block(pos), SPEED, 0);
    }
}

/// `stopInPlace`.
fn stop_in_place(t: &mut Tick<'_, '_>) {
    let m = t.mob();
    m.nav.stop();
    m.xxa = 0.0;
    m.yya = 0.0;
    m.speed = 0.0;
    let v = t.velocity();
    t.set_velocity(Vec3::new(0.0, v.y, 0.0));
}

/// `isPickingUpItems`: an empty main hand.
fn picking_up(t: &Tick<'_, '_>) -> bool {
    t.mob_ref()
        .equipment
        .get(EquipmentSlot::MainHand)
        .is_empty()
}

/// `isWantedBlock`: copper chests to take from, chests and trapped chests to put into.
fn wanted_block(t: &Tick<'_, '_>, state: BlockState) -> bool {
    if picking_up(t) {
        vanillite_worldgen::behaviour::tags()
            .blocks
            .copper_chests
            .contains(state)
    } else {
        state.block() == vanillite_data::block::ids::CHEST
            || state.block() == vanillite_data::block::ids::TRAPPED_CHEST
    }
}

/// `matchesLeavingItemsRequirement`: empty, or a stack of the held item.
fn leaving_requirement(t: &Tick<'_, '_>, c: ContainerRef) -> bool {
    let held = t.mob_ref().equipment.get(EquipmentSlot::MainHand);
    let size = c.size(t.level);
    c.is_empty(t.level) || (0..size).any(|i| c.get(t.level, i).item == held.item)
}

/// `TransportItemTarget.tryCreatePossibleTarget`: the chest's container as
/// `ChestBlock.getContainer(..., ignoreBeingBlocked = false)` gives it.
fn target_container(t: &Tick<'_, '_>, pos: BlockPos, state: BlockState) -> Option<ContainerRef> {
    let level = t.level;
    if !matches!(level.block_entity(pos), Some(BlockEntity::Container(_)))
        || !matches!(
            kind_of(state.block()),
            Some(ContainerKind::Chest | ContainerKind::TrappedChest)
        )
        || chest_blocked(level, pos)
    {
        return None;
    }
    let Some(other) = crate::block::container::double_chest_partner(level, state, pos) else {
        return Some(ContainerRef::Block(pos));
    };
    if chest_blocked(level, other) {
        return None;
    }
    Some(if state.value("type") == Some("right") {
        ContainerRef::Double(pos, other)
    } else {
        ContainerRef::Double(other, pos)
    })
}

/// The chest and its other half (`getConnectedTargets`).
fn connected(t: &Tick<'_, '_>, pos: BlockPos, state: BlockState) -> [Option<BlockPos>; 2] {
    let other = crate::block::container::double_chest_partner(t.level, state, pos)
        .filter(|&o| target_container(t, o, t.level.block(o)).is_some());
    [Some(pos), other]
}

/// `isAnotherMobInteractingWithTarget` with `CopperGolemAi.shouldQueueForTarget`: a user has
/// one of the chest's halves open.
fn another_mob_interacting(t: &Tick<'_, '_>, pos: BlockPos, state: BlockState) -> bool {
    connected(t, pos, state)
        .into_iter()
        .flatten()
        .any(|p| match t.level.block_entity(p) {
            Some(BlockEntity::Container(c)) => {
                c.openers > 0
                    && matches!(
                        kind_of(t.level.block(p).block()),
                        Some(ContainerKind::Chest | ContainerKind::TrappedChest)
                    )
            }
            _ => false,
        })
}

fn global(t: &Tick<'_, '_>, pos: BlockPos) -> GlobalPos {
    GlobalPos {
        dimension: t.env.dimension.into(),
        pos,
    }
}

fn positions(t: &Tick<'_, '_>, m: M) -> Vec<GlobalPos> {
    match brain_ref(t).get(m) {
        Some(MemValue::GlobalPositions(v)) => v.clone(),
        _ => Vec::new(),
    }
}

/// `getTransportTarget`: the nearest wanted chest in the loaded chunks around.
fn transport_target(t: &mut Tick<'_, '_>) -> Option<(BlockPos, BlockState)> {
    let passenger = t.ents.vehicle_of(t.id).is_some();
    let (h, v) = if passenger { (1, 1) } else { SEARCH };
    let me = t.block_pos();
    let area = Aabb::of_block(me).inflate(f64::from(h), f64::from(v), f64::from(h));
    let visited = positions(t, M::VisitedBlockPositions);
    let unreachable = positions(t, M::UnreachableTransportBlockPositions);
    let radius = h.div_euclid(16) + 1;
    let center = me.chunk();
    let here = t.pos();
    let mut best: Option<(BlockPos, BlockState)> = None;
    let mut closest = f64::from(f32::MAX);
    let mut chests = Vec::new();
    for z in center.z - radius..=center.z + radius {
        for x in center.x - radius..=center.x + radius {
            let chunk = ChunkPos::new(x, z);
            if t.level.chunks.chunk(chunk).is_none() {
                continue;
            }
            chests.clear();
            chests.extend(
                t.level
                    .block_entities
                    .in_chunk(chunk)
                    .filter(|(_, e)| matches!(e, BlockEntity::Container(_)))
                    .map(|(p, _)| p),
            );
            for &pos in &chests {
                let state = t.level.block(pos);
                if !matches!(
                    kind_of(state.block()),
                    Some(ContainerKind::Chest | ContainerKind::TrappedChest)
                ) {
                    continue;
                }
                let distance = crate::level::game_event::center(pos).distance_to_sqr(here);
                if distance >= closest
                    || !area.contains(f64::from(pos.x), f64::from(pos.y), f64::from(pos.z))
                    || target_container(t, pos, state).is_none()
                    || !wanted_block(t, state)
                {
                    continue;
                }
                let seen = connected(t, pos, state).into_iter().flatten().any(|p| {
                    let g = global(t, p);
                    visited.contains(&g) || unreachable.contains(&g)
                });
                if !seen {
                    best = Some((pos, state));
                    closest = distance;
                }
            }
        }
    }
    best
}

/// `setVisitedBlockPos`.
fn set_visited(t: &mut Tick<'_, '_>, s: &mut Transport, pos: BlockPos) {
    let mut visited = positions(t, M::VisitedBlockPositions);
    let g = global(t, pos);
    if !visited.contains(&g) {
        visited.push(g);
    }
    if visited.len() > MAX_VISITED {
        enter_cooldown(t, s);
    } else {
        brain::brain(t).set_with_expiry(
            M::VisitedBlockPositions,
            MemValue::GlobalPositions(visited),
            VISITED_POSITIONS_MEMORY_TIME,
        );
    }
}

/// `markVisitedBlockPosAsUnreachable`.
fn mark_unreachable(t: &mut Tick<'_, '_>, s: &mut Transport, pos: BlockPos) {
    let g = global(t, pos);
    let mut visited = positions(t, M::VisitedBlockPositions);
    visited.retain(|v| *v != g);
    let mut unreachable = positions(t, M::UnreachableTransportBlockPositions);
    if !unreachable.contains(&g) {
        unreachable.push(g);
    }
    if unreachable.len() > MAX_UNREACHABLE {
        enter_cooldown(t, s);
    } else {
        let b = brain::brain(t);
        b.set_with_expiry(
            M::VisitedBlockPositions,
            MemValue::GlobalPositions(visited),
            VISITED_POSITIONS_MEMORY_TIME,
        );
        b.set_with_expiry(
            M::UnreachableTransportBlockPositions,
            MemValue::GlobalPositions(unreachable),
            VISITED_POSITIONS_MEMORY_TIME,
        );
    }
}

/// `stopTargetingCurrentTarget`.
fn stop_targeting(t: &mut Tick<'_, '_>, s: &mut Transport) {
    s.ticks = 0;
    s.target = None;
    t.mob().nav.stop();
    brain::brain(t).erase(M::WalkTarget);
}

/// `clearMemoriesAfterMatchingTargetFound`.
fn clear_memories(t: &mut Tick<'_, '_>, s: &mut Transport) {
    stop_targeting(t, s);
    let b = brain::brain(t);
    b.erase(M::VisitedBlockPositions);
    b.erase(M::UnreachableTransportBlockPositions);
}

/// `enterCooldownAfterNoMatchingTargetFound`.
fn enter_cooldown(t: &mut Tick<'_, '_>, s: &mut Transport) {
    stop_targeting(t, s);
    let b = brain::brain(t);
    b.set(M::TransportItemsCooldownTicks, MemValue::Int(IDLE_COOLDOWN));
    b.erase(M::VisitedBlockPositions);
    b.erase(M::UnreachableTransportBlockPositions);
}

/// `pickUpItems`: up to 16 items of the first stack go into the hand, which always drops.
fn pick_up_items(t: &mut Tick<'_, '_>, s: &mut Transport, c: ContainerRef) {
    let mut taken = ItemStack::default();
    for slot in 0..c.size(t.level) {
        let mut stack = c.get(t.level, slot);
        if !stack.is_empty() {
            taken = stack.split(stack.count.min(MAX_STACK));
            t.cx.actions
                .push(WorldAction::ContainerSlots(c, vec![(slot, stack)]));
            break;
        }
    }
    let m = t.mob();
    m.equipment.set(EquipmentSlot::MainHand, taken);
    m.equipment.set_guaranteed_drop(EquipmentSlot::MainHand);
    crate::mob::equipment::broadcast(t);
    clear_memories(t, s);
}

/// `putDownItem` with `addItemsToContainer` (Vanilla takes the room of a stack off the held
/// count, not the items moved).
fn put_down_item(t: &mut Tick<'_, '_>, s: &mut Transport, c: ContainerRef) {
    let mut held = t.mob_ref().equipment.get(EquipmentSlot::MainHand).clone();
    let mut changed = Vec::new();
    for slot in 0..c.size(t.level) {
        let mut stack = c.get(t.level, slot);
        if stack.is_empty() {
            changed.push((slot, std::mem::take(&mut held)));
            break;
        }
        let max = crate::item::max_stack_size(&stack);
        if stack.same_item_same_components(&held) && stack.count < max {
            let room = max - stack.count;
            stack.count += room.min(held.count);
            held.count -= room;
            changed.push((slot, stack));
            if held.count <= 0 {
                held = ItemStack::default();
                break;
            }
        }
    }
    t.cx.actions.push(WorldAction::ContainerSlots(c, changed));
    let empty = held.is_empty();
    t.mob().equipment.set(EquipmentSlot::MainHand, held);
    crate::mob::equipment::broadcast(t);
    if empty {
        clear_memories(t, s);
    } else {
        stop_targeting(t, s);
    }
}
