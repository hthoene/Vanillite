//! NBT paths such as `Inventory[0].tag.display{a:1}` (Vanilla's `NbtPathArgument`), parsed
//! iteratively with a node limit (SEC-04).

use vanillite_nbt::Compound;

use crate::error::CommandError;
use crate::limits::Limits;
use crate::reader::StringReader;
use crate::snbt;

/// One node of a path.
#[derive(Debug, Clone, PartialEq)]
pub enum PathNode {
    /// `name`: a child of a compound.
    Child(String),
    /// `name{...}`: a child compound matching a pattern.
    MatchObject(String, Compound),
    /// `[]`: all list elements.
    AllElements,
    /// `[i]`: one list element (negative counts from the end).
    Index(i32),
    /// `[{...}]`: the list elements matching a pattern.
    MatchElement(Compound),
    /// `{...}` at the start: the root when it matches.
    MatchRoot(Compound),
}

/// A parsed path with its original text.
#[derive(Debug, Clone, PartialEq)]
pub struct NbtPath {
    /// The path as written.
    pub text: String,
    /// Its nodes.
    pub nodes: Vec<PathNode>,
}

fn invalid(r: &StringReader<'_>) -> CommandError {
    CommandError::new("arguments.nbtpath.node.invalid", vec![]).at(r)
}

fn unquoted_name_char(c: char) -> bool {
    !matches!(c, ' ' | '"' | '\'' | '[' | ']' | '.' | '{' | '}')
}

fn object_node(
    r: &mut StringReader<'_>,
    name: String,
    limits: &Limits,
) -> Result<PathNode, CommandError> {
    if name.is_empty() {
        return Err(invalid(r));
    }
    if r.peek_is('{') {
        let pattern = snbt::parse_compound(r, limits)?;
        Ok(PathNode::MatchObject(name, pattern))
    } else {
        Ok(PathNode::Child(name))
    }
}

fn parse_node(
    r: &mut StringReader<'_>,
    first: bool,
    limits: &Limits,
) -> Result<PathNode, CommandError> {
    match r.peek() {
        Some('"' | '\'') => {
            let name = r.read_string()?;
            object_node(r, name, limits)
        }
        Some('[') => {
            r.skip();
            match r.peek() {
                Some('{') => {
                    let pattern = snbt::parse_compound(r, limits)?;
                    r.expect(']')?;
                    Ok(PathNode::MatchElement(pattern))
                }
                Some(']') => {
                    r.skip();
                    Ok(PathNode::AllElements)
                }
                _ => {
                    let index = r.read_int()?;
                    r.expect(']')?;
                    Ok(PathNode::Index(index))
                }
            }
        }
        Some('{') => {
            if !first {
                return Err(invalid(r));
            }
            Ok(PathNode::MatchRoot(snbt::parse_compound(r, limits)?))
        }
        _ => {
            let name = r.read_while(unquoted_name_char);
            if name.is_empty() {
                return Err(invalid(r));
            }
            object_node(r, name.to_owned(), limits)
        }
    }
}

/// `NbtPathArgument.parse`.
pub fn parse(r: &mut StringReader<'_>, limits: &Limits) -> Result<NbtPath, CommandError> {
    let start = r.cursor();
    let mut nodes = Vec::new();
    let mut first = true;
    while r.can_read() && !r.peek_is(' ') {
        if nodes.len() >= limits.max_nbt_path_nodes {
            return Err(CommandError::new("arguments.nbtpath.too_deep", vec![]).at(r));
        }
        nodes.push(parse_node(r, first, limits)?);
        first = false;
        if let Some(next) = r.peek()
            && !matches!(next, ' ' | '[' | '{')
        {
            r.expect('.')?;
        }
    }
    Ok(NbtPath {
        text: r.slice(start, r.cursor()).to_owned(),
        nodes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        let limits = Limits::default();
        let p = parse(
            &mut StringReader::new("Inventory[0].tag.\"a b\"[]"),
            &limits,
        )
        .ok();
        assert_eq!(
            p.map(|p| p.nodes),
            Some(vec![
                PathNode::Child("Inventory".into()),
                PathNode::Index(0),
                PathNode::Child("tag".into()),
                PathNode::Child("a b".into()),
                PathNode::AllElements,
            ])
        );
        let e = parse(&mut StringReader::new("a.{b:1}"), &limits).err();
        assert_eq!(e.map(|e| e.key), Some("arguments.nbtpath.node.invalid"));
    }

    #[test]
    fn deep_paths_fail_cleanly() {
        let limits = Limits::default();
        let long = "a.".repeat(100_000);
        let e = parse(&mut StringReader::new(&long), &limits).err();
        assert_eq!(e.map(|e| e.key), Some("arguments.nbtpath.too_deep"));
        let deep = format!("a{}", "{b:".repeat(100_000));
        assert!(parse(&mut StringReader::new(&deep), &limits).is_err());
    }
}
