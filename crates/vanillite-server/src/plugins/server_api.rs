//! The server-wide side of the plugin API: scoreboard, boss bars, access lists, world time,
//! weather and game rules. Each change works like the Vanilla command that does the same
//! (`/scoreboard`, `/team`, `/bossbar`, `/op`, `/whitelist`, `/ban`, `/time`, `/weather`,
//! `/gamerule`) without its feedback.

use vanillite_abi::{access_list, bossbar_prop, display_slot, status};
use vanillite_game::scoreboard::{
    COLOR_NAMES, CollisionRule, CriteriaKind, Objective, Team, Visibility,
};
use vanillite_game::survival::rules::{Rule, RuleType};
use vanillite_plugin_host::{AccessOp, BossBarOp, ScoreReply, ScoreboardOp};
use vanillite_protocol::text::Component;

use super::component;
use crate::access::{Ban, IpBan, UserBan};
use crate::global::Global;
use crate::player_list::Uuid;

type Result<T> = std::result::Result<T, i32>;

/// A text component from a plugin's JSON or a plain text.
fn json_or_text(value: &str) -> Component {
    match serde_json::from_str::<serde_json::Value>(value) {
        Ok(v) if v.is_object() || v.is_array() || v.is_string() => Component::from_json(&v),
        _ => Component::text(value),
    }
}

/// The `/team modify` options plugins change.
fn modify_team(team: &mut Team, option: &str, value: &str) -> Result<()> {
    let visibility = |v: &str| {
        Visibility::ALL
            .into_iter()
            .find(|x| x.name() == v)
            .ok_or(status::INVALID_ARGUMENT)
    };
    let flag = |v: &str| match v {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(status::INVALID_ARGUMENT),
    };
    match option {
        "displayName" => team.display_name = json_or_text(value),
        "prefix" => team.prefix = json_or_text(value),
        "suffix" => team.suffix = json_or_text(value),
        "color" => {
            team.color = match value {
                "reset" => None,
                v => Some(
                    COLOR_NAMES
                        .iter()
                        .position(|c| *c == v)
                        .and_then(|i| u8::try_from(i).ok())
                        .ok_or(status::INVALID_ARGUMENT)?,
                ),
            };
        }
        "friendlyFire" => team.allow_friendly_fire = flag(value)?,
        "seeFriendlyInvisibles" => team.see_friendly_invisibles = flag(value)?,
        "nametagVisibility" => team.nametag_visibility = visibility(value)?,
        "deathMessageVisibility" => team.death_message_visibility = visibility(value)?,
        "collisionRule" => {
            team.collision_rule = CollisionRule::ALL
                .into_iter()
                .find(|x| x.name() == value)
                .ok_or(status::INVALID_ARGUMENT)?;
        }
        _ => return Err(status::INVALID_ARGUMENT),
    }
    Ok(())
}

/// Runs a scoreboard operation and sends the change.
pub(super) fn scoreboard(global: &Global, op: ScoreboardOp<'_>) -> Result<ScoreReply> {
    let reply = {
        let mut board = global.systems.scoreboard();
        match op {
            ScoreboardOp::AddObjective {
                name,
                criteria,
                display,
            } => {
                let criteria = CriteriaKind::by_name(criteria).ok_or(status::INVALID_ARGUMENT)?;
                if name.contains(' ') || board.objective(name).is_some() {
                    return Err(status::INVALID_ARGUMENT);
                }
                board.add_objective(Objective {
                    name: name.to_owned(),
                    render_type: criteria.default_render_type(),
                    criteria,
                    display_name: component(display),
                    display_auto_update: false,
                    number_format: None,
                });
                ScoreReply::Done
            }
            ScoreboardOp::RemoveObjective { name } => {
                if !board.remove_objective(name) {
                    return Err(status::NOT_FOUND);
                }
                ScoreReply::Done
            }
            ScoreboardOp::Display { slot, objective } => {
                let slot = u8::try_from(slot)
                    .ok()
                    .filter(|s| u32::from(*s) < display_slot::SIDEBAR_TEAM_BLACK + 16)
                    .ok_or(status::INVALID_ARGUMENT)?;
                if objective.is_some_and(|o| board.objective(o).is_none()) {
                    return Err(status::NOT_FOUND);
                }
                board.set_display(slot, objective);
                ScoreReply::Done
            }
            ScoreboardOp::GetScore { objective, holder } => {
                if board.objective(objective).is_none() {
                    return Err(status::NOT_FOUND);
                }
                let score = board.score(holder, objective).ok_or(status::NOT_FOUND)?;
                ScoreReply::Score(score.value)
            }
            ScoreboardOp::SetScore {
                objective,
                holder,
                value,
            } => {
                if board
                    .objective(objective)
                    .is_some_and(|o| o.criteria.is_read_only())
                {
                    return Err(status::INVALID_ARGUMENT);
                }
                if !board.set_score(holder, objective, value, None) {
                    return Err(status::NOT_FOUND);
                }
                ScoreReply::Done
            }
            ScoreboardOp::ResetScore { objective, holder } => {
                match objective {
                    Some(o) => board.reset_score(holder, o),
                    None => board.reset_all(holder),
                };
                ScoreReply::Done
            }
            ScoreboardOp::AddTeam { name } => {
                if name.contains(' ') || !board.add_team(name) {
                    return Err(status::INVALID_ARGUMENT);
                }
                ScoreReply::Done
            }
            ScoreboardOp::RemoveTeam { name } => {
                if !board.remove_team(name) {
                    return Err(status::NOT_FOUND);
                }
                ScoreReply::Done
            }
            ScoreboardOp::JoinTeam { team, holder } => {
                if board.team(team).is_none() {
                    return Err(status::NOT_FOUND);
                }
                board.join_team(holder, team);
                ScoreReply::Done
            }
            ScoreboardOp::LeaveTeam { holder } => {
                board.leave_team(holder);
                ScoreReply::Done
            }
            ScoreboardOp::ModifyTeam {
                team,
                option,
                value,
            } => {
                let mut changed = board.team(team).cloned().ok_or(status::NOT_FOUND)?;
                modify_team(&mut changed, option, value)?;
                board.modify_team(team, |t| *t = changed);
                ScoreReply::Done
            }
            ScoreboardOp::TeamOf { holder } => match board.team_of(holder) {
                Some(team) => ScoreReply::Team(team.name.clone()),
                None => return Err(status::NOT_FOUND),
            },
        }
    };
    crate::scoreboard::flush(global);
    Ok(reply)
}

