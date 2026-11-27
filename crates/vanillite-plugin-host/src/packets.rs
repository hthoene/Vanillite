//! Packet listeners: the plugins' side of the packet tap ([`vanillite_protocol::tap`]).
//!
//! The host keeps the listeners per protocol state and direction, sorted by priority, and
//! publishes the packet types that have one as the tap's bit mask. The network layer tests
//! that mask for every packet; only a packet with a listener comes here. Listeners run on
//! network threads (broadcast listeners also on the thread that encodes a packet for many
//! receivers) with a context that owns no world, so nothing here assumes a main thread or
//! one owner per world.
//!
//! What a listener can break is limited to the connection the packet belongs to: a replaced
//! serverbound packet is decoded and validated like any packet of the client, a cancelled one
//! still counts against the rate limits, and a clientbound body the client cannot read
//! disconnects that client only.

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock, Weak};

use vanillite_abi::{self as abi, Status, status};
use vanillite_protocol::tap::{Interceptor, Mask, PacketTap, Peer, Stage, Verdict, packet_id};
use vanillite_protocol::{Direction, State};

use crate::api::CallCtx;
use crate::{Inner, PluginHost, SendPtr, crash};

/// Listeners all plugins of a host may have at once.
const MAX_LISTENERS: usize = 4096;

const STATES: [State; 5] = [
    State::Handshake,
    State::Status,
    State::Login,
    State::Configuration,
    State::Play,
];

/// One packet listener of a plugin.
struct Listener {
    handle: u64,
    plugin: u32,
    tag: &'static crash::Tag,
    /// The packet id, or [`abi::packet_id::ANY`].
    id: i32,
    priority: i32,
    flags: u32,
    run: abi::PacketFn,
    drop: Option<abi::DropFn>,
    user: SendPtr,
}

impl Listener {
    fn broadcast(&self) -> bool {
        self.flags & abi::packet_listen::BROADCAST != 0
    }
}

impl Drop for Listener {
    /// Runs once the listener is removed and the last dispatch that saw it has finished.
    fn drop(&mut self) {
        if let Some(drop) = self.drop {
            let _entered = crash::enter(self.tag);
            // SAFETY: the plugin's drop function for its own user data, called once; no call
            // of the listener is running any more.
            unsafe { drop(self.user.0) };
        }
    }
}

type List = Arc<[Arc<Listener>]>;

/// The packet listeners of a host.
pub(crate) struct Packets {
    /// The listeners per state and direction ([`table`]), sorted by priority, then plugin
    /// and age. A dispatch takes its list out of the lock, so listeners may subscribe and
    /// unsubscribe.
    tables: RwLock<[List; 10]>,
    next: AtomicU64,
    pub(crate) tap: Arc<PacketTap>,
}

impl Default for Packets {
    fn default() -> Self {
        Self {
            tables: RwLock::new(std::array::from_fn(|_| Arc::from([]))),
            next: AtomicU64::new(0),
            tap: Arc::default(),
        }
    }
}

impl std::fmt::Debug for Packets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Packets").finish_non_exhaustive()
    }
}

fn state_index(state: State) -> usize {
    match state {
        State::Handshake => 0,
        State::Status => 1,
        State::Login => 2,
        State::Configuration => 3,
        State::Play => 4,
    }
}

fn table(direction: Direction, state: State) -> usize {
    match direction {
        Direction::Clientbound => state_index(state),
        Direction::Serverbound => state_index(state) + STATES.len(),
    }
}

fn direction_of(direction: u32) -> Option<Direction> {
    match direction {
        abi::packet_direction::CLIENTBOUND => Some(Direction::Clientbound),
        abi::packet_direction::SERVERBOUND => Some(Direction::Serverbound),
        _ => None,
    }
}

fn state_of(state: u32) -> Option<State> {
    STATES.get(usize::try_from(state).ok()?).copied()
}

/// The packet names of a state and direction, by id.
fn names(direction: Direction, state: State) -> &'static [&'static str] {
    use vanillite_data::packets as p;
    match (state, direction) {
        (State::Handshake, Direction::Serverbound) => &p::handshake::serverbound::NAMES,
        (State::Handshake, Direction::Clientbound) => &[],
        (State::Status, Direction::Clientbound) => &p::status::clientbound::NAMES,
        (State::Status, Direction::Serverbound) => &p::status::serverbound::NAMES,
        (State::Login, Direction::Clientbound) => &p::login::clientbound::NAMES,
        (State::Login, Direction::Serverbound) => &p::login::serverbound::NAMES,
        (State::Configuration, Direction::Clientbound) => &p::configuration::clientbound::NAMES,
        (State::Configuration, Direction::Serverbound) => &p::configuration::serverbound::NAMES,
        (State::Play, Direction::Clientbound) => &p::play::clientbound::NAMES,
        (State::Play, Direction::Serverbound) => &p::play::serverbound::NAMES,
    }
}

