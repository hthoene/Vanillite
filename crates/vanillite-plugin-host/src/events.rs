//! The event bus: per event kind a list of subscribers in priority and load order, and the
//! typed hook points the server calls.
//!
//! With no subscriber a hook costs one load and branch on the subscription mask.

use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{PoisonError, RwLock};

use vanillite_abi::{self as abi, Status, status};

use crate::api::CallCtx;
use crate::{Inner, PluginHost, SendPtr, Subscriber, WorldAccess};

/// Highest event kind of this ABI version.
const MAX_KIND: u32 = abi::event::BLOCK_PLACE;

#[derive(Debug, Default)]
pub(crate) struct Events {
    /// Bit `kind` is set when the kind has subscribers.
    mask: AtomicU64,
    /// Subscribers by kind, sorted by priority, then load order.
    lists: RwLock<Vec<Vec<Subscriber>>>,
}

impl Events {
    #[inline]
    pub(crate) fn wants(&self, kind: u32) -> bool {
        kind < 64 && self.mask.load(Ordering::Relaxed) & (1 << kind) != 0
    }

    pub(crate) fn add(
        &self,
        plugin: u32,
        kind: u32,
        priority: i32,
        callback: abi::EventFn,
        user: *mut c_void,
    ) -> Status {
        if !(1..=MAX_KIND).contains(&kind) {
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
        self.mask.fetch_or(1 << kind, Ordering::Relaxed);
        status::OK
    }

    /// Removes a plugin's subscriptions (failed load, development reload).
    pub(crate) fn remove_plugin(&self, _inner: &Inner, plugin: u32) {
        let mut lists = self.lists.write().unwrap_or_else(PoisonError::into_inner);
        let mut mask = 0u64;
        for (kind, list) in lists.iter_mut().enumerate() {
            list.retain(|s| s.plugin != plugin);
            if !list.is_empty() {
                mask |= 1 << kind;
            }
        }
        self.mask.store(mask, Ordering::Relaxed);
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
            let mut ctx = CallCtx::new(crate::api::reborrow(&mut world));
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
