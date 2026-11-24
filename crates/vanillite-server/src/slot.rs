//! Slots: replaceable systems (architecture section 5).
//!
//! A slot is a batch-shaped trait owned by the crate that defines its contract. It is filled
//! exactly once at startup, either by an embedding Rust build ([`ServerBuilder`](crate::ServerBuilder)),
//! by a native provider through the plugin host, or with the built-in implementation when
//! nothing else claimed it. After startup the slot is frozen; calls go straight to the
//! implementation through one `dyn` call per batch.

use std::fmt;

/// Who filled a slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// The implementation that ships with Vanillite.
    BuiltIn,
    /// A Rust implementation linked into a custom build.
    Embedded,
    /// A native provider, by plugin id.
    Plugin(String),
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Origin::BuiltIn => f.write_str("built-in"),
            Origin::Embedded => f.write_str("embedded"),
            Origin::Plugin(id) => write!(f, "plugin {id}"),
        }
    }
}

/// One replaceable system, filled exactly once.
pub struct Slot<P: ?Sized> {
    name: &'static str,
    filled: Option<(Box<P>, Origin)>,
}

impl<P: ?Sized> Slot<P> {
    /// An empty slot.
    pub const fn new(name: &'static str) -> Self {
        Slot { name, filled: None }
    }

    /// The slot's name, e.g. `light-engine`.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Fills the slot. Fails if it is already filled: two providers for one slot are a
    /// configuration error the admin must resolve.
    pub fn fill(&mut self, implementation: Box<P>, origin: Origin) -> Result<(), SlotError> {
        if let Some((_, existing)) = &self.filled {
            return Err(SlotError {
                slot: self.name,
                existing: existing.clone(),
                rejected: origin,
            });
        }
        self.filled = Some((implementation, origin));
        Ok(())
    }

    /// Takes the implementation at the end of startup. `None` if nobody claimed the slot;
    /// the caller then installs the built-in implementation.
    pub fn take(self) -> Option<(Box<P>, Origin)> {
        self.filled
    }
}

impl<P: ?Sized> fmt::Debug for Slot<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Slot")
            .field("name", &self.name)
            .field("origin", &self.filled.as_ref().map(|(_, origin)| origin))
            .finish()
    }
}

/// A slot was filled twice.
#[derive(Debug)]
pub struct SlotError {
    /// The slot.
    pub slot: &'static str,
    /// Who filled it first.
    pub existing: Origin,
    /// Who tried to fill it again.
    pub rejected: Origin,
}

impl fmt::Display for SlotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "slot {} is already filled by {}; {} cannot replace it",
            self.slot, self.existing, self.rejected
        )
    }
}

impl std::error::Error for SlotError {}
