//! Per-state packet sets: one enum per state and direction with a decoder that dispatches on the
//! packet id. A packet of another state is an unknown id here and ends the connection (SEC-05).

use crate::codec::{Decode, Error, Reader, Result};
use crate::packets::{configuration as cfg, handshake, login, play, status};
use crate::{Bucket, Packet};

macro_rules! packet_set {
    ($(#[$m:meta])* $name:ident { $($variant:ident($ty:ty)),* $(,)? } $(unhandled $unhandled:ident)?) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq)]
        #[allow(clippy::large_enum_variant)]
        pub enum $name {
            $($variant($ty),)*
            $(
                /// A known packet of this state that the server does not handle yet; dropped.
                $unhandled { id: i32 },
            )?
        }

        impl $name {
            /// Decodes `id` + body; the body must be consumed completely.
            pub fn decode(packet: &[u8]) -> Result<Self> {
                let mut r = Reader::new(packet);
                let id = r.varint()?;
                $(
                    if id == <$ty as Packet>::ID {
                        let p = <$ty as Decode>::decode(&mut r)?;
                        r.finish(id)?;
                        return Ok(Self::$variant(p));
                    }
                )*
                $(
                    if Self::is_known(id) {
                        return Ok(Self::$unhandled { id });
                    }
                )?
                Err(Error::UnknownPacket { id })
            }

            /// Rate-limit bucket and cost (SEC-16).
            pub fn cost(&self) -> (Bucket, u16) {
                match self {
                    $(Self::$variant(_) => (<$ty as Packet>::BUCKET, <$ty as Packet>::COST),)*
                    $(Self::$unhandled { .. } => (Bucket::Default, 1),)?
                }
            }

            /// Encodes the packet as `id` + body (empty for unhandled packets).
            pub fn encode_body(&self) -> Vec<u8> {
                match self {
                    $(Self::$variant(p) => crate::frame::encode_body(p),)*
                    $(Self::$unhandled { .. } => Vec::new(),)?
                }
            }

            /// Vanilla name of the packet, for logs.
            pub fn name(&self) -> &'static str {
                match self {
                    $(Self::$variant(_) => <$ty as Packet>::NAME,)*
                    $(Self::$unhandled { .. } => "unhandled",)?
                }
            }
        }
    };
}

packet_set! {
    /// Serverbound handshake packets.
    ServerboundHandshake { Intention(handshake::Intention) }
}

packet_set! {
    /// Serverbound status packets.
    ServerboundStatus { StatusRequest(status::StatusRequest), PingRequest(status::PingRequest) }
}

packet_set! {
    /// Serverbound login packets.
    ServerboundLogin {
        Hello(login::Hello),
        Key(login::Key),
        CustomQueryAnswer(login::CustomQueryAnswer),
        LoginAcknowledged(login::LoginAcknowledged),
        CookieResponse(login::CookieResponse),
    }
}

packet_set! {
    /// Serverbound configuration packets.
    ServerboundConfiguration {
        ClientInformation(cfg::serverbound::ClientInformation),
        CookieResponse(cfg::serverbound::CookieResponse),
        CustomPayload(cfg::serverbound::CustomPayload),
        FinishConfiguration(cfg::serverbound::FinishConfiguration),
        KeepAlive(cfg::serverbound::KeepAlive),
        Pong(cfg::serverbound::Pong),
        ResourcePack(cfg::serverbound::ResourcePack),
        SelectKnownPacks(cfg::serverbound::SelectKnownPacks),
        CustomClickAction(cfg::serverbound::CustomClickAction),
        AcceptCodeOfConduct(cfg::serverbound::AcceptCodeOfConduct),
    }
}

impl ServerboundPlay {
    fn is_known(id: i32) -> bool {
        usize::try_from(id).is_ok_and(|i| i < crate::ids::play::serverbound::NAMES.len())
    }
}

