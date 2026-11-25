//! Commands: trees of literals and typed arguments that join the server's Brigadier tree, so
//! clients get syntax highlighting and completion like for Vanilla commands.

use std::ffi::c_void;

use vanillite_abi::{self as abi, Status, status};

use crate::Plugin;
use crate::ffi::{guard, str, with_plugin};
use crate::server::Ctx;
use crate::types::{BlockPos, Text, Uuid, Vec3, World};

/// The type of a command argument.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ArgType {
    /// `true` or `false`.
    Bool,
    /// A 32-bit integer with optional bounds.
    Integer {
        /// Lower bound.
        min: Option<i32>,
        /// Upper bound.
        max: Option<i32>,
    },
    /// A 64-bit integer with optional bounds.
    Long {
        /// Lower bound.
        min: Option<i64>,
        /// Upper bound.
        max: Option<i64>,
    },
    /// A 32-bit float with optional bounds.
    Float {
        /// Lower bound.
        min: Option<f32>,
        /// Upper bound.
        max: Option<f32>,
    },
    /// A 64-bit float with optional bounds.
    Double {
        /// Lower bound.
        min: Option<f64>,
        /// Upper bound.
        max: Option<f64>,
    },
    /// One word.
    Word,
    /// A word or a quoted string.
    String,
    /// The rest of the input.
    GreedyString,
    /// One player: a name or a selector like `@p`.
    Player,
    /// Players: names or selectors like `@a`.
    Players,
    /// A block position; relative coordinates (`~ ~1 ~`) are resolved.
    BlockPos,
}

impl ArgType {
    /// The ABI code and the bounds as `f64`.
    fn encode(self) -> (u32, Option<f64>, Option<f64>) {
        use abi::arg;
        match self {
            ArgType::Bool => (arg::BOOL, None, None),
            ArgType::Integer { min, max } => (arg::INTEGER, min.map(f64::from), max.map(f64::from)),
            ArgType::Long { min, max } => (arg::LONG, min.map(|v| v as f64), max.map(|v| v as f64)),
            ArgType::Float { min, max } => (arg::FLOAT, min.map(f64::from), max.map(f64::from)),
            ArgType::Double { min, max } => (arg::DOUBLE, min, max),
            ArgType::Word => (arg::WORD, None, None),
            ArgType::String => (arg::STRING, None, None),
            ArgType::GreedyString => (arg::GREEDY_STRING, None, None),
            ArgType::Player => (arg::PLAYER, None, None),
            ArgType::Players => (arg::PLAYERS, None, None),
            ArgType::BlockPos => (arg::BLOCK_POS, None, None),
        }
    }
}

type Exec<P> = dyn FnMut(&mut P, &mut Ctx<'_>, &Invocation<'_>) -> Result<i32, Text> + Send;
type Suggest<P> = dyn FnMut(&mut P, &mut Ctx<'_>, &SuggestRequest<'_>) -> Vec<String> + Send;

enum Kind {
    Literal(String),
    Argument(String, ArgType),
}

/// A command node with its children, built like Brigadier's builders.
pub struct Command<P> {
    kind: Kind,
    level: u8,
    permission: Option<String>,
    executes: Option<Box<Exec<P>>>,
    suggests: Option<Box<Suggest<P>>>,
    children: Vec<Command<P>>,
}

impl<P> std::fmt::Debug for Command<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match &self.kind {
            Kind::Literal(n) | Kind::Argument(n, _) => n,
        };
        f.debug_struct("Command")
            .field("name", name)
            .field("children", &self.children)
            .finish_non_exhaustive()
    }
}

impl<P: Plugin> Command<P> {
    fn new(kind: Kind) -> Self {
        Command {
            kind,
            level: 0,
            permission: None,
            executes: None,
            suggests: None,
            children: Vec::new(),
        }
    }

    /// A literal word, e.g. the command's name.
    pub fn literal(name: &str) -> Self {
        Self::new(Kind::Literal(name.to_owned()))
    }

