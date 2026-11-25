//! The source of a command (Vanilla's `CommandSourceStack`): who runs it, where, with which
//! permission level, and where its feedback goes.

use std::sync::Arc;

use vanillite_base::Vec3;
use vanillite_game::player::PacketSink;
use vanillite_game::player::join::display_name;
use vanillite_protocol::packets::play::clientbound::SystemChat;
use vanillite_protocol::text::{Color, Component, NamedColor};

use crate::global::Global;
use crate::player_list::Uuid;
use crate::world::WorldId;

/// Who runs a command.
#[derive(Clone)]
pub(crate) enum SourceKind {
    /// A player on this world.
    Player {
        uuid: u128,
        name: String,
        sink: Arc<dyn PacketSink>,
    },
    /// The server console.
    Console,
}

/// A command source.
#[derive(Clone)]
pub(crate) struct CommandSource {
    /// The server, for suggestions.
    pub(crate) global: Arc<Global>,
    pub(crate) kind: SourceKind,
    /// Permission level 0..=4.
    pub(crate) level: u8,
    /// The world the command runs in.
    pub(crate) world: WorldId,
    /// Position.
    pub(crate) pos: Vec3,
    /// Rotation (`y_rot`, `x_rot`).
    pub(crate) rot: (f32, f32),
}

impl vanillite_command::Source for CommandSource {
    fn permission_level(&self) -> u8 {
        self.level
    }

    fn online_player_names(&self) -> Vec<String> {
        self.global
            .players
            .snapshot()
            .into_iter()
            .map(|p| p.name)
            .collect()
    }

    fn has_permission_node(&self, node: &str) -> bool {
        match &self.kind {
            SourceKind::Player { uuid, .. } => {
                let operator = self.global.access.op_level(*uuid).is_some();
                self.global.plugins.has_permission(*uuid, node, operator)
            }
            SourceKind::Console => true,
        }
    }

    fn suggest_custom(
        &self,
        provider: &str,
        args: &vanillite_command::Args,
        builder: &mut vanillite_command::SuggestionsBuilder<'_>,
    ) {
        use vanillite_game::commands::suggest;
        if provider.starts_with(vanillite_plugin_host::SUGGEST_PREFIX) {
            let args = super::dispatch::plugin_args(None, self, args).unwrap_or_default();
            let list = self.global.plugins.suggest(
                provider,
                &self.plugin_sender(),
                &args,
                builder.remaining(),
            );
            builder.suggest_matching(list.iter().map(String::as_str));
            return;
        }
        let access = &self.global.access;
        let online = self.global.players.snapshot();
        let names: Vec<String> = match provider {
            suggest::OP_CANDIDATES => online
                .into_iter()
                .filter(|p| access.op_level(p.uuid.0).is_none())
                .map(|p| p.name)
                .collect(),
            suggest::OPS => access.ops().into_iter().map(|o| o.name).collect(),
            suggest::WHITELIST_CANDIDATES => {
                let listed = access.whitelisted_names();
                online
                    .into_iter()
                    .filter(|p| !listed.iter().any(|n| n.eq_ignore_ascii_case(&p.name)))
                    .map(|p| p.name)
                    .collect()
            }
            suggest::WHITELISTED => access.whitelisted_names(),
            suggest::TIMELINES => {
                builder.suggest_resource_ids(
                    self.registry_entries("minecraft:timeline")
                        .iter()
                        .map(String::as_str),
                );
                return;
            }
            _ => Vec::new(),
        };
        builder.suggest_matching(names.iter().map(String::as_str));
    }

    fn resource_exists(&self, registry: &str, id: &str) -> bool {
        match self.global.registries.get(registry) {
            Some(entries) => entries.iter().any(|e| e == id),
            None => vanillite_data::registry::Registry::by_name(registry)
                .is_none_or(|r| r.id(id).is_some()),
        }
    }

    fn registry_entries(&self, registry: &str) -> Vec<String> {
        match self.global.registries.get(registry) {
            Some(entries) => entries.clone(),
            None => vanillite_data::registry::Registry::by_name(registry)
                .map(|r| r.entries.iter().map(|e| (*e).to_owned()).collect())
                .unwrap_or_default(),
        }
    }
}

impl CommandSource {
    /// The source as a plugin command sender.
    pub(crate) fn plugin_sender(&self) -> vanillite_plugin_host::Sender {
        vanillite_plugin_host::Sender {
            player: self.uuid(),
            world: Some(u32::from(self.world.0)),
            pos: [self.pos.x, self.pos.y, self.pos.z],
        }
    }

    /// The player's UUID, if a player runs the command.
    pub(crate) fn uuid(&self) -> Option<u128> {
        match &self.kind {
            SourceKind::Player { uuid, .. } => Some(*uuid),
            SourceKind::Console => None,
        }
    }

    /// `getDisplayName`: the player's display name or `Server`.
    pub(crate) fn display_name(&self) -> Component {
        match &self.kind {
            SourceKind::Player { uuid, name, .. } => display_name(name, *uuid),
            SourceKind::Console => Component::text("Server"),
        }
    }

    /// `getTextName`.
    pub(crate) fn text_name(&self) -> &str {
        match &self.kind {
            SourceKind::Player { name, .. } => name,
            SourceKind::Console => "Server",
        }
    }

    /// `sendSystemMessage`: to the player (if it accepts system messages) or the log.
    pub(crate) fn send_system(&self, global: &Global, message: Component) {
        match &self.kind {
            SourceKind::Player { uuid, sink, .. } => {
                let accepts = global
                    .players
                    .connection(Uuid(*uuid))
                    .is_none_or(|(_, inbox)| {
                        inbox
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .accepts_system(false)
                    });
                if accepts {
                    sink.send(&SystemChat {
                        content: message,
                        overlay: false,
                    });
                }
            }
            SourceKind::Console => log::info!("{}", message.plain()),
        }
    }

    /// `sendSuccess`: feedback to the source and, when `inform_admins`, a gray italic note to
    /// the other operators and the log (`broadcastToAdmins`; the game rules
    /// `sendCommandFeedback` and `logAdminCommands` are on, as by default).
    pub(crate) fn send_success(&self, global: &Global, message: Component, inform_admins: bool) {
        self.send_system(global, message.clone());
        if !inform_admins {
            return;
        }
        let broadcast =
            Component::translatable("chat.type.admin", vec![self.display_name(), message])
                .color(Color::Named(NamedColor::Gray))
                .italic(true);
        let own = self.uuid();
        for entry in global.players.snapshot() {
            if Some(entry.uuid.0) != own && global.access.op_level(entry.uuid.0).is_some() {
                let accepts = entry
                    .inbox
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .accepts_system(false);
                if accepts {
                    entry.sink.send(&SystemChat {
                        content: broadcast.clone(),
                        overlay: false,
                    });
                }
            }
        }
        if own.is_some() {
            log::info!("{}", broadcast.plain());
        }
    }

    /// `sendFailure`: the message in red.
    pub(crate) fn send_failure(&self, global: &Global, message: Component) {
        self.send_system(
            global,
            Component::empty()
                .append(message)
                .color(Color::Named(NamedColor::Red)),
        );
    }
}
