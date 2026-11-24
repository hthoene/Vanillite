//! Completion suggestions (`CommandDispatcher.getCompletionSuggestions`), walking only nodes
//! the source may use (SEC-11).

use crate::argument::ParseCtx;
use crate::parse::{Args, Context, ParseResults};
use crate::source::Source;
use crate::suggestion::{Suggestions, SuggestionsBuilder};
use crate::tree::{Dispatcher, NodeId, NodeKind};

impl<C: Copy> Dispatcher<C> {
    /// Suggestions for a `command_suggestion` request: `input` is what the client sent,
    /// usually with a leading `/`. Too long inputs get none (SEC-11).
    pub fn suggest(&self, input: &str, source: &dyn Source) -> Suggestions {
        if input.len() > self.limits.max_suggestion_input {
            return Suggestions::default();
        }
        let start = usize::from(input.starts_with('/'));
        let parse = self.parse_from(input, start, source);
        let mut s = self.completion_suggestions(&parse, source);
        s.list.truncate(self.limits.max_suggestions);
        s
    }

    /// Suggestions at the end of the parsed input.
    pub fn completion_suggestions(
        &self,
        parse: &ParseResults<C>,
        source: &dyn Source,
    ) -> Suggestions {
        let input = parse.input.as_str();
        let cursor = input.len();
        let (parent, start, args) = find_context(&parse.context, cursor);
        let start = start.min(cursor);
        let Some(node) = self.node(parent) else {
            return Suggestions::default();
        };
        let ctx = ParseCtx {
            source,
            limits: &self.limits,
        };
        let mut all = Vec::new();
        for &child in &node.children {
            let Some(c) = self.node(child) else {
                continue;
            };
            if !source.meets(&c.requirement) {
                continue;
            }
            let mut builder = SuggestionsBuilder::new(input, start);
            match &c.kind {
                NodeKind::Root => {}
                NodeKind::Literal(l) => {
                    if l.to_lowercase().starts_with(builder.remaining_lower()) {
                        builder.suggest(l.as_str());
                    }
                    all.push(builder.build());
                }
                NodeKind::Argument { ty, .. } => match c.suggests {
                    Some(provider) => {
                        source.suggest_custom(provider, args, &mut builder);
                        all.push(builder.build());
                    }
                    None => all.push(ty.suggestions(builder, ctx, args)),
                },
            }
        }
        Suggestions::merge(input, all)
    }
}

/// `CommandContextBuilder.findSuggestionContext`: the node whose children are suggested,
/// where the suggested range starts and the arguments parsed so far.
fn find_context<C>(ctx: &Context<C>, cursor: usize) -> (NodeId, usize, &Args) {
    if ctx.range.start > cursor {
        return (ctx.root, ctx.range.start, &ctx.args);
    }
    if ctx.range.end < cursor {
        if let Some(child) = &ctx.child {
            return find_context(child, cursor);
        }
        return match ctx.nodes.last() {
            Some((node, range)) => (*node, range.end.saturating_add(1), &ctx.args),
            None => (ctx.root, ctx.range.start, &ctx.args),
        };
    }
    let mut prev = ctx.root;
    for (node, range) in &ctx.nodes {
        if range.start <= cursor && cursor <= range.end {
            return (prev, range.start, &ctx.args);
        }
        prev = *node;
    }
    (prev, ctx.range.start, &ctx.args)
}
