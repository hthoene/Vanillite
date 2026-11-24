//! Plugin tasks on owners: the global owner (server thread between ticks), worlds (also for
//! locations; regions later), entities (on the entity's world while it exists) and background
//! threads. There is no main thread: a task only touches what its owner owns.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use vanillite_abi::{self as abi, Status, status};

use crate::api::CallCtx;
use crate::{Inner, Plugin, SendPtr, WorldAccess};

#[derive(Debug)]
struct Task {
    id: u64,
    plugin: u32,
    due: u64,
    period: u64,
    /// The entity an entity task follows.
    entity: Option<u128>,
    run: abi::TaskFn,
    drop: Option<abi::DropFn>,
    user: SendPtr,
}

#[derive(Debug, Default)]
struct Queues {
    global: Vec<Task>,
    worlds: HashMap<u32, Vec<Task>>,
    /// Tasks cancelled while running (they are not put back).
    cancelled: HashSet<u64>,
}

#[derive(Debug, Default)]
pub(crate) struct Tasks {
    next_id: AtomicU64,
    queues: Mutex<Queues>,
}

/// Frees a task's user data through the plugin.
fn drop_task(inner: &Inner, task: &Task) {
    let (Some(drop), Some(plugin)) = (task.drop, inner.plugin(task.plugin)) else {
        return;
    };
    plugin.call_always("dropping a task", || {
        // SAFETY: the plugin's drop function for its own user data, called once.
        unsafe { drop(task.user.0) };
        status::OK
    });
}

