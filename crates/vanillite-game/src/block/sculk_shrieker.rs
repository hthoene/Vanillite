//! The sculk shrieker (`SculkShriekerBlock`, `SculkShriekerBlockEntity`) and the players'
//! warden warnings (`WardenSpawnTracker`).
//!
//! A shrieker hears players (and their projectiles and dropped items) like a sculk sensor
//! hears vibrations. When it shrieks (90 ticks) the warning level of the players around goes
//! up; a shrieker that can summon then answers: at level 4 a warden emerges nearby, below
//! that the warden's distant sounds play, and the players around get darkness.

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Vec3};
use vanillite_config::GameMode;
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_nbt::{Compound, Tag};
use vanillite_world::entity::EntityId;
use vanillite_worldgen::behaviour::StateExt;

use super::sculk_vibration::{self, Listenable, Vibration, VibrationData};
use super::{BlockBehaviour, sound_id};
use crate::level::game_event::{GameEventContext, center, containing, events};
use crate::level::{BlockEntity, Level, update};

/// `VibrationUser.LISTENER_RADIUS`.
pub const LISTENER_RADIUS: i32 = 8;
/// `SHRIEKING_TICKS`.
const SHRIEKING_TICKS: i32 = 90;
/// `DARKNESS_RADIUS`.
const DARKNESS_RADIUS: f64 = 40.0;
/// `LevelEvent.PARTICLES_SCULK_SHRIEK`.
const SHRIEK_EVENT: i32 = 3007;

/// The data of a `SculkShriekerBlockEntity`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Shrieker {
    /// `warningLevel` of the last shriek.
    pub warning_level: i32,
    /// `VibrationSystem.Data`.
    pub vibration: VibrationData,
}

impl Shrieker {
    /// `loadAdditional`.
    pub fn load(&mut self, nbt: &Compound) {
        self.warning_level = match nbt.get("warning_level") {
            Some(Tag::Int(v)) => *v,
            _ => 0,
        };
        if let Some(Tag::Compound(l)) = nbt.get("listener") {
            self.vibration = VibrationData::load(l, |_| None);
        }
    }

    /// `saveAdditional`.
    pub fn save(&self, nbt: &mut Compound) {
        nbt.insert("warning_level", Tag::Int(self.warning_level));
        nbt.insert("listener", Tag::Compound(self.vibration.save(|_| None)));
    }
}

/// `WardenSpawnTracker` of a player.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WardenSpawnTracker {
    pub ticks_since_last_warning: i32,
    pub warning_level: i32,
    pub cooldown_ticks: i32,
}

impl WardenSpawnTracker {
    /// `MAX_WARNING_LEVEL`.
    pub const MAX_WARNING_LEVEL: i32 = 4;

    /// `tick`: the level drops by one every 12000 ticks without a warning.
    pub fn tick(&mut self) {
        if self.ticks_since_last_warning >= 12000 {
            self.set_warning_level(self.warning_level - 1);
            self.ticks_since_last_warning = 0;
        } else {
            self.ticks_since_last_warning += 1;
        }
        if self.cooldown_ticks > 0 {
            self.cooldown_ticks -= 1;
        }
    }

    /// `reset`.
    pub fn reset(&mut self) {
        *self = WardenSpawnTracker::default();
    }

    /// `onCooldown`.
    pub fn on_cooldown(&self) -> bool {
        self.cooldown_ticks > 0
    }

    /// `increaseWarningLevel`.
    pub fn increase_warning_level(&mut self) {
        if !self.on_cooldown() {
            self.ticks_since_last_warning = 0;
            self.cooldown_ticks = 200;
            self.set_warning_level(self.warning_level + 1);
        }
    }

    /// `setWarningLevel`.
    pub fn set_warning_level(&mut self, level: i32) {
        self.warning_level = level.clamp(0, Self::MAX_WARNING_LEVEL);
    }

    /// The `warden_spawn_tracker` NBT.
    pub fn save(&self) -> Compound {
        let mut c = Compound::new();
        c.insert(
            "ticks_since_last_warning",
            Tag::Int(self.ticks_since_last_warning),
        );
        c.insert("warning_level", Tag::Int(self.warning_level));
        c.insert("cooldown_ticks", Tag::Int(self.cooldown_ticks));
        c
    }

