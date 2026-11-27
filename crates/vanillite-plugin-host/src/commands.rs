//! Plugin commands: trees copied at registration, added to the server's Brigadier tree before
//! the server starts, and run through the plugin's callbacks.
//!
//! A command node of a plugin carries a key from [`PluginHost::register_commands`]'s mapping of
//! `(tree << 16) | command number`; suggestion providers are named
//! `vanillite-plugin:<tree>:<provider>`. A development reload keeps the trees and replaces the
//! callbacks.

use std::sync::{Mutex, PoisonError, RwLock};

use vanillite_abi::{self as abi, Status, status};
use vanillite_command::{ArgumentType, Builder, Dispatcher, StringKind, argument, literal};

use crate::api::{CallCtx, str_checked};
use crate::{Inner, Plugin, PluginHost, SendPtr, WorldAccess};

/// Prefix of the suggestion provider names of plugin arguments.
pub const SUGGEST_PREFIX: &str = "vanillite-plugin:";

/// Who runs a plugin command.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sender {
    /// One of [`abi::sender`]; with `CONSOLE` (the default) a sender with a player counts
    /// as `PLAYER`.
    pub kind: u32,
    /// The player (or entity for `ENTITY`), `None` for the console and command blocks.
    pub player: Option<u128>,
    /// The sender's world.
    pub world: Option<u32>,
    /// The sender's position.
    pub pos: [f64; 3],
}

/// A parsed argument value.
#[derive(Clone, Debug, PartialEq)]
pub enum ArgValue {
    /// `bool`.
    Bool(bool),
    /// `integer`.
    Integer(i64),
    /// `long`.
    Long(i64),
    /// `float`.
    Float(f64),
    /// `double`.
    Double(f64),
    /// Any string argument.
    Text(String),
    /// Selected players.
    Players(Vec<u128>),
    /// A resolved block position.
    Pos([i32; 3]),
    /// Selected entities.
    Entities(Vec<u128>),
    /// A resolved position.
    Vec3([f64; 3]),
    /// A resolved rotation: yaw and pitch.
    Rotation([f64; 2]),
    /// A value of an argument type plugins get as text ([`abi::arg`] kind and the text).
    Raw(u32, String),
    /// A value of an argument type plugins get as an integer ([`abi::arg`] kind and value).
    Int(u32, i64),
}

/// A parsed argument.
#[derive(Clone, Debug, PartialEq)]
pub struct Arg {
    /// The argument's name.
    pub name: String,
    /// Its value.
    pub value: ArgValue,
}

#[derive(Clone, Debug, PartialEq)]
struct Node {
    parent: Option<usize>,
    literal: bool,
    name: String,
    arg_type: u32,
    min: Option<f64>,
    max: Option<f64>,
    level: u8,
    permission: Option<String>,
    executes: u32,
    suggests: u32,
}

#[derive(Debug)]
struct Tree {
    plugin: u32,
    nodes: Vec<Node>,
    execute: abi::CommandFn,
    suggest: Option<abi::SuggestFn>,
    user: SendPtr,
}

#[derive(Debug, Default)]
pub(crate) struct Commands {
    trees: RwLock<Vec<Tree>>,
    /// During a development reload: the plugin and how many of its trees were registered again.
    reload: Mutex<Option<(u32, usize)>>,
}

