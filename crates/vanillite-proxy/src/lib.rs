//! `vanillite-proxy`: a Velocity-compatible proxy (architecture 14.4, phase 1).
//!
//! The proxy terminates the client's encryption and compression, authenticates players with
//! the session server, and logs them in to backends with Velocity "modern" forwarding. In play
//! it relays packet bodies with no work beyond framing, and switches servers through Vanilla's
//! reconfiguration (`start_configuration` → configuration → play), so the client never
//! reconnects. A lost backend sends the player to the next server of the try order.
//!
//! Vanillite backends accept a trusted link (negotiated with the forwarding secret): they
//! compress for the client's threshold, and the proxy passes their frames on unchanged instead
//! of inflating and deflating every packet. Other backends keep the re-encoding relay.
//!
//! With `[bedrock]` enabled, the `vanillite-bedrock` frontend runs in the proxy: each Bedrock
//! player becomes a virtual connection that the proxy logs in with the identity the frontend
//! verified and forwards like a Java player.

mod admin;
mod backend;
mod bungee;
mod client;
pub mod config;
mod link;
mod session;
mod zlib;

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use tokio::sync::Semaphore;
use vanillite_net::{
    Authenticator, ConnectionLimiter, KeyPair, SessionServer, StatusCache, StatusInfo,
};
use vanillite_protocol::text::Component;
use vanillite_protocol::types::Uuid;

pub use config::ProxyConfig;

/// A player on the proxy.
#[derive(Debug, Clone)]
pub(crate) struct Online {
    pub(crate) name: String,
    pub(crate) server: Option<Arc<str>>,
    pub(crate) address: SocketAddr,
    /// Orders to the player's session (commands, plugin messages of other players).
    pub(crate) control: tokio::sync::mpsc::Sender<Control>,
}

/// An order to a player's session from elsewhere on the proxy.
#[derive(Debug)]
pub(crate) enum Control {
    /// Connect to this server.
    Send(Arc<str>),
    /// Show a chat message.
    Chat(Component),
    /// Disconnect with this reason.
    Kick(Component),
    /// Send this BungeeCord plugin message to the player's current server.
    Payload(Vec<u8>),
}

/// Orders queued per session; more are dropped.
pub(crate) const CONTROL_QUEUE: usize = 64;

/// State shared by all connections.
pub(crate) struct Shared {
    /// The configuration; replaced by a reload.
    config: RwLock<Arc<ProxyConfig>>,
    pub(crate) keys: Option<KeyPair>,
    pub(crate) authenticator: Arc<dyn Authenticator>,
    pub(crate) auth_permits: Semaphore,
    pub(crate) limiter: Arc<ConnectionLimiter>,
    /// Last login per IP (connection rate limit, SEC-18).
    throttle: Mutex<HashMap<IpAddr, Instant>>,
    pub(crate) players: Mutex<HashMap<Uuid, Online>>,
    pub(crate) status: StatusCache,
    /// Server list values of the Bedrock frontend.
    bedrock_status: Arc<vanillite_bedrock::Status>,
    pub(crate) session_id: Uuid,
}

impl Shared {
    /// The current configuration.
    pub(crate) fn config(&self) -> Arc<ProxyConfig> {
        match self.config.read() {
            Ok(c) => Arc::clone(&c),
            Err(e) => Arc::clone(&e.into_inner()),
        }
    }

    /// Reads the configuration again from its folder.
    pub(crate) fn reload(&self) -> Result<(), String> {
        let old = self.config();
        let Some(dir) = old.dir.as_deref() else {
            return Err("the configuration was not loaded from a folder".to_owned());
        };
        let new = ProxyConfig::load(dir)?;
        let restart = [
            ("bind", old.bind != new.bind),
            ("online-mode", old.online_mode != new.online_mode),
            ("io-threads", old.io_threads != new.io_threads),
            (
                "limits.max-connections",
                old.limits.max_connections != new.limits.max_connections,
            ),
            (
                "limits.max-pending-per-ip",
                old.limits.max_pending_per_ip != new.limits.max_pending_per_ip,
            ),
            (
                "limits.max-concurrent-auth",
                old.limits.max_concurrent_auth != new.limits.max_concurrent_auth,
            ),
            (
                "bedrock",
                format!("{:?}", old.bedrock) != format!("{:?}", new.bedrock),
            ),
        ];
        for (key, changed) in restart {
            if changed {
                log::warn!("{key} changed: this takes effect after a restart");
            }
        }
        match self.config.write() {
            Ok(mut c) => *c = Arc::new(new),
            Err(e) => *e.into_inner() = Arc::new(new),
        }
        if let Ok(players) = self.players.lock() {
            self.update_status(&players);
        }
        log::info!("Reloaded the configuration");
        Ok(())
    }

