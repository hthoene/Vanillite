//! The event bus: per event kind a list of subscribers in priority and load order, and the
//! typed hook points the server calls.
//!
//! With no subscriber a hook costs one load and branch on the subscription mask.

use std::ffi::c_void;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{PoisonError, RwLock};

use vanillite_abi::{self as abi, Status, status};

use crate::api::CallCtx;
use crate::{Inner, PluginHost, SendPtr, Subscriber, WorldAccess};

/// Highest event kind of this ABI version.
const MAX_KIND: u32 = abi::event::PLAYER_INTERACT;

/// Event kinds the subscription mask can hold.
const MASK_BITS: u32 = 256;

#[derive(Debug, Default)]
pub(crate) struct Events {
    /// Bit `kind` is set when the kind has subscribers.
    mask: [AtomicU64; (MASK_BITS / 64) as usize],
    /// Subscribers by kind, sorted by priority, then load order.
    lists: RwLock<Vec<Vec<Subscriber>>>,
}

impl Events {
    #[inline]
    pub(crate) fn wants(&self, kind: u32) -> bool {
        self.mask
            .get((kind / 64) as usize)
            .is_some_and(|m| m.load(Ordering::Relaxed) & (1 << (kind % 64)) != 0)
    }

    fn set_mask(&self, kind: usize) {
        if let Some(m) = self.mask.get(kind / 64) {
            m.fetch_or(1 << (kind % 64), Ordering::Relaxed);
        }
    }

    pub(crate) fn add(
        &self,
        plugin: u32,
        kind: u32,
        priority: i32,
        callback: abi::EventFn,
        user: *mut c_void,
    ) -> Status {
        if !(1..=MAX_KIND.min(MASK_BITS - 1)).contains(&kind) {
            return status::UNSUPPORTED;
        }
        let mut lists = self.lists.write().unwrap_or_else(PoisonError::into_inner);
        let index = usize::try_from(kind).unwrap_or(0);
        if lists.len() <= index {
            lists.resize_with(index + 1, Vec::new);
        }
        let Some(list) = lists.get_mut(index) else {
            return status::ERROR;
        };
        // After every subscriber with a lower or equal priority, and among equal priorities
        // after earlier plugins.
        let at = list
            .iter()
            .position(|s| (s.priority, s.plugin) > (priority, plugin))
            .unwrap_or(list.len());
        list.insert(
            at,
            Subscriber {
                plugin,
                priority,
                callback,
                user: SendPtr(user),
            },
        );
        self.set_mask(index);
        status::OK
    }

    /// Removes a plugin's subscriptions (failed load, development reload).
    pub(crate) fn remove_plugin(&self, _inner: &Inner, plugin: u32) {
        let mut lists = self.lists.write().unwrap_or_else(PoisonError::into_inner);
        for m in &self.mask {
            m.store(0, Ordering::Relaxed);
        }
        for (kind, list) in lists.iter_mut().enumerate() {
            list.retain(|s| s.plugin != plugin);
            if !list.is_empty() {
                self.set_mask(kind);
            }
        }
    }
}

impl Inner {
    /// Delivers an event to every subscriber of its kind. The record behind `header` stays
    /// valid and is changed in place by the handlers.
    fn dispatch(
        &self,
        kind: u32,
        mut world: Option<&mut dyn WorldAccess>,
        header: *mut abi::EventHeader,
    ) {
        let lists = self
            .events
            .lists
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let Some(list) = lists.get(usize::try_from(kind).unwrap_or(usize::MAX)) else {
            return;
        };
        for subscriber in list {
            let Some(plugin) = self.plugin(subscriber.plugin) else {
                continue;
            };
            let monitor = subscriber.priority >= abi::priority::MONITOR;
            // SAFETY: `header` points to the live record of this dispatch.
            let cancelled = unsafe { (*header).cancelled };
            let mut ctx = CallCtx::new(crate::api::reborrow(&mut world), &plugin);
            plugin.call("an event handler", || {
                // SAFETY: the plugin's handler and user data live as long as its library; the
                // record and context are valid for the call.
                unsafe { (subscriber.callback)(subscriber.user.0, ctx.as_raw(), header) }
            });
            if monitor {
                // Monitors observe the outcome; they cannot change it.
                // SAFETY: as above.
                unsafe { (*header).cancelled = cancelled };
            }
        }
    }
}

fn header<T>(kind: u32) -> abi::EventHeader {
    abi::EventHeader {
        size: abi::size_of::<T>(),
        kind,
        cancelled: 0,
    }
}

