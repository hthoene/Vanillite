//! SNBT, the string form of NBT, following Vanilla's grammar (26.3): typed and unsigned integer
//! suffixes, hex and binary numerals, underscores, escapes, heterogeneous lists, typed arrays and
//! the built-in operations `bool(x)` and `uuid(s)`.
//!
//! The parser is recursive descent with the same depth limit as binary NBT (SEC-04); callers
//! bound the input length.

use crate::{Compound, Error, List, Result, Tag, read::MAX_DEPTH};
use std::fmt::Write as _;

/// Parses one SNBT value. The whole input must be consumed (surrounding whitespace aside).
pub fn parse(input: &str) -> Result<Tag> {
    let mut p = Parser {
        s: input,
        pos: 0,
        depth: 0,
    };
    let tag = p.value()?;
    p.ws();
    if p.pos != p.s.len() {
        return Err(p.err("trailing characters"));
    }
    Ok(tag)
}

/// Parses an SNBT compound.
pub fn parse_compound(input: &str) -> Result<Compound> {
    match parse(input)? {
        Tag::Compound(c) => Ok(c),
        _ => Err(Error::Syntax {
            offset: 0,
            message: "expected a compound",
        }),
    }
}

struct Parser<'a> {
    s: &'a str,
    pos: usize,
    depth: u32,
}

#[derive(Clone, Copy, PartialEq)]
enum Ty {
    Byte,
    Short,
    Int,
    Long,
    Float,
    Double,
}

fn can_start_number(c: char) -> bool {
    matches!(c, '+' | '-' | '.' | '0'..='9')
}

fn is_unquoted(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+')
}

