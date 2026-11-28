//! Java dialogs (`show_dialog`) as Bedrock forms: a notice or a list of actions becomes a simple
//! form, a confirmation a modal form, a dialog with inputs a custom form (with a dropdown for the
//! action when there are several). The chosen action goes back like the Java client sends it: a
//! command, or `custom_click_action` with the inputs.

use vanillite_nbt::{Compound, Tag};
use vanillite_protocol::packets::play::serverbound as psb;
use vanillite_protocol::text::Component;

use super::Session;
use crate::forms::{Button, Element, Form};
use crate::packets::{self as bp, id};

/// Form id of dialogs.
pub(super) const DIALOG_FORM: u32 = u32::MAX - 2;

/// Most actions, inputs and options read from one dialog.
const MAX_ENTRIES: usize = 64;

/// What a button does (`Action`).
#[derive(Debug, Clone, PartialEq)]
enum Action {
    Command(String),
    Template(String),
    Custom(String, Option<Tag>),
    CustomAll(String, Option<Compound>),
    Dialog(Box<Tag>),
    /// A link or text the player can only read on Bedrock.
    Show(String),
    None,
}

/// One input control (`InputControl`) and its key.
#[derive(Debug, Clone, PartialEq)]
enum Input {
    Text {
        key: String,
    },
    Bool {
        key: String,
        on_true: String,
        on_false: String,
    },
    Option {
        key: String,
        ids: Vec<String>,
    },
    Range {
        key: String,
        min: f32,
        max: f32,
    },
}

/// A shown dialog: its actions and inputs, to answer the form.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Shown {
    kind: Kind,
    actions: Vec<Action>,
    exit: Action,
    inputs: Vec<Input>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Simple form: the button index is the action.
    Buttons,
    /// Modal form: yes = action 0, no = action 1.
    Modal,
    /// Custom form: the inputs, then (with several actions) a dropdown of the action.
    Custom,
}

fn id_of(tag: Option<&Tag>) -> String {
    let s = tag.and_then(Tag::as_str).unwrap_or_default();
    s.strip_prefix("minecraft:").unwrap_or(s).to_owned()
}

fn list(tag: Option<&Tag>) -> Vec<&Tag> {
    match tag {
        Some(Tag::List(l)) => l.0.iter().take(MAX_ENTRIES).collect(),
        Some(t) => vec![t],
        None => Vec::new(),
    }
}

fn float(tag: Option<&Tag>) -> Option<f32> {
    match tag? {
        Tag::Float(v) => Some(*v),
        Tag::Double(v) => Some(crate::num::f32_of(*v)),
        t => t
            .as_i64()
            .and_then(|v| i16::try_from(v).ok())
            .map(f32::from),
    }
}

fn flag(tag: Option<&Tag>, default: bool) -> bool {
    tag.and_then(Tag::as_i64).map_or(default, |v| v != 0)
}

