//! Command errors: Vanilla's `CommandSyntaxException` as data. The game renders them: the
//! message, and for errors with a position the context line (`Commands.finishParsing`).

use vanillite_protocol::text::Component;

use crate::reader::StringReader;

/// An argument of an error message (Vanilla's `translatableEscape` keeps numbers and strings
/// as primitives).
#[derive(Debug, Clone, PartialEq)]
pub enum Arg {
    /// A string.
    Str(String),
    /// An int.
    Int(i32),
    /// A long.
    Long(i64),
    /// A float.
    Float(f32),
    /// A double.
    Double(f64),
    /// A component.
    Component(Box<Component>),
}

impl From<&str> for Arg {
    fn from(s: &str) -> Self {
        Self::Str(s.to_owned())
    }
}

impl From<String> for Arg {
    fn from(s: String) -> Self {
        Self::Str(s)
    }
}

impl From<i32> for Arg {
    fn from(v: i32) -> Self {
        Self::Int(v)
    }
}

impl From<i64> for Arg {
    fn from(v: i64) -> Self {
        Self::Long(v)
    }
}

impl From<f32> for Arg {
    fn from(v: f32) -> Self {
        Self::Float(v)
    }
}

impl From<f64> for Arg {
    fn from(v: f64) -> Self {
        Self::Double(v)
    }
}

/// A failed parse or command: a translation key with arguments and, for syntax errors, the
/// input and the position of the error.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandError {
    /// Translation key, e.g. `command.unknown.command`.
    pub key: &'static str,
    /// Arguments of the translation.
    pub args: Vec<Arg>,
    /// The command line the error refers to.
    pub input: Option<String>,
    /// Byte offset of the error in `input`.
    pub cursor: Option<usize>,
}

/// The context line of a syntax error, split like Vanilla: up to ten characters before the
/// error (after an ellipsis when more precede) and the rest of the input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorContext {
    /// More than ten characters precede the error (Vanilla prints `...`).
    pub ellipsis: bool,
    /// Up to ten characters before the error.
    pub before: String,
    /// The input from the error on (Vanilla shows it red and underlined).
    pub after: String,
}

impl CommandError {
    /// An error without position.
    pub fn new(key: &'static str, args: Vec<Arg>) -> Self {
        Self {
            key,
            args,
            input: None,
            cursor: None,
        }
    }

    /// Attaches the reader's input and cursor (`createWithContext`).
    #[must_use]
    pub fn at(mut self, reader: &StringReader<'_>) -> Self {
        self.input = Some(reader.string().to_owned());
        self.cursor = Some(reader.cursor());
        self
    }

    /// Attaches an input and a cursor.
    #[must_use]
    pub fn at_position(mut self, input: &str, cursor: usize) -> Self {
        self.input = Some(input.to_owned());
        self.cursor = Some(cursor);
        self
    }

    /// `command.unknown.command` at the reader.
    pub fn unknown_command(reader: &StringReader<'_>) -> Self {
        Self::new("command.unknown.command", vec![]).at(reader)
    }

    /// The message with plain components for its arguments. Numbers become literal text here;
    /// renderers that need Vanilla's exact primitive arguments use [`CommandError::args`].
    pub fn message(&self) -> Component {
        let args = self
            .args
            .iter()
            .map(|a| match a {
                Arg::Str(s) => Component::text(s.as_str()),
                Arg::Int(v) => Component::text(v.to_string()),
                Arg::Long(v) => Component::text(v.to_string()),
                Arg::Float(v) => Component::text(v.to_string()),
                Arg::Double(v) => Component::text(v.to_string()),
                Arg::Component(c) => (**c).clone(),
            })
            .collect();
        Component::translatable(self.key, args)
    }

    /// The context line of an error with a position (Vanilla counts UTF-16 units; this counts
    /// characters, which only differs for characters outside the BMP).
    pub fn context(&self) -> Option<ErrorContext> {
        let input = self.input.as_deref()?;
        let cursor = self.cursor?.min(input.len());
        let head = input.get(..cursor)?;
        let tail = input.get(cursor..)?;
        let count = head.chars().count();
        let skip = count.saturating_sub(10);
        Some(ErrorContext {
            ellipsis: count > 10,
            before: head.chars().skip(skip).collect(),
            after: tail.to_owned(),
        })
    }
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.key)?;
        if let Some(cursor) = self.cursor {
            write!(f, " at {cursor}")?;
        }
        Ok(())
    }
}

impl std::error::Error for CommandError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_like_vanilla() {
        let e = CommandError::new("command.unknown.command", vec![])
            .at_position("time query daytime", 18);
        let c = e.context();
        assert_eq!(
            c,
            Some(ErrorContext {
                ellipsis: true,
                before: "ry daytime".into(),
                after: String::new(),
            })
        );
        let e = CommandError::new("command.unknown.command", vec![]).at_position("say no", 0);
        assert_eq!(
            e.context().map(|c| (c.ellipsis, c.before, c.after)),
            Some((false, String::new(), "say no".into()))
        );
    }
}
