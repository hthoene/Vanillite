//! Handshake state.

use crate::Packet;
use crate::codec::{Decode, Encode, Error, Reader, Result, write_varint};

/// What the client wants after the handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Intent {
    Status,
    Login,
    /// Login after a transfer packet from another server.
    Transfer,
}

impl Encode for Intent {
    fn encode(&self, out: &mut Vec<u8>) {
        write_varint(
            out,
            match self {
                Self::Status => 1,
                Self::Login => 2,
                Self::Transfer => 3,
            },
        );
    }
}

impl Decode for Intent {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        match r.varint()? {
            1 => Ok(Self::Status),
            2 => Ok(Self::Login),
            3 => Ok(Self::Transfer),
            _ => Err(Error::Invalid("connection intent")),
        }
    }
}

/// `intention`: protocol version, address the client used, and the intent.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Handshake, dir = Serverbound, name = "intention")]
pub struct Intention {
    #[field(varint)]
    pub protocol_version: i32,
    #[field(max_len = 255)]
    pub host: String,
    pub port: u16,
    pub intent: Intent,
}
