//! Note blocks (`NoteBlock`): they play their note when they get a signal, when used and when
//! hit. The instrument follows the blocks above and below (`updateShape`, shared with world
//! generation).

use vanillite_base::BlockPos;
use vanillite_base::random::RandomSource;
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_world::entity::EntityId;
use vanillite_worldgen::behaviour::{PosExt, StateExt};

use super::{BlockBehaviour, Interaction, ItemInteraction, UseContext, sound_id};
use crate::item::ItemStack;
use crate::level::game_event::events;
use crate::level::{Level, SOUND_RECORDS};
use crate::stats::Stat;

/// Fills the note block behaviours.
pub(super) fn fill(block: BlockId, b: &mut BlockBehaviour) {
    if block == ids::NOTE_BLOCK {
        b.neighbor_changed = Some(neighbor_changed);
        b.trigger_event = Some(trigger_event);
        b.use_without_item = Some(use_without_item);
        b.use_item_on = Some(use_item_on);
        b.attack = Some(|level, state, pos, player| {
            play_note(level, Some(player), state, pos);
            level.award_stat(player, crate::stats::custom::PLAY_NOTEBLOCK, 1);
        });
    }
}

/// `NoteBlock.useItemOn`: a mob head held against the top is placed instead of tuning.
fn use_item_on(
    _: &mut Level,
    _: BlockState,
    _: BlockPos,
    context: &UseContext,
    stack: &mut ItemStack,
) -> ItemInteraction {
    let top_instrument = crate::player::play::game_data()
        .is_some_and(|d| d.item_in_tag(stack.item, "minecraft:noteblock_top_instruments"));
    if top_instrument && context.face == vanillite_base::Direction::Up {
        ItemInteraction::Pass
    } else {
        ItemInteraction::TryWithEmptyHand
    }
}

/// The instrument of a note block state.
fn instrument(state: BlockState) -> &'static str {
    state.value("instrument").unwrap_or("harp")
}

/// `NoteBlockInstrument.worksAboveNoteBlock()`: mob heads and custom heads.
fn works_above_note_block(instrument: &str) -> bool {
    matches!(
        instrument,
        "zombie" | "skeleton" | "creeper" | "dragon" | "wither_skeleton" | "piglin" | "custom_head"
    )
}

/// `NoteBlock.getPitchFromNote(note)`.
pub fn pitch_from_note(note: i32) -> f32 {
    2.0f64.powf(f64::from(note - 12) / 12.0) as f32
}

/// `NoteBlock.neighborChanged`: a rising signal plays the note.
fn neighbor_changed(level: &mut Level, state: BlockState, pos: BlockPos, _: BlockId, _: bool) {
    let signal = level.has_neighbor_signal(pos);
    if signal != state.flag("powered") {
        if signal {
            play_note(level, None, state, pos);
        }
        level.set_block_and_update(pos, state.set_flag("powered", signal));
    }
}

/// `NoteBlock.playNote`: base instruments only play with air above.
fn play_note(level: &mut Level, source: Option<EntityId>, state: BlockState, pos: BlockPos) {
    if works_above_note_block(instrument(state)) || level.block(pos.above()).is_air() {
        level.block_event(pos, ids::NOTE_BLOCK, 0, 0);
        level.game_event_at(source, events().note_block_play, pos);
    }
}

/// `NoteBlock.useWithoutItem`: the note goes up one step and plays.
fn use_without_item(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    context: &UseContext,
) -> Interaction {
    let note = state.value_index("note").unwrap_or(0);
    let state = state.with_index("note", (note + 1) % 25).unwrap_or(state);
    level.set_block_and_update(pos, state);
    play_note(level, Some(context.player), state, pos);
    level.award_stat(context.player, crate::stats::custom::TUNE_NOTEBLOCK, 1);
    Interaction::Success
}

/// `NoteBlock.triggerEvent`: the sound, with the note's pitch for tunable instruments.
fn trigger_event(level: &mut Level, state: BlockState, pos: BlockPos, _: i32, _: i32) -> bool {
    let instrument = instrument(state);
    let pitch = if works_above_note_block(instrument) {
        1.0
    } else {
        pitch_from_note(i32::from(state.value_index("note").unwrap_or(0)))
    };
    let sound = match instrument {
        // The sound of the player head above (`getCustomSoundId`); heads have no block entity
        // data here yet, so there is none.
        "custom_head" => return false,
        "zombie" => sound_id("block.note_block.imitate.zombie"),
        "skeleton" => sound_id("block.note_block.imitate.skeleton"),
        "creeper" => sound_id("block.note_block.imitate.creeper"),
        "dragon" => sound_id("block.note_block.imitate.ender_dragon"),
        "wither_skeleton" => sound_id("block.note_block.imitate.wither_skeleton"),
        "piglin" => sound_id("block.note_block.imitate.piglin"),
        base => {
            let mut name = [0u8; 48];
            let prefix = b"block.note_block.";
            let len = prefix.len() + base.len();
            name[..prefix.len()].copy_from_slice(prefix);
            name[prefix.len()..len].copy_from_slice(base.as_bytes());
            sound_id(std::str::from_utf8(&name[..len]).unwrap_or_default())
        }
    };
    let seed = level.random().next_long();
    level.play_seeded_sound(
        None,
        f64::from(pos.x) + 0.5,
        f64::from(pos.y) + 0.5,
        f64::from(pos.z) + 0.5,
        sound,
        SOUND_RECORDS,
        3.0,
        pitch,
        seed,
    );
    true
}
