//! `minecraft:block_state` and `minecraft:block_predicate` (Vanilla's `BlockStateParser`):
//! `id[prop=value,...]{nbt}` and, for predicates, `#tag[prop=value,...]{nbt}`.

use vanillite_data::block::{BlockId, BlockState, PROPERTIES, PropertyInfo};
use vanillite_nbt::Compound;

use crate::argument::{ArgValue, ParseCtx};
use crate::error::{Arg, CommandError};
use crate::game_values::{BlockPredicateArg, BlockStateArg, GameValue, ResourceOrTag};
use crate::reader::StringReader;
use crate::snbt;
use crate::suggestion::{Suggestions, SuggestionsBuilder};
use crate::values::parse_identifier;

const BLOCK_REGISTRY: &str = "minecraft:block";

/// What may follow where parsing stopped (`BlockStateParser.suggestions`).
#[derive(Debug, Clone)]
enum Suggest {
    Nothing,
    Block,
    BlockOrTag,
    Tag,
    OpenVaguePropertiesOrNbt,
    OpenPropertiesOrNbt,
    OpenNbt,
    PropertyNameOrEnd,
    VaguePropertyNameOrEnd,
    PropertyName,
    VaguePropertyName,
    Equals,
    PropertyValue(&'static PropertyInfo),
    VaguePropertyValue(String),
    NextPropertyOrEnd,
}

struct Parser<'r, 'a, 's> {
    r: &'r mut StringReader<'a>,
    ctx: ParseCtx<'s>,
    for_testing: bool,
    /// The block id as typed (`minecraft:` for tags, like Vanilla's empty default).
    id: String,
    block: Option<BlockId>,
    state: Option<BlockState>,
    properties: Vec<(String, String)>,
    vague: Vec<(String, String)>,
    tag: Option<String>,
    tag_blocks: Vec<BlockId>,
    nbt: Option<Compound>,
    suggest: Suggest,
}

/// The properties of a block in Vanilla's order.
fn block_properties(block: BlockId) -> impl Iterator<Item = &'static PropertyInfo> {
    block
        .info()
        .properties
        .iter()
        .filter_map(|&p| PROPERTIES.get(usize::from(p)))
}

fn property(block: BlockId, name: &str) -> Option<&'static PropertyInfo> {
    block_properties(block).find(|p| p.name == name)
}

/// Whether the property is an `IntegerProperty` (its values are numbers).
fn is_int_property(p: &PropertyInfo) -> bool {
    p.values.iter().all(|v| v.parse::<i32>().is_ok())
}

