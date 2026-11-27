//! Packet listeners: observe, cancel, rewrite and send packets.
//!
//! ```no_run
//! use vanillite_plugin::prelude::*;
//! use vanillite_plugin::packet::PacketFilter;
//!
//! # struct Quiet;
//! # impl Plugin for Quiet {
//! fn load(registrar: &mut Registrar<Self>) -> Result<Self, Error> {
//!     // Signs are read-only: the client's edit never reaches the game.
//!     registrar.on_packet(PacketFilter::serverbound("sign_update"), |event| event.cancel());
//!     Ok(Quiet)
//! }
//! # }
//! ```
//!
//! # Cost
//!
//! A packet type without a listener costs the server nothing (one bit test where packets are
//! written and read). With listeners the server still encodes a packet once for all its
//! receivers:
//!
//! - A listener **per receiver** (the default) is called for every receiver. Reading and
//!   cancelling copy nothing; [`PacketEvent::replace`] gives that one receiver its own copy.
//! - A listener **per broadcast** ([`PacketFilter::per_broadcast`]) must not depend on the
//!   receiver. Where the server encodes a packet once for many receivers it is called once,
//!   without a player; cancelling holds for all of them and a replacement is again one buffer
//!   shared by all. Broadcast listeners run before the listeners per receiver.
//!
//! Listening on every packet ([`PacketFilter::all_clientbound`]) puts a call on every packet
//! of every player: name the packets instead.
//!
//! # Threads
//!
//! Packet listeners run where packets are written and read: on network threads, for many
//! connections at once, and per broadcast also on the thread that encodes the packet. They
//! never own the player or its world; the packets of one connection arrive in order. A
//! listener is therefore `Fn + Send + Sync`, keeps its own state (atomics, locks) and gets no
//! plugin instance and no world access. To act on the world, schedule a task on the player's
//! owner ([`Server::schedule`](crate::Server::schedule)). Plugins that listen on packets
//! should declare `threading = "concurrent"` in `plugin.toml`: with `serialized` every packet
//! call waits for the plugin's other handlers.
//!
//! # Limits
//!
//! Bodies are raw protocol data of the server's Minecraft version
//! ([`PacketEvent::protocol`]); they change between versions, so a plugin that reads fields
//! is tied to one. Packet names change rarely: subscribe by name. Clients of other versions
//! and editions are translated behind the listeners.
//!
//! The server does not check what a listener writes into a clientbound packet: a body the
//! client cannot read disconnects that client. A replaced serverbound packet is validated
//! like any packet of the client, and a cancelled one still counts against the rate limits.
//! Cancelling packets the protocol waits for (keep-alives, teleport confirmations,
//! acknowledgements in login and configuration) ends that connection after its timeout; the
//! server itself is not affected.

use std::ffi::c_void;

use vanillite_abi::{self as abi, Status, status};

use crate::ffi::{guard, read_string};
use crate::server::Server;
use crate::types::{Error, Uuid};

/// The protocol state a packet belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum State {
    /// The first packet of a connection.
    Handshake,
    /// The server list ping.
    Status,
    /// Login.
    Login,
    /// Configuration (registries, resource packs).
    Configuration,
    /// In a world.
    Play,
}

impl State {
    fn to_abi(self) -> u32 {
        match self {
            State::Handshake => abi::packet_state::HANDSHAKE,
            State::Status => abi::packet_state::STATUS,
            State::Login => abi::packet_state::LOGIN,
            State::Configuration => abi::packet_state::CONFIGURATION,
            State::Play => abi::packet_state::PLAY,
        }
    }

    fn from_abi(state: u32) -> State {
        match state {
            abi::packet_state::HANDSHAKE => State::Handshake,
            abi::packet_state::STATUS => State::Status,
            abi::packet_state::LOGIN => State::Login,
            abi::packet_state::CONFIGURATION => State::Configuration,
            _ => State::Play,
        }
    }
}

/// The direction of a packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    /// From the server to a client.
    Clientbound,
    /// From a client to the server.
    Serverbound,
}

