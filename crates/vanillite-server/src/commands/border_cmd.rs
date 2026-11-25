//! `worldborder` (`WorldBorderCommand`) on the border of the world the command runs on; every
//! change goes to that world's players like `PlayerList.addWorldborderListener`.

use vanillite_command::error::CommandError;
use vanillite_command::{Arg as ErrArg, Args};
use vanillite_game::commands::WorldBorderOp;
use vanillite_game::level::border::{MAX_CENTER_COORDINATE, MAX_SIZE};
use vanillite_protocol::Packet;
use vanillite_protocol::packets::play::clientbound::{
    SetBorderCenter, SetBorderLerpSize, SetBorderSize, SetBorderWarningDelay,
    SetBorderWarningDistance,
};
use vanillite_protocol::text::{Arg, Component};

use super::Env;
use super::blocks_cmd::{missing, this_world};
use super::source::CommandSource;

/// `String.format(Locale.ROOT, "%.<digits>f", v)`: Java rounds the shortest decimal form of
/// the value half up.
pub(super) fn java_fixed(v: f64, digits: usize) -> String {
    if v.is_nan() {
        return "NaN".to_owned();
    }
    if v.is_infinite() {
        return if v > 0.0 { "Infinity" } else { "-Infinity" }.to_owned();
    }
    let text = format!("{}", v.abs());
    let (int, frac) = text.split_once('.').unwrap_or((text.as_str(), ""));
    let mut number: Vec<u8> = int
        .bytes()
        .chain(frac.bytes().chain(std::iter::repeat(b'0')).take(digits))
        .map(|b| b.saturating_sub(b'0'))
        .collect();
    if frac.as_bytes().get(digits).is_some_and(|&d| d >= b'5') {
        let mut carry = true;
        for d in number.iter_mut().rev() {
            if *d == 9 {
                *d = 0;
            } else {
                *d = d.saturating_add(1);
                carry = false;
                break;
            }
        }
        if carry {
            number.insert(0, 1);
        }
    }
    let split = number.len().saturating_sub(digits);
    let digit = |d: &u8| char::from(d.saturating_add(b'0'));
    let mut out = String::new();
    if v.is_sign_negative() {
        out.push('-');
    }
    out.extend(number.get(..split).unwrap_or_default().iter().map(digit));
    if digits > 0 {
        out.push('.');
        out.extend(number.get(split..).unwrap_or_default().iter().map(digit));
    }
    out
}

/// `formatTicksToSeconds`.
fn seconds(ticks: i64) -> String {
    #[allow(clippy::cast_precision_loss, clippy::as_conversions)]
    java_fixed(ticks as f64 / 20.0, 2)
}

fn error(key: &'static str) -> CommandError {
    CommandError::new(key, vec![])
}

/// Java's `(int)` of a double: saturating, NaN is 0.
#[allow(clippy::cast_possible_truncation, clippy::as_conversions)]
fn int(v: f64) -> i32 {
    v as i32
}

fn broadcast(env: &Env<'_>, packet: &impl Packet) {
    crate::border::broadcast_world(env.level.entities.players.iter(), packet);
}

fn success(source: &CommandSource, env: &Env<'_>, key: &str, args: Vec<Arg>) {
    source.send_success(env.global, Component::translatable_args(key, args), true);
}

/// `WorldBorderCommand.setSize`.
fn set_size(
    source: &CommandSource,
    env: &mut Env<'_>,
    distance: f64,
    ticks: i64,
) -> Result<i32, CommandError> {
    let current = env.level.border.size();
    if current == distance {
        return Err(error("commands.worldborder.set.failed.nochange"));
    }
    if distance < 1.0 {
        return Err(error("commands.worldborder.set.failed.small"));
    }
    if distance > MAX_SIZE {
        #[allow(clippy::cast_possible_truncation, clippy::as_conversions)]
        return Err(CommandError::new(
            "commands.worldborder.set.failed.big",
            vec![ErrArg::Float(MAX_SIZE as f32)],
        ));
    }
    let formatted = Arg::String(java_fixed(distance, 1));
    if ticks > 0 {
        let game_time = env.level.game_time();
        let border = &mut env.level.border;
        border.lerp_size_between(current, distance, ticks, game_time);
        let packet = SetBorderLerpSize {
            old_size: border.size(),
            new_size: border.lerp_target(),
            lerp_time: border.lerp_time(),
        };
        broadcast(env, &packet);
        let key = if distance > current {
            "commands.worldborder.set.grow"
        } else {
            "commands.worldborder.set.shrink"
        };
        success(
            source,
            env,
            key,
            vec![formatted, Arg::String(seconds(ticks))],
        );
    } else {
        env.level.border.set_size(distance);
        let size = env.level.border.lerp_target();
        broadcast(env, &SetBorderSize { size });
        success(
            source,
            env,
            "commands.worldborder.set.immediate",
            vec![formatted],
        );
    }
    Ok(int(distance - current))
}

