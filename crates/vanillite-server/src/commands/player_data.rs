//! `/experience`, `/recipe` and `/attribute` (Vanilla's `ExperienceCommand`, `RecipeCommand`,
//! `AttributeCommand`).

use vanillite_command::error::CommandError;
use vanillite_command::{Arg as ErrArg, Args};
use vanillite_data::registry::ATTRIBUTE;
use vanillite_game::commands::{AttributeOp, ExperienceOp};
use vanillite_game::level::Level;
use vanillite_protocol::text::{Arg, Component};
use vanillite_world::entity::EntityId;

use super::Env;
use super::items::on_player;
use super::select::{self, Target};
use super::source::CommandSource;
use super::tracker::Tracker;

fn missing(name: &str) -> CommandError {
    CommandError::new(
        "command.exception",
        vec![ErrArg::Str(format!("missing argument {name}"))],
    )
}

fn error(key: &'static str, args: Vec<ErrArg>) -> CommandError {
    CommandError::new(key, args)
}

fn many(
    args: &Args,
    name: &str,
    source: &CommandSource,
    env: &Env<'_>,
) -> Result<Vec<Target>, CommandError> {
    let selector = args.selector(name).ok_or_else(|| missing(name))?;
    select::many(env, source, selector)
}

fn single(
    args: &Args,
    name: &str,
    source: &CommandSource,
    env: &Env<'_>,
) -> Result<Target, CommandError> {
    let selector = args.selector(name).ok_or_else(|| missing(name))?;
    select::single(env, source, selector)
}

// --- experience ---------------------------------------------------------------------------------

/// `giveExperiencePoints` / `giveExperienceLevels` of `ServerPlayer`.
pub(crate) fn add_experience(level: &mut Level, id: EntityId, amount: i32, levels: bool) {
    let Some(player) = level.entities.player_mut(id) else {
        return;
    };
    if amount == 0 {
        return;
    }
    let s = &mut player.survival;
    if levels {
        s.xp.give_levels(amount);
    } else {
        s.score = s.score.wrapping_add(amount);
        s.xp.give_points(amount);
    }
    s.resend_experience();
}

/// `ExperienceCommand.Type.set`: points must stay below the next level.
#[allow(clippy::as_conversions, clippy::cast_precision_loss)]
fn set_experience(level: &mut Level, id: EntityId, amount: i32, levels: bool) -> bool {
    let Some(player) = level.entities.player_mut(id) else {
        return false;
    };
    let s = &mut player.survival;
    if levels {
        if amount != s.xp.level {
            s.xp.level = amount;
            s.resend_experience();
        }
        return true;
    }
    let needed = s.xp.needed_for_next_level();
    if amount >= needed {
        return false;
    }
    let limit = needed as f32;
    let progress = (amount as f32 / limit).clamp(0.0, (limit - 1.0) / limit);
    if progress != s.xp.progress {
        s.xp.progress = progress;
        s.resend_experience();
    }
    true
}

/// `/experience (add|set|query) ...`. Players in other worlds are changed there and count as
/// changed; they cannot be queried.
#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss
)]
pub(crate) fn experience(
    op: ExperienceOp,
    levels: bool,
    args: &Args,
    source: &CommandSource,
    env: &mut Env<'_>,
) -> Result<i32, CommandError> {
    let unit = if levels { "levels" } else { "points" };
    if op == ExperienceOp::Query {
        let target = single(args, "target", source, env)?;
        let player = target
            .entity
            .and_then(|id| env.level.entities.player(id))
            .ok_or_else(|| error("command.failed", vec![]))?;
        let xp = &player.survival.xp;
        let result = if levels {
            xp.level
        } else {
            (xp.progress * xp.needed_for_next_level() as f32).floor() as i32
        };
        source.send_success(
            env.global,
            Component::translatable_args(
                format!("commands.experience.query.{unit}"),
                vec![Arg::from(target.display()), Arg::Int(result)],
            ),
            false,
        );
        return Ok(result);
    }
    let players = many(args, "target", source, env)?;
    let amount = *args.int("amount").ok_or_else(|| missing("amount"))?;
    let mut tracker = Tracker::<Component>::default();
    for player in &players {
        let changed = if op == ExperienceOp::Add {
            on_player(env, player, move |level, id| {
                add_experience(level, id, amount, levels);
            });
            true
        } else {
            match player.entity {
                Some(id) => set_experience(env.level, id, amount, levels),
                None => {
                    on_player(env, player, move |level, id| {
                        set_experience(level, id, amount, levels);
                    });
                    true
                }
            }
        };
        tracker.track(&player.display(), i32::from(changed));
    }
    let (action, on_zero) = match op {
        ExperienceOp::Add => ("add", None),
        _ => (
            "set",
            Some(error("commands.experience.set.points.invalid", vec![])),
        ),
    };
    tracker.feedback(source, env, true, false, on_zero, |one, n, _| match one {
        Some(player) => Component::translatable_args(
            format!("commands.experience.{action}.{unit}.success.single"),
            vec![Arg::Int(amount), Arg::from(player.clone())],
        ),
        None => Component::translatable_args(
            format!("commands.experience.{action}.{unit}.success.multiple"),
            vec![Arg::Int(amount), Arg::Int(n)],
        ),
    })
}