/// The id of a packet name (`set_entity_data` or `minecraft:set_entity_data`).
pub(crate) fn id_of(direction: u32, state: u32, name: &str) -> Result<i32, Status> {
    let (Some(direction), Some(state)) = (direction_of(direction), state_of(state)) else {
        return Err(status::INVALID_ARGUMENT);
    };
    let name = name.strip_prefix("minecraft:").unwrap_or(name);
    names(direction, state)
        .iter()
        .position(|n| n.strip_prefix("minecraft:").unwrap_or(n) == name)
        .and_then(|i| i32::try_from(i).ok())
        .ok_or(status::NOT_FOUND)
}

/// The name of a packet id, e.g. `minecraft:set_entity_data`.
pub(crate) fn name_of(direction: u32, state: u32, id: i32) -> Result<&'static str, Status> {
    let (Some(direction), Some(state)) = (direction_of(direction), state_of(state)) else {
        return Err(status::INVALID_ARGUMENT);
    };
    usize::try_from(id)
        .ok()
        .and_then(|i| names(direction, state).get(i))
        .copied()
        .ok_or(status::NOT_FOUND)
}

/// Whether `body` starts with the id of a clientbound play packet and is not too long.
pub(crate) fn sendable(body: &[u8]) -> bool {
    body.len() <= abi::packet_limits::MAX_BODY as usize
        && packet_id(body)
            .and_then(|id| usize::try_from(id).ok())
            .is_some_and(|id| id < names(Direction::Clientbound, State::Play).len())
}

impl Packets {
    /// Publishes the packet types with listeners to the tap.
    fn publish(&self, tables: &[List; 10]) {
        let mut mask = Mask::default();
        for direction in [Direction::Clientbound, Direction::Serverbound] {
            for state in STATES {
                let Some(list) = tables.get(table(direction, state)) else {
                    continue;
                };
                for listener in list.iter() {
                    let stage = match direction {
                        Direction::Serverbound => Stage::Serverbound,
                        Direction::Clientbound if listener.broadcast() => Stage::Broadcast,
                        Direction::Clientbound => Stage::Receiver,
                    };
                    if listener.id == abi::packet_id::ANY {
                        mask.set_all(stage, state);
                    } else {
                        mask.set(stage, state, listener.id);
                    }
                }
            }
        }
        self.tap.store(&mask);
    }

    pub(crate) fn add(
        &self,
        plugin: &crate::Plugin,
        spec: &abi::PacketSpec,
    ) -> Result<u64, Status> {
        let (Some(direction), Some(state)) = (direction_of(spec.direction), state_of(spec.state))
        else {
            return Err(status::INVALID_ARGUMENT);
        };
        let known = abi::packet_listen::BROADCAST | abi::packet_listen::IGNORE_CANCELLED;
        if spec.flags & !known != 0 {
            return Err(status::UNSUPPORTED);
        }
        // Serverbound packets come from one client: there is no broadcast stage.
        if spec.flags & abi::packet_listen::BROADCAST != 0 && direction == Direction::Serverbound {
            return Err(status::INVALID_ARGUMENT);
        }
        let count = names(direction, state).len();
        let valid =
            spec.id == abi::packet_id::ANY || usize::try_from(spec.id).is_ok_and(|id| id < count);
        if !valid {
            return Err(status::INVALID_ARGUMENT);
        }
        let mut tables = self.tables.write().unwrap_or_else(PoisonError::into_inner);
        if tables.iter().map(|t| t.len()).sum::<usize>() >= MAX_LISTENERS {
            return Err(status::ERROR);
        }
        let handle = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let listener = Arc::new(Listener {
            handle,
            plugin: plugin.local,
            tag: plugin.tag,
            id: spec.id,
            priority: spec.priority,
            flags: spec.flags,
            run: spec.run,
            drop: spec.drop,
            user: SendPtr(spec.user),
        });
        let Some(slot) = tables.get_mut(table(direction, state)) else {
            return Err(status::ERROR);
        };
        let mut list = slot.to_vec();
        // After every listener with a lower or equal priority, and among equal priorities
        // after earlier plugins and earlier listeners.
        let at = list
            .iter()
            .position(|l| (l.priority, l.plugin) > (spec.priority, plugin.local))
            .unwrap_or(list.len());
        list.insert(at, listener);
        *slot = list.into();
        self.publish(&tables);
        Ok(handle)
    }

    /// Removes one listener of a plugin (`Some(handle)`) or all of them.
    pub(crate) fn remove(&self, plugin: u32, handle: Option<u64>) -> Status {
        let mut removed = Vec::new();
        {
            let mut tables = self.tables.write().unwrap_or_else(PoisonError::into_inner);
            for slot in tables.iter_mut() {
                let gone =
                    |l: &Listener| l.plugin == plugin && handle.is_none_or(|h| h == l.handle);
                if slot.iter().any(|l| gone(l)) {
                    removed.extend(slot.iter().filter(|l| gone(l)).cloned());
                    *slot = slot.iter().filter(|l| !gone(l)).cloned().collect();
                }
            }
            if !removed.is_empty() {
                self.publish(&tables);
            }
        }
        // The user data is freed here, outside the lock, or by the last running dispatch.
        if removed.is_empty() && handle.is_some() {
            status::NOT_FOUND
        } else {
            status::OK
        }
    }