impl Parser<'_> {
    fn err(&self, message: &'static str) -> Error {
        Error::Syntax {
            offset: self.pos,
            message,
        }
    }

    fn rest(&self) -> &str {
        self.s.get(self.pos..).unwrap_or("")
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos = self.pos.saturating_add(c.len_utf8());
        Some(c)
    }

    fn ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.bump();
        }
    }

    fn eat(&mut self, c: char) -> bool {
        self.ws();
        if self.peek() == Some(c) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, c: char, message: &'static str) -> Result<()> {
        if self.eat(c) {
            Ok(())
        } else {
            Err(self.err(message))
        }
    }

    fn eat_ci(&mut self, c: char) -> bool {
        if self.peek().is_some_and(|p| p.eq_ignore_ascii_case(&c)) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn enter(&mut self) -> Result<()> {
        if self.depth >= MAX_DEPTH {
            return Err(Error::DepthExceeded);
        }
        self.depth = self.depth.saturating_add(1);
        Ok(())
    }

    fn value(&mut self) -> Result<Tag> {
        self.ws();
        match self.peek() {
            None => Err(self.err("expected a value")),
            Some(c) if can_start_number(c) => self.number(),
            Some('"' | '\'') => Ok(Tag::String(self.quoted()?)),
            Some('{') => {
                self.enter()?;
                let r = self.compound();
                self.depth = self.depth.saturating_sub(1);
                r.map(Tag::Compound)
            }
            Some('[') => {
                self.enter()?;
                let r = self.list();
                self.depth = self.depth.saturating_sub(1);
                r
            }
            Some(_) => self.unquoted_or_builtin(),
        }
    }

    fn unquoted(&mut self) -> Result<&str> {
        let start = self.pos;
        while self.peek().is_some_and(is_unquoted) {
            self.bump();
        }
        if self.pos == start {
            return Err(self.err("expected an unquoted string"));
        }
        self.s.get(start..self.pos).ok_or(Error::UnexpectedEnd)
    }

    fn unquoted_or_builtin(&mut self) -> Result<Tag> {
        let word = self.unquoted()?.to_owned();
        if self.eat('(') {
            let mut args = Vec::new();
            loop {
                if self.eat(')') {
                    break;
                }
                args.push(self.value()?);
                if !self.eat(',') {
                    self.expect(')', "expected ')'")?;
                    break;
                }
            }
            return match (word.as_str(), args.as_slice()) {
                ("bool", [arg]) => match arg {
                    Tag::Byte(_) | Tag::Short(_) | Tag::Int(_) | Tag::Long(_) => {
                        Ok(Tag::from(arg.as_i64().is_some_and(|v| v != 0)))
                    }
                    Tag::Float(v) => Ok(Tag::from(*v != 0.0)),
                    Tag::Double(v) => Ok(Tag::from(*v != 0.0)),
                    _ => Err(self.err("expected a number or boolean")),
                },
                ("uuid", [Tag::String(s)]) => {
                    let u = parse_uuid(s).ok_or_else(|| self.err("expected a UUID string"))?;
                    Ok(Tag::IntArray(uuid_to_ints(u).to_vec()))
                }
                _ => Err(self.err("no such operation")),
            };
        }
        if word.eq_ignore_ascii_case("true") {
            Ok(Tag::Byte(1))
        } else if word.eq_ignore_ascii_case("false") {
            Ok(Tag::Byte(0))
        } else {
            Ok(Tag::String(word))
        }
    }

    fn compound(&mut self) -> Result<Compound> {
        self.expect('{', "expected '{'")?;
        let mut c = Compound::new();
        loop {
            if self.eat('}') {
                return Ok(c);
            }
            self.ws();
            let key = match self.peek() {
                Some('"' | '\'') => self.quoted()?,
                _ => self.unquoted()?.to_owned(),
            };
            if key.is_empty() {
                return Err(self.err("empty key"));
            }
            self.expect(':', "expected ':'")?;
            let value = self.value()?;
            c.insert(key, value);
            if !self.eat(',') {
                self.expect('}', "expected '}'")?;
                return Ok(c);
            }
        }
    }

    fn list(&mut self) -> Result<Tag> {
        self.expect('[', "expected '['")?;
        self.ws();
        let rest = self.rest();
        let array = match rest.as_bytes() {
            [b'B', b';', ..] => Some(Ty::Byte),
            [b'I', b';', ..] => Some(Ty::Int),
            [b'L', b';', ..] => Some(Ty::Long),
            _ => None,
        };
        if let Some(ty) = array {
            self.pos = self.pos.saturating_add(2);
            return self.array(ty);
        }
        let mut items = Vec::new();
        loop {
            if self.eat(']') {
                return Ok(Tag::List(List(items)));
            }
            items.push(self.value()?);
            if !self.eat(',') {
                self.expect(']', "expected ']'")?;
                return Ok(Tag::List(List(items)));
            }
        }
    }

    fn array(&mut self, ty: Ty) -> Result<Tag> {
        let mut values = Vec::new();
        loop {
            if self.eat(']') {
                break;
            }
            self.ws();
            let lit = self.integer_literal()?;
            if lit.ty.is_some_and(|t| t != ty) {
                return Err(self.err("invalid array element type"));
            }
            values.push(self.integer_value(&lit, ty)?);
            if !self.eat(',') {
                self.expect(']', "expected ']'")?;
                break;
            }
        }
        Ok(match ty {
            Ty::Byte => Tag::ByteArray(
                values
                    .into_iter()
                    .filter_map(|v| v.as_i64())
                    .filter_map(|v| i8::try_from(v).ok())
                    .collect(),
            ),
            Ty::Int => Tag::IntArray(
                values
                    .into_iter()
                    .filter_map(|v| v.as_i64())
                    .filter_map(|v| i32::try_from(v).ok())
                    .collect(),
            ),
            _ => Tag::LongArray(values.into_iter().filter_map(|v| v.as_i64()).collect()),
        })
    }

    fn quoted(&mut self) -> Result<String> {
        let quote = self.bump().ok_or(Error::UnexpectedEnd)?;
        let mut out = String::new();
        loop {
            match self.bump() {
                None => return Err(self.err("unterminated string")),
                Some(c) if c == quote => return Ok(out),
                Some('\\') => out.push(self.escape()?),
                Some(c) => out.push(c),
            }
        }
    }

    fn escape(&mut self) -> Result<char> {
        let c = self.bump().ok_or(Error::UnexpectedEnd)?;
        Ok(match c {
            'b' => '\u{8}',
            's' => ' ',
            't' => '\t',
            'n' => '\n',
            'f' => '\u{c}',
            'r' => '\r',
            '\\' | '\'' | '"' => c,
            'x' => self.hex_escape(2)?,
            'u' => self.hex_escape(4)?,
            'U' => self.hex_escape(8)?,
            _ => return Err(self.err("invalid escape sequence")),
        })
    }

    fn hex_escape(&mut self, len: usize) -> Result<char> {
        let digits = self
            .rest()
            .get(..len)
            .ok_or_else(|| self.err("expected hex escape"))?;
        if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(self.err("expected hex escape"));
        }
        let code = u32::from_str_radix(digits, 16).map_err(|_| self.err("expected hex escape"))?;
        self.pos = self.pos.saturating_add(len);
        char::from_u32(code).ok_or_else(|| self.err("invalid codepoint"))
    }

    fn digits(&mut self, accept: fn(char) -> bool) -> Result<String> {
        let start = self.pos;
        while self.peek().is_some_and(|c| accept(c) || c == '_') {
            self.bump();
        }
        let run = self.s.get(start..self.pos).unwrap_or("");
        if run.is_empty() || run.starts_with('_') || run.ends_with('_') {
            return Err(self.err("invalid numeral"));
        }
        Ok(run.replace('_', ""))
    }

    fn sign(&mut self) -> bool {
        if self.peek() == Some('-') {
            self.bump();
            true
        } else {
            if self.peek() == Some('+') {
                self.bump();
            }
            false
        }
    }

    fn number(&mut self) -> Result<Tag> {
        let start = self.pos;
        if let Some(tag) = self.try_float()? {
            return Ok(tag);
        }
        self.pos = start;
        let lit = self.integer_literal()?;
        let ty = lit.ty.unwrap_or(Ty::Int);
        self.integer_value(&lit, ty)
    }

    /// Floats need a '.', an exponent or an f/d suffix; otherwise this returns `None`.
    fn try_float(&mut self) -> Result<Option<Tag>> {
        let negative = self.sign();
        let mut text = String::from(if negative { "-" } else { "" });
        let whole = if self.peek().is_some_and(|c| c.is_ascii_digit()) {
            Some(self.digits(|c| c.is_ascii_digit())?)
        } else {
            None
        };
        if whole
            .as_deref()
            .is_some_and(|w| w.len() > 1 && w.starts_with('0'))
        {
            return Ok(None);
        }
        let mut is_float = false;
        if let Some(w) = &whole {
            text.push_str(w);
        }
        if self.peek() == Some('.') {
            self.bump();
            is_float = true;
            text.push('.');
            if self.peek().is_some_and(|c| c.is_ascii_digit()) {
                text.push_str(&self.digits(|c| c.is_ascii_digit())?);
            } else if whole.is_none() {
                return Err(self.err("expected decimal numeral"));
            }
        } else if whole.is_none() {
            return Ok(None);
        }
        if self.peek().is_some_and(|c| c == 'e' || c == 'E') {
            self.bump();
            is_float = true;
            text.push('e');
            if self.sign() {
                text.push('-');
            }
            text.push_str(&self.digits(|c| c.is_ascii_digit())?);
        }
        let suffix = if self.eat_ci('f') {
            Some(Ty::Float)
        } else if self.eat_ci('d') {
            Some(Ty::Double)
        } else {
            None
        };
        if !is_float && suffix.is_none() {
            return Ok(None);
        }
        if text.ends_with('.') {
            text.push('0');
        }
        if suffix == Some(Ty::Float) {
            let v: f32 = text.parse().map_err(|_| self.err("number parse failure"))?;
            if !v.is_finite() {
                return Err(self.err("infinity not allowed"));
            }
            Ok(Some(Tag::Float(v)))
        } else {
            let v: f64 = text.parse().map_err(|_| self.err("number parse failure"))?;
            if !v.is_finite() {
                return Err(self.err("infinity not allowed"));
            }
            Ok(Some(Tag::Double(v)))
        }
    }

    fn integer_literal(&mut self) -> Result<IntLiteral> {
        let negative = self.sign();
        let (radix, digits) = if self.peek() == Some('0') {
            self.bump();
            if self.eat_ci('x') {
                (16, self.digits(|c| c.is_ascii_hexdigit())?)
            } else if self.eat_ci('b') && self.peek().is_some_and(|c| c == '0' || c == '1') {
                (2, self.digits(|c| c == '0' || c == '1')?)
            } else {
                // A lone "0b" is zero with a byte suffix: step back over the 'b'.
                if self
                    .s
                    .get(..self.pos)
                    .is_some_and(|s| s.ends_with(['b', 'B']))
                {
                    self.pos = self.pos.saturating_sub(1);
                }
                if self.peek().is_some_and(|c| c.is_ascii_digit()) {
                    return Err(self.err("leading zero not allowed"));
                }
                (10, String::from("0"))
            }
        } else {
            (10, self.digits(|c| c.is_ascii_digit())?)
        };
        let mut signed = None;
        if self.eat_ci('u') {
            signed = Some(false);
        } else if self.peek().is_some_and(|c| c == 's' || c == 'S')
            && self
                .rest()
                .chars()
                .nth(1)
                .is_some_and(|c| "bsilBSIL".contains(c))
        {
            self.bump();
            signed = Some(true);
        }
        let ty = if self.eat_ci('b') {
            Some(Ty::Byte)
        } else if self.eat_ci('s') {
            Some(Ty::Short)
        } else if self.eat_ci('i') {
            Some(Ty::Int)
        } else if self.eat_ci('l') {
            Some(Ty::Long)
        } else {
            None
        };
        if signed.is_some() && ty.is_none() {
            return Err(self.err("expected integer type"));
        }
        let signed = signed.unwrap_or(radix == 10);
        Ok(IntLiteral {
            negative,
            radix,
            digits,
            signed,
            ty,
        })
    }

    fn integer_value(&self, lit: &IntLiteral, ty: Ty) -> Result<Tag> {
        if !lit.signed && lit.negative {
            return Err(self.err("expected non-negative number"));
        }
        let bad = || self.err("number parse failure");
        let magnitude = u128::from_str_radix(&lit.digits, lit.radix).map_err(|_| bad())?;
        let bits: u32 = match ty {
            Ty::Byte => 8,
            Ty::Short => 16,
            Ty::Int => 32,
            Ty::Long => 64,
            _ => return Err(self.err("expected integer type")),
        };
        let value: i128 = if lit.signed {
            let m = i128::try_from(magnitude).map_err(|_| bad())?;
            let v = if lit.negative {
                m.checked_neg().ok_or_else(bad)?
            } else {
                m
            };
            let min = 1i128
                .checked_shl(bits.saturating_sub(1))
                .map(|x| x.saturating_neg())
                .ok_or_else(bad)?;
            let max = min
                .checked_neg()
                .and_then(|x| x.checked_sub(1))
                .ok_or_else(bad)?;
            if v < min || v > max {
                return Err(bad());
            }
            v
        } else {
            let limit = 1u128.checked_shl(bits).ok_or_else(bad)?;
            if magnitude >= limit {
                return Err(bad());
            }
            // Reinterpret the unsigned value as the signed type of the same width.
            let half = limit.checked_div(2).ok_or_else(bad)?;
            let m = i128::try_from(magnitude).map_err(|_| bad())?;
            if magnitude >= half {
                m.checked_sub(i128::try_from(limit).map_err(|_| bad())?)
                    .ok_or_else(bad)?
            } else {
                m
            }
        };
        Ok(match ty {
            Ty::Byte => Tag::Byte(i8::try_from(value).map_err(|_| bad())?),
            Ty::Short => Tag::Short(i16::try_from(value).map_err(|_| bad())?),
            Ty::Int => Tag::Int(i32::try_from(value).map_err(|_| bad())?),
            _ => Tag::Long(i64::try_from(value).map_err(|_| bad())?),
        })
    }
}

