//! Query: Vanilla's GameSpy4 UDP listener (`QueryThreadGs4`) on the network runtime.
//!
//! A client asks for a challenge token (type 9) with a 4-byte session id, then for the basic
//! (type 0, 11 bytes) or full stat (type 0 with 4 padding bytes, 15 bytes) with that token.
//! The full stat is cached for 5 seconds like in Vanilla. Tokens live up to 30 seconds.
//!
//! SEC-24 on top of Vanilla: packets per address and second are limited and the number of
//! open challenges is bounded, so the listener cannot be used to flood others.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;

/// `GAME_TYPE`.
const GAME_TYPE: &str = "SMP";
/// `GAME_ID`.
const GAME_ID: &str = "MINECRAFT";
/// Lifetime of a challenge (`CHALLENGE_CHECK_INTERVAL`).
const CHALLENGE_TIME: Duration = Duration::from_secs(30);
/// Cache time of the full stat (`RESPONSE_CACHE_TIME`).
const RULES_CACHE_TIME: Duration = Duration::from_secs(5);
/// Open challenges and rate-limited addresses at most.
const MAX_TRACKED: usize = 16_384;

/// Settings and fixed values of the Query listener.
#[derive(Debug, Clone)]
pub struct QueryConfig {
    /// Address to listen on.
    pub bind: SocketAddr,
    /// `hostname`: the `motd`.
    pub motd: String,
    /// `map`: `level-name`.
    pub level_name: String,
    /// `maxplayers`.
    pub max_players: i32,
    /// `hostport`: the game port.
    pub server_port: u16,
    /// `hostip`: `server-ip`, or the address of this host.
    pub host_ip: String,
    /// Packets answered per address and second.
    pub max_packets_per_second: u32,
}

/// The names of the online players, updated by the server.
#[derive(Debug, Default)]
pub struct QueryPlayers(RwLock<Arc<[String]>>);

impl QueryPlayers {
    /// Replaces the names.
    pub fn set(&self, names: Vec<String>) {
        *self.0.write().unwrap_or_else(PoisonError::into_inner) = names.into();
    }

    fn get(&self) -> Arc<[String]> {
        Arc::clone(&self.0.read().unwrap_or_else(PoisonError::into_inner))
    }
}

#[derive(Debug)]
struct Challenge {
    created: Instant,
    token: i32,
    session: [u8; 4],
}

/// The listener's state; one task owns it.
#[derive(Debug)]
pub struct QueryState {
    config: QueryConfig,
    players: Arc<QueryPlayers>,
    challenges: HashMap<SocketAddr, Challenge>,
    rates: HashMap<IpAddr, (Instant, u32)>,
    rules: Option<(Instant, Vec<u8>)>,
}

impl QueryState {
    /// A state with no challenges.
    pub fn new(config: QueryConfig, players: Arc<QueryPlayers>) -> Self {
        Self {
            config,
            players,
            challenges: HashMap::new(),
            rates: HashMap::new(),
            rules: None,
        }
    }

    /// Whether another packet of `ip` may be answered now.
    fn admit(&mut self, ip: IpAddr, now: Instant) -> bool {
        if self.rates.len() >= MAX_TRACKED {
            self.rates.retain(|_, (since, _)| {
                now.saturating_duration_since(*since) < Duration::from_secs(1)
            });
            if self.rates.len() >= MAX_TRACKED {
                return false;
            }
        }
        let entry = self.rates.entry(ip).or_insert((now, 0));
        if now.saturating_duration_since(entry.0) >= Duration::from_secs(1) {
            *entry = (now, 0);
        }
        entry.1 = entry.1.saturating_add(1);
        entry.1 <= self.config.max_packets_per_second
    }

