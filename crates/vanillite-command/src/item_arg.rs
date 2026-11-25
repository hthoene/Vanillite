//! `minecraft:item_stack` (Vanilla's `ItemParser`: `id[component=value,!component]`) and
//! `minecraft:item_predicate` (`ComponentPredicateParser`: `id|#tag|*` with
//! `[test,test|test,!test]`, parsed like Vanilla's packrat grammar: the errors and suggestions
//! of the furthest position win).

use vanillite_data::registry::{DATA_COMPONENT_PREDICATE_TYPE, DATA_COMPONENT_TYPE, ITEM};
use vanillite_nbt::Tag;

use crate::argument::{ArgValue, ParseCtx};
use crate::error::{Arg, CommandError};
use crate::game_args::allowed_in_identifier;
use crate::game_values::{GameValue, ItemArg, ItemPredicateArg, ItemTest, ResourceOrTag};
use crate::reader::StringReader;
use crate::snbt;
use crate::suggestion::{Suggestions, SuggestionsBuilder, resource_matches};
use crate::values::{normalize_identifier, parse_identifier};

const ITEM_REGISTRY: &str = "minecraft:item";
/// The pseudo component and predicate `minecraft:count`.
const COUNT: &str = "minecraft:count";
/// Components without a codec (`DataComponentType.isTransient`).
const TRANSIENT: [&str; 3] = [
    "minecraft:creative_slot_lock",
    "minecraft:additional_trade_cost",
    "minecraft:map_post_processing",
];

fn persistent_components() -> impl Iterator<Item = &'static str> {
    DATA_COMPONENT_TYPE
        .entries
        .iter()
        .copied()
        .filter(|c| !TRANSIENT.contains(c))
}