struct IntLiteral {
    negative: bool,
    radix: u32,
    digits: String,
    signed: bool,
    ty: Option<Ty>,
}

fn parse_uuid(s: &str) -> Option<u128> {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 5 || parts.iter().any(|p| p.is_empty() || p.len() > 16) {
        return None;
    }
    // Java's UUID.fromString: five hex groups of at most 8-4-4-4-12 significant digits.
    let widths = [32u32, 16, 16, 16, 48];
    let mut value: u128 = 0;
    for (part, width) in parts.iter().zip(widths) {
        let v = u64::from_str_radix(part, 16).ok()?;
        if width < 64 && v >> width != 0 {
            return None;
        }
        value = value.checked_shl(width)? | u128::from(v);
    }
    Some(value)
}

/// Vanilla's UUID layout as four ints, most significant first.
pub fn uuid_to_ints(u: u128) -> [i32; 4] {
    let b = u.to_be_bytes();
    let int = |i: usize| -> i32 {
        let s = b
            .get(i..i.saturating_add(4))
            .and_then(|s| <[u8; 4]>::try_from(s).ok())
            .unwrap_or_default();
        i32::from_be_bytes(s)
    };
    [int(0), int(4), int(8), int(12)]
}

/// Formats a value as compact SNBT like Vanilla's `Tag.toString()`. Floating values use Rust's
/// shortest round-trip form, which can differ from Java's in exponent notation.
pub fn to_string(tag: &Tag) -> String {
    let mut out = String::new();
    write_tag(tag, &mut out);
    out
}

