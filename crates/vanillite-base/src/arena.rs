//! The per-tick arena for temporary data (collision candidates, path nodes, packet scratch).

use bumpalo::Bump;

/// A bump arena that is reset once per tick.
///
/// Allocation is a pointer bump; nothing is freed individually. [`Self::reset`] drops every
/// allocation at once and keeps the largest chunk, so a steady-state tick allocates no memory
/// from the system. Values must not need `Drop` (bumpalo never runs destructors).
#[derive(Debug, Default)]
pub struct TickArena {
    bump: Bump,
}

impl TickArena {
    /// An empty arena.
    pub fn new() -> Self {
        Self::default()
    }

    /// An arena with `bytes` preallocated.
    pub fn with_capacity(bytes: usize) -> Self {
        Self {
            bump: Bump::with_capacity(bytes),
        }
    }

    /// Allocates a value for the rest of the tick.
    #[inline]
    pub fn alloc<T>(&self, value: T) -> &mut T {
        self.bump.alloc(value)
    }

    /// Copies a slice into the arena.
    #[inline]
    pub fn alloc_slice_copy<T: Copy>(&self, values: &[T]) -> &mut [T] {
        self.bump.alloc_slice_copy(values)
    }

    /// A growable vector in the arena.
    #[inline]
    pub fn vec<T>(&self) -> bumpalo::collections::Vec<'_, T> {
        bumpalo::collections::Vec::new_in(&self.bump)
    }

    /// The underlying bump allocator, for arena collections.
    #[inline]
    pub fn bump(&self) -> &Bump {
        &self.bump
    }

    /// Frees everything allocated since the last reset. Called at the end of each tick.
    #[inline]
    pub fn reset(&mut self) {
        self.bump.reset();
    }

    /// Bytes currently reserved from the system.
    #[inline]
    pub fn allocated_bytes(&self) -> usize {
        self.bump.allocated_bytes()
    }
}
