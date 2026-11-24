//! Configuration state.

/// Serverbound configuration packets.
pub mod serverbound {
    use crate::Packet;
    use crate::types::KnownPack;

    crate::common_serverbound!(Configuration);

    /// `finish_configuration`: the client is ready for play.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Configuration, dir = Serverbound, name = "finish_configuration")]
    pub struct FinishConfiguration;

    /// `select_known_packs`: the packs the client has of those the server offered (max 64).
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Configuration, dir = Serverbound, name = "select_known_packs")]
    pub struct SelectKnownPacks {
        #[field(max_len = 64)]
        pub packs: Vec<KnownPack>,
    }

    /// `accept_code_of_conduct`.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Configuration, dir = Serverbound, name = "accept_code_of_conduct")]
    pub struct AcceptCodeOfConduct;
}

/// Clientbound configuration packets.
pub mod clientbound {
    use crate::Packet;
    use crate::codec::{Decode, Encode, Reader, Result};
    use crate::types::{Identifier, KnownPack, RawNbt};

    crate::common_clientbound!(Configuration);

    /// `finish_configuration`.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Configuration, dir = Clientbound, name = "finish_configuration")]
    pub struct FinishConfiguration;

    /// `reset_chat`.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Configuration, dir = Clientbound, name = "reset_chat")]
    pub struct ResetChat;

    /// One registry entry: its id and, unless the client knows it from a shared pack, its data.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct RegistryEntry {
        pub id: Identifier,
        pub data: Option<RawNbt>,
    }

    impl Encode for RegistryEntry {
        fn encode(&self, out: &mut Vec<u8>) {
            self.id.encode(out);
            self.data.encode(out);
        }
    }

    impl Decode for RegistryEntry {
        fn decode(r: &mut Reader<'_>) -> Result<Self> {
            use crate::codec::DecodeLimited;
            Ok(Self {
                id: Identifier::decode(r)?,
                data: Option::<RawNbt>::decode_limited(r, 2_097_152)?,
            })
        }
    }

    /// `registry_data`: the entries of one synchronised registry, in network id order.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Configuration, dir = Clientbound, name = "registry_data")]
    pub struct RegistryData {
        pub registry: Identifier,
        #[field(max_len = 1 << 20)]
        pub entries: Vec<RegistryEntry>,
    }

    /// `update_enabled_features`: feature flags of the world.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Configuration, dir = Clientbound, name = "update_enabled_features")]
    pub struct UpdateEnabledFeatures {
        #[field(max_len = 1024)]
        pub features: Vec<Identifier>,
    }

    /// `select_known_packs`: the packs the server's registries come from.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Configuration, dir = Clientbound, name = "select_known_packs")]
    pub struct SelectKnownPacks {
        #[field(max_len = 64)]
        pub packs: Vec<KnownPack>,
    }

    /// `code_of_conduct`: text the player has to accept.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Configuration, dir = Clientbound, name = "code_of_conduct")]
    pub struct CodeOfConduct {
        #[field(max_len = 32767)]
        pub text: String,
    }
}
