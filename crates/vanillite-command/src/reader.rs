//! Brigadier's `StringReader`: a cursor over the command line with its token readers.
//!
//! The cursor is a byte offset into the input; it always sits on a character boundary.

use crate::error::{Arg, CommandError};

/// A cursor over a command line.
#[derive(Debug, Clone)]
pub struct StringReader<'a> {
    input: &'a str,
    cursor: usize,
}

/// Characters of Brigadier's number tokens.
pub fn is_allowed_number(c: char) -> bool {
    c.is_ascii_digit() || c == '.' || c == '-'
}

/// Characters of Brigadier's unquoted strings.
pub fn is_allowed_in_unquoted_string(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+')
}

/// Whether `c` starts a quoted string.
pub fn is_quoted_string_start(c: char) -> bool {
    c == '"' || c == '\''
}

impl<'a> StringReader<'a> {
    /// A reader at the start of `input`.
    pub fn new(input: &'a str) -> Self {
        Self { input, cursor: 0 }
    }

    /// The whole input.
    pub fn string(&self) -> &'a str {
        self.input
    }

    /// The cursor (byte offset).
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Moves the cursor; offsets inside a character or past the end are clamped back to the
    /// previous boundary.
    pub fn set_cursor(&mut self, cursor: usize) {
        let mut c = cursor.min(self.input.len());
        while c > 0 && !self.input.is_char_boundary(c) {
            c = c.saturating_sub(1);
        }
        self.cursor = c;
    }

    /// The input from the cursor on.
    pub fn remaining(&self) -> &'a str {
        self.input.get(self.cursor..).unwrap_or("")
    }

    /// The input before the cursor.
    pub fn read_part(&self) -> &'a str {
        self.input.get(..self.cursor).unwrap_or("")
    }

    /// The input between two byte offsets.
    pub fn slice(&self, start: usize, end: usize) -> &'a str {
        self.input.get(start..end).unwrap_or("")
    }

    /// Whether at least one character is left.
    pub fn can_read(&self) -> bool {
        self.cursor < self.input.len()
    }

    /// Whether at least `n` characters are left.
    pub fn can_read_n(&self, n: usize) -> bool {
        match n.checked_sub(1) {
            None => true,
            Some(i) => self.remaining().chars().nth(i).is_some(),
        }
    }

    /// The next character.
    pub fn peek(&self) -> Option<char> {
        self.remaining().chars().next()
    }

    /// The character `offset` characters after the cursor.
    pub fn peek_at(&self, offset: usize) -> Option<char> {
        self.remaining().chars().nth(offset)
    }

    /// Whether the next character is `c`.
    pub fn peek_is(&self, c: char) -> bool {
        self.peek() == Some(c)
    }

    /// Reads one character.
    pub fn read(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.cursor = self.cursor.saturating_add(c.len_utf8());
        Some(c)
    }

    /// Skips one character.
    pub fn skip(&mut self) {
        self.read();
    }

    /// Skips Java whitespace.
    pub fn skip_whitespace(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.skip();
        }
    }

    /// Consumes characters while `f` holds and returns them.
    pub fn read_while(&mut self, f: impl Fn(char) -> bool) -> &'a str {
        let start = self.cursor;
        while self.peek().is_some_and(&f) {
            self.skip();
        }
        self.slice(start, self.cursor)
    }

    fn read_number<T: std::str::FromStr>(
        &mut self,
        expected: &'static str,
        invalid: &'static str,
    ) -> Result<T, CommandError> {
        let start = self.cursor;
        let number = self.read_while(is_allowed_number);
        if number.is_empty() {
            return Err(CommandError::new(expected, vec![]).at(self));
        }
        match number.parse::<T>() {
            Ok(v) => Ok(v),
            Err(_) => {
                self.cursor = start;
                Err(CommandError::new(invalid, vec![Arg::from(number)]).at(self))
            }
        }
    }

    /// `readInt`.
    pub fn read_int(&mut self) -> Result<i32, CommandError> {
        self.read_number("parsing.int.expected", "parsing.int.invalid")
    }

    /// `readLong`.
    pub fn read_long(&mut self) -> Result<i64, CommandError> {
        self.read_number("parsing.long.expected", "parsing.long.invalid")
    }

    /// `readDouble`.
    pub fn read_double(&mut self) -> Result<f64, CommandError> {
        self.read_number("parsing.double.expected", "parsing.double.invalid")
    }

    /// `readFloat`.
    pub fn read_float(&mut self) -> Result<f32, CommandError> {
        self.read_number("parsing.float.expected", "parsing.float.invalid")
    }

    /// `readUnquotedString`.
    pub fn read_unquoted_string(&mut self) -> &'a str {
        self.read_while(is_allowed_in_unquoted_string)
    }

    /// `readQuotedString`: a string in `"` or `'` with `\` escapes of the quote and `\`.
    pub fn read_quoted_string(&mut self) -> Result<String, CommandError> {
        let Some(next) = self.peek() else {
            return Ok(String::new());
        };
        if !is_quoted_string_start(next) {
            return Err(CommandError::new("parsing.quote.expected.start", vec![]).at(self));
        }
        self.skip();
        self.read_string_until(next)
    }

    /// `readStringUntil`: reads up to the unescaped `terminator` and consumes it.
    pub fn read_string_until(&mut self, terminator: char) -> Result<String, CommandError> {
        let mut result = String::new();
        let mut escaped = false;
        while let Some(c) = self.read() {
            if escaped {
                if c == terminator || c == '\\' {
                    result.push(c);
                    escaped = false;
                } else {
                    self.cursor = self.cursor.saturating_sub(c.len_utf8());
                    return Err(CommandError::new(
                        "parsing.quote.escape",
                        vec![Arg::from(c.to_string())],
                    )
                    .at(self));
                }
            } else if c == '\\' {
                escaped = true;
            } else if c == terminator {
                return Ok(result);
            } else {
                result.push(c);
            }
        }
        Err(CommandError::new("parsing.quote.expected.end", vec![]).at(self))
    }

    /// `readString`: a quoted or unquoted string.
    pub fn read_string(&mut self) -> Result<String, CommandError> {
        match self.peek() {
            None => Ok(String::new()),
            Some(c) if is_quoted_string_start(c) => {
                self.skip();
                self.read_string_until(c)
            }
            Some(_) => Ok(self.read_unquoted_string().to_owned()),
        }
    }

    /// `readBoolean`.
    pub fn read_boolean(&mut self) -> Result<bool, CommandError> {
        let start = self.cursor;
        let value = self.read_string()?;
        if value.is_empty() {
            return Err(CommandError::new("parsing.bool.expected", vec![]).at(self));
        }
        match value.as_str() {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => {
                self.cursor = start;
                Err(CommandError::new("parsing.bool.invalid", vec![Arg::from(value)]).at(self))
            }
        }
    }

    /// `expect`: consumes `c` or fails with `parsing.expected`.
    pub fn expect(&mut self, c: char) -> Result<(), CommandError> {
        if self.peek_is(c) {
            self.skip();
            Ok(())
        } else {
            Err(CommandError::new("parsing.expected", vec![Arg::from(c.to_string())]).at(self))
        }
    }
}

