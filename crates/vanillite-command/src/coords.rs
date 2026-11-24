//! Coordinates of command arguments: absolute, `~relative` and `^local`, kept unresolved
//! (Vanilla's `WorldCoordinates` and `LocalCoordinates`). The game resolves them against the
//! source's position and rotation.

use crate::error::CommandError;
use crate::reader::StringReader;

/// One world coordinate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorldCoordinate {
    /// Relative to the source (`~`).
    pub relative: bool,
    /// The value (an offset when relative).
    pub value: f64,
}

impl WorldCoordinate {
    /// The coordinate for a source coordinate `origin`.
    pub fn get(&self, origin: f64) -> f64 {
        if self.relative {
            self.value + origin
        } else {
            self.value
        }
    }
}

/// Parsed coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Coordinates {
    /// `x y z` (vec2 and column positions have a relative `y`; rotations hold `x_rot y_rot`
    /// in `x` and `y` order of Vanilla: `[x_rot, y_rot, ~]`).
    World([WorldCoordinate; 3]),
    /// `^left ^up ^forwards`.
    Local([f64; 3]),
}

const ZERO_RELATIVE: WorldCoordinate = WorldCoordinate {
    relative: true,
    value: 0.0,
};

fn mixed(r: &StringReader<'_>) -> CommandError {
    CommandError::new("argument.pos.mixed", vec![]).at(r)
}

fn is_relative(r: &mut StringReader<'_>) -> bool {
    if r.peek_is('~') {
        r.skip();
        true
    } else {
        false
    }
}

/// `WorldCoordinate.parseDouble`.
pub fn parse_double(
    r: &mut StringReader<'_>,
    center: bool,
) -> Result<WorldCoordinate, CommandError> {
    if r.peek_is('^') {
        return Err(mixed(r));
    }
    if !r.can_read() {
        return Err(CommandError::new("argument.pos.missing.double", vec![]).at(r));
    }
    let relative = is_relative(r);
    let start = r.cursor();
    let mut value = if r.can_read() && !r.peek_is(' ') {
        r.read_double()?
    } else {
        0.0
    };
    let number = r.slice(start, r.cursor());
    if relative && number.is_empty() {
        return Ok(ZERO_RELATIVE);
    }
    if !number.contains('.') && !relative && center {
        value += 0.5;
    }
    Ok(WorldCoordinate { relative, value })
}

/// `WorldCoordinate.parseInt`.
pub fn parse_int(r: &mut StringReader<'_>) -> Result<WorldCoordinate, CommandError> {
    if r.peek_is('^') {
        return Err(mixed(r));
    }
    if !r.can_read() {
        return Err(CommandError::new("argument.pos.missing.int", vec![]).at(r));
    }
    let relative = is_relative(r);
    let value = if r.can_read() && !r.peek_is(' ') {
        if relative {
            r.read_double()?
        } else {
            f64::from(r.read_int()?)
        }
    } else {
        0.0
    };
    Ok(WorldCoordinate { relative, value })
}

fn incomplete(r: &mut StringReader<'_>, start: usize, key: &'static str) -> CommandError {
    r.set_cursor(start);
    CommandError::new(key, vec![]).at(r)
}

fn space(r: &mut StringReader<'_>) -> bool {
    if r.peek_is(' ') {
        r.skip();
        true
    } else {
        false
    }
}

fn local_double(r: &mut StringReader<'_>, start: usize) -> Result<f64, CommandError> {
    if !r.can_read() {
        return Err(CommandError::new("argument.pos.missing.double", vec![]).at(r));
    }
    if !r.peek_is('^') {
        r.set_cursor(start);
        return Err(mixed(r));
    }
    r.skip();
    if r.can_read() && !r.peek_is(' ') {
        r.read_double()
    } else {
        Ok(0.0)
    }
}

/// `LocalCoordinates.parse`.
pub fn parse_local(r: &mut StringReader<'_>) -> Result<Coordinates, CommandError> {
    let start = r.cursor();
    let left = local_double(r, start)?;
    if !space(r) {
        return Err(incomplete(r, start, "argument.pos3d.incomplete"));
    }
    let up = local_double(r, start)?;
    if !space(r) {
        return Err(incomplete(r, start, "argument.pos3d.incomplete"));
    }
    let forwards = local_double(r, start)?;
    Ok(Coordinates::Local([left, up, forwards]))
}

