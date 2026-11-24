//! Bedrock forms (server menus): the seam for plugins. A [`Forms`] handle sends a [`Form`] to a
//! Bedrock player; the answer reaches the [`FormHandler`] set on the handle. There is no Java
//! counterpart, so forms only exist for Bedrock players.

use std::sync::{Arc, Mutex, RwLock};

use rustc_hash::FxHashMap;
use serde_json::{Value, json};
use tokio::sync::mpsc;

/// A button of a simple form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Button {
    pub text: String,
    /// Image URL or resource path (`textures/...`).
    pub image: Option<String>,
}

/// An element of a custom form.
#[derive(Debug, Clone, PartialEq)]
pub enum Element {
    Label(String),
    Input {
        text: String,
        placeholder: String,
        default: String,
    },
    Toggle {
        text: String,
        default: bool,
    },
    Slider {
        text: String,
        min: f32,
        max: f32,
        step: f32,
        default: f32,
    },
    Dropdown {
        text: String,
        options: Vec<String>,
        default: usize,
    },
}

/// A form.
#[derive(Debug, Clone, PartialEq)]
pub enum Form {
    /// Text with two buttons.
    Modal {
        title: String,
        content: String,
        yes: String,
        no: String,
    },
    /// Text with a list of buttons.
    Simple {
        title: String,
        content: String,
        buttons: Vec<Button>,
    },
    /// Input elements.
    Custom {
        title: String,
        elements: Vec<Element>,
    },
}

impl Form {
    /// The form's JSON as the client expects it.
    pub fn to_json(&self) -> String {
        let v = match self {
            Form::Modal {
                title,
                content,
                yes,
                no,
            } => json!({"type": "modal", "title": title, "content": content,
                        "button1": yes, "button2": no}),
            Form::Simple {
                title,
                content,
                buttons,
            } => {
                let buttons: Vec<Value> = buttons
                    .iter()
                    .map(|b| match &b.image {
                        Some(img) => {
                            let kind = if img.starts_with("http") {
                                "url"
                            } else {
                                "path"
                            };
                            json!({"text": b.text, "image": {"type": kind, "data": img}})
                        }
                        None => json!({"text": b.text}),
                    })
                    .collect();
                json!({"type": "form", "title": title, "content": content, "buttons": buttons})
            }
            Form::Custom { title, elements } => {
                let content: Vec<Value> = elements
                    .iter()
                    .map(|e| match e {
                        Element::Label(t) => json!({"type": "label", "text": t}),
                        Element::Input {
                            text,
                            placeholder,
                            default,
                        } => json!({"type": "input", "text": text, "placeholder": placeholder,
                                    "default": default}),
                        Element::Toggle { text, default } => {
                            json!({"type": "toggle", "text": text, "default": default})
                        }
                        Element::Slider {
                            text,
                            min,
                            max,
                            step,
                            default,
                        } => json!({"type": "slider", "text": text, "min": min, "max": max,
                                    "step": step, "default": default}),
                        Element::Dropdown {
                            text,
                            options,
                            default,
                        } => json!({"type": "dropdown", "text": text, "options": options,
                                    "default": default}),
                    })
                    .collect();
                json!({"type": "custom_form", "title": title, "content": content})
            }
        };
        v.to_string()
    }
}

/// A player's answer to a form.
#[derive(Debug, Clone, PartialEq)]
pub enum FormResponse {
    /// Closed without an answer.
    Closed,
    /// Modal form: the first (`true`) or second button.
    Modal(bool),
    /// Simple form: index of the button.
    Simple(usize),
    /// Custom form: one value per element (labels give `null`).
    Custom(Vec<Value>),
}

