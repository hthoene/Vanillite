//! The technical blocks (`GameMasterBlock`): command blocks, structure blocks, jigsaws, test
//! blocks and test instance blocks. Only operators (permission level 2) in creative use them
//! (`Player.canUseGameMasterBlocks`): their edit screens open on the client, the edits come
//! back as the packets handled here.

pub mod command_block;
pub mod jigsaw;
pub mod structure_block;
pub mod templates;
pub mod test_block;
pub mod test_instance;

use vanillite_base::BlockPos;
use vanillite_data::block::{BlockId, BlockState};
use vanillite_protocol::packets::play::clientbound::SystemChat;
use vanillite_protocol::text::Component;
use vanillite_world::entity::EntityId;

use super::BlockBehaviour;
use crate::level::{BlockEntity, Level};
use crate::player::sink::PacketSink;

/// The player that edits a technical block.
pub struct Editor<'a> {
    /// The player's entity.
    pub player: Option<EntityId>,
    /// The player's UUID.
    pub uuid: u128,
    /// `Player.canUseGameMasterBlocks`: creative and permission level 2.
    pub allowed: bool,
    /// The player's permission level.
    pub permission_level: u8,
    /// The player's connection.
    pub sink: &'a dyn PacketSink,
}

impl std::fmt::Debug for Editor<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Editor")
            .field("player", &self.player)
            .field("allowed", &self.allowed)
            .finish_non_exhaustive()
    }
}

impl Editor<'_> {
    /// `sendSystemMessage`.
    pub fn message(&self, message: Component) {
        self.sink.send(&SystemChat {
            content: message,
            overlay: false,
        });
    }
}

/// `Player.canUseGameMasterBlocks`.
pub fn can_use_game_master_blocks(creative: bool, permission_level: u8) -> bool {
    creative && permission_level >= 2
}

/// The behaviours of the technical blocks.
pub(super) fn fill(block: BlockId, b: &mut BlockBehaviour) {
    command_block::fill(block, b);
    structure_block::fill(block, b);
    jigsaw::fill(block, b);
    test_block::fill(block, b);
    test_instance::fill(block, b);
}

/// `newBlockEntity` of the technical blocks.
pub(crate) fn new_block_entity(block: BlockId) -> Option<BlockEntity> {
    use vanillite_data::block::ids;
    Some(match block {
        b if b == ids::COMMAND_BLOCK
            || b == ids::CHAIN_COMMAND_BLOCK
            || b == ids::REPEATING_COMMAND_BLOCK =>
        {
            BlockEntity::CommandBlock(Box::new(command_block::CommandBlock::new(block)))
        }
        b if b == ids::STRUCTURE_BLOCK => BlockEntity::StructureBlock(Box::default()),
        b if b == ids::JIGSAW => BlockEntity::Jigsaw(Box::default()),
        b if b == ids::TEST_BLOCK => BlockEntity::TestBlock(Box::default()),
        b if b == ids::TEST_INSTANCE_BLOCK => BlockEntity::TestInstance(Box::default()),
        _ => return None,
    })
}

/// `useWithoutItem` of the game master blocks: an operator in creative opens the edit screen
/// (`SUCCESS`, `true`), anyone else passes (`false`).
pub fn use_without_item(level: &mut Level, pos: BlockPos, editor: &Editor<'_>) -> bool {
    if !editor.allowed {
        return false;
    }
    match level.block_entity(pos) {
        // `ServerPlayer.openCommandBlock`: the block entity's data goes to the player.
        Some(BlockEntity::CommandBlock(_)) => {
            command_block::open(level, pos, editor);
            true
        }
        // The other screens open on the client from the block entity it already has.
        Some(
            BlockEntity::StructureBlock(_)
            | BlockEntity::Jigsaw(_)
            | BlockEntity::TestBlock(_)
            | BlockEntity::TestInstance(_),
        ) => true,
        _ => false,
    }
}

/// Whether `state` is one of the game master blocks.
pub fn is_game_master_block(state: BlockState) -> bool {
    use vanillite_data::block::BlockKind;
    matches!(
        state.block().info().kind,
        BlockKind::CommandBlock
            | BlockKind::StructureBlock
            | BlockKind::JigsawBlock
            | BlockKind::TestBlock
            | BlockKind::TestInstanceBlock
    )
}