fn copy_nodes(spec: &abi::CommandSpec) -> Result<Vec<Node>, String> {
    if spec.nodes.is_null() || spec.count == 0 || spec.count > 4096 {
        return Err("a command needs 1 to 4096 nodes".into());
    }
    if spec.stride < size_of::<abi::CommandNode>() {
        return Err("command node records are too small".into());
    }
    let mut nodes = Vec::with_capacity(spec.count);
    for i in 0..spec.count {
        // SAFETY: the plugin passes `count` records `stride` bytes apart, valid for the call.
        let raw = unsafe {
            &*spec
                .nodes
                .cast::<u8>()
                .add(i * spec.stride)
                .cast::<abi::CommandNode>()
        };
        // SAFETY: the plugin's strings are valid for the call.
        let name = unsafe { str_checked(raw.name) }.ok_or("a node name is not UTF-8")?;
        // SAFETY: as above.
        let permission =
            unsafe { str_checked(raw.permission) }.ok_or("a permission is not UTF-8")?;
        if name.is_empty() || name.len() > 64 || name.contains(' ') {
            return Err(format!("invalid node name {name:?}"));
        }
        let parent = (raw.parent != abi::limits::ROOT)
            .then(|| usize::try_from(raw.parent).unwrap_or(usize::MAX));
        if parent.is_some_and(|p| p >= i) || (i > 0 && parent.is_none()) {
            return Err("node parents must come first, with one root".into());
        }
        if raw.kind == abi::node::ARGUMENT && raw.arg_type > abi::arg::UUID {
            return Err(format!("unknown argument type {}", raw.arg_type));
        }
        if raw.executes > 0xffff || raw.suggests > 0xffff {
            return Err("command and provider numbers must be below 65536".into());
        }
        nodes.push(Node {
            parent,
            literal: raw.kind != abi::node::ARGUMENT,
            name: name.to_owned(),
            arg_type: raw.arg_type,
            min: (raw.has_min != 0).then_some(raw.min),
            max: (raw.has_max != 0).then_some(raw.max),
            level: u8::try_from(raw.level.min(4)).unwrap_or(4),
            permission: (!permission.is_empty()).then(|| permission.to_owned()),
            executes: raw.executes,
            suggests: raw.suggests,
        });
    }
    Ok(nodes)
}

impl Commands {
    pub(crate) fn register(
        &self,
        inner: &Inner,
        plugin: &Plugin,
        spec: &abi::CommandSpec,
    ) -> Status {
        if (spec.size as usize) < size_of::<abi::CommandSpec>() {
            return status::INVALID_ARGUMENT;
        }
        let nodes = match copy_nodes(spec) {
            Ok(nodes) => nodes,
            Err(e) => {
                log::error!("[{}] Invalid command: {e}", plugin.id());
                return status::INVALID_ARGUMENT;
            }
        };
        let mut trees = self.trees.write().unwrap_or_else(PoisonError::into_inner);
        if inner.is_reloading(plugin) {
            let mut reload = self.reload.lock().unwrap_or_else(PoisonError::into_inner);
            let Some((_, done)) = reload.as_mut() else {
                return status::WRONG_PHASE;
            };
            let existing = trees
                .iter_mut()
                .filter(|t| t.plugin == plugin.local)
                .nth(*done);
            *done += 1;
            match existing {
                Some(tree) => {
                    if tree.nodes != nodes {
                        log::warn!(
                            "[{}] The command /{} changed; its new syntax takes effect after a restart",
                            plugin.id(),
                            nodes.first().map_or("", |n| n.name.as_str())
                        );
                    }
                    tree.execute = spec.execute;
                    tree.suggest = spec.suggest;
                    tree.user = SendPtr(spec.user);
                }
                None => log::warn!(
                    "[{}] The new command /{} takes effect after a restart",
                    plugin.id(),
                    nodes.first().map_or("", |n| n.name.as_str())
                ),
            }
            return status::OK;
        }
        if trees.len() >= 0xffff {
            return status::ERROR;
        }
        trees.push(Tree {
            plugin: plugin.local,
            nodes,
            execute: spec.execute,
            suggest: spec.suggest,
            user: SendPtr(spec.user),
        });
        status::OK
    }

