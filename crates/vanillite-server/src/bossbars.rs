//! Command boss bars (`CustomBossEvents` with its `CustomBossEvent`s): created by `/bossbar`,
//! shown to players by UUID across all worlds, saved as the `custom_boss_events` saved data.

use std::collections::BTreeMap;
use std::sync::Arc;

use vanillite_nbt::{Compound, List, Tag};
use vanillite_protocol::SharedPacket;
use vanillite_protocol::packets::play::clientbound::{
    BossBarColor, BossBarFlags, BossBarOverlay, BossEvent, BossEventOperation,
};
use vanillite_protocol::text::{Color, Component, HoverEvent, NamedColor};
use vanillite_protocol::types::Uuid;

/// The saved data id (`CustomBossEvents.TYPE`).
pub(crate) const SAVED_DATA: &str = "custom_boss_events";

const COLORS: [(BossBarColor, &str, NamedColor); 7] = [
    (BossBarColor::Pink, "pink", NamedColor::LightPurple),
    (BossBarColor::Blue, "blue", NamedColor::Blue),
    (BossBarColor::Red, "red", NamedColor::Red),
    (BossBarColor::Green, "green", NamedColor::Green),
    (BossBarColor::Yellow, "yellow", NamedColor::Yellow),
    (BossBarColor::Purple, "purple", NamedColor::DarkPurple),
    (BossBarColor::White, "white", NamedColor::White),
];

const OVERLAYS: [(BossBarOverlay, &str); 5] = [
    (BossBarOverlay::Progress, "progress"),
    (BossBarOverlay::Notched6, "notched_6"),
    (BossBarOverlay::Notched10, "notched_10"),
    (BossBarOverlay::Notched12, "notched_12"),
    (BossBarOverlay::Notched20, "notched_20"),
];

/// The colour with the `BossBarColor` id (command order).
pub(crate) fn color_of(id: u8) -> BossBarColor {
    COLORS
        .get(usize::from(id))
        .map_or(BossBarColor::White, |c| c.0)
}

/// The overlay with the `BossBarOverlay` id (command order).
pub(crate) fn overlay_of(id: u8) -> BossBarOverlay {
    OVERLAYS
        .get(usize::from(id))
        .map_or(BossBarOverlay::Progress, |o| o.0)
}

/// Packets for players, by UUID.
pub(crate) type Outbox = Vec<(u128, Arc<SharedPacket>)>;

/// `CustomBossEvent`.
#[derive(Clone, Debug)]
pub(crate) struct CustomBossEvent {
    pub(crate) custom_id: String,
    uuid: u128,
    name: Component,
    progress: f32,
    color: BossBarColor,
    overlay: BossBarOverlay,
    flags: BossBarFlags,
    visible: bool,
    value: i32,
    max: i32,
    /// `CustomBossEvent.players`: the saved UUIDs.
    saved: Vec<u128>,
    /// `ServerBossEvent.players`: the online players that have the bar.
    shown: Vec<u128>,
}

impl CustomBossEvent {
    fn new(uuid: u128, custom_id: String, name: Component) -> CustomBossEvent {
        CustomBossEvent {
            custom_id,
            uuid,
            name,
            progress: 0.0,
            color: BossBarColor::White,
            overlay: BossBarOverlay::Progress,
            flags: BossBarFlags::default(),
            visible: true,
            value: 0,
            max: 100,
            saved: Vec::new(),
            shown: Vec::new(),
        }
    }

    pub(crate) fn name(&self) -> &Component {
        &self.name
    }
    pub(crate) fn color(&self) -> BossBarColor {
        self.color
    }
    pub(crate) fn overlay(&self) -> BossBarOverlay {
        self.overlay
    }
    pub(crate) fn visible(&self) -> bool {
        self.visible
    }
    pub(crate) fn value(&self) -> i32 {
        self.value
    }
    pub(crate) fn max(&self) -> i32 {
        self.max
    }
    /// `getPlayers`: the online players that have the bar.
    pub(crate) fn shown(&self) -> &[u128] {
        &self.shown
    }

    /// `getDisplayName`: the name in square brackets in the bar's colour, with the id as
    /// hover text and insertion.
    pub(crate) fn display_name(&self) -> Component {
        let color = COLORS
            .iter()
            .find(|c| c.0 == self.color)
            .map_or(NamedColor::White, |c| c.2);
        Component::translatable("chat.square_brackets", vec![self.name.clone()])
            .color(Color::Named(color))
            .hover(HoverEvent::ShowText(Component::text(
                self.custom_id.clone(),
            )))
            .insertion(self.custom_id.clone())
    }

