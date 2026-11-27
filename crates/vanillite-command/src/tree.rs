//! The command tree: nodes in an arena, a fluent builder and the dispatcher that owns them
//! (Brigadier's `CommandNode`, `ArgumentBuilder` and `CommandDispatcher`).

use crate::argument::ArgumentType;
use crate::limits::Limits;

/// Index of a node in its dispatcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub(crate) usize);

/// Who may use a node: a Vanilla permission level and optionally a plugin permission node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Requirement {
    /// Minimum permission level (0 = everyone, 2 = game masters, 3 = admins, 4 = owners).
    pub level: u8,
    /// A permission node a plugin permission system checks (hook for later).
    pub permission: Option<&'static str>,
    /// The node decides alone when a permission provider or a plugin's registered default
    /// answers it; `level` is only the default (Vanillite's own commands).
    pub node_decides: bool,
}

impl Requirement {
    /// Whether a source without any permission fails it (the packet's restricted flag).
    pub fn is_restricted(&self) -> bool {
        self.level > 0 || (self.permission.is_some() && !self.node_decides)
    }
}

/// What a node is.
#[derive(Debug, Clone, PartialEq)]
pub enum NodeKind {
    /// The root.
    Root,
    /// A literal word.
    Literal(String),
    /// An argument.
    Argument {
        /// Name, also the key of its value.
        name: String,
        /// Type.
        ty: ArgumentType,
    },
}

/// One node of the tree.
#[derive(Debug, Clone)]
pub struct Node<C> {
    /// Kind.
    pub kind: NodeKind,
    /// Children in registration order.
    pub children: Vec<NodeId>,
    /// Who may use it.
    pub requirement: Requirement,
    /// The command that runs when the input ends here.
    pub command: Option<C>,
    /// Parsing continues at this node's children instead of its own.
    pub redirect: Option<NodeId>,
    /// What the redirect does to the source (Brigadier's `RedirectModifier`).
    pub modifier: Option<C>,
    /// The redirect forks: one source per result, errors of one do not stop the others.
    pub forks: bool,
    /// Custom suggestion provider (sent as `minecraft:ask_server`).
    pub suggests: Option<&'static str>,
}

impl<C> Node<C> {
    /// Brigadier's `getName`: the literal or the argument name.
    pub fn name(&self) -> &str {
        match &self.kind {
            NodeKind::Root => "",
            NodeKind::Literal(l) => l,
            NodeKind::Argument { name, .. } => name,
        }
    }

    /// `getUsageText`: `literal` or `<name>`.
    pub fn usage_text(&self) -> String {
        match &self.kind {
            NodeKind::Root => String::new(),
            NodeKind::Literal(l) => l.clone(),
            NodeKind::Argument { name, .. } => format!("<{name}>"),
        }
    }
}

/// A node under construction (`LiteralArgumentBuilder` / `RequiredArgumentBuilder`).
#[derive(Debug, Clone)]
pub struct Builder<C> {
    kind: NodeKind,
    requirement: Requirement,
    command: Option<C>,
    redirect: Option<NodeId>,
    modifier: Option<C>,
    forks: bool,
    suggests: Option<&'static str>,
    children: Vec<Builder<C>>,
}

/// A literal node.
pub fn literal<C>(literal: &str) -> Builder<C> {
    Builder::new(NodeKind::Literal(literal.to_owned()))
}

/// An argument node.
pub fn argument<C>(name: &str, ty: ArgumentType) -> Builder<C> {
    Builder::new(NodeKind::Argument {
        name: name.to_owned(),
        ty,
    })
}

impl<C> Builder<C> {
    fn new(kind: NodeKind) -> Self {
        Self {
            kind,
            requirement: Requirement::default(),
            command: None,
            redirect: None,
            modifier: None,
            forks: false,
            suggests: None,
            children: Vec::new(),
        }
    }

    /// Adds a child.
    #[must_use]
    pub fn then(mut self, child: Builder<C>) -> Self {
        self.children.push(child);
        self
    }

    /// The command that runs when the input ends at this node.
    #[must_use]
    pub fn executes(mut self, command: C) -> Self {
        self.command = Some(command);
        self
    }

    /// Requires a permission level.
    #[must_use]
    pub fn requires(mut self, level: u8) -> Self {
        self.requirement.level = level;
        self
    }

    /// Requires a plugin permission node as well.
    #[must_use]
    pub fn permission(mut self, node: &'static str) -> Self {
        self.requirement.permission = Some(node);
        self
    }

