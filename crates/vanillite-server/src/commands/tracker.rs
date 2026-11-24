//! Vanilla's `CommandResponseTracker`: counts the elements a command touched and picks the
//! single-element or the multiple-element feedback.

use vanillite_command::error::CommandError;
use vanillite_protocol::text::Component;

use super::Env;
use super::source::CommandSource;

/// `CommandResponseTracker`.
pub(crate) struct Tracker<T> {
    total: i32,
    count: i32,
    only: Option<T>,
    non_zero: i32,
    only_non_zero: Option<T>,
}

impl<T> Default for Tracker<T> {
    fn default() -> Self {
        Tracker {
            total: 0,
            count: 0,
            only: None,
            non_zero: 0,
            only_non_zero: None,
        }
    }
}

impl<T: Clone> Tracker<T> {
    /// `track(element, value)`.
    pub(crate) fn track(&mut self, element: &T, value: i32) {
        self.total = self.total.wrapping_add(value);
        self.count = self.count.saturating_add(1);
        self.only = (self.count == 1).then(|| element.clone());
        if value != 0 {
            self.non_zero = self.non_zero.saturating_add(1);
            self.only_non_zero = (self.non_zero == 1).then(|| element.clone());
        }
    }

    /// `totalValue`.
    pub(crate) fn total(&self) -> i32 {
        self.total
    }

    /// The single element and the element count of `ANY` or `NON_ZERO`.
    pub(crate) fn pick(&self, any: bool) -> (Option<&T>, i32) {
        if any {
            (self.only.as_ref(), self.count)
        } else {
            (self.only_non_zero.as_ref(), self.non_zero)
        }
    }

    /// `sendFeedback`: `on_zero` fails when no element counts; the message is broadcast.
    pub(crate) fn feedback(
        &self,
        source: &CommandSource,
        env: &Env<'_>,
        any: bool,
        on_zero: Option<&'static str>,
        message: impl FnOnce(Option<&T>, i32, i32) -> Component,
    ) -> Result<i32, CommandError> {
        let (single, count) = self.pick(any);
        if count == 0
            && let Some(key) = on_zero
        {
            return Err(CommandError::new(key, vec![]));
        }
        source.send_success(env.global, message(single, count, self.total), true);
        Ok(self.total)
    }
}
