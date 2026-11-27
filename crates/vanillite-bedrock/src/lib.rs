//! Bedrock Edition frontend (architecture 14.2).
//!
//! Accepts Bedrock clients over RakNet (UDP) or NetherNet (WebRTC on the same UDP port, signaling
//! over TCP), verifies their Xbox Live login, encrypts the connection and translates between
//! Bedrock packets and Java packets of the current version. Each Bedrock player becomes a
//! virtual connection (14.1) to the server, so the core stays Java-only and every translated
//! packet passes the same validation as a Java client's. The frontend runs on its own small
//! runtime and can run inside the server or a proxy.
//!
//! Translated: join, chunks (blocks, water layer, biomes), movement, other players (Bedrock
//! skins, Java skins from the texture server) and mobs with their flags, names, equipment and
//! effects, item entities, falling blocks and TNT, server authoritative block breaking and
//! placing, the inventory (item stack requests as Java clicks, creative slots, the creative
//! palette), attacking, health, hunger, experience, death and respawn, dimension changes and
//! server switches behind a proxy, time and weather, block break particles and sounds, chat,
//! commands (the Java command tree with syntax hints), titles and boss bars, game mode and
//! abilities. Forms are offered to plugins through [`Forms`]. Not translated yet: other
//! containers (chests, crafting tables, furnaces), crafting, interacting with entities, Java
//! particles.

mod batch;
mod blockentity;
mod chunk;
pub mod client;
mod commands;
mod data;
mod entity;
pub mod forms;
mod io;
mod items;
mod login;
mod nbt;
mod nethernet;
mod num;
mod packets;
mod png;
mod raknet;
mod recipes;
mod session;
mod skin;
pub mod version;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use rustc_hash::FxHashMap;
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::mpsc;
use vanillite_net::VirtualConnection;
use vanillite_protocol::types::GameProfile;

pub use forms::Forms;
pub use raknet::Limits as RakNetLimits;
pub use version::Version;

/// Opens the Java connection of a Bedrock player (a virtual connection in the server).
pub type Connector = Arc<dyn Fn(GameProfile, SocketAddr) -> VirtualConnection + Send + Sync>;

/// Settings of the frontend.
#[derive(Debug, Clone)]
pub struct BedrockConfig {
    /// UDP address to listen on.
    pub bind: SocketAddr,
    /// Require an Xbox Live identity. Off accepts any claimed identity: tests only.
    pub online_mode: bool,
    /// Oldest accepted Bedrock network protocol (0: every supported one, see
    /// [`version::SUPPORTED`]).
    pub min_protocol: i32,
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
    /// Key file of the server's NetherNet identity (clients remember the key; `None`: a new
    /// key every start).
    pub identity_file: Option<std::path::PathBuf>,
}

impl Default for BedrockConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([0, 0, 0, 0], 19132)),
            online_mode: true,
            min_protocol: 0,
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
            identity_file: None,
        }
    }
}

impl BedrockConfig {
    /// Whether clients of `protocol` may join (it must also be supported).
    pub fn accepts(&self, protocol: i32) -> bool {
        protocol >= self.min_protocol
    }

    /// The oldest and the newest accepted version.
    pub fn accepted_range(&self) -> (&'static Version, &'static Version) {
        let oldest = version::SUPPORTED
            .iter()
            .copied()
            .find(|v| self.accepts(v.protocol))
            .unwrap_or_else(Version::latest);
        (oldest, Version::latest())
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

    /// Message of the day, online and maximum players.
    fn values(&self) -> (String, u32, u32) {
        (
            self.motd.read().map(|m| m.clone()).unwrap_or_default(),
            self.online.load(Ordering::Relaxed),
            self.max.load(Ordering::Relaxed),
        )
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
            Version::latest().protocol,
            Version::latest().name(),
            self.online.load(Ordering::Relaxed),
            self.max.load(Ordering::Relaxed),
        )
    }
}