    /// Answers one packet (`processPacket`); `None` when nothing is sent back.
    pub fn handle(&mut self, packet: &[u8], from: SocketAddr, now: Instant) -> Option<Vec<u8>> {
        let [0xFE, 0xFD, kind, ..] = *packet else {
            return None;
        };
        if !self.admit(from.ip(), now) {
            return None;
        }
        self.challenges
            .retain(|_, c| now.saturating_duration_since(c.created) < CHALLENGE_TIME);
        match kind {
            9 => {
                let session = *packet.get(3..7)?.first_chunk::<4>()?;
                if self.challenges.len() >= MAX_TRACKED {
                    return None;
                }
                let token = i32::from_le_bytes(crate::conn::rand_bytes()).rem_euclid(16_777_216);
                self.challenges.insert(
                    from,
                    Challenge {
                        created: now,
                        token,
                        session,
                    },
                );
                // `"\t" + ident + challenge + "\0"`.
                let mut out = vec![9];
                out.extend_from_slice(String::from_utf8_lossy(&session).as_bytes());
                out.extend_from_slice(token.to_string().as_bytes());
                out.push(0);
                Some(out)
            }
            0 => {
                let challenge = self.challenges.get(&from)?;
                let token = packet
                    .get(7..11)
                    .and_then(|b| b.first_chunk::<4>())
                    .map_or(0, |b| i32::from_be_bytes(*b));
                if token != challenge.token {
                    return None;
                }
                let session = challenge.session;
                Some(if packet.len() == 15 {
                    self.full_stat(session, now)
                } else {
                    self.basic_stat(session)
                })
            }
            _ => None,
        }
    }

    fn basic_stat(&self, session: [u8; 4]) -> Vec<u8> {
        let c = &self.config;
        let mut out = vec![0];
        out.extend_from_slice(&session);
        for s in [
            c.motd.as_str(),
            GAME_TYPE,
            &c.level_name,
            &self.players.get().len().to_string(),
            &c.max_players.to_string(),
        ] {
            put(&mut out, s);
        }
        out.extend_from_slice(&c.server_port.to_le_bytes());
        put(&mut out, &c.host_ip);
        out
    }

    fn full_stat(&mut self, session: [u8; 4], now: Instant) -> Vec<u8> {
        if let Some((built, data)) = &mut self.rules
            && now.saturating_duration_since(*built) < RULES_CACHE_TIME
        {
            if let Some(ident) = data.get_mut(1..5) {
                ident.copy_from_slice(&session);
            }
            return data.clone();
        }
        let c = &self.config;
        let players = self.players.get();
        let mut out = vec![0];
        out.extend_from_slice(&session);
        put(&mut out, "splitnum");
        out.extend_from_slice(&[128, 0]);
        let rules = [
            ("hostname", c.motd.clone()),
            ("gametype", GAME_TYPE.to_owned()),
            ("game_id", GAME_ID.to_owned()),
            ("version", vanillite_protocol::VERSION_ID.to_owned()),
            ("plugins", String::new()),
            ("map", c.level_name.clone()),
            ("numplayers", players.len().to_string()),
            ("maxplayers", c.max_players.to_string()),
            ("hostport", c.server_port.to_string()),
            ("hostip", c.host_ip.clone()),
        ];
        for (key, value) in &rules {
            put(&mut out, key);
            put(&mut out, value);
        }
        out.extend_from_slice(&[0, 1]);
        put(&mut out, "player_");
        out.push(0);
        for name in players.iter() {
            put(&mut out, name);
        }
        out.push(0);
        self.rules = Some((now, out.clone()));
        out
    }
}

/// `NetworkDataOutputStream.writeString`: UTF-8 and a NUL.
fn put(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(s.as_bytes());
    out.push(0);
}