    /// A typed argument; its value is read by `name` from the [`Invocation`].
    pub fn argument(name: &str, ty: ArgType) -> Self {
        Self::new(Kind::Argument(name.to_owned(), ty))
    }

    /// Adds a child node.
    #[must_use]
    pub fn then(mut self, child: Command<P>) -> Self {
        self.children.push(child);
        self
    }

    /// Requires a Vanilla permission level (0 everyone, 2 game masters, 3 admins, 4 owners).
    #[must_use]
    pub fn requires_level(mut self, level: u8) -> Self {
        self.level = level;
        self
    }

    /// Requires a permission node (checked by the permission provider; without one, operators
    /// have every node and other players none).
    #[must_use]
    pub fn requires_permission(mut self, node: &str) -> Self {
        self.permission = Some(node.to_owned());
        self
    }

    /// Runs `f` when the input ends at this node. `Err` sends the text in red to the sender.
    #[must_use]
    pub fn executes(
        mut self,
        f: impl FnMut(&mut P, &mut Ctx<'_>, &Invocation<'_>) -> Result<i32, Text> + Send + 'static,
    ) -> Self {
        self.executes = Some(Box::new(f));
        self
    }

    /// Answers tab completion of this argument with `f` (the host filters by the typed prefix).
    #[must_use]
    pub fn suggests(
        mut self,
        f: impl FnMut(&mut P, &mut Ctx<'_>, &SuggestRequest<'_>) -> Vec<String> + Send + 'static,
    ) -> Self {
        self.suggests = Some(Box::new(f));
        self
    }
}

/// The callbacks of one registered tree, numbered from 1 in the nodes.
pub(crate) struct Tree<P> {
    execs: Vec<Box<Exec<P>>>,
    suggests: Vec<Box<Suggest<P>>>,
}

/// A flattened node: its name, the record (strings still empty) and its permission node.
pub(crate) type FlatNode = (Vec<u8>, abi::CommandNode, Option<String>);

/// Flattens a command into ABI nodes (parents first) and its callbacks.
pub(crate) fn flatten<P: Plugin>(command: Command<P>) -> (Vec<FlatNode>, Tree<P>) {
    let mut nodes = Vec::new();
    let mut tree = Tree {
        execs: Vec::new(),
        suggests: Vec::new(),
    };
    let mut queue = vec![(abi::limits::ROOT, command)];
    while let Some((parent, c)) = queue.pop() {
        let index = u32::try_from(nodes.len()).unwrap_or(u32::MAX);
        let (kind, name, ty) = match c.kind {
            Kind::Literal(name) => (abi::node::LITERAL, name, (abi::arg::BOOL, None, None)),
            Kind::Argument(name, ty) => (abi::node::ARGUMENT, name, ty.encode()),
        };
        let executes = c.executes.map_or(0, |f| {
            tree.execs.push(f);
            u32::try_from(tree.execs.len()).unwrap_or(0)
        });
        let suggests = c.suggests.map_or(0, |f| {
            tree.suggests.push(f);
            u32::try_from(tree.suggests.len()).unwrap_or(0)
        });
        let node = abi::CommandNode {
            parent,
            kind,
            name: abi::Str::EMPTY,
            arg_type: ty.0,
            has_min: u32::from(ty.1.is_some()),
            min: ty.1.unwrap_or(0.0),
            has_max: u32::from(ty.2.is_some()),
            max: ty.2.unwrap_or(0.0),
            level: u32::from(c.level),
            permission: abi::Str::EMPTY,
            executes,
            suggests,
        };
        nodes.push((name.into_bytes(), node, c.permission));
        // Children in registration order: the queue is a stack, so push them reversed.
        for child in c.children.into_iter().rev() {
            queue.push((index, child));
        }
    }
    (nodes, tree)
}

