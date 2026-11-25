//! Server boss bars (`ServerBossEvent`): the bar's state, its players and the `boss_event`
//! packets that keep the players' bars in sync.

use std::sync::Arc;

use vanillite_protocol::SharedPacket;
use vanillite_protocol::packets::play::clientbound::{
    BossBarColor, BossBarFlags, BossBarOverlay, BossEvent, BossEventOperation,
};
use vanillite_protocol::text::Component;
use vanillite_protocol::types::Uuid;
use vanillite_world::entity::EntityId;

use crate::survival::Effect;

/// A boss bar shown to a set of players.
#[derive(Clone, Debug, PartialEq)]
pub struct BossBar {
    pub id: u128,
    name: Component,
    progress: f32,
    color: BossBarColor,
    overlay: BossBarOverlay,
    flags: BossBarFlags,
    visible: bool,
    players: Vec<EntityId>,
}

impl BossBar {
    /// `new ServerBossEvent(id, name, color, overlay)`: full progress, visible, no players.
    pub fn new(id: u128, name: Component, color: BossBarColor, overlay: BossBarOverlay) -> BossBar {
        BossBar {
            id,
            name,
            progress: 1.0,
            color,
            overlay,
            flags: BossBarFlags::default(),
            visible: true,
            players: Vec::new(),
        }
    }

    /// With `setDarkenScreen` / `setPlayBossMusic` / `setCreateWorldFog` (before players see it).
    pub fn with_flags(mut self, flags: BossBarFlags) -> BossBar {
        self.flags = flags;
        self
    }

    /// The players that see the bar.
    pub fn players(&self) -> &[EntityId] {
        &self.players
    }

    pub fn progress(&self) -> f32 {
        self.progress
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    fn packet(&self, operation: BossEventOperation) -> Arc<SharedPacket> {
        SharedPacket::of(&BossEvent {
            id: Uuid(self.id),
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

    fn broadcast(&self, operation: BossEventOperation, out: &mut Vec<Effect>) {
        if !self.visible || self.players.is_empty() {
            return;
        }
        let packet = self.packet(operation);
        for &player in &self.players {
            out.push(Effect::To {
                player,
                packet: Arc::clone(&packet),
            });
        }
    }

    /// `setProgress`: sent when it changed.
    pub fn set_progress(&mut self, progress: f32, out: &mut Vec<Effect>) {
        if progress != self.progress {
            self.progress = progress;
            self.broadcast(BossEventOperation::UpdateProgress(progress), out);
        }
    }

    /// `setName`.
    pub fn set_name(&mut self, name: Component, out: &mut Vec<Effect>) {
        if name != self.name {
            self.name = name.clone();
            self.broadcast(BossEventOperation::UpdateName(name), out);
        }
    }

    /// `setColor` and `setOverlay`.
    pub fn set_style(
        &mut self,
        color: BossBarColor,
        overlay: BossBarOverlay,
        out: &mut Vec<Effect>,
    ) {
        if color != self.color || overlay != self.overlay {
            self.color = color;
            self.overlay = overlay;
            self.broadcast(BossEventOperation::UpdateStyle { color, overlay }, out);
        }
    }

    /// `setDarkenScreen` / `setPlayBossMusic` / `setCreateWorldFog`.
    pub fn set_flags(&mut self, flags: BossBarFlags, out: &mut Vec<Effect>) {
        if flags != self.flags {
            self.flags = flags;
            self.broadcast(BossEventOperation::UpdateProperties(flags), out);
        }
    }

    /// `addPlayer`.
    pub fn add_player(&mut self, player: EntityId, out: &mut Vec<Effect>) {
        if self.players.contains(&player) {
            return;
        }
        self.players.push(player);
        if self.visible {
            out.push(Effect::To {
                player,
                packet: self.add_packet(),
            });
        }
    }

    /// `removePlayer`.
    pub fn remove_player(&mut self, player: EntityId, out: &mut Vec<Effect>) {
        let Some(i) = self.players.iter().position(|&p| p == player) else {
            return;
        };
        self.players.swap_remove(i);
        if self.visible {
            out.push(Effect::To {
                player,
                packet: self.packet(BossEventOperation::Remove),
            });
        }
    }

    /// `removeAllPlayers`.
    pub fn remove_all_players(&mut self, out: &mut Vec<Effect>) {
        self.broadcast(BossEventOperation::Remove, out);
        self.players.clear();
    }

    /// `setVisible`.
    pub fn set_visible(&mut self, visible: bool, out: &mut Vec<Effect>) {
        if visible == self.visible {
            return;
        }
        self.visible = visible;
        for &player in &self.players {
            out.push(Effect::To {
                player,
                packet: if visible {
                    self.add_packet()
                } else {
                    self.packet(BossEventOperation::Remove)
                },
            });
        }
    }

    /// Makes the players exactly `wanted` (the bosses' per-second player update): removes the
    /// others, adds the new ones. `wanted` must not contain duplicates.
    pub fn sync_players(&mut self, wanted: &[EntityId], out: &mut Vec<Effect>) {
        let gone: Vec<EntityId> = self
            .players
            .iter()
            .copied()
            .filter(|p| !wanted.contains(p))
            .collect();
        for p in gone {
            self.remove_player(p, out);
        }
        for &p in wanted {
            self.add_player(p, out);
        }
    }
}
