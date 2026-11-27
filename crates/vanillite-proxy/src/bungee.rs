//! The BungeeCord plugin message channel (`bungeecord:main`), which many backend plugins use to
//! ask the proxy about players and servers, move players and message other servers. A request
//! arrives as a clientbound plugin message on the player's backend connection; the answer goes
//! back on the same connection. The data is Java `DataOutput`: strings as a two-byte length and
//! UTF-8, numbers big-endian. Unknown or malformed requests are ignored.

use std::collections::BTreeMap;
use std::sync::Arc;

use vanillite_protocol::text::Component;
use vanillite_protocol::types::Uuid;

use crate::admin::legacy_colors;
use crate::{Control, Shared};

/// The channel's name since 1.13.
pub(crate) const CHANNEL: &str = "bungeecord:main";

/// The player whose server sent a request.
#[derive(Debug)]
pub(crate) struct Me<'a> {
    pub(crate) id: Uuid,
    pub(crate) server: Option<&'a str>,
}

/// What the session does with a request.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Reply {
    Nothing,
    /// Send this answer to the player's server.
    Answer(Vec<u8>),
    /// Move the player to this server.
    Connect(String),
}

/// Reads `DataInput` values.
struct In<'a>(&'a [u8]);

impl In<'_> {
    fn bytes(&mut self, n: usize) -> Option<&[u8]> {
        let (head, rest) = self.0.split_at_checked(n)?;
        self.0 = rest;
        Some(head)
    }

    fn short(&mut self) -> Option<u16> {
        let b = self.bytes(2)?;
        Some(u16::from_be_bytes([*b.first()?, *b.get(1)?]))
    }

    fn utf(&mut self) -> Option<String> {
        let n = usize::from(self.short()?);
        String::from_utf8(self.bytes(n)?.to_vec()).ok()
    }
}

/// Writes `DataOutput` values.
#[derive(Default)]
struct Out(Vec<u8>);

impl Out {
    fn utf(mut self, s: &str) -> Self {
        let bytes = s.as_bytes();
        let n = u16::try_from(bytes.len()).unwrap_or(u16::MAX);
        self.0.extend_from_slice(&n.to_be_bytes());
        self.0
            .extend_from_slice(bytes.get(..usize::from(n)).unwrap_or_default());
        self
    }

    fn int(mut self, v: i32) -> Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }

    fn short(mut self, v: u16) -> Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
}

/// Answers one request.
pub(crate) fn handle(shared: &Shared, me: &Me<'_>, data: &[u8]) -> Reply {
    handle_inner(shared, me, data).unwrap_or(Reply::Nothing)
}

