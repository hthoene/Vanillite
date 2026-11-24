//! Text components: literal and translatable text with basic styles.
//!
//! Components go on the wire as NBT in configuration and play, and as JSON in status and login
//! disconnects. Text that comes from players is only ever wrapped as a literal (SEC-09).

use crate::codec::Encode;
use vanillite_nbt::{Compound, List, Tag};

/// The sixteen named chat colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NamedColor {
    Black,
    DarkBlue,
    DarkGreen,
    DarkAqua,
    DarkRed,
    DarkPurple,
    Gold,
    Gray,
    DarkGray,
    Blue,
    Green,
    Aqua,
    Red,
    LightPurple,
    Yellow,
    White,
}

impl NamedColor {
    /// The Vanilla name (`dark_blue`).
    pub const fn name(self) -> &'static str {
        match self {
            Self::Black => "black",
            Self::DarkBlue => "dark_blue",
            Self::DarkGreen => "dark_green",
            Self::DarkAqua => "dark_aqua",
            Self::DarkRed => "dark_red",
            Self::DarkPurple => "dark_purple",
            Self::Gold => "gold",
            Self::Gray => "gray",
            Self::DarkGray => "dark_gray",
            Self::Blue => "blue",
            Self::Green => "green",
            Self::Aqua => "aqua",
            Self::Red => "red",
            Self::LightPurple => "light_purple",
            Self::Yellow => "yellow",
            Self::White => "white",
        }
    }
}

/// A text colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Color {
    Named(NamedColor),
    /// `0xRRGGBB`.
    Rgb(u32),
}

impl Color {
    fn serialize(self) -> String {
        match self {
            Self::Named(n) => n.name().to_owned(),
            Self::Rgb(v) => format!("#{:06X}", v & 0xFF_FFFF),
        }
    }
}

/// Style attributes; `None` inherits from the parent.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Style {
    pub color: Option<Color>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underlined: Option<bool>,
    pub strikethrough: Option<bool>,
    pub obfuscated: Option<bool>,
    pub insertion: Option<String>,
    pub font: Option<String>,
}

impl Style {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// What a component shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    /// Literal text.
    Text(String),
    /// A translation key with arguments, resolved by the client.
    Translatable {
        key: String,
        fallback: Option<String>,
        args: Vec<Component>,
    },
}

/// A text component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Component {
    pub content: Content,
    pub style: Style,
    pub extra: Vec<Component>,
}

impl Component {
    /// A literal text component. This is the only way player-supplied text enters a component.
    pub fn text(s: impl Into<String>) -> Self {
        Self {
            content: Content::Text(s.into()),
            style: Style::default(),
            extra: Vec::new(),
        }
    }

    /// A translatable component.
    pub fn translatable(key: impl Into<String>, args: Vec<Component>) -> Self {
        Self {
            content: Content::Translatable {
                key: key.into(),
                fallback: None,
                args,
            },
            style: Style::default(),
            extra: Vec::new(),
        }
    }

    /// Sets the colour.
    #[must_use]
    pub fn color(mut self, color: Color) -> Self {
        self.style.color = Some(color);
        self
    }

    /// Appends a child.
    #[must_use]
    pub fn append(mut self, child: Component) -> Self {
        self.extra.push(child);
        self
    }

    fn is_plain(&self) -> bool {
        matches!(self.content, Content::Text(_)) && self.style.is_empty() && self.extra.is_empty()
    }

    /// The NBT form (a plain string for unstyled literal text, like Vanilla).
    pub fn to_nbt(&self) -> Tag {
        if let (true, Content::Text(t)) = (self.is_plain(), &self.content) {
            return Tag::String(t.clone());
        }
        let mut c = Compound::new();
        match &self.content {
            Content::Text(t) => c.insert("text", t.as_str()),
            Content::Translatable {
                key,
                fallback,
                args,
            } => {
                c.insert("translate", key.as_str());
                if let Some(f) = fallback {
                    c.insert("fallback", f.as_str());
                }
                if !args.is_empty() {
                    c.insert("with", List(args.iter().map(Self::to_nbt).collect()));
                }
            }
        }
        let s = &self.style;
        if let Some(color) = s.color {
            c.insert("color", color.serialize());
        }
        for (name, v) in [
            ("bold", s.bold),
            ("italic", s.italic),
            ("underlined", s.underlined),
            ("strikethrough", s.strikethrough),
            ("obfuscated", s.obfuscated),
        ] {
            if let Some(v) = v {
                c.insert(name, v);
            }
        }
        if let Some(i) = &s.insertion {
            c.insert("insertion", i.as_str());
        }
        if let Some(f) = &s.font {
            c.insert("font", f.as_str());
        }
        if !self.extra.is_empty() {
            c.insert("extra", List(self.extra.iter().map(Self::to_nbt).collect()));
        }
        Tag::Compound(c)
    }

