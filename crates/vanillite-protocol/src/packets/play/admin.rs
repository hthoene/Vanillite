//! Play packets of the operator screens: abilities, difficulty, game mode switcher, game rule
//! screen, F3+I tag queries and the spectator teleport.

use crate::Packet;
use crate::codec::{Decode, DecodeLimited, Encode, Reader, Result, write_len, write_varint};
use crate::types::{BlockPos, Identifier, Nbt, Uuid};

/// The maximum number of entries of `set_game_rule` and `game_rule_values` a peer may send.
/// Vanilla has 59 rules; the bound only stops oversized lists before they are allocated.
pub const MAX_GAME_RULE_ENTRIES: usize = 1024;

/// `player_abilities` (serverbound): the client started or stopped flying.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Serverbound, name = "player_abilities", bucket = "movement")]
pub struct PlayerAbilitiesServerbound {
    /// Bit 2 is `flying`; the other bits are ignored.
    pub flags: u8,
}

impl PlayerAbilitiesServerbound {
    /// `isFlying`.
    pub fn is_flying(&self) -> bool {
        self.flags & 2 != 0
    }
}

/// `change_difficulty` (serverbound): the difficulty button of the options screen.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Serverbound, name = "change_difficulty")]
pub struct ChangeDifficultyServerbound {
    /// The raw `Difficulty` id; ids out of range wrap (`OutOfBoundsStrategy.WRAP`).
    #[field(varint)]
    pub difficulty: i32,
}

/// `lock_difficulty`: the lock button of the options screen.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Serverbound, name = "lock_difficulty")]
pub struct LockDifficulty {
    pub locked: bool,
}

/// `change_game_mode`: the F3+F4 game mode switcher.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Serverbound, name = "change_game_mode")]
pub struct ChangeGameMode {
    /// The raw `GameType` id; ids out of range are survival (`OutOfBoundsStrategy.ZERO`).
    #[field(varint)]
    pub mode: i32,
}

/// One entry of `set_game_rule`: the rule's registry key and its serialized value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameRuleEntry {
    pub rule: Identifier,
    pub value: String,
}

/// `set_game_rule`: the changed values of the in-game game rule screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetGameRule {
    pub entries: Vec<GameRuleEntry>,
}

impl Encode for SetGameRule {
    fn encode(&self, out: &mut Vec<u8>) {
        write_len(out, self.entries.len());
        for e in &self.entries {
            e.rule.encode(out);
            e.value.encode(out);
        }
    }
}

impl Decode for SetGameRule {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        let len = r.len_prefix("list", MAX_GAME_RULE_ENTRIES, 2)?;
        let mut entries = Vec::with_capacity(len);
        for _ in 0..len {
            let rule = Identifier::decode(r)?;
            let value = r.string(32767)?.to_owned();
            entries.push(GameRuleEntry { rule, value });
        }
        Ok(SetGameRule { entries })
    }
}

impl crate::Packet for SetGameRule {
    const ID: i32 = crate::ids::play::serverbound::SET_GAME_RULE;
    const NAME: &'static str = "set_game_rule";
    const STATE: crate::State = crate::State::Play;
    const DIRECTION: crate::Direction = crate::Direction::Serverbound;
    const COST: u16 = 1;
    const BUCKET: crate::Bucket = crate::Bucket::Command;
}

/// `block_entity_tag_query`: F3+I on a block.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Serverbound, name = "block_entity_tag_query", bucket = "command")]
pub struct BlockEntityTagQuery {
    #[field(varint)]
    pub transaction_id: i32,
    pub pos: BlockPos,
}

/// `entity_tag_query`: F3+I on an entity.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Serverbound, name = "entity_tag_query", bucket = "command")]
pub struct EntityTagQuery {
    #[field(varint)]
    pub transaction_id: i32,
    #[field(varint)]
    pub entity_id: i32,
}

/// `teleport_to_entity`: the spectator menu's teleport.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Serverbound, name = "teleport_to_entity", bucket = "movement")]
pub struct TeleportToEntity {
    pub uuid: Uuid,
}

/// `tag_query`: the answer to a tag query; `None` when the block has no block entity.
#[derive(Debug, Clone, PartialEq)]
pub struct TagQuery {
    pub transaction_id: i32,
    pub tag: Nbt,
}

impl Encode for TagQuery {
    fn encode(&self, out: &mut Vec<u8>) {
        write_varint(out, self.transaction_id);
        self.tag.encode(out);
    }
}

impl Decode for TagQuery {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        let transaction_id = r.varint()?;
        let tag = Nbt::decode_limited(r, 2_097_152)?;
        Ok(TagQuery {
            transaction_id,
            tag,
        })
    }
}

impl crate::Packet for TagQuery {
    const ID: i32 = crate::ids::play::clientbound::TAG_QUERY;
    const NAME: &'static str = "tag_query";
    const STATE: crate::State = crate::State::Play;
    const DIRECTION: crate::Direction = crate::Direction::Clientbound;
    const COST: u16 = 1;
    const BUCKET: crate::Bucket = crate::Bucket::Default;
}

/// `game_rule_values`: every rule's serialized value, for the game rule screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameRuleValues {
    pub values: Vec<GameRuleEntry>,
}

impl Encode for GameRuleValues {
    fn encode(&self, out: &mut Vec<u8>) {
        write_len(out, self.values.len());
        for e in &self.values {
            e.rule.encode(out);
            e.value.encode(out);
        }
    }
}

impl Decode for GameRuleValues {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        let set = SetGameRule::decode(r)?;
        Ok(GameRuleValues {
            values: set.entries,
        })
    }
}

impl crate::Packet for GameRuleValues {
    const ID: i32 = crate::ids::play::clientbound::GAME_RULE_VALUES;
    const NAME: &'static str = "game_rule_values";
    const STATE: crate::State = crate::State::Play;
    const DIRECTION: crate::Direction = crate::Direction::Clientbound;
    const COST: u16 = 1;
    const BUCKET: crate::Bucket = crate::Bucket::Default;
}