/// `Vec3Argument.parse`.
pub fn parse_vec3(r: &mut StringReader<'_>, center: bool) -> Result<Coordinates, CommandError> {
    if r.peek_is('^') {
        return parse_local(r);
    }
    let start = r.cursor();
    let x = parse_double(r, center)?;
    if !space(r) {
        return Err(incomplete(r, start, "argument.pos3d.incomplete"));
    }
    let y = parse_double(r, false)?;
    if !space(r) {
        return Err(incomplete(r, start, "argument.pos3d.incomplete"));
    }
    let z = parse_double(r, center)?;
    Ok(Coordinates::World([x, y, z]))
}

/// `BlockPosArgument.parse`.
pub fn parse_block_pos(r: &mut StringReader<'_>) -> Result<Coordinates, CommandError> {
    if r.peek_is('^') {
        return parse_local(r);
    }
    let start = r.cursor();
    let x = parse_int(r)?;
    if !space(r) {
        return Err(incomplete(r, start, "argument.pos3d.incomplete"));
    }
    let y = parse_int(r)?;
    if !space(r) {
        return Err(incomplete(r, start, "argument.pos3d.incomplete"));
    }
    let z = parse_int(r)?;
    Ok(Coordinates::World([x, y, z]))
}

/// `Vec2Argument.parse`.
pub fn parse_vec2(r: &mut StringReader<'_>, center: bool) -> Result<Coordinates, CommandError> {
    let start = r.cursor();
    if !r.can_read() {
        return Err(CommandError::new("argument.pos2d.incomplete", vec![]).at(r));
    }
    let x = parse_double(r, center)?;
    if !space(r) {
        return Err(incomplete(r, start, "argument.pos2d.incomplete"));
    }
    let z = parse_double(r, center)?;
    Ok(Coordinates::World([x, ZERO_RELATIVE, z]))
}

/// `ColumnPosArgument.parse`.
pub fn parse_column_pos(r: &mut StringReader<'_>) -> Result<Coordinates, CommandError> {
    let start = r.cursor();
    if !r.can_read() {
        return Err(CommandError::new("argument.pos2d.incomplete", vec![]).at(r));
    }
    let x = parse_int(r)?;
    if !space(r) {
        return Err(incomplete(r, start, "argument.pos2d.incomplete"));
    }
    let z = parse_int(r)?;
    Ok(Coordinates::World([x, ZERO_RELATIVE, z]))
}

/// `RotationArgument.parse`: `<y_rot> <x_rot>`, stored like Vanilla as `[x_rot, y_rot, ~]`.
pub fn parse_rotation(r: &mut StringReader<'_>) -> Result<Coordinates, CommandError> {
    let start = r.cursor();
    if !r.can_read() {
        return Err(CommandError::new("argument.rotation.incomplete", vec![]).at(r));
    }
    let y = parse_double(r, false)?;
    if !space(r) {
        return Err(incomplete(r, start, "argument.rotation.incomplete"));
    }
    let x = parse_double(r, false)?;
    Ok(Coordinates::World([x, y, ZERO_RELATIVE]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wc(relative: bool, value: f64) -> WorldCoordinate {
        WorldCoordinate { relative, value }
    }

    #[test]
    fn vec3_centers_whole_numbers() {
        let c = parse_vec3(&mut StringReader::new("1 2 3"), true).ok();
        assert_eq!(
            c,
            Some(Coordinates::World([
                wc(false, 1.5),
                wc(false, 2.0),
                wc(false, 3.5)
            ]))
        );
        let c = parse_vec3(&mut StringReader::new("~ ~1 ~-2.5"), true).ok();
        assert_eq!(
            c,
            Some(Coordinates::World([
                wc(true, 0.0),
                wc(true, 1.0),
                wc(true, -2.5)
            ]))
        );
        let c = parse_vec3(&mut StringReader::new("^ ^1 ^2"), true).ok();
        assert_eq!(c, Some(Coordinates::Local([0.0, 1.0, 2.0])));
    }

    #[test]
    fn vec3_errors() {
        let key = |s: &str| {
            parse_vec3(&mut StringReader::new(s), true)
                .err()
                .map(|e| e.key)
        };
        assert_eq!(key("1 2"), Some("argument.pos3d.incomplete"));
        assert_eq!(key("^1 2 3"), Some("argument.pos.mixed"));
        assert_eq!(key("1 ^2 3"), Some("argument.pos.mixed"));
        assert_eq!(key("x"), Some("parsing.double.expected"));
    }
}