    /// Reads the `warden_spawn_tracker` NBT.
    pub fn load(tag: Option<&Tag>) -> WardenSpawnTracker {
        let Some(Tag::Compound(c)) = tag else {
            return WardenSpawnTracker::default();
        };
        let int = |k: &str| match c.get(k) {
            Some(Tag::Int(v)) => (*v).max(0),
            _ => 0,
        };
        WardenSpawnTracker {
            ticks_since_last_warning: int("ticks_since_last_warning"),
            warning_level: int("warning_level").min(Self::MAX_WARNING_LEVEL),
            cooldown_ticks: int("cooldown_ticks"),
        }
    }
}

/// Fills the sculk shrieker behaviours.
pub(super) fn fill(block: BlockId, b: &mut BlockBehaviour) {
    if block == ids::SCULK_SHRIEKER {
        b.tick = Some(tick);
    }
}

fn is_shrieking(state: BlockState) -> bool {
    state.value("shrieking") == Some("true")
}

/// `SculkShriekerBlock.tick`: the shriek ends and the shrieker answers.
fn tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    if is_shrieking(state) {
        level.set_block_and_update(pos, state.set("shrieking", "false"));
        try_respond(level, pos);
    }
}

/// `SculkShriekerBlock.stepOn`: a player stepping on the shrieker makes it shriek. Called by
/// entity movement.
pub fn step_on(level: &mut Level, pos: BlockPos, entity: EntityId) {
    if let Some(player) = try_get_player(level, Some(entity))
        && matches!(level.block_entity(pos), Some(BlockEntity::SculkShrieker(_)))
    {
        try_shriek(level, pos, Some(player));
    }
}

/// `preRemoveSideEffects`: a shrieker removed while shrieking answers at once.
pub fn pre_remove(level: &mut Level, pos: BlockPos, state: BlockState) {
    if is_shrieking(state) {
        try_respond(level, pos);
    }
}

/// `SculkShriekerBlockEntity.tryGetPlayer`: the player behind an entity (itself, the player
/// steering it, the owner of its projectile, the thrower of a dropped item).
pub fn try_get_player(level: &Level, entity: Option<EntityId>) -> Option<EntityId> {
    let entity = entity?;
    if level.entities.player(entity).is_some() {
        return Some(entity);
    }
    // `getControllingPassenger() instanceof ServerPlayer`: a ridden horse, boat, pig, ...
    if let Some(&rider) = level.entities.riding.passengers_of(entity).first()
        && level.entities.player(rider).is_some()
        && crate::riding::controls(&level.entities, rider, entity)
    {
        return Some(rider);
    }
    if let Some(owner) = sculk_vibration::projectile_owner(level, Some(entity)) {
        return level.entities.player(owner).is_some().then_some(owner);
    }
    let thrower = level.entities.item(entity)?.thrower?;
    level.entities.players.iter().map(|(id, _)| id).find(|&id| {
        level
            .entities
            .store
            .get(id)
            .is_some_and(|e| e.uuid() == thrower)
    })
}

/// `tryShriek(level, player)`.
pub fn try_shriek(level: &mut Level, pos: BlockPos, player: Option<EntityId>) {
    let Some(player) = player else {
        return;
    };
    let state = level.block(pos);
    if is_shrieking(state) {
        return;
    }
    if let Some(BlockEntity::SculkShrieker(s)) = level.block_entity_mut(pos) {
        s.warning_level = 0;
    }
    if !can_respond(level, state) || try_to_warn(level, pos, player) {
        shriek(level, pos, Some(player));
    }
}

/// `tryToWarn`.
fn try_to_warn(level: &mut Level, pos: BlockPos, player: EntityId) -> bool {
    let Some(warning) = try_warn(level, pos, player) else {
        return false;
    };
    if let Some(BlockEntity::SculkShrieker(s)) = level.block_entity_mut(pos) {
        s.warning_level = warning;
    }
    true
}

