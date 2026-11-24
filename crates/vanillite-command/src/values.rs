//! Small value types of arguments: game modes, entity anchors, identifiers and UUIDs.

use crate::error::CommandError;
use crate::reader::StringReader;

/// A game mode (`GameType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GameMode {
    /// `survival`.
    Survival,
    /// `creative`.
    Creative,
    /// `adventure`.
    Adventure,
    /// `spectator`.
    Spectator,
}

impl GameMode {
    /// All modes in Vanilla's order.
    pub const ALL: [GameMode; 4] = [
        GameMode::Survival,
        GameMode::Creative,
        GameMode::Adventure,
        GameMode::Spectator,
    ];

    /// The command name.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Survival => "survival",
            Self::Creative => "creative",
            Self::Adventure => "adventure",
            Self::Spectator => "spectator",
        }
    }

    /// `GameType.byName`.
    pub fn by_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.name() == name)
    }
}

/// The anchor of `facing entity` (`EntityAnchorArgument.Anchor`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    /// The entity's feet.
    Feet,
    /// The entity's eyes.
    Eyes,
}

impl Anchor {
    /// By command name.
    pub fn by_name(name: &str) -> Option<Self> {
        match name {
            "feet" => Some(Self::Feet),
            "eyes" => Some(Self::Eyes),
            _ => None,
        }
    }
}

fn allowed_in_identifier(c: char) -> bool {
    c.is_ascii_digit() || c.is_ascii_lowercase() || matches!(c, '_' | ':' | '/' | '.' | '-')
}

/// Validates `namespace:path` like `Identifier.parse` and returns the full form.
pub fn normalize_identifier(raw: &str) -> Option<String> {
    let (ns, path) = match raw.split_once(':') {
        Some(("", path)) => ("minecraft", path),
        Some((ns, path)) => (ns, path),
        None => ("minecraft", raw),
    };
    let ns_ok = ns
        .chars()
        .all(|c| c.is_ascii_digit() || c.is_ascii_lowercase() || matches!(c, '_' | '.' | '-'));
    let path_ok = path.chars().all(|c| {
        c.is_ascii_digit() || c.is_ascii_lowercase() || matches!(c, '_' | '.' | '-' | '/')
    });
    (ns_ok && path_ok).then(|| format!("{ns}:{path}"))
}

/// `Identifier.read`.
pub fn parse_identifier(r: &mut StringReader<'_>) -> Result<String, CommandError> {
    let start = r.cursor();
    let raw = r.read_while(allowed_in_identifier);
    match normalize_identifier(raw) {
        Some(id) => Ok(id),
        None => {
            r.set_cursor(start);
            Err(CommandError::new("argument.id.invalid", vec![]).at(r))
        }
    }
}

/// Java's `UUID.fromString`: five hex groups, at most 36 characters.
pub fn parse_uuid(s: &str) -> Option<u128> {
    if s.len() > 36 {
        return None;
    }
    let mut parts = s.split('-');
    let mut next = || -> Option<u64> {
        let p = parts.next()?;
        if p.is_empty() || p.starts_with('+') {
            return None;
        }
        i64::from_str_radix(p, 16)
            .ok()
            .and_then(|v| u64::try_from(v).ok())
    };
    let (a, b, c, d, e) = (next()?, next()?, next()?, next()?, next()?);
    if parts.next().is_some() {
        return None;
    }
    let msb = ((a & 0xffff_ffff) << 32) | ((b & 0xffff) << 16) | (c & 0xffff);
    let lsb = ((d & 0xffff) << 48) | (e & 0xffff_ffff_ffff);
    Some((u128::from(msb) << 64) | u128::from(lsb))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuids() {
        assert_eq!(
            parse_uuid("10920508-d5d8-3eed-93d2-92f193afe7d7"),
            Some(0x10920508_d5d8_3eed_93d2_92f193afe7d7)
        );
        assert_eq!(
            parse_uuid("1-2-3-4-5"),
            Some((1u128 << 96) | (2 << 80) | (3 << 64) | (4 << 48) | 5)
        );
        assert_eq!(parse_uuid("Notch"), None);
        assert_eq!(parse_uuid("1-2-3-4"), None);
    }

    #[test]
    fn identifiers() {
        assert_eq!(
            normalize_identifier("cow").as_deref(),
            Some("minecraft:cow")
        );
        assert_eq!(
            normalize_identifier(":cow").as_deref(),
            Some("minecraft:cow")
        );
        assert_eq!(normalize_identifier("a:b:c"), None);
    }
}
