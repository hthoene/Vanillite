//! Game events and their listeners (`GameEvent`, `GameEventDispatcher`,
//! `EuclideanGameEventListenerRegistry`): what vibration listeners (sculk sensors, later
//! wardens, allays and shriekers) hear.
//!
//! [`Level::game_event`] posts an event at a position: every listener registered in a section
//! within the event's notification radius whose own radius reaches the event handles it, in
//! Vanilla's order (section X, then Z, then Y, then registration order). Listeners are
//! registered per section of their position; block entity listeners are registered and
//! unregistered by the block entity store as the block entities come and go.
//!
//! Vanilla posts game events from many places; the ones that exist here are noted at their
//! call sites. Code that implements the following must post them with [`Level::game_event_at`]
//! or [`Level::game_event`] like Vanilla does:
//! - placing and breaking blocks by players and entities (`block_place`, `block_destroy`, with
//!   the affected state; `BlockItem.place`, `ServerPlayerGameMode.destroyBlock`,
//!   `Level.destroyBlock`),
//! - levers, buttons, pressure plates, tripwire hooks, doors, trapdoors, fence gates, lecterns,
//!   containers (`block_activate`/`block_deactivate`, `block_open`/`block_close`,
//!   `container_open`/`container_close`, `block_change`),
//! - entities: `step`, `swim`, `flap`, `hit_ground`, `splash`, `projectile_*`, `entity_*`,
//!   `eat`, `drink`, `equip`, `teleport`, `explode`, `prime_fuse`, `fluid_place/pickup`,
//!   `lightning_strike`, `item_interact_*`, `elytra_glide`, `shear`.

use std::sync::OnceLock;

use vanillite_base::hash::FxHashMap;
use vanillite_base::{BlockPos, ChunkPos, SectionPos, Vec3};
use vanillite_data::block::BlockState;
use vanillite_data::registry::GAME_EVENT;
use vanillite_world::entity::EntityId;

use super::Level;

/// `GameEvent.Context`: who caused an event and the block state it affected.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GameEventContext {
    /// The entity that caused it.
    pub source: Option<EntityId>,
    /// The block state it affected.
    pub affected: Option<BlockState>,
}

impl GameEventContext {
    /// `Context.of(sourceEntity)`.
    pub fn of(source: Option<EntityId>) -> Self {
        GameEventContext {
            source,
            affected: None,
        }
    }

    /// `Context.of(sourceEntity, affectedState)`.
    pub fn with_state(source: Option<EntityId>, affected: BlockState) -> Self {
        GameEventContext {
            source,
            affected: Some(affected),
        }
    }
}

macro_rules! game_events {
    ($($field:ident),* $(,)?) => {
        /// The `minecraft:game_event` ids of the Vanilla game events.
        #[derive(Debug)]
        pub struct GameEvents {
            $(#[doc = concat!("`minecraft:", stringify!($field), "`.")] pub $field: u16,)*
        }

        impl GameEvents {
            fn resolve() -> Self {
                let id = |name: &str| {
                    GAME_EVENT
                        .id(&format!("minecraft:{name}"))
                        .unwrap_or(u16::MAX)
                };
                GameEvents { $($field: id(stringify!($field)),)* }
            }
        }
    };
}

game_events!(
    block_activate,
    block_attach,
    block_change,
    block_close,
    block_deactivate,
    block_destroy,
    block_detach,
    block_open,
    block_place,
    bounce,
    container_close,
    container_open,
    drink,
    eat,
    elytra_glide,
    entity_damage,
    entity_die,
    entity_dismount,
    entity_interact,
    entity_mount,
    entity_place,
    entity_action,
    equip,
    explode,
    flap,
    fluid_pickup,
    fluid_place,
    hit_ground,
    instrument_play,
    item_interact_finish,
    item_interact_start,
    jukebox_play,
    jukebox_stop_play,
    lightning_strike,
    note_block_play,
    prime_fuse,
    projectile_land,
    projectile_shoot,
    sculk_sensor_tendrils_clicking,
    shear,
    shriek,
    splash,
    step,
    swim,
    teleport,
    unequip,
    resonate_1,
    resonate_2,
    resonate_3,
    resonate_4,
    resonate_5,
    resonate_6,
    resonate_7,
    resonate_8,
    resonate_9,
    resonate_10,
    resonate_11,
    resonate_12,
    resonate_13,
    resonate_14,
    resonate_15,
);

/// The ids of the Vanilla game events.
pub fn events() -> &'static GameEvents {
    static EVENTS: OnceLock<GameEvents> = OnceLock::new();
    EVENTS.get_or_init(GameEvents::resolve)
}

