//! Wire types of secure chat: message signatures, last-seen updates, chat sessions, filter masks
//! and bound chat types (Vanilla's `net.minecraft.network.chat`).

use crate::codec::{Decode, DecodeLimited, Encode, Error, Reader, Result, write_len, write_varint};
use crate::text::Component;
use crate::types::{BitSet, Uuid};

/// Bytes of a message signature (RSA-2048, `MessageSignature.BYTES`).
pub const SIGNATURE_BYTES: usize = 256;
/// Size of the last-seen window (`LastSeenMessages.LAST_SEEN_MESSAGES_MAX_LENGTH`).
pub const LAST_SEEN_COUNT: usize = 20;
/// Maximum argument signatures of a signed command (`ArgumentSignatures.MAX_ARGUMENT_COUNT`).
pub const MAX_ARGUMENT_SIGNATURES: usize = 8;
/// Maximum length of a signed argument name.
pub const MAX_ARGUMENT_NAME: usize = 16;
/// Maximum bytes of an encoded public key (`ByteBufCodecs.PUBLIC_KEY`).
pub const MAX_PUBLIC_KEY_BYTES: usize = 512;
/// Maximum bytes of a key signature (`ProfilePublicKey.Data.MAX_KEY_SIGNATURE_SIZE`).
pub const MAX_KEY_SIGNATURE_BYTES: usize = 4096;
/// Maximum characters of a chat message.
pub const MAX_MESSAGE_LENGTH: usize = 256;

/// A message signature: exactly [`SIGNATURE_BYTES`] bytes.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct MessageSignature(pub Box<[u8; SIGNATURE_BYTES]>);

impl std::fmt::Debug for MessageSignature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let head = self.0.first_chunk::<4>().copied().unwrap_or_default();
        write!(f, "MessageSignature({head:02x?}..)")
    }
}

impl MessageSignature {
    /// Java's `Arrays.hashCode(byte[])`, used for last-seen checksums.
    pub fn checksum(&self) -> i32 {
        self.0.iter().fold(1i32, |h, &b| {
            h.wrapping_mul(31)
                .wrapping_add(i32::from(i8::from_ne_bytes([b])))
        })
    }
}

impl Encode for MessageSignature {
    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.0[..]);
    }
}

impl Decode for MessageSignature {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        Ok(Self(Box::new(r.array::<SIGNATURE_BYTES>()?)))
    }
}

/// `LastSeenMessages.Update`: how far the client moved its window, which of the 20 tracked
/// messages it has seen (`fixedBitSet(20)`, 3 bytes) and a checksum (0 = not checked).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LastSeenUpdate {
    /// Messages the window advanced by.
    pub offset: i32,
    /// Acknowledged bits, little-endian bit order like Java's `BitSet.valueOf`.
    pub acknowledged: [u8; 3],
    /// `LastSeenMessages.computeChecksum` of the acknowledged signatures, or 0.
    pub checksum: i8,
}

impl LastSeenUpdate {
    /// Whether bit `i` is set.
    pub fn is_acknowledged(&self, i: usize) -> bool {
        self.acknowledged
            .get(i / 8)
            .is_some_and(|b| b & (1 << (i % 8)) != 0)
    }

    /// Java's `BitSet.length()`: index of the highest set bit plus one.
    pub fn length(&self) -> usize {
        (1..=24usize)
            .rev()
            .find(|&n| self.is_acknowledged(n.saturating_sub(1)))
            .unwrap_or(0)
    }
}

impl Encode for LastSeenUpdate {
    fn encode(&self, out: &mut Vec<u8>) {
        write_varint(out, self.offset);
        out.extend_from_slice(&self.acknowledged);
        self.checksum.encode(out);
    }
}

impl Decode for LastSeenUpdate {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        Ok(Self {
            offset: r.varint()?,
            acknowledged: r.array::<3>()?,
            checksum: r.i8()?,
        })
    }
}

/// One entry of `ArgumentSignatures`: a signed command argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArgumentSignature {
    /// Argument name (at most 16 characters).
    pub name: String,
    /// Its signature.
    pub signature: MessageSignature,
}

impl Encode for ArgumentSignature {
    fn encode(&self, out: &mut Vec<u8>) {
        self.name.encode(out);
        self.signature.encode(out);
    }
}

impl Decode for ArgumentSignature {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        Ok(Self {
            name: r.string(MAX_ARGUMENT_NAME)?.to_owned(),
            signature: MessageSignature::decode(r)?,
        })
    }
}

