//! Status state (server list ping).

use crate::Packet;

/// `status_request`.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Status, dir = Serverbound, name = "status_request")]
pub struct StatusRequest;

/// `ping_request`.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Status, dir = Serverbound, name = "ping_request")]
pub struct PingRequest {
    pub time: i64,
}

/// `status_response`: the server status as JSON.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Status, dir = Clientbound, name = "status_response")]
pub struct StatusResponse {
    #[field(max_len = 32767)]
    pub json: String,
}

/// `pong_response`.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Status, dir = Clientbound, name = "pong_response")]
pub struct PongResponse {
    pub time: i64,
}
