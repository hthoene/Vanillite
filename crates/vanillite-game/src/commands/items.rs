//! Syntax of the item and player data commands: attribute, clear, enchant, experience, give,
//! item, recipe.

use vanillite_command::{ArgumentType, Builder, Dispatcher, argument, literal};

use super::{
    AttributeOp, CommandId, ExperienceOp, ItemOp, LootSource, LootTarget, LootTool,
    ModifierOperation, level, suggest,
};

fn players() -> ArgumentType {
    ArgumentType::Entity {
        single: false,
        players_only: true,
    }
}

fn entities() -> ArgumentType {
    ArgumentType::Entity {
        single: false,
        players_only: false,
    }
}

fn double() -> ArgumentType {
    ArgumentType::Double {
        min: None,
        max: None,
    }
}

fn attribute(op: AttributeOp) -> CommandId {
    CommandId::Attribute(op)
}

/// `AttributeCommand.register`.
pub(super) fn register_attribute(d: &mut Dispatcher<CommandId>) {
    use AttributeOp as A;
    let operation =
        |name: &str, op: ModifierOperation| literal(name).executes(attribute(A::ModifierAdd(op)));
    d.register(
        literal("attribute").requires(level::GAMEMASTERS).then(
            argument(
                "target",
                ArgumentType::Entity {
                    single: true,
                    players_only: false,
                },
            )
            .then(
                argument(
                    "attribute",
                    ArgumentType::Resource {
                        registry: "minecraft:attribute",
                    },
                )
                .then(
                    literal("get")
                        .executes(attribute(A::Get))
                        .then(argument("scale", double()).executes(attribute(A::Get))),
                )
                .then(
                    literal("base")
                        .then(
                            literal("set")
                                .then(argument("value", double()).executes(attribute(A::BaseSet))),
                        )
                        .then(
                            literal("get")
                                .executes(attribute(A::BaseGet))
                                .then(argument("scale", double()).executes(attribute(A::BaseGet))),
                        )
                        .then(literal("reset").executes(attribute(A::BaseReset))),
                )
                .then(
                    literal("modifier")
                        .then(
                            literal("add").then(
                                argument("id", ArgumentType::Identifier).then(
                                    argument("value", double())
                                        .then(operation("add_value", ModifierOperation::AddValue))
                                        .then(operation(
                                            "add_multiplied_base",
                                            ModifierOperation::AddMultipliedBase,
                                        ))
                                        .then(operation(
                                            "add_multiplied_total",
                                            ModifierOperation::AddMultipliedTotal,
                                        )),
                                ),
                            ),
                        )
                        .then(
                            literal("remove").then(
                                argument("id", ArgumentType::Identifier)
                                    .suggests(suggest::ATTRIBUTE_MODIFIERS)
                                    .executes(attribute(A::ModifierRemove)),
                            ),
                        )
                        .then(
                            literal("value").then(
                                literal("get").then(
                                    argument("id", ArgumentType::Identifier)
                                        .suggests(suggest::ATTRIBUTE_MODIFIERS)
                                        .executes(attribute(A::ModifierValueGet))
                                        .then(
                                            argument("scale", double())
                                                .executes(attribute(A::ModifierValueGet)),
                                        ),
                                ),
                            ),
                        ),
                ),
            ),
        ),
    );
}

/// `ClearInventoryCommands.register`.
pub(super) fn register_clear(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("clear")
            .requires(level::GAMEMASTERS)
            .executes(CommandId::Clear)
            .then(
                argument("targets", players())
                    .executes(CommandId::Clear)
                    .then(
                        argument("item", ArgumentType::ItemPredicate)
                            .executes(CommandId::Clear)
                            .then(
                                argument(
                                    "maxCount",
                                    ArgumentType::Integer {
                                        min: Some(0),
                                        max: None,
                                    },
                                )
                                .executes(CommandId::Clear),
                            ),
                    ),
            ),
    );
}

/// `EnchantCommand.register`.
pub(super) fn register_enchant(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("enchant").requires(level::GAMEMASTERS).then(
            argument("targets", entities()).then(
                argument(
                    "enchantment",
                    ArgumentType::Resource {
                        registry: "minecraft:enchantment",
                    },
                )
                .executes(CommandId::Enchant)
                .then(
                    argument(
                        "level",
                        ArgumentType::Integer {
                            min: Some(0),
                            max: None,
                        },
                    )
                    .executes(CommandId::Enchant),
                ),
            ),
        ),
    );
}

/// `ExperienceCommand.register`.
pub(super) fn register_experience(d: &mut Dispatcher<CommandId>) {
    let xp = |op: ExperienceOp, levels: bool| CommandId::Experience { op, levels };
    let change = |name: &str, op: ExperienceOp, min: Option<i32>| {
        literal(name).then(
            argument("target", players()).then(
                argument("amount", ArgumentType::Integer { min, max: None })
                    .executes(xp(op, false))
                    .then(literal("points").executes(xp(op, false)))
                    .then(literal("levels").executes(xp(op, true))),
            ),
        )
    };
    let experience = d.register(
        literal("experience")
            .requires(level::GAMEMASTERS)
            .then(change("add", ExperienceOp::Add, None))
            .then(change("set", ExperienceOp::Set, Some(0)))
            .then(
                literal("query").then(
                    argument(
                        "target",
                        ArgumentType::Entity {
                            single: true,
                            players_only: true,
                        },
                    )
                    .then(literal("points").executes(xp(ExperienceOp::Query, false)))
                    .then(literal("levels").executes(xp(ExperienceOp::Query, true))),
                ),
            ),
    );
    d.register(
        literal("xp")
            .requires(level::GAMEMASTERS)
            .redirect(experience),
    );
}

