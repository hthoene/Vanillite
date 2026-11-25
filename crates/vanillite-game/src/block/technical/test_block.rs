//! `TestBlock`: the start, log, fail and accept blocks of block based game tests.

use vanillite_data::block::BlockId;
use vanillite_nbt::Compound;
use vanillite_protocol::packets::play::serverbound::SetTestBlock;

use super::Editor;
use crate::block::BlockBehaviour;
use crate::level::Level;

/// `TestBlockEntity`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TestBlock {}

impl TestBlock {
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

/// `handle...` of `set_test_block`.
pub fn handle(_level: &mut Level, _packet: SetTestBlock, _editor: &Editor<'_>) {}