    /// Starts counting the re-registered trees of a reloaded plugin.
    pub(crate) fn begin_reload(&self, plugin: u32) {
        *self.reload.lock().unwrap_or_else(PoisonError::into_inner) = Some((plugin, 0));
    }
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

fn argument_type(node: &Node) -> ArgumentType {
    let int = |v: Option<f64>| v.map(|v| v.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32);
    match node.arg_type {
        abi::arg::BOOL => ArgumentType::Bool,
        abi::arg::INTEGER => ArgumentType::Integer {
            min: int(node.min),
            max: int(node.max),
        },
        abi::arg::LONG => ArgumentType::Long {
            min: node.min.map(|v| v as i64),
            max: node.max.map(|v| v as i64),
        },
        abi::arg::FLOAT => ArgumentType::Float {
            min: node.min.map(|v| v as f32),
            max: node.max.map(|v| v as f32),
        },
        abi::arg::DOUBLE => ArgumentType::Double {
            min: node.min,
            max: node.max,
        },
        abi::arg::WORD => ArgumentType::String(StringKind::SingleWord),
        abi::arg::STRING => ArgumentType::String(StringKind::QuotablePhrase),
        abi::arg::GREEDY_STRING => ArgumentType::String(StringKind::GreedyPhrase),
        abi::arg::PLAYER => ArgumentType::Entity {
            single: true,
            players_only: true,
        },
        abi::arg::PLAYERS => ArgumentType::Entity {
            single: false,
            players_only: true,
        },
        abi::arg::ENTITY => ArgumentType::Entity {
            single: true,
            players_only: false,
        },
        abi::arg::ENTITIES => ArgumentType::Entity {
            single: false,
            players_only: false,
        },
        abi::arg::VEC3 => ArgumentType::Vec3 { center: true },
        abi::arg::ROTATION => ArgumentType::Rotation,
        abi::arg::IDENTIFIER => ArgumentType::Identifier,
        abi::arg::BLOCK_STATE => ArgumentType::BlockState,
        abi::arg::ITEM_STACK => ArgumentType::ItemStack,
        abi::arg::COMPONENT => ArgumentType::Component,
        abi::arg::NBT_COMPOUND => ArgumentType::NbtCompound,
        abi::arg::GAME_MODE => ArgumentType::GameMode,
        abi::arg::TIME => ArgumentType::Time { min: 0 },
        abi::arg::UUID => ArgumentType::Uuid,
        _ => ArgumentType::BlockPos,
    }
}

impl PluginHost {
    /// Adds the plugins' command trees to `dispatcher`; `key` maps a plugin command number to
    /// the dispatcher's command key. A top-level name that already exists (a Vanilla command or
    /// an earlier plugin) is not replaced.
    pub fn register_commands<C: Copy>(
        &self,
        dispatcher: &mut Dispatcher<C>,
        key: impl Fn(u32) -> C,
    ) {
        let trees = self
            .inner
            .commands
            .trees
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        for (index, tree) in trees.iter().enumerate() {
            let Some(root) = tree.nodes.first() else {
                continue;
            };
            let id = self
                .inner
                .plugin(tree.plugin)
                .map(|p| p.id().to_owned())
                .unwrap_or_default();
            if !root.literal {
                log::error!(
                    "[{id}] A command must start with a literal; /{} is skipped",
                    root.name
                );
                continue;
            }
            if dispatcher
                .child(Dispatcher::<C>::ROOT, &root.name)
                .is_some()
            {
                log::warn!(
                    "[{id}] The command /{} already exists; the plugin's one is skipped",
                    root.name
                );
                continue;
            }
            let tree_index = u32::try_from(index).unwrap_or(0);
            dispatcher.register(build(tree, 0, tree_index, &key));
        }
    }

