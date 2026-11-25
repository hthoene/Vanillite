//! Vanilla's `CommandResponseTracker`: the feedback of commands that act on several elements
//! (one message naming the single element, or one counting them).

use vanillite_command::error::CommandError;
use vanillite_protocol::text::Component;

use super::Env;
use super::source::CommandSource;

/// `CommandResponseTracker`.
#[derive(Debug)]
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

    /// The single element and the element count of `ANY` (`any`) or `NON_ZERO`.
    pub(crate) fn pick(&self, any: bool) -> (Option<&T>, i32) {
        if any {
            (self.only.as_ref(), self.count)
        } else {
            (self.only_non_zero.as_ref(), self.non_zero)
        }
    }

    /// `sendFeedback`: `on_zero` fails when no element counts; the message (single element or
    /// count, and the total) goes to the source and, with `broadcast`, the admins.
    pub(crate) fn feedback(
        &self,
        source: &CommandSource,
        env: &Env<'_>,
        broadcast: bool,
        any: bool,
        on_zero: Option<CommandError>,
        message: impl FnOnce(Option<&T>, i32, i32) -> Component,
    ) -> Result<i32, CommandError> {
        let (single, count) = self.pick(any);
        if count == 0
            && let Some(error) = on_zero
        {
            return Err(error);
        }
        source.send_success(env.global, message(single, count, self.total), broadcast);
        Ok(self.total)
    }
}