impl Direction {
    fn to_abi(self) -> u32 {
        match self {
            Direction::Clientbound => abi::packet_direction::CLIENTBOUND,
            Direction::Serverbound => abi::packet_direction::SERVERBOUND,
        }
    }
}

/// Which packets a listener gets, and when.
#[derive(Clone, Debug)]
pub struct PacketFilter {
    direction: Direction,
    state: State,
    /// `None`: every packet of the state and direction.
    name: Option<String>,
    priority: i32,
    flags: u32,
}

impl PacketFilter {
    fn new(direction: Direction, name: Option<&str>) -> PacketFilter {
        PacketFilter {
            direction,
            state: State::Play,
            name: name.map(str::to_owned),
            priority: abi::priority::NORMAL,
            flags: 0,
        }
    }

    /// A play packet to clients by its Vanilla name, e.g. `set_entity_data`.
    pub fn clientbound(name: &str) -> PacketFilter {
        Self::new(Direction::Clientbound, Some(name))
    }

    /// A play packet from clients by its Vanilla name, e.g. `sign_update`.
    pub fn serverbound(name: &str) -> PacketFilter {
        Self::new(Direction::Serverbound, Some(name))
    }

    /// Every play packet to clients. Costs a call per packet and receiver.
    pub fn all_clientbound() -> PacketFilter {
        Self::new(Direction::Clientbound, None)
    }

    /// Every play packet from clients.
    pub fn all_serverbound() -> PacketFilter {
        Self::new(Direction::Serverbound, None)
    }

    /// The packet type `P` of `vanillite-protocol`.
    #[cfg(feature = "packets")]
    pub fn of<P: vanillite_protocol::Packet>() -> PacketFilter {
        let direction = match P::DIRECTION {
            vanillite_protocol::Direction::Clientbound => Direction::Clientbound,
            vanillite_protocol::Direction::Serverbound => Direction::Serverbound,
        };
        let state = match P::STATE {
            vanillite_protocol::State::Handshake => State::Handshake,
            vanillite_protocol::State::Status => State::Status,
            vanillite_protocol::State::Login => State::Login,
            vanillite_protocol::State::Configuration => State::Configuration,
            vanillite_protocol::State::Play => State::Play,
        };
        Self::new(direction, Some(P::NAME)).state(state)
    }

    /// Another protocol state than play.
    pub fn state(mut self, state: State) -> PacketFilter {
        self.state = state;
        self
    }

    /// The priority ([`priority`](crate::priority)): listeners run from the lowest to the
    /// highest, then the monitors, which only observe.
    pub fn priority(mut self, priority: i32) -> PacketFilter {
        self.priority = priority;
        self
    }

    /// Clientbound only: the listener does not depend on the receiver and runs once per
    /// encoded packet where the server encodes it once for many receivers (see the module
    /// documentation).
    pub fn per_broadcast(mut self) -> PacketFilter {
        self.flags |= abi::packet_listen::BROADCAST;
        self
    }

    /// The listener is skipped while the packet is cancelled.
    pub fn ignore_cancelled(mut self) -> PacketFilter {
        self.flags |= abi::packet_listen::IGNORE_CANCELLED;
        self
    }
}

/// A registered packet listener, for [`Server::remove_packet_listener`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PacketListener(pub u64);

/// A packet on its way, as a listener sees it.
#[derive(Debug)]
pub struct PacketEvent<'a> {
    raw: &'a mut abi::PacketEvent,
    /// The body this listener put in place.
    replaced: Option<Vec<u8>>,
}

