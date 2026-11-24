//! The source of a command (Vanilla's `CommandSourceStack`): who runs it, where, with which
//! permission level, and where its feedback goes.

use std::sync::Arc;

use vanillite_base::Vec3;
use vanillite_game::player::PacketSink;
use vanillite_game::player::join::display_name;
use vanillite_protocol::packets::play::clientbound::SystemChat;
use vanillite_protocol::text::{Color, Component, NamedColor};

use vanillite_command::values::Anchor;
use vanillite_data::entity::EntityTypeId;
use vanillite_world::entity::EntityId;

use super::engine::Callback;
use crate::global::Global;
use crate::player_list::Uuid;
use crate::world::WorldId;

/// Who gets the feedback of a command (Vanilla's `CommandSource`).
#[derive(Clone)]
pub(crate) enum SourceKind {
    /// A player.
    Player {
        uuid: u128,
        name: String,
        sink: Arc<dyn PacketSink>,
    },
    /// The server console.
    Console,
    /// The server itself (functions of the game loop, scheduled functions): no feedback.
    Server,
}

/// The entity a command runs as (`CommandSourceStack.entity`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SourceEntity {
    /// UUID.
    pub(crate) uuid: u128,
    /// The world the entity is in.
    pub(crate) world: WorldId,
    /// The entity, when it is in the world the command runs on.
    pub(crate) id: Option<EntityId>,
    /// Its type.
    pub(crate) kind: EntityTypeId,
    /// `getScoreboardName`/`getName`: the player name, else the custom name or the type name.
    pub(crate) name: String,
    /// Its display name.
    pub(crate) display: Component,
    /// Eye height above the feet.
    pub(crate) eye_height: f64,
}

impl SourceEntity {
    /// Whether it is a player.
    pub(crate) fn is_player(&self) -> bool {
        self.kind == vanillite_data::entity::ids::PLAYER
    }
}