/// `SharedSuggestionProvider.filterResources` with a suffix.
fn suggest_ids<'s>(
    b: &mut SuggestionsBuilder<'_>,
    ids: impl IntoIterator<Item = &'s str>,
    suffix: &str,
) {
    let contents = b.remaining_lower().to_owned();
    for id in ids {
        if resource_matches(&contents, id) {
            b.suggest(format!("{id}{suffix}"));
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Item stacks.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ItemSuggest {
    Nothing,
    Item,
    StartComponents,
    AssignmentOrRemoval,
    Component,
    Assignment,
    NextOrEnd,
}

struct ItemParser<'r, 'a, 's> {
    r: &'r mut StringReader<'a>,
    ctx: ParseCtx<'s>,
    item: String,
    components: Vec<(String, Option<Tag>)>,
    suggest: ItemSuggest,
}

impl ItemParser<'_, '_, '_> {
    fn parse(&mut self) -> Result<(), CommandError> {
        self.suggest = ItemSuggest::Item;
        let start = self.r.cursor();
        let id = parse_identifier(self.r)?;
        if !self.ctx.source.resource_exists(ITEM_REGISTRY, &id) {
            self.r.set_cursor(start);
            return Err(
                CommandError::new("argument.item.id.invalid", vec![Arg::from(id)]).at(self.r),
            );
        }
        self.item = id;
        self.suggest = ItemSuggest::StartComponents;
        if self.r.peek_is('[') {
            self.suggest = ItemSuggest::Nothing;
            self.read_components()?;
        }
        Ok(())
    }

    fn read_components(&mut self) -> Result<(), CommandError> {
        self.r.expect('[')?;
        self.suggest = ItemSuggest::AssignmentOrRemoval;
        while self.r.can_read() && !self.r.peek_is(']') {
            self.r.skip_whitespace();
            if self.r.peek_is('!') {
                self.r.skip();
                self.suggest = ItemSuggest::Component;
                let id = read_component_type(self.r)?;
                self.visit(id, None)?;
                self.suggest = ItemSuggest::Nothing;
                self.r.skip_whitespace();
            } else {
                let id = read_component_type(self.r)?;
                if self.components.iter().any(|(c, _)| *c == id) {
                    return Err(repeated(&id));
                }
                self.suggest = ItemSuggest::Assignment;
                self.r.skip_whitespace();
                self.r.expect('=')?;
                self.suggest = ItemSuggest::Nothing;
                self.r.skip_whitespace();
                let value = snbt::parse_tag(self.r, self.ctx.limits)?;
                self.visit(id, Some(value))?;
                self.r.skip_whitespace();
            }
            self.suggest = ItemSuggest::NextOrEnd;
            if !self.r.peek_is(',') {
                break;
            }
            self.r.skip();
            self.r.skip_whitespace();
            self.suggest = ItemSuggest::AssignmentOrRemoval;
            if !self.r.can_read() {
                return Err(
                    CommandError::new("arguments.item.component.expected", vec![]).at(self.r),
                );
            }
        }
        self.r.expect(']')?;
        self.suggest = ItemSuggest::Nothing;
        Ok(())
    }

    fn visit(&mut self, id: String, value: Option<Tag>) -> Result<(), CommandError> {
        if self.components.iter().any(|(c, _)| *c == id) {
            return Err(repeated(&id));
        }
        self.components.push((id, value));
        Ok(())
    }

    fn suggestions(&self, builder: &SuggestionsBuilder<'_>) -> Suggestions {
        let mut b = builder.offset(self.r.cursor());
        let empty = b.remaining().is_empty();
        match self.suggest {
            ItemSuggest::Nothing => {}
            ItemSuggest::Item => {
                b.suggest_resource_ids(ITEM.entries.iter().copied());
            }
            ItemSuggest::StartComponents => {
                if empty {
                    b.suggest("[");
                }
            }
            ItemSuggest::AssignmentOrRemoval => {
                b.suggest("!");
                suggest_ids(&mut b, persistent_components(), "=");
            }
            ItemSuggest::Component => suggest_ids(&mut b, persistent_components(), ""),
            ItemSuggest::Assignment => {
                if empty {
                    b.suggest("=");
                }
            }
            ItemSuggest::NextOrEnd => {
                if empty {
                    b.suggest(",");
                    b.suggest("]");
                }
            }
        }
        b.build()
    }
}

/// `arguments.item.component.repeated` (without position, like Vanilla).
fn repeated(id: &str) -> CommandError {
    CommandError::new("arguments.item.component.repeated", vec![Arg::from(id)])
}

/// `ItemParser.State.readComponentType`.
fn read_component_type(r: &mut StringReader<'_>) -> Result<String, CommandError> {
    if !r.can_read() {
        return Err(CommandError::new("arguments.item.component.expected", vec![]).at(r));
    }
    let start = r.cursor();
    let id = parse_identifier(r)?;
    if DATA_COMPONENT_TYPE.id(&id).is_some() && !TRANSIENT.contains(&id.as_str()) {
        Ok(id)
    } else {
        r.set_cursor(start);
        Err(CommandError::new("arguments.item.component.unknown", vec![Arg::from(id)]).at(r))
    }
}

/// `ItemArgument`.
pub(crate) fn parse_item(
    r: &mut StringReader<'_>,
    ctx: ParseCtx<'_>,
) -> Result<ArgValue, CommandError> {
    let start = r.cursor();
    let mut p = ItemParser {
        r,
        ctx,
        item: String::new(),
        components: Vec::new(),
        suggest: ItemSuggest::Nothing,
    };
    if let Err(e) = p.parse() {
        p.r.set_cursor(start);
        return Err(e);
    }
    Ok(ArgValue::Game(GameValue::Item(Box::new(ItemArg {
        item: std::mem::take(&mut p.item),
        components: std::mem::take(&mut p.components),
    }))))
}

/// `ItemParser.fillSuggestions`.
pub(crate) fn item_suggestions(builder: &SuggestionsBuilder<'_>, ctx: ParseCtx<'_>) -> Suggestions {
    let mut r = StringReader::new(builder.input());
    r.set_cursor(builder.start());
    let mut p = ItemParser {
        r: &mut r,
        ctx,
        item: String::new(),
        components: Vec::new(),
        suggest: ItemSuggest::Nothing,
    };
    let _ = p.parse();
    p.suggestions(builder)
}

// ---------------------------------------------------------------------------------------------
// Item predicates.

/// What an error entry suggests (`SuggestionSupplier` / `ResourceSuggestion`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hint {
    None,
    Literal(char),
    Items,
    ItemTags,
    Components,
    Predicates,
}

