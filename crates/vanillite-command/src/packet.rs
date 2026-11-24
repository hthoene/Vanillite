//! The commands packet for one source (`Commands.sendCommands` + `ClientboundCommandsPacket`).

use std::collections::{HashMap, VecDeque};

use vanillite_protocol::commands::{CommandTree, Node as WireNode, NodeKind as WireKind};
use vanillite_protocol::types::Identifier;

use crate::source::Source;
use crate::tree::{Dispatcher, NodeId, NodeKind};

/// A node of the filtered tree: the original node, its usable children and its redirect.
struct Usable {
    original: NodeId,
    children: Vec<usize>,
    redirect: Option<usize>,
}

impl<C: Copy> Dispatcher<C> {
    /// The tree the source may use, numbered breadth-first like Vanilla.
    pub fn to_tree(&self, source: &dyn Source) -> CommandTree {
        // `fillUsableCommands`: copy the usable nodes depth-first; redirects point to the copy
        // of their target when it exists by then.
        let mut usable = vec![Usable {
            original: Self::ROOT,
            children: Vec::new(),
            redirect: None,
        }];
        let mut converted: HashMap<NodeId, usize> = HashMap::from([(Self::ROOT, 0)]);
        let mut stack = vec![(Self::ROOT, 0usize, 0usize)];
        while let Some((original, copy, next)) = stack.pop() {
            let Some(node) = self.node(original) else {
                continue;
            };
            let Some(&child) = node.children.get(next) else {
                continue;
            };
            stack.push((original, copy, next.saturating_add(1)));
            let Some(c) = self.node(child) else {
                continue;
            };
            if !source.meets(&c.requirement) {
                continue;
            }
            let index = usable.len();
            usable.push(Usable {
                original: child,
                children: Vec::new(),
                redirect: c.redirect.and_then(|r| converted.get(&r).copied()),
            });
            converted.insert(child, index);
            if let Some(parent) = usable.get_mut(copy) {
                parent.children.push(index);
            }
            stack.push((child, index, 0));
        }
        // `enumerateNodes`: breadth-first, children then redirect.
        let mut ids: Vec<Option<i32>> = vec![None; usable.len()];
        let mut order = Vec::new();
        let mut queue = VecDeque::from([0usize]);
        while let Some(i) = queue.pop_front() {
            let Some(slot) = ids.get_mut(i) else {
                continue;
            };
            if slot.is_some() {
                continue;
            }
            *slot = i32::try_from(order.len()).ok();
            order.push(i);
            if let Some(u) = usable.get(i) {
                queue.extend(u.children.iter().copied());
                queue.extend(u.redirect);
            }
        }
        let id_of = |i: usize| ids.get(i).copied().flatten().unwrap_or(0);
        let nodes = order
            .iter()
            .filter_map(|&i| {
                let u = usable.get(i)?;
                let n = self.node(u.original)?;
                let kind = match &n.kind {
                    NodeKind::Root => WireKind::Root,
                    NodeKind::Literal(l) => WireKind::Literal(l.clone()),
                    NodeKind::Argument { name, ty } => WireKind::Argument {
                        name: name.clone(),
                        parser: ty.wire(),
                        suggestions: n.suggests.map(|_| Identifier::vanilla("ask_server")),
                    },
                };
                Some(WireNode {
                    kind,
                    executable: n.command.is_some(),
                    restricted: n.requirement.is_restricted(),
                    children: u.children.iter().map(|&c| id_of(c)).collect(),
                    redirect: u.redirect.map(id_of),
                })
            })
            .collect();
        CommandTree { nodes, root: 0 }
    }
}
