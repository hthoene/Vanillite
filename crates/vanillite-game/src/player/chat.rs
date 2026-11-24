//! Secure chat: chat sessions with Mojang-signed profile keys, the signed message chain of a
//! sender, the last-seen window and signature cache of a receiver, and spam throttling
//! (Vanilla's `RemoteChatSession`, `SignedMessageChain`, `LastSeenMessagesValidator`,
//! `MessageSignatureCache` and `TickThrottler`).

use std::collections::VecDeque;
use std::sync::Arc;

use rsa::pkcs8::DecodePublicKey;
use rsa::{Pkcs1v15Sign, RsaPublicKey};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use vanillite_protocol::chat::{
    ChatSessionData, ChatTypeBound, FilterMask, LAST_SEEN_COUNT, LastSeenUpdate, MessageSignature,
    PackedSignature,
};
use vanillite_protocol::packets::common::ChatVisibility;
use vanillite_protocol::packets::play::clientbound::{DisguisedChat, PlayerChat};
use vanillite_protocol::text::Component;
use vanillite_protocol::types::Uuid;

use super::movement::Kick;
use super::sink::PacketSink;

/// Entries of a receiver's signature cache (`MessageSignatureCache.DEFAULT_CAPACITY`).
const SIGNATURE_CACHE_SIZE: usize = 128;
/// Messages after which an unsigned key counts as expired for the server log
/// (`PlayerChatMessage.MESSAGE_EXPIRES_AFTER_SERVER`, 5 minutes).
const MESSAGE_EXPIRES_AFTER_SERVER_MS: i64 = 5 * 60 * 1000;

/// The current time in epoch milliseconds.
pub fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Mojang's public keys that sign players' profile keys (`ServicesKeyType.PROFILE_KEY`).
#[derive(Default)]
pub struct ServicesKeys {
    keys: Vec<RsaPublicKey>,
}

impl std::fmt::Debug for ServicesKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ServicesKeys({} keys)", self.keys.len())
    }
}

impl ServicesKeys {
    /// Keys from X.509 `SubjectPublicKeyInfo` DER; invalid keys are skipped.
    pub fn from_der<'a>(keys: impl IntoIterator<Item = &'a [u8]>) -> ServicesKeys {
        ServicesKeys {
            keys: keys
                .into_iter()
                .filter_map(|der| RsaPublicKey::from_public_key_der(der).ok())
                .collect(),
        }
    }

    /// Whether profile keys can be validated (`Services.canValidateProfileKeys`).
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// `SignatureValidator.from(keySet, PROFILE_KEY)`: SHA1withRSA by any of the keys.
    fn validate(&self, payload: &[u8], signature: &[u8]) -> bool {
        let digest = Sha1::digest(payload);
        self.keys.iter().any(|key| {
            key.verify(Pkcs1v15Sign::new::<Sha1>(), &digest, signature)
                .is_ok()
        })
    }
}

/// Chat settings of the server.
#[derive(Debug, Clone)]
pub struct ChatSettings {
    /// `enforce-secure-profile` in online mode with Mojang's keys available
    /// (`DedicatedServer.enforceSecureProfile`).
    pub enforce_secure: bool,
    /// Mojang's keys for profile keys.
    pub keys: Arc<ServicesKeys>,
    /// Ticks of `chat-spam-threshold-seconds` (Vanilla kicks non-ops above it).
    pub chat_spam_threshold: i32,
    /// Ticks of `command-spam-threshold-seconds`.
    pub command_spam_threshold: i32,
    /// Messages sent to a player that it has not acknowledged before it is kicked
    /// (Vanilla: 4096).
    pub max_pending_messages: usize,
}

impl Default for ChatSettings {
    fn default() -> Self {
        ChatSettings {
            enforce_secure: false,
            keys: Arc::new(ServicesKeys::default()),
            chat_spam_threshold: 200,
            command_spam_threshold: 200,
            max_pending_messages: 4096,
        }
    }
}