    /// Runs plugin command `command` (the number given to the key mapping). `world` is the
    /// sender's world when the caller owns it. Returns the command's result, `None` when the
    /// plugin failed or is disabled.
    pub fn execute_command(
        &self,
        command: u32,
        sender: &Sender,
        line: &str,
        args: &[Arg],
        world: Option<&mut dyn WorldAccess>,
    ) -> Option<i32> {
        let (tree_index, number) = (command >> 16, command & 0xffff);
        let trees = self
            .inner
            .commands
            .trees
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let tree = trees.get(usize::try_from(tree_index).ok()?)?;
        let plugin = self.inner.plugin(tree.plugin)?;
        let (raw_args, _players) = encode_args(args);
        let invocation = abi::CommandInvocation {
            size: abi::size_of::<abi::CommandInvocation>(),
            command: number,
            sender: encode_sender(sender),
            line: abi::Str::new(line),
            args: raw_args.as_ptr(),
            arg_count: raw_args.len(),
            arg_stride: size_of::<abi::CommandArg>(),
        };
        let mut result = 0;
        let mut ctx = CallCtx::new(world, &plugin);
        let s = plugin.call("a command", || {
            // SAFETY: the plugin's callback and user data live as long as its library; the
            // records, strings and context are valid for the call.
            unsafe { (tree.execute)(tree.user.0, ctx.as_raw(), &invocation, &mut result) }
        });
        (s == status::OK).then_some(result)
    }

    /// Answers a plugin's suggestion provider (`provider` as registered in the tree).
    pub fn suggest(
        &self,
        provider: &str,
        sender: &Sender,
        args: &[Arg],
        remaining: &str,
    ) -> Vec<String> {
        let Some((tree_index, number)) = provider
            .strip_prefix(SUGGEST_PREFIX)
            .and_then(|rest| rest.split_once(':'))
            .and_then(|(t, n)| Some((t.parse::<usize>().ok()?, n.parse::<u32>().ok()?)))
        else {
            return Vec::new();
        };
        let trees = self
            .inner
            .commands
            .trees
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let Some(tree) = trees.get(tree_index) else {
            return Vec::new();
        };
        let (Some(suggest), Some(plugin)) = (tree.suggest, self.inner.plugin(tree.plugin)) else {
            return Vec::new();
        };
        let (raw_args, _players) = encode_args(args);
        let request = abi::SuggestRequest {
            size: abi::size_of::<abi::SuggestRequest>(),
            provider: number,
            sender: encode_sender(sender),
            remaining: abi::Str::new(remaining),
            args: raw_args.as_ptr(),
            arg_count: raw_args.len(),
            arg_stride: size_of::<abi::CommandArg>(),
        };
        let mut out: Vec<String> = Vec::new();
        let sink = abi::SuggestSink {
            size: abi::size_of::<abi::SuggestSink>(),
            state: std::ptr::from_mut(&mut out).cast(),
            add: add_suggestion,
        };
        let mut ctx = CallCtx::new(None, &plugin);
        plugin.call("a suggestion provider", || {
            // SAFETY: as in `execute_command`.
            unsafe { suggest(tree.user.0, ctx.as_raw(), &request, &sink) }
        });
        out
    }
}

/// Largest number of suggestions taken from one provider call.
const MAX_SUGGESTIONS: usize = 10_000;

unsafe extern "C" fn add_suggestion(state: *mut std::ffi::c_void, text: abi::Str) {
    let _ = std::panic::catch_unwind(|| {
        // SAFETY: the host passes its own `Vec<String>` as state for the call.
        let Some(out) = (unsafe { state.cast::<Vec<String>>().as_mut() }) else {
            return;
        };
        // SAFETY: the plugin's string is valid for the call.
        if let Some(s) = unsafe { str_checked(text) }
            && out.len() < MAX_SUGGESTIONS
            && s.len() <= 256
        {
            out.push(s.to_owned());
        }
    });
}

fn build<C: Copy>(
    tree: &Tree,
    index: usize,
    tree_index: u32,
    key: &impl Fn(u32) -> C,
) -> Builder<C> {
    let Some(node) = tree.nodes.get(index) else {
        return literal("");
    };
    let mut b = if node.literal {
        literal(&node.name)
    } else {
        argument(&node.name, argument_type(node))
    };
    b = b.requires(node.level);
    if let Some(permission) = &node.permission {
        b = b.permission(leak(permission.clone()));
    }
    if node.executes != 0 {
        b = b.executes(key((tree_index << 16) | node.executes));
    }
    if node.suggests != 0 && !node.literal {
        b = b.suggests(leak(format!(
            "{SUGGEST_PREFIX}{tree_index}:{}",
            node.suggests
        )));
    }
    for (child, _) in tree
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.parent == Some(index))
    {
        b = b.then(build(tree, child, tree_index, key));
    }
    b
}