/// The running frontend.
pub struct Frontend {
    runtime: tokio::runtime::Runtime,
    local_addr: SocketAddr,
    forms: Forms,
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
        // NetherNet signaling on the same port over TCP (clients since 1.26.60 need it).
        let signaling = match runtime.block_on(TcpListener::bind(local_addr)) {
            Ok(l) => Some(l),
            Err(e) => {
                log::warn!(
                    "Bedrock NetherNet signaling cannot listen on TCP {local_addr}: {e}; Bedrock 1.26.60 and newer cannot join"
                );
                None
            }
        };
        // A RakNet session's address, or `None` when a NetherNet session ended.
        let (closed_tx, closed_rx) = mpsc::unbounded_channel::<Option<SocketAddr>>();
        let hub = Arc::new(nethernet::Hub::new(
            config.identity_file.as_deref(),
            closed_tx.clone(),
        ));
        let keys = Arc::new(login::TokenKeys::default());
        if config.online_mode {
            login::spawn_key_refresh(Arc::clone(&keys), runtime.handle());
        } else {
            log::warn!("Bedrock online mode is off: Bedrock identities are not verified");
        }
        let accepted: Vec<String> = version::SUPPORTED
            .iter()
            .filter(|v| config.accepts(v.protocol))
            .map(|v| format!("{} (protocol {})", v.releases, v.protocol))
            .collect();
        log::info!(
            "Bedrock listening on UDP {local_addr}{} for {}",
            if signaling.is_some() {
                " and TCP (NetherNet)"
            } else {
                ""
            },
            accepted.join(", ")
        );
        let entity_types = entity_types();
        let forms = Arc::new(forms::FormRegistry::default());
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
            java_skins: Mutex::new(FxHashMap::default()),
            forms: Arc::clone(&forms),
        });
        let socket = Arc::new(socket);
        if let Some(l) = signaling {
            runtime.spawn(nethernet::serve(
                l,
                Arc::clone(&shared),
                Arc::clone(&hub),
                Arc::clone(&socket),
            ));
        }
        runtime.spawn(listen(socket, shared, hub, closed_tx, closed_rx));
        Ok(Self {
            runtime,
            local_addr,
            forms: Forms(forms),
        })
    }

    /// Sends forms to the frontend's players.
    pub fn forms(&self) -> Forms {
        self.forms.clone()
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
pub(crate) fn entity_types() -> rustc_hash::FxHashSet<String> {
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

/// A connected peer as the listener sees it: its session's datagram queue.
enum Peer {
    Rak(nethernet::Inbox),
    Net(nethernet::Inbox),
}

/// The UDP loop: unconnected messages here, connected datagrams to their session task.
async fn listen(
    socket: Arc<UdpSocket>,
    shared: Arc<session::Shared>,
    hub: Arc<nethernet::Hub>,
    closed_tx: mpsc::UnboundedSender<Option<SocketAddr>>,
    mut closed_rx: mpsc::UnboundedReceiver<Option<SocketAddr>>,
) {
    let mut buf = vec![0u8; 2048];
    let mut peers: FxHashMap<SocketAddr, Peer> = FxHashMap::default();
    let mut per_ip = raknet::PerIp::default();
    let mut cookies = raknet::Cookies::new(Instant::now());
    let guid = shared.guid;
    let port = socket.local_addr().map_or(0, |a| a.port());
    let mut pongs = (Instant::now(), 0u32);
    // Refused handshakes are logged at most every few seconds (spoofed floods cannot spam).
    let mut refusal_logged: Option<Instant> = None;
    let mut log_refusal = |now: Instant, message: std::fmt::Arguments<'_>| {
        if refusal_logged.is_none_or(|t| now.saturating_duration_since(t) >= Duration::from_secs(5))
        {
            refusal_logged = Some(now);
            log::warn!("{message}");
        }
    };
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
            Some(closed) = closed_rx.recv() => {
                match closed {
                    Some(addr) => {
                        if peers.remove(&addr).is_some() {
                            per_ip.release(addr.ip());
                        }
                    }
                    None => peers.retain(|_, p| match p {
                        Peer::Net(tx) => !tx.is_closed(),
                        Peer::Rak(_) => true,
                    }),
                }
                continue;
            }
        };
        let data = buf.get(..n).unwrap_or_default();
        let Some(&kind) = data.first() else {
            continue;
        };
        match peers.get(&from) {
            // A full queue drops the datagram; RakNet and SCTP recover.
            Some(Peer::Rak(tx)) if kind & 0x80 != 0 => {
                let _ = tx.try_send((from, data.to_vec()));
                continue;
            }
            Some(Peer::Net(tx)) => {
                let _ = tx.try_send((from, data.to_vec()));
                continue;
            }
            _ => {}
        }
        // The first STUN request of a NetherNet client names its session.
        if let Some(tx) = hub.route(data) {
            let _ = tx.try_send((from, data.to_vec()));
            peers.insert(from, Peer::Net(tx));
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
                    log_refusal(
                        now,
                        format_args!(
                            "Bedrock client {from} uses RakNet protocol {protocol}, not {}",
                            raknet::PROTOCOL
                        ),
                    );
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
                    log::debug!("Bedrock handshake from {from} with a stale cookie");
                    continue;
                }
                let mtu = mtu.clamp(raknet::MIN_MTU, raknet::MAX_MTU);
                if !peers.contains_key(&from) {
                    let full = u32::try_from(peers.len()).unwrap_or(u32::MAX)
                        >= shared.config.max_connections;
                    if full || !per_ip.admit(from.ip(), shared.config.max_connections_per_ip) {
                        log_refusal(
                            now,
                            format_args!(
                                "Bedrock connection from {from} refused: {}",
                                if full {
                                    "too many connections"
                                } else {
                                    "too many connections from this address"
                                }
                            ),
                        );
                        continue;
                    }
                    let (tx, rx) = mpsc::channel(512);
                    peers.insert(from, Peer::Rak(tx));
                    let shared = Arc::clone(&shared);
                    let socket = Arc::clone(&socket);
                    let closed = closed_tx.clone();
                    let link = session::Link::Rak(Box::new(raknet::RakConn::new(
                        from,
                        mtu,
                        shared.config.raknet,
                        Instant::now(),
                    )));
                    tokio::spawn(async move {
                        session::run(shared, socket, from, link, rx).await;
                        let _ = closed.send(Some(from));
                    });
                }
                let _ = socket.try_send_to(&raknet::reply_2(guid, from, mtu), from);
            }
            _ => {}
        }
    }
}
