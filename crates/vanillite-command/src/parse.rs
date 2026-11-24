//! Parsing a command line against the tree, the parse result and the typed arguments
//! (Brigadier's `CommandDispatcher.parse`, `ParseResults` and `CommandContext`).

use vanillite_nbt::{Compound, Tag};

use crate::argument::{ArgValue, GameProfileArg, Message, ParseCtx};
use crate::bounds::{DoubleRange, IntRange};
use crate::coords::Coordinates;
use crate::error::{Arg, CommandError};
use crate::nbt_path::NbtPath;
use crate::reader::StringReader;
use crate::selector::EntitySelector;
use crate::source::Source;
use crate::tree::{Dispatcher, NodeId, NodeKind};
use crate::values::{Anchor, GameMode};

/// A range of the input (byte offsets).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Range {
    /// Start.
    pub start: usize,
    /// End.
    pub end: usize,
}

impl Range {
    fn at(pos: usize) -> Self {
        Self {
            start: pos,
            end: pos,
        }
    }

    fn encompassing(a: Self, b: Self) -> Self {
        Self {
            start: a.start.min(b.start),
            end: a.end.max(b.end),
        }
    }

    /// Whether the range is empty.
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

/// One parsed argument.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedArgument {
    /// Argument name.
    pub name: String,
    /// Its node.
    pub node: NodeId,
    /// Where it stands in the input.
    pub range: Range,
    /// The value.
    pub value: ArgValue,
}

/// The arguments of a command by name.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Args {
    list: Vec<ParsedArgument>,
}