/// `Property.getValue`: the canonical value name (integers are parsed).
fn property_value(p: &'static PropertyInfo, raw: &str) -> Option<&'static str> {
    if is_int_property(p) {
        let n = raw.parse::<i32>().ok()?;
        p.values
            .iter()
            .copied()
            .find(|v| v.parse::<i32>().ok() == Some(n))
    } else {
        p.values.iter().copied().find(|v| *v == raw)
    }
}

fn has_block_entity(block: BlockId) -> bool {
    block.info().block_entity != u16::MAX
}

impl<'r, 'a, 's> Parser<'r, 'a, 's> {
    fn new(r: &'r mut StringReader<'a>, ctx: ParseCtx<'s>, for_testing: bool) -> Self {
        Self {
            r,
            ctx,
            for_testing,
            id: "minecraft:".to_owned(),
            block: None,
            state: None,
            properties: Vec::new(),
            vague: Vec::new(),
            tag: None,
            tag_blocks: Vec::new(),
            nbt: None,
            suggest: Suggest::Nothing,
        }
    }

    fn parse(&mut self) -> Result<(), CommandError> {
        self.suggest = if self.for_testing {
            Suggest::BlockOrTag
        } else {
            Suggest::Block
        };
        if self.r.peek_is('#') {
            self.read_tag()?;
            self.suggest = Suggest::OpenVaguePropertiesOrNbt;
            if self.r.peek_is('[') {
                self.read_vague_properties()?;
                self.suggest = Suggest::OpenNbt;
            }
        } else {
            self.read_block()?;
            self.suggest = Suggest::OpenPropertiesOrNbt;
            if self.r.peek_is('[') {
                self.read_properties()?;
                self.suggest = Suggest::OpenNbt;
            }
        }
        if self.r.peek_is('{') {
            self.suggest = Suggest::Nothing;
            self.nbt = Some(snbt::parse_compound(self.r, self.ctx.limits)?);
        }
        Ok(())
    }

    fn read_block(&mut self) -> Result<(), CommandError> {
        let start = self.r.cursor();
        self.id = parse_identifier(self.r)?;
        let Some(block) = BlockId::by_name(&self.id) else {
            self.r.set_cursor(start);
            return Err(CommandError::new(
                "argument.block.id.invalid",
                vec![Arg::from(self.id.as_str())],
            )
            .at(self.r));
        };
        self.block = Some(block);
        self.state = Some(block.default_state());
        Ok(())
    }

    fn read_tag(&mut self) -> Result<(), CommandError> {
        if !self.for_testing {
            return Err(CommandError::new("argument.block.tag.disallowed", vec![]).at(self.r));
        }
        let start = self.r.cursor();
        self.r.expect('#')?;
        self.suggest = Suggest::Tag;
        let id = parse_identifier(self.r)?;
        if !self.ctx.source.tag_exists(BLOCK_REGISTRY, &id) {
            self.r.set_cursor(start);
            return Err(
                CommandError::new("arguments.block.tag.unknown", vec![Arg::from(id)]).at(self.r),
            );
        }
        self.tag_blocks = self
            .ctx
            .source
            .tag_entries(BLOCK_REGISTRY, &id)
            .iter()
            .filter_map(|b| BlockId::by_name(b))
            .collect();
        self.tag = Some(id);
        Ok(())
    }

    fn read_properties(&mut self) -> Result<(), CommandError> {
        let Some(block) = self.block else {
            return Ok(());
        };
        self.r.skip();
        self.suggest = Suggest::PropertyNameOrEnd;
        self.r.skip_whitespace();
        while self.r.can_read() && !self.r.peek_is(']') {
            self.r.skip_whitespace();
            let key_start = self.r.cursor();
            let key = self.r.read_string()?;
            let Some(prop) = property(block, &key) else {
                self.r.set_cursor(key_start);
                return Err(CommandError::new(
                    "argument.block.property.unknown",
                    vec![Arg::from(self.id.as_str()), Arg::from(key)],
                )
                .at(self.r));
            };
            if self.properties.iter().any(|(k, _)| *k == prop.name) {
                self.r.set_cursor(key_start);
                return Err(CommandError::new(
                    "argument.block.property.duplicate",
                    vec![Arg::from(key), Arg::from(self.id.as_str())],
                )
                .at(self.r));
            }
            self.r.skip_whitespace();
            self.suggest = Suggest::Equals;
            if !self.r.peek_is('=') {
                return Err(CommandError::new(
                    "argument.block.property.novalue",
                    vec![Arg::from(key), Arg::from(self.id.as_str())],
                )
                .at(self.r));
            }
            self.r.skip();
            self.r.skip_whitespace();
            self.suggest = Suggest::PropertyValue(prop);
            let start = self.r.cursor();
            let raw = self.r.read_string()?;
            let Some(value) = property_value(prop, &raw) else {
                self.r.set_cursor(start);
                return Err(CommandError::new(
                    "argument.block.property.invalid",
                    vec![
                        Arg::from(self.id.as_str()),
                        Arg::from(raw),
                        Arg::from(prop.name),
                    ],
                )
                .at(self.r));
            };
            self.state = self.state.and_then(|s| s.with(prop.name, value));
            self.properties
                .push((prop.name.to_owned(), value.to_owned()));
            self.suggest = Suggest::NextPropertyOrEnd;
            self.r.skip_whitespace();
            if self.r.can_read() {
                if !self.r.peek_is(',') {
                    if !self.r.peek_is(']') {
                        return Err(
                            CommandError::new("argument.block.property.unclosed", vec![])
                                .at(self.r),
                        );
                    }
                    break;
                }
                self.r.skip();
                self.suggest = Suggest::PropertyName;
            }
        }
        if self.r.can_read() {
            self.r.skip();
            Ok(())
        } else {
            Err(CommandError::new("argument.block.property.unclosed", vec![]).at(self.r))
        }
    }

    fn read_vague_properties(&mut self) -> Result<(), CommandError> {
        self.r.skip();
        self.suggest = Suggest::VaguePropertyNameOrEnd;
        let mut value_start: Option<usize> = None;
        self.r.skip_whitespace();
        while self.r.can_read() && !self.r.peek_is(']') {
            self.r.skip_whitespace();
            let key_start = self.r.cursor();
            let key = self.r.read_string()?;
            if self.vague.iter().any(|(k, _)| *k == key) {
                self.r.set_cursor(key_start);
                return Err(CommandError::new(
                    "argument.block.property.duplicate",
                    vec![Arg::from(key), Arg::from(self.id.as_str())],
                )
                .at(self.r));
            }
            self.r.skip_whitespace();
            if !self.r.peek_is('=') {
                self.r.set_cursor(key_start);
                return Err(CommandError::new(
                    "argument.block.property.novalue",
                    vec![Arg::from(key), Arg::from(self.id.as_str())],
                )
                .at(self.r));
            }
            self.r.skip();
            self.r.skip_whitespace();
            self.suggest = Suggest::VaguePropertyValue(key.clone());
            value_start = Some(self.r.cursor());
            let value = self.r.read_string()?;
            self.vague.push((key, value));
            self.r.skip_whitespace();
            if self.r.can_read() {
                value_start = None;
                if !self.r.peek_is(',') {
                    if !self.r.peek_is(']') {
                        return Err(
                            CommandError::new("argument.block.property.unclosed", vec![])
                                .at(self.r),
                        );
                    }
                    break;
                }
                self.r.skip();
                self.suggest = Suggest::VaguePropertyName;
            }
        }
        if self.r.can_read() {
            self.r.skip();
            Ok(())
        } else {
            if let Some(start) = value_start {
                self.r.set_cursor(start);
            }
            Err(CommandError::new("argument.block.property.unclosed", vec![]).at(self.r))
        }
    }

    /// `parser.suggestions.apply(builder.createOffset(reader.getCursor()))`.
    fn suggestions(&self, builder: &SuggestionsBuilder<'_>) -> Suggestions {
        let mut b = builder.offset(self.r.cursor());
        let empty = b.remaining().is_empty();
        let prefix = b.remaining_lower().to_owned();
        match &self.suggest {
            Suggest::Nothing => {}
            Suggest::Block => suggest_blocks(&mut b),
            Suggest::BlockOrTag => {
                self.suggest_tags(&mut b);
                suggest_blocks(&mut b);
            }
            Suggest::Tag => self.suggest_tags(&mut b),
            Suggest::OpenVaguePropertiesOrNbt => {
                if empty {
                    let props = self
                        .tag_blocks
                        .iter()
                        .any(|b| block_properties(*b).next().is_some());
                    let entity = self.tag_blocks.iter().any(|b| has_block_entity(*b));
                    if props {
                        b.suggest("[");
                    }
                    if entity {
                        b.suggest("{");
                    }
                }
            }
            Suggest::OpenPropertiesOrNbt => {
                if empty && let Some(block) = self.block {
                    if block_properties(block).next().is_some() {
                        b.suggest("[");
                    }
                    if has_block_entity(block) {
                        b.suggest("{");
                    }
                }
            }
            Suggest::OpenNbt => {
                let entity = match self.block {
                    Some(block) => has_block_entity(block),
                    None => self.tag_blocks.iter().any(|b| has_block_entity(*b)),
                };
                if empty && entity {
                    b.suggest("{");
                }
            }
            Suggest::PropertyNameOrEnd | Suggest::PropertyName => {
                if matches!(self.suggest, Suggest::PropertyNameOrEnd) && empty {
                    b.suggest("]");
                }
                if let Some(block) = self.block {
                    for p in block_properties(block) {
                        if !self.properties.iter().any(|(k, _)| k == p.name)
                            && p.name.starts_with(prefix.as_str())
                        {
                            b.suggest(format!("{}=", p.name));
                        }
                    }
                }
            }
            Suggest::VaguePropertyNameOrEnd | Suggest::VaguePropertyName => {
                if matches!(self.suggest, Suggest::VaguePropertyNameOrEnd) && empty {
                    b.suggest("]");
                }
                for block in &self.tag_blocks {
                    for p in block_properties(*block) {
                        if !self.vague.iter().any(|(k, _)| k == p.name)
                            && p.name.starts_with(prefix.as_str())
                        {
                            b.suggest(format!("{}=", p.name));
                        }
                    }
                }
            }
            Suggest::Equals => {
                if empty {
                    b.suggest("=");
                }
            }
            Suggest::PropertyValue(p) => add_values(&mut b, p),
            Suggest::VaguePropertyValue(key) => {
                let mut more = false;
                for block in &self.tag_blocks {
                    if let Some(p) = property(*block, key) {
                        add_values(&mut b, p);
                    }
                    if !more {
                        more = block_properties(*block)
                            .any(|p| !self.vague.iter().any(|(k, _)| k == p.name));
                    }
                }
                if more {
                    b.suggest(",");
                }
                b.suggest("]");
            }
            Suggest::NextPropertyOrEnd => {
                if empty {
                    b.suggest("]");
                    let count = self.block.map_or(0, |b| block_properties(b).count());
                    if self.properties.len() < count {
                        b.suggest(",");
                    }
                }
            }
        }
        b.build()
    }

    fn suggest_tags(&self, b: &mut SuggestionsBuilder<'_>) {
        let tags: Vec<String> = self
            .ctx
            .source
            .registry_tags(BLOCK_REGISTRY)
            .iter()
            .map(|t| crate::game_args::full_id(t))
            .collect();
        b.suggest_resources(tags.iter().map(String::as_str), "#");
    }
}

fn suggest_blocks(b: &mut SuggestionsBuilder<'_>) {
    b.suggest_resource_ids(vanillite_data::registry::BLOCK.entries.iter().copied());
}

/// `addSuggestions`: integer values as integer suggestions.
fn add_values(b: &mut SuggestionsBuilder<'_>, p: &PropertyInfo) {
    for v in p.values {
        match v.parse::<i32>() {
            Ok(n) if is_int_property(p) => {
                b.suggest_int(n);
            }
            _ => {
                b.suggest(*v);
            }
        }
    }
}

/// `BlockStateArgument`: a block (no tags) with properties and block entity data.
pub(crate) fn parse_state(
    r: &mut StringReader<'_>,
    ctx: ParseCtx<'_>,
) -> Result<ArgValue, CommandError> {
    let start = r.cursor();
    let mut p = Parser::new(r, ctx, false);
    if let Err(e) = p.parse() {
        p.r.set_cursor(start);
        return Err(e);
    }
    let block = p.block.map_or(BlockState::AIR, BlockId::default_state);
    Ok(ArgValue::Game(GameValue::BlockState(Box::new(
        BlockStateArg {
            block: p.id.clone(),
            state: p.state.unwrap_or(block).0,
            properties: std::mem::take(&mut p.properties),
            nbt: p.nbt.take(),
        },
    ))))
}

/// `BlockPredicateArgument`: a block or `#tag` with properties and block entity data.
pub(crate) fn parse_predicate(
    r: &mut StringReader<'_>,
    ctx: ParseCtx<'_>,
) -> Result<ArgValue, CommandError> {
    let start = r.cursor();
    let mut p = Parser::new(r, ctx, true);
    if let Err(e) = p.parse() {
        p.r.set_cursor(start);
        return Err(e);
    }
    let (block, properties) = match p.tag.take() {
        Some(tag) => (ResourceOrTag::Tag(tag), std::mem::take(&mut p.vague)),
        None => (
            ResourceOrTag::Resource(p.id.clone()),
            std::mem::take(&mut p.properties),
        ),
    };
    Ok(ArgValue::Game(GameValue::BlockPredicate(Box::new(
        BlockPredicateArg {
            block,
            properties,
            nbt: p.nbt.take(),
        },
    ))))
}

/// `BlockStateParser.fillSuggestions`.
pub(crate) fn suggestions(
    builder: &SuggestionsBuilder<'_>,
    ctx: ParseCtx<'_>,
    for_testing: bool,
) -> Suggestions {
    let mut r = StringReader::new(builder.input());
    r.set_cursor(builder.start());
    let mut p = Parser::new(&mut r, ctx, for_testing);
    let _ = p.parse();
    p.suggestions(builder)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::Limits;
    use crate::source::Source;

    struct S;

    impl Source for S {
        fn permission_level(&self) -> u8 {
            4
        }

        fn online_player_names(&self) -> Vec<String> {
            Vec::new()
        }

        fn registry_tags(&self, registry: &str) -> Vec<String> {
            if registry == BLOCK_REGISTRY {
                vec!["minecraft:logs".into(), "minecraft:beds".into()]
            } else {
                Vec::new()
            }
        }

        fn tag_entries(&self, _registry: &str, tag: &str) -> Vec<String> {
            match tag {
                "minecraft:logs" => vec!["minecraft:oak_log".into(), "minecraft:birch_log".into()],
                "minecraft:beds" => vec!["minecraft:chest".into()],
                _ => Vec::new(),
            }
        }
    }

    fn run(predicate: bool, input: &str) -> (Result<ArgValue, CommandError>, usize) {
        let limits = Limits::default();
        let ctx = ParseCtx {
            source: &S,
            limits: &limits,
        };
        let mut r = StringReader::new(input);
        let v = if predicate {
            parse_predicate(&mut r, ctx)
        } else {
            parse_state(&mut r, ctx)
        };
        (v, r.cursor())
    }

    fn err(predicate: bool, input: &str) -> (&'static str, Vec<Arg>, Option<usize>) {
        let e = run(predicate, input)
            .0
            .err()
            .unwrap_or_else(|| panic!("{input} parsed"));
        (e.key, e.args, e.cursor)
    }

    fn state(input: &str) -> BlockStateArg {
        match run(false, input) {
            (Ok(ArgValue::Game(GameValue::BlockState(s))), _) => *s,
            other => panic!("{input}: {other:?}"),
        }
    }

    fn suggest(predicate: bool, input: &str) -> Vec<String> {
        let limits = Limits::default();
        let ctx = ParseCtx {
            source: &S,
            limits: &limits,
        };
        suggestions(&SuggestionsBuilder::new(input, 0), ctx, predicate)
            .texts()
            .into_iter()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn parses_states() {
        let s = state("oak_stairs[facing=east, half=top]{a:1}");
        assert_eq!(s.block, "minecraft:oak_stairs");
        let expected = BlockId::by_name("minecraft:oak_stairs")
            .map(BlockId::default_state)
            .and_then(|s| s.with("facing", "east"))
            .and_then(|s| s.with("half", "top"));
        assert_eq!(Some(BlockState(s.state)), expected);
        assert_eq!(s.properties.len(), 2);
        assert!(s.nbt.is_some());
        let (v, c) = run(false, "stone rest");
        assert!(v.is_ok());
        assert_eq!(c, 5);
        let s = state("wheat[age=05]");
        assert_eq!(BlockState(s.state).value("age"), Some("5"));
    }

    #[test]
    fn errors_like_vanilla() {
        assert_eq!(
            err(false, "nope"),
            (
                "argument.block.id.invalid",
                vec![Arg::from("minecraft:nope")],
                Some(0)
            )
        );
        assert_eq!(err(false, "#logs").0, "argument.block.tag.disallowed");
        assert_eq!(
            err(false, "stone[foo=1]"),
            (
                "argument.block.property.unknown",
                vec![Arg::from("minecraft:stone"), Arg::from("foo")],
                Some(6)
            )
        );
        assert_eq!(err(false, "oak_stairs[half=top,half=top]").2, Some(20));
        assert_eq!(
            err(false, "oak_stairs[half=top,half=top]").0,
            "argument.block.property.duplicate"
        );
        assert_eq!(
            err(false, "oak_stairs[half=left]"),
            (
                "argument.block.property.invalid",
                vec![
                    Arg::from("minecraft:oak_stairs"),
                    Arg::from("left"),
                    Arg::from("half")
                ],
                Some(16)
            )
        );
        assert_eq!(
            err(false, "oak_stairs[half]"),
            (
                "argument.block.property.novalue",
                vec![Arg::from("half"), Arg::from("minecraft:oak_stairs")],
                Some(15)
            )
        );
        assert_eq!(
            err(false, "oak_stairs[half=top").0,
            "argument.block.property.unclosed"
        );
        assert_eq!(
            err(false, "oak_stairs[half=top x]").0,
            "argument.block.property.unclosed"
        );
    }

    #[test]
    fn predicates() {
        match run(true, "#logs[axis=y]") {
            (Ok(ArgValue::Game(GameValue::BlockPredicate(p))), 13) => {
                assert_eq!(p.block, ResourceOrTag::Tag("minecraft:logs".into()));
                assert_eq!(p.properties, vec![("axis".to_owned(), "y".to_owned())]);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            err(true, "#nope"),
            (
                "arguments.block.tag.unknown",
                vec![Arg::from("minecraft:nope")],
                Some(0)
            )
        );
        assert_eq!(
            err(true, "#logs[axis=y,axis=x]"),
            (
                "argument.block.property.duplicate",
                vec![Arg::from("axis"), Arg::from("minecraft:")],
                Some(13)
            )
        );
        assert_eq!(
            err(true, "#logs[axis=y"),
            ("argument.block.property.unclosed", Vec::new(), Some(11))
        );
    }

    #[test]
    fn suggests_like_vanilla() {
        assert!(suggest(false, "oak_sta").contains(&"minecraft:oak_stairs".to_owned()));
        assert!(suggest(true, "").contains(&"#minecraft:logs".to_owned()));
        assert!(!suggest(false, "").contains(&"#minecraft:logs".to_owned()));
        assert_eq!(suggest(false, "stone"), Vec::<String>::new());
        assert_eq!(suggest(false, "chest"), ["[", "{"]);
        assert_eq!(suggest(false, "oak_stairs[half"), ["="]);
        assert_eq!(suggest(false, "oak_stairs[ha"), ["half="]);
        assert_eq!(suggest(false, "oak_stairs[half="), ["bottom", "top"]);
        assert_eq!(suggest(false, "oak_stairs[half=top"), [",", "]"]);
        let ages = suggest(false, "wheat[age=");
        assert_eq!(ages, ["0", "1", "2", "3", "4", "5", "6", "7"]);
        let ages = suggest(false, "cactus[age=");
        assert_eq!(ages.get(2).map(String::as_str), Some("2"));
        assert_eq!(suggest(true, "#logs"), ["["]);
        assert_eq!(suggest(true, "#beds"), ["[", "{"]);
        assert_eq!(suggest(true, "#logs[axis="), ["]", "x", "y", "z"]);
    }
}
