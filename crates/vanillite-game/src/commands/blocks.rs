//! Syntax of the block and generation commands: clone, fill, fillbiome, place, setblock.

use vanillite_command::{ArgumentType, Builder, Dispatcher, argument, literal};

use super::{CloneFilter, CloneMode, CommandId, FillMode, PlaceOp, SetBlockMode, level, suggest};

const BIOMES: &str = "minecraft:worldgen/biome";

fn clone_id(filter: CloneFilter, mode: CloneMode, strict: bool) -> CommandId {
    CommandId::Clone {
        filter,
        mode,
        strict,
    }
}

/// `CloneCommands.wrapWithCloneMode`.
fn clone_modes(
    builder: Builder<CommandId>,
    filter: CloneFilter,
    strict: bool,
) -> Builder<CommandId> {
    let mode = |name: &str, mode: CloneMode| literal(name).executes(clone_id(filter, mode, strict));
    builder
        .executes(clone_id(filter, CloneMode::Normal, strict))
        .then(mode("force", CloneMode::Force))
        .then(mode("move", CloneMode::Move))
        .then(mode("normal", CloneMode::Normal))
}

/// `CloneCommands.modeSuffix`.
fn clone_suffix(builder: Builder<CommandId>, strict: bool) -> Builder<CommandId> {
    builder
        .executes(clone_id(CloneFilter::All, CloneMode::Normal, strict))
        .then(clone_modes(literal("replace"), CloneFilter::All, strict))
        .then(clone_modes(literal("masked"), CloneFilter::Masked, strict))
        .then(literal("filtered").then(clone_modes(
            argument("filter", ArgumentType::BlockPredicate),
            CloneFilter::Filtered,
            strict,
        )))
}

/// `CloneCommands.destinationAndStrictSuffix`.
fn clone_destination() -> Builder<CommandId> {
    clone_suffix(argument("destination", ArgumentType::BlockPos), false)
        .then(clone_suffix(literal("strict"), true))
}

/// `CloneCommands.beginEndDestinationAndModeSuffix`.
fn clone_region() -> Builder<CommandId> {
    argument("begin", ArgumentType::BlockPos).then(
        argument("end", ArgumentType::BlockPos)
            .then(clone_destination())
            .then(literal("to").then(
                argument("targetDimension", ArgumentType::Dimension).then(clone_destination()),
            )),
    )
}

/// `CloneCommands`.
pub(super) fn register_clone(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("clone")
            .requires(level::GAMEMASTERS)
            .then(clone_region())
            .then(
                literal("from").then(
                    argument("sourceDimension", ArgumentType::Dimension).then(clone_region()),
                ),
            ),
    );
}

/// `FillCommand.wrapWithMode`.
fn fill_modes(builder: Builder<CommandId>) -> Builder<CommandId> {
    let mode = |name: &str, mode: FillMode| literal(name).executes(CommandId::Fill(mode));
    builder
        .executes(CommandId::Fill(FillMode::Replace))
        .then(mode("outline", FillMode::Outline))
        .then(mode("hollow", FillMode::Hollow))
        .then(mode("destroy", FillMode::Destroy))
        .then(mode("strict", FillMode::Strict))
}

/// `FillCommand`.
pub(super) fn register_fill(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("fill").requires(level::GAMEMASTERS).then(
            argument("from", ArgumentType::BlockPos).then(
                argument("to", ArgumentType::BlockPos).then(
                    fill_modes(argument("block", ArgumentType::BlockState))
                        .then(
                            literal("replace")
                                .executes(CommandId::Fill(FillMode::Replace))
                                .then(fill_modes(argument("filter", ArgumentType::BlockPredicate))),
                        )
                        .then(literal("keep").executes(CommandId::Fill(FillMode::Keep))),
                ),
            ),
        ),
    );
}

