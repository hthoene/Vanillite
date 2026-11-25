//! Vanilla's `CommandResponseTracker` with its default `NON_ZERO` element type: counts what a
//! command changed and picks the single or the multiple success message.

use vanillite_command::error::CommandError;
use vanillite_protocol::text::Component;

use super::source::CommandSource;
use crate::global::Global;

/// `CommandResponseTracker`.
pub(crate) struct Tracker<T> {
    total: i32,
    count: i32,
    only: Option<T>,
}

impl<T> Tracker<T> {
    /// `create`.
    pub(crate) fn new() -> Tracker<T> {
        Tracker {
            total: 0,
            count: 0,
            only: None,
        }
    }

    /// `track(element, value)`.
    pub(crate) fn track_value(&mut self, element: T, value: i32) {
        self.total = self.total.saturating_add(value);
        if value != 0 {
            self.count = self.count.saturating_add(1);
            self.only = (self.count == 1).then_some(element);
        }
    }

    /// `track(element)` and `track(element, boolean)`.
    pub(crate) fn track(&mut self, element: T, changed: bool) {
        self.track_value(element, i32::from(changed));
    }

    /// `sendFeedback`: `zero` when nothing was tracked, else the single message for the only
    /// element or the multiple one with the element count; returns the total value.
    pub(crate) fn feedback(
        self,
        source: &CommandSource,
        global: &Global,
        zero: impl FnOnce() -> CommandError,
        single: impl FnOnce(T) -> Component,
        multiple: impl FnOnce(i32) -> Component,
    ) -> Result<i32, CommandError> {
        if self.count == 0 {
            return Err(zero());
        }
        let message = match self.only {
            Some(element) => single(element),
            None => multiple(self.count),
        };
        source.send_success(global, message, true);
        Ok(self.total)
    }
}
