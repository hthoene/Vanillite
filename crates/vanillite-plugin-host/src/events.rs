//! The event bus: per event kind a list of subscribers in priority and load order, and the
//! typed hook points the server calls.
//!
//! With no subscriber a hook costs one load and branch on the subscription mask.

use std::ffi::c_void;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock};

use vanillite_abi::{self as abi, Status, status};

use crate::api::CallCtx;
use crate::{Inner, PluginHost, SendPtr, Subscriber, WorldAccess};

/// Highest event kind of this ABI version.
const MAX_KIND: u32 = abi::event::PLAYER_ADVANCEMENT_DONE;

/// Event kinds the subscription mask can hold.
const MASK_BITS: u32 = 256;

#[derive(Debug, Default)]
pub(crate) struct Events {
    /// Bit `kind` is set when the kind has subscribers.
    mask: [AtomicU64; (MASK_BITS / 64) as usize],
    /// Subscribers by kind, sorted by priority, then load order. A dispatch takes its list
    /// out of the lock, so handlers may subscribe, unsubscribe and fire events.
    lists: RwLock<Vec<Arc<[Subscriber]>>>,
    /// Names of the plugin event kinds, from [`abi::limits::FIRST_CUSTOM_EVENT`] on.
    custom: RwLock<Vec<String>>,
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

    /// The kind of a plugin event name, registered on first use; `None` when all are taken.
    pub(crate) fn custom_kind(&self, name: &str) -> Option<u32> {
        let mut custom = self.custom.write().unwrap_or_else(PoisonError::into_inner);
        let index = match custom.iter().position(|n| n == name) {
            Some(index) => index,
            None => {
                let room =
                    (abi::limits::LAST_CUSTOM_EVENT - abi::limits::FIRST_CUSTOM_EVENT + 1) as usize;
                if custom.len() >= room {
                    return None;
                }
                custom.push(name.to_owned());
                custom.len() - 1
            }
        };
        u32::try_from(index)
            .ok()
            .map(|i| abi::limits::FIRST_CUSTOM_EVENT + i)
    }

    /// The kind of an already registered plugin event name.
    fn custom_kind_of(&self, name: &str) -> Option<u32> {
        let index = self
            .custom
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .position(|n| n == name)?;
        u32::try_from(index)
            .ok()
            .map(|i| abi::limits::FIRST_CUSTOM_EVENT + i)
    }