/// A random version 4 UUID for a plugin's boss bar.
fn new_uuid() -> u128 {
    let hi = u64::from_le_bytes(vanillite_net::rand_bytes());
    let lo = u64::from_le_bytes(vanillite_net::rand_bytes());
    let v = (u128::from(hi) << 64) | u128::from(lo);
    // Version 4 in bits 76..80, variant `10` in bits 62..64.
    let v = (v & !(0xf << 76)) | (0x4 << 76);
    (v & !(0x3 << 62)) | (0x2 << 62)
}

/// Changes a custom boss bar and sends the change.
pub(super) fn boss_bar(global: &Global, op: BossBarOp<'_>) -> Result<()> {
    let mut out = Vec::new();
    {
        let mut bars = global.systems.bossbars();
        match op {
            BossBarOp::Create { key, name } => {
                if bars.get(key).is_some() {
                    return Err(status::INVALID_ARGUMENT);
                }
                bars.create(new_uuid(), key.to_owned(), component(name));
            }
            BossBarOp::Remove { key } => {
                if bars.get(key).is_none() {
                    return Err(status::NOT_FOUND);
                }
                bars.remove(key, &mut out);
            }
            BossBarOp::Set { key, prop, value } => {
                let bar = bars.get_mut(key).ok_or(status::NOT_FOUND)?;
                let int = i32::try_from(value).map_err(|_| status::INVALID_ARGUMENT)?;
                let index = u8::try_from(value).map_err(|_| status::INVALID_ARGUMENT);
                match prop {
                    bossbar_prop::VALUE if int >= 0 => bar.set_value(int, &mut out),
                    bossbar_prop::MAX if int >= 1 => bar.set_max(int, &mut out),
                    bossbar_prop::COLOR if value <= 6 => {
                        bar.set_color(crate::bossbars::color_of(index?), &mut out);
                    }
                    bossbar_prop::STYLE if value <= 4 => {
                        bar.set_overlay(crate::bossbars::overlay_of(index?), &mut out);
                    }
                    bossbar_prop::VISIBLE => bar.set_visible(value != 0, &mut out),
                    _ => return Err(status::INVALID_ARGUMENT),
                }
            }
            BossBarOp::SetName { key, name } => {
                bars.get_mut(key)
                    .ok_or(status::NOT_FOUND)?
                    .set_name(component(name), &mut out);
            }
            BossBarOp::Player { key, player, show } => {
                if show && !global.players.contains(Uuid(player)) {
                    return Err(status::NOT_FOUND);
                }
                bars.get_mut(key)
                    .ok_or(status::NOT_FOUND)?
                    .set_player(player, show, &mut out);
            }
        }
    }
    crate::bossbars::send(global, out);
    Ok(())
}

