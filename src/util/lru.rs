//! A small least-recently-used map.
//!
//! Eviction scans for the oldest entry. That is O(n), which is cheaper than linked-list
//! bookkeeping for the few hundred entries this app keeps.

use std::collections::HashMap;
use std::hash::Hash;

pub struct Lru<K, V> {
    map: HashMap<K, (V, u64)>,
    capacity: usize,
    tick: u64,
}

impl<K: Eq + Hash + Clone, V> Lru<K, V> {
    pub fn new(capacity: usize) -> Lru<K, V> {
        Lru {
            map: HashMap::with_capacity(capacity.min(64)),
            capacity: capacity.max(1),
            tick: 0,
        }
    }

    /// Returns the value and marks it as most recently used.
    pub fn get(&mut self, key: &K) -> Option<&V> {
        self.tick += 1;
        let tick = self.tick;
        match self.map.get_mut(key) {
            Some(entry) => {
                entry.1 = tick;
                Some(&entry.0)
            }
            None => None,
        }
    }

    /// Inserts or replaces a value, evicting the least recently used entry when full.
    pub fn insert(&mut self, key: K, value: V) {
        self.tick += 1;
        if !self.map.contains_key(&key) && self.map.len() >= self.capacity {
            let oldest = self
                .map
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(k, _)| k.clone());
            if let Some(oldest) = oldest {
                self.map.remove(&oldest);
            }
        }
        self.map.insert(key, (value, self.tick));
    }

    pub fn remove(&mut self, key: &K) -> Option<V> {
        self.map.remove(key).map(|(v, _)| v)
    }

    pub fn clear(&mut self) {
        self.map.clear();
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::Lru;

    #[test]
    fn evicts_least_recently_used() {
        let mut lru = Lru::new(2);
        lru.insert("a", 1);
        lru.insert("b", 2);
        assert_eq!(lru.get(&"a"), Some(&1)); // "b" is now the oldest
        lru.insert("c", 3);
        assert_eq!(lru.get(&"b"), None);
        assert_eq!(lru.get(&"a"), Some(&1));
        assert_eq!(lru.get(&"c"), Some(&3));
        assert_eq!(lru.len(), 2);
    }

    #[test]
    fn replacing_does_not_evict() {
        let mut lru = Lru::new(2);
        lru.insert(1, "x");
        lru.insert(2, "y");
        lru.insert(2, "z");
        assert_eq!(lru.len(), 2);
        assert_eq!(lru.get(&1), Some(&"x"));
        assert_eq!(lru.get(&2), Some(&"z"));
    }
}