/// `VibrationSystem.getGameEventFrequency(event)`: 0 for events without a frequency.
pub fn frequency(event: u16) -> i32 {
    static TABLE: OnceLock<Vec<u8>> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let e = events();
        let mut table = vec![0u8; GAME_EVENT.len()];
        let pairs = [
            (e.step, 1),
            (e.swim, 1),
            (e.flap, 1),
            (e.projectile_land, 2),
            (e.hit_ground, 2),
            (e.splash, 2),
            (e.bounce, 2),
            (e.item_interact_finish, 3),
            (e.projectile_shoot, 3),
            (e.instrument_play, 3),
            (e.entity_action, 4),
            (e.elytra_glide, 4),
            (e.unequip, 4),
            (e.entity_dismount, 5),
            (e.equip, 5),
            (e.entity_interact, 6),
            (e.shear, 6),
            (e.entity_mount, 6),
            (e.entity_damage, 7),
            (e.drink, 8),
            (e.eat, 8),
            (e.container_close, 9),
            (e.block_close, 9),
            (e.block_deactivate, 9),
            (e.block_detach, 9),
            (e.container_open, 10),
            (e.block_open, 10),
            (e.block_activate, 10),
            (e.block_attach, 10),
            (e.prime_fuse, 10),
            (e.note_block_play, 10),
            (e.block_change, 11),
            (e.block_destroy, 12),
            (e.fluid_pickup, 12),
            (e.block_place, 13),
            (e.fluid_place, 13),
            (e.entity_place, 14),
            (e.lightning_strike, 14),
            (e.teleport, 14),
            (e.entity_die, 15),
            (e.explode, 15),
        ];
        for (event, frequency) in pairs {
            if let Some(slot) = table.get_mut(usize::from(event)) {
                *slot = frequency;
            }
        }
        for frequency in 1..=15 {
            let event = resonance_event(frequency);
            if let Some(slot) = table.get_mut(usize::from(event)) {
                *slot = frequency as u8;
            }
        }
        table
    });
    table.get(usize::from(event)).map_or(0, |&f| i32::from(f))
}

/// `VibrationSystem.getResonanceEventByFrequency(frequency)` (1 to 15).
pub fn resonance_event(frequency: i32) -> u16 {
    let e = events();
    let all = [
        e.resonate_1,
        e.resonate_2,
        e.resonate_3,
        e.resonate_4,
        e.resonate_5,
        e.resonate_6,
        e.resonate_7,
        e.resonate_8,
        e.resonate_9,
        e.resonate_10,
        e.resonate_11,
        e.resonate_12,
        e.resonate_13,
        e.resonate_14,
        e.resonate_15,
    ];
    all[(frequency.clamp(1, 15) - 1) as usize]
}

/// `GameEvent.notificationRadius()`.
pub fn notification_radius(event: u16) -> i32 {
    let e = events();
    if event == e.jukebox_play || event == e.jukebox_stop_play {
        10
    } else if event == e.shriek {
        32
    } else {
        16
    }
}

/// A registered game event listener.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Listener {
    /// The listener of the block entity at `pos` (`BlockPositionSource`).
    Block {
        /// Position of the block entity.
        pos: BlockPos,
        /// `getListenerRadius()`.
        radius: i32,
    },
    /// The listener of an entity (`DynamicGameEventListener` with an `EntityPositionSource` at
    /// the eyes, the warden's vibration listener), registered in the section its position was
    /// in when it last moved there.
    Entity {
        /// The entity.
        id: EntityId,
        /// The section it is registered in.
        section: SectionPos,
        /// `getListenerRadius()`.
        radius: i32,
    },
}

