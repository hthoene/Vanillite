//! Example Vanillite plugin for the packet interface. It shows the three kinds of packet
//! listeners and sending packets:
//!
//! - **Per receiver, rewriting**: `/nametags off` hides the name tags above other players'
//!   heads for the player who runs it, and only for that player. Team packets on their way
//!   to such a viewer are rewritten to "name tag never visible"; every other receiver keeps
//!   the buffer the server encoded once. Players without a team are put into a team that only
//!   this viewer knows, with a packet sent to that viewer alone. The listener exists only
//!   while a viewer hides name tags: without one, team packets cost nothing.
//! - **Per broadcast, rewriting**: `chat.prefix` in the config is put in front of every system
//!   message. The listener runs once per message, however many players receive it, and its
//!   replacement is again one shared buffer.
//! - **Serverbound, cancelling**: `signs.read-only` cancels the clients' sign edits before the
//!   game sees them. This listener works on the raw packet by name and needs no packet types.
//!
//! The typed packets come from the SDK's `packets` feature (the packet types of
//! `vanillite-protocol`, for the Minecraft version the plugin is built for).
//!
//! Build it with `cargo build -p vanillite-example-packets` and put `plugin.toml` and the
//! library into `plugins/packets/`.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock};

use vanillite_plugin::packet::{PacketFilter, PacketListener};
use vanillite_plugin::prelude::*;
use vanillite_plugin::protocol::packets::play::clientbound::{
    SetPlayerTeam, SystemChat, TeamAction, TeamParameters,
};
use vanillite_plugin::protocol::text::Component;
use vanillite_plugin::{Invocation, Sender};

/// `Team.Visibility.NEVER`.
const NEVER: i32 = 1;
/// The team only viewers with hidden name tags know.
const HIDDEN_TEAM: &str = "packets.hidden";

/// The viewers who turned name tags off. Shared with the packet listener, which runs on
/// network threads.
type Viewers = Arc<RwLock<HashSet<Uuid>>>;

struct Packets {
    hidden: Viewers,
    /// The listener on team packets, while a viewer hides name tags.
    team_listener: Option<PacketListener>,
    /// Sign edits cancelled so far.
    blocked_edits: Arc<AtomicU64>,
}

impl Plugin for Packets {
    fn load(registrar: &mut Registrar<Self>) -> Result<Self, Error> {
        registrar.config(include_str!("../config.toml"))?;
        let server = registrar.server();
        let hidden = Viewers::default();
        let blocked_edits = Arc::new(AtomicU64::new(0));

        // Per broadcast: one rewrite per system message for all its receivers.
        if let Some(prefix) = server
            .config_string("chat.prefix")
            .filter(|p| !p.is_empty())
        {
            server.on_packet_of::<SystemChat>(
                PacketFilter::of::<SystemChat>().per_broadcast(),
                move |event, packet| {
                    if packet.overlay {
                        return;
                    }
                    let content = Component::text(prefix.clone()).append(packet.content);
                    let _ = event.replace_with(&SystemChat {
                        content,
                        overlay: false,
                    });
                },
            )?;
        }

        // Serverbound, by name and without packet types: the edit never reaches the game.
        if server.config_bool("signs.read-only").unwrap_or(false) {
            let blocked = Arc::clone(&blocked_edits);
            registrar.on_packet(PacketFilter::serverbound("sign_update"), move |event| {
                blocked.fetch_add(1, Ordering::Relaxed);
                event.cancel();
            });
        }

        // A player who joins gets no name tag for the viewers who turned them off.
        registrar.on_join(priority::NORMAL, |plugin, ctx, event| {
            let joined = SetPlayerTeam {
                name: HIDDEN_TEAM.to_owned(),
                action: TeamAction::Join(vec![event.name().to_owned()]),
            };
            for viewer in plugin.viewers() {
                if viewer != event.player() {
                    let _ = ctx.server().send(viewer, &joined);
                }
            }
        });

        registrar.command(
            Command::literal("nametags")
                .then(
                    Command::literal("off").executes(|plugin: &mut Packets, ctx, invocation| {
                        plugin.set_name_tags(ctx, invocation, false)
                    }),
                )
                .then(
                    Command::literal("on").executes(|plugin: &mut Packets, ctx, invocation| {
                        plugin.set_name_tags(ctx, invocation, true)
                    }),
                )
                .then(Command::literal("status").executes(
                    |plugin: &mut Packets, ctx, invocation| {
                        invocation.reply(
                            ctx,
                            format!(
                                "{} viewers without name tags, {} sign edits blocked",
                                plugin.viewers().len(),
                                plugin.blocked_edits.load(Ordering::Relaxed)
                            ),
                        );
                        Ok(1)
                    },
                )),
        );

        Ok(Packets {
            hidden,
            team_listener: None,
            blocked_edits,
        })
    }
}

