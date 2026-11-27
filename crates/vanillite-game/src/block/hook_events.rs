//! Plugin events of block changes (`vanillite_abi::event::BLOCK_GROW`, `BLOCK_FORM`,
//! `BLOCK_SPREAD`, `BLOCK_FADE` and kin), Bukkit's `CraftEventFactory.handleBlock*Event`: a
//! hook site asks before it writes the new state, and a cancelled event leaves the block as
//! it is. Without a listening plugin every helper is one branch.

use vanillite_base::BlockPos;
use vanillite_data::block::BlockState;

use crate::hooks::Event;
use crate::level::Level;

/// A block state as text: `minecraft:oak_log[axis=y]` (the `key` of block change events).
pub fn state_text(state: BlockState) -> String {
    let mut text = state.block().name().to_owned();
    let mut properties = state.properties().peekable();
    if properties.peek().is_some() {
        text.push('[');
        for (i, (name, value)) in properties.enumerate() {
            if i > 0 {
                text.push(',');
            }
            text.push_str(name);
            text.push('=');
            text.push_str(value);
        }
        text.push(']');
    }
    text
}

/// Whether plugins let the block at `pos` become `new` (`kind` one of `BLOCK_GROW`,
/// `BLOCK_FORM`, `BLOCK_SPREAD`, `BLOCK_FADE`: `pos`, `key` the old state, `state` the new
/// one). `true` when no plugin listens.
#[inline]
pub fn allows(level: &Level, kind: u32, pos: BlockPos, new: BlockState) -> bool {
    match level.hooks_for(kind) {
        None => true,
        Some(hooks) => {
            let mut ev = Event {
                pos,
                key: state_text(level.block(pos)),
                state: Some(new),
                ..Event::new(kind)
            };
            hooks.event(crate::hooks::HookWorld::of(level), &mut ev);
            !ev.cancelled
        }
    }
}

/// `level.setBlock(pos, new, flags)` unless a plugin cancels the change event `kind` (see
/// [`allows`]); `false` when cancelled or nothing changed.
#[inline]
pub fn set_block(level: &mut Level, kind: u32, pos: BlockPos, new: BlockState, flags: u32) -> bool {
    allows(level, kind, pos, new) && level.set_block(pos, new, flags)
}

/// [`set_block`] with `Block.UPDATE_ALL` (`setBlockAndUpdate`).
#[inline]
pub fn set_block_and_update(level: &mut Level, kind: u32, pos: BlockPos, new: BlockState) -> bool {
    allows(level, kind, pos, new) && level.set_block_and_update(pos, new)
}

/// Whether plugins let fire start at `pos` (`BLOCK_IGNITE`, Bukkit's `BlockIgniteEvent`):
/// `cause` from `vanillite_abi::ignite_cause`, `entity` and `player` the cause when known (0
/// otherwise). `true` when no plugin listens.
#[inline]
pub fn may_ignite(level: &Level, pos: BlockPos, cause: u32, entity: u128, player: u128) -> bool {
    match level.hooks_for(vanillite_abi::event::BLOCK_IGNITE) {
        None => true,
        Some(hooks) => {
            let mut ev = Event {
                pos,
                cause,
                entity,
                player,
                state: Some(level.block(pos)),
                ..Event::new(vanillite_abi::event::BLOCK_IGNITE)
            };
            hooks.event(crate::hooks::HookWorld::of(level), &mut ev);
            !ev.cancelled
        }
    }
}