/// A chat session with a validated profile key (`RemoteChatSession`).
#[derive(Clone)]
pub struct ChatSession {
    /// The data the client sent, forwarded to other players in the tab list.
    pub data: ChatSessionData,
    key: RsaPublicKey,
}

impl std::fmt::Debug for ChatSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatSession")
            .field("session_id", &self.data.session_id)
            .field("expires_at", &self.data.expires_at)
            .finish_non_exhaustive()
    }
}

impl ChatSession {
    /// `RemoteChatSession.Data.validate`: checks Mojang's signature over the profile id, the
    /// expiry and the key. The error kicks the player.
    pub fn validate(
        data: ChatSessionData,
        profile_id: u128,
        keys: &ServicesKeys,
    ) -> Result<ChatSession, Kick> {
        let invalid = || {
            Kick(Box::new(Component::translatable(
                "multiplayer.disconnect.invalid_public_key_signature",
                vec![],
            )))
        };
        let mut payload = Vec::with_capacity(data.public_key.len().saturating_add(24));
        payload.extend_from_slice(&profile_id.to_be_bytes());
        payload.extend_from_slice(&data.expires_at.to_be_bytes());
        payload.extend_from_slice(&data.public_key);
        if !keys.validate(&payload, &data.key_signature) {
            return Err(invalid());
        }
        let key = RsaPublicKey::from_public_key_der(&data.public_key).map_err(|_| invalid())?;
        Ok(ChatSession { data, key })
    }

    /// Whether the profile key has expired at `now` (epoch milliseconds).
    pub fn has_expired(&self, now: i64) -> bool {
        self.data.expires_at < now
    }

    /// SHA256withRSA by the player's key.
    fn verify(&self, payload: &[u8], signature: &MessageSignature) -> bool {
        let digest = Sha256::digest(payload);
        self.key
            .verify(Pkcs1v15Sign::new::<Sha256>(), &digest, &signature.0[..])
            .is_ok()
    }
}

/// The signed part of a message (`SignedMessageBody`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageBody {
    /// The text.
    pub content: String,
    /// Epoch milliseconds.
    pub timestamp: i64,
    /// Salt.
    pub salt: i64,
    /// Signatures of the messages the sender had seen.
    pub last_seen: Vec<MessageSignature>,
}

impl MessageBody {
    /// `SignedMessageBody.unsigned`: now, salt 0, nothing seen.
    pub fn unsigned(content: String) -> MessageBody {
        MessageBody {
            content,
            timestamp: now_millis(),
            salt: 0,
            last_seen: Vec::new(),
        }
    }
}

/// A chat message (`PlayerChatMessage`): link, signature, body and the unsigned decorated
/// content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    /// Sender UUID; 0 (nil) for a system message.
    pub sender: u128,
    /// Chat session of the sender; 0 for an unsigned message.
    pub session_id: u128,
    /// Index in the sender's chain.
    pub index: i32,
    /// Signature, if the message is signed.
    pub signature: Option<MessageSignature>,
    /// The signed body.
    pub body: MessageBody,
    /// What is shown instead of the signed content (the chat decorator's output).
    pub unsigned_content: Option<Component>,
}

impl ChatMessage {
    /// `PlayerChatMessage.system`: a message without sender.
    pub fn system(content: String) -> ChatMessage {
        ChatMessage {
            sender: 0,
            session_id: 0,
            index: 0,
            signature: None,
            body: MessageBody::unsigned(content),
            unsigned_content: None,
        }
    }

    /// `PlayerChatMessage.unsigned`.
    pub fn unsigned(sender: u128, content: String) -> ChatMessage {
        ChatMessage {
            sender,
            ..ChatMessage::system(content)
        }
    }

    /// A system message is shown with `disguised_chat`.
    pub fn is_system(&self) -> bool {
        self.sender == 0
    }

    /// `decoratedContent`: the unsigned content or the signed text as literal (SEC-09).
    pub fn decorated_content(&self) -> Component {
        self.unsigned_content
            .clone()
            .unwrap_or_else(|| Component::text(self.body.content.as_str()))
    }