/// `GiveCommand.register`.
pub(super) fn register_give(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("give").requires(level::GAMEMASTERS).then(
            argument("targets", players()).then(
                argument("item", ArgumentType::ItemStack)
                    .executes(CommandId::Give)
                    .then(
                        argument(
                            "count",
                            ArgumentType::Integer {
                                min: Some(1),
                                max: None,
                            },
                        )
                        .executes(CommandId::Give),
                    ),
            ),
        ),
    );
}

/// `ArgProvider.wrap` of the `ItemCommands` providers (entity, block).
fn item_providers(
    parent: Builder<CommandId>,
    name: &str,
    f: &dyn Fn(Builder<CommandId>) -> Builder<CommandId>,
) -> Builder<CommandId> {
    parent
        .then(literal("entity").then(f(argument(name, entities()))))
        .then(literal("block").then(f(argument(name, ArgumentType::BlockPos))))
}

/// `ItemCommands.wrapSetItems`.
fn set_items(name: &str, op: ItemOp) -> Builder<CommandId> {
    let id = CommandId::Item(op);
    item_providers(literal(name), "target", &|target| {
        let slots = argument("slots", ArgumentType::SlotSource).then(item_providers(
            literal("from"),
            "source",
            &|source| {
                source.then(
                    argument("sourceSlots", ArgumentType::SlotSource)
                        .executes(id)
                        .then(argument("modifier", ArgumentType::LootModifier).executes(id)),
                )
            },
        ));
        target.then(
            slots.then(
                literal("with").then(
                    argument("item", ArgumentType::ItemStack).executes(id).then(
                        argument(
                            "count",
                            ArgumentType::Integer {
                                min: Some(1),
                                max: Some(99),
                            },
                        )
                        .executes(id),
                    ),
                ),
            ),
        )
    })
}

/// `ItemCommands.register`.
pub(super) fn register_item(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("item")
            .requires(level::GAMEMASTERS)
            .then(set_items("replace", ItemOp::Replace))
            .then(set_items("fill", ItemOp::Fill))
            .then(set_items("override", ItemOp::Override))
            .then(item_providers(literal("modify"), "target", &|target| {
                target.then(
                    argument("slots", ArgumentType::SlotSource).then(
                        argument("modifier", ArgumentType::LootModifier)
                            .executes(CommandId::Item(ItemOp::Modify)),
                    ),
                )
            })),
    );
}

/// `RecipeCommand.register`.
pub(super) fn register_recipe(d: &mut Dispatcher<CommandId>) {
    let action = |name: &str, id: CommandId| {
        literal(name).then(
            argument("targets", players())
                .then(
                    argument(
                        "recipe",
                        ArgumentType::ResourceKey {
                            registry: "minecraft:recipe",
                        },
                    )
                    .executes(id),
                )
                .then(literal("*").executes(id)),
        )
    };
    d.register(
        literal("recipe")
            .requires(level::GAMEMASTERS)
            .then(action("give", CommandId::RecipeGive))
            .then(action("take", CommandId::RecipeTake)),
    );
}

/// `LootCommand`'s sources after a target node (`TailProvider`).
fn loot_tail(node: Builder<CommandId>, target: LootTarget) -> Builder<CommandId> {
    let id = |source| CommandId::Loot { target, source };
    let tools = |node: Builder<CommandId>, tool: fn(LootTool) -> LootSource| {
        node.executes(id(tool(LootTool::Empty)))
            .then(argument("tool", ArgumentType::ItemStack).executes(id(tool(LootTool::Item))))
            .then(literal("mainhand").executes(id(tool(LootTool::MainHand))))
            .then(literal("offhand").executes(id(tool(LootTool::OffHand))))
    };
    node.then(
        literal("fish").then(argument("loot_table", ArgumentType::LootTable).then(tools(
            argument("pos", ArgumentType::BlockPos),
            LootSource::Fish,
        ))),
    )
    .then(
        literal("loot")
            .then(argument("loot_table", ArgumentType::LootTable).executes(id(LootSource::Loot))),
    )
    .then(
        literal("kill").then(
            argument(
                "target",
                ArgumentType::Entity {
                    single: true,
                    players_only: false,
                },
            )
            .executes(id(LootSource::Kill)),
        ),
    )
    .then(literal("mine").then(tools(
        argument("pos", ArgumentType::BlockPos),
        LootSource::Mine,
    )))
}

/// `LootCommand.register` with `addTargets`.
pub(super) fn register_loot(d: &mut Dispatcher<CommandId>) {
    use LootTarget as T;
    let count = || {
        argument(
            "count",
            ArgumentType::Integer {
                min: Some(0),
                max: None,
            },
        )
    };
    d.register(
        literal("loot")
            .requires(level::GAMEMASTERS)
            .then(
                literal("replace")
                    .then(
                        literal("entity").then(
                            argument("entities", entities()).then(
                                loot_tail(
                                    argument("slot", ArgumentType::ItemSlot),
                                    T::ReplaceEntity,
                                )
                                .then(loot_tail(count(), T::ReplaceEntity)),
                            ),
                        ),
                    )
                    .then(
                        literal("block").then(
                            argument("targetPos", ArgumentType::BlockPos).then(
                                loot_tail(
                                    argument("slot", ArgumentType::ItemSlot),
                                    T::ReplaceBlock,
                                )
                                .then(loot_tail(count(), T::ReplaceBlock)),
                            ),
                        ),
                    ),
            )
            .then(literal("insert").then(loot_tail(
                argument("targetPos", ArgumentType::BlockPos),
                T::Insert,
            )))
            .then(literal("give").then(loot_tail(argument("players", players()), T::Give)))
            .then(literal("spawn").then(loot_tail(
                argument("targetPos", ArgumentType::Vec3 { center: true }),
                T::Spawn,
            ))),
    );
}