/// A command source (`CommandSourceStack`).
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
    /// The entity it runs as.
    pub(crate) entity: Option<SourceEntity>,
    /// `anchored`.
    pub(crate) anchor: Anchor,
    /// No feedback at all (`withSuppressedOutput`).
    pub(crate) silent: bool,
    /// Gets the result of the command (`execute store`, function results).
    pub(crate) callback: Callback,
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

    fn suggest_custom(
        &self,
        provider: &str,
        _args: &vanillite_command::Args,
        builder: &mut vanillite_command::SuggestionsBuilder<'_>,
    ) {
        use vanillite_game::commands::suggest;
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
            suggest::SCORE_HOLDERS => {
                let mut names: Vec<String> = online.into_iter().map(|p| p.name).collect();
                names.extend(
                    self.global
                        .systems
                        .scoreboard()
                        .holders()
                        .filter(|h| !names.iter().any(|n| n == h))
                        .map(str::to_owned)
                        .collect::<Vec<_>>(),
                );
                names
            }
            suggest::TRIGGER_OBJECTIVES => self
                .global
                .systems
                .scoreboard()
                .objectives()
                .filter(|o| o.criteria == vanillite_game::scoreboard::CriteriaKind::Trigger)
                .map(|o| o.name.clone())
                .collect(),
            suggest::FUNCTIONS => {
                let library = self.global.systems.functions();
                let tags: Vec<String> = library.tag_names().map(|t| format!("#{t}")).collect();
                let names: Vec<&str> = library.names().collect();
                builder.suggest_resource_ids(names.iter().copied());
                builder.suggest_matching(tags.iter().map(String::as_str));
                return;
            }
            suggest::STORAGE => {
                let ids: Vec<String> = self.global.systems.storage().keys().cloned().collect();
                builder.suggest_resource_ids(ids.iter().map(String::as_str));
                return;
            }
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

    fn registry_tags(&self, registry: &str) -> Vec<String> {
        if registry == "minecraft:function" {
            return self
                .global
                .systems
                .functions()
                .tag_names()
                .map(str::to_owned)
                .collect();
        }
        self.global
            .systems
            .tags()
            .get(registry)
            .map(|t| t.keys().cloned().collect())
            .unwrap_or_default()
    }

    fn tag_entries(&self, registry: &str, tag: &str) -> Vec<String> {
        self.global
            .systems
            .tags()
            .get(registry)
            .and_then(|t| t.get(tag))
            .cloned()
            .unwrap_or_default()
    }

    fn objective_names(&self) -> Vec<String> {
        self.global
            .systems
            .scoreboard()
            .objectives()
            .map(|o| o.name.clone())
            .collect()
    }

    fn team_names(&self) -> Vec<String> {
        self.global
            .systems
            .scoreboard()
            .teams()
            .map(|t| t.name.clone())
            .collect()
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
    /// A source with the defaults of `CommandSourceStack`'s constructor.
    pub(crate) fn new(
        global: Arc<Global>,
        kind: SourceKind,
        level: u8,
        world: WorldId,
        pos: Vec3,
        rot: (f32, f32),
        entity: Option<SourceEntity>,
    ) -> CommandSource {
        CommandSource {
            global,
            kind,
            level,
            world,
            pos,
            rot,
            entity,
            anchor: Anchor::Feet,
            silent: false,
            callback: Callback::Empty,
        }
    }

    /// `EntityAnchorArgument.Anchor.apply(source)`: the position, or the entity's eyes.
    pub(crate) fn anchor_pos(&self) -> Vec3 {
        match (self.anchor, &self.entity) {
            (Anchor::Eyes, Some(e)) => self.pos + Vec3::new(0.0, e.eye_height, 0.0),
            _ => self.pos,
        }
    }

    /// The player's UUID, if the source runs as a player (`getPlayer`).
    pub(crate) fn uuid(&self) -> Option<u128> {
        self.entity
            .as_ref()
            .filter(|e| e.is_player())
            .map(|e| e.uuid)
    }

    /// The UUID of the player that gets the feedback.
    pub(crate) fn receiver_uuid(&self) -> Option<u128> {
        match &self.kind {
            SourceKind::Player { uuid, .. } => Some(*uuid),
            SourceKind::Console | SourceKind::Server => None,
        }
    }

    /// `getDisplayName`: the entity's display name, the player's, or `Server`.
    pub(crate) fn display_name(&self) -> Component {
        if let Some(e) = &self.entity {
            return e.display.clone();
        }
        match &self.kind {
            SourceKind::Player { uuid, name, .. } => display_name(name, *uuid),
            SourceKind::Console | SourceKind::Server => Component::text("Server"),
        }
    }

    /// `getTextName`.
    pub(crate) fn text_name(&self) -> &str {
        if let Some(e) = &self.entity {
            return &e.name;
        }
        match &self.kind {
            SourceKind::Player { name, .. } => name,
            SourceKind::Console | SourceKind::Server => "Server",
        }
    }

    /// `withCallback`.
    pub(crate) fn with_callback(&self, callback: Callback) -> CommandSource {
        CommandSource {
            callback,
            ..self.clone()
        }
    }

    /// `sendSystemMessage`: to the player (if it accepts system messages) or the log.
    pub(crate) fn send_system(&self, global: &Global, message: Component) {
        if self.silent {
            return;
        }
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
            SourceKind::Server => {}
        }
    }

    /// `sendSuccess`: feedback to the source and, when `inform_admins`, a gray italic note to
    /// the other operators and the log (`broadcastToAdmins`; the game rules
    /// `sendCommandFeedback` and `logAdminCommands` are on, as by default).
    pub(crate) fn send_success(&self, global: &Global, message: Component, inform_admins: bool) {
        if self.silent || matches!(self.kind, SourceKind::Server) {
            return;
        }
        self.send_system(global, message.clone());
        if !inform_admins {
            return;
        }
        let broadcast =
            Component::translatable("chat.type.admin", vec![self.display_name(), message])
                .color(Color::Named(NamedColor::Gray))
                .italic(true);
        let own = self.receiver_uuid();
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
