//! Bedrock Edition frontend (architecture 14.2).
//!
//! Accepts Bedrock clients over RakNet (UDP), verifies their Xbox Live login, encrypts the
//! connection and translates between Bedrock packets and Java packets of the current version.
//! Each Bedrock player becomes a virtual connection (14.1) to the server, so the core stays
//! Java-only and every translated packet passes the same validation as a Java client's. The
//! frontend runs on its own small runtime and can run inside the server or a proxy.
//!
//! Translated: join, chunks (blocks, water layer, biomes), movement (Bedrock's client
//! authoritative input -> Java movement, teleports), other players (with the skins of Bedrock
//! players, a plain skin for Java players) and mobs, block changes, chat, commands (names, free
//! arguments), game mode and abilities. Not translated yet: inventory and items, block breaking
//! and placing, interaction and combat, health, hunger and death, dimension changes and respawn,
//! entity metadata (sneaking, names of mobs), sounds, particles, time and weather, Java skins.

mod batch;
mod chunk;
pub mod client;
mod data;
mod io;
mod login;
mod num;
mod packets;
mod raknet;
mod session;
mod skin;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use rustc_hash::FxHashMap;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use vanillite_net::VirtualConnection;
use vanillite_protocol::types::GameProfile;

pub use data::{PROTOCOL, VERSION};
pub use raknet::Limits as RakNetLimits;

/// Opens the Java connection of a Bedrock player (a virtual connection in the server).
pub type Connector = Arc<dyn Fn(GameProfile, SocketAddr) -> VirtualConnection + Send + Sync>;

/// Settings of the frontend.
#[derive(Debug, Clone)]
pub struct BedrockConfig {
    /// UDP address to listen on.
    pub bind: SocketAddr,
    /// Require an Xbox Live identity. Off accepts any claimed identity: tests only.
    pub online_mode: bool,
    /// Prefix of Bedrock players' Java names.
    pub name_prefix: String,
    /// World name shown in the client.
    pub world_name: String,
    /// Connections at the same time.
    pub max_connections: u32,
    /// Connections per IP address.
    pub max_connections_per_ip: u32,
    /// Time to complete the login.
    pub login_timeout: Duration,
    /// Time without any datagram after which a peer is dropped.
    pub idle_timeout: Duration,
    /// Largest game batch after decompression (also the largest RakNet message).
    pub max_batch_size: usize,
    /// Packets in one batch.
    pub max_packets_per_batch: usize,
    /// Packets per second from one client.
    pub max_packets_per_second: u32,
    /// Batches below this many bytes are sent uncompressed.
    pub compression_threshold: u16,
    /// DEFLATE level (1-12).
    pub compression_level: i32,
    /// Largest chunk radius granted.
    pub max_view_distance: u8,
    /// Translated sections kept for all players.
    pub chunk_cache_sections: usize,
    /// Unacknowledged bytes after which chunk batches wait.
    pub chunk_backlog: usize,
    /// RakNet limits.
    pub raknet: raknet::Limits,
}

impl Default for BedrockConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([0, 0, 0, 0], 19132)),
            online_mode: true,
            name_prefix: ".".into(),
            world_name: "Vanillite".into(),
            max_connections: 1000,
            max_connections_per_ip: 8,
            login_timeout: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(15),
            max_batch_size: 4 << 20,
            max_packets_per_batch: 1024,
            max_packets_per_second: 1000,
            compression_threshold: 256,
            compression_level: 6,
            max_view_distance: 12,
            chunk_cache_sections: 16_384,
            chunk_backlog: 512 << 10,
            raknet: raknet::Limits::default(),
        }
    }
}

/// Server list values, updated by the server.
#[derive(Debug)]
pub struct Status {
    motd: RwLock<String>,
    online: AtomicU32,
    max: AtomicU32,
}

impl Status {
    /// Initial values.
    pub fn new(motd: &str, max: u32) -> Self {
        Self {
            motd: RwLock::new(motd.into()),
            online: AtomicU32::new(0),
            max: AtomicU32::new(max),
        }
    }

