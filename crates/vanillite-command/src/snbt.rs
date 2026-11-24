//! SNBT inside command arguments, parsed with the depth limit (SEC-04).

use vanillite_nbt::{Compound, Tag};

use crate::error::{Arg, CommandError};
use crate::limits::Limits;
use crate::reader::StringReader;

/// `TagParser.parseAsArgument`: one SNBT value at the cursor.
pub fn parse_tag(r: &mut StringReader<'_>, limits: &Limits) -> Result<Tag, CommandError> {
    let start = r.cursor();
    match vanillite_nbt::snbt::parse_prefix(r.remaining(), limits.max_nbt_depth) {
        Ok((tag, used)) => {
            r.set_cursor(start.saturating_add(used));
            Ok(tag)
        }
        Err(e) => {
            let (offset, message) = match e {
                vanillite_nbt::Error::Syntax { offset, message } => (offset, message),
                vanillite_nbt::Error::DepthExceeded => (0, "too deep"),
                _ => (0, "invalid"),
            };
            r.set_cursor(start.saturating_add(offset));
            Err(CommandError::new("command.exception", vec![Arg::from(message)]).at(r))
        }
    }
}

/// `TagParser.parseCompoundAsArgument`.
pub fn parse_compound(r: &mut StringReader<'_>, limits: &Limits) -> Result<Compound, CommandError> {
    match parse_tag(r, limits)? {
        Tag::Compound(c) => Ok(c),
        _ => Err(CommandError::new("argument.nbt.expected.compound", vec![]).at(r)),
    }
}