impl PacketEvent<'_> {
    /// The direction.
    pub fn direction(&self) -> Direction {
        if self.raw.direction == abi::packet_direction::SERVERBOUND {
            Direction::Serverbound
        } else {
            Direction::Clientbound
        }
    }

    /// The protocol state.
    pub fn state(&self) -> State {
        State::from_abi(self.raw.state)
    }

    /// The packet id of this Minecraft version.
    pub fn id(&self) -> i32 {
        match &self.replaced {
            Some(body) => PacketReader::new(body).varint().unwrap_or(self.raw.id),
            None => self.raw.id,
        }
    }

    /// The packet's Vanilla name, e.g. `minecraft:set_entity_data`.
    pub fn name(&self) -> Option<String> {
        Server::get()?.packet_name(self.state(), self.direction(), self.id())
    }

    /// The player the packet goes to or comes from. `None` before its login finished and in
    /// a broadcast call.
    pub fn player(&self) -> Option<Uuid> {
        let player = Uuid::from_abi(self.raw.player);
        (player.0 != 0).then_some(player)
    }

    /// The connection, a number unique in this server run (0 in a broadcast call).
    pub fn connection(&self) -> u64 {
        self.raw.connection
    }

    /// Whether this call decides for every receiver of a packet encoded once
    /// ([`PacketFilter::per_broadcast`]).
    pub fn is_broadcast(&self) -> bool {
        self.raw.flags & abi::packet_event::BROADCAST != 0
    }

    /// Whether a listener replaced the body.
    pub fn is_replaced(&self) -> bool {
        self.replaced.is_some() || self.raw.flags & abi::packet_event::REPLACED != 0
    }

    /// The protocol version the body is encoded for.
    pub fn protocol(&self) -> i32 {
        self.raw.protocol
    }

    /// The packet body: the id as a VarInt, then the fields.
    pub fn body(&self) -> &[u8] {
        if let Some(body) = &self.replaced {
            return body;
        }
        if self.raw.body.ptr.is_null() || self.raw.body.len == 0 {
            return &[];
        }
        // SAFETY: the host's bytes are valid for the call.
        unsafe { std::slice::from_raw_parts(self.raw.body.ptr, self.raw.body.len) }
    }

    /// A reader over the fields (after the id).
    pub fn reader(&self) -> PacketReader<'_> {
        let mut reader = PacketReader::new(self.body());
        let _ = reader.varint();
        reader
    }

    /// Whether the packet is cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.raw.cancelled != 0
    }

    /// Cancels the packet: a clientbound packet is not sent, a serverbound one not handled.
    pub fn cancel(&mut self) {
        self.raw.cancelled = 1;
    }

    /// Cancels the packet or lets a cancelled one through.
    pub fn set_cancelled(&mut self, cancelled: bool) {
        self.raw.cancelled = u32::from(cancelled);
    }

    /// Replaces the body (`id` + fields). Per receiver this copies the packet for that
    /// receiver only; per broadcast the replacement is shared by all receivers.
    pub fn replace(&mut self, body: Vec<u8>) -> Result<(), Error> {
        // SAFETY: the host's record is valid for the call; it copies the bytes.
        let out = unsafe { self.raw.out.as_ref() }.ok_or(Error::Unsupported)?;
        // SAFETY: as above.
        Error::check(unsafe { (out.set)(out.state, abi::Bytes::new(&body)) })?;
        self.replaced = Some(body);
        Ok(())
    }

    /// The packet as type `P` of `vanillite-protocol`. `None` when it is another packet,
    /// the server speaks another protocol version than this plugin was built for, or the
    /// body does not decode.
    #[cfg(feature = "packets")]
    pub fn decode<P: vanillite_protocol::Packet>(&self) -> Option<P> {
        if self.protocol() != vanillite_protocol::PROTOCOL_VERSION {
            return None;
        }
        let mut reader = vanillite_protocol::Reader::new(self.body());
        if reader.varint().ok()? != P::ID {
            return None;
        }
        P::decode(&mut reader).ok()
    }

    /// Replaces the packet with a typed one ([`PacketEvent::replace`]).
    #[cfg(feature = "packets")]
    pub fn replace_with<P: vanillite_protocol::Packet>(&mut self, packet: &P) -> Result<(), Error> {
        self.replace(vanillite_protocol::frame::encode_body(packet))
    }
}

