//! Syntax of `data` and `tag`.

use vanillite_command::{ArgumentType, Builder, Dispatcher, StringKind, argument, literal};

use super::{CommandId, DataOp, ModifyOp, ModifySource, TagOp, level, suggest};

fn entity() -> ArgumentType {
    ArgumentType::Entity {
        single: true,
        players_only: false,
    }
}

fn int() -> ArgumentType {
    ArgumentType::Integer {
        min: None,
        max: None,
    }
}

fn data(op: DataOp) -> CommandId {
    CommandId::Data(op)
}

/// `ArgProvider.wrap` of the `DataCommands` providers (entity, block, storage): `parent` gets
/// `<kind> <name>` for each, with `f` applied to the argument node.
fn providers(
    parent: Builder<CommandId>,
    name: &str,
    f: &dyn Fn(Builder<CommandId>) -> Builder<CommandId>,
) -> Builder<CommandId> {
    parent
        .then(literal("entity").then(f(argument(name, entity()))))
        .then(literal("block").then(f(argument(name, ArgumentType::BlockPos))))
        .then(literal("storage").then(f(
            argument(name, ArgumentType::Identifier).suggests(suggest::STORAGE),
        )))
}

/// `LootContextSources.addContextSources` with the float and integer providers.
fn compute(id: CommandId) -> Builder<CommandId> {
    let providers = |node: Builder<CommandId>| {
        node.then(
            literal("float")
                .then(argument("provider", ArgumentType::ContextFloatProvider).executes(id)),
        )
        .then(
            literal("integer")
                .then(argument("provider", ArgumentType::ContextIntProvider).executes(id)),
        )
    };
    literal("compute")
        .then(providers(literal("default")))
        .then(literal("block").then(providers(argument("computePos", ArgumentType::BlockPos))))
        .then(literal("entity").then(providers(argument("computeTarget", entity()))))
}

/// The sources of one `data modify` operation, in Vanilla's order: `from` and `string` of
/// the source providers, `compute`, `value`.
fn modify_sources(node: Builder<CommandId>, op: ModifyOp) -> Builder<CommandId> {
    let from = data(DataOp::Modify(op, ModifySource::From));
    let string = data(DataOp::Modify(op, ModifySource::String));
    node.then(providers(literal("from"), "source", &|s| {
        s.executes(from)
            .then(argument("sourcePath", ArgumentType::NbtPath).executes(from))
    }))
    .then(providers(literal("string"), "source", &|s| {
        s.executes(string).then(
            argument("sourcePath", ArgumentType::NbtPath)
                .executes(string)
                .then(
                    argument("start", int())
                        .executes(string)
                        .then(argument("end", int()).executes(string)),
                ),
        )
    }))
    .then(compute(data(DataOp::Modify(op, ModifySource::Compute))))
    .then(
        literal("value").then(
            argument("value", ArgumentType::NbtTag)
                .executes(data(DataOp::Modify(op, ModifySource::Value))),
        ),
    )
}

/// `DataCommands.register`.
pub(super) fn register_data(d: &mut Dispatcher<CommandId>) {
    let target_path = argument("targetPath", ArgumentType::NbtPath)
        .then(literal("insert").then(modify_sources(argument("index", int()), ModifyOp::Insert)))
        .then(modify_sources(literal("prepend"), ModifyOp::Prepend))
        .then(modify_sources(literal("append"), ModifyOp::Append))
        .then(modify_sources(literal("set"), ModifyOp::Set))
        .then(modify_sources(literal("merge"), ModifyOp::Merge));
    let get = data(DataOp::Get);
    let scale = ArgumentType::Double {
        min: None,
        max: None,
    };
    d.register(
        literal("data")
            .requires(level::GAMEMASTERS)
            .then(providers(literal("merge"), "target", &|t| {
                t.then(argument("nbt", ArgumentType::NbtCompound).executes(data(DataOp::Merge)))
            }))
            .then(providers(literal("get"), "target", &|t| {
                t.executes(get).then(
                    argument("path", ArgumentType::NbtPath)
                        .executes(get)
                        .then(argument("scale", scale.clone()).executes(get)),
                )
            }))
            .then(providers(literal("remove"), "target", &|t| {
                t.then(argument("path", ArgumentType::NbtPath).executes(data(DataOp::Remove)))
            }))
            .then(providers(literal("modify"), "target", &|t| {
                t.then(target_path.clone())
            })),
    );
}

/// `TagCommand.register`.
pub(super) fn register_tag(d: &mut Dispatcher<CommandId>) {
    let word = || ArgumentType::String(StringKind::SingleWord);
    d.register(
        literal("tag").requires(level::GAMEMASTERS).then(
            argument(
                "targets",
                ArgumentType::Entity {
                    single: false,
                    players_only: false,
                },
            )
            .then(
                literal("add").then(argument("name", word()).executes(CommandId::Tag(TagOp::Add))),
            )
            .then(
                literal("remove").then(
                    argument("name", word())
                        .suggests(suggest::ENTITY_TAGS)
                        .executes(CommandId::Tag(TagOp::Remove)),
                ),
            )
            .then(literal("list").executes(CommandId::Tag(TagOp::List))),
        ),
    );
}
