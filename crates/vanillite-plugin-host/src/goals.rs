//! Plugin goals ([`abi::GoalSpec`]): an adapter the server's goal selector calls like its own
//! goals. Calls run on the mob's owner during its AI tick, contained like every plugin call.

use std::sync::Arc;

use vanillite_abi::{self as abi, status};

use crate::Plugin;

/// A goal a plugin added to one mob. Dropping it frees the plugin's data.
pub struct PluginGoal {
    plugin: Arc<Plugin>,
    spec: abi::GoalSpec,
}

// SAFETY: the plugin's goal functions and user data are called on the mob's owner only, one
// call at a time, under the plugin's call discipline (`Plugin::call`).
unsafe impl Send for PluginGoal {}
// SAFETY: as above.
unsafe impl Sync for PluginGoal {}

impl std::fmt::Debug for PluginGoal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginGoal")
            .field("plugin", &self.plugin.id())
            .field("priority", &self.spec.priority)
            .field("flags", &self.spec.flags)
            .finish_non_exhaustive()
    }
}

impl PluginGoal {
    pub(crate) fn new(plugin: Arc<Plugin>, spec: abi::GoalSpec) -> PluginGoal {
        PluginGoal { plugin, spec }
    }

    /// The goal's priority (lower runs first).
    pub fn priority(&self) -> i32 {
        self.spec.priority
    }

    /// The [`abi::goal_flag`] bits it controls.
    pub fn flags(&self) -> u32 {
        self.spec.flags
    }

    /// Whether it belongs to the target selector.
    pub fn is_target(&self) -> bool {
        self.spec.target != 0
    }

    fn check(&self, f: abi::GoalCheckFn, mob: &abi::GoalMob) -> bool {
        let mut yes = 0;
        let s = self.plugin.call("a goal", || {
            // SAFETY: the plugin's goal function and user data live until `drop`; the record
            // is valid for the call.
            yes = unsafe { f(self.spec.user, mob) };
            status::OK
        });
        s == status::OK && yes != 0
    }

    /// Whether it should start (`false` while the plugin is disabled).
    pub fn can_use(&self, mob: &abi::GoalMob) -> bool {
        self.check(self.spec.can_use, mob)
    }

    /// Whether it should go on.
    pub fn can_continue(&self, mob: &abi::GoalMob) -> bool {
        self.check(self.spec.can_continue.unwrap_or(self.spec.can_use), mob)
    }

    fn notify(&self, f: Option<abi::GoalFn>, mob: &abi::GoalMob) {
        if let Some(f) = f {
            self.plugin.call("a goal", || {
                // SAFETY: as in `check`.
                unsafe { f(self.spec.user, mob) };
                status::OK
            });
        }
    }

    /// It starts.
    pub fn start(&self, mob: &abi::GoalMob) {
        self.notify(self.spec.start, mob);
    }

    /// It stops.
    pub fn stop(&self, mob: &abi::GoalMob) {
        self.notify(self.spec.stop, mob);
    }

    /// One tick: what the mob does (`actions` 0 when the call failed).
    pub fn tick(&self, mob: &abi::GoalMob) -> abi::GoalAction {
        let mut action = abi::GoalAction {
            size: abi::size_of::<abi::GoalAction>(),
            actions: 0,
            move_to: mob.pos,
            speed: 1.0,
            look_at: mob.pos,
            target: abi::Uuid::NIL,
        };
        let s = self.plugin.call("a goal", || {
            // SAFETY: as in `check`; the action record is valid for the call.
            unsafe { (self.spec.tick)(self.spec.user, mob, &mut action) };
            status::OK
        });
        if s != status::OK {
            action.actions = 0;
        }
        action
    }
}

impl Drop for PluginGoal {
    fn drop(&mut self) {
        if let Some(drop) = self.spec.drop {
            self.plugin.call_always("dropping a goal", || {
                // SAFETY: the plugin's drop function for its own user data, called once.
                unsafe { drop(self.spec.user) };
                status::OK
            });
        }
    }
}
