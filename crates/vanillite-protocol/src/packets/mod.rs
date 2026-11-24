//! Packet definitions per connection state. Ids come from the generated table through
//! `#[derive(Packet)]`; field limits follow Vanilla's codecs (26.3).

pub mod common;
pub mod configuration;
pub mod handshake;
pub mod login;
pub mod play;
pub mod status;