    /// Sets the online player count.
    pub fn set_online(&self, n: u32) {
        self.online.store(n, Ordering::Relaxed);
    }

    /// Sets the message of the day (first line).
    pub fn set_motd(&self, motd: &str) {
        if let Ok(mut g) = self.motd.write() {
            *g = motd.into();
        }
    }

    fn pong_text(&self, guid: i64, port: u16) -> String {
        let clean = |s: &str| -> String {
            s.chars()
                .filter(|c| *c != ';' && !c.is_control())
                .take(64)
                .collect()
        };
        let motd = self.motd.read().map(|m| clean(&m)).unwrap_or_default();
        format!(
            "MCPE;{motd};{};{};{};{};{guid};Vanillite;Survival;1;{port};{port};0;",
            data::PROTOCOL,
            data::VERSION,
            self.online.load(Ordering::Relaxed),
            self.max.load(Ordering::Relaxed),
        )
    }
}

/// The running frontend.
pub struct Frontend {
    runtime: tokio::runtime::Runtime,
    local_addr: SocketAddr,
}

impl std::fmt::Debug for Frontend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Frontend")
            .field("local_addr", &self.local_addr)
            .finish_non_exhaustive()
    }
}

impl Frontend {
    /// Binds the UDP socket and starts accepting Bedrock players.
    pub fn start(
        config: BedrockConfig,
        connector: Connector,
        status: Arc<Status>,
        threads: usize,
    ) -> std::io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(threads.max(1))
            .thread_name("vanillite-bedrock")
            .enable_io()
            .enable_time()
            .build()?;
        let socket = runtime.block_on(UdpSocket::bind(config.bind))?;
        let local_addr = socket.local_addr()?;
        let keys = Arc::new(login::TokenKeys::default());
        if config.online_mode {
            login::spawn_key_refresh(Arc::clone(&keys), runtime.handle());
        } else {
            log::warn!("Bedrock online mode is off: Bedrock identities are not verified");
        }
        let entity_types = entity_types();
        let shared = Arc::new(session::Shared {
            cache: chunk::ChunkCache::new(config.chunk_cache_sections),
            config,
            connector,
            keys,
            skins: Mutex::new(FxHashMap::default()),
            plain: Arc::new(skin::Skin::plain()),
            status,
            guid: i64::from_ne_bytes(vanillite_net::rand_bytes()),
            entity_types,
        });
        runtime.spawn(listen(Arc::new(socket), shared));
        log::info!(
            "Bedrock {} (protocol {}) listening on UDP {local_addr}",
            data::VERSION,
            data::PROTOCOL
        );
        Ok(Self {
            runtime,
            local_addr,
        })
    }

    /// The bound address.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Stops the frontend and drops all Bedrock connections.
    pub fn shutdown(self) {
        self.runtime.shutdown_timeout(Duration::from_secs(1));
    }
}

/// Entity identifiers the client knows (from the `available_actor_identifiers` data).
fn entity_types() -> rustc_hash::FxHashSet<String> {
    let bytes = data::ENTITY_IDENTIFIERS;
    let mut set = rustc_hash::FxHashSet::default();
    let needle = b"minecraft:";
    let mut i = 0usize;
    while let Some(pos) = bytes
        .get(i..)
        .and_then(|rest| rest.windows(needle.len()).position(|w| w == needle))
    {
        let start = i.saturating_add(pos);
        let end = bytes
            .get(start..)
            .unwrap_or_default()
            .iter()
            .position(|b| !(b.is_ascii_lowercase() || b.is_ascii_digit() || b"_:.".contains(b)))
            .map_or(bytes.len(), |n| start.saturating_add(n));
        if let Some(s) = bytes
            .get(start..end)
            .and_then(|s| std::str::from_utf8(s).ok())
        {
            set.insert(s.to_owned());
        }
        i = end.max(start.saturating_add(1));
    }
    set
}