/// Binds the socket and starts answering on the current tokio runtime.
pub(crate) async fn bind(
    config: QueryConfig,
    players: Arc<QueryPlayers>,
) -> std::io::Result<SocketAddr> {
    let socket = UdpSocket::bind(config.bind).await?;
    let local = socket.local_addr()?;
    log::info!("Query running on {local}");
    let mut state = QueryState::new(config, players);
    tokio::spawn(async move {
        let mut buf = [0u8; 1460];
        loop {
            let (len, from) = match socket.recv_from(&mut buf).await {
                Ok(v) => v,
                Err(e) => {
                    log::debug!("Query receive failed: {e}");
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    continue;
                }
            };
            let packet = buf.get(..len).unwrap_or_default();
            if let Some(answer) = state.handle(packet, from, Instant::now()) {
                let _ = socket.send_to(&answer, from).await;
            }
        }
    });
    Ok(local)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> QueryState {
        let players = Arc::new(QueryPlayers::default());
        players.set(vec!["Alice".into(), "Bob".into()]);
        QueryState::new(
            QueryConfig {
                bind: SocketAddr::from(([127, 0, 0, 1], 0)),
                motd: "A Minecraft Server".into(),
                level_name: "world".into(),
                max_players: 20,
                server_port: 25565,
                host_ip: "127.0.0.1".into(),
                max_packets_per_second: 20,
            },
            players,
        )
    }

    fn token(answer: &[u8]) -> i32 {
        let digits = answer
            .get(5..answer.len().saturating_sub(1))
            .unwrap_or_default();
        std::str::from_utf8(digits)
            .unwrap_or("")
            .parse()
            .unwrap_or(-1)
    }

    #[test]
    fn basic_and_full_stat() {
        let mut s = state();
        let from = SocketAddr::from(([10, 0, 0, 1], 5000));
        let now = Instant::now();
        let hello = s
            .handle(&[0xFE, 0xFD, 9, 1, 2, 3, 4], from, now)
            .unwrap_or_default();
        assert_eq!(hello.get(..5), Some(&[9, 1, 2, 3, 4][..]));
        let t = token(&hello).to_be_bytes();
        let mut basic = vec![0xFE, 0xFD, 0, 1, 2, 3, 4];
        basic.extend_from_slice(&t);
        let answer = s.handle(&basic, from, now).unwrap_or_default();
        let mut expected = vec![0, 1, 2, 3, 4];
        expected.extend_from_slice(b"A Minecraft Server\0SMP\0world\x002\x0020\0");
        expected.extend_from_slice(&25565u16.to_le_bytes());
        expected.extend_from_slice(b"127.0.0.1\0");
        assert_eq!(answer, expected);

        let mut full = basic.clone();
        full.extend_from_slice(&[0, 0, 0, 0]);
        let answer = s.handle(&full, from, now).unwrap_or_default();
        let text = String::from_utf8_lossy(&answer);
        assert!(text.contains("splitnum\0\u{fffd}\0hostname\0A Minecraft Server\0"));
        assert!(text.ends_with("\0\u{1}player_\0\0Alice\0Bob\0\0"));

        // A wrong token or an unknown address gets nothing.
        let mut wrong = basic.clone();
        if let Some(b) = wrong.last_mut() {
            *b ^= 1;
        }
        assert_eq!(s.handle(&wrong, from, now), None);
        let other = SocketAddr::from(([10, 0, 0, 2], 5000));
        assert_eq!(s.handle(&basic, other, now), None);
        // Tokens expire.
        assert_eq!(s.handle(&basic, from, now + CHALLENGE_TIME), None);
    }

    #[test]
    fn rate_limits_per_address() {
        let mut s = state();
        let from = SocketAddr::from(([10, 0, 0, 1], 5000));
        let now = Instant::now();
        let answered = (0..30)
            .filter(|_| s.handle(&[0xFE, 0xFD, 9, 1, 2, 3, 4], from, now).is_some())
            .count();
        assert_eq!(answered, 20);
        assert!(
            s.handle(
                &[0xFE, 0xFD, 9, 1, 2, 3, 4],
                from,
                now + Duration::from_secs(1)
            )
            .is_some()
        );
    }
}
