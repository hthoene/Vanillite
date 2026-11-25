//! `/schedule` (Vanilla's `ScheduleCommand`) and the scheduled functions of the server tick
//! (`TimerQueue.tick` in `ServerLevel.tickTime`).

use vanillite_command::error::CommandError;
use vanillite_command::{Arg as ErrArg, Args, FunctionRef, GameValue};
use vanillite_protocol::text::{Arg, Component};

use super::Env;
use super::functions;
use super::source::CommandSource;
use crate::command_state::TimerCallback;

fn missing(name: &str) -> CommandError {
    CommandError::new(
        "command.exception",
        vec![ErrArg::Str(format!("missing argument {name}"))],
    )
}

/// `schedule function <function> <time> [append|replace]`.
pub(crate) fn schedule(
    args: &Args,
    replace: bool,
    source: &CommandSource,
    env: &mut Env<'_>,
) -> Result<i32, CommandError> {
    let global = env.global;
    let Some(GameValue::Function(FunctionRef { tag, id })) = args.game("function") else {
        return Err(missing("function"));
    };
    // `FunctionArgument.getFunctionOrTag`: unknown functions and tags fail.
    let found = functions::resolve(global, args, "function")?;
    let time = *args.time("time").ok_or_else(|| missing("time"))?;
    if time == 0 {
        return Err(CommandError::new("commands.schedule.same_tick", vec![]));
    }
    let tick_time = global.level.game_time().saturating_add(i64::from(time));
    let (schedule_id, callback, key) = if *tag {
        (
            format!("#{id}"),
            TimerCallback::FunctionTag(id.clone()),
            "commands.schedule.created.tag",
        )
    } else {
        if found.first().is_some_and(|f| f.is_macro()) {
            return Err(CommandError::new("commands.schedule.macro", vec![]));
        }
        (
            id.clone(),
            TimerCallback::Function(id.clone()),
            "commands.schedule.created.function",
        )
    };
    {
        let mut queue = global.systems.scheduled();
        if replace {
            queue.remove(&schedule_id);
        }
        queue.schedule(&schedule_id, tick_time, callback);
    }
    source.send_success(
        global,
        Component::translatable_args(
            key,
            vec![
                Arg::String(id.clone()),
                Arg::Int(time),
                Arg::Long(tick_time),
            ],
        ),
        true,
    );
    // `Math.floorMod(tickTime, Integer.MAX_VALUE)`.
    Ok(i32::try_from(tick_time.rem_euclid(i64::from(i32::MAX))).unwrap_or(0))
}

/// `schedule clear <function>`.
pub(crate) fn clear(
    args: &Args,
    source: &CommandSource,
    env: &mut Env<'_>,
) -> Result<i32, CommandError> {
    let id = args
        .identifier("function")
        .ok_or_else(|| missing("function"))?;
    let count = env.global.systems.scheduled().remove(id);
    if count == 0 {
        return Err(CommandError::new(
            "commands.schedule.cleared.failure",
            vec![ErrArg::Str(id.clone())],
        ));
    }
    let count = i32::try_from(count).unwrap_or(i32::MAX);
    source.send_success(
        env.global,
        Component::translatable_args(
            "commands.schedule.cleared.success",
            vec![Arg::Int(count), Arg::String(id.clone())],
        ),
        true,
    );
    Ok(count)
}

/// `TimerQueue.tick`: runs the events due at the current game time as the game loop sender
/// (`FunctionCallback`, `FunctionTagCallback`). One event at a time, so functions may schedule
/// again.
pub(crate) fn tick(env: &mut Env<'_>) {
    let global = env.global;
    let now = global.level.game_time();
    let Some(sender) = functions::game_loop_sender(global) else {
        return;
    };
    loop {
        let Some(event) = global.systems.scheduled().next_due(now) else {
            break;
        };
        let library = global.systems.functions();
        match &event.callback {
            TimerCallback::Function(id) => {
                if let Some(f) = library.get(id) {
                    functions::run_function(f, &sender, env);
                }
            }
            TimerCallback::FunctionTag(id) => {
                for f in library.tag(id).into_iter().flatten() {
                    functions::run_function(f, &sender, env);
                }
            }
        }
    }
}
