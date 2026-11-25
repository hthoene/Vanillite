//! Suggestions (Brigadier's `Suggestion`, `Suggestions` and `SuggestionsBuilder`). Ranges are
//! byte offsets into the input; [`Suggestions::to_packet`] converts them to the UTF-16 offsets
//! the client counts.

use std::cmp::Ordering;

use vanillite_protocol::packets::play::clientbound::{
    CommandSuggestions, Suggestion as WireSuggestion,
};
use vanillite_protocol::text::Component;

use crate::reader::utf16_len;

/// One suggestion: the text that replaces `start..end` of the input.
#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    /// Start of the replaced range (byte offset).
    pub start: usize,
    /// End of the replaced range (byte offset).
    pub end: usize,
    /// Replacement text.
    pub text: String,
    /// Tooltip.
    pub tooltip: Option<Component>,
    /// The value of an integer suggestion (Brigadier's `IntegerSuggestion`, sorted by value
    /// among other integer suggestions).
    pub int: Option<i32>,
}

impl Suggestion {
    /// `Suggestion.expand`: widens the suggestion to `start..end` with the input around it.
    fn expand(self, input: &str, start: usize, end: usize) -> Self {
        if start == self.start && end == self.end {
            return self;
        }
        let mut text = String::new();
        if start < self.start {
            text.push_str(input.get(start..self.start).unwrap_or(""));
        }
        text.push_str(&self.text);
        if end > self.end {
            text.push_str(input.get(self.end..end).unwrap_or(""));
        }
        Self {
            start,
            end,
            text,
            tooltip: self.tooltip,
            int: None,
        }
    }
}

/// Java's `String.compareToIgnoreCase` over UTF-16 code units.
pub fn compare_ignore_case(a: &str, b: &str) -> Ordering {
    fn fold(u: u16) -> u32 {
        let c = char::from_u32(u32::from(u)).unwrap_or('\u{FFFD}');
        let mut up = c.to_uppercase();
        let upper = match (up.next(), up.next()) {
            (Some(x), None) => x,
            _ => c,
        };
        let mut low = upper.to_lowercase();
        let lower = match (low.next(), low.next()) {
            (Some(x), None) => x,
            _ => upper,
        };
        u32::from(lower)
    }
    let mut ia = a.encode_utf16();
    let mut ib = b.encode_utf16();
    loop {
        match (ia.next(), ib.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                if x != y {
                    let o = fold(x).cmp(&fold(y));
                    if o != Ordering::Equal {
                        return o;
                    }
                }
            }
        }
    }
}

/// A finished list of suggestions for one range.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Suggestions {
    /// Start of the range (byte offset).
    pub start: usize,
    /// End of the range (byte offset).
    pub end: usize,
    /// Suggestions in Brigadier's order.
    pub list: Vec<Suggestion>,
}

impl Suggestions {
    /// `Suggestions.create`: one range covering all, expanded, deduplicated and sorted.
    pub fn create(input: &str, suggestions: Vec<Suggestion>) -> Self {
        if suggestions.is_empty() {
            return Self::default();
        }
        let start = suggestions.iter().map(|s| s.start).min().unwrap_or(0);
        let end = suggestions.iter().map(|s| s.end).max().unwrap_or(0);
        let mut list: Vec<Suggestion> = suggestions
            .into_iter()
            .map(|s| s.expand(input, start, end))
            .collect();
        list.sort_by(|a, b| match (a.int, b.int) {
            (Some(x), Some(y)) => x.cmp(&y),
            // `IntegerSuggestion.compareToIgnoreCase` falls back to the case-sensitive text order.
            (Some(_), None) => a.text.cmp(&b.text),
            _ => compare_ignore_case(&a.text, &b.text).then_with(|| a.text.cmp(&b.text)),
        });
        list.dedup();
        Self { start, end, list }
    }

    /// `Suggestions.merge`.
    pub fn merge(input: &str, mut all: Vec<Suggestions>) -> Self {
        all.retain(|s| !s.list.is_empty());
        match all.len() {
            0 => Self::default(),
            1 => all.pop().unwrap_or_default(),
            _ => Self::create(input, all.into_iter().flat_map(|s| s.list).collect()),
        }
    }

    /// The answer packet; at most `max` entries.
    pub fn to_packet(&self, id: i32, input: &str, max: usize) -> CommandSuggestions {
        let start = utf16_len(input.get(..self.start).unwrap_or(""));
        let length = utf16_len(input.get(self.start..self.end).unwrap_or(""));
        CommandSuggestions {
            id,
            start: i32::try_from(start).unwrap_or(i32::MAX),
            length: i32::try_from(length).unwrap_or(i32::MAX),
            suggestions: self
                .list
                .iter()
                .take(max)
                .map(|s| WireSuggestion {
                    text: s.text.clone(),
                    tooltip: s.tooltip.clone(),
                })
                .collect(),
        }
    }

    /// The suggestion texts (tests and logs).
    pub fn texts(&self) -> Vec<&str> {
        self.list.iter().map(|s| s.text.as_str()).collect()
    }
}

/// Collects suggestions for the input from `start` on (`SuggestionsBuilder`).
#[derive(Debug, Clone)]
pub struct SuggestionsBuilder<'a> {
    input: &'a str,
    start: usize,
    remaining_lower: String,
    result: Vec<Suggestion>,
}

impl<'a> SuggestionsBuilder<'a> {
    /// A builder for `input[start..]`.
    pub fn new(input: &'a str, start: usize) -> Self {
        let start = start.min(input.len());
        Self {
            input,
            start,
            remaining_lower: input.get(start..).unwrap_or("").to_lowercase(),
            result: Vec::new(),
        }
    }

