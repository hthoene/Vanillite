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

mod backend;
mod client;
pub mod config;
mod link;
mod session;
mod zlib;

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
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
}

/// State shared by all connections.
pub(crate) struct Shared {
    pub(crate) config: ProxyConfig,
    pub(crate) keys: Option<KeyPair>,
    pub(crate) authenticator: Arc<dyn Authenticator>,
    pub(crate) auth_permits: Semaphore,
    pub(crate) limiter: Arc<ConnectionLimiter>,
    /// Last login per IP (connection rate limit, SEC-18).
    throttle: Mutex<HashMap<IpAddr, Instant>>,
    pub(crate) players: Mutex<HashMap<Uuid, Online>>,
    pub(crate) status: StatusCache,
    pub(crate) session_id: Uuid,
}

impl Shared {
    /// Whether a login from `ip` is allowed now; records it.
    pub(crate) fn login_allowed(&self, ip: IpAddr) -> bool {
        let interval = self.config.limits.login_interval;
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
        self.status.update(&StatusInfo {
            description: Component::text(self.config.motd.clone()),
            max_players: self.config.max_players,
            online_players: i32::try_from(players.len()).unwrap_or(i32::MAX),
            sample: players
                .iter()
                .take(12)
                .map(|(id, p)| (p.name.clone(), *id))
                .collect(),
            favicon: None,
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
    local_addr: SocketAddr,
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
            session_id: Uuid(u128::from_be_bytes(vanillite_net::rand_bytes())),
            config,
        });
        shared.update_status(&HashMap::new());
        let listener = runtime.block_on(tokio::net::TcpListener::bind(shared.config.bind))?;
        let local_addr = listener.local_addr()?;
        runtime.spawn(accept_loop(listener, shared));
        log::info!("Proxy listening on {local_addr}");
        Ok(Self {
            runtime,
            local_addr,
        })
    }

    /// The bound address.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Closes all connections and stops.
    pub fn shutdown(self) {
        self.runtime
            .shutdown_timeout(std::time::Duration::from_secs(1));
    }
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
