//! A byte-budgeted LRU that hands back what it evicts, so the owner can release it (the
//! desktop calls `drop_image` on each decoded image). Pure: no clock, no IO.

use std::{collections::HashMap, hash::Hash};

pub struct Lru<K, V> {
    budget: usize,
    used: usize,
    tick: u64,
    map: HashMap<K, Entry<V>>,
}

struct Entry<V> {
    value: V,
    weight: usize,
    used_at: u64,
}

impl<K: Eq + Hash + Clone, V> Lru<K, V> {
    pub fn new(budget: usize) -> Self {
        Self {
            budget,
            used: 0,
            tick: 0,
            map: HashMap::new(),
        }
    }

    pub fn get(&mut self, k: &K) -> Option<&V> {
        self.tick += 1;
        let tick = self.tick;
        let e = self.map.get_mut(k)?;
        e.used_at = tick;
        Some(&e.value)
    }

    pub fn contains(&self, k: &K) -> bool {
        self.map.contains_key(k)
    }

    /// Inserts (replacing any value under `k`, which is returned too) and evicts the least
    /// recently used until the budget holds. An entry heavier than the whole budget still goes
    /// in, alone: the one big image in the dialog must show.
    #[must_use]
    pub fn insert(&mut self, k: K, v: V, weight: usize) -> Vec<V> {
        let mut out = Vec::new();
        if let Some(old) = self.map.remove(&k) {
            self.used -= old.weight;
            out.push(old.value);
        }
        while self.used + weight > self.budget && !self.map.is_empty() {
            let oldest = self
                .map
                .iter()
                .min_by_key(|(_, e)| e.used_at)
                .map(|(k, _)| k.clone())
                .expect("non-empty");
            let e = self.map.remove(&oldest).expect("present");
            self.used -= e.weight;
            out.push(e.value);
        }
        self.tick += 1;
        self.used += weight;
        self.map.insert(
            k,
            Entry {
                value: v,
                weight,
                used_at: self.tick,
            },
        );
        out
    }

    #[must_use]
    pub fn clear(&mut self) -> Vec<V> {
        self.used = 0;
        self.map.drain().map(|(_, e)| e.value).collect()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn used(&self) -> usize {
        self.used
    }
}