    /// The whole input.
    pub fn input(&self) -> &'a str {
        self.input
    }

    /// Start of the suggested range.
    pub fn start(&self) -> usize {
        self.start
    }

    /// The input from `start` on.
    pub fn remaining(&self) -> &'a str {
        self.input.get(self.start..).unwrap_or("")
    }

    /// The remaining input in lower case.
    pub fn remaining_lower(&self) -> &str {
        &self.remaining_lower
    }

    /// Suggests `text` (skipped when it equals what is already typed).
    pub fn suggest(&mut self, text: impl Into<String>) -> &mut Self {
        self.suggest_with(text, None)
    }

    /// Suggests `text` with a tooltip.
    pub fn suggest_with(
        &mut self,
        text: impl Into<String>,
        tooltip: Option<Component>,
    ) -> &mut Self {
        let text = text.into();
        if text == self.remaining() {
            return self;
        }
        self.result.push(Suggestion {
            start: self.start,
            end: self.input.len(),
            text,
            tooltip,
            int: None,
        });
        self
    }

    /// Suggests an integer (`suggest(int)`); never skipped.
    pub fn suggest_int(&mut self, value: i32) -> &mut Self {
        self.result.push(Suggestion {
            start: self.start,
            end: self.input.len(),
            text: value.to_string(),
            tooltip: None,
            int: Some(value),
        });
        self
    }

    /// A builder for the same input from another start (`createOffset`).
    pub fn offset(&self, start: usize) -> SuggestionsBuilder<'a> {
        SuggestionsBuilder::new(self.input, start)
    }

    /// Takes over the suggestions of another builder (`add`).
    pub fn add(&mut self, other: SuggestionsBuilder<'_>) -> &mut Self {
        self.result.extend(other.result);
        self
    }

    /// `SharedSuggestionProvider.suggest`: the values matching the remaining input at the
    /// start or after `.`, `_` or `/`.
    pub fn suggest_matching<'s>(&mut self, values: impl IntoIterator<Item = &'s str>) -> &mut Self {
        let prefix = self.remaining_lower.clone();
        for v in values {
            if matches_sub_str(&prefix, &v.to_lowercase()) {
                self.suggest(v);
            }
        }
        self
    }

    /// `SharedSuggestionProvider.suggestResource` with a prefix such as `!` or `#`.
    pub fn suggest_resources<'s>(
        &mut self,
        ids: impl IntoIterator<Item = &'s str>,
        prefix: &str,
    ) -> &mut Self {
        let contents = self.remaining_lower.clone();
        let stripped = if contents.is_empty() {
            Some(contents.as_str())
        } else {
            let common = common_prefix_len(&contents, prefix);
            if common == 0 {
                None
            } else {
                contents.get(common..)
            }
        };
        let Some(stripped) = stripped else {
            return self;
        };
        for id in ids {
            if contents.is_empty() || resource_matches(stripped, id) {
                self.suggest(format!("{prefix}{id}"));
            }
        }
        self
    }

    /// `SharedSuggestionProvider.suggestResource` without a prefix.
    pub fn suggest_resource_ids<'s>(
        &mut self,
        ids: impl IntoIterator<Item = &'s str>,
    ) -> &mut Self {
        let contents = self.remaining_lower.clone();
        for id in ids {
            if resource_matches(&contents, id) {
                self.suggest(id);
            }
        }
        self
    }

    /// The finished suggestions.
    pub fn build(self) -> Suggestions {
        Suggestions::create(self.input, self.result)
    }
}

fn common_prefix_len(a: &str, b: &str) -> usize {
    a.char_indices()
        .zip(b.chars())
        .take_while(|((_, x), y)| x == y)
        .last()
        .map_or(0, |((i, c), _)| i.saturating_add(c.len_utf8()))
}

/// `filterResources`: with a namespace typed, match the full id; otherwise the namespace or
/// the path.
pub(crate) fn resource_matches(contents: &str, id: &str) -> bool {
    if contents.contains(':') {
        matches_sub_str(contents, id)
    } else {
        let (ns, path) = id.split_once(':').unwrap_or(("minecraft", id));
        matches_sub_str(contents, ns) || matches_sub_str(contents, path)
    }
}

/// `SharedSuggestionProvider.matchesSubStr`.
pub fn matches_sub_str(pattern: &str, input: &str) -> bool {
    let mut index = 0usize;
    loop {
        if input.get(index..).is_some_and(|s| s.starts_with(pattern)) {
            return true;
        }
        let Some(rest) = input.get(index..) else {
            return false;
        };
        match rest.find(['.', '_', '/']) {
            Some(i) => index = index.saturating_add(i).saturating_add(1),
            None => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_like_java() {
        let mut b = SuggestionsBuilder::new("/tp ", 4);
        for t in ["~", "Bob", "@a", "Alice", "@e"] {
            b.suggest(t);
        }
        assert_eq!(b.build().texts(), ["@a", "@e", "Alice", "Bob", "~"]);
    }

    #[test]
    fn matches_sub_strings() {
        assert!(matches_sub_str("q", "query"));
        assert!(matches_sub_str("st", "minecraft:armor_stand"));
        assert!(!matches_sub_str("x", "query"));
        assert!(matches_sub_str("", "a"));
    }

    #[test]
    fn skips_typed_text() {
        let mut b = SuggestionsBuilder::new("time query", 5);
        b.suggest("query");
        assert!(b.build().list.is_empty());
    }
}