/// A failed lookup: what it suggests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lookup {
    Item,
    ItemTag,
    Component,
    Predicate,
}

/// The packrat parse state: the reader and the errors of the furthest position
/// (`ErrorCollector.LongestOnly`).
struct Peg<'r, 'a, 's> {
    r: &'r mut StringReader<'a>,
    ctx: ParseCtx<'s>,
    last: Option<usize>,
    entries: Vec<(Hint, CommandError)>,
}

type Alternatives = Vec<ItemTest>;

impl Peg<'_, '_, '_> {
    fn store(&mut self, cursor: usize, hint: Hint, error: CommandError) {
        if self.last.is_none_or(|l| cursor > l) {
            self.last = Some(cursor);
            self.entries.clear();
        }
        if self.last == Some(cursor) {
            self.entries.push((hint, error));
        }
    }

    fn restore(&mut self, mark: usize) {
        self.r.set_cursor(mark);
    }

    /// `StringReaderTerms.character`.
    fn char(&mut self, c: char) -> bool {
        self.r.skip_whitespace();
        let mark = self.r.cursor();
        if self.r.peek_is(c) {
            self.r.skip();
            return true;
        }
        let error = CommandError::new("argument.literal.incorrect", vec![Arg::from(c.to_string())])
            .at_position(self.r.string(), mark);
        self.store(mark, Hint::Literal(c), error);
        false
    }

    /// `IdentifierParseRule`.
    fn id(&mut self) -> Option<String> {
        self.r.skip_whitespace();
        let start = self.r.cursor();
        let raw = self.r.read_while(allowed_in_identifier);
        let id = (!raw.is_empty())
            .then(|| normalize_identifier(raw))
            .flatten();
        if id.is_none() {
            self.r.set_cursor(start);
        }
        id
    }

    /// A `ResourceLookupRule`.
    fn lookup(&mut self, kind: Lookup) -> Option<String> {
        self.r.skip_whitespace();
        let mark = self.r.cursor();
        let hint = match kind {
            Lookup::Item => Hint::Items,
            Lookup::ItemTag => Hint::ItemTags,
            Lookup::Component => Hint::Components,
            Lookup::Predicate => Hint::Predicates,
        };
        let Some(id) = self.id() else {
            let e =
                CommandError::new("argument.id.invalid", vec![]).at_position(self.r.string(), mark);
            self.store(mark, hint, e);
            return None;
        };
        let source = self.ctx.source;
        let known = match kind {
            Lookup::Item => source.resource_exists(ITEM_REGISTRY, &id),
            Lookup::ItemTag => source.tag_exists(ITEM_REGISTRY, &id),
            Lookup::Component => {
                id == COUNT
                    || (DATA_COMPONENT_TYPE.id(&id).is_some() && !TRANSIENT.contains(&id.as_str()))
            }
            Lookup::Predicate => {
                id == COUNT
                    || DATA_COMPONENT_PREDICATE_TYPE.id(&id).is_some()
                    || DATA_COMPONENT_TYPE.id(&id).is_some()
            }
        };
        if known {
            return Some(id);
        }
        let key = match kind {
            Lookup::Item => "argument.item.id.invalid",
            Lookup::ItemTag => "arguments.item.tag.unknown",
            Lookup::Component => "arguments.item.component.unknown",
            Lookup::Predicate => "arguments.item.predicate.unknown",
        };
        let e = CommandError::new(key, vec![Arg::from(id)]).at(self.r);
        self.store(mark, hint, e);
        None
    }

    /// `TagParseRule`.
    fn tag(&mut self) -> Option<Tag> {
        self.r.skip_whitespace();
        let mark = self.r.cursor();
        match snbt::parse_tag(self.r, self.ctx.limits) {
            Ok(tag) => Some(tag),
            Err(e) => {
                self.store(mark, Hint::None, e);
                None
            }
        }
    }

    /// `type`: an item, `#tag` or `*` (`Some(None)`).
    fn item_type(&mut self) -> Option<Option<ResourceOrTag>> {
        let mark = self.r.cursor();
        if let Some(id) = self.lookup(Lookup::Item) {
            return Some(Some(ResourceOrTag::Resource(id)));
        }
        self.restore(mark);
        if self.char('#') {
            if let Some(id) = self.lookup(Lookup::ItemTag) {
                return Some(Some(ResourceOrTag::Tag(id)));
            }
            self.restore(mark);
            return None;
        }
        self.restore(mark);
        if self.char('*') {
            return Some(None);
        }
        self.restore(mark);
        None
    }

    /// `test`: `component=value`, `predicate~value` or `component`.
    fn test(&mut self) -> Option<ItemTest> {
        let mark = self.r.cursor();
        let component = self.lookup(Lookup::Component);
        let after_component = self.r.cursor();
        if let Some(c) = &component
            && self.char('=')
        {
            return self.tag().map(|t| ItemTest::Equals(c.clone(), t));
        }
        self.restore(mark);
        if let Some(p) = self.lookup(Lookup::Predicate)
            && self.char('~')
        {
            return self.tag().map(|t| ItemTest::Matches(p, t));
        }
        self.restore(mark);
        let c = component?;
        self.restore(after_component);
        Some(ItemTest::Has(c))
    }

    /// `term`: a test or `!test`.
    fn term(&mut self) -> Option<ItemTest> {
        let mark = self.r.cursor();
        if let Some(t) = self.test() {
            return Some(t);
        }
        self.restore(mark);
        if self.char('!')
            && let Some(t) = self.test()
        {
            return Some(ItemTest::Not(Box::new(t)));
        }
        self.restore(mark);
        None
    }

    /// A list of `element` separated by `sep` (`x (sep rest)?`, without recursion).
    fn separated<T>(
        &mut self,
        sep: char,
        mut element: impl FnMut(&mut Self) -> Option<T>,
    ) -> Option<Vec<T>> {
        let mut out = vec![element(self)?];
        loop {
            let mark = self.r.cursor();
            if !self.char(sep) {
                self.restore(mark);
                break;
            }
            match element(self) {
                Some(v) => out.push(v),
                None => {
                    self.restore(mark);
                    break;
                }
            }
        }
        Some(out)
    }

    fn alternatives(&mut self) -> Option<Alternatives> {
        self.separated('|', Self::term)
    }

    /// `top`.
    fn top(&mut self) -> Option<ItemPredicateArg> {
        let item = self.item_type()?;
        let after_type = self.r.cursor();
        if self.char('[') {
            let mark = self.r.cursor();
            let tests = match self.separated(',', Self::alternatives) {
                Some(t) => t,
                None => {
                    self.restore(mark);
                    Vec::new()
                }
            };
            if !self.char(']') {
                return None;
            }
            return Some(ItemPredicateArg { item, tests });
        }
        self.restore(after_type);
        Some(ItemPredicateArg {
            item,
            tests: Vec::new(),
        })
    }

    /// `Grammar.parseForSuggestions`.
    fn suggestions(&self, builder: &SuggestionsBuilder<'_>) -> Suggestions {
        let Some(cursor) = self.last else {
            return builder.offset(builder.start()).build();
        };
        let mut b = builder.offset(cursor);
        let source = self.ctx.source;
        for (hint, _) in &self.entries {
            match hint {
                Hint::None => {}
                Hint::Literal(c) => {
                    b.suggest_matching([c.to_string().as_str()]);
                }
                Hint::Items => {
                    b.suggest_resource_ids(ITEM.entries.iter().copied());
                }
                Hint::ItemTags => {
                    let tags: Vec<String> = source
                        .registry_tags(ITEM_REGISTRY)
                        .iter()
                        .map(|t| crate::game_args::full_id(t))
                        .collect();
                    b.suggest_resource_ids(tags.iter().map(String::as_str));
                }
                Hint::Components => {
                    b.suggest_resource_ids(std::iter::once(COUNT).chain(persistent_components()));
                }
                Hint::Predicates => {
                    b.suggest_resource_ids(
                        std::iter::once(COUNT)
                            .chain(DATA_COMPONENT_PREDICATE_TYPE.entries.iter().copied()),
                    );
                }
            }
        }
        b.build()
    }
}