/// A connected peer as the listener sees it.
struct Peer {
    tx: mpsc::Sender<Vec<u8>>,
}

/// The UDP loop: unconnected messages here, connected datagrams to their session task.
async fn listen(socket: Arc<UdpSocket>, shared: Arc<session::Shared>) {
    let mut buf = vec![0u8; 2048];
    let mut peers: FxHashMap<SocketAddr, Peer> = FxHashMap::default();
    let mut per_ip = raknet::PerIp::default();
    let mut cookies = raknet::Cookies::new(Instant::now());
    let (closed_tx, mut closed_rx) = mpsc::unbounded_channel::<SocketAddr>();
    let guid = shared.guid;
    let port = socket.local_addr().map_or(0, |a| a.port());
    let mut pongs = (Instant::now(), 0u32);
    loop {
        let (n, from) = tokio::select! {
            r = socket.recv_from(&mut buf) => match r {
                Ok(v) => v,
                Err(e) => {
                    // ICMP errors of earlier sends surface here on some systems.
                    log::trace!("Bedrock UDP receive error: {e}");
                    continue;
                }
            },
            Some(addr) = closed_rx.recv() => {
                if peers.remove(&addr).is_some() {
                    per_ip.release(addr.ip());
                }
                continue;
            }
        };
        let data = buf.get(..n).unwrap_or_default();
        let Some(&kind) = data.first() else {
            continue;
        };
        if let Some(peer) = peers.get(&from)
            && kind & 0x80 != 0
        {
            // A full queue drops the datagram; RakNet recovers.
            let _ = peer.tx.try_send(data.to_vec());
            continue;
        }
        let now = Instant::now();
        cookies.rotate(now);
        match kind {
            raknet::id::UNCONNECTED_PING | raknet::id::UNCONNECTED_PING_OPEN => {
                if now.saturating_duration_since(pongs.0) >= Duration::from_secs(1) {
                    pongs = (now, 0);
                }
                pongs.1 = pongs.1.saturating_add(1);
                if pongs.1 > 2000 {
                    continue;
                }
                if let Some(pong) = raknet::pong(data, guid, &shared.status.pong_text(guid, port)) {
                    let _ = socket.try_send_to(&pong, from);
                }
            }
            raknet::id::OPEN_CONNECTION_REQUEST_1 => {
                let Some((protocol, mtu)) = raknet::parse_request_1(data) else {
                    continue;
                };
                if protocol != raknet::PROTOCOL {
                    let _ = socket.try_send_to(&raknet::incompatible(guid), from);
                    continue;
                }
                let mtu = mtu.clamp(raknet::MIN_MTU, raknet::MAX_MTU);
                let _ = socket.try_send_to(&raknet::reply_1(guid, cookies.issue(from), mtu), from);
            }
            raknet::id::OPEN_CONNECTION_REQUEST_2 => {
                let Some((cookie, mtu, _client)) = raknet::parse_request_2(data) else {
                    continue;
                };
                if !cookies.check(from, cookie) {
                    continue;
                }
                let mtu = mtu.clamp(raknet::MIN_MTU, raknet::MAX_MTU);
                if !peers.contains_key(&from) {
                    let full = u32::try_from(peers.len()).unwrap_or(u32::MAX)
                        >= shared.config.max_connections;
                    if full || !per_ip.admit(from.ip(), shared.config.max_connections_per_ip) {
                        continue;
                    }
                    let (tx, rx) = mpsc::channel(512);
                    peers.insert(from, Peer { tx });
                    let shared = Arc::clone(&shared);
                    let socket = Arc::clone(&socket);
                    let closed = closed_tx.clone();
                    tokio::spawn(async move {
                        session::run(shared, socket, from, mtu, rx).await;
                        let _ = closed.send(from);
                    });
                }
                let _ = socket.try_send_to(&raknet::reply_2(guid, from, mtu), from);
            }
            _ => {}
        }
    }
}