    /// `PlayerList.verifyChatTrusted`: signed and not older than 5 minutes.
    pub fn is_trusted(&self, now: i64) -> bool {
        self.signature.is_some()
            && now
                <= self
                    .body
                    .timestamp
                    .saturating_add(MESSAGE_EXPIRES_AFTER_SERVER_MS)
    }

    /// `PlayerChatMessage.updateSignature`: the bytes the sender signed.
    fn signed_payload(&self) -> Vec<u8> {
        let content = self.body.content.as_bytes();
        let mut out = Vec::with_capacity(
            content
                .len()
                .saturating_add(self.body.last_seen.len().saturating_mul(256))
                .saturating_add(64),
        );
        out.extend_from_slice(&1i32.to_be_bytes());
        out.extend_from_slice(&self.sender.to_be_bytes());
        out.extend_from_slice(&self.session_id.to_be_bytes());
        out.extend_from_slice(&self.index.to_be_bytes());
        out.extend_from_slice(&self.body.salt.to_be_bytes());
        out.extend_from_slice(&self.body.timestamp.div_euclid(1000).to_be_bytes());
        let len = i32::try_from(content.len()).unwrap_or(i32::MAX);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(content);
        let seen = i32::try_from(self.body.last_seen.len()).unwrap_or(i32::MAX);
        out.extend_from_slice(&seen.to_be_bytes());
        for signature in &self.body.last_seen {
            out.extend_from_slice(&signature.0[..]);
        }
        out
    }
}

/// Why a signed message was refused (`SignedMessageChain.DecodeException`); shown to the
/// sender in red.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainError {
    /// No signature although secure chat is enforced or a session exists.
    MissingProfileKey,
    /// The chain broke earlier.
    ChainBroken,
    /// The profile key expired.
    ExpiredProfileKey,
    /// The signature does not match.
    InvalidSignature,
    /// The timestamp went backwards.
    OutOfOrderChat,
    /// The signed arguments of a command do not match the command (`chat.disabled.invalid_command_signature`).
    InvalidCommandSignature,
}

impl ChainError {
    /// The translation key.
    pub fn key(self) -> &'static str {
        match self {
            Self::MissingProfileKey => "chat.disabled.missingProfileKey",
            Self::ChainBroken => "chat.disabled.chain_broken",
            Self::ExpiredProfileKey => "chat.disabled.expiredProfileKey",
            Self::InvalidSignature => "chat.disabled.invalid_signature",
            Self::OutOfOrderChat => "chat.disabled.out_of_order_chat",
            Self::InvalidCommandSignature => "chat.disabled.invalid_command_signature",
        }
    }
}

/// The sender side of a player's chat: its session and message chain
/// (`SignedMessageChain.Decoder`).
#[derive(Debug, Default)]
pub struct MessageChain {
    session: Option<ChatSession>,
    /// Index of the next message; `None` once the chain broke.
    next_index: Option<i32>,
    last_timestamp: i64,
}

impl MessageChain {
    /// The validated session, if any.
    pub fn session(&self) -> Option<&ChatSession> {
        self.session.as_ref()
    }

    /// `resetPlayerChatState`: a new session starts a new chain.
    pub fn reset(&mut self, session: ChatSession) {
        self.session = Some(session);
        self.next_index = Some(0);
        self.last_timestamp = 0;
    }

    /// `setChainBroken` (only a keyed chain can break).
    pub fn set_broken(&mut self) {
        if self.session.is_some() {
            self.next_index = None;
        }
    }