    fn list(&self, direction: Direction, state: State) -> Option<List> {
        self.tables
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(table(direction, state))
            .cloned()
    }
}

thread_local! {
    /// The replacement body a listener wrote during the current call.
    static REPLACEMENT: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
}

/// [`abi::PacketOut::set`].
unsafe extern "C" fn packet_set(_state: *mut c_void, body: abi::Bytes) -> Status {
    if body.ptr.is_null() || body.len == 0 || body.len > abi::packet_limits::MAX_BODY as usize {
        return status::INVALID_ARGUMENT;
    }
    // SAFETY: the plugin passes bytes valid for the call.
    let body = unsafe { std::slice::from_raw_parts(body.ptr, body.len) }.to_vec();
    REPLACEMENT.with(|slot| *slot.borrow_mut() = Some(body));
    status::OK
}

impl Inner {
    /// Runs the listeners of one stage on a packet. `peer` is `None` in the broadcast stage.
    fn packet(
        &self,
        direction: Direction,
        state: State,
        id: i32,
        peer: Option<Peer>,
        body: &[u8],
    ) -> Verdict<Vec<u8>> {
        let Some(list) = self.packets.list(direction, state) else {
            return Verdict::Pass;
        };
        let broadcast = peer.is_none();
        let peer = peer.unwrap_or_default();
        let out = abi::PacketOut {
            size: abi::size_of::<abi::PacketOut>(),
            state: std::ptr::null_mut(),
            set: packet_set,
        };
        let mut cancelled = 0u32;
        let mut replaced: Option<Vec<u8>> = None;
        for listener in list.iter() {
            if (listener.id != abi::packet_id::ANY && listener.id != id)
                || (direction == Direction::Clientbound && listener.broadcast() != broadcast)
                || (cancelled != 0 && listener.flags & abi::packet_listen::IGNORE_CANCELLED != 0)
            {
                continue;
            }
            let Some(plugin) = self.plugin(listener.plugin) else {
                continue;
            };
            let current = replaced.as_deref().unwrap_or(body);
            let mut flags = 0;
            if broadcast {
                flags |= abi::packet_event::BROADCAST;
            }
            if replaced.is_some() {
                flags |= abi::packet_event::REPLACED;
            }
            let mut event = abi::PacketEvent {
                size: abi::size_of::<abi::PacketEvent>(),
                direction: match direction {
                    Direction::Clientbound => abi::packet_direction::CLIENTBOUND,
                    Direction::Serverbound => abi::packet_direction::SERVERBOUND,
                },
                state: u32::try_from(state_index(state)).unwrap_or(0),
                id: packet_id(current).unwrap_or(id),
                cancelled,
                flags,
                protocol: vanillite_protocol::PROTOCOL_VERSION,
                connection: peer.connection,
                player: abi::Uuid::from_u128(peer.player),
                body: abi::Bytes {
                    ptr: current.as_ptr(),
                    len: current.len(),
                },
                out: &out,
            };
            REPLACEMENT.with(|slot| slot.borrow_mut().take());
            let mut ctx = CallCtx::new(None, &plugin);
            plugin.call("a packet listener", || {
                // SAFETY: the plugin's listener and user data live as long as the listener,
                // which this dispatch holds; the event and context are valid for the call.
                unsafe { (listener.run)(listener.user.0, ctx.as_raw(), &mut event) }
            });
            let replacement = REPLACEMENT.with(|slot| slot.borrow_mut().take());
            // Monitors observe the outcome; they cannot change it.
            if listener.priority < abi::priority::MONITOR {
                cancelled = event.cancelled;
                if replacement.is_some() {
                    replaced = replacement;
                }
            }
        }
        match (cancelled != 0, replaced) {
            (true, _) => Verdict::Drop,
            (false, Some(body)) => Verdict::Replace(body),
            (false, None) => Verdict::Pass,
        }
    }
}

/// The tap's way into a host.
pub(crate) struct Bridge(pub(crate) Weak<Inner>);

impl Interceptor for Bridge {
    fn broadcast(&self, state: State, id: i32, body: &[u8]) -> Verdict<Vec<u8>> {
        match self.0.upgrade() {
            Some(inner) => inner.packet(Direction::Clientbound, state, id, None, body),
            None => Verdict::Pass,
        }
    }

    fn clientbound(&self, peer: Peer, state: State, id: i32, body: &[u8]) -> Verdict<Vec<u8>> {
        match self.0.upgrade() {
            Some(inner) => inner.packet(Direction::Clientbound, state, id, Some(peer), body),
            None => Verdict::Pass,
        }
    }

    fn serverbound(&self, peer: Peer, state: State, id: i32, body: &[u8]) -> Verdict<Vec<u8>> {
        match self.0.upgrade() {
            Some(inner) => inner.packet(Direction::Serverbound, state, id, Some(peer), body),
            None => Verdict::Pass,
        }
    }
}

impl PluginHost {
    /// The packet tap the network layer asks about every packet. Without packet listeners
    /// it answers from its bit mask alone.
    pub fn packet_tap(&self) -> &Arc<PacketTap> {
        &self.inner.packets.tap
    }
}
