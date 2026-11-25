//! Jukeboxes (`JukeboxBlock`, `JukeboxBlockEntity`, `JukeboxSongPlayer`): a music disc goes in,
//! the song plays for its length (a redstone signal of 15 meanwhile, the song's comparator
//! output while the disc is in), using the block again pops the disc out.

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Direction, Vec3};
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_nbt::{Compound, Tag};
use vanillite_worldgen::behaviour::StateExt;

use super::{BlockBehaviour, Interaction, ItemInteraction, UseContext};
use crate::item::{self, ItemStack};
use crate::level::Level;
use crate::level::block_entity::BlockEntity;
use crate::level::game_event::{GameEventContext, events};
use crate::stats::Stat;

/// An entry of the `minecraft:jukebox_song` registry.
#[derive(Clone, Debug, PartialEq)]
pub struct Song {
    /// Its id, e.g. `minecraft:cat`.
    pub id: String,
    /// `length_in_seconds`.
    pub length_in_seconds: f32,
    /// `comparator_output`.
    pub comparator_output: i32,
}

impl Song {
    /// Parses a registry entry.
    pub fn parse(id: &str, json: &serde_json::Value) -> Song {
        Song {
            id: id.to_owned(),
            length_in_seconds: json["length_in_seconds"].as_f64().unwrap_or(0.0) as f32,
            comparator_output: json["comparator_output"]
                .as_i64()
                .and_then(|v| i32::try_from(v).ok())
                .unwrap_or(0),
        }
    }

    /// `hasFinished(ticks)`: `lengthInTicks() + 20` ticks after the start.
    fn has_finished(&self, ticks: i64) -> bool {
        let length = (self.length_in_seconds * 20.0).ceil() as i64;
        ticks >= length + 20
    }
}

/// `JukeboxBlockEntity` with its song player.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Jukebox {
    /// `RecordItem`.
    pub item: ItemStack,
    /// The playing song (its `minecraft:jukebox_song` protocol id).
    pub song: Option<u16>,
    /// `ticksSinceSongStarted`.
    pub ticks: i64,
}

/// The song of a stack's default `jukebox_playable` component: protocol id and entry.
fn song_of(stack: &ItemStack) -> Option<(u16, Song)> {
    if stack.is_empty() {
        return None;
    }
    let name = item::item_id(stack).info().jukebox_song;
    if name.is_empty() {
        return None;
    }
    let data = crate::player::play::game_data()?;
    let index = data.jukebox_songs.iter().position(|s| s.id == name)?;
    Some((
        u16::try_from(index).ok()?,
        data.jukebox_songs.get(index)?.clone(),
    ))
}

fn song_by_id(id: u16) -> Option<Song> {
    crate::player::play::game_data()?
        .jukebox_songs
        .get(usize::from(id))
        .cloned()
}

impl Jukebox {
    /// `loadAdditional`.
    pub fn load(&mut self, nbt: &Compound) {
        self.item = match nbt.get("RecordItem") {
            Some(Tag::Compound(c)) => crate::item_nbt::stack_from_nbt(c),
            _ => ItemStack::EMPTY,
        };
        if let Some(Tag::Long(ticks)) = nbt.get("ticks_since_song_started")
            && let Some((id, song)) = song_of(&self.item)
            && !song.has_finished(*ticks)
        {
            self.song = Some(id);
            self.ticks = *ticks;
        }
    }

    /// `saveAdditional`.
    pub fn save(&self, nbt: &mut Compound) {
        if !self.item.is_empty() {
            nbt.insert(
                "RecordItem",
                Tag::Compound(crate::item_nbt::stack_to_nbt(&self.item)),
            );
        }
        if self.song.is_some() {
            nbt.insert("ticks_since_song_started", Tag::Long(self.ticks));
        }
    }
}

/// Fills the jukebox behaviours.
pub(super) fn fill(block: BlockId, b: &mut BlockBehaviour) {
    if block == ids::JUKEBOX {
        b.use_item_on = Some(use_item_on);
        b.use_without_item = Some(use_without_item);
        b.signal = Some(|level, _, pos, _| if playing(level, pos) { 15 } else { 0 });
        b.analog_output = Some(analog_output);
        b.affect_neighbors_after_removal = Some(|level, state, pos, _| {
            level.update_neighbors_at(pos, state.block());
        });
    }
}

fn playing(level: &Level, pos: BlockPos) -> bool {
    matches!(level.block_entity(pos), Some(BlockEntity::Jukebox(j)) if j.song.is_some())
}

/// `getAnalogOutputSignal`: the comparator output of the disc's song.
fn analog_output(level: &Level, _: BlockState, pos: BlockPos, _: Direction) -> i32 {
    match level.block_entity(pos) {
        Some(BlockEntity::Jukebox(j)) => song_of(&j.item).map_or(0, |(_, s)| s.comparator_output),
        _ => 0,
    }
}

