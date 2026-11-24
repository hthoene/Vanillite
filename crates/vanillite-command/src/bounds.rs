//! Number ranges `1..5`, `..5`, `1..` and `3` (Vanilla's `MinMaxBounds`).

use crate::error::{Arg, CommandError};
use crate::reader::StringReader;

/// A range of numbers; `None` is open. A single number has `min == max`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Bounds<T> {
    /// Lower bound.
    pub min: Option<T>,
    /// Upper bound.
    pub max: Option<T>,
}

/// An int range (`MinMaxBounds.Ints`).
pub type IntRange = Bounds<i32>;
/// A double range (`MinMaxBounds.Doubles`).
pub type DoubleRange = Bounds<f64>;
/// A float range of degrees (`MinMaxBounds.FloatDegrees`).
pub type FloatRange = Bounds<f32>;

impl<T: PartialOrd + Copy> Bounds<T> {
    /// Whether `v` lies inside.
    pub fn matches(&self, v: T) -> bool {
        self.min.is_none_or(|m| m <= v) && self.max.is_none_or(|m| v <= m)
    }

    fn swapped(&self) -> bool {
        matches!((self.min, self.max), (Some(a), Some(b)) if a > b)
    }
}

fn allowed_char(r: &StringReader<'_>) -> bool {
    match r.peek() {
        Some(c) if c.is_ascii_digit() || c == '-' => true,
        Some('.') => r.peek_at(1) != Some('.'),
        _ => false,
    }
}

fn read_bound<T: std::str::FromStr>(
    r: &mut StringReader<'_>,
    invalid: &'static str,
) -> Result<Option<T>, CommandError> {
    let start = r.cursor();
    while r.can_read() && allowed_char(r) {
        r.skip();
    }
    let number = r.slice(start, r.cursor());
    if number.is_empty() {
        return Ok(None);
    }
    number
        .parse::<T>()
        .map(Some)
        .map_err(|_| CommandError::new(invalid, vec![Arg::from(number)]).at(r))
}

fn read_bounds<T: std::str::FromStr + PartialOrd + Copy>(
    r: &mut StringReader<'_>,
    invalid: &'static str,
) -> Result<Bounds<T>, CommandError> {
    if !r.can_read() {
        return Err(CommandError::new("argument.range.empty", vec![]).at(r));
    }
    let start = r.cursor();
    let result = (|| {
        let min = read_bound::<T>(r, invalid)?;
        let max = if r.peek_is('.') && r.peek_at(1) == Some('.') {
            r.skip();
            r.skip();
            read_bound::<T>(r, invalid)?
        } else {
            min
        };
        if min.is_none() && max.is_none() {
            return Err(CommandError::new("argument.range.empty", vec![]).at(r));
        }
        Ok(Bounds { min, max })
    })();
    result.map_err(|mut e| {
        r.set_cursor(start);
        e.cursor = Some(start);
        e
    })
}

fn checked<T: PartialOrd + Copy>(
    r: &mut StringReader<'_>,
    start: usize,
    b: Bounds<T>,
) -> Result<Bounds<T>, CommandError> {
    if b.swapped() {
        r.set_cursor(start);
        return Err(CommandError::new("argument.range.swapped", vec![]).at(r));
    }
    Ok(b)
}

/// `MinMaxBounds.Ints.fromReader`.
pub fn read_int_range(r: &mut StringReader<'_>) -> Result<IntRange, CommandError> {
    let start = r.cursor();
    let b = read_bounds::<i32>(r, "parsing.int.invalid")?;
    checked(r, start, b)
}

/// `MinMaxBounds.Doubles.fromReader`.
pub fn read_double_range(r: &mut StringReader<'_>) -> Result<DoubleRange, CommandError> {
    let start = r.cursor();
    let b = read_bounds::<f64>(r, "parsing.double.invalid")?;
    checked(r, start, b)
}

/// `MinMaxBounds.FloatDegrees.fromReader` (no swap check: degree ranges may wrap).
pub fn read_float_range(r: &mut StringReader<'_>) -> Result<FloatRange, CommandError> {
    read_bounds::<f32>(r, "parsing.float.invalid")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        let p = |s: &str| read_int_range(&mut StringReader::new(s));
        assert_eq!(
            p("3").ok(),
            Some(Bounds {
                min: Some(3),
                max: Some(3)
            })
        );
        assert_eq!(
            p("1..5").ok(),
            Some(Bounds {
                min: Some(1),
                max: Some(5)
            })
        );
        assert_eq!(
            p("..5").ok(),
            Some(Bounds {
                min: None,
                max: Some(5)
            })
        );
        assert_eq!(
            p("-2..").ok(),
            Some(Bounds {
                min: Some(-2),
                max: None
            })
        );
        assert_eq!(
            p("5..1").err().map(|e| e.key),
            Some("argument.range.swapped")
        );
        assert_eq!(p("..").err().map(|e| e.key), Some("argument.range.empty"));
        let d = read_double_range(&mut StringReader::new("0.5..2.")).ok();
        assert_eq!(
            d,
            Some(Bounds {
                min: Some(0.5),
                max: Some(2.0)
            })
        );
    }
}