impl Listener {
    fn radius(self) -> i32 {
        match self {
            Listener::Block { radius, .. } | Listener::Entity { radius, .. } => radius,
        }
    }

    /// The section the listener is registered in.
    fn section(self) -> SectionPos {
        match self {
            Listener::Block { pos, .. } => pos.section(),
            Listener::Entity { section, .. } => section,
        }
    }
}

/// The game event listeners of a level, per section (`LevelChunk.gameEventListenerRegistry
/// Sections`).
#[derive(Debug, Default)]
pub struct GameEventListeners {
    sections: FxHashMap<SectionPos, Vec<Listener>>,
    scratch: Vec<Listener>,
}

impl GameEventListeners {
    /// `register(listener)`: appended to its section's list.
    pub fn register(&mut self, listener: Listener) {
        let list = self.sections.entry(listener.section()).or_default();
        if !list.contains(&listener) {
            list.push(listener);
        }
    }

    /// `unregister(listener)`; an emptied section list goes.
    pub fn unregister(&mut self, listener: Listener) {
        let section = listener.section();
        if let Some(list) = self.sections.get_mut(&section) {
            list.retain(|l| *l != listener);
            if list.is_empty() {
                self.sections.remove(&section);
            }
        }
    }

    /// Number of registered listeners.
    pub fn len(&self) -> usize {
        self.sections.values().map(Vec::len).sum()
    }

    /// Whether no listener is registered.
    pub fn is_empty(&self) -> bool {
        self.sections.is_empty()
    }
}

/// `BlockPos.containing(x, y, z)`.
#[inline]
pub(crate) fn containing(pos: Vec3) -> BlockPos {
    BlockPos::new(
        pos.x.floor() as i32,
        pos.y.floor() as i32,
        pos.z.floor() as i32,
    )
}

/// `a.distSqr(b)` of block positions.
#[inline]
pub(crate) fn dist_sqr(a: BlockPos, b: BlockPos) -> f64 {
    let (dx, dy, dz) = (
        f64::from(a.x - b.x),
        f64::from(a.y - b.y),
        f64::from(a.z - b.z),
    );
    dx * dx + dy * dy + dz * dz
}

impl Level {
    /// The game event listeners.
    pub fn game_event_listeners(&mut self) -> &mut GameEventListeners {
        &mut self.block_entities.listeners
    }

    /// `gameEvent(sourceEntity, event, pos)`: an event at the centre of a block.
    pub fn game_event_at(&mut self, source: Option<EntityId>, event: u16, pos: BlockPos) {
        self.game_event(event, center(pos), GameEventContext::of(source));
    }

    /// `gameEvent(event, pos, context)` at the centre of a block.
    pub fn game_event_block(&mut self, event: u16, pos: BlockPos, context: GameEventContext) {
        self.game_event(event, center(pos), context);
    }

    /// `ServerLevel.gameEvent(event, position, context)` (`GameEventDispatcher.post`).
    pub fn game_event(&mut self, event: u16, position: Vec3, context: GameEventContext) {
        if self.block_entities.listeners.is_empty() {
            return;
        }
        let radius = notification_radius(event);
        let center = containing(position);
        let (min_x, min_y, min_z) = (
            (center.x - radius) >> 4,
            (center.y - radius) >> 4,
            (center.z - radius) >> 4,
        );
        let (max_x, max_y, max_z) = (
            (center.x + radius) >> 4,
            (center.y + radius) >> 4,
            (center.z + radius) >> 4,
        );
        let mut found = std::mem::take(&mut self.block_entities.listeners.scratch);
        found.clear();
        let mut stale = Vec::new();
        for x in min_x..=max_x {
            for z in min_z..=max_z {
                if self.chunks.chunk(ChunkPos::new(x, z)).is_none() {
                    continue;
                }
                for y in min_y..=max_y {
                    let Some(list) = self
                        .block_entities
                        .listeners
                        .sections
                        .get(&SectionPos::new(x, y, z))
                    else {
                        continue;
                    };
                    // `getPostableListenerPosition`.
                    for &l in list {
                        let Some(position) = self.listener_position(l) else {
                            // An entity listener whose entity is gone.
                            stale.push(l);
                            continue;
                        };
                        let r = f64::from(l.radius());
                        if dist_sqr(containing(position), center) <= r * r {
                            found.push(l);
                        }
                    }
                }
            }
        }
        for listener in stale {
            self.block_entities.listeners.unregister(listener);
        }
        for &listener in &found {
            self.handle_game_event(listener, event, context, position);
        }
        found.clear();
        self.block_entities.listeners.scratch = found;
    }