    /// Sends an order to the player named `name` (any case); `false` if nobody has that name.
    pub(crate) fn control_by_name(&self, name: &str, order: Control) -> bool {
        let sender = self.players.lock().ok().and_then(|players| {
            players
                .values()
                .find(|p| p.name.eq_ignore_ascii_case(name))
                .map(|p| p.control.clone())
        });
        sender.is_some_and(|s| s.try_send(order).is_ok())
    }

    /// Sends an order to every player for which `pick` is true; the number reached.
    pub(crate) fn control_all(
        &self,
        pick: impl Fn(&Online) -> bool,
        order: impl Fn() -> Control,
    ) -> usize {
        let senders: Vec<_> = match self.players.lock() {
            Ok(players) => players
                .values()
                .filter(|p| pick(p))
                .map(|p| p.control.clone())
                .collect(),
            Err(_) => return 0,
        };
        senders
            .iter()
            .filter(|s| s.try_send(order()).is_ok())
            .count()
    }

    /// Whether a login from `ip` is allowed now; records it.
    pub(crate) fn login_allowed(&self, ip: IpAddr) -> bool {
        let interval = self.config().limits.login_interval;
        if interval.is_zero() {
            return true;
        }
        let Ok(mut map) = self.throttle.lock() else {
            return false;
        };
        let now = Instant::now();
        if map.len() > 10_000 {
            map.retain(|_, t| now.saturating_duration_since(*t) < interval);
        }
        match map.get_mut(&ip) {
            Some(t) if now.saturating_duration_since(*t) < interval => false,
            Some(t) => {
                *t = now;
                true
            }
            None => {
                map.insert(ip, now);
                true
            }
        }
    }

    /// Renders the server list status from the player table.
    pub(crate) fn update_status(&self, players: &HashMap<Uuid, Online>) {
        let config = self.config();
        self.bedrock_status
            .set_online(u32::try_from(players.len()).unwrap_or(u32::MAX));
        self.status.update(&StatusInfo {
            description: Component::text(config.motd.clone()),
            max_players: config.max_players,
            online_players: i32::try_from(players.len()).unwrap_or(i32::MAX),
            sample: players
                .iter()
                .take(12)
                .map(|(id, p)| (p.name.clone(), *id))
                .collect(),
            favicon: config.favicon.clone(),
            enforces_secure_chat: true,
        });
    }

    /// Records the server a player is on.
    pub(crate) fn set_server(&self, id: Uuid, server: Option<Arc<str>>) {
        if let Ok(mut players) = self.players.lock()
            && let Some(p) = players.get_mut(&id)
        {
            p.server = server;
        }
    }
}

/// The running proxy.
pub struct Proxy {
    runtime: tokio::runtime::Runtime,
    shared: Arc<Shared>,
    local_addr: SocketAddr,
    bedrock: Option<vanillite_bedrock::Frontend>,
}

impl std::fmt::Debug for Proxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Proxy")
            .field("local_addr", &self.local_addr)
            .finish_non_exhaustive()
    }
}