    fn packet(&self, operation: BossEventOperation) -> Arc<SharedPacket> {
        SharedPacket::of(&BossEvent {
            id: Uuid(self.uuid),
            operation,
        })
    }

    fn add_packet(&self) -> Arc<SharedPacket> {
        self.packet(BossEventOperation::Add {
            name: self.name.clone(),
            progress: self.progress,
            color: self.color,
            overlay: self.overlay,
            flags: self.flags,
        })
    }

    /// `ServerBossEvent.broadcast`: to every player that has the bar, while it is visible.
    fn broadcast(&self, operation: BossEventOperation, out: &mut Outbox) {
        if self.visible && !self.shown.is_empty() {
            let packet = self.packet(operation);
            out.extend(self.shown.iter().map(|&p| (p, Arc::clone(&packet))));
        }
    }

    fn set_progress(&mut self, progress: f32, out: &mut Outbox) {
        if progress != self.progress {
            self.progress = progress;
            self.broadcast(BossEventOperation::UpdateProgress(progress), out);
        }
    }

    /// `ServerBossEvent.addPlayer` (online players only).
    fn show(&mut self, player: u128, out: &mut Outbox) {
        if !self.shown.contains(&player) {
            self.shown.push(player);
            if self.visible {
                out.push((player, self.add_packet()));
            }
        }
    }

    /// `ServerBossEvent.removePlayer`.
    fn hide(&mut self, player: u128, out: &mut Outbox) {
        if let Some(i) = self.shown.iter().position(|&p| p == player) {
            self.shown.remove(i);
            if self.visible {
                out.push((player, self.packet(BossEventOperation::Remove)));
            }
        }
    }

    pub(crate) fn set_name(&mut self, name: Component, out: &mut Outbox) {
        if name != self.name {
            self.name = name.clone();
            self.broadcast(BossEventOperation::UpdateName(name), out);
        }
    }

    pub(crate) fn set_color(&mut self, color: BossBarColor, out: &mut Outbox) {
        if color != self.color {
            self.color = color;
            self.broadcast(
                BossEventOperation::UpdateStyle {
                    color,
                    overlay: self.overlay,
                },
                out,
            );
        }
    }

    pub(crate) fn set_overlay(&mut self, overlay: BossBarOverlay, out: &mut Outbox) {
        if overlay != self.overlay {
            self.overlay = overlay;
            self.broadcast(
                BossEventOperation::UpdateStyle {
                    color: self.color,
                    overlay,
                },
                out,
            );
        }
    }

    /// `ServerBossEvent.setVisible`.
    pub(crate) fn set_visible(&mut self, visible: bool, out: &mut Outbox) {
        if visible != self.visible {
            self.visible = visible;
            for &p in &self.shown {
                let packet = if visible {
                    self.add_packet()
                } else {
                    self.packet(BossEventOperation::Remove)
                };
                out.push((p, packet));
            }
        }
    }

    /// `setValue`.
    pub(crate) fn set_value(&mut self, value: i32, out: &mut Outbox) {
        self.value = value;
        self.update_progress(out);
    }

    /// `setMax`.
    pub(crate) fn set_max(&mut self, max: i32, out: &mut Outbox) {
        self.max = max;
        self.update_progress(out);
    }

    fn update_progress(&mut self, out: &mut Outbox) {
        #[allow(clippy::cast_precision_loss)]
        let progress = (self.value as f32 / self.max as f32).clamp(0.0, 1.0);
        // `Mth.clamp` of NaN (0 / 0) is NaN in Java too; keep the bar's value then.
        let progress = if progress.is_nan() { 0.0 } else { progress };
        self.set_progress(progress, out);
    }

    /// `setPlayers`: `targets` are the online players; whether the saved players changed.
    pub(crate) fn set_players(&mut self, targets: &[u128], out: &mut Outbox) -> bool {
        let remove: Vec<u128> = self
            .saved
            .iter()
            .copied()
            .filter(|u| !targets.contains(u))
            .collect();
        let add: Vec<u128> = targets
            .iter()
            .copied()
            .filter(|u| !self.saved.contains(u))
            .collect();
        for u in &remove {
            self.hide(*u, out);
            self.saved.retain(|s| s != u);
        }
        for u in &add {
            self.show(*u, out);
            self.saved.push(*u);
        }
        !remove.is_empty() || !add.is_empty()
    }