impl Tasks {
    pub(crate) fn schedule(
        &self,
        inner: &Inner,
        plugin: &Plugin,
        spec: &abi::TaskSpec,
    ) -> Result<u64, Status> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let now = inner.tick.load(Ordering::Acquire);
        let task = Task {
            id,
            plugin: plugin.local,
            due: now.saturating_add(spec.delay),
            period: spec.period,
            entity: (spec.owner.kind == abi::owner::ENTITY).then(|| spec.owner.entity.to_u128()),
            run: spec.run,
            drop: spec.drop,
            user: SendPtr(spec.user),
        };
        match spec.owner.kind {
            abi::owner::BACKGROUND if spec.delay == 0 => {
                let server = inner.server().ok_or(status::WRONG_PHASE)?;
                let host = HostRef(inner.self_arc().ok_or(status::WRONG_PHASE)?);
                server.spawn_background(Box::new(move || run_background(&host.0, task)));
                return Ok(id);
            }
            abi::owner::BACKGROUND | abi::owner::GLOBAL => {
                self.queues
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .global
                    .push(Task {
                        // A delayed background task waits on the global queue, then moves off.
                        period: if spec.owner.kind == abi::owner::BACKGROUND {
                            u64::MAX
                        } else {
                            task.period
                        },
                        ..task
                    });
            }
            abi::owner::WORLD | abi::owner::LOCATION | abi::owner::ENTITY => {
                if let Some(server) = inner.server()
                    && server.world_name(spec.owner.world).is_none()
                {
                    return Err(status::NOT_FOUND);
                }
                self.queues
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .worlds
                    .entry(spec.owner.world)
                    .or_default()
                    .push(task);
            }
            _ => return Err(status::UNSUPPORTED),
        }
        Ok(id)
    }

    pub(crate) fn cancel(&self, inner: &Inner, plugin: u32, id: u64) -> Status {
        let removed = {
            let mut queues = self.queues.lock().unwrap_or_else(PoisonError::into_inner);
            let mut found = None;
            let Queues { global, worlds, .. } = &mut *queues;
            for queue in std::iter::once(global).chain(worlds.values_mut()) {
                if let Some(i) = queue.iter().position(|t| t.id == id && t.plugin == plugin) {
                    found = Some(queue.swap_remove(i));
                    break;
                }
            }
            if found.is_none() {
                queues.cancelled.insert(id);
            }
            found
        };
        match removed {
            Some(task) => {
                drop_task(inner, &task);
                status::OK
            }
            None => status::OK,
        }
    }

    /// Takes the due tasks out of a queue.
    fn take_due(&self, world: Option<u32>, tick: u64) -> Vec<Task> {
        let mut queues = self.queues.lock().unwrap_or_else(PoisonError::into_inner);
        let queue = match world {
            None => &mut queues.global,
            Some(w) => match queues.worlds.get_mut(&w) {
                Some(q) => q,
                None => return Vec::new(),
            },
        };
        if queue.is_empty() {
            return Vec::new();
        }
        let mut due = Vec::new();
        let mut i = 0;
        while i < queue.len() {
            if queue.get(i).is_some_and(|t| t.due <= tick) {
                due.push(queue.swap_remove(i));
            } else {
                i += 1;
            }
        }
        // Run in scheduling order.
        due.sort_by_key(|t| t.id);
        due
    }

    /// Puts a periodic task back, or frees it.
    fn finish(&self, inner: &Inner, mut task: Task, world: Option<u32>, tick: u64, ran: Status) {
        let cancelled = self
            .queues
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .cancelled
            .remove(&task.id);
        let alive = inner
            .plugin(task.plugin)
            .is_some_and(|p| p.enabled.load(Ordering::Acquire));
        if task.period == 0 || cancelled || !alive || ran == status::PANICKED {
            drop_task(inner, &task);
            return;
        }
        task.due = tick.saturating_add(task.period);
        let mut queues = self.queues.lock().unwrap_or_else(PoisonError::into_inner);
        match world {
            None => queues.global.push(task),
            Some(w) => queues.worlds.entry(w).or_default().push(task),
        }
    }

    pub(crate) fn run_global(&self, inner: &Inner, tick: u64) {
        for task in self.take_due(None, tick) {
            if task.period == u64::MAX {
                // A delayed background task: now off to a background thread.
                if let (Some(server), Some(host)) = (inner.server(), inner.self_arc()) {
                    let host = HostRef(host);
                    let task = Task { period: 0, ..task };
                    server.spawn_background(Box::new(move || run_background(&host.0, task)));
                } else {
                    drop_task(inner, &task);
                }
                continue;
            }
            let ran = run(inner, &task, None);
            self.finish(inner, task, None, tick, ran);
        }
    }

    pub(crate) fn run_world(&self, inner: &Inner, world: &mut dyn WorldAccess, tick: u64) {
        let id = world.id();
        for task in self.take_due(Some(id), tick) {
            if let Some(entity) = task.entity
                && !world.has_entity(entity)
            {
                // The entity left this world: a player continues on its new world, anything
                // else ends.
                let moved = inner.server().and_then(|s| s.player_world(entity));
                match moved {
                    Some(w) if w != id => {
                        self.queues
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .worlds
                            .entry(w)
                            .or_default()
                            .push(task);
                    }
                    _ => drop_task(inner, &task),
                }
                continue;
            }
            let ran = run(inner, &task, Some(&mut *world));
            self.finish(inner, task, Some(id), tick, ran);
        }
    }

    /// Drops every task of a plugin (disabled or reloaded).
    pub(crate) fn drop_plugin(&self, inner: &Inner, plugin: u32) {
        let removed: Vec<Task> = {
            let mut queues = self.queues.lock().unwrap_or_else(PoisonError::into_inner);
            let mut removed = Vec::new();
            let Queues { global, worlds, .. } = &mut *queues;
            for queue in std::iter::once(global).chain(worlds.values_mut()) {
                let mut i = 0;
                while i < queue.len() {
                    if queue.get(i).is_some_and(|t| t.plugin == plugin) {
                        removed.push(queue.swap_remove(i));
                    } else {
                        i += 1;
                    }
                }
            }
            removed
        };
        for task in &removed {
            drop_task(inner, task);
        }
    }

    /// Drops every task (shutdown).
    pub(crate) fn clear(&self, inner: &Inner) {
        let removed: Vec<Task> = {
            let mut queues = self.queues.lock().unwrap_or_else(PoisonError::into_inner);
            let mut removed: Vec<Task> = std::mem::take(&mut queues.global);
            for queue in queues.worlds.values_mut() {
                removed.append(queue);
            }
            removed
        };
        for task in &removed {
            drop_task(inner, task);
        }
    }
}

fn run(inner: &Inner, task: &Task, world: Option<&mut dyn WorldAccess>) -> Status {
    let Some(plugin) = inner.plugin(task.plugin) else {
        return status::NOT_FOUND;
    };
    let mut ctx = CallCtx::new(world);
    plugin.call("a scheduled task", || {
        // SAFETY: the plugin's task and user data live until its drop function runs.
        unsafe { (task.run)(task.user.0, ctx.as_raw()) }
    })
}

/// A host reference for a background job.
struct HostRef(Arc<Inner>);

fn run_background(inner: &Inner, task: Task) {
    run(inner, &task, None);
    drop_task(inner, &task);
}

impl Inner {
    /// A strong reference to this host (for background jobs).
    pub(crate) fn self_arc(&self) -> Option<Arc<Inner>> {
        crate::HOSTS
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(usize::from(self.index))?
            .upgrade()
    }
}