    /// The JSON form.
    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::{Map, Value};
        if let (true, Content::Text(t)) = (self.is_plain(), &self.content) {
            return Value::String(t.clone());
        }
        let mut m = Map::new();
        match &self.content {
            Content::Text(t) => {
                m.insert("text".into(), Value::String(t.clone()));
            }
            Content::Translatable {
                key,
                fallback,
                args,
            } => {
                m.insert("translate".into(), Value::String(key.clone()));
                if let Some(f) = fallback {
                    m.insert("fallback".into(), Value::String(f.clone()));
                }
                if !args.is_empty() {
                    m.insert(
                        "with".into(),
                        Value::Array(args.iter().map(Self::to_json).collect()),
                    );
                }
            }
        }
        let s = &self.style;
        if let Some(color) = s.color {
            m.insert("color".into(), Value::String(color.serialize()));
        }
        for (name, v) in [
            ("bold", s.bold),
            ("italic", s.italic),
            ("underlined", s.underlined),
            ("strikethrough", s.strikethrough),
            ("obfuscated", s.obfuscated),
        ] {
            if let Some(v) = v {
                m.insert(name.into(), Value::Bool(v));
            }
        }
        if let Some(i) = &s.insertion {
            m.insert("insertion".into(), Value::String(i.clone()));
        }
        if let Some(f) = &s.font {
            m.insert("font".into(), Value::String(f.clone()));
        }
        if !self.extra.is_empty() {
            m.insert(
                "extra".into(),
                Value::Array(self.extra.iter().map(Self::to_json).collect()),
            );
        }
        Value::Object(m)
    }

    /// The JSON text.
    pub fn to_json_string(&self) -> String {
        self.to_json().to_string()
    }

    /// Plain text of literal parts, for logs.
    pub fn plain(&self) -> String {
        let mut out = String::new();
        self.collect_plain(&mut out);
        out
    }

    fn collect_plain(&self, out: &mut String) {
        match &self.content {
            Content::Text(t) => out.push_str(t),
            Content::Translatable { key, .. } => out.push_str(key),
        }
        for e in &self.extra {
            e.collect_plain(out);
        }
    }
}

impl From<&str> for Component {
    fn from(s: &str) -> Self {
        Self::text(s)
    }
}

/// Wire form of a component in configuration and play: network NBT.
impl Encode for Component {
    fn encode(&self, out: &mut Vec<u8>) {
        crate::types::Nbt(Some(self.to_nbt())).encode(out);
    }
}

/// A component written as a JSON string (status, login disconnect).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonComponent(pub Component);

impl Encode for JsonComponent {
    fn encode(&self, out: &mut Vec<u8>) {
        self.0.to_json_string().encode(out);
    }
}

impl Component {
    /// Best-effort reading of a component from NBT (used by the test client for messages).
    pub fn from_nbt(tag: &Tag) -> Self {
        match tag {
            Tag::String(s) => Self::text(s.as_str()),
            Tag::Compound(c) => {
                let content = match (
                    c.get("text").and_then(Tag::as_str),
                    c.get("translate").and_then(Tag::as_str),
                ) {
                    (Some(t), _) => Content::Text(t.to_owned()),
                    (None, Some(k)) => Content::Translatable {
                        key: k.to_owned(),
                        fallback: c.get("fallback").and_then(Tag::as_str).map(str::to_owned),
                        args: list_of(c.get("with")),
                    },
                    _ => Content::Text(String::new()),
                };
                Self {
                    content,
                    style: Style::default(),
                    extra: list_of(c.get("extra")),
                }
            }
            Tag::List(l) => {
                let mut items = l.0.iter().map(Self::from_nbt);
                let first = items.next().unwrap_or_else(|| Self::text(""));
                items.fold(first, Self::append)
            }
            other => Self::text(vanillite_nbt::snbt::to_string(other)),
        }
    }

    /// Best-effort reading of a component from JSON.
    pub fn from_json(v: &serde_json::Value) -> Self {
        use serde_json::Value;
        match v {
            Value::String(s) => Self::text(s.as_str()),
            Value::Array(items) => {
                let mut items = items.iter().map(Self::from_json);
                let first = items.next().unwrap_or_else(|| Self::text(""));
                items.fold(first, Self::append)
            }
            Value::Object(m) => {
                let list = |k: &str| -> Vec<Component> {
                    m.get(k)
                        .and_then(Value::as_array)
                        .map(|a| a.iter().map(Self::from_json).collect())
                        .unwrap_or_default()
                };
                let content = match (
                    m.get("text").and_then(Value::as_str),
                    m.get("translate").and_then(Value::as_str),
                ) {
                    (Some(t), _) => Content::Text(t.to_owned()),
                    (None, Some(k)) => Content::Translatable {
                        key: k.to_owned(),
                        fallback: m.get("fallback").and_then(Value::as_str).map(str::to_owned),
                        args: list("with"),
                    },
                    _ => Content::Text(String::new()),
                };
                Self {
                    content,
                    style: Style::default(),
                    extra: list("extra"),
                }
            }
            other => Self::text(other.to_string()),
        }
    }
}

fn list_of(tag: Option<&Tag>) -> Vec<Component> {
    match tag {
        Some(Tag::List(l)) => l.0.iter().map(Component::from_nbt).collect(),
        _ => Vec::new(),
    }
}

/// Components from the network are read with the given NBT quota.
impl crate::codec::DecodeLimited for Component {
    fn decode_limited(r: &mut crate::codec::Reader<'_>, max: usize) -> crate::codec::Result<Self> {
        let nbt = crate::types::Nbt::decode_limited(r, max)?;
        Ok(nbt
            .0
            .as_ref()
            .map(Self::from_nbt)
            .unwrap_or_else(|| Self::text("")))
    }
}

/// JSON components are read as a string of at most `max` characters.
impl crate::codec::DecodeLimited for JsonComponent {
    fn decode_limited(r: &mut crate::codec::Reader<'_>, max: usize) -> crate::codec::Result<Self> {
        let s = r.string(max)?;
        let v: serde_json::Value =
            serde_json::from_str(s).map_err(|_| crate::codec::Error::Invalid("JSON component"))?;
        Ok(Self(Component::from_json(&v)))
    }
}
