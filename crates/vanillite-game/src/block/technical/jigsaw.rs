//! `JigsawBlock`: the connection points of jigsaw structures and `jigsaw_generate`.

use vanillite_data::block::BlockId;
use vanillite_nbt::Compound;
use vanillite_protocol::packets::play::serverbound::SetJigsawBlock;

use super::Editor;
use crate::block::BlockBehaviour;
use crate::level::Level;

/// `JigsawBlockEntity`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Jigsaw {}

impl Jigsaw {
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

/// `handle...` of `set_jigsaw_block`.
pub fn handle(_level: &mut Level, _packet: SetJigsawBlock, _editor: &Editor<'_>) {}

/// `handleJigsawGenerate`.
pub fn handle_generate(
    _level: &mut Level,
    _packet: vanillite_protocol::packets::play::serverbound::JigsawGenerate,
    _editor: &Editor<'_>,
) {
}