type Listener = Box<dyn Fn(&mut PacketEvent<'_>) + Send + Sync>;

unsafe extern "C" fn on_packet(
    user: *mut c_void,
    _ctx: *mut abi::Context,
    event: *mut abi::PacketEvent,
) -> Status {
    guard(|| {
        // SAFETY: `user` is the listener boxed in `Server::on_packet`; the host keeps it
        // alive while calls run.
        let listener = unsafe { &*user.cast::<Listener>() };
        // SAFETY: the host passes a record valid for the call, or null.
        let Some(raw) = (unsafe { event.as_mut() }) else {
            return status::INVALID_ARGUMENT;
        };
        if (raw.size as usize) < size_of::<abi::PacketEvent>() {
            return status::UNSUPPORTED;
        }
        listener(&mut PacketEvent {
            raw,
            replaced: None,
        });
        status::OK
    })
}

unsafe extern "C" fn drop_listener(user: *mut c_void) {
    let _ = guard(|| {
        // SAFETY: the box made in `Server::on_packet`, freed once by the host.
        drop(unsafe { Box::from_raw(user.cast::<Listener>()) });
        status::OK
    });
}

impl Server {
    /// Adds a packet listener. Allowed at any time and from any thread; see the
    /// [module documentation](crate::packet) for cost, threads and limits. Since ABI 1.7.
    pub fn on_packet(
        &self,
        filter: PacketFilter,
        listener: impl Fn(&mut PacketEvent<'_>) + Send + Sync + 'static,
    ) -> Result<PacketListener, Error> {
        let id = match &filter.name {
            Some(name) => self
                .packet_id(filter.state, filter.direction, name)
                .ok_or(Error::NotFound)?,
            None => abi::packet_id::ANY,
        };
        let listener: Listener = Box::new(listener);
        let spec = abi::PacketSpec {
            size: abi::size_of::<abi::PacketSpec>(),
            direction: filter.direction.to_abi(),
            state: filter.state.to_abi(),
            id,
            priority: filter.priority,
            flags: filter.flags,
            run: on_packet,
            drop: Some(drop_listener),
            user: Box::into_raw(Box::new(listener)).cast(),
        };
        let mut handle = 0;
        // SAFETY: valid host function, spec and output; the host frees `user` through
        // `drop` when it refuses the listener or removes it.
        Error::check(unsafe {
            (self.host().api.packet_subscribe)(self.host().id, &spec, &mut handle)
        })?;
        Ok(PacketListener(handle))
    }

    /// Adds a listener for the packet type `P` of `vanillite-protocol`, called with the
    /// decoded packet. Packets that do not decode are skipped. Since ABI 1.7.
    #[cfg(feature = "packets")]
    pub fn on_packet_of<P: vanillite_protocol::Packet>(
        &self,
        filter: PacketFilter,
        listener: impl Fn(&mut PacketEvent<'_>, P) + Send + Sync + 'static,
    ) -> Result<PacketListener, Error> {
        self.on_packet(filter, move |event| {
            if let Some(packet) = event.decode::<P>() {
                listener(event, packet);
            }
        })
    }

    /// Removes a packet listener; calls that are running finish first. Since ABI 1.7.
    pub fn remove_packet_listener(&self, listener: PacketListener) -> Result<(), Error> {
        // SAFETY: valid host function.
        Error::check(unsafe { (self.host().api.packet_unsubscribe)(self.host().id, listener.0) })
    }

    /// The id of a packet in this server's Minecraft version by its Vanilla name
    /// (`set_entity_data` or `minecraft:set_entity_data`). Since ABI 1.7.
    pub fn packet_id(&self, state: State, direction: Direction, name: &str) -> Option<i32> {
        let mut id = 0;
        // SAFETY: valid host function, string and output.
        let s = unsafe {
            (self.host().api.packet_id)(
                self.host().id,
                state.to_abi(),
                direction.to_abi(),
                abi::Str::new(name),
                &mut id,
            )
        };
        (s == status::OK).then_some(id)
    }

    /// The Vanilla name of a packet id, e.g. `minecraft:set_entity_data`. Since ABI 1.7.
    pub fn packet_name(&self, state: State, direction: Direction, id: i32) -> Option<String> {
        // SAFETY: valid host function and buffer.
        read_string(|out| unsafe {
            (self.host().api.packet_name)(
                self.host().id,
                state.to_abi(),
                direction.to_abi(),
                id,
                out,
            )
        })
        .ok()
    }

    /// The protocol version packet bodies are encoded for. Since ABI 1.7.
    pub fn protocol_version(&self) -> i32 {
        let mut version = 0;
        // SAFETY: valid host function and output.
        unsafe { (self.host().api.protocol_version)(self.host().id, &mut version) };
        version
    }

    /// Sends a play packet body (`id` + fields) to a player, in order with what the server
    /// sends from this thread. Packet listeners see it like any packet. Since ABI 1.7.
    pub fn send_packet(&self, player: Uuid, body: &[u8]) -> Result<(), Error> {
        self.send_packet_flags(player, body, 0)
    }

    /// [`Server::send_packet`] past every packet listener. Since ABI 1.7.
    pub fn send_packet_unlistened(&self, player: Uuid, body: &[u8]) -> Result<(), Error> {
        self.send_packet_flags(player, body, abi::packet_send::SKIP_LISTENERS)
    }

    fn send_packet_flags(&self, player: Uuid, body: &[u8], flags: u32) -> Result<(), Error> {
        // SAFETY: valid host function and bytes.
        Error::check(unsafe {
            (self.host().api.packet_send)(
                self.host().id,
                player.to_abi(),
                abi::Bytes::new(body),
                flags,
            )
        })
    }

    /// Sends a typed play packet of `vanillite-protocol` to a player
    /// ([`Server::send_packet`]). Since ABI 1.7.
    #[cfg(feature = "packets")]
    pub fn send<P: vanillite_protocol::Packet>(
        &self,
        player: Uuid,
        packet: &P,
    ) -> Result<(), Error> {
        self.send_packet(player, &vanillite_protocol::frame::encode_body(packet))
    }
}

/// Reads the fields of a packet body. Every read fails with `None` at the end of the body.
#[derive(Clone, Debug)]
pub struct PacketReader<'a> {
    buf: &'a [u8],
}

macro_rules! read_be {
    ($($name:ident: $t:ty),* $(,)?) => {
        $(
            #[doc = concat!("Reads a big-endian `", stringify!($t), "`.")]
            pub fn $name(&mut self) -> Option<$t> {
                let (head, tail) = self.buf.split_first_chunk()?;
                self.buf = tail;
                Some(<$t>::from_be_bytes(*head))
            }
        )*
    };
}

impl<'a> PacketReader<'a> {
    /// A reader over `buf`.
    pub fn new(buf: &'a [u8]) -> Self {
        PacketReader { buf }
    }

    /// The bytes left.
    pub fn rest(&self) -> &'a [u8] {
        self.buf
    }

    /// Takes `n` bytes.
    pub fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, tail) = self.buf.split_at_checked(n)?;
        self.buf = tail;
        Some(head)
    }

    read_be!(u8: u8, i8: i8, u16: u16, i16: i16, i32: i32, i64: i64, f32: f32, f64: f64);

    /// Reads a boolean.
    pub fn bool(&mut self) -> Option<bool> {
        self.u8().map(|b| b != 0)
    }

    /// Reads a VarInt.
    pub fn varint(&mut self) -> Option<i32> {
        let mut value: u32 = 0;
        for i in 0..5 {
            let b = self.u8()?;
            value |= u32::from(b & 0x7f) << (7 * i);
            if b & 0x80 == 0 {
                return Some(value.cast_signed());
            }
        }
        None
    }

    /// Reads a VarLong.
    pub fn varlong(&mut self) -> Option<i64> {
        let mut value: u64 = 0;
        for i in 0..10 {
            let b = self.u8()?;
            value |= u64::from(b & 0x7f) << (7 * i);
            if b & 0x80 == 0 {
                return Some(value.cast_signed());
            }
        }
        None
    }

    /// Reads a UUID.
    pub fn uuid(&mut self) -> Option<Uuid> {
        let (head, tail) = self.buf.split_first_chunk()?;
        self.buf = tail;
        Some(Uuid(u128::from_be_bytes(*head)))
    }

    /// Reads a string (VarInt length in bytes, UTF-8).
    pub fn string(&mut self) -> Option<&'a str> {
        let len = usize::try_from(self.varint()?).ok()?;
        std::str::from_utf8(self.bytes(len)?).ok()
    }
}