/// Builds the record of an event, dispatches it and returns it with the handlers' changes.
macro_rules! fire {
    ($self:ident, $kind:expr, $world:expr, $ty:ident { $($field:ident $(: $value:expr)?),* $(,)? }) => {{
        let mut event = abi::$ty {
            header: header::<abi::$ty>($kind),
            $($field $(: $value)?),*
        };
        $self
            .inner
            .dispatch($kind, $world, std::ptr::from_mut(&mut event).cast());
        event
    }};
}

impl PluginHost {
    /// A player joined; `world` is its world, borrowed by its owner.
    pub fn player_join(&self, world: &mut dyn WorldAccess, player: u128, name: &str) {
        if !self.wants(abi::event::PLAYER_JOIN) {
            return;
        }
        let id = world.id();
        fire!(
            self,
            abi::event::PLAYER_JOIN,
            Some(world),
            PlayerEvent {
                player: abi::Uuid::from_u128(player),
                name: abi::Str::new(name),
                world: id,
            }
        );
    }

    /// A player left.
    pub fn player_leave(&self, world: u32, player: u128, name: &str) {
        if !self.wants(abi::event::PLAYER_LEAVE) {
            return;
        }
        fire!(
            self,
            abi::event::PLAYER_LEAVE,
            None,
            PlayerEvent {
                player: abi::Uuid::from_u128(player),
                name: abi::Str::new(name),
                world,
            }
        );
    }

    /// A player sends a chat message. Returns `false` when a plugin cancelled it.
    pub fn player_chat(&self, player: u128, name: &str, message: &str) -> bool {
        self.player_text(abi::event::PLAYER_CHAT, player, name, message)
    }

    /// A player runs a command (without `/`). Returns `false` when a plugin cancelled it.
    pub fn player_command(&self, player: u128, name: &str, command: &str) -> bool {
        self.player_text(abi::event::PLAYER_COMMAND, player, name, command)
    }

    fn player_text(&self, kind: u32, player: u128, name: &str, text: &str) -> bool {
        if !self.wants(kind) {
            return true;
        }
        let event = fire!(
            self,
            kind,
            None,
            PlayerTextEvent {
                player: abi::Uuid::from_u128(player),
                name: abi::Str::new(name),
                text: abi::Str::new(text),
            }
        );
        event.header.cancelled == 0
    }

    /// The players of a world that moved this tick: `(player, from, to)`.
    pub fn players_moved(&self, world: &mut dyn WorldAccess, moves: &[(u128, [f64; 3], [f64; 3])]) {
        if moves.is_empty() || !self.wants(abi::event::PLAYERS_MOVED) {
            return;
        }
        let v = |[x, y, z]: [f64; 3]| abi::Vec3 { x, y, z };
        let records: Vec<abi::PlayerMove> = moves
            .iter()
            .map(|&(player, from, to)| abi::PlayerMove {
                player: abi::Uuid::from_u128(player),
                from: v(from),
                to: v(to),
            })
            .collect();
        let id = world.id();
        fire!(
            self,
            abi::event::PLAYERS_MOVED,
            Some(world),
            PlayersMovedEvent {
                world: id,
                moves: records.as_ptr(),
                count: records.len(),
                stride: size_of::<abi::PlayerMove>(),
            }
        );
    }

    /// An entity is about to take `amount` damage. Returns the damage to apply, `None` when a
    /// plugin cancelled it.
    pub fn entity_damage(
        &self,
        world: u32,
        entity: u128,
        entity_type: &str,
        damage_type: &str,
        amount: f32,
    ) -> Option<f32> {
        if !self.wants(abi::event::ENTITY_DAMAGE) {
            return Some(amount);
        }
        let event = fire!(
            self,
            abi::event::ENTITY_DAMAGE,
            None,
            EntityDamageEvent {
                world,
                entity: abi::Uuid::from_u128(entity),
                entity_type: abi::Str::new(entity_type),
                damage_type: abi::Str::new(damage_type),
                amount,
            }
        );
        (event.header.cancelled == 0).then_some(event.amount)
    }

    /// A player event about a block (`PLAYER_BED_ENTER`, `PLAYER_BED_LEAVE`,
    /// `PLAYER_SPAWN_CHANGE`). Returns `false` when a plugin cancelled it.
    pub fn player_block(
        &self,
        kind: u32,
        world: u32,
        player: u128,
        pos: [i32; 3],
        state: &str,
    ) -> bool {
        if !self.wants(kind) {
            return true;
        }
        let event = fire!(
            self,
            kind,
            None,
            BlockEvent {
                world,
                player: abi::Uuid::from_u128(player),
                pos: abi::BlockPos {
                    x: pos[0],
                    y: pos[1],
                    z: pos[2],
                },
                state: abi::Str::new(state),
            }
        );
        event.header.cancelled == 0
    }

