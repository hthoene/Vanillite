//! Syntax of the chat and team commands: dialog, me, team, teammsg, tellraw, title, trigger.

use vanillite_command::{ArgumentType, Builder, Dispatcher, StringKind, argument, literal};

use super::{CommandId, TeamOp, TitleOp, TriggerOp, level, suggest};
use crate::scoreboard::{CollisionRule, Visibility};

fn players() -> ArgumentType {
    ArgumentType::Entity {
        single: false,
        players_only: true,
    }
}

/// `DialogCommand`.
pub(super) fn register_dialog(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("dialog")
            .requires(level::GAMEMASTERS)
            .then(
                literal("show").then(argument("targets", players()).then(
                    argument("dialog", ArgumentType::Dialog).executes(CommandId::DialogShow),
                )),
            )
            .then(
                literal("clear")
                    .then(argument("targets", players()).executes(CommandId::DialogClear)),
            ),
    );
}

/// `EmoteCommands`.
pub(super) fn register_me(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("me").then(argument("action", ArgumentType::Message).executes(CommandId::Me)),
    );
}

fn team(op: TeamOp) -> CommandId {
    CommandId::Team(op)
}

/// `team modify <team> <option>` with a literal per value; `id` gives the value's id.
fn choices(
    name: &str,
    values: &[&str],
    id: fn(&str) -> Option<i32>,
    op: fn(u8) -> TeamOp,
) -> Builder<CommandId> {
    values.iter().fold(literal(name), |node, value| {
        let id = id(value).and_then(|i| u8::try_from(i).ok()).unwrap_or(0);
        node.then(literal(value).executes(team(op(id))))
    })
}

fn visibility_id(name: &str) -> Option<i32> {
    Visibility::by_name(name).map(Visibility::id)
}

fn collision_id(name: &str) -> Option<i32> {
    CollisionRule::by_name(name).map(CollisionRule::id)
}

/// `TeamCommand`.
pub(super) fn register_team(d: &mut Dispatcher<CommandId>) {
    let team_arg = || argument("team", ArgumentType::Team);
    let visibility = ["never", "hideForOtherTeams", "hideForOwnTeam", "always"];
    let modify = team_arg()
        .then(literal("displayName").then(
            argument("displayName", ArgumentType::Component).executes(team(TeamOp::DisplayName)),
        ))
        .then(
            literal("color")
                .then(literal("reset").executes(team(TeamOp::ColorReset)))
                .then(argument("value", ArgumentType::TeamColor).executes(team(TeamOp::Color))),
        )
        .then(
            literal("friendlyFire")
                .then(argument("allowed", ArgumentType::Bool).executes(team(TeamOp::FriendlyFire))),
        )
        .then(literal("seeFriendlyInvisibles").then(
            argument("allowed", ArgumentType::Bool).executes(team(TeamOp::SeeFriendlyInvisibles)),
        ))
        .then(choices(
            "nametagVisibility",
            &visibility,
            visibility_id,
            TeamOp::NametagVisibility,
        ))
        .then(choices(
            "deathMessageVisibility",
            &visibility,
            visibility_id,
            TeamOp::DeathMessageVisibility,
        ))
        .then(choices(
            "collisionRule",
            &["never", "pushOwnTeam", "pushOtherTeams", "always"],
            collision_id,
            TeamOp::CollisionRule,
        ))
        .then(
            literal("prefix")
                .then(argument("prefix", ArgumentType::Component).executes(team(TeamOp::Prefix))),
        )
        .then(
            literal("suffix")
                .then(argument("suffix", ArgumentType::Component).executes(team(TeamOp::Suffix))),
        );
    let members = || {
        argument("members", ArgumentType::ScoreHolder { multiple: true })
            .suggests(suggest::SCORE_HOLDERS)
    };
    d.register(
        literal("team")
            .requires(level::GAMEMASTERS)
            .then(
                literal("list")
                    .executes(team(TeamOp::List))
                    .then(team_arg().executes(team(TeamOp::ListMembers))),
            )
            .then(
                literal("add").then(
                    argument("team", ArgumentType::String(StringKind::SingleWord))
                        .executes(team(TeamOp::Add))
                        .then(
                            argument("displayName", ArgumentType::Component)
                                .executes(team(TeamOp::Add)),
                        ),
                ),
            )
            .then(literal("remove").then(team_arg().executes(team(TeamOp::Remove))))
            .then(literal("empty").then(team_arg().executes(team(TeamOp::Empty))))
            .then(
                literal("join").then(
                    team_arg()
                        .executes(team(TeamOp::JoinSelf))
                        .then(members().executes(team(TeamOp::Join))),
                ),
            )
            .then(literal("leave").then(members().executes(team(TeamOp::Leave))))
            .then(literal("modify").then(modify)),
    );
}

/// `TeamMsgCommand`.
pub(super) fn register_team_msg(d: &mut Dispatcher<CommandId>) {
    let msg = d.register(
        literal("teammsg")
            .then(argument("message", ArgumentType::Message).executes(CommandId::TeamMsg)),
    );
    d.register(literal("tm").redirect(msg));
}

/// `TellRawCommand`.
pub(super) fn register_tellraw(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("tellraw").requires(level::GAMEMASTERS).then(
            argument("targets", players())
                .then(argument("message", ArgumentType::Component).executes(CommandId::TellRaw)),
        ),
    );
}

/// `TitleCommand`.
pub(super) fn register_title(d: &mut Dispatcher<CommandId>) {
    let title = |op: TitleOp| CommandId::Title(op);
    let text = |name: &str, op: TitleOp| {
        literal(name).then(argument("title", ArgumentType::Component).executes(title(op)))
    };
    let time = || ArgumentType::Time { min: 0 };
    d.register(
        literal("title").requires(level::GAMEMASTERS).then(
            argument("targets", players())
                .then(literal("clear").executes(title(TitleOp::Clear)))
                .then(literal("reset").executes(title(TitleOp::Reset)))
                .then(text("title", TitleOp::Title))
                .then(text("subtitle", TitleOp::Subtitle))
                .then(text("actionbar", TitleOp::Actionbar))
                .then(
                    literal("times").then(
                        argument("fadeIn", time()).then(
                            argument("stay", time())
                                .then(argument("fadeOut", time()).executes(title(TitleOp::Times))),
                        ),
                    ),
                ),
        ),
    );
}

/// `TriggerCommand`.
pub(super) fn register_trigger(d: &mut Dispatcher<CommandId>) {
    let trigger = |op: TriggerOp| CommandId::Trigger(op);
    let value = |op: TriggerOp| {
        argument(
            "value",
            ArgumentType::Integer {
                min: None,
                max: None,
            },
        )
        .executes(trigger(op))
    };
    d.register(
        literal("trigger").then(
            argument("objective", ArgumentType::Objective)
                .suggests(suggest::TRIGGERS)
                .executes(trigger(TriggerOp::Simple))
                .then(literal("add").then(value(TriggerOp::Add)))
                .then(literal("set").then(value(TriggerOp::Set))),
        ),
    );
}
