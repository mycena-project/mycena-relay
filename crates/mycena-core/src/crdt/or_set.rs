// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Observed-Remove set (OR-Set) with tag compaction to bound tombstone growth.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Unique tag for one add operation (actor hash + monotonic seq).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OrTag {
    pub actor: u64,
    pub seq: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OrSet<T>
where
    T: Clone + Eq + std::hash::Hash,
{
    /// Element → live add tags.
    adds: HashMap<T, HashSet<OrTag>>,
    /// Removed tags (observed-remove semantics).
    tombstones: HashSet<OrTag>,
    next_seq: u64,
    actor: u64,
}

impl<T> OrSet<T>
where
    T: Clone + Eq + std::hash::Hash,
{
    pub fn new(actor_id: &str) -> Self {
        Self {
            adds: HashMap::new(),
            tombstones: HashSet::new(),
            next_seq: 0,
            actor: super::hlc::stable_actor_hash(actor_id),
        }
    }

    fn bump_seq(&mut self) -> OrTag {
        self.next_seq = self.next_seq.saturating_add(1);
        OrTag {
            actor: self.actor,
            seq: self.next_seq,
        }
    }

    pub fn add(&mut self, element: T) -> OrTag {
        let tag = self.bump_seq();
        self.adds.entry(element).or_default().insert(tag);
        tag
    }

    pub fn remove(&mut self, element: &T) {
        if let Some(tags) = self.adds.get(element) {
            for tag in tags {
                self.tombstones.insert(*tag);
            }
        }
    }

    pub fn contains(&self, element: &T) -> bool {
        self.live_tags(element).next().is_some()
    }

    fn live_tags<'a>(&'a self, element: &'a T) -> impl Iterator<Item = OrTag> + 'a {
        self.adds
            .get(element)
            .into_iter()
            .flat_map(|tags| tags.iter().copied())
            .filter(|tag| !self.tombstones.contains(tag))
    }

    pub fn elements(&self) -> Vec<T>
    where
        T: Ord,
    {
        let mut out: Vec<T> = self
            .adds
            .keys()
            .filter(|e| self.contains(e))
            .cloned()
            .collect();
        out.sort();
        out
    }

    pub fn merge(&mut self, other: &Self) {
        for (elem, tags) in &other.adds {
            self.adds
                .entry(elem.clone())
                .or_default()
                .extend(tags.iter().copied());
        }
        self.tombstones.extend(other.tombstones.iter().copied());
        self.next_seq = self.next_seq.max(other.next_seq);
        self.compact_tombstones();
    }

    /// Apply observed removes to `adds`, drop empty elements, then clear applied tombstones.
    pub fn compact_tombstones(&mut self) {
        for tags in self.adds.values_mut() {
            tags.retain(|t| !self.tombstones.contains(t));
        }
        self.adds.retain(|_, tags| !tags.is_empty());
        self.tombstones.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn or_set_add_remove_add_again() {
        let mut set = OrSet::new("peer-a");
        set.add("alice".to_string());
        assert!(set.contains(&"alice".to_string()));
        set.remove(&"alice".to_string());
        assert!(!set.contains(&"alice".to_string()));
        set.add("alice".to_string());
        assert!(set.contains(&"alice".to_string()));
    }

    #[test]
    fn or_set_merge_converges() {
        let mut a = OrSet::new("a");
        a.add("x".to_string());
        let mut b = OrSet::new("b");
        b.add("y".to_string());
        a.merge(&b);
        b.merge(&a);
        assert_eq!(a.elements(), b.elements());
    }

    #[test]
    fn tombstones_compact_after_merge() {
        let mut set = OrSet::new("a");
        set.add("m".to_string());
        set.remove(&"m".to_string());
        set.compact_tombstones();
        assert!(set.tombstones.is_empty());
    }
}