    /// Shows the bar to an online player or hides it from a player (`addPlayer`,
    /// `removePlayer`); whether that changed anything.
    pub(crate) fn set_player(&mut self, player: u128, show: bool, out: &mut Outbox) -> bool {
        let mut targets = self.saved.clone();
        if show && !targets.contains(&player) {
            targets.push(player);
        } else if !show {
            targets.retain(|u| *u != player);
        }
        self.set_players(&targets, out)
    }

    /// `removeAllPlayers`.
    fn remove_all(&mut self, out: &mut Outbox) {
        for p in std::mem::take(&mut self.shown) {
            if self.visible {
                out.push((p, self.packet(BossEventOperation::Remove)));
            }
        }
        self.saved.clear();
    }

    /// `Packed.CODEC`.
    fn pack(&self) -> Compound {
        let mut c = Compound::new();
        c.insert("Name", self.name.to_nbt());
        c.insert("Visible", self.visible);
        c.insert("Value", self.value);
        c.insert("Max", self.max);
        let color = COLORS
            .iter()
            .find(|x| x.0 == self.color)
            .map_or("white", |x| x.1);
        c.insert("Color", color);
        let overlay = OVERLAYS
            .iter()
            .find(|x| x.0 == self.overlay)
            .map_or("progress", |x| x.1);
        c.insert("Overlay", overlay);
        c.insert("DarkenScreen", self.flags.darken_screen);
        c.insert("PlayBossMusic", self.flags.play_music);
        c.insert("CreateWorldFog", self.flags.create_fog);
        c.insert(
            "Players",
            Tag::List(List(self.saved.iter().map(|&u| uuid_tag(u)).collect())),
        );
        c
    }

    /// `load(randomUUID, id, packed)`.
    fn load(uuid: u128, custom_id: String, c: &Compound) -> Option<CustomBossEvent> {
        let name = Component::try_from_nbt(c.get("Name")?).ok()?;
        let flag = |k: &str| matches!(c.get(k), Some(Tag::Byte(b)) if *b != 0);
        let int = |k: &str, d: i32| c.get(k).and_then(Tag::as_i64).map_or(d, |v| v as i32);
        let text = |k: &str| c.get(k).and_then(Tag::as_str).unwrap_or("");
        let mut out = Vec::new();
        let mut e = CustomBossEvent::new(uuid, custom_id, name);
        e.set_visible(flag("Visible"), &mut out);
        e.set_value(int("Value", 0), &mut out);
        e.set_max(int("Max", 100), &mut out);
        e.color = COLORS
            .iter()
            .find(|x| x.1 == text("Color"))
            .map_or(BossBarColor::White, |x| x.0);
        e.overlay = OVERLAYS
            .iter()
            .find(|x| x.1 == text("Overlay"))
            .map_or(BossBarOverlay::Progress, |x| x.0);
        e.flags = BossBarFlags {
            darken_screen: flag("DarkenScreen"),
            play_music: flag("PlayBossMusic"),
            create_fog: flag("CreateWorldFog"),
        };
        if let Some(Tag::List(list)) = c.get("Players") {
            e.saved = list.0.iter().filter_map(tag_uuid).collect();
        }
        Some(e)
    }
}

fn uuid_tag(uuid: u128) -> Tag {
    #[allow(clippy::cast_possible_truncation)]
    Tag::IntArray(vec![
        (uuid >> 96) as i32,
        (uuid >> 64) as i32,
        (uuid >> 32) as i32,
        uuid as i32,
    ])
}

fn tag_uuid(tag: &Tag) -> Option<u128> {
    let Tag::IntArray(v) = tag else {
        return None;
    };
    let [a, b, c, d] = v.as_slice() else {
        return None;
    };
    Some([*a, *b, *c, *d].iter().fold(0u128, |acc, &x| {
        (acc << 32) | u128::from(u32::from_ne_bytes(x.to_ne_bytes()))
    }))
}

/// `CustomBossEvents`.
#[derive(Debug, Default)]
pub(crate) struct CustomBossEvents {
    events: BTreeMap<String, CustomBossEvent>,
    dirty: bool,
}