    /// `Decoder.unpack`: checks a message of `sender` and advances the chain.
    pub fn unpack(
        &mut self,
        sender: u128,
        signature: Option<MessageSignature>,
        body: MessageBody,
        enforce_secure: bool,
    ) -> Result<ChatMessage, ChainError> {
        let Some(session) = &self.session else {
            if enforce_secure {
                return Err(ChainError::MissingProfileKey);
            }
            return Ok(ChatMessage::unsigned(sender, body.content));
        };
        let Some(signature) = signature else {
            return Err(ChainError::MissingProfileKey);
        };
        let now = now_millis();
        if session.has_expired(now) {
            return Err(ChainError::ExpiredProfileKey);
        }
        let Some(index) = self.next_index else {
            return Err(ChainError::ChainBroken);
        };
        if body.timestamp < self.last_timestamp {
            self.next_index = None;
            return Err(ChainError::OutOfOrderChat);
        }
        self.last_timestamp = body.timestamp;
        let message = ChatMessage {
            sender,
            session_id: session.data.session_id.0,
            index,
            signature: Some(signature),
            body,
            unsigned_content: None,
        };
        let verified = message
            .signature
            .as_ref()
            .is_some_and(|s| session.verify(&message.signed_payload(), s));
        if !verified {
            self.next_index = None;
            return Err(ChainError::InvalidSignature);
        }
        if !message.is_trusted(now) {
            log::warn!(
                "Received expired chat: '{}'. Is the client/server system time unsynchronized?",
                message.body.content
            );
        }
        self.next_index = index.checked_add(1);
        Ok(message)
    }
}

/// One tracked message of the last-seen window.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Tracked {
    signature: MessageSignature,
    pending: bool,
}

/// `LastSeenMessagesValidator`: the signed messages sent to a player that its client may
/// acknowledge.
#[derive(Debug)]
pub struct LastSeenValidator {
    tracked: VecDeque<Option<Tracked>>,
    last_pending: Option<MessageSignature>,
}

impl Default for LastSeenValidator {
    fn default() -> Self {
        LastSeenValidator {
            tracked: std::iter::repeat_n(None, LAST_SEEN_COUNT).collect(),
            last_pending: None,
        }
    }
}

impl LastSeenValidator {
    /// `addPending`: a signed message was sent to the player.
    pub fn add_pending(&mut self, signature: &MessageSignature) {
        if self.last_pending.as_ref() != Some(signature) {
            self.tracked.push_back(Some(Tracked {
                signature: signature.clone(),
                pending: true,
            }));
            self.last_pending = Some(signature.clone());
        }
    }

    /// Messages in the window and waiting to enter it.
    pub fn tracked_count(&self) -> usize {
        self.tracked.len()
    }

    /// `applyOffset`: the client moved its window by `offset` messages.
    pub fn apply_offset(&mut self, offset: i32) -> Result<(), String> {
        let max = self.tracked.len().saturating_sub(LAST_SEEN_COUNT);
        match usize::try_from(offset) {
            Ok(n) if n <= max => {
                self.tracked.drain(..n);
                Ok(())
            }
            _ => Err(format!(
                "Advanced last seen window by {offset} messages, but expected at most {max}"
            )),
        }
    }

    /// `applyUpdate`: applies the offset and the acknowledged bits and returns the signatures
    /// the client has seen, in window order.
    pub fn apply_update(
        &mut self,
        update: &LastSeenUpdate,
    ) -> Result<Vec<MessageSignature>, String> {
        self.apply_offset(update.offset)?;
        if update.length() > LAST_SEEN_COUNT {
            return Err(format!(
                "Last seen update contained {} messages, but maximum window size is {LAST_SEEN_COUNT}",
                update.length()
            ));
        }
        let mut seen = Vec::new();
        for (i, slot) in self.tracked.iter_mut().take(LAST_SEEN_COUNT).enumerate() {
            if update.is_acknowledged(i) {
                let Some(entry) = slot.as_mut() else {
                    return Err(format!(
                        "Last seen update acknowledged unknown or previously ignored message at index {i}"
                    ));
                };
                entry.pending = false;
                seen.push(entry.signature.clone());
            } else {
                if slot.as_ref().is_some_and(|e| !e.pending) {
                    return Err(format!(
                        "Last seen update ignored previously acknowledged message at index {i}"
                    ));
                }
                *slot = None;
            }
        }
        if update.checksum != 0 && update.checksum != checksum(&seen) {
            return Err(
                "Checksum mismatch on last seen update: the client and server must have desynced"
                    .to_owned(),
            );
        }
        Ok(seen)
    }
}