/// `JukeboxBlock.useItemOn` / `JukeboxPlayable.tryInsertIntoJukebox`.
fn use_item_on(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    ctx: &UseContext,
    stack: &mut ItemStack,
) -> ItemInteraction {
    if state.flag("has_record") || song_of(stack).is_none() {
        return ItemInteraction::TryWithEmptyHand;
    }
    // `consumeAndReturn(1, player)`: creative players keep theirs.
    let inserted = if ctx.infinite {
        stack.with_count(1)
    } else {
        stack.split(1)
    };
    if matches!(level.block_entity(pos), Some(BlockEntity::Jukebox(_))) {
        set_the_item(level, pos, inserted);
        level.game_event_block(
            events().block_change,
            pos,
            GameEventContext {
                source: Some(ctx.player),
                affected: Some(state),
            },
        );
    }
    level.award_stat(ctx.player, Stat::custom_named("minecraft:play_record"), 1);
    ItemInteraction::Done(Interaction::Success)
}

/// `JukeboxBlock.useWithoutItem`: the disc pops out.
fn use_without_item(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    _: &UseContext,
) -> Interaction {
    if state.flag("has_record") && matches!(level.block_entity(pos), Some(BlockEntity::Jukebox(_)))
    {
        pop_out_the_item(level, pos);
        Interaction::Success
    } else {
        Interaction::Pass
    }
}

/// `JukeboxBlockEntity.setTheItem`: the state follows, the song starts or stops.
pub fn set_the_item(level: &mut Level, pos: BlockPos, stack: ItemStack) {
    let song = song_of(&stack);
    let inserted = !stack.is_empty();
    if let Some(BlockEntity::Jukebox(j)) = level.block_entity_mut(pos) {
        j.item = stack;
    }
    // `notifyItemChangedInJukebox`.
    let state = level.block(pos);
    if state.is(ids::JUKEBOX) {
        level.set_block(
            pos,
            state.set_flag("has_record", inserted),
            crate::level::update::CLIENTS,
        );
        level.game_event_block(
            events().block_change,
            pos,
            GameEventContext {
                source: None,
                affected: Some(state),
            },
        );
    }
    match song {
        Some((id, _)) if inserted => {
            if let Some(BlockEntity::Jukebox(j)) = level.block_entity_mut(pos) {
                j.song = Some(id);
                j.ticks = 0;
            }
            level.level_event(None, 1010, pos, i32::from(id));
            song_changed(level, pos);
        }
        _ => stop(level, pos),
    }
}

/// `JukeboxBlockEntity.popOutTheItem`.
pub fn pop_out_the_item(level: &mut Level, pos: BlockPos) {
    let item = match level.block_entity(pos) {
        Some(BlockEntity::Jukebox(j)) => j.item.clone(),
        _ => return,
    };
    if item.is_empty() {
        return;
    }
    // `removeTheItem` = `splitTheItem` → `setTheItem(EMPTY)`.
    set_the_item(level, pos, ItemStack::EMPTY);
    let random = level.random();
    let dx = f64::from((random.next_float() - 0.5) * 0.7);
    let dz = f64::from((random.next_float() - 0.5) * 0.7);
    let at = Vec3::new(
        f64::from(pos.x) + 0.5 + dx,
        f64::from(pos.y) + 1.01,
        f64::from(pos.z) + 0.5 + dz,
    );
    super::drops::spawn_item_moving(level, at, item, None, 10, None);
    song_changed(level, pos);
}

/// `JukeboxSongPlayer.stop`.
fn stop(level: &mut Level, pos: BlockPos) {
    let was_playing = match level.block_entity_mut(pos) {
        Some(BlockEntity::Jukebox(j)) => {
            let was = j.song.is_some();
            j.song = None;
            j.ticks = 0;
            was
        }
        _ => false,
    };
    if was_playing {
        let state = level.block(pos);
        level.game_event_block(
            events().jukebox_stop_play,
            pos,
            GameEventContext {
                source: None,
                affected: Some(state),
            },
        );
        level.level_event(None, 1011, pos, 0);
        song_changed(level, pos);
    }
}

/// `JukeboxBlockEntity.onSongChanged`.
fn song_changed(level: &mut Level, pos: BlockPos) {
    let block = level.block(pos).block();
    level.update_neighbors_at(pos, block);
    level.block_entities.touch(pos.chunk());
}

/// `JukeboxBlockEntity.setRemoved`: the clients stop the song.
pub fn removed(level: &mut Level, pos: BlockPos, state: BlockState) {
    level.game_event_block(
        events().jukebox_stop_play,
        pos,
        GameEventContext {
            source: None,
            affected: Some(state),
        },
    );
    level.level_event(None, 1011, pos, 0);
}

/// `JukeboxBlockEntity.tick` (a ticker only while the state has a record).
pub fn tick(level: &mut Level, pos: BlockPos, state: BlockState) {
    if !state.flag("has_record") {
        return;
    }
    let Some(BlockEntity::Jukebox(j)) = level.block_entity(pos) else {
        return;
    };
    let Some(song) = j.song else {
        return;
    };
    let ticks = j.ticks;
    if song_by_id(song).is_none_or(|s| s.has_finished(ticks)) {
        stop(level, pos);
        return;
    }
    if ticks % 20 == 0 {
        level.game_event_block(
            events().jukebox_play,
            pos,
            GameEventContext {
                source: None,
                affected: Some(state),
            },
        );
        // `spawnMusicParticles`: the note colour (the particle itself is client side).
        let _color = level.random().next_int_bounded(4);
    }
    if let Some(BlockEntity::Jukebox(j)) = level.block_entity_mut(pos) {
        j.ticks += 1;
    }
}