fn encode_sender(sender: &Sender) -> abi::Sender {
    abi::Sender {
        kind: if sender.kind != abi::sender::CONSOLE {
            sender.kind
        } else if sender.player.is_some() {
            abi::sender::PLAYER
        } else {
            abi::sender::CONSOLE
        },
        player: abi::Uuid::from_u128(sender.player.unwrap_or(0)),
        world: sender.world.unwrap_or(abi::limits::NO_WORLD),
        pos: abi::Vec3 {
            x: sender.pos[0],
            y: sender.pos[1],
            z: sender.pos[2],
        },
    }
}

/// The ABI records of `args` and the player arrays they point to (keep both alive).
fn encode_args(args: &[Arg]) -> (Vec<abi::CommandArg>, Vec<Vec<abi::Uuid>>) {
    let players: Vec<Vec<abi::Uuid>> = args
        .iter()
        .map(|a| match &a.value {
            ArgValue::Players(p) | ArgValue::Entities(p) => {
                p.iter().map(|u| abi::Uuid::from_u128(*u)).collect()
            }
            _ => Vec::new(),
        })
        .collect();
    let records = args
        .iter()
        .zip(&players)
        .map(|(a, players)| {
            let mut r = abi::CommandArg {
                name: abi::Str::new(&a.name),
                kind: abi::arg::BOOL,
                int: 0,
                float: 0.0,
                text: abi::Str::EMPTY,
                pos: abi::BlockPos { x: 0, y: 0, z: 0 },
                players: players.as_ptr(),
                player_count: players.len(),
                vec: abi::Vec3 {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                },
            };
            match &a.value {
                ArgValue::Bool(b) => r.int = i64::from(*b),
                ArgValue::Integer(v) => (r.kind, r.int) = (abi::arg::INTEGER, *v),
                ArgValue::Long(v) => (r.kind, r.int) = (abi::arg::LONG, *v),
                ArgValue::Float(v) => (r.kind, r.float) = (abi::arg::FLOAT, *v),
                ArgValue::Double(v) => (r.kind, r.float) = (abi::arg::DOUBLE, *v),
                ArgValue::Text(s) => (r.kind, r.text) = (abi::arg::STRING, abi::Str::new(s)),
                ArgValue::Players(_) => r.kind = abi::arg::PLAYERS,
                ArgValue::Entities(_) => r.kind = abi::arg::ENTITIES,
                ArgValue::Vec3([x, y, z]) => {
                    (r.kind, r.vec) = (
                        abi::arg::VEC3,
                        abi::Vec3 {
                            x: *x,
                            y: *y,
                            z: *z,
                        },
                    );
                }
                ArgValue::Rotation([yaw, pitch]) => {
                    (r.kind, r.vec) = (
                        abi::arg::ROTATION,
                        abi::Vec3 {
                            x: *yaw,
                            y: *pitch,
                            z: 0.0,
                        },
                    );
                }
                ArgValue::Raw(kind, s) => (r.kind, r.text) = (*kind, abi::Str::new(s)),
                ArgValue::Int(kind, v) => (r.kind, r.int) = (*kind, *v),
                ArgValue::Pos([x, y, z]) => {
                    (r.kind, r.pos) = (
                        abi::arg::BLOCK_POS,
                        abi::BlockPos {
                            x: *x,
                            y: *y,
                            z: *z,
                        },
                    );
                }
            }
            r
        })
        .collect();
    (records, players)
}