packet_set! {
    /// Serverbound play packets (milestone 1 subset; other known ids decode as `Unhandled`).
    ServerboundPlay {
        AcceptTeleportation(play::serverbound::AcceptTeleportation),
        Attack(play::serverbound::Attack),
        Interact(play::serverbound::Interact),
        MoveVehicle(play::serverbound::MoveVehicle),
        PaddleBoat(play::serverbound::PaddleBoat),
        ContainerClick(play::serverbound::ContainerClick),
        ContainerClose(play::serverbound::ContainerClose),
        ContainerButtonClick(play::serverbound::ContainerButtonClick),
        ContainerSlotStateChanged(play::serverbound::ContainerSlotStateChanged),
        RenameItem(play::serverbound::RenameItem),
        SetBeacon(play::serverbound::SetBeacon),
        SignUpdate(play::serverbound::SignUpdate),
        PlaceRecipe(play::serverbound::PlaceRecipe),
        RecipeBookSeenRecipe(play::serverbound::RecipeBookSeenRecipe),
        RecipeBookChangeSettings(play::serverbound::RecipeBookChangeSettings),
        PickItemFromBlock(play::serverbound::PickItemFromBlock),
        PlayerAction(play::serverbound::PlayerActionPacket),
        Punch(play::serverbound::Punch),
        SetCarriedItem(play::serverbound::SetCarriedItem),
        SetCreativeModeSlot(play::serverbound::SetCreativeModeSlot),
        UseItem(play::serverbound::UseItem),
        UseItemOn(play::serverbound::UseItemOn),
        Chat(play::serverbound::Chat),
        ChatAck(play::serverbound::ChatAck),
        ChatCommand(play::serverbound::ChatCommand),
        ChatCommandSigned(play::serverbound::ChatCommandSigned),
        ChatSessionUpdate(play::serverbound::ChatSessionUpdate),
        CommandSuggestion(play::serverbound::CommandSuggestion),
        ClientCommand(play::serverbound::ClientCommand),
        ChunkBatchReceived(play::serverbound::ChunkBatchReceived),
        ClientInformation(play::serverbound::ClientInformation),
        ClientTickEnd(play::serverbound::ClientTickEnd),
        ConfigurationAcknowledged(play::serverbound::ConfigurationAcknowledged),
        CookieResponse(play::serverbound::CookieResponse),
        CustomClickAction(play::serverbound::CustomClickAction),
        CustomPayload(play::serverbound::CustomPayload),
        KeepAlive(play::serverbound::KeepAlive),
        MovePlayerPos(play::serverbound::MovePlayerPos),
        MovePlayerPosRot(play::serverbound::MovePlayerPosRot),
        MovePlayerRot(play::serverbound::MovePlayerRot),
        MovePlayerStatusOnly(play::serverbound::MovePlayerStatusOnly),
        PlayerCommand(play::serverbound::PlayerCommand),
        PlayerInput(play::serverbound::PlayerInput),
        PingRequest(play::serverbound::PingRequest),
        PlayerLoaded(play::serverbound::PlayerLoaded),
        Pong(play::serverbound::Pong),
        ResourcePack(play::serverbound::ResourcePack),
        SeenAdvancements(play::serverbound::SeenAdvancementsPacket),
        SetCommandBlock(play::serverbound::SetCommandBlock),
        SetCommandMinecart(play::serverbound::SetCommandMinecart),
        SetStructureBlock(play::serverbound::SetStructureBlock),
        SetJigsawBlock(play::serverbound::SetJigsawBlock),
        JigsawGenerate(play::serverbound::JigsawGenerate),
        SetTestBlock(play::serverbound::SetTestBlock),
        TestInstanceBlockAction(play::serverbound::TestInstanceBlockAction),
    }
    unhandled Unhandled
}

packet_set! {
    /// Clientbound status packets (for the test client).
    ClientboundStatus { StatusResponse(status::StatusResponse), PongResponse(status::PongResponse) }
}

packet_set! {
    /// Clientbound login packets (for the test client).
    ClientboundLogin {
        LoginDisconnect(login::LoginDisconnect),
        EncryptionRequest(login::EncryptionRequest),
        LoginFinished(login::LoginFinished),
        LoginCompression(login::LoginCompression),
        CustomQuery(login::CustomQuery),
        CookieRequest(login::CookieRequest),
    }
}

packet_set! {
    /// Clientbound configuration packets (for the test client).
    ClientboundConfiguration {
        CookieRequest(cfg::clientbound::CookieRequest),
        CustomPayload(cfg::clientbound::CustomPayload),
        Disconnect(cfg::clientbound::Disconnect),
        FinishConfiguration(cfg::clientbound::FinishConfiguration),
        KeepAlive(cfg::clientbound::KeepAlive),
        Ping(cfg::clientbound::Ping),
        ResetChat(cfg::clientbound::ResetChat),
        RegistryData(cfg::clientbound::RegistryData),
        StoreCookie(cfg::clientbound::StoreCookie),
        Transfer(cfg::clientbound::Transfer),
        UpdateEnabledFeatures(cfg::clientbound::UpdateEnabledFeatures),
        UpdateTags(cfg::clientbound::UpdateTags),
        SelectKnownPacks(cfg::clientbound::SelectKnownPacks),
        CodeOfConduct(cfg::clientbound::CodeOfConduct),
        ResourcePackPush(cfg::clientbound::ResourcePackPush),
        ServerLinks(cfg::clientbound::ServerLinks),
    }
}

