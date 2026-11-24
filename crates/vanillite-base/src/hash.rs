//! Hashing policy (SEC-27).
//!
//! - **Server keys** (ids, positions, handles, registry indices: values the server chooses) use
//!   the fast, unkeyed Fx hash: [`FxHashMap`], [`FxHashSet`].
//! - **Untrusted keys** (anything a client, a file or a plugin can choose freely: names, strings,
//!   arbitrary numbers from packets) use a per-process randomly keyed SipHash so nobody can
//!   craft collisions: [`SeededHashMap`], [`SeededHashSet`].
//!
//! When in doubt, the key is untrusted.

use std::collections::{HashMap, HashSet};

pub use rustc_hash::{FxBuildHasher, FxHasher};
pub use std::hash::RandomState as SeededBuildHasher;

/// A hash map for server-chosen keys.
pub type FxHashMap<K, V> = HashMap<K, V, FxBuildHasher>;
/// A hash set for server-chosen keys.
pub type FxHashSet<T> = HashSet<T, FxBuildHasher>;
/// A hash map for keys from untrusted input, keyed randomly per process.
pub type SeededHashMap<K, V> = HashMap<K, V, SeededBuildHasher>;
/// A hash set for keys from untrusted input, keyed randomly per process.
pub type SeededHashSet<T> = HashSet<T, SeededBuildHasher>;