    /// The name of a registered plugin event kind.
    pub(crate) fn custom_name(&self, kind: u32) -> Option<String> {
        let index = kind.checked_sub(abi::limits::FIRST_CUSTOM_EVENT)?;
        self.custom
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(usize::try_from(index).ok()?)
            .cloned()
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn add(
        &self,
        plugin: u32,
        kind: u32,
        priority: i32,
        flags: u32,
        callback: abi::EventFn,
        user: *mut c_void,
    ) -> Status {
        let known = (1..=MAX_KIND).contains(&kind)
            || (kind < MASK_BITS && self.custom_name(kind).is_some());
        if !known || flags & !abi::subscribe_flag::IGNORE_CANCELLED != 0 {
            return status::UNSUPPORTED;
        }
        let mut lists = self.lists.write().unwrap_or_else(PoisonError::into_inner);
        let index = usize::try_from(kind).unwrap_or(0);
        if lists.len() <= index {
            lists.resize_with(index + 1, || Arc::from([]));
        }
        let Some(slot) = lists.get_mut(index) else {
            return status::ERROR;
        };
        let mut list = slot.to_vec();
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
                flags,
                callback,
                user: SendPtr(user),
            },
        );
        *slot = list.into();
        self.set_mask(index);
        status::OK
    }

    /// Removes a plugin's subscriptions (failed load, development reload).
    pub(crate) fn remove_plugin(&self, _inner: &Inner, plugin: u32) {
        self.remove(plugin, None);
    }

    /// Removes a plugin's subscriptions of one kind, or of all (`None`).
    pub(crate) fn remove(&self, plugin: u32, only: Option<u32>) {
        let mut lists = self.lists.write().unwrap_or_else(PoisonError::into_inner);
        for m in &self.mask {
            m.store(0, Ordering::Relaxed);
        }
        for (kind, list) in lists.iter_mut().enumerate() {
            if only.is_none_or(|k| usize::try_from(k).ok() == Some(kind))
                && list.iter().any(|s| s.plugin == plugin)
            {
                *list = list
                    .iter()
                    .filter(|s| s.plugin != plugin)
                    .cloned()
                    .collect();
            }
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
        world: Option<&mut dyn WorldAccess>,
        header: *mut abi::EventHeader,
    ) {
        self.dispatch_except(kind, world, header, None);
    }

    /// [`Inner::dispatch`] without the handlers of one plugin (the one firing a plugin
    /// event: it is in a call already, and plugins are never re-entered).
    fn dispatch_except(
        &self,
        kind: u32,
        mut world: Option<&mut dyn WorldAccess>,
        header: *mut abi::EventHeader,
        except: Option<u32>,
    ) {
        let list = self
            .events
            .lists
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(usize::try_from(kind).unwrap_or(usize::MAX))
            .cloned();
        let Some(list) = list else {
            return;
        };
        for subscriber in list.iter() {
            if Some(subscriber.plugin) == except {
                continue;
            }
            let Some(plugin) = self.plugin(subscriber.plugin) else {
                continue;
            };
            let monitor = subscriber.priority >= abi::priority::MONITOR;
            // SAFETY: `header` points to the live record of this dispatch.
            let cancelled = unsafe { (*header).cancelled };
            if cancelled != 0 && subscriber.flags & abi::subscribe_flag::IGNORE_CANCELLED != 0 {
                continue;
            }
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
    /// A player joined; `world` is its world, borrowed by its owner, and `message` the join
    /// message as JSON. Returns the message a plugin put in its place.
    pub fn player_join(
        &self,
        world: &mut dyn WorldAccess,
        player: u128,
        name: &str,
        message: &str,
    ) -> Option<NewText> {
        if !self.wants(abi::event::PLAYER_JOIN) {
            return None;
        }
        let id = world.id();
        let slot = TextSlot::new();
        fire!(
            self,
            abi::event::PLAYER_JOIN,
            Some(world),
            PlayerEvent {
                player: abi::Uuid::from_u128(player),
                name: abi::Str::new(name),
                world: id,
                message: abi::Str::new(message),
                message_out: &slot.record,
            }
        );
        slot.take()
    }

    /// A player left; `message` is the leave message as JSON. Returns the message a plugin
    /// put in its place.
    pub fn player_leave(
        &self,
        world: u32,
        player: u128,
        name: &str,
        message: &str,
    ) -> Option<NewText> {
        if !self.wants(abi::event::PLAYER_LEAVE) {
            return None;
        }
        let slot = TextSlot::new();
        fire!(
            self,
            abi::event::PLAYER_LEAVE,
            None,
            PlayerEvent {
                player: abi::Uuid::from_u128(player),
                name: abi::Str::new(name),
                world,
                message: abi::Str::new(message),
                message_out: &slot.record,
            }
        );
        slot.take()
    }

    /// Whether a plugin registered a plugin message channel (check before copying a payload).
    pub fn wants_channel(&self, channel: &str) -> bool {
        self.wants(abi::event::PLAYER_PLUGIN_MESSAGE)
            && self
                .inner
                .channels
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .any(|(_, c)| c == channel)
    }

    /// A player sent a plugin message on a registered channel.
    pub fn plugin_message(&self, world: u32, player: u128, channel: &str, data: &[u8]) {
        if !self.wants_channel(channel) {
            return;
        }
        fire!(
            self,
            abi::event::PLAYER_PLUGIN_MESSAGE,
            None,
            PluginMessageEvent {
                world,
                player: abi::Uuid::from_u128(player),
                channel: abi::Str::new(channel),
                data: abi::Bytes::new(data),
            }
        );
    }

    /// A player completed an advancement.
    pub fn advancement_done(&self, world: u32, player: u128, advancement: &str) {
        if !self.wants(abi::event::PLAYER_ADVANCEMENT_DONE) {
            return;
        }
        fire!(
            self,
            abi::event::PLAYER_ADVANCEMENT_DONE,
            None,
            AdvancementEvent {
                world,
                player: abi::Uuid::from_u128(player),
                advancement: abi::Str::new(advancement),
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

impl Inner {
    /// Fires a plugin event of a registered kind; returns whether it ended cancelled.
    pub(crate) fn fire_custom(
        &self,
        kind: u32,
        source: (u32, &str),
        data: &[u8],
        world: Option<&mut dyn WorldAccess>,
        cancelled: bool,
    ) -> Result<bool, Status> {
        let name = self
            .events
            .custom_name(kind)
            .ok_or(status::INVALID_ARGUMENT)?;
        if !self.events.wants(kind) {
            return Ok(false);
        }
        let mut event = abi::CustomEvent {
            header: abi::EventHeader {
                cancelled: u32::from(cancelled),
                ..header::<abi::CustomEvent>(kind)
            },
            name: abi::Str::new(&name),
            source: abi::Str::new(source.1),
            data: abi::Bytes::new(data),
        };
        let except = (source.0 != u32::MAX).then_some(source.0);
        self.dispatch_except(kind, world, std::ptr::from_mut(&mut event).cast(), except);
        Ok(event.header.cancelled != 0)
    }
}

impl PluginHost {
    /// Fires a plugin event by name to the plugins' handlers (`source` names the firing
    /// side, e.g. a bridge to other plugin APIs); returns whether it ended cancelled, `None`
    /// when no plugin registered the name.
    pub fn fire_event(
        &self,
        name: &str,
        source: &str,
        data: &[u8],
        cancelled: bool,
    ) -> Option<bool> {
        let kind = self.inner.events.custom_kind_of(name)?;
        self.inner
            .fire_custom(kind, (u32::MAX, source), data, None, cancelled)
            .ok()
    }

    /// The newest service table offered under a name, with its version (for bridges to other
    /// plugin APIs).
    pub fn service(&self, name: &str) -> Option<(u32, *const std::ffi::c_void)> {
        self.inner
            .services
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .rev()
            .find(|s| {
                s.name == name
                    && self
                        .inner
                        .plugin(s.plugin)
                        .is_some_and(|p| p.enabled.load(std::sync::atomic::Ordering::Acquire))
            })
            .map(|s| (s.version, s.table.0.cast_const()))
    }
}