    /// A player is about to break (`place == false`) or place a block. Returns `false` when a
    /// plugin cancelled it.
    pub fn block_change(
        &self,
        place: bool,
        world: u32,
        player: u128,
        pos: [i32; 3],
        state: &str,
    ) -> bool {
        let kind = if place {
            abi::event::BLOCK_PLACE
        } else {
            abi::event::BLOCK_BREAK
        };
        if !self.wants(kind) {
            return true;
        }
        let event = fire!(
            self,
            kind,
            None,
            BlockEvent {
                world,
                player: abi::Uuid::from_u128(player),
                pos: abi::BlockPos {
                    x: pos[0],
                    y: pos[1],
                    z: pos[2],
                },
                state: abi::Str::new(state),
            }
        );
        event.header.cancelled == 0
    }
}

/// A text a handler put in place of the server's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NewText {
    /// Plain text.
    Plain(String),
    /// A JSON text component.
    Json(String),
}

/// Collects the last text set through a [`abi::TextOut`].
struct TextSlot {
    record: abi::TextOut,
    text: Box<Option<NewText>>,
}

impl TextSlot {
    fn new() -> TextSlot {
        let mut text = Box::new(None);
        TextSlot {
            record: abi::TextOut {
                size: abi::size_of::<abi::TextOut>(),
                state: std::ptr::from_mut::<Option<NewText>>(&mut *text).cast(),
                set: set_text,
            },
            text,
        }
    }

    fn take(self) -> Option<NewText> {
        let TextSlot { record, text } = self;
        let _ = record;
        *text
    }
}

/// [`abi::TextSetFn`] of a [`TextSlot`].
unsafe extern "C" fn set_text(state: *mut c_void, text: abi::Str, format: u32) {
    let _ = panic::catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: the host passes its own slot as state for the dispatch.
        let Some(slot) = (unsafe { state.cast::<Option<NewText>>().as_mut() }) else {
            return;
        };
        // SAFETY: the plugin's string is valid for the call.
        let Some(text) = (unsafe { crate::api::str_checked(text) }) else {
            return;
        };
        *slot = match format {
            abi::text_format::PLAIN => Some(NewText::Plain(text.to_owned())),
            abi::text_format::JSON => Some(NewText::Json(text.to_owned())),
            _ => return,
        };
    }));
}

/// What the plugins decided about a login.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginOutcome {
    /// Whether the login is refused.
    pub refused: bool,
    /// The refusal message a plugin set; `None` keeps the server's.
    pub message: Option<NewText>,
}

/// What the plugins changed of a player's death.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayerDeath {
    /// The death message a plugin set (an empty one shows none); `None` keeps the server's.
    pub message: Option<NewText>,
    /// Whether the player keeps its inventory and experience.
    pub keep_inventory: bool,
}

/// The block of an interaction on a block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InteractTarget<'a> {
    /// The block.
    pub pos: [i32; 3],
    /// The clicked face (0 down, 1 up, 2 north, 3 south, 4 west, 5 east).
    pub face: u32,
    /// Its state, e.g. `minecraft:oak_door[...]`.
    pub state: &'a str,
}

fn uuid(v: Option<u128>) -> abi::Uuid {
    v.map_or(abi::Uuid::NIL, abi::Uuid::from_u128)
}

fn block_pos([x, y, z]: [i32; 3]) -> abi::BlockPos {
    abi::BlockPos { x, y, z }
}

impl PluginHost {
    /// A player logs in after the server's checks: `reason` is one of
    /// [`abi::login_reason`], `message` the refusal as JSON (empty when allowed). Runs on the
    /// global owner. `None` when no plugin listens.
    pub fn player_login(
        &self,
        player: u128,
        name: &str,
        address: &str,
        reason: u32,
        message: &str,
    ) -> Option<LoginOutcome> {
        if !self.wants(abi::event::PLAYER_LOGIN) {
            return None;
        }
        let slot = TextSlot::new();
        let kind = abi::event::PLAYER_LOGIN;
        let mut event = abi::LoginEvent {
            header: abi::EventHeader {
                cancelled: u32::from(reason != abi::login_reason::ALLOWED),
                ..header::<abi::LoginEvent>(kind)
            },
            player: abi::Uuid::from_u128(player),
            name: abi::Str::new(name),
            address: abi::Str::new(address),
            reason,
            message: abi::Str::new(message),
            message_out: &raw const slot.record,
        };
        self.inner
            .dispatch(kind, None, std::ptr::from_mut(&mut event).cast());
        Some(LoginOutcome {
            refused: event.header.cancelled != 0,
            message: slot.take(),
        })
    }