/// The SDK's `execute` callback of a registered tree.
pub(crate) unsafe extern "C" fn execute<P: Plugin>(
    user: *mut c_void,
    ctx: *mut abi::Context,
    invocation: *const abi::CommandInvocation,
    result: *mut i32,
) -> Status {
    guard(|| {
        if invocation.is_null() || result.is_null() {
            return status::INVALID_ARGUMENT;
        }
        let invocation = Invocation {
            // SAFETY: the host passes a record valid for the call.
            raw: unsafe { &*invocation },
        };
        let mut ctx = Ctx::new(ctx);
        let outcome = with_plugin::<P, _>(|p| {
            // SAFETY: `user` is the leaked `Tree<P>` of this registration; calls are serialized
            // by the plugin lock held here.
            let tree = unsafe { &mut *user.cast::<Tree<P>>() };
            let index = usize::try_from(invocation.raw.command).unwrap_or(0);
            let f = index.checked_sub(1).and_then(|i| tree.execs.get_mut(i))?;
            Some(f(p, &mut ctx, &invocation))
        });
        match outcome.flatten() {
            Some(Ok(value)) => {
                // SAFETY: checked above; valid for the call.
                unsafe { *result = value };
                status::OK
            }
            Some(Err(text)) => {
                invocation.reply_text(&ctx, text);
                // SAFETY: checked above; valid for the call.
                unsafe { *result = 0 };
                status::OK
            }
            None => status::NOT_FOUND,
        }
    })
}

/// The SDK's `suggest` callback of a registered tree.
pub(crate) unsafe extern "C" fn suggest<P: Plugin>(
    user: *mut c_void,
    ctx: *mut abi::Context,
    request: *const abi::SuggestRequest,
    sink: *const abi::SuggestSink,
) -> Status {
    guard(|| {
        if request.is_null() || sink.is_null() {
            return status::INVALID_ARGUMENT;
        }
        // SAFETY: the host passes records valid for the call.
        let (raw, sink) = unsafe { (&*request, &*sink) };
        let request = SuggestRequest { raw };
        let mut ctx = Ctx::new(ctx);
        let list = with_plugin::<P, _>(|p| {
            // SAFETY: as in `execute`.
            let tree = unsafe { &mut *user.cast::<Tree<P>>() };
            let index = usize::try_from(request.raw.provider).unwrap_or(0);
            let f = index
                .checked_sub(1)
                .and_then(|i| tree.suggests.get_mut(i))?;
            Some(f(p, &mut ctx, &request))
        });
        for s in list.flatten().unwrap_or_default() {
            // SAFETY: the host's sink is valid for the call and copies the string.
            unsafe { (sink.add)(sink.state, abi::Str::new(&s)) };
        }
        status::OK
    })
}

/// Who runs a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sender {
    /// The server console.
    Console,
    /// A player.
    Player(Uuid),
}

fn sender(raw: &abi::Sender) -> Sender {
    if raw.kind == abi::sender::PLAYER {
        Sender::Player(Uuid::from_abi(raw.player))
    } else {
        Sender::Console
    }
}

/// Looks up an argument by name in a host argument array.
fn find_arg<'a>(
    args: *const abi::CommandArg,
    count: usize,
    stride: usize,
    name: &str,
) -> Option<&'a abi::CommandArg> {
    (0..count).find_map(|i| {
        // SAFETY: the host passes `count` records `stride` bytes apart, valid for the call.
        let a = unsafe { &*args.cast::<u8>().add(i * stride).cast::<abi::CommandArg>() };
        // SAFETY: as above.
        (unsafe { str(a.name) } == name).then_some(a)
    })
}