impl Packets {
    fn viewers(&self) -> Vec<Uuid> {
        self.hidden
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .copied()
            .collect()
    }

    /// Per receiver: rewrites the team packets on their way to viewers without name tags.
    fn listen_on_teams(&self, server: Server) -> Result<PacketListener, Error> {
        let viewers = Arc::clone(&self.hidden);
        server.on_packet_of::<SetPlayerTeam>(
            PacketFilter::of::<SetPlayerTeam>(),
            move |event, mut packet| {
                let hides = event.player().is_some_and(|viewer| {
                    viewers
                        .read()
                        .unwrap_or_else(PoisonError::into_inner)
                        .contains(&viewer)
                });
                if !hides {
                    return;
                }
                if let TeamAction::Add(parameters, _) | TeamAction::Change(parameters) =
                    &mut packet.action
                    && parameters.nametag_visibility != NEVER
                {
                    parameters.nametag_visibility = NEVER;
                    // Only this viewer gets a copy; everyone else shares the original.
                    let _ = event.replace_with(&packet);
                }
            },
        )
    }

    /// Shows or hides the other players' name tags for the player who runs the command.
    fn set_name_tags(
        &mut self,
        ctx: &Ctx<'_>,
        invocation: &Invocation<'_>,
        visible: bool,
    ) -> Result<i32, Text> {
        let Sender::Player(viewer) = invocation.sender() else {
            return Err("Only a player can hide name tags".into());
        };
        let server = ctx.server();
        let mut hidden = self.hidden.write().unwrap_or_else(PoisonError::into_inner);
        let packet = if visible {
            if !hidden.remove(&viewer) {
                return Err("Your name tags are visible".into());
            }
            SetPlayerTeam {
                name: HIDDEN_TEAM.to_owned(),
                action: TeamAction::Remove,
            }
        } else {
            if !hidden.insert(viewer) {
                return Err("Your name tags are hidden already".into());
            }
            // Everyone online joins a team whose name tags are never shown. The packet goes
            // to this viewer only, so nobody else's client knows the team.
            let members = server
                .online_players()
                .into_iter()
                .filter(|player| *player != viewer)
                .filter_map(|player| server.player_name(player))
                .collect();
            SetPlayerTeam {
                name: HIDDEN_TEAM.to_owned(),
                action: TeamAction::Add(
                    Box::new(TeamParameters {
                        display_name: Component::text(HIDDEN_TEAM),
                        prefix: Component::empty(),
                        suffix: Component::empty(),
                        nametag_visibility: NEVER,
                        collision_rule: 0,
                        color: None,
                        options: 0,
                    }),
                    members,
                ),
            }
        };
        let viewers = hidden.len();
        drop(hidden);
        // The listener lives while somebody needs it.
        if viewers == 0
            && let Some(listener) = self.team_listener.take()
        {
            let _ = server.remove_packet_listener(listener);
        } else if viewers > 0 && self.team_listener.is_none() {
            self.team_listener = self.listen_on_teams(server).ok();
        }
        server
            .send(viewer, &packet)
            .map_err(|e| Text::from(format!("Cannot send the team: {e}")))?;
        invocation.reply(
            ctx,
            if visible {
                "Name tags are visible again"
            } else {
                "Name tags are hidden for you"
            },
        );
        Ok(1)
    }
}

vanillite_plugin::export_plugin!(Packets);
