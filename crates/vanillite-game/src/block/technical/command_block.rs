//! `CommandBlock`, `CommandBlockEntity` and `BaseCommandBlock`: impulse, chain and repeating
//! command blocks.

use vanillite_base::BlockPos;
use vanillite_data::block::BlockId;
use vanillite_nbt::Compound;
use vanillite_protocol::packets::play::serverbound::{SetCommandBlock, SetCommandMinecart};

use super::Editor;
use crate::block::BlockBehaviour;
use crate::level::Level;

/// `CommandBlockEntity`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CommandBlock {}

impl CommandBlock {
    /// `CommandBlock.newBlockEntity`: chain command blocks start automatic.
    pub fn new(_block: BlockId) -> Self {
        Self::default()
    }

    /// `loadAdditional`.
    pub fn load(&mut self, _nbt: &Compound) {}

    /// `saveAdditional`.
    pub fn save(&self, _nbt: &mut Compound) {}
}

/// The behaviour of the block.
pub(super) fn fill(_block: BlockId, _b: &mut BlockBehaviour) {}

/// `ServerPlayer.openCommandBlock`.
pub(super) fn open(_level: &mut Level, _pos: BlockPos, _editor: &Editor<'_>) {}

/// `handleSetCommandBlock`.
pub fn handle(_level: &mut Level, _packet: SetCommandBlock, _editor: &Editor<'_>) {}

/// `handleSetCommandMinecart`.
pub fn handle_minecart(_level: &mut Level, _packet: SetCommandMinecart, _editor: &Editor<'_>) {}