    /// A player died; `message` is its death message as JSON (empty when none is shown).
    /// `None` when no plugin listens.
    pub fn player_death(
        &self,
        world: u32,
        player: u128,
        damage_type: &str,
        killer: Option<u128>,
        message: &str,
        keep_inventory: bool,
    ) -> Option<PlayerDeath> {
        if !self.wants(abi::event::PLAYER_DEATH) {
            return None;
        }
        let slot = TextSlot::new();
        let event = fire!(
            self,
            abi::event::PLAYER_DEATH,
            None,
            PlayerDeathEvent {
                world,
                player: abi::Uuid::from_u128(player),
                damage_type: abi::Str::new(damage_type),
                killer: uuid(killer),
                message: abi::Str::new(message),
                message_out: &raw const slot.record,
                keep_inventory: u32::from(keep_inventory),
            }
        );
        Some(PlayerDeath {
            message: slot.take(),
            keep_inventory: event.keep_inventory != 0,
        })
    }

    /// A mob died.
    pub fn entity_death(
        &self,
        world: u32,
        entity: u128,
        entity_type: &str,
        damage_type: &str,
        killer: Option<u128>,
    ) {
        if !self.wants(abi::event::ENTITY_DEATH) {
            return;
        }
        fire!(
            self,
            abi::event::ENTITY_DEATH,
            None,
            EntityDeathEvent {
                world,
                entity: abi::Uuid::from_u128(entity),
                entity_type: abi::Str::new(entity_type),
                damage_type: abi::Str::new(damage_type),
                killer: uuid(killer),
            }
        );
    }

    /// A player respawned (`respawn`) or arrived from another world, now in `world` at `pos`.
    pub fn player_world(
        &self,
        respawn: bool,
        world: &mut dyn WorldAccess,
        player: u128,
        from: u32,
        pos: [f64; 3],
    ) {
        let kind = if respawn {
            abi::event::PLAYER_RESPAWN
        } else {
            abi::event::PLAYER_CHANGE_WORLD
        };
        if !self.wants(kind) {
            return;
        }
        let id = world.id();
        let [x, y, z] = pos;
        fire!(
            self,
            kind,
            Some(world),
            PlayerWorldEvent {
                player: abi::Uuid::from_u128(player),
                from,
                world: id,
                pos: abi::Vec3 { x, y, z },
            }
        );
    }

    /// A player's game mode is about to change (modes from [`abi::game_mode`]). Returns
    /// `false` when a plugin cancelled it.
    pub fn game_mode_change(&self, world: u32, player: u128, from: u32, to: u32) -> bool {
        if !self.wants(abi::event::PLAYER_GAME_MODE_CHANGE) {
            return true;
        }
        let event = fire!(
            self,
            abi::event::PLAYER_GAME_MODE_CHANGE,
            None,
            GameModeEvent {
                world,
                player: abi::Uuid::from_u128(player),
                from,
                to,
            }
        );
        event.header.cancelled == 0
    }

    /// A player uses `item` in `hand` (from [`abi::hand`]) on a block or in the air. Returns
    /// `false` when a plugin cancelled it.
    pub fn player_interact(
        &self,
        world: u32,
        player: u128,
        hand: u32,
        item: &str,
        block: Option<InteractTarget<'_>>,
    ) -> bool {
        if !self.wants(abi::event::PLAYER_INTERACT) {
            return true;
        }
        let event = fire!(
            self,
            abi::event::PLAYER_INTERACT,
            None,
            InteractEvent {
                world,
                player: abi::Uuid::from_u128(player),
                action: if block.is_some() {
                    abi::interact::USE_ON_BLOCK
                } else {
                    abi::interact::USE_ITEM
                },
                hand,
                item: abi::Str::new(item),
                pos: block_pos(block.map_or([0; 3], |b| b.pos)),
                face: block.map_or(0, |b| b.face),
                state: abi::Str::new(block.map_or("", |b| b.state)),
            }
        );
        event.header.cancelled == 0
    }
}