/// `WardenSpawnTracker.tryWarn`: no warden within 24 blocks and no player around on cooldown;
/// the highest warning level of the players around (and the trigger) goes up and is copied
/// to all of them.
pub fn try_warn(level: &mut Level, pos: BlockPos, trigger: EntityId) -> Option<i32> {
    let origin = center(pos);
    // `hasNearbyWarden`: `AABB.ofSize(center, 48, 48, 48)`.
    let area = vanillite_base::Aabb::new(
        origin.x - 24.0,
        origin.y - 24.0,
        origin.z - 24.0,
        origin.x + 24.0,
        origin.y + 24.0,
        origin.z + 24.0,
    );
    let mut warden = false;
    level.entities.store.for_each_in(&area, false, |id| {
        warden |= level.entities.store.get(id).is_some_and(|e| {
            e.kind() == vanillite_data::entity::ids::WARDEN && e.aabb().intersects(&area)
        });
    });
    if warden {
        return None;
    }
    let mut players: Vec<EntityId> = level
        .entities
        .players
        .iter()
        .filter(|(_, p)| p.game_mode != GameMode::Spectator && !p.survival.is_dead_or_dying())
        .filter_map(|(id, _)| {
            let e = level.entities.store.get(id)?;
            (e.pos().distance_to_sqr(origin) < 16.0 * 16.0).then_some(id)
        })
        .collect();
    if !players.contains(&trigger) {
        players.push(trigger);
    }
    let tracker = |id: EntityId| {
        level
            .entities
            .player(id)
            .map(|p| p.survival.warden_spawn_tracker)
    };
    if players
        .iter()
        .any(|&id| tracker(id).is_some_and(|t| t.on_cooldown()))
    {
        return None;
    }
    // `max(comparingInt(getWarningLevel))`: the last of the highest.
    let mut highest: Option<WardenSpawnTracker> = None;
    for &id in &players {
        if let Some(t) = tracker(id)
            && highest.is_none_or(|h| t.warning_level >= h.warning_level)
        {
            highest = Some(t);
        }
    }
    let mut spawn_tracker = highest?;
    spawn_tracker.increase_warning_level();
    for &id in &players {
        if let Some(p) = level.entities.player_mut(id) {
            p.survival.warden_spawn_tracker = spawn_tracker;
        }
    }
    Some(spawn_tracker.warning_level)
}

/// `shriek(level, sourceEntity)`.
fn shriek(level: &mut Level, pos: BlockPos, source: Option<EntityId>) {
    let state = level.block(pos);
    level.set_block(pos, state.set("shrieking", "true"), update::CLIENTS);
    level.schedule_block_tick(pos, state.block(), SHRIEKING_TICKS);
    level.level_event(None, SHRIEK_EVENT, pos, 0);
    level.game_event_block(events().shriek, pos, GameEventContext::of(source));
}

/// `canRespond`: a shrieker that can summon, not in peaceful, with `spawnWardens`.
fn can_respond(level: &Level, state: BlockState) -> bool {
    state.value("can_summon") == Some("true")
        && !level.settings.peaceful
        && level.settings.spawn_wardens
}

/// `tryRespond`: a summoning shrieker that warned answers with a warden (level 4) or the
/// warden's distant sounds, and darkness around.
pub fn try_respond(level: &mut Level, pos: BlockPos) {
    let state = level.block(pos);
    let warning = match level.block_entity(pos) {
        Some(BlockEntity::SculkShrieker(s)) => s.warning_level,
        _ => return,
    };
    if !can_respond(level, state) || warning <= 0 {
        return;
    }
    if !try_summon_warden(level, pos, warning) {
        play_warden_reply_sound(level, pos, warning);
    }
    crate::mob::boss::warden::apply_darkness_around(
        &mut level.entities,
        center(pos),
        None,
        DARKNESS_RADIUS,
    );
}

/// `playWardenReplySound`.
fn play_warden_reply_sound(level: &mut Level, pos: BlockPos, warning: i32) {
    let sound = match warning {
        1 => "entity.warden.nearby_close",
        2 => "entity.warden.nearby_closer",
        3 => "entity.warden.nearby_closest",
        4 => "entity.warden.listening_angry",
        _ => return,
    };
    let random = level.random();
    let mut offset = || random.next_int_bounded(21) - 10;
    let (x, y, z) = (pos.x + offset(), pos.y + offset(), pos.z + offset());
    level.play_sound(
        None,
        f64::from(x),
        f64::from(y),
        f64::from(z),
        sound_id(sound),
        crate::level::SOUND_HOSTILE,
        5.0,
        1.0,
    );
}