fn action(tag: Option<&Tag>) -> Action {
    let Some(c) = tag.and_then(Tag::as_compound) else {
        return Action::None;
    };
    let text = |name: &str| {
        c.get(name)
            .and_then(Tag::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    match id_of(c.get("type")).as_str() {
        "run_command" => Action::Command(text("command")),
        "dynamic/run_command" => Action::Template(text("template")),
        "custom" => Action::Custom(id_of(c.get("id")), c.get("payload").cloned()),
        "dynamic/custom" => Action::CustomAll(
            id_of(c.get("id")),
            c.get("additions").and_then(Tag::as_compound).cloned(),
        ),
        "show_dialog" => c
            .get("dialog")
            .map_or(Action::None, |d| Action::Dialog(Box::new(d.clone()))),
        "open_url" => Action::Show(text("url")),
        "copy_to_clipboard" => Action::Show(text("value")),
        "suggest_command" => Action::Show(text("command")),
        _ => Action::None,
    }
}

impl Session {
    fn nbt_text(&self, tag: Option<&Tag>) -> String {
        tag.map(|t| self.text(&Component::from_nbt(t)))
            .unwrap_or_default()
    }

    /// A button: its label and action.
    fn button(&self, tag: &Tag) -> (String, Action) {
        let c = tag.as_compound();
        (
            self.nbt_text(c.and_then(|c| c.get("label"))),
            action(c.and_then(|c| c.get("action"))),
        )
    }

    /// `show_dialog` with the dialog's NBT (registered ones are looked up by the caller).
    pub(super) fn show_dialog(&mut self, dialog: &Tag) {
        let Some(d) = dialog.as_compound() else {
            return;
        };
        let kind = id_of(d.get("type"));
        let title = self.nbt_text(d.get("title"));
        let mut body: Vec<String> = list(d.get("body"))
            .into_iter()
            .map(|b| match b.as_compound() {
                Some(c) if c.get("contents").is_some() => self.nbt_text(c.get("contents")),
                Some(c) if c.get("item").is_some() => {
                    let description = c
                        .get("description")
                        .map(|d| match d.as_compound().and_then(|c| c.get("contents")) {
                            Some(contents) => self.nbt_text(Some(contents)),
                            None => self.nbt_text(Some(d)),
                        })
                        .unwrap_or_default();
                    let item = c
                        .get("item")
                        .and_then(Tag::as_compound)
                        .map(|i| id_of(i.get("id")))
                        .unwrap_or_default();
                    format!("[{item}] {description}")
                }
                _ => self.nbt_text(Some(b)),
            })
            .collect();
        let mut buttons: Vec<(String, Action)> = Vec::new();
        let mut exit = Action::None;
        let ok = || {
            crate::text::render(
                &Component::translatable("gui.ok", vec![]),
                &self.shared.config.language,
            )
        };
        match kind.as_str() {
            "notice" => buttons.push(
                d.get("action")
                    .map_or_else(|| (ok(), Action::None), |b| self.button(b)),
            ),
            "confirmation" => {
                for key in ["yes", "no"] {
                    buttons.push(d.get(key).map_or((ok(), Action::None), |b| self.button(b)));
                }
                exit = buttons.get(1).map_or(Action::None, |b| b.1.clone());
            }
            "multi_action" => {
                buttons.extend(list(d.get("actions")).into_iter().map(|b| self.button(b)));
            }
            "server_links" => {
                buttons.extend(
                    self.server_links.iter().map(|(label, url)| {
                        (label.clone(), Action::Show(format!("{label}: {url}")))
                    }),
                );
            }
            "dialog_list" => {
                buttons.extend(list(d.get("dialogs")).into_iter().filter_map(|t| {
                    let inner = t.as_compound()?;
                    let label = inner.get("external_title").or_else(|| inner.get("title"));
                    Some((self.nbt_text(label), Action::Dialog(Box::new(t.clone()))))
                }));
            }
            _ => return,
        }
        if let Some(e) = d.get("exit_action") {
            let (label, a) = self.button(e);
            exit = a.clone();
            buttons.push((label, a));
        }
        let inputs: Vec<(Input, Element)> = list(d.get("inputs"))
            .into_iter()
            .filter_map(|i| self.input(i.as_compound()?))
            .collect();
        let (form, kind) = if !inputs.is_empty() {
            let mut elements: Vec<Element> = body.drain(..).map(Element::Label).collect();
            if buttons.len() > 1 {
                elements.push(Element::Dropdown {
                    text: String::new(),
                    options: buttons.iter().map(|b| b.0.clone()).collect(),
                    default: 0,
                });
            }
            elements.extend(inputs.iter().map(|(_, e)| e.clone()));
            (Form::Custom { title, elements }, Kind::Custom)
        } else if kind == "confirmation" {
            let no = buttons.get(1).map(|b| b.0.clone()).unwrap_or_default();
            let yes = buttons.first().map(|b| b.0.clone()).unwrap_or_default();
            (
                Form::Modal {
                    title,
                    content: body.join("\n"),
                    yes,
                    no,
                },
                Kind::Modal,
            )
        } else {
            let list = buttons
                .iter()
                .map(|b| Button {
                    text: b.0.clone(),
                    image: None,
                })
                .collect();
            (
                Form::Simple {
                    title,
                    content: body.join("\n"),
                    buttons: list,
                },
                Kind::Buttons,
            )
        };
        self.dialog = Some(Box::new(Shown {
            kind,
            actions: buttons.into_iter().map(|b| b.1).collect(),
            exit,
            inputs: inputs.into_iter().map(|(i, _)| i).collect(),
        }));
        self.send(
            id::MODAL_FORM_REQUEST,
            &bp::modal_form_request(DIALOG_FORM, &form.to_json()),
        );
    }

    fn input(&self, c: &Compound) -> Option<(Input, Element)> {
        let key = c.get("key").and_then(Tag::as_str)?.to_owned();
        let label = self.nbt_text(c.get("label"));
        Some(match id_of(c.get("type")).as_str() {
            "text" => (
                Input::Text { key },
                Element::Input {
                    text: label,
                    placeholder: String::new(),
                    default: c
                        .get("initial")
                        .and_then(Tag::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                },
            ),
            "boolean" => {
                let s = |name: &str, default: &str| {
                    c.get(name)
                        .and_then(Tag::as_str)
                        .unwrap_or(default)
                        .to_owned()
                };
                (
                    Input::Bool {
                        key,
                        on_true: s("on_true", "true"),
                        on_false: s("on_false", "false"),
                    },
                    Element::Toggle {
                        text: label,
                        default: flag(c.get("initial"), false),
                    },
                )
            }
            "single_option" => {
                let mut ids = Vec::new();
                let mut names = Vec::new();
                let mut default = 0;
                for (i, o) in list(c.get("options")).into_iter().enumerate() {
                    let (id, display, initial) = match o {
                        Tag::Compound(e) => (
                            e.get("id").and_then(Tag::as_str).unwrap_or_default(),
                            e.get("display"),
                            flag(e.get("initial"), false),
                        ),
                        t => (t.as_str().unwrap_or_default(), None, false),
                    };
                    if initial {
                        default = i;
                    }
                    names.push(display.map_or_else(|| id.to_owned(), |d| self.nbt_text(Some(d))));
                    ids.push(id.to_owned());
                }
                if ids.is_empty() {
                    return None;
                }
                (
                    Input::Option { key, ids },
                    Element::Dropdown {
                        text: label,
                        options: names,
                        default,
                    },
                )
            }
            "number_range" => {
                let start = float(c.get("start"))?;
                let end = float(c.get("end"))?;
                let step = float(c.get("step")).unwrap_or((end - start).abs() / 100.0);
                let step = if step > 0.0 { step } else { 1.0 };
                let initial = float(c.get("initial")).unwrap_or((start + end) / 2.0);
                (
                    Input::Range {
                        key,
                        min: start.min(end),
                        max: start.max(end),
                    },
                    Element::Slider {
                        text: label,
                        min: start.min(end),
                        max: start.max(end),
                        step,
                        default: initial,
                    },
                )
            }
            _ => return None,
        })
    }

    /// The answer to a dialog form (`None`: closed).
    pub(super) fn dialog_answer(&mut self, data: Option<&str>) {
        let Some(shown) = self.dialog.take() else {
            return;
        };
        let value = data.and_then(|d| serde_json::from_str::<serde_json::Value>(d).ok());
        let (action, values) = match (shown.kind, value) {
            (_, None | Some(serde_json::Value::Null)) => (shown.exit.clone(), Vec::new()),
            (Kind::Buttons, Some(v)) => {
                let i = v.as_u64().and_then(|i| usize::try_from(i).ok());
                (
                    i.and_then(|i| shown.actions.get(i))
                        .cloned()
                        .unwrap_or(Action::None),
                    Vec::new(),
                )
            }
            (Kind::Modal, Some(v)) => {
                let i = usize::from(v.as_bool() != Some(true));
                (
                    shown.actions.get(i).cloned().unwrap_or(Action::None),
                    Vec::new(),
                )
            }
            (Kind::Custom, Some(serde_json::Value::Array(all))) => {
                // Labels answer null; with several actions a dropdown comes first.
                let mut answers = all.into_iter().filter(|v| !v.is_null());
                let chosen = if shown.actions.len() > 1 {
                    answers
                        .next()
                        .and_then(|v| v.as_u64())
                        .and_then(|i| usize::try_from(i).ok())
                        .unwrap_or(0)
                } else {
                    0
                };
                let values = shown
                    .inputs
                    .iter()
                    .zip(answers)
                    .filter_map(|(input, v)| value_of(input, &v))
                    .collect();
                (
                    shown.actions.get(chosen).cloned().unwrap_or(Action::None),
                    values,
                )
            }
            (Kind::Custom, Some(_)) => return,
        };
        self.run_dialog_action(action, &values);
    }

    fn run_dialog_action(&mut self, action: Action, values: &[(String, String, Tag)]) {
        match action {
            Action::Command(c) => self.dialog_command(&c),
            Action::Template(t) => {
                let mut command = t;
                for (key, text, _) in values {
                    command = command.replace(&format!("$({key})"), text);
                }
                self.dialog_command(&command);
            }
            Action::Custom(id, payload) => self.custom_click(&id, payload.as_ref()),
            Action::CustomAll(id, additions) => {
                let mut c = additions.unwrap_or_default();
                for (key, _, tag) in values {
                    c = c.with(key.clone(), tag.clone());
                }
                self.custom_click(&id, Some(&Tag::Compound(c)));
            }
            Action::Dialog(d) => self.show_dialog(&d),
            Action::Show(text) => {
                if !text.is_empty() {
                    self.send(id::TEXT, &bp::text_raw(&text));
                }
            }
            Action::None => {}
        }
    }

    fn dialog_command(&mut self, command: &str) {
        let command: String = command
            .strip_prefix('/')
            .unwrap_or(command)
            .chars()
            .filter(|c| !c.is_control())
            .collect();
        if !command.is_empty() {
            self.java_send(&psb::ChatCommand { command });
        }
    }

    fn custom_click(&mut self, id: &str, payload: Option<&Tag>) {
        let Ok(id) = vanillite_protocol::types::Identifier::new(id) else {
            return;
        };
        let mut bytes = Vec::new();
        match payload {
            Some(tag) => {
                if vanillite_nbt::write::write_network(tag, &mut bytes).is_err() {
                    return;
                }
            }
            None => bytes.push(0),
        }
        self.java_send(&psb::CustomClickAction {
            id,
            payload: vanillite_protocol::types::RawBytes(bytes),
        });
    }

    /// `show_dialog`: an inline dialog, or a registered one (Vanilla's own dialogs come
    /// without data from the known packs; of those only the server links can be shown).
    pub(super) fn java_show_dialog(
        &mut self,
        d: vanillite_protocol::packets::play::clientbound::DialogRef,
    ) {
        use vanillite_protocol::packets::play::clientbound::DialogRef;
        let tag = match d {
            DialogRef::Direct(nbt) => nbt.0,
            DialogRef::Registered(i) => {
                let Some((name, tag)) = usize::try_from(i).ok().and_then(|i| self.dialogs.get(i))
                else {
                    return;
                };
                match (name.as_str(), tag) {
                    (_, Some(tag)) => Some(tag.clone()),
                    ("minecraft:server_links", None) => Some(Tag::Compound(
                        Compound::new().with("type", "minecraft:server_links").with(
                            "title",
                            Component::translatable("menu.server_links.title", vec![]).to_nbt(),
                        ),
                    )),
                    _ => None,
                }
            }
        };
        if let Some(tag) = tag {
            self.show_dialog(&tag);
        }
    }

    /// `server_links`, for the server links dialog.
    pub(super) fn java_server_links(
        &mut self,
        links: &[vanillite_protocol::packets::common::ServerLink],
    ) {
        use vanillite_protocol::packets::common::ServerLinkLabel;
        const KNOWN: [&str; 10] = [
            "report_bug",
            "community_guidelines",
            "support",
            "status",
            "feedback",
            "community",
            "website",
            "forums",
            "news",
            "announcements",
        ];
        self.server_links = links
            .iter()
            .take(MAX_ENTRIES)
            .map(|l| {
                let label = match &l.label {
                    ServerLinkLabel::Known(i) => {
                        let name = usize::try_from(*i)
                            .ok()
                            .and_then(|i| KNOWN.get(i))
                            .unwrap_or(&"website");
                        self.text(&Component::translatable(
                            format!("known_server_link.{name}"),
                            vec![],
                        ))
                    }
                    ServerLinkLabel::Custom(c) => self.text(c),
                };
                (label, l.url.clone())
            })
            .collect();
    }

    /// `clear_dialog`: closes the form.
    pub(super) fn clear_dialog(&mut self) {
        if self.dialog.take().is_some() {
            self.send(id::CLIENTBOUND_CLOSE_FORM, &[]);
        }
    }
}

/// An input's answer: its key, template text and tag.
fn value_of(input: &Input, v: &serde_json::Value) -> Option<(String, String, Tag)> {
    Some(match input {
        Input::Text { key } => {
            let s = v.as_str()?.to_owned();
            (key.clone(), s.clone(), Tag::String(s))
        }
        Input::Bool {
            key,
            on_true,
            on_false,
        } => {
            let b = v.as_bool()?;
            let text = if b { on_true } else { on_false };
            (key.clone(), text.clone(), Tag::Byte(i8::from(b)))
        }
        Input::Option { key, ids } => {
            let i = usize::try_from(v.as_u64()?).ok()?;
            let id = ids.get(i)?.clone();
            (key.clone(), id.clone(), Tag::String(id))
        }
        Input::Range { key, min, max } => {
            let raw = crate::num::f32_of(v.as_f64()?);
            let value = raw.clamp(*min, *max);
            let text = if value.fract() == 0.0 && value.abs() < 1e9 {
                format!("{value:.0}")
            } else {
                value.to_string()
            };
            (key.clone(), text, Tag::Float(value))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_actions_and_inputs() {
        let run = Tag::Compound(
            Compound::new()
                .with("type", "minecraft:dynamic/run_command")
                .with("template", "say $(name)"),
        );
        assert_eq!(action(Some(&run)), Action::Template("say $(name)".into()));
        let v = value_of(
            &Input::Text { key: "name".into() },
            &serde_json::json!("Alex"),
        );
        assert_eq!(
            v,
            Some(("name".into(), "Alex".into(), Tag::String("Alex".into())))
        );
        let r = Input::Range {
            key: "n".into(),
            min: 0.0,
            max: 10.0,
        };
        assert_eq!(
            value_of(&r, &serde_json::json!(4.0)).map(|v| v.1),
            Some("4".into())
        );
    }
}