impl Proxy {
    /// Binds the listener and starts accepting.
    pub fn start(config: ProxyConfig) -> std::io::Result<Self> {
        let threads = match config.io_threads {
            0 => std::thread::available_parallelism().map_or(2, usize::from),
            n => n,
        };
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(threads)
            .max_blocking_threads(config.limits.max_concurrent_auth.max(1))
            .thread_name("vanillite-proxy")
            .enable_io()
            .enable_time()
            .build()?;
        let keys = if config.online_mode {
            Some(KeyPair::generate().map_err(std::io::Error::other)?)
        } else {
            log::warn!("The proxy runs in offline mode: player identities are not verified");
            None
        };
        if config.secret.is_none() {
            log::warn!("No forwarding secret: backends cannot verify players");
        }
        let shared = Arc::new(Shared {
            keys,
            authenticator: Arc::new(SessionServer::new(std::time::Duration::from_secs(10))),
            auth_permits: Semaphore::new(config.limits.max_concurrent_auth.max(1)),
            limiter: ConnectionLimiter::new(
                config.limits.max_connections,
                config.limits.max_pending_per_ip,
            ),
            throttle: Mutex::new(HashMap::new()),
            players: Mutex::new(HashMap::new()),
            status: StatusCache::new(&StatusInfo::default()),
            bedrock_status: Arc::new(vanillite_bedrock::Status::new(
                &config.motd,
                u32::try_from(config.max_players).unwrap_or(0),
            )),
            session_id: Uuid(u128::from_be_bytes(vanillite_net::rand_bytes())),
            config: RwLock::new(Arc::new(config)),
        });
        shared.update_status(&HashMap::new());
        let config = shared.config();
        let listener = runtime.block_on(tokio::net::TcpListener::bind(config.bind))?;
        let local_addr = listener.local_addr()?;
        let bedrock = if config.bedrock.enabled {
            Some(start_bedrock(&shared, runtime.handle().clone())?)
        } else {
            None
        };
        runtime.spawn(accept_loop(listener, Arc::clone(&shared)));
        log::info!("Proxy listening on {local_addr}");
        Ok(Self {
            runtime,
            shared,
            local_addr,
            bedrock,
        })
    }

    /// Runs a console command (`help` lists them) and returns its output.
    pub fn console(&self, line: &str) -> String {
        admin::console(&self.shared, line)
    }

    /// The bound address.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// The UDP address of the Bedrock frontend, if it runs.
    pub fn bedrock_addr(&self) -> Option<SocketAddr> {
        self.bedrock
            .as_ref()
            .map(vanillite_bedrock::Frontend::local_addr)
    }

    /// Closes all connections and stops.
    pub fn shutdown(self) {
        if let Some(frontend) = self.bedrock {
            frontend.shutdown();
        }
        self.runtime
            .shutdown_timeout(std::time::Duration::from_secs(1));
    }
}

/// Starts the Bedrock frontend: each Bedrock player becomes a virtual connection that the proxy
/// handles like a client after its login, with the identity the frontend verified.
fn start_bedrock(
    shared: &Arc<Shared>,
    runtime: tokio::runtime::Handle,
) -> std::io::Result<vanillite_bedrock::Frontend> {
    let proxy_config = shared.config();
    let b = &proxy_config.bedrock;
    let config = vanillite_bedrock::BedrockConfig {
        bind: b.bind,
        online_mode: b.online_mode,
        min_protocol: b.min_protocol,
        name_prefix: b.name_prefix.clone(),
        world_name: proxy_config.motd.clone(),
        max_connections: u32::try_from(proxy_config.limits.max_connections).unwrap_or(u32::MAX),
        max_connections_per_ip: b.max_connections_per_ip,
        max_packets_per_second: b.max_packets_per_second,
        max_view_distance: b.max_view_distance,
        chunk_cache_sections: b.chunk_cache_sections,
        ..vanillite_bedrock::BedrockConfig::default()
    };
    let owner = Arc::clone(shared);
    let connector: vanillite_bedrock::Connector = Arc::new(move |profile, address| {
        let (to_server, from_client) = tokio::sync::mpsc::channel(256);
        let (to_client, from_server) = tokio::sync::mpsc::channel(1024);
        // The proxy's connection limits apply (SEC-18); a refused player sees its connection
        // close at once.
        if let Some(guard) = owner.limiter.admit(address.ip()) {
            runtime.spawn(client::handle_virtual(
                Arc::clone(&owner),
                from_client,
                to_client,
                address,
                guard,
                profile,
            ));
        }
        vanillite_net::VirtualConnection {
            to_server,
            from_server,
        }
    });
    vanillite_bedrock::Frontend::start(
        config,
        connector,
        Arc::clone(&shared.bedrock_status),
        b.threads,
    )
}

async fn accept_loop(listener: tokio::net::TcpListener, shared: Arc<Shared>) {
    loop {
        let (socket, addr) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                log::warn!("accept failed: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                continue;
            }
        };
        let Some(guard) = shared.limiter.admit(addr.ip()) else {
            // Over the total or per-IP limit (SEC-18): close without reading anything.
            continue;
        };
        tokio::spawn(client::handle(Arc::clone(&shared), socket, addr, guard));
    }
}