/// Builds a packet body.
#[derive(Clone, Debug, Default)]
pub struct PacketWriter {
    buf: Vec<u8>,
}

macro_rules! write_be {
    ($($name:ident: $t:ty),* $(,)?) => {
        $(
            #[doc = concat!("Writes a big-endian `", stringify!($t), "`.")]
            pub fn $name(&mut self, value: $t) -> &mut Self {
                self.buf.extend_from_slice(&value.to_be_bytes());
                self
            }
        )*
    };
}

impl PacketWriter {
    /// A body that starts with packet id `id` ([`Server::packet_id`]).
    pub fn new(id: i32) -> Self {
        let mut writer = PacketWriter::default();
        writer.varint(id);
        writer
    }

    write_be!(u8: u8, i8: i8, u16: u16, i16: i16, i32: i32, i64: i64, f32: f32, f64: f64);

    /// Writes a boolean.
    pub fn bool(&mut self, value: bool) -> &mut Self {
        self.u8(u8::from(value))
    }

    /// Writes a VarInt.
    pub fn varint(&mut self, value: i32) -> &mut Self {
        let mut v = value.cast_unsigned();
        loop {
            let low = u8::try_from(v & 0x7f).unwrap_or(0);
            v >>= 7;
            if v == 0 {
                self.buf.push(low);
                return self;
            }
            self.buf.push(low | 0x80);
        }
    }