/// Runs a `worldborder` subcommand.
pub(super) fn run(
    op: WorldBorderOp,
    args: &Args,
    source: &CommandSource,
    env: &mut Env<'_>,
) -> Result<i32, CommandError> {
    this_world(source, env)?;
    let double = |name: &str| args.double(name).copied().ok_or_else(|| missing(name));
    let float = |name: &str| args.float(name).copied().ok_or_else(|| missing(name));
    let time = args.time("time").copied();
    match op {
        WorldBorderOp::Add => {
            let border = &env.level.border;
            let distance = border.size() + double("distance")?;
            let ticks = time.map_or(0, |t| border.lerp_time().saturating_add(i64::from(t)));
            set_size(source, env, distance, ticks)
        }
        WorldBorderOp::Set => {
            let distance = double("distance")?;
            set_size(source, env, distance, time.map_or(0, i64::from))
        }
        WorldBorderOp::Center => {
            let c = args.coordinates("pos").ok_or_else(|| missing("pos"))?;
            let pos = super::exec::resolve_position(source, c);
            // `Vec2Argument.getVec2`: floats.
            #[allow(clippy::cast_possible_truncation, clippy::as_conversions)]
            let (x, z) = (pos.x as f32, pos.z as f32);
            let (x, z) = (f64::from(x), f64::from(z));
            let border = &mut env.level.border;
            if border.center_x() == x && border.center_z() == z {
                return Err(error("commands.worldborder.center.failed"));
            }
            if x.abs() > MAX_CENTER_COORDINATE || z.abs() > MAX_CENTER_COORDINATE {
                return Err(CommandError::new(
                    "commands.worldborder.set.failed.far",
                    vec![ErrArg::Double(MAX_CENTER_COORDINATE)],
                ));
            }
            border.set_center(x, z);
            broadcast(
                env,
                &SetBorderCenter {
                    center_x: x,
                    center_z: z,
                },
            );
            success(
                source,
                env,
                "commands.worldborder.center.success",
                vec![Arg::String(java_fixed(x, 2)), Arg::String(java_fixed(z, 2))],
            );
            Ok(0)
        }
        WorldBorderOp::DamageAmount => {
            let amount = float("damagePerBlock")?;
            let border = &mut env.level.border;
            if border.damage_per_block() == f64::from(amount) {
                return Err(error("commands.worldborder.damage.amount.failed"));
            }
            border.set_damage_per_block(f64::from(amount));
            success(
                source,
                env,
                "commands.worldborder.damage.amount.success",
                vec![Arg::String(java_fixed(f64::from(amount), 2))],
            );
            Ok(int(f64::from(amount)))
        }
        WorldBorderOp::DamageBuffer => {
            let distance = float("distance")?;
            let border = &mut env.level.border;
            if border.safe_zone() == f64::from(distance) {
                return Err(error("commands.worldborder.damage.buffer.failed"));
            }
            border.set_safe_zone(f64::from(distance));
            success(
                source,
                env,
                "commands.worldborder.damage.buffer.success",
                vec![Arg::String(java_fixed(f64::from(distance), 2))],
            );
            Ok(int(f64::from(distance)))
        }
        WorldBorderOp::Get => {
            let size = env.level.border.size();
            source.send_success(
                env.global,
                Component::translatable_args(
                    "commands.worldborder.get",
                    vec![Arg::String(java_fixed(size, 0))],
                ),
                false,
            );
            Ok(vanillite_base::math::floor(size + 0.5))
        }
        WorldBorderOp::WarningDistance => {
            let distance = args
                .int("distance")
                .copied()
                .ok_or_else(|| missing("distance"))?;
            let border = &mut env.level.border;
            if border.warning_blocks() == distance {
                return Err(error("commands.worldborder.warning.distance.failed"));
            }
            border.set_warning_blocks(distance);
            broadcast(
                env,
                &SetBorderWarningDistance {
                    warning_blocks: distance,
                },
            );
            success(
                source,
                env,
                "commands.worldborder.warning.distance.success",
                vec![Arg::Int(distance)],
            );
            Ok(distance)
        }
        WorldBorderOp::WarningTime => {
            let ticks = time.ok_or_else(|| missing("time"))?;
            let border = &mut env.level.border;
            if border.warning_time() == ticks {
                return Err(error("commands.worldborder.warning.time.failed"));
            }
            border.set_warning_time(ticks);
            broadcast(
                env,
                &SetBorderWarningDelay {
                    warning_delay: ticks,
                },
            );
            success(
                source,
                env,
                "commands.worldborder.warning.time.success",
                vec![Arg::String(seconds(i64::from(ticks)))],
            );
            Ok(ticks)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::java_fixed;

    #[test]
    fn formats_like_java() {
        assert_eq!(java_fixed(0.125, 2), "0.13");
        assert_eq!(java_fixed(2.675, 2), "2.68");
        assert_eq!(java_fixed(59_999_968.0, 0), "59999968");
        assert_eq!(java_fixed(9.96, 1), "10.0");
        assert_eq!(java_fixed(-1.5, 1), "-1.5");
        assert_eq!(java_fixed(f64::from(0.2_f32), 2), "0.20");
    }
}