/// `ItemPredicateArgument` (`Grammar.parseForCommands`).
pub(crate) fn parse_predicate(
    r: &mut StringReader<'_>,
    ctx: ParseCtx<'_>,
) -> Result<ArgValue, CommandError> {
    let mut peg = Peg {
        r,
        ctx,
        last: None,
        entries: Vec::new(),
    };
    match peg.top() {
        Some(p) => Ok(ArgValue::Game(GameValue::ItemPredicate(Box::new(p)))),
        None => Err(peg
            .entries
            .into_iter()
            .next()
            .map(|(_, e)| e)
            .unwrap_or_else(|| CommandError::new("argument.id.invalid", vec![]))),
    }
}

/// `ItemPredicateArgument.listSuggestions`.
pub(crate) fn predicate_suggestions(
    builder: &SuggestionsBuilder<'_>,
    ctx: ParseCtx<'_>,
) -> Suggestions {
    let mut r = StringReader::new(builder.input());
    r.set_cursor(builder.start());
    let mut peg = Peg {
        r: &mut r,
        ctx,
        last: None,
        entries: Vec::new(),
    };
    let _ = peg.top();
    peg.suggestions(builder)
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
            if registry == ITEM_REGISTRY {
                vec!["minecraft:logs".into()]
            } else {
                Vec::new()
            }
        }
    }

    fn ctx_run<T>(f: impl FnOnce(ParseCtx<'_>) -> T) -> T {
        let limits = Limits::default();
        f(ParseCtx {
            source: &S,
            limits: &limits,
        })
    }

    fn item(input: &str) -> (Result<ArgValue, CommandError>, usize) {
        ctx_run(|ctx| {
            let mut r = StringReader::new(input);
            let v = parse_item(&mut r, ctx);
            (v, r.cursor())
        })
    }

    fn pred(input: &str) -> (Result<ArgValue, CommandError>, usize) {
        ctx_run(|ctx| {
            let mut r = StringReader::new(input);
            let v = parse_predicate(&mut r, ctx);
            (v, r.cursor())
        })
    }

    fn pred_value(input: &str) -> ItemPredicateArg {
        match pred(input).0 {
            Ok(ArgValue::Game(GameValue::ItemPredicate(p))) => *p,
            other => panic!("{input}: {other:?}"),
        }
    }

    fn pred_err(input: &str) -> (&'static str, Vec<Arg>, Option<usize>) {
        let e = pred(input)
            .0
            .err()
            .unwrap_or_else(|| panic!("{input} parsed"));
        (e.key, e.args, e.cursor)
    }

    fn item_err(input: &str) -> (&'static str, Option<usize>) {
        let e = item(input)
            .0
            .err()
            .unwrap_or_else(|| panic!("{input} parsed"));
        (e.key, e.cursor)
    }

    fn texts(s: Suggestions) -> Vec<String> {
        s.texts().into_iter().map(str::to_owned).collect()
    }

    fn item_sugg(input: &str) -> Vec<String> {
        ctx_run(|ctx| texts(item_suggestions(&SuggestionsBuilder::new(input, 0), ctx)))
    }

    fn pred_sugg(input: &str) -> Vec<String> {
        ctx_run(|ctx| {
            texts(predicate_suggestions(
                &SuggestionsBuilder::new(input, 0),
                ctx,
            ))
        })
    }

    #[test]
    fn item_stacks() {
        let (v, c) = item("diamond_sword[damage=5,!food] rest");
        assert_eq!(c, 29);
        match v {
            Ok(ArgValue::Game(GameValue::Item(i))) => {
                assert_eq!(i.item, "minecraft:diamond_sword");
                assert_eq!(
                    i.components,
                    vec![
                        ("minecraft:damage".to_owned(), Some(Tag::Int(5))),
                        ("minecraft:food".to_owned(), None)
                    ]
                );
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(item_err("nope"), ("argument.item.id.invalid", Some(0)));
        assert_eq!(
            item_err("stick[foo=1]"),
            ("arguments.item.component.unknown", Some(6))
        );
        assert_eq!(
            item_err("stick[damage=1,damage=2]"),
            ("arguments.item.component.repeated", None)
        );
        assert_eq!(
            item_err("stick[damage=1,"),
            ("arguments.item.component.expected", Some(15))
        );
        assert_eq!(item_err("stick[damage]").0, "parsing.expected");
        assert_eq!(
            item_err("stick[creative_slot_lock=1]").0,
            "arguments.item.component.unknown"
        );
    }

    #[test]
    fn item_suggestions_like_vanilla() {
        assert!(item_sugg("diamond_sw").contains(&"minecraft:diamond_sword".to_owned()));
        assert_eq!(item_sugg("stick"), ["["]);
        let s = item_sugg("stick[");
        assert!(s.contains(&"!".to_owned()));
        assert!(s.contains(&"minecraft:damage=".to_owned()));
        assert!(!s.contains(&"minecraft:creative_slot_lock=".to_owned()));
        assert_eq!(item_sugg("stick[!max_d"), ["minecraft:max_damage"]);
        assert_eq!(item_sugg("stick[damage"), ["="]);
        assert_eq!(item_sugg("stick[damage=1"), [",", "]"]);
    }

    #[test]
    fn predicates() {
        let p = pred_value("*");
        assert_eq!((p.item, p.tests.len()), (None, 0));
        let p = pred_value("#logs[damage=3,count~{min:2}|!enchantments,minecraft:food]");
        assert_eq!(p.item, Some(ResourceOrTag::Tag("minecraft:logs".into())));
        assert_eq!(
            p.tests,
            vec![
                vec![ItemTest::Equals("minecraft:damage".into(), Tag::Int(3))],
                vec![
                    ItemTest::Matches(
                        "minecraft:count".into(),
                        Tag::Compound(
                            vanillite_nbt::Compound::with_capacity(1).with("min", Tag::Int(2))
                        )
                    ),
                    ItemTest::Not(Box::new(ItemTest::Has("minecraft:enchantments".into())))
                ],
                vec![ItemTest::Has("minecraft:food".into())],
            ]
        );
        let (v, c) = pred("stick rest");
        assert!(v.is_ok());
        assert_eq!(c, 5);
        let (v, c) = pred("stick []");
        assert!(v.is_ok());
        assert_eq!(c, 8);
    }

    #[test]
    fn predicate_errors() {
        assert_eq!(pred_err(""), ("argument.id.invalid", Vec::new(), Some(0)));
        assert_eq!(
            pred_err("nope"),
            (
                "argument.item.id.invalid",
                vec![Arg::from("minecraft:nope")],
                Some(4)
            )
        );
        assert_eq!(
            pred_err("#nope"),
            (
                "arguments.item.tag.unknown",
                vec![Arg::from("minecraft:nope")],
                Some(5)
            )
        );
        assert_eq!(
            pred_err("stick[foo]"),
            (
                "arguments.item.component.unknown",
                vec![Arg::from("minecraft:foo")],
                Some(9)
            )
        );
        assert_eq!(pred_err("stick[damage=1,]").0, "argument.id.invalid");
        assert_eq!(pred_err("stick[damage=1,]").2, Some(15));
        assert_eq!(pred_err("stick[damage=1").0, "argument.literal.incorrect");
        assert_eq!(
            pred_err("stick[foo~1]").0,
            "arguments.item.component.unknown"
        );
    }

    #[test]
    fn predicate_suggestions_like_vanilla() {
        let s = pred_sugg("");
        assert!(s.contains(&"#".to_owned()));
        assert!(s.contains(&"*".to_owned()));
        assert!(s.contains(&"minecraft:stick".to_owned()));
        assert_eq!(pred_sugg("#"), ["minecraft:logs"]);
        assert_eq!(pred_sugg("stick"), ["["]);
        let s = pred_sugg("stick[");
        assert!(s.contains(&"!".to_owned()));
        assert!(s.contains(&"minecraft:count".to_owned()));
        assert!(s.contains(&"minecraft:damage".to_owned()));
        assert_eq!(pred_sugg("stick[damage=1"), [",", "]", "|"]);
    }
}
