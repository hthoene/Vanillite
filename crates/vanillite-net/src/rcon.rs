//! RCON: Vanilla's remote console (`RconThread`, `RconClient`) on the network runtime.
//!
//! Packets are little-endian `length, request id, type` followed by a NUL-terminated string
//! and one padding byte. Type 3 logs in with `rcon.password`, type 2 runs a command as the
//! RCON source; its output comes back in packets of up to 4096 characters. Like Vanilla, a
//! command before the login and a wrong password are answered with request id -1.
//!
//! SEC-24 on top of Vanilla: failed logins per address are limited, the number of open
//! connections is bounded, an unauthenticated connection times out, and the password is
//! compared in constant time and never logged.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;

/// `SERVERDATA_AUTH`.
pub const AUTH: i32 = 3;
/// `SERVERDATA_EXECCOMMAND`.
pub const EXEC_COMMAND: i32 = 2;
/// `SERVERDATA_AUTH_RESPONSE`.
pub const AUTH_RESPONSE: i32 = 2;
/// `SERVERDATA_RESPONSE_VALUE`.
pub const RESPONSE_VALUE: i32 = 0;
/// Largest packet Vanilla reads (`PktUtils.MAX_PACKET_SIZE`), including the length field.
pub const MAX_PACKET: usize = 1460;
/// Characters per response packet (`RconClient.sendCmdResponse`).
const RESPONSE_CHUNK: usize = 4096;
/// Time an unauthenticated connection may stay open.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(30);
/// Window of the failed-login limit.
const FAILURE_WINDOW: Duration = Duration::from_secs(60);
/// Addresses tracked by the failed-login limit at most.
const MAX_TRACKED: usize = 4096;

/// Settings of the RCON listener.
#[derive(Clone)]
pub struct RconConfig {
    /// Address to listen on.
    pub bind: SocketAddr,
    /// `rcon.password` (not empty).
    pub password: String,
    /// Connections open at the same time.
    pub max_connections: usize,
    /// Failed logins per address and minute.
    pub max_failed_logins: u32,
    /// `log-ips`: addresses in the log.
    pub log_ips: bool,
}

impl std::fmt::Debug for RconConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The password never reaches a log.
        f.debug_struct("RconConfig")
            .field("bind", &self.bind)
            .field("max_connections", &self.max_connections)
            .field("max_failed_logins", &self.max_failed_logins)
            .finish_non_exhaustive()
    }
}

/// Runs a command line as the RCON source and passes its output to the reply, on the server's
/// thread (`DedicatedServer.runCommand`).
pub type RconRunner = Arc<dyn Fn(String, Box<dyn FnOnce(String) + Send>) + Send + Sync>;

/// Failed logins per address in the current window.
#[derive(Default)]
struct Failures(Mutex<HashMap<IpAddr, (Instant, u32)>>);

impl Failures {
    fn blocked(&self, ip: IpAddr, max: u32) -> bool {
        let map = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        map.get(&ip)
            .is_some_and(|(since, n)| since.elapsed() < FAILURE_WINDOW && *n >= max)
    }

    /// Counts a failed login; `true` when the address is now blocked.
    fn fail(&self, ip: IpAddr, max: u32) -> bool {
        let mut map = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if map.len() >= MAX_TRACKED {
            map.retain(|_, (since, _)| since.elapsed() < FAILURE_WINDOW);
        }
        if map.len() >= MAX_TRACKED {
            // Flooded with addresses: forget the oldest window rather than grow.
            map.clear();
        }
        let entry = map.entry(ip).or_insert((Instant::now(), 0));
        if entry.0.elapsed() >= FAILURE_WINDOW {
            *entry = (Instant::now(), 0);
        }
        entry.1 = entry.1.saturating_add(1);
        entry.1 >= max
    }
}

/// Binds the listener and starts accepting on the current tokio runtime.
pub(crate) async fn bind(config: RconConfig, runner: RconRunner) -> std::io::Result<SocketAddr> {
    let listener = TcpListener::bind(config.bind).await?;
    let local = listener.local_addr()?;
    log::info!("RCON running on {local}");
    let shared = Arc::new(Listener {
        permits: Arc::new(Semaphore::new(config.max_connections.max(1))),
        failures: Failures::default(),
        config,
        runner,
    });
    tokio::spawn(accept_loop(listener, shared));
    Ok(local)
}

struct Listener {
    config: RconConfig,
    runner: RconRunner,
    permits: Arc<Semaphore>,
    failures: Failures,
}

impl Listener {
    fn show(&self, addr: SocketAddr) -> String {
        if self.config.log_ips {
            addr.to_string()
        } else {
            "<ip address withheld>".to_owned()
        }
    }
}

async fn accept_loop(listener: TcpListener, shared: Arc<Listener>) {
    loop {
        let (socket, addr) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                log::debug!("RCON accept failed: {e}");
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        if shared
            .failures
            .blocked(addr.ip(), shared.config.max_failed_logins)
        {
            continue;
        }
        let Ok(permit) = Arc::clone(&shared.permits).try_acquire_owned() else {
            continue;
        };
        let shared = Arc::clone(&shared);
        tokio::spawn(async move {
            let _ = socket.set_nodelay(true);
            let _ = client(socket, addr, &shared).await;
            log::info!("Thread RCON Client {} shutting down", shared.show(addr));
            drop(permit);
        });
    }
}

/// One request: id, type and string.
#[derive(Debug, PartialEq, Eq)]
pub struct Request {
    pub id: i32,
    pub kind: i32,
    pub body: String,
}

