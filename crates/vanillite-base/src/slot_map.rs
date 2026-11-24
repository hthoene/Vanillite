//! A generational slot map: dense storage addressed by 8-byte keys that go stale on removal.
//!
//! Keys are a 32-bit slot index plus a 32-bit generation. Removing a value bumps the slot's
//! generation, so old keys fail lookups instead of reaching a new occupant. The packed `u64` form
//! is what plugin handles carry; `0` is never a valid key.

use std::fmt;
use std::marker::PhantomData;

/// An untyped slot map key: slot index and generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RawKey {
    index: u32,
    generation: u32,
}

impl RawKey {
    /// The slot index.
    #[inline]
    pub const fn index(self) -> u32 {
        self.index
    }

    /// The generation (odd for live keys, never 0).
    #[inline]
    pub const fn generation(self) -> u32 {
        self.generation
    }

    /// Packs the key into a `u64`: generation in the high, index in the low 32 bits.
    #[inline]
    pub const fn to_bits(self) -> u64 {
        ((self.generation as u64) << 32) | self.index as u64
    }

    /// Unpacks a key from [`Self::to_bits`]. Returns `None` for bits no slot map ever issues.
    #[inline]
    pub const fn from_bits(bits: u64) -> Option<Self> {
        let key = Self {
            index: bits as u32,
            generation: (bits >> 32) as u32,
        };
        if key.generation & 1 == 1 {
            Some(key)
        } else {
            None
        }
    }
}

/// A typed slot map key. Implement it with [`define_slot_key!`](crate::define_slot_key).
pub trait SlotKey: Copy {
    /// Wraps a raw key.
    fn from_raw(raw: RawKey) -> Self;
    /// The raw key.
    fn raw(self) -> RawKey;
}

impl SlotKey for RawKey {
    #[inline]
    fn from_raw(raw: RawKey) -> Self {
        raw
    }

    #[inline]
    fn raw(self) -> RawKey {
        self
    }
}