/// Length of `s` in UTF-16 code units (Java string indices, as the client counts).
pub fn utf16_len(s: &str) -> usize {
    s.chars()
        .map(char::len_utf16)
        .fold(0, usize::saturating_add)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_tokens_like_brigadier() {
        let mut r = StringReader::new("12 -3.5 word \"a \\\"q\\\" b\" 'x' true");
        assert_eq!(r.read_int().ok(), Some(12));
        r.skip();
        assert_eq!(r.read_double().ok(), Some(-3.5));
        r.skip();
        assert_eq!(r.read_unquoted_string(), "word");
        r.skip();
        assert_eq!(r.read_string().ok().as_deref(), Some("a \"q\" b"));
        r.skip();
        assert_eq!(r.read_quoted_string().ok().as_deref(), Some("x"));
        r.skip();
        assert_eq!(r.read_boolean().ok(), Some(true));
        assert!(!r.can_read());
    }

    #[test]
    fn errors_carry_key_and_cursor() {
        let mut r = StringReader::new("1.5.6");
        let e = r.read_int().err();
        assert_eq!(e.as_ref().map(|e| e.key), Some("parsing.int.invalid"));
        assert_eq!(e.and_then(|e| e.cursor), Some(0));
        let mut r = StringReader::new("\"open");
        let e = r.read_string().err();
        assert_eq!(e.map(|e| e.key), Some("parsing.quote.expected.end"));
        let mut r = StringReader::new("\"bad\\n\"");
        let e = r.read_string().err();
        assert_eq!(e.as_ref().map(|e| e.key), Some("parsing.quote.escape"));
        assert_eq!(e.and_then(|e| e.cursor), Some(5));
    }

    #[test]
    fn huge_numbers_fail_cleanly() {
        let digits = "9".repeat(10_000);
        assert!(StringReader::new(&digits).read_int().is_err());
        assert!(StringReader::new(&digits).read_long().is_err());
        assert!(StringReader::new(&digits).read_double().is_ok());
    }
}