/// Decodes the part after the length field (`RconClient.run`): `None` if it is too short.
pub fn decode(packet: &[u8]) -> Option<Request> {
    let (head, rest) = packet.split_first_chunk::<8>()?;
    if rest.len() < 2 {
        return None;
    }
    let id = i32::from_le_bytes(*head.first_chunk::<4>()?);
    let kind = i32::from_le_bytes(*head.last_chunk::<4>()?);
    // `PktUtils.stringFromByteArray`: up to the first NUL, never including the last byte.
    let text = rest.get(..rest.len().saturating_sub(1)).unwrap_or_default();
    let end = text.iter().position(|&b| b == 0).unwrap_or(text.len());
    let body = String::from_utf8_lossy(text.get(..end).unwrap_or_default()).into_owned();
    Some(Request { id, kind, body })
}

/// Encodes one packet (`RconClient.send`).
pub fn encode(out: &mut Vec<u8>, id: i32, kind: i32, body: &str) {
    let len = i32::try_from(body.len().saturating_add(10)).unwrap_or(i32::MAX);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&id.to_le_bytes());
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(body.as_bytes());
    out.extend_from_slice(&[0, 0]);
}

/// Encodes a command response in packets of up to 4096 UTF-16 units, at least one
/// (`RconClient.sendCmdResponse`).
pub fn encode_response(out: &mut Vec<u8>, id: i32, response: &str) {
    let mut rest = response;
    loop {
        let mut units = 0usize;
        let mut cut = rest.len();
        for (i, c) in rest.char_indices() {
            units = units.saturating_add(c.len_utf16());
            if units > RESPONSE_CHUNK {
                cut = i;
                break;
            }
        }
        let (chunk, tail) = rest.split_at(cut);
        encode(out, id, RESPONSE_VALUE, chunk);
        rest = tail;
        if rest.is_empty() {
            return;
        }
    }
}

/// Constant-time comparison (no early exit on the first difference).
fn same_secret(a: &[u8], b: &[u8]) -> bool {
    let mut diff = u8::from(a.len() != b.len());
    for (i, x) in a.iter().enumerate() {
        diff |= x ^ b.get(i).copied().unwrap_or(!x);
    }
    diff == 0
}

async fn client(mut socket: TcpStream, addr: SocketAddr, shared: &Listener) -> std::io::Result<()> {
    let mut authed = false;
    let mut buf = [0u8; MAX_PACKET];
    let started = Instant::now();
    loop {
        let read = async {
            let mut len = [0u8; 4];
            socket.read_exact(&mut len).await?;
            let len = usize::try_from(i32::from_le_bytes(len)).unwrap_or(0);
            // Vanilla needs at least 10 bytes and reads at most `MAX_PACKET` in total.
            if !(10..=MAX_PACKET.saturating_sub(4)).contains(&len) {
                return Err(std::io::ErrorKind::InvalidData.into());
            }
            let body = buf.get_mut(..len).ok_or(std::io::ErrorKind::InvalidData)?;
            socket.read_exact(body).await?;
            Ok::<_, std::io::Error>(len)
        };
        let len = if authed {
            read.await?
        } else {
            let left = LOGIN_TIMEOUT.saturating_sub(started.elapsed());
            tokio::time::timeout(left, read)
                .await
                .map_err(|_| std::io::Error::from(std::io::ErrorKind::TimedOut))??
        };
        let request = buf
            .get(..len)
            .and_then(decode)
            .ok_or(std::io::ErrorKind::InvalidData)?;
        let mut out = Vec::new();
        match request.kind {
            EXEC_COMMAND if authed => {
                let command = request.body;
                let (tx, rx) = tokio::sync::oneshot::channel();
                (shared.runner)(
                    command.clone(),
                    Box::new(move |output| {
                        let _ = tx.send(output);
                    }),
                );
                let response = rx.await.unwrap_or_else(|_| {
                    format!("Error executing: {command} (the server is stopping)")
                });
                encode_response(&mut out, request.id, &response);
            }
            EXEC_COMMAND => encode(&mut out, -1, AUTH_RESPONSE, ""),
            AUTH => {
                let password = shared.config.password.as_bytes();
                if !request.body.is_empty() && same_secret(request.body.as_bytes(), password) {
                    authed = true;
                    encode(&mut out, request.id, AUTH_RESPONSE, "");
                } else {
                    authed = false;
                    encode(&mut out, -1, AUTH_RESPONSE, "");
                    log::warn!("RCON: failed login from {}", shared.show(addr));
                    if shared
                        .failures
                        .fail(addr.ip(), shared.config.max_failed_logins)
                    {
                        socket.write_all(&out).await?;
                        return Ok(());
                    }
                }
            }
            kind => encode_response(&mut out, request.id, &format!("Unknown request {kind:x}")),
        }
        socket.write_all(&out).await?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_like_vanilla() {
        let mut packet = Vec::new();
        encode(&mut packet, 7, AUTH, "secret");
        let body = packet.get(4..).unwrap_or_default();
        assert_eq!(
            decode(body),
            Some(Request {
                id: 7,
                kind: AUTH,
                body: "secret".into()
            })
        );
        // Too short for id, type and padding.
        assert_eq!(decode(&[0; 9]), None);
    }

    #[test]
    fn splits_long_responses() {
        let mut out = Vec::new();
        encode_response(&mut out, 1, &"a".repeat(5000));
        // 4096 + 904 characters in two packets.
        assert_eq!(out.len(), 4 + 10 + 4096 + 4 + 10 + 904);
        let mut empty = Vec::new();
        encode_response(&mut empty, 1, "");
        assert_eq!(empty.len(), 14);
    }

    #[test]
    fn compares_secrets() {
        assert!(same_secret(b"abc", b"abc"));
        assert!(!same_secret(b"abc", b"abd"));
        assert!(!same_secret(b"abc", b"abcd"));
        assert!(!same_secret(b"", b"a"));
    }
}