/// `LastSeenMessages.computeChecksum`.
pub fn checksum(signatures: &[MessageSignature]) -> i8 {
    let sum = signatures
        .iter()
        .fold(1i32, |h, s| h.wrapping_mul(31).wrapping_add(s.checksum()));
    let byte = i8::from_ne_bytes([sum.to_le_bytes().first().copied().unwrap_or(0)]);
    if byte == 0 { 1 } else { byte }
}

/// `MessageSignatureCache`: signatures the client knows, so they can be sent as ids.
#[derive(Debug)]
pub struct SignatureCache {
    entries: Vec<Option<MessageSignature>>,
}

impl Default for SignatureCache {
    fn default() -> Self {
        SignatureCache {
            entries: vec![None; SIGNATURE_CACHE_SIZE],
        }
    }
}

impl SignatureCache {
    /// `MessageSignature.pack`: the cache id or the full signature.
    pub fn pack(&self, signature: &MessageSignature) -> PackedSignature {
        match self
            .entries
            .iter()
            .position(|e| e.as_ref() == Some(signature))
            .and_then(|i| i32::try_from(i).ok())
        {
            Some(id) => PackedSignature::Id(id),
            None => PackedSignature::Full(signature.clone()),
        }
    }

    /// `push(body, signature)`: the client pushes the same entries after receiving a message.
    pub fn push(&mut self, last_seen: &[MessageSignature], signature: &MessageSignature) {
        let mut queue: VecDeque<MessageSignature> = last_seen.iter().cloned().collect();
        queue.push_back(signature.clone());
        let new: Vec<MessageSignature> = queue.iter().cloned().collect();
        for slot in self.entries.iter_mut() {
            let Some(next) = queue.pop_back() else {
                break;
            };
            if let Some(old) = slot.replace(next)
                && !new.contains(&old)
            {
                queue.push_front(old);
            }
        }
    }
}

/// The receiver side of a player's chat, shared by all threads that send chat to it.
#[derive(Debug)]
pub struct ChatInbox {
    /// Chat visibility from the client settings.
    pub visibility: ChatVisibility,
    /// Messages the client may acknowledge.
    pub last_seen: LastSeenValidator,
    cache: SignatureCache,
    next_index: i32,
}

impl Default for ChatInbox {
    fn default() -> Self {
        ChatInbox {
            visibility: ChatVisibility::Full,
            last_seen: LastSeenValidator::default(),
            cache: SignatureCache::default(),
            next_index: 0,
        }
    }
}

impl ChatInbox {
    /// The inbox of a joining player.
    pub fn new(visibility: ChatVisibility) -> ChatInbox {
        ChatInbox {
            visibility,
            ..ChatInbox::default()
        }
    }

    /// `acceptsChatMessages`.
    pub fn accepts_chat(&self) -> bool {
        self.visibility == ChatVisibility::Full
    }

    /// `acceptsSystemMessages`.
    pub fn accepts_system(&self, overlay: bool) -> bool {
        self.visibility != ChatVisibility::Hidden || overlay
    }

    /// `OutgoingChatMessage.sendToPlayer`: a system message as `disguised_chat`, a player's
    /// message as `player_chat`. Returns `false` when the player has too many unacknowledged
    /// messages and must be kicked (`multiplayer.disconnect.too_many_pending_chats`).
    pub fn send(
        &mut self,
        message: &ChatMessage,
        chat_type: &ChatTypeBound,
        sink: &dyn PacketSink,
        max_pending: usize,
    ) -> bool {
        if !self.accepts_chat() {
            return true;
        }
        if message.is_system() {
            sink.send(&DisguisedChat {
                message: message.decorated_content(),
                chat_type: chat_type.clone(),
            });
            return true;
        }
        let global_index = self.next_index;
        self.next_index = self.next_index.wrapping_add(1);
        sink.send(&PlayerChat {
            global_index,
            sender: Uuid(message.sender),
            index: message.index,
            signature: message.signature.clone(),
            content: message.body.content.clone(),
            timestamp: message.body.timestamp,
            salt: message.body.salt,
            last_seen: message
                .body
                .last_seen
                .iter()
                .map(|s| self.cache.pack(s))
                .collect(),
            unsigned_content: message.unsigned_content.clone(),
            filter_mask: FilterMask::PassThrough,
            chat_type: chat_type.clone(),
        });
        if let Some(signature) = &message.signature {
            self.cache.push(&message.body.last_seen, signature);
            self.last_seen.add_pending(signature);
            if self.last_seen.tracked_count() > max_pending {
                return false;
            }
        }
        true
    }
}