impl CustomBossEvents {
    pub(crate) fn get(&self, id: &str) -> Option<&CustomBossEvent> {
        self.events.get(id)
    }

    /// The event for a change; marks the data dirty (`setDirty`).
    pub(crate) fn get_mut(&mut self, id: &str) -> Option<&mut CustomBossEvent> {
        let e = self.events.get_mut(id)?;
        self.dirty = true;
        Some(e)
    }

    pub(crate) fn len(&self) -> usize {
        self.events.len()
    }

    pub(crate) fn events(&self) -> impl Iterator<Item = &CustomBossEvent> {
        self.events.values()
    }

    /// `create`.
    pub(crate) fn create(&mut self, uuid: u128, id: String, name: Component) -> &CustomBossEvent {
        self.dirty = true;
        self.events
            .entry(id.clone())
            .or_insert_with(|| CustomBossEvent::new(uuid, id, name))
    }

    /// `removeAllPlayers` and `remove`.
    pub(crate) fn remove(&mut self, id: &str, out: &mut Outbox) {
        if let Some(mut e) = self.events.remove(id) {
            e.remove_all(out);
            self.dirty = true;
        }
    }

    /// `onPlayerConnect`.
    pub(crate) fn player_connected(&mut self, player: u128, out: &mut Outbox) {
        for e in self.events.values_mut() {
            if e.saved.contains(&player) {
                e.show(player, out);
            }
        }
    }

    /// `onPlayerDisconnect`: the bars forget the connection (no packets to a gone player).
    pub(crate) fn player_disconnected(&mut self, player: u128) {
        for e in self.events.values_mut() {
            e.shown.retain(|&p| p != player);
        }
    }

    /// The saved data when it changed.
    pub(crate) fn take_dirty(&mut self) -> Option<Compound> {
        if !std::mem::take(&mut self.dirty) {
            return None;
        }
        let mut c = Compound::new();
        for (id, e) in &self.events {
            c.insert(id.as_str(), Tag::Compound(e.pack()));
        }
        Some(c)
    }

    /// Loads the saved data; `new_uuid` gives each bar its random UUID.
    pub(crate) fn load(c: &Compound, new_uuid: &mut dyn FnMut() -> u128) -> CustomBossEvents {
        let mut events = BTreeMap::new();
        for (id, tag) in c.iter() {
            if let Tag::Compound(packed) = tag
                && let Some(e) = CustomBossEvent::load(new_uuid(), id.to_string(), packed)
            {
                events.insert(id.to_string(), e);
            }
        }
        CustomBossEvents {
            events,
            dirty: false,
        }
    }
}

/// Sends the packets of a change to the online players.
pub(crate) fn send(global: &crate::global::Global, out: Outbox) {
    for (uuid, packet) in out {
        if let Some((sink, _)) = global.players.connection(crate::player_list::Uuid(uuid)) {
            sink.send_shared(&packet);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn players_visibility_and_save() {
        let mut bars = CustomBossEvents::default();
        bars.create(7, "test:bar".to_owned(), Component::text("Bar"));
        let mut out = Vec::new();
        let bar = bars.get_mut("test:bar").expect("bar");
        assert!(bar.set_players(&[1, 2], &mut out));
        // Visible: both players get the bar.
        assert_eq!(out.len(), 2);
        assert!(!bar.set_players(&[1, 2], &mut out));
        out.clear();
        bar.set_visible(false, &mut out);
        assert_eq!(out.len(), 2);
        out.clear();
        // Hidden bars send no updates.
        bar.set_value(50, &mut out);
        assert!(out.is_empty());
        assert!(bar.set_players(&[2], &mut out));
        assert_eq!(bar.shown(), &[2]);
        let saved = bars.take_dirty().expect("dirty");
        assert!(bars.take_dirty().is_none());
        let mut next = 0u128;
        let mut loaded = CustomBossEvents::load(&saved, &mut || {
            next += 1;
            next
        });
        let bar = loaded.get_mut("test:bar").expect("loaded");
        assert_eq!((bar.value(), bar.max(), bar.visible()), (50, 100, false));
        assert!(bar.shown().is_empty());
        loaded.player_connected(2, &mut out);
        assert_eq!(loaded.get("test:bar").expect("bar").shown(), &[2]);
    }
}