    /// `listener.handleGameEvent(level, event, context, sourcePosition)`.
    fn handle_game_event(
        &mut self,
        listener: Listener,
        event: u16,
        context: GameEventContext,
        source: Vec3,
    ) {
        match listener {
            Listener::Block { pos, .. } => match self.block_entities.get(pos) {
                Some(super::BlockEntity::SculkSensor(_)) => {
                    crate::block::sculk::handle_game_event(self, pos, event, context, source);
                }
                Some(super::BlockEntity::SculkShrieker(_)) => {
                    crate::block::sculk_shrieker::handle_game_event(
                        self, pos, event, context, source,
                    );
                }
                // Catalysts hear deaths through `sculk_catalyst::on_mob_death`.
                _ => {}
            },
            Listener::Entity { id, .. } => {
                crate::mob::boss::warden::handle_game_event(self, id, event, context, source);
            }
        }
    }

    /// The listener's position (`PositionSource.getPosition`): a block entity's centre, an
    /// entity's eyes; `None` for an entity that is gone.
    pub fn listener_position(&self, listener: Listener) -> Option<Vec3> {
        match listener {
            Listener::Block { pos, .. } => Some(center(pos)),
            Listener::Entity { id, .. } => {
                let e = self.entities.store.get(id)?;
                let p = e.pos();
                let eye = crate::mob::boss::warden::listener_eye_height(self, id)?;
                Some(Vec3::new(p.x, p.y + eye, p.z))
            }
        }
    }

    /// The block entity listeners whose own radius reaches `position`, in the sections within
    /// the notification `radius`, whose block entity matches `select`, nearest first
    /// (`DeliveryMode.BY_DISTANCE`), into `out`.
    pub fn block_listeners_near(
        &self,
        position: Vec3,
        radius: i32,
        select: impl Fn(&super::BlockEntity) -> bool,
        out: &mut Vec<(BlockPos, f64)>,
    ) {
        out.clear();
        let center = containing(position);
        for x in (center.x - radius) >> 4..=(center.x + radius) >> 4 {
            for z in (center.z - radius) >> 4..=(center.z + radius) >> 4 {
                for y in (center.y - radius) >> 4..=(center.y + radius) >> 4 {
                    let Some(list) = self
                        .block_entities
                        .listeners
                        .sections
                        .get(&SectionPos::new(x, y, z))
                    else {
                        continue;
                    };
                    for &l in list {
                        let Listener::Block { pos, radius: r } = l else {
                            continue;
                        };
                        let r = f64::from(r);
                        let d = dist_sqr(pos, center);
                        if d <= r * r && self.block_entities.get(pos).is_some_and(&select) {
                            out.push((pos, center_of(pos).distance_to_sqr(position)));
                        }
                    }
                }
            }
        }
        out.sort_by(|a, b| a.1.total_cmp(&b.1));
    }
}

fn center_of(pos: BlockPos) -> Vec3 {
    center(pos)
}

/// `Vec3.atCenterOf(pos)`.
#[inline]
pub(crate) fn center(pos: BlockPos) -> Vec3 {
    Vec3::new(
        f64::from(pos.x) + 0.5,
        f64::from(pos.y) + 0.5,
        f64::from(pos.z) + 0.5,
    )
}