/// `trySummonWarden`: `SpawnUtil.trySpawnMob(WARDEN, TRIGGERED, level, pos, 20, 5, 6,
/// ON_TOP_OF_COLLIDER, false)` at warning level 4.
fn try_summon_warden(level: &mut Level, pos: BlockPos, warning: i32) -> bool {
    warning >= WardenSpawnTracker::MAX_WARNING_LEVEL
        && crate::mob::boss::warden::try_spawn_triggered(level, pos, 20, 5, 6)
}

/// `VibrationSystem.Listener.handleGameEvent` of the shrieker at `pos`.
pub(crate) fn handle_game_event(
    level: &mut Level,
    pos: BlockPos,
    event: u16,
    context: GameEventContext,
    source: Vec3,
) {
    match level.block_entity(pos) {
        Some(BlockEntity::SculkShrieker(s)) if s.vibration.current.is_none() => {}
        _ => return,
    }
    if !sculk_vibration::is_valid_vibration(level, event, context, Listenable::Shrieker) {
        return;
    }
    let destination = center(pos);
    // `canReceiveVibration`: not shrieking and a player behind the event.
    if is_shrieking(level.block(pos)) || try_get_player(level, context.source).is_none() {
        return;
    }
    if sculk_vibration::is_occluded(level, source, destination) {
        return;
    }
    let time = level.game_time();
    let projectile_owner = sculk_vibration::projectile_owner(level, context.source);
    if let Some(BlockEntity::SculkShrieker(s)) = level.block_entity_mut(pos) {
        let distance = source.distance_to_sqr(destination).sqrt() as f32;
        s.vibration.add_candidate(
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

/// The block entity ticker (`VibrationSystem.Ticker.tick`, needing the chunks around ticking).
pub fn tick_block_entity(level: &mut Level, pos: BlockPos, _: BlockState) {
    let time = level.game_time();
    let Some(BlockEntity::SculkShrieker(s)) = level.block_entity_mut(pos) else {
        return;
    };
    s.vibration.try_select(time);
    let Some(vibration) = s.vibration.current else {
        return;
    };
    s.vibration.travel_time = (s.vibration.travel_time - 1).max(0);
    if s.vibration.travel_time > 0 {
        return;
    }
    if !sculk_vibration::adjacent_chunks_ticking(level, pos) {
        return;
    }
    let _ = containing(vibration.pos);
    // `onReceiveVibration`.
    let player = try_get_player(level, vibration.projectile_owner.or(vibration.source));
    try_shriek(level, pos, player);
    if let Some(BlockEntity::SculkShrieker(s)) = level.block_entity_mut(pos) {
        s.vibration.current = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warning_level_rises_with_cooldown_and_decays() {
        let mut t = WardenSpawnTracker::default();
        t.increase_warning_level();
        assert_eq!(t.warning_level, 1);
        // On cooldown for 200 ticks: no further increase.
        t.increase_warning_level();
        assert_eq!(t.warning_level, 1);
        for _ in 0..200 {
            t.tick();
        }
        assert!(!t.on_cooldown());
        t.increase_warning_level();
        assert_eq!(t.warning_level, 2);
        // 12000 ticks without warning, then one more tick drops a level.
        for _ in 0..12001 {
            t.tick();
        }
        assert_eq!(t.warning_level, 1);
        t.set_warning_level(9);
        assert_eq!(t.warning_level, WardenSpawnTracker::MAX_WARNING_LEVEL);
    }

    #[test]
    fn tracker_nbt_round_trip() {
        let t = WardenSpawnTracker {
            ticks_since_last_warning: 17,
            warning_level: 3,
            cooldown_ticks: 5,
        };
        assert_eq!(WardenSpawnTracker::load(Some(&Tag::Compound(t.save()))), t);
    }
}
