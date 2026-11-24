//! Usage texts for `/help` (Brigadier's `getSmartUsage`).

use crate::error::CommandError;
use crate::source::Source;
use crate::tree::{Dispatcher, NodeId};

impl<C: Copy> Dispatcher<C> {
    /// `getSmartUsage(node, source)`: one usage line per usable child.
    pub fn smart_usage(&self, node: NodeId, source: &dyn Source) -> Vec<(NodeId, String)> {
        let Some(n) = self.node(node) else {
            return Vec::new();
        };
        let optional = n.command.is_some();
        n.children
            .iter()
            .filter_map(|&c| {
                self.smart_usage_of(c, source, optional, false)
                    .map(|u| (c, u))
            })
            .collect()
    }

    fn redirect_text(&self, target: NodeId) -> String {
        if target == Self::ROOT {
            "...".to_owned()
        } else {
            format!(
                "-> {}",
                self.node(target)
                    .map(|n| n.usage_text())
                    .unwrap_or_default()
            )
        }
    }

    fn smart_usage_of(
        &self,
        node: NodeId,
        source: &dyn Source,
        optional: bool,
        deep: bool,
    ) -> Option<String> {
        let n = self.node(node)?;
        if !source.meets(&n.requirement) {
            return None;
        }
        let me = if optional {
            format!("[{}]", n.usage_text())
        } else {
            n.usage_text()
        };
        let child_optional = n.command.is_some();
        let (open, close) = if child_optional {
            ("[", "]")
        } else {
            ("(", ")")
        };
        if deep {
            return Some(me);
        }
        if let Some(target) = n.redirect {
            return Some(format!("{me} {}", self.redirect_text(target)));
        }
        let children: Vec<NodeId> = n
            .children
            .iter()
            .copied()
            .filter(|c| self.node(*c).is_some_and(|c| source.meets(&c.requirement)))
            .collect();
        match children.as_slice() {
            [] => {}
            [only] => {
                if let Some(usage) =
                    self.smart_usage_of(*only, source, child_optional, child_optional)
                {
                    return Some(format!("{me} {usage}"));
                }
            }
            _ => {
                let mut usages: Vec<String> = Vec::new();
                for &c in &children {
                    if let Some(u) = self.smart_usage_of(c, source, child_optional, true)
                        && !usages.contains(&u)
                    {
                        usages.push(u);
                    }
                }
                match usages.as_slice() {
                    [] => {}
                    [usage] => {
                        let usage = if child_optional {
                            format!("[{usage}]")
                        } else {
                            usage.clone()
                        };
                        return Some(format!("{me} {usage}"));
                    }
                    _ => {
                        let texts: Vec<String> = children
                            .iter()
                            .filter_map(|c| self.node(*c).map(|n| n.usage_text()))
                            .collect();
                        return Some(format!("{me} {open}{}{close}", texts.join("|")));
                    }
                }
            }
        }
        Some(me)
    }

    /// The lines of `/help` (`HelpCommand`): without a command all usages from the root,
    /// with a command the usages after it, each starting with `/`.
    pub fn help_lines(
        &self,
        command: Option<&str>,
        source: &dyn Source,
    ) -> Result<Vec<String>, CommandError> {
        match command {
            None => Ok(self
                .smart_usage(Self::ROOT, source)
                .into_iter()
                .map(|(_, u)| format!("/{u}"))
                .collect()),
            Some(command) => {
                let parse = self.parse(command, source);
                let Some(last) = parse.nodes().last() else {
                    return Err(CommandError::new("commands.help.failed", vec![]));
                };
                Ok(self
                    .smart_usage(last, source)
                    .into_iter()
                    .map(|(_, u)| format!("/{command} {u}"))
                    .collect())
            }
        }
    }
}