/// `MessageSignature.Packed`: an id into the receiver's signature cache or the full signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackedSignature {
    /// Index in the receiver's `MessageSignatureCache`.
    Id(i32),
    /// The full signature.
    Full(MessageSignature),
}

impl Encode for PackedSignature {
    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Self::Id(id) => write_varint(out, id.wrapping_add(1)),
            Self::Full(signature) => {
                write_varint(out, 0);
                signature.encode(out);
            }
        }
    }
}

impl Decode for PackedSignature {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        match r.varint()? {
            0 => Ok(Self::Full(MessageSignature::decode(r)?)),
            id => Ok(Self::Id(id.wrapping_sub(1))),
        }
    }
}

/// `RemoteChatSession.Data`: a chat session with the player's profile public key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatSessionData {
    /// Session id.
    pub session_id: Uuid,
    /// Expiry of the key in epoch milliseconds.
    pub expires_at: i64,
    /// X.509 DER encoded RSA public key (at most 512 bytes).
    pub public_key: Vec<u8>,
    /// Mojang's signature of the key (at most 4096 bytes).
    pub key_signature: Vec<u8>,
}

impl Encode for ChatSessionData {
    fn encode(&self, out: &mut Vec<u8>) {
        self.session_id.encode(out);
        self.expires_at.encode(out);
        write_len(out, self.public_key.len());
        out.extend_from_slice(&self.public_key);
        write_len(out, self.key_signature.len());
        out.extend_from_slice(&self.key_signature);
    }
}

impl Decode for ChatSessionData {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        let session_id = Uuid::decode(r)?;
        let expires_at = r.i64()?;
        let len = r.len_prefix("public key", MAX_PUBLIC_KEY_BYTES, 1)?;
        let public_key = r.take(len)?.to_vec();
        let len = r.len_prefix("key signature", MAX_KEY_SIGNATURE_BYTES, 1)?;
        let key_signature = r.take(len)?.to_vec();
        Ok(Self {
            session_id,
            expires_at,
            public_key,
            key_signature,
        })
    }
}

/// `FilterMask`: which characters of a message the text filter hid.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum FilterMask {
    /// Nothing filtered.
    #[default]
    PassThrough,
    /// The whole message is hidden.
    FullyFiltered,
    /// The characters whose bits are set are hidden.
    PartiallyFiltered(BitSet),
}

impl Encode for FilterMask {
    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Self::PassThrough => write_varint(out, 0),
            Self::FullyFiltered => write_varint(out, 1),
            Self::PartiallyFiltered(mask) => {
                write_varint(out, 2);
                mask.encode(out);
            }
        }
    }
}

impl Decode for FilterMask {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        match r.varint()? {
            1 => Ok(Self::FullyFiltered),
            2 => Ok(Self::PartiallyFiltered(BitSet::decode_limited(r, 1 << 12)?)),
            // `ByIdMap.OutOfBoundsStrategy.ZERO`.
            _ => Ok(Self::PassThrough),
        }
    }
}

/// `ChatType.Bound`: a chat type from the `minecraft:chat_type` registry with the sender's name
/// and the optional target name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatTypeBound {
    /// Network id of the chat type in the registry the client received.
    pub chat_type: i32,
    /// Sender name.
    pub name: Component,
    /// Target name (direct and team messages).
    pub target_name: Option<Component>,
}

/// Quota of a component in a chat packet (`ComponentSerialization.TRUSTED_STREAM_CODEC` reads
/// unlimited NBT; the client limit is the packet size).
const COMPONENT_QUOTA: usize = 2_097_152;

impl Encode for ChatTypeBound {
    fn encode(&self, out: &mut Vec<u8>) {
        // `ByteBufCodecs.holder`: registry id + 1; 0 would be an inline chat type.
        write_varint(out, self.chat_type.wrapping_add(1));
        self.name.encode(out);
        self.target_name.encode(out);
    }
}

impl Decode for ChatTypeBound {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        let id = r.varint()?;
        if id == 0 {
            return Err(Error::Invalid("inline chat types are not supported"));
        }
        Ok(Self {
            chat_type: id.wrapping_sub(1),
            name: Component::decode_limited(r, COMPONENT_QUOTA)?,
            target_name: Option::<Component>::decode_limited(r, COMPONENT_QUOTA)?,
        })
    }
}