macro_rules! accessor {
    ($(#[$m:meta])* $fn:ident -> $ty:ty, $variant:ident) => {
        $(#[$m])*
        pub fn $fn(&self, name: &str) -> Option<&$ty> {
            match self.get(name)? {
                ArgValue::$variant(v) => Some(v),
                _ => None,
            }
        }
    };
}

impl Args {
    /// The value of `name`.
    pub fn get(&self, name: &str) -> Option<&ArgValue> {
        self.list.iter().find(|a| a.name == name).map(|a| &a.value)
    }

    /// Whether `name` was given.
    pub fn has(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// All arguments in input order.
    pub fn iter(&self) -> impl Iterator<Item = &ParsedArgument> {
        self.list.iter()
    }

    fn put(&mut self, arg: ParsedArgument) {
        self.list.retain(|a| a.name != arg.name);
        self.list.push(arg);
    }

    accessor!(/// A bool.
        bool -> bool, Bool);
    accessor!(/// An int.
        int -> i32, Int);
    accessor!(/// A long.
        long -> i64, Long);
    accessor!(/// A float.
        float -> f32, Float);
    accessor!(/// A double.
        double -> f64, Double);
    accessor!(/// A string.
        string -> String, String);
    accessor!(/// Coordinates.
        coordinates -> Coordinates, Coordinates);
    accessor!(/// An NBT compound.
        compound -> Compound, Compound);
    accessor!(/// An NBT value.
        tag -> Tag, Tag);
    accessor!(/// An NBT path.
        nbt_path -> NbtPath, NbtPath);
    accessor!(/// A game mode.
        game_mode -> GameMode, GameMode);
    accessor!(/// Ticks of a time argument.
        time -> i32, Time);
    accessor!(/// A UUID.
        uuid -> u128, Uuid);
    accessor!(/// An identifier or resource.
        identifier -> String, Identifier);
    accessor!(/// An entity anchor.
        anchor -> Anchor, Anchor);
    accessor!(/// An int range.
        int_range -> IntRange, IntRange);
    accessor!(/// A float range.
        float_range -> DoubleRange, FloatRange);

    /// An entity selector.
    pub fn selector(&self, name: &str) -> Option<&EntitySelector> {
        match self.get(name)? {
            ArgValue::Entity(s) => Some(s),
            _ => None,
        }
    }

    /// A game profile argument.
    pub fn game_profile(&self, name: &str) -> Option<&GameProfileArg> {
        match self.get(name)? {
            ArgValue::GameProfile(p) => Some(p),
            _ => None,
        }
    }

    /// A message.
    pub fn message(&self, name: &str) -> Option<&Message> {
        match self.get(name)? {
            ArgValue::Message(m) => Some(m),
            _ => None,
        }
    }
}

/// `CommandContextBuilder`.
#[derive(Debug, Clone)]
pub(crate) struct Context<C> {
    pub(crate) root: NodeId,
    pub(crate) nodes: Vec<(NodeId, Range)>,
    pub(crate) args: Args,
    pub(crate) command: Option<C>,
    pub(crate) child: Option<Box<Context<C>>>,
    pub(crate) range: Range,
}

impl<C> Context<C> {
    fn new(root: NodeId, start: usize) -> Self {
        Self {
            root,
            nodes: Vec::new(),
            args: Args::default(),
            command: None,
            child: None,
            range: Range::at(start),
        }
    }

    fn with_node(&mut self, node: NodeId, range: Range) {
        self.nodes.push((node, range));
        self.range = Range::encompassing(self.range, range);
    }

    /// The innermost context (after redirects).
    pub(crate) fn last(&self) -> &Context<C> {
        let mut c = self;
        while let Some(child) = &c.child {
            c = child;
        }
        c
    }
}

/// The result of parsing: how far the input was understood and the errors of the children
/// that failed there.
#[derive(Debug, Clone)]
pub struct ParseResults<C> {
    pub(crate) input: String,
    pub(crate) context: Context<C>,
    pub(crate) cursor: usize,
    pub(crate) errors: Vec<(NodeId, CommandError)>,
}

struct Partial<C> {
    context: Context<C>,
    cursor: usize,
    errors: Vec<(NodeId, CommandError)>,
}

/// A successfully parsed command.
#[derive(Debug, Clone)]
pub struct Parsed<C> {
    /// The command to run.
    pub command: C,
    /// Its arguments (of the innermost context after redirects).
    pub args: Args,
    /// The command line.
    pub input: String,
    /// Signable arguments as (name, raw text), like Vanilla's `SignableCommand`.
    pub signable: Vec<(String, String)>,
}

impl<C: Copy> ParseResults<C> {
    /// The command line.
    pub fn input(&self) -> &str {
        &self.input
    }

    /// Whether the input was read to its end.
    pub fn is_complete(&self) -> bool {
        self.cursor >= self.input.len()
    }

    /// The nodes of the outer context (for `/help <command>`).
    pub fn nodes(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.context.nodes.iter().map(|(n, _)| *n)
    }

    /// Signable arguments (name, raw text) parsed so far (`SignableCommand.of`).
    pub fn signable_arguments(&self, dispatcher: &Dispatcher<C>) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut ctx = Some(&self.context);
        while let Some(c) = ctx {
            for (node, range) in &c.nodes {
                if let Some(NodeKind::Argument { name, ty }) =
                    dispatcher.node(*node).map(|n| &n.kind)
                    && ty.is_signable()
                    && c.args.has(name)
                {
                    let text = self.input.get(range.start..range.end).unwrap_or("");
                    out.push((name.clone(), text.to_owned()));
                }
            }
            // `rejectRootRedirects`: a redirect back to the root ends the visit.
            ctx = c
                .child
                .as_deref()
                .filter(|child| child.root != Dispatcher::<C>::ROOT);
        }
        out
    }

    /// Turns the result into a command or Vanilla's error (`Commands.finishParsing`).
    pub fn finish(self, dispatcher: &Dispatcher<C>) -> Result<Parsed<C>, CommandError> {
        let mut reader = StringReader::new(&self.input);
        reader.set_cursor(self.cursor);
        if reader.can_read() {
            if self.errors.len() == 1
                && let Some((_, e)) = self.errors.into_iter().next()
            {
                return Err(e);
            }
            let key = if self.context.range.is_empty() {
                "command.unknown.command"
            } else {
                "command.unknown.argument"
            };
            return Err(CommandError::new(key, vec![]).at(&reader));
        }
        let signable = self.signable_arguments(dispatcher);
        let last = self.context.last();
        match last.command {
            Some(command) => Ok(Parsed {
                command,
                args: last.args.clone(),
                input: self.input.clone(),
                signable,
            }),
            None => Err(CommandError::unknown_command(&reader)),
        }
    }
}

impl<C: Copy> Dispatcher<C> {
    /// Parses `input` (without the leading `/`).
    pub fn parse(&self, input: &str, source: &dyn Source) -> ParseResults<C> {
        self.parse_from(input, 0, source)
    }

    /// Parses `input` from `start` on (suggestions skip a leading `/`).
    pub fn parse_from(&self, input: &str, start: usize, source: &dyn Source) -> ParseResults<C> {
        let mut reader = StringReader::new(input);
        reader.set_cursor(start);
        let context = Context::new(Self::ROOT, reader.cursor());
        if input.len() > self.limits.max_command_length {
            return ParseResults {
                input: input.to_owned(),
                context,
                cursor: reader.cursor(),
                errors: vec![(
                    Self::ROOT,
                    CommandError::new("command.unknown.command", vec![]).at(&reader),
                )],
            };
        }
        let p = self.parse_nodes(Self::ROOT, &reader, &context, source, 0);
        ParseResults {
            input: input.to_owned(),
            context: p.context,
            cursor: p.cursor,
            errors: p.errors,
        }
    }

    /// Parses and finishes in one step.
    pub fn parse_command(
        &self,
        input: &str,
        source: &dyn Source,
    ) -> Result<Parsed<C>, CommandError> {
        self.parse(input, source).finish(self)
    }

    /// `getRelevantNodes`: the literal child matching the next word, else the argument
    /// children.
    fn relevant(&self, node: NodeId, reader: &StringReader<'_>) -> Vec<NodeId> {
        let Some(n) = self.node(node) else {
            return Vec::new();
        };
        let word = reader.remaining().split(' ').next().unwrap_or("");
        for &c in &n.children {
            if let Some(NodeKind::Literal(l)) = self.node(c).map(|n| &n.kind)
                && l == word
            {
                return vec![c];
            }
        }
        n.children
            .iter()
            .copied()
            .filter(|c| {
                matches!(
                    self.node(*c).map(|n| &n.kind),
                    Some(NodeKind::Argument { .. })
                )
            })
            .collect()
    }

    fn parse_child(
        &self,
        child: NodeId,
        reader: &mut StringReader<'_>,
        context: &mut Context<C>,
        source: &dyn Source,
    ) -> Result<(), CommandError> {
        let Some(node) = self.node(child) else {
            return Err(CommandError::unknown_command(reader));
        };
        let start = reader.cursor();
        match &node.kind {
            NodeKind::Root => return Err(CommandError::unknown_command(reader)),
            NodeKind::Literal(l) => {
                let rest = reader.remaining();
                let ends = rest
                    .strip_prefix(l.as_str())
                    .is_some_and(|after| after.is_empty() || after.starts_with(' '));
                if !ends {
                    return Err(CommandError::new(
                        "argument.literal.incorrect",
                        vec![Arg::from(l.as_str())],
                    )
                    .at(reader));
                }
                reader.set_cursor(start.saturating_add(l.len()));
                context.with_node(
                    child,
                    Range {
                        start,
                        end: reader.cursor(),
                    },
                );
            }
            NodeKind::Argument { name, ty } => {
                let ctx = ParseCtx {
                    source,
                    limits: &self.limits,
                };
                let value = ty.parse(reader, ctx)?;
                let range = Range {
                    start,
                    end: reader.cursor(),
                };
                context.args.put(ParsedArgument {
                    name: name.clone(),
                    node: child,
                    range,
                    value,
                });
                context.with_node(child, range);
            }
        }
        if reader.can_read() && !reader.peek_is(' ') {
            return Err(CommandError::new("command.expected.separator", vec![]).at(reader));
        }
        Ok(())
    }

    fn parse_nodes(
        &self,
        node: NodeId,
        original: &StringReader<'_>,
        so_far: &Context<C>,
        source: &dyn Source,
        depth: usize,
    ) -> Partial<C> {
        let mut errors = Vec::new();
        let mut potentials: Vec<Partial<C>> = Vec::new();
        for child in self.relevant(node, original) {
            let Some(child_node) = self.node(child) else {
                continue;
            };
            if !source.meets(&child_node.requirement) {
                continue;
            }
            let mut context = so_far.clone();
            let mut reader = original.clone();
            if let Err(e) = self.parse_child(child, &mut reader, &mut context, source) {
                errors.push((child, e));
                continue;
            }
            context.command = child_node.command;
            let need = if child_node.redirect.is_none() { 2 } else { 1 };
            if reader.can_read_n(need) {
                reader.skip();
                if let Some(redirect) = child_node.redirect {
                    if depth >= self.limits.max_redirect_depth {
                        errors.push((child, CommandError::unknown_command(&reader)));
                        continue;
                    }
                    let child_context = Context::new(redirect, reader.cursor());
                    let parse = self.parse_nodes(
                        redirect,
                        &reader,
                        &child_context,
                        source,
                        depth.saturating_add(1),
                    );
                    context.child = Some(Box::new(parse.context));
                    return Partial {
                        context,
                        cursor: parse.cursor,
                        errors: parse.errors,
                    };
                }
                potentials.push(self.parse_nodes(
                    child,
                    &reader,
                    &context,
                    source,
                    depth.saturating_add(1),
                ));
            } else {
                potentials.push(Partial {
                    context,
                    cursor: reader.cursor(),
                    errors: Vec::new(),
                });
            }
        }
        if !potentials.is_empty() {
            let len = original.string().len();
            potentials.sort_by_key(|p| (p.cursor < len, !p.errors.is_empty()));
            if let Some(best) = potentials.into_iter().next() {
                return best;
            }
        }
        Partial {
            context: so_far.clone(),
            cursor: original.cursor(),
            errors,
        }
    }
}