    /// Writes a VarLong.
    pub fn varlong(&mut self, value: i64) -> &mut Self {
        let mut v = value.cast_unsigned();
        loop {
            let low = u8::try_from(v & 0x7f).unwrap_or(0);
            v >>= 7;
            if v == 0 {
                self.buf.push(low);
                return self;
            }
            self.buf.push(low | 0x80);
        }
    }

    /// Writes a UUID.
    pub fn uuid(&mut self, value: Uuid) -> &mut Self {
        self.buf.extend_from_slice(&value.0.to_be_bytes());
        self
    }

    /// Writes a string (VarInt length in bytes, UTF-8).
    pub fn string(&mut self, value: &str) -> &mut Self {
        self.varint(i32::try_from(value.len()).unwrap_or(i32::MAX));
        self.buf.extend_from_slice(value.as_bytes());
        self
    }

    /// Writes raw bytes.
    pub fn bytes(&mut self, value: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(value);
        self
    }

    /// The body.
    pub fn finish(self) -> Vec<u8> {
        self.buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_and_reader_agree() {
        let mut w = PacketWriter::new(300);
        w.varint(-1)
            .varlong(i64::MIN)
            .string("häuser")
            .bool(true)
            .i64(-7)
            .f32(1.5)
            .uuid(Uuid(42));
        let body = w.finish();
        let mut r = PacketReader::new(&body);
        assert_eq!(r.varint(), Some(300));
        assert_eq!(r.varint(), Some(-1));
        assert_eq!(r.varlong(), Some(i64::MIN));
        assert_eq!(r.string(), Some("häuser"));
        assert_eq!(r.bool(), Some(true));
        assert_eq!(r.i64(), Some(-7));
        assert_eq!(r.f32(), Some(1.5));
        assert_eq!(r.uuid(), Some(Uuid(42)));
        assert_eq!(r.u8(), None);
        assert!(r.rest().is_empty());
    }
}