impl FormResponse {
    /// Parses the client's response JSON for a form of `form`'s kind.
    pub fn parse(form: &Form, data: Option<&str>) -> Option<FormResponse> {
        let Some(data) = data else {
            return Some(FormResponse::Closed);
        };
        let v: Value = serde_json::from_str(data.trim()).ok()?;
        match form {
            Form::Modal { .. } => v.as_bool().map(FormResponse::Modal),
            Form::Simple { buttons, .. } => v
                .as_u64()
                .and_then(|i| usize::try_from(i).ok())
                .filter(|&i| i < buttons.len())
                .map(FormResponse::Simple),
            Form::Custom { elements, .. } => match v {
                Value::Array(values) if values.len() == elements.len() => {
                    Some(FormResponse::Custom(values))
                }
                Value::Null => Some(FormResponse::Closed),
                _ => None,
            },
        }
    }
}

/// Receives the answers to forms.
pub trait FormHandler: Send + Sync {
    /// `player` (Java UUID of the Bedrock player) answered form `id`.
    fn response(&self, player: u128, id: u32, response: FormResponse);
}

/// Forms of the frontend's players.
#[derive(Default)]
pub(crate) struct FormRegistry {
    /// Open sessions by Java UUID: where to send forms.
    pub sessions: Mutex<FxHashMap<u128, mpsc::Sender<crate::session::Event>>>,
    /// Sent forms waiting for an answer, by player and id.
    pub pending: Mutex<FxHashMap<(u128, u32), Form>>,
    pub handler: RwLock<Option<Arc<dyn FormHandler>>>,
    pub next_id: std::sync::atomic::AtomicU32,
}

impl std::fmt::Debug for FormRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FormRegistry").finish_non_exhaustive()
    }
}

/// Forms open per player at most.
const MAX_PENDING: usize = 16;

impl FormRegistry {
    /// Delivers an answer from a session.
    pub fn answer(&self, player: u128, id: u32, data: Option<&str>) {
        let form = self
            .pending
            .lock()
            .ok()
            .and_then(|mut p| p.remove(&(player, id)));
        let Some(form) = form else {
            return;
        };
        let Some(response) = FormResponse::parse(&form, data) else {
            return;
        };
        let handler = self.handler.read().ok().and_then(|h| h.clone());
        if let Some(h) = handler {
            h.response(player, id, response);
        }
    }

    /// Forgets a player's forms.
    pub fn leave(&self, player: u128) {
        if let Ok(mut s) = self.sessions.lock() {
            s.remove(&player);
        }
        if let Ok(mut p) = self.pending.lock() {
            p.retain(|(u, _), _| *u != player);
        }
    }
}

/// Handle to send forms to Bedrock players (cheap to clone).
#[derive(Clone)]
pub struct Forms(pub(crate) Arc<FormRegistry>);

impl std::fmt::Debug for Forms {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Forms").finish_non_exhaustive()
    }
}

impl Forms {
    /// Sets who receives the answers.
    pub fn set_handler(&self, handler: Arc<dyn FormHandler>) {
        if let Ok(mut h) = self.0.handler.write() {
            *h = Some(handler);
        }
    }

    /// Sends a form to a Bedrock player (by Java UUID). Returns the form id, or `None` when the
    /// player is not connected through this frontend or has too many open forms.
    pub fn send(&self, player: u128, form: Form) -> Option<u32> {
        let tx = self.0.sessions.lock().ok()?.get(&player)?.clone();
        let id = self
            .0
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        {
            let mut pending = self.0.pending.lock().ok()?;
            if pending.keys().filter(|(u, _)| *u == player).count() >= MAX_PENDING {
                return None;
            }
            pending.insert((player, id), form.clone());
        }
        tx.try_send(crate::session::Event::Form(id, form.to_json()))
            .ok()?;
        Some(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_and_responses() {
        let form = Form::Simple {
            title: "Menu".into(),
            content: "Pick".into(),
            buttons: vec![Button {
                text: "A".into(),
                image: None,
            }],
        };
        assert!(form.to_json().contains("\"type\":\"form\""));
        assert_eq!(
            FormResponse::parse(&form, Some("0\n")),
            Some(FormResponse::Simple(0))
        );
        assert_eq!(FormResponse::parse(&form, Some("3")), None);
        assert_eq!(FormResponse::parse(&form, None), Some(FormResponse::Closed));
    }
}
