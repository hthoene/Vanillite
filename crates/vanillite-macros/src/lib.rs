//! Proc macros: `#[derive(Packet)]`, the rule catalogue and behaviour tables.

mod packet;

use proc_macro::TokenStream;

/// Derives `Encode`, `Decode` and `Packet` for a packet struct (architecture 8.2).
///
/// ```ignore
/// #[derive(Packet)]
/// #[packet(state = Play, dir = Serverbound, name = "move_player_pos", cost = 1, bucket = "movement")]
/// pub struct MovePlayerPos {
///     #[field(finite)] pub x: f64,
///     #[field(finite, range = "-2.0E7..=2.0E7")] pub y: f64,
///     #[field(finite)] pub z: f64,
///     pub flags: MoveFlags,
/// }
/// ```
///
/// Struct attribute `packet`: `state` (Handshake, Status, Login, Configuration, Play), `dir`
/// (Serverbound, Clientbound), `name` (the key in Vanilla's `packets.json` without namespace; the
/// id comes from the generated table), optional `cost` (default 1) and `bucket` (default
/// "default") for rate limiting (SEC-16).
///
/// Field attribute `field`: `varint`/`varlong` for VarInt-encoded integers, `rest` for a last field
/// that takes the rest of the packet (with `max_len`), `max_len = N` for
/// strings, lists, byte arrays, NBT and components (mandatory for those types, SEC-02),
/// `finite` for floating values, `range = "a..=b"` for a checked range.
#[proc_macro_derive(Packet, attributes(packet, field))]
pub fn derive_packet(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as syn::DeriveInput);
    packet::expand(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