fn handle_inner(shared: &Shared, me: &Me<'_>, data: &[u8]) -> Option<Reply> {
    let mut r = In(data);
    let sub = r.utf()?;
    let config = shared.config();
    let answer = |out: Out| Some(Reply::Answer(out.0));
    match sub.as_str() {
        "Connect" => Some(Reply::Connect(r.utf()?)),
        "ConnectOther" => {
            let name = r.utf()?;
            let server = r.utf()?;
            let (server, _) = config.servers.get_key_value(&server)?;
            shared.control_by_name(&name, Control::Send(Arc::from(server.as_str())));
            Some(Reply::Nothing)
        }
        "IP" => {
            let address = shared.players.lock().ok()?.get(&me.id)?.address;
            answer(
                Out::default()
                    .utf("IP")
                    .utf(&address.ip().to_string())
                    .int(i32::from(address.port())),
            )
        }
        "IPOther" => {
            let name = r.utf()?;
            let ((name, address), _) = find(shared, &name, |p| (p.name.clone(), p.address))?;
            answer(
                Out::default()
                    .utf("IPOther")
                    .utf(&name)
                    .utf(&address.ip().to_string())
                    .int(i32::from(address.port())),
            )
        }
        "PlayerCount" | "PlayerList" => {
            let server = r.utf()?;
            let players = shared.players.lock().ok()?;
            let mut names: Vec<&str> = players
                .values()
                .filter(|p| server == "ALL" || p.server.as_deref() == Some(server.as_str()))
                .map(|p| p.name.as_str())
                .collect();
            let out = Out::default().utf(&sub).utf(&server);
            if sub == "PlayerCount" {
                answer(out.int(i32::try_from(names.len()).unwrap_or(i32::MAX)))
            } else {
                names.sort_unstable();
                answer(out.utf(&names.join(", ")))
            }
        }
        "GetServers" => {
            let names: Vec<&str> = config.servers.keys().map(String::as_str).collect();
            answer(Out::default().utf("GetServers").utf(&names.join(", ")))
        }
        "GetServer" => answer(Out::default().utf("GetServer").utf(me.server?)),
        "GetPlayerServer" => {
            let name = r.utf()?;
            let ((name, server), _) = find(shared, &name, |p| (p.name.clone(), p.server.clone()))?;
            answer(
                Out::default()
                    .utf("GetPlayerServer")
                    .utf(&name)
                    .utf(server.as_deref().unwrap_or_default()),
            )
        }
        "UUID" => answer(Out::default().utf("UUID").utf(&format!("{:032x}", me.id.0))),
        "UUIDOther" => {
            let name = r.utf()?;
            let (name, id) = find(shared, &name, |p| p.name.clone())?;
            answer(
                Out::default()
                    .utf("UUIDOther")
                    .utf(&name)
                    .utf(&format!("{:032x}", id.0)),
            )
        }
        "ServerIP" => {
            let name = r.utf()?;
            let address = config.servers.get(&name)?;
            let (host, port) = address.rsplit_once(':')?;
            answer(
                Out::default()
                    .utf("ServerIP")
                    .utf(&name)
                    .utf(host)
                    .short(port.parse().ok()?),
            )
        }
        "Message" | "MessageRaw" => {
            let target = r.utf()?;
            let text = r.utf()?;
            let message = if sub == "Message" {
                Component::text(legacy_colors(&text))
            } else {
                Component::from_json(&serde_json::from_str(&text).ok()?)
            };
            if target == "ALL" {
                shared.control_all(|_| true, || Control::Chat(message.clone()));
            } else {
                shared.control_by_name(&target, Control::Chat(message));
            }
            Some(Reply::Nothing)
        }
        "KickPlayer" | "KickPlayerRaw" => {
            let name = r.utf()?;
            let text = r.utf()?;
            let reason = if sub == "KickPlayer" {
                Component::text(legacy_colors(&text))
            } else {
                Component::from_json(&serde_json::from_str(&text).ok()?)
            };
            shared.control_by_name(&name, Control::Kick(reason));
            Some(Reply::Nothing)
        }
        "Forward" => {
            let target = r.utf()?;
            let message = forwarded(&mut r)?;
            // One player per server carries the message; ALL and ONLINE skip the sender's.
            let mut carriers = BTreeMap::new();
            if let Ok(players) = shared.players.lock() {
                for p in players.values() {
                    let Some(server) = p.server.as_deref() else {
                        continue;
                    };
                    let wanted = match target.as_str() {
                        "ALL" | "ONLINE" => Some(server) != me.server,
                        name => name == server,
                    };
                    if wanted {
                        carriers
                            .entry(server.to_owned())
                            .or_insert_with(|| p.control.clone());
                    }
                }
            }
            for control in carriers.values() {
                let _ = control.try_send(Control::Payload(message.clone()));
            }
            Some(Reply::Nothing)
        }
        "ForwardToPlayer" => {
            let name = r.utf()?;
            let message = forwarded(&mut r)?;
            shared.control_by_name(&name, Control::Payload(message));
            Some(Reply::Nothing)
        }
        _ => None,
    }
}

/// The channel and data of a `Forward` request, as the receiving server gets them.
fn forwarded(r: &mut In<'_>) -> Option<Vec<u8>> {
    let channel = r.utf()?;
    let n = r.short()?;
    let data = r.bytes(usize::from(n))?;
    let mut out = Out::default().utf(&channel).short(n).0;
    out.extend_from_slice(data);
    Some(out)
}

/// A player by name (any case): `f` of the entry and the player's UUID.
fn find<T>(shared: &Shared, name: &str, f: impl Fn(&crate::Online) -> T) -> Option<(T, Uuid)> {
    let players = shared.players.lock().ok()?;
    players
        .iter()
        .find(|(_, p)| p.name.eq_ignore_ascii_case(name))
        .map(|(id, p)| (f(p), *id))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn data_io() {
        let out = Out::default().utf("PlayerCount").utf("lobby").int(3).0;
        let mut r = In(&out);
        assert_eq!(r.utf().as_deref(), Some("PlayerCount"));
        assert_eq!(r.utf().as_deref(), Some("lobby"));
        assert_eq!(r.bytes(4), Some(&[0, 0, 0, 3][..]));
        assert_eq!(r.short(), None);
        let request = Out::default().utf("ch").short(2).0;
        let mut with_data = request.clone();
        with_data.extend_from_slice(&[7, 8]);
        assert_eq!(forwarded(&mut In(&with_data)).unwrap(), with_data);
        assert_eq!(forwarded(&mut In(&request)), None);
    }
}