    /// Requires a permission node that a permission provider can grant or deny; without its
    /// answer the source needs permission level `level`.
    #[must_use]
    pub fn permission_or_level(mut self, node: &'static str, level: u8) -> Self {
        self.requirement = Requirement {
            level,
            permission: Some(node),
            node_decides: true,
        };
        self
    }

    /// Continues parsing at another node's children.
    #[must_use]
    pub fn redirect(mut self, target: NodeId) -> Self {
        self.redirect = Some(target);
        self
    }

    /// Continues parsing at another node's children; `modifier` changes the source on the way
    /// (Brigadier's `redirect(target, modifier)`).
    #[must_use]
    pub fn redirect_with(mut self, target: NodeId, modifier: C) -> Self {
        self.redirect = Some(target);
        self.modifier = Some(modifier);
        self
    }

    /// Continues parsing at another node's children; `modifier` turns one source into any
    /// number of them (Brigadier's `fork`).
    #[must_use]
    pub fn fork(mut self, target: NodeId, modifier: C) -> Self {
        self.redirect = Some(target);
        self.modifier = Some(modifier);
        self.forks = true;
        self
    }

    /// Replaces each child by `f(child)`.
    #[must_use]
    pub fn map_children(mut self, f: impl Fn(Builder<C>) -> Builder<C>) -> Self {
        self.children = self.children.into_iter().map(&f).collect();
        self
    }

    /// A custom suggestion provider the source answers.
    #[must_use]
    pub fn suggests(mut self, provider: &'static str) -> Self {
        self.suggests = Some(provider);
        self
    }
}

/// The command tree with its limits.
#[derive(Debug, Clone)]
pub struct Dispatcher<C> {
    pub(crate) nodes: Vec<Node<C>>,
    pub(crate) limits: Limits,
}

impl<C: Copy> Default for Dispatcher<C> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C: Copy> Dispatcher<C> {
    /// An empty tree with default limits.
    pub fn new() -> Self {
        Self {
            nodes: vec![Node {
                kind: NodeKind::Root,
                children: Vec::new(),
                requirement: Requirement::default(),
                command: None,
                redirect: None,
                modifier: None,
                forks: false,
                suggests: None,
            }],
            limits: Limits::default(),
        }
    }

    /// The root node.
    pub const ROOT: NodeId = NodeId(0);

    /// The limits.
    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    /// Replaces the limits (from the configuration).
    pub fn set_limits(&mut self, limits: Limits) {
        self.limits = limits;
    }

    /// A node.
    pub fn node(&self, id: NodeId) -> Option<&Node<C>> {
        self.nodes.get(id.0)
    }

    /// The child of `parent` with this name.
    pub fn child(&self, parent: NodeId, name: &str) -> Option<NodeId> {
        self.node(parent)?
            .children
            .iter()
            .copied()
            .find(|c| self.node(*c).is_some_and(|n| n.name() == name))
    }

    /// Registers a top-level command and returns its node (`CommandDispatcher.register`).
    pub fn register(&mut self, builder: Builder<C>) -> NodeId {
        self.add_child(Self::ROOT, builder)
    }

    /// `CommandNode.addChild`: a child with the same name is merged (its command replaced
    /// when the new one has one, the new grandchildren added to it).
    pub fn add_child(&mut self, parent: NodeId, builder: Builder<C>) -> NodeId {
        let Builder {
            kind,
            requirement,
            command,
            redirect,
            modifier,
            forks,
            suggests,
            children,
        } = builder;
        let name = match &kind {
            NodeKind::Literal(l) => l.clone(),
            NodeKind::Argument { name, .. } => name.clone(),
            NodeKind::Root => String::new(),
        };
        let id = match self.child(parent, &name) {
            Some(existing) => {
                if let Some(node) = self.nodes.get_mut(existing.0)
                    && command.is_some()
                {
                    node.command = command;
                }
                existing
            }
            None => {
                let id = NodeId(self.nodes.len());
                self.nodes.push(Node {
                    kind,
                    children: Vec::new(),
                    requirement,
                    command,
                    redirect,
                    modifier,
                    forks,
                    suggests,
                });
                if let Some(p) = self.nodes.get_mut(parent.0) {
                    p.children.push(id);
                }
                id
            }
        };
        for child in children {
            self.add_child(id, child);
        }
        id
    }

    /// Follows literal names from the root, e.g. `["time", "query"]`.
    pub fn find(&self, path: &[&str]) -> Option<NodeId> {
        path.iter()
            .try_fold(Self::ROOT, |node, name| self.child(node, name))
    }
}