/// `FillBiomeCommand`.
pub(super) fn register_fill_biome(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("fillbiome").requires(level::GAMEMASTERS).then(
            argument("from", ArgumentType::BlockPos).then(
                argument("to", ArgumentType::BlockPos).then(
                    argument("biome", ArgumentType::Resource { registry: BIOMES })
                        .executes(CommandId::FillBiome)
                        .then(
                            literal("replace").then(
                                argument(
                                    "filter",
                                    ArgumentType::ResourceOrTag { registry: BIOMES },
                                )
                                .executes(CommandId::FillBiome),
                            ),
                        ),
                ),
            ),
        ),
    );
}

/// `PlaceCommand`.
pub(super) fn register_place(d: &mut Dispatcher<CommandId>) {
    let pos = |name: &str, op: PlaceOp| {
        argument(name, ArgumentType::BlockPos).executes(CommandId::Place(op))
    };
    let template = CommandId::Place(PlaceOp::Template);
    d.register(
        literal("place")
            .requires(level::GAMEMASTERS)
            .then(
                literal("feature").then(
                    argument("feature", ArgumentType::Feature)
                        .executes(CommandId::Place(PlaceOp::Feature))
                        .then(pos("pos", PlaceOp::Feature)),
                ),
            )
            .then(
                literal("jigsaw").then(
                    argument(
                        "pool",
                        ArgumentType::ResourceKey {
                            registry: "minecraft:worldgen/template_pool",
                        },
                    )
                    .then(
                        argument("target", ArgumentType::Identifier).then(
                            argument(
                                "max_depth",
                                ArgumentType::Integer {
                                    min: Some(1),
                                    max: Some(20),
                                },
                            )
                            .executes(CommandId::Place(PlaceOp::Jigsaw))
                            .then(pos("position", PlaceOp::Jigsaw)),
                        ),
                    ),
                ),
            )
            .then(
                literal("structure").then(
                    argument(
                        "structure",
                        ArgumentType::ResourceKey {
                            registry: "minecraft:worldgen/structure",
                        },
                    )
                    .executes(CommandId::Place(PlaceOp::Structure))
                    .then(pos("pos", PlaceOp::Structure)),
                ),
            )
            .then(
                literal("template").then(
                    argument("template", ArgumentType::Identifier)
                        .suggests(suggest::TEMPLATES)
                        .executes(template)
                        .then(
                            argument("pos", ArgumentType::BlockPos)
                                .executes(template)
                                .then(
                                    argument("rotation", ArgumentType::TemplateRotation)
                                        .executes(template)
                                        .then(
                                            argument("mirror", ArgumentType::TemplateMirror)
                                                .executes(template)
                                                .then(
                                                    argument(
                                                        "integrity",
                                                        ArgumentType::Float {
                                                            min: Some(0.0),
                                                            max: Some(1.0),
                                                        },
                                                    )
                                                    .executes(template)
                                                    .then(
                                                        argument(
                                                            "seed",
                                                            ArgumentType::Integer {
                                                                min: None,
                                                                max: None,
                                                            },
                                                        )
                                                        .executes(template)
                                                        .then(literal("strict").executes(template)),
                                                    ),
                                                ),
                                        ),
                                ),
                        ),
                ),
            ),
    );
}

/// `SetBlockCommand`.
pub(super) fn register_set_block(d: &mut Dispatcher<CommandId>) {
    let mode = |name: &str, mode: SetBlockMode| literal(name).executes(CommandId::SetBlock(mode));
    d.register(
        literal("setblock").requires(level::GAMEMASTERS).then(
            argument("pos", ArgumentType::BlockPos).then(
                argument("block", ArgumentType::BlockState)
                    .executes(CommandId::SetBlock(SetBlockMode::Replace))
                    .then(mode("destroy", SetBlockMode::Destroy))
                    .then(mode("keep", SetBlockMode::Keep))
                    .then(mode("replace", SetBlockMode::Replace))
                    .then(mode("strict", SetBlockMode::Strict)),
            ),
        ),
    );
}