impl ClientboundPlay {
    fn is_known(id: i32) -> bool {
        usize::try_from(id).is_ok_and(|i| i < crate::ids::play::clientbound::NAMES.len())
    }
}

packet_set! {
    /// Clientbound play packets of milestone 1 (for the test client); others decode as
    /// `Unhandled`.
    ClientboundPlay {
        AddEntity(play::clientbound::AddEntity),
        AwardStats(play::clientbound::AwardStats),
        BlockChangedAck(play::clientbound::BlockChangedAck),
        BlockDestruction(play::clientbound::BlockDestruction),
        BlockUpdate(play::clientbound::BlockUpdate),
        ContainerSetContent(play::clientbound::ContainerSetContent),
        ContainerSetSlot(play::clientbound::ContainerSetSlot),
        LevelEvent(play::clientbound::LevelEvent),
        SectionBlocksUpdate(play::clientbound::SectionBlocksUpdate),
        SetCursorItem(play::clientbound::SetCursorItem),
        SetEquipment(play::clientbound::SetEquipment),
        SetHeldSlot(play::clientbound::SetHeldSlot),
        SetPlayerInventory(play::clientbound::SetPlayerInventory),
        SwingAnimation(play::clientbound::SwingAnimation),
        TakeItemEntity(play::clientbound::TakeItemEntity),
        BundleDelimiter(play::clientbound::BundleDelimiter),
        ChangeDifficulty(play::clientbound::ChangeDifficulty),
        ChunkBatchFinished(play::clientbound::ChunkBatchFinished),
        ChunkBatchStart(play::clientbound::ChunkBatchStart),
        CommandSuggestions(play::clientbound::CommandSuggestions),
        Commands(play::clientbound::Commands),
        DamageEvent(play::clientbound::DamageEvent),
        Disconnect(play::clientbound::Disconnect),
        DisguisedChat(play::clientbound::DisguisedChat),
        EntityEvent(play::clientbound::EntityEvent),
        EntityPositionSync(play::clientbound::EntityPositionSync),
        ForgetLevelChunk(play::clientbound::ForgetLevelChunk),
        GameEvent(play::clientbound::GameEvent),
        HurtAnimation(play::clientbound::HurtAnimation),
        KeepAlive(play::clientbound::KeepAlive),
        LevelChunkWithLight(play::clientbound::LevelChunkWithLight),
        Login(play::clientbound::Login),
        MoveEntityPos(play::clientbound::MoveEntityPos),
        MoveEntityPosRot(play::clientbound::MoveEntityPosRot),
        MoveEntityRot(play::clientbound::MoveEntityRot),
        PlayerAbilities(play::clientbound::PlayerAbilities),
        PlayerChat(play::clientbound::PlayerChat),
        PlayerCombatKill(play::clientbound::PlayerCombatKill),
        PlayerLookAt(play::clientbound::PlayerLookAt),
        PlayerInfoRemove(play::clientbound::PlayerInfoRemove),
        PlayerInfoUpdate(play::clientbound::PlayerInfoUpdate),
        PlayerPosition(play::clientbound::PlayerPosition),
        PongResponse(play::clientbound::PongResponse),
        RemoveEntities(play::clientbound::RemoveEntities),
        Respawn(play::clientbound::Respawn),
        RotateHead(play::clientbound::RotateHead),
        SetChunkCacheCenter(play::clientbound::SetChunkCacheCenter),
        SetDefaultSpawnPosition(play::clientbound::SetDefaultSpawnPosition),
        SetEntityData(play::clientbound::SetEntityData),
        SetEntityMotion(play::clientbound::SetEntityMotion),
        SetExperience(play::clientbound::SetExperience),
        SetHealth(play::clientbound::SetHealth),
        SetTime(play::clientbound::SetTime),
        Sound(play::clientbound::Sound),
        UpdateAdvancements(play::clientbound::UpdateAdvancements),
        UpdateMobEffect(play::clientbound::UpdateMobEffect),
        RemoveMobEffect(play::clientbound::RemoveMobEffect),
        SystemChat(play::clientbound::SystemChat),
    }
    unhandled Unhandled
}