// --- recipe -------------------------------------------------------------------------------------

/// `/recipe (give|take) <targets> (<recipe>|*)`. Players have no recipe book yet, so nothing
/// is awarded or taken and the command fails like Vanilla when nothing changed.
pub(crate) fn recipe(
    give: bool,
    args: &Args,
    source: &CommandSource,
    env: &mut Env<'_>,
) -> Result<i32, CommandError> {
    let players = many(args, "targets", source, env)?;
    let game = env.global.game();
    let ids: Vec<String> = match args.identifier("recipe") {
        Some(id) => {
            if !game.recipes.book.contains(id) {
                return Err(error("recipe.notFound", vec![ErrArg::Str(id.clone())]));
            }
            vec![id.clone()]
        }
        // `*`: every recipe.
        None => game.recipes.book.recipe_ids().map(str::to_owned).collect(),
    };
    let mut tracker = Tracker::<Component>::default();
    for player in &players {
        let changed = match player.entity.and_then(|id| env.level.entities.player_mut(id)) {
            Some(p) => {
                let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
                if give {
                    p.unlock_recipes(&refs)
                } else {
                    p.lock_recipes(&refs)
                }
            }
            None => {
                let list = ids.clone();
                super::items::on_player(env, player, move |level, id| {
                    if let Some(p) = level.entities.player_mut(id) {
                        let refs: Vec<&str> = list.iter().map(String::as_str).collect();
                        if give {
                            p.unlock_recipes(&refs);
                        } else {
                            p.lock_recipes(&refs);
                        }
                    }
                });
                ids.len()
            }
        };
        tracker.track(&player.display(), i32::try_from(changed).unwrap_or(i32::MAX));
    }
    let action = if give { "give" } else { "take" };
    let failed = if give {
        "commands.recipe.give.failed"
    } else {
        "commands.recipe.take.failed"
    };
    tracker.feedback(
        source,
        env,
        true,
        false,
        Some(error(failed, vec![])),
        |one, n, total| match one {
            Some(player) => Component::translatable_args(
                format!("commands.recipe.{action}.success.single"),
                vec![Arg::Int(total), Arg::from(player.clone())],
            ),
            None => Component::translatable_args(
                format!("commands.recipe.{action}.success.multiple"),
                vec![Arg::Int(total), Arg::Int(n)],
            ),
        },
    )
}

// --- attribute ----------------------------------------------------------------------------------

/// `/attribute <target> <attribute> ...`. Entities have no attribute instances yet: the base
/// values are the type's defaults, there are no modifiers, and changes are not supported.
#[allow(clippy::as_conversions, clippy::cast_possible_truncation)]
pub(crate) fn attribute(
    op: AttributeOp,
    args: &Args,
    source: &CommandSource,
    env: &mut Env<'_>,
) -> Result<i32, CommandError> {
    let target = single(args, "target", source, env)?;
    let name = args
        .identifier("attribute")
        .ok_or_else(|| missing("attribute"))?;
    let display = target.display();
    let description = Component::translatable(
        format!("attribute.name.{}", name.rsplit(':').next().unwrap_or("")),
        vec![],
    );
    let defaults = target.kind.info().attributes;
    if defaults.is_empty() {
        return Err(error(
            "commands.attribute.failed.entity",
            vec![ErrArg::Component(Box::new(display))],
        ));
    }
    let Some(base) = ATTRIBUTE
        .id(name)
        .and_then(|id| defaults.iter().find(|(a, _)| *a == id))
        .map(|(_, v)| *v)
    else {
        return Err(error(
            "commands.attribute.failed.no_attribute",
            vec![
                ErrArg::Component(Box::new(display)),
                ErrArg::Component(Box::new(description)),
            ],
        ));
    };
    let send = |key: &str, value: f64| {
        source.send_success(
            env.global,
            Component::translatable_args(
                key,
                vec![
                    Arg::from(description.clone()),
                    Arg::from(display.clone()),
                    Arg::Double(value),
                ],
            ),
            false,
        );
    };
    let scale = args.double("scale").copied().unwrap_or(1.0);
    match op {
        AttributeOp::Get => {
            send("commands.attribute.value.get.success", base);
            Ok((base * scale) as i32)
        }
        AttributeOp::BaseGet => {
            send("commands.attribute.base_value.get.success", base);
            Ok((base * scale) as i32)
        }
        AttributeOp::BaseReset => {
            send("commands.attribute.base_value.reset.success", base);
            Ok(1)
        }
        AttributeOp::ModifierRemove | AttributeOp::ModifierValueGet => {
            let id = args.identifier("id").ok_or_else(|| missing("id"))?;
            Err(error(
                "commands.attribute.failed.no_modifier",
                vec![
                    ErrArg::Component(Box::new(description.clone())),
                    ErrArg::Component(Box::new(display.clone())),
                    ErrArg::Str(id.clone()),
                ],
            ))
        }
        AttributeOp::BaseSet | AttributeOp::ModifierAdd(_) => Err(error("command.failed", vec![])),
    }
}
