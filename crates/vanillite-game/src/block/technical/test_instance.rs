//! `TestInstanceBlock`: a game test placed in the world.

use vanillite_data::block::BlockId;
use vanillite_nbt::Compound;
use vanillite_protocol::packets::play::serverbound::TestInstanceBlockAction;

use super::Editor;
use crate::block::BlockBehaviour;
use crate::level::Level;

/// `TestInstanceBlockEntity`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TestInstance {}

impl TestInstance {
    /// `loadAdditional`.
    pub fn load(&mut self, _nbt: &Compound) {}

    /// `saveAdditional`.
    pub fn save(&self, _nbt: &mut Compound) {}

    /// `getUpdateTag`.
    pub fn update_tag(&self) -> Option<Compound> {
        None
    }
}

/// The behaviour of the block.
pub(super) fn fill(_block: BlockId, _b: &mut BlockBehaviour) {}

/// `handle...` of `test_instance_block_action`.
pub fn handle(_level: &mut Level, _packet: TestInstanceBlockAction, _editor: &Editor<'_>) {}