/// `TickThrottler`: counts up by `step` per action and down by one per tick.
#[derive(Debug, Clone, Copy)]
pub struct TickThrottler {
    step: i32,
    threshold: i32,
    count: i32,
}

impl TickThrottler {
    /// A throttler that trips at `threshold` (0 or less never trips).
    pub fn new(step: i32, threshold: i32) -> TickThrottler {
        TickThrottler {
            step,
            threshold,
            count: 0,
        }
    }

    /// One action.
    pub fn increment(&mut self) {
        self.count = self.count.saturating_add(self.step);
    }

    /// One tick.
    pub fn tick(&mut self) {
        if self.count > 0 {
            self.count = self.count.saturating_sub(1);
        }
    }

    /// Whether the count is below the threshold.
    pub fn is_under_threshold(&self) -> bool {
        self.threshold <= 0 || self.count < self.threshold
    }
}

/// `StringUtil.isAllowedChatCharacter` over UTF-16 units: no `§`, no control characters, no
/// DEL.
pub fn is_chat_message_illegal(message: &str) -> bool {
    message
        .encode_utf16()
        .any(|c| c == 167 || c < 32 || c == 127)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn sig(b: u8) -> MessageSignature {
        MessageSignature(Box::new([b; 256]))
    }

    #[test]
    fn last_seen_window_acknowledges_and_checks() {
        let mut v = LastSeenValidator::default();
        v.add_pending(&sig(1));
        v.add_pending(&sig(2));
        // The client moves its window by 2 and acknowledges both (bits 18 and 19).
        let mut update = LastSeenUpdate {
            offset: 2,
            acknowledged: [0, 0, 0b1100],
            checksum: 0,
        };
        update.checksum = checksum(&[sig(1), sig(2)]);
        assert_eq!(v.apply_update(&update).unwrap(), vec![sig(1), sig(2)]);
        // Ignoring an acknowledged message is an error.
        let update = LastSeenUpdate {
            offset: 0,
            acknowledged: [0, 0, 0b0100],
            checksum: 0,
        };
        assert!(v.apply_update(&update).is_err());
    }

    #[test]
    fn last_seen_rejects_unknown_and_too_far() {
        let mut v = LastSeenValidator::default();
        assert!(v.apply_offset(1).is_err());
        assert!(v.apply_offset(-1).is_err());
        let update = LastSeenUpdate {
            offset: 0,
            acknowledged: [1, 0, 0],
            checksum: 0,
        };
        assert!(v.apply_update(&update).is_err());
        let update = LastSeenUpdate {
            offset: 0,
            acknowledged: [0, 0, 0x10],
            checksum: 0,
        };
        assert!(v.apply_update(&update).is_err());
    }

    #[test]
    fn signature_cache_packs_known_signatures() {
        let mut cache = SignatureCache::default();
        assert_eq!(cache.pack(&sig(1)), PackedSignature::Full(sig(1)));
        cache.push(&[sig(1)], &sig(2));
        assert_eq!(cache.pack(&sig(2)), PackedSignature::Id(0));
        assert_eq!(cache.pack(&sig(1)), PackedSignature::Id(1));
        cache.push(&[sig(2)], &sig(3));
        assert_eq!(cache.pack(&sig(3)), PackedSignature::Id(0));
        assert_eq!(cache.pack(&sig(2)), PackedSignature::Id(1));
        assert_eq!(cache.pack(&sig(1)), PackedSignature::Id(2));
    }

    #[test]
    fn throttler_trips_and_recovers() {
        let mut t = TickThrottler::new(20, 200);
        for _ in 0..9 {
            t.increment();
        }
        assert!(t.is_under_threshold());
        t.increment();
        assert!(!t.is_under_threshold());
        t.tick();
        assert!(t.is_under_threshold());
    }

    #[test]
    fn unsigned_chain_follows_enforcement() {
        let mut chain = MessageChain::default();
        let body = MessageBody::unsigned("hi".to_owned());
        assert_eq!(
            chain.unpack(7, None, body.clone(), true),
            Err(ChainError::MissingProfileKey)
        );
        let message = chain.unpack(7, None, body, false).unwrap();
        assert!(!message.is_system());
        assert_eq!(message.session_id, 0);
    }

    /// A real chain: Mojang signs the profile key, the player signs messages.
    #[test]
    fn signed_chain_verifies_and_breaks() {
        use rsa::pkcs8::EncodePublicKey;
        use rsa::{RsaPrivateKey, rand_core::OsRng};
        let mojang = RsaPrivateKey::new(&mut OsRng, 1024).unwrap();
        let player = RsaPrivateKey::new(&mut OsRng, 2048).unwrap();
        let mojang_der = mojang.to_public_key().to_public_key_der().unwrap();
        let keys = ServicesKeys::from_der([mojang_der.as_bytes()]);
        let profile: u128 = 0x1234;
        let public_key = player
            .to_public_key()
            .to_public_key_der()
            .unwrap()
            .into_vec();
        let expires_at = now_millis() + 3_600_000;
        let mut payload = profile.to_be_bytes().to_vec();
        payload.extend_from_slice(&expires_at.to_be_bytes());
        payload.extend_from_slice(&public_key);
        let key_signature = mojang
            .sign(Pkcs1v15Sign::new::<Sha1>(), &Sha1::digest(&payload))
            .unwrap();
        let data = ChatSessionData {
            session_id: Uuid(99),
            expires_at,
            public_key,
            key_signature,
        };
        assert!(ChatSession::validate(data.clone(), profile + 1, &keys).is_err());
        let session = ChatSession::validate(data, profile, &keys).unwrap();

        let mut chain = MessageChain::default();
        chain.reset(session);
        let sign = |index: i32, body: &MessageBody| {
            let message = ChatMessage {
                sender: profile,
                session_id: 99,
                index,
                signature: None,
                body: body.clone(),
                unsigned_content: None,
            };
            let digest = Sha256::digest(message.signed_payload());
            let bytes = player.sign(Pkcs1v15Sign::new::<Sha256>(), &digest).unwrap();
            MessageSignature(Box::new(bytes.try_into().unwrap()))
        };
        let first = MessageBody {
            content: "hello".to_owned(),
            timestamp: now_millis(),
            salt: 42,
            last_seen: vec![sig(7)],
        };
        let signature = sign(0, &first);
        let message = chain
            .unpack(profile, Some(signature), first.clone(), true)
            .unwrap();
        assert_eq!(message.index, 0);
        assert!(message.is_trusted(now_millis()));
        // The next message must carry index 1; a signature for index 0 breaks the chain.
        let second = MessageBody {
            content: "again".to_owned(),
            ..first
        };
        let wrong = sign(0, &second);
        assert_eq!(
            chain.unpack(profile, Some(wrong), second.clone(), true),
            Err(ChainError::InvalidSignature)
        );
        let right = sign(1, &second);
        assert_eq!(
            chain.unpack(profile, Some(right), second, true),
            Err(ChainError::ChainBroken)
        );
    }

    #[test]
    fn illegal_characters() {
        assert!(is_chat_message_illegal("a\u{a7}b"));
        assert!(is_chat_message_illegal("a\nb"));
        assert!(!is_chat_message_illegal("hello wörld"));
    }
}