fn write_tag(tag: &Tag, out: &mut String) {
    // `write!` into a String cannot fail.
    let _ = match tag {
        Tag::Byte(v) => write!(out, "{v}b"),
        Tag::Short(v) => write!(out, "{v}s"),
        Tag::Int(v) => write!(out, "{v}"),
        Tag::Long(v) => write!(out, "{v}L"),
        Tag::Float(v) => write!(out, "{v:?}f"),
        Tag::Double(v) => write!(out, "{v:?}d"),
        Tag::String(s) => {
            quote(s, out);
            Ok(())
        }
        Tag::ByteArray(v) => write_array(out, "B", v.iter().map(|x| format!("{x}B"))),
        Tag::IntArray(v) => write_array(out, "I", v.iter().map(i32::to_string)),
        Tag::LongArray(v) => write_array(out, "L", v.iter().map(|x| format!("{x}L"))),
        Tag::List(l) => {
            out.push('[');
            for (i, item) in l.0.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_tag(item, out);
            }
            out.push(']');
            Ok(())
        }
        Tag::Compound(c) => {
            out.push('{');
            for (i, (k, v)) in c.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                if is_simple_key(k) {
                    out.push_str(k);
                } else {
                    quote(k, out);
                }
                out.push(':');
                write_tag(v, out);
            }
            out.push('}');
            Ok(())
        }
    };
}

fn write_array(
    out: &mut String,
    prefix: &str,
    items: impl Iterator<Item = String>,
) -> std::fmt::Result {
    out.push('[');
    out.push_str(prefix);
    out.push(';');
    for (i, item) in items.enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&item);
    }
    out.push(']');
    Ok(())
}

fn is_simple_key(k: &str) -> bool {
    let mut chars = k.chars();
    let starts = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '.' || c == '_');
    starts
        && k.chars().all(is_unquoted)
        && !k.eq_ignore_ascii_case("true")
        && !k.eq_ignore_ascii_case("false")
}

/// Vanilla's `StringTag.quoteAndEscape`: prefers double quotes unless the text contains one first.
fn quote(s: &str, out: &mut String) {
    let quote = match s.chars().find(|c| *c == '"' || *c == '\'') {
        Some('"') => '\'',
        _ => '"',
    };
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            c if c < ' ' => {
                let _ = write!(out, "\\x{:02X}", u32::from(c));
            }
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push(quote);
}