macro_rules! arg_accessors {
    () => {
        /// A boolean argument.
        pub fn bool(&self, name: &str) -> Option<bool> {
            self.arg(name)
                .filter(|a| a.kind == abi::arg::BOOL)
                .map(|a| a.int != 0)
        }

        /// An integer argument (`Integer` or `Long`).
        pub fn int(&self, name: &str) -> Option<i64> {
            self.arg(name)
                .filter(|a| a.kind == abi::arg::INTEGER || a.kind == abi::arg::LONG)
                .map(|a| a.int)
        }

        /// A float argument (`Float` or `Double`).
        pub fn float(&self, name: &str) -> Option<f64> {
            self.arg(name)
                .filter(|a| a.kind == abi::arg::FLOAT || a.kind == abi::arg::DOUBLE)
                .map(|a| a.float)
        }

        /// A string argument (`Word`, `String`, `GreedyString`).
        pub fn string(&self, name: &str) -> Option<&str> {
            self.arg(name)
                .filter(|a| {
                    matches!(
                        a.kind,
                        abi::arg::WORD | abi::arg::STRING | abi::arg::GREEDY_STRING
                    )
                })
                // SAFETY: the host's strings are valid for the call.
                .map(|a| unsafe { str(a.text) })
        }

        /// The players of a `Player` or `Players` argument.
        pub fn players(&self, name: &str) -> Vec<Uuid> {
            self.arg(name)
                .filter(|a| a.kind == abi::arg::PLAYER || a.kind == abi::arg::PLAYERS)
                .filter(|a| !a.players.is_null())
                .map(|a| {
                    // SAFETY: the host passes `player_count` UUIDs valid for the call.
                    unsafe { std::slice::from_raw_parts(a.players, a.player_count) }
                        .iter()
                        .map(|u| Uuid::from_abi(*u))
                        .collect()
                })
                .unwrap_or_default()
        }

        /// A `BlockPos` argument.
        pub fn block_pos(&self, name: &str) -> Option<BlockPos> {
            self.arg(name)
                .filter(|a| a.kind == abi::arg::BLOCK_POS)
                .map(|a| BlockPos::from_abi(a.pos))
        }

        /// Who runs the command.
        pub fn sender(&self) -> Sender {
            sender(&self.raw.sender)
        }

        /// The sender's world (`None` for the console).
        pub fn world(&self) -> Option<World> {
            (self.raw.sender.world != abi::limits::NO_WORLD).then_some(World(self.raw.sender.world))
        }

        /// The sender's position.
        pub fn position(&self) -> Vec3 {
            Vec3::from_abi(self.raw.sender.pos)
        }
    };
}

/// A command being run: sender and parsed arguments, valid for the call.
#[derive(Debug)]
pub struct Invocation<'a> {
    raw: &'a abi::CommandInvocation,
}

impl Invocation<'_> {
    fn arg(&self, name: &str) -> Option<&abi::CommandArg> {
        find_arg(self.raw.args, self.raw.arg_count, self.raw.arg_stride, name)
    }

    arg_accessors!();

    /// The whole command line without `/`.
    pub fn line(&self) -> &str {
        // SAFETY: the host's strings are valid for the call.
        unsafe { str(self.raw.line) }
    }

    /// Sends feedback to the sender: a system message to a player, a log line for the console.
    pub fn reply(&self, ctx: &Ctx<'_>, text: impl Into<Text>) {
        self.reply_text(ctx, text.into());
    }

    fn reply_text(&self, ctx: &Ctx<'_>, text: Text) {
        match self.sender() {
            Sender::Player(player) => {
                let _ = ctx.server().send_message(player, text);
            }
            Sender::Console => match text {
                Text::Plain(s) | Text::Json(s) => crate::log(abi::log_level::INFO, &s),
            },
        }
    }
}

/// A tab completion request of a custom suggestion provider, valid for the call.
#[derive(Debug)]
pub struct SuggestRequest<'a> {
    raw: &'a abi::SuggestRequest,
}

impl SuggestRequest<'_> {
    fn arg(&self, name: &str) -> Option<&abi::CommandArg> {
        find_arg(self.raw.args, self.raw.arg_count, self.raw.arg_stride, name)
    }

    arg_accessors!();

    /// What the sender typed of this argument so far.
    pub fn remaining(&self) -> &str {
        // SAFETY: the host's strings are valid for the call.
        unsafe { str(self.raw.remaining) }
    }
}