/// Defines a typed key for [`SlotMap`]: `define_slot_key!(pub struct EntityId;)`.
#[macro_export]
macro_rules! define_slot_key {
    ($(#[$meta:meta])* $vis:vis struct $name:ident;) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        #[repr(transparent)]
        $vis struct $name($crate::slot_map::RawKey);

        impl $crate::slot_map::SlotKey for $name {
            #[inline]
            fn from_raw(raw: $crate::slot_map::RawKey) -> Self {
                Self(raw)
            }

            #[inline]
            fn raw(self) -> $crate::slot_map::RawKey {
                self.0
            }
        }
    };
}

/// One slot. The generation is odd while occupied and even while vacant.
struct Slot<V> {
    generation: u32,
    content: Content<V>,
}

enum Content<V> {
    Occupied(V),
    /// Next vacant slot in the free list, `u32::MAX` for none.
    Vacant(u32),
}

const NO_FREE: u32 = u32::MAX;

/// A generational slot map from keys `K` to values `V`.
pub struct SlotMap<K: SlotKey, V> {
    slots: Vec<Slot<V>>,
    free_head: u32,
    len: u32,
    _key: PhantomData<fn() -> K>,
}

impl<K: SlotKey, V> SlotMap<K, V> {
    /// An empty map.
    pub const fn new() -> Self {
        Self {
            slots: Vec::new(),
            free_head: NO_FREE,
            len: 0,
            _key: PhantomData,
        }
    }

    /// An empty map with room for `capacity` values.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            slots: Vec::with_capacity(capacity),
            ..Self::new()
        }
    }

    /// Number of values.
    #[inline]
    pub const fn len(&self) -> usize {
        self.len as usize
    }

    /// Whether the map is empty.
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Inserts a value and returns its key.
    ///
    /// # Panics
    /// When more than `u32::MAX - 1` slots would be needed.
    pub fn insert(&mut self, value: V) -> K {
        self.len += 1;
        if self.free_head != NO_FREE {
            let index = self.free_head;
            let slot = &mut self.slots[index as usize];
            let Content::Vacant(next) = slot.content else {
                unreachable!("free list points at an occupied slot");
            };
            self.free_head = next;
            slot.generation = slot.generation.wrapping_add(1);
            slot.content = Content::Occupied(value);
            return K::from_raw(RawKey {
                index,
                generation: slot.generation,
            });
        }
        let index = u32::try_from(self.slots.len())
            .ok()
            .filter(|&i| i != NO_FREE)
            .expect("slot map full");
        self.slots.push(Slot {
            generation: 1,
            content: Content::Occupied(value),
        });
        K::from_raw(RawKey {
            index,
            generation: 1,
        })
    }

    /// Removes a value. Its key and all copies of it go stale.
    pub fn remove(&mut self, key: K) -> Option<V> {
        let raw = key.raw();
        let slot = self.slots.get_mut(raw.index as usize)?;
        if slot.generation != raw.generation {
            return None;
        }
        // An even generation marks the slot vacant. A slot whose generation wraps around to 0 is
        // retired instead of reused, so stale keys can never become valid again.
        slot.generation = slot.generation.wrapping_add(1);
        let retire = slot.generation == 0;
        let next = if retire { NO_FREE } else { self.free_head };
        let Content::Occupied(value) = std::mem::replace(&mut slot.content, Content::Vacant(next))
        else {
            unreachable!("odd generation on a vacant slot");
        };
        if !retire {
            self.free_head = raw.index;
        }
        self.len -= 1;
        Some(value)
    }

    /// Whether the key refers to a live value.
    #[inline]
    pub fn contains(&self, key: K) -> bool {
        self.get(key).is_some()
    }

    /// The value of a key, `None` if it is stale.
    #[inline]
    pub fn get(&self, key: K) -> Option<&V> {
        let raw = key.raw();
        match self.slots.get(raw.index as usize) {
            Some(Slot {
                generation,
                content: Content::Occupied(value),
            }) if *generation == raw.generation => Some(value),
            _ => None,
        }
    }

    /// The value of a key, mutably, `None` if it is stale.
    #[inline]
    pub fn get_mut(&mut self, key: K) -> Option<&mut V> {
        let raw = key.raw();
        match self.slots.get_mut(raw.index as usize) {
            Some(Slot {
                generation,
                content: Content::Occupied(value),
            }) if *generation == raw.generation => Some(value),
            _ => None,
        }
    }

    /// Live keys and values in slot order.
    pub fn iter(&self) -> impl Iterator<Item = (K, &V)> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| match &slot.content {
                Content::Occupied(value) => Some((
                    K::from_raw(RawKey {
                        index: index as u32,
                        generation: slot.generation,
                    }),
                    value,
                )),
                Content::Vacant(_) => None,
            })
    }

    /// Live keys and mutable values in slot order.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (K, &mut V)> {
        self.slots
            .iter_mut()
            .enumerate()
            .filter_map(|(index, slot)| match &mut slot.content {
                Content::Occupied(value) => Some((
                    K::from_raw(RawKey {
                        index: index as u32,
                        generation: slot.generation,
                    }),
                    value,
                )),
                Content::Vacant(_) => None,
            })
    }
}

impl<K: SlotKey, V> Default for SlotMap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: SlotKey, V> fmt::Debug for SlotMap<K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SlotMap")
            .field("len", &self.len)
            .field("slots", &self.slots.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    crate::define_slot_key!(
        struct TestKey;
    );

    #[test]
    fn stale_keys_fail() {
        let mut map: SlotMap<TestKey, &str> = SlotMap::new();
        let a = map.insert("a");
        assert_eq!(map.remove(a), Some("a"));
        let b = map.insert("b");
        assert_eq!(a.raw().index(), b.raw().index());
        assert_eq!(map.get(a), None);
        assert_eq!(map.get(b), Some(&"b"));
        assert_eq!(map.remove(a), None);
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn keys_pack_into_nonzero_bits() {
        let mut map: SlotMap<RawKey, u8> = SlotMap::new();
        let key = map.insert(7);
        assert_ne!(key.to_bits(), 0);
        assert_eq!(RawKey::from_bits(key.to_bits()), Some(key));
        assert_eq!(RawKey::from_bits(0), None);
    }

    #[test]
    fn iterates_live_values() {
        let mut map: SlotMap<RawKey, u32> = SlotMap::new();
        let keys: Vec<_> = (0..5).map(|i| map.insert(i)).collect();
        map.remove(keys[1]);
        map.remove(keys[3]);
        let values: Vec<_> = map.iter().map(|(_, v)| *v).collect();
        assert_eq!(values, [0, 2, 4]);
        for (_, v) in map.iter_mut() {
            *v += 1;
        }
        assert_eq!(map.get(keys[4]), Some(&5));
    }
}