/// Reads or changes an access list as `/op`, `/whitelist`, `/ban` and `/ban-ip` do.
pub(super) fn access(global: &Global, op: AccessOp<'_>) -> Result<bool> {
    let access = &global.access;
    let AccessOp {
        list,
        player,
        name,
        change,
    } = op;
    let reason = |r: &str| (!r.is_empty()).then(|| r.to_owned());
    let Some((add, why)) = change else {
        return match list {
            access_list::OPERATOR => Ok(access.op_level(player).is_some()),
            access_list::WHITELIST => Ok(access.is_whitelisted(player)),
            access_list::BAN => Ok(access.is_banned(player)),
            access_list::IP_BAN => Ok(access.is_ip_banned(name)),
            access_list::WHITELIST_ON => Ok(access.whitelist_enabled()),
            _ => Err(status::INVALID_ARGUMENT),
        };
    };
    let changed = match (list, add) {
        (access_list::OPERATOR, true) => access.op(player, name),
        (access_list::OPERATOR, false) => access.deop(player),
        (access_list::WHITELIST, true) => access.whitelist_add(player, name),
        (access_list::WHITELIST, false) => access.whitelist_remove(player),
        (access_list::BAN, true) => {
            access.ban(UserBan {
                uuid: Uuid(player),
                name: name.to_owned(),
                ban: Ban::now("Server", reason(why)),
            });
            if let Some((sink, _)) = global.players.connection(Uuid(player)) {
                sink.kick(Component::translatable(
                    "multiplayer.disconnect.banned",
                    vec![],
                ));
            }
            true
        }
        (access_list::BAN, false) => access.pardon(player),
        (access_list::IP_BAN, true) => {
            if name.parse::<std::net::IpAddr>().is_err() {
                return Err(status::INVALID_ARGUMENT);
            }
            access.ban_ip(IpBan {
                ip: name.to_owned(),
                ban: Ban::now("Server", reason(why)),
            });
            for p in global.players.snapshot().iter().filter(|p| p.ip == name) {
                p.sink.kick(Component::translatable(
                    "multiplayer.disconnect.ip_banned",
                    vec![],
                ));
            }
            true
        }
        (access_list::IP_BAN, false) => access.pardon_ip(name),
        (access_list::WHITELIST_ON, on) => {
            let changed = access.whitelist_enabled() != on;
            access.set_whitelist_enabled(on);
            changed
        }
        _ => return Err(status::INVALID_ARGUMENT),
    };
    match list {
        access_list::OPERATOR if changed => crate::commands::permissions_changed(global, player),
        access_list::WHITELIST if !add => crate::commands::exec::kick_unlisted(global),
        access_list::WHITELIST_ON if add => crate::commands::exec::kick_unlisted(global),
        _ => {}
    }
    Ok(changed)
}

/// The default clock of a world (`getDefaultClock`).
fn world_clock(global: &Global, world: u32) -> Option<usize> {
    global
        .dimensions
        .default_clock(crate::world::WorldId(u16::try_from(world).ok()?))
}

/// The total ticks of a world's default clock.
pub(super) fn world_time(global: &Global, world: u32) -> Option<i64> {
    let clock = world_clock(global, world)?;
    global
        .level
        .clocks()
        .clocks
        .get(clock)
        .map(|c| c.total_ticks)
}

/// `/time set` on a world's default clock.
pub(super) fn set_world_time(global: &Global, world: u32, time: i64) -> Result<()> {
    let clock = world_clock(global, world).ok_or(status::NOT_FOUND)?;
    global.level.clocks().set_total_ticks(clock, time);
    crate::commands::clock_changed(global, clock);
    Ok(())
}

/// `/weather`.
pub(super) fn set_weather(global: &Global, weather: u32, duration: i32) -> Result<()> {
    use crate::commands::Weather;
    let kind = match weather {
        vanillite_abi::weather::CLEAR => Weather::Clear,
        vanillite_abi::weather::RAIN => Weather::Rain,
        vanillite_abi::weather::THUNDER => Weather::Thunder,
        _ => return Err(status::INVALID_ARGUMENT),
    };
    crate::commands::set_weather(global, kind, (duration >= 0).then_some(duration));
    Ok(())
}

/// A game rule by its name, with or without the `minecraft:` namespace.
fn rule(name: &str) -> Option<Rule> {
    Rule::by_name(name.strip_prefix("minecraft:").unwrap_or(name))
}

/// `/gamerule <rule>`.
pub(super) fn game_rule(global: &Global, name: &str) -> Option<String> {
    let rule = rule(name)?;
    Some(rule.serialize(global.level.rules().get(rule)))
}

/// `/gamerule <rule> <value>`.
pub(super) fn set_game_rule(global: &Global, name: &str, value: &str) -> Result<()> {
    let rule = rule(name).ok_or(status::NOT_FOUND)?;
    let value = match rule.info().kind {
        RuleType::Bool => match value {
            "true" => 1,
            "false" => 0,
            _ => return Err(status::INVALID_ARGUMENT),
        },
        RuleType::Int { min, max } => value
            .parse::<i32>()
            .ok()
            .filter(|v| (min..=max).contains(v))
            .ok_or(status::INVALID_ARGUMENT)?,
    };
    global.level.rules_mut().set(rule, value);
    crate::commands::on_game_rule_changed(global, rule, value);
    Ok(())
}
