//! Parsers and suggestions of the game-specific argument types.

use crate::argument::{ArgValue, ArgumentType, ParseCtx};
use crate::error::CommandError;
use crate::reader::StringReader;

/// Parses a game argument type at the reader's cursor.
pub(crate) fn parse(
    ty: &ArgumentType,
    r: &mut StringReader<'_>,
    _ctx: ParseCtx<'_>,
) -> Result<ArgValue, CommandError> {
    let _ = ty;
    Err(CommandError::new("command.unknown.argument", vec![]).at(r))
}
