// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Last-Writer-Wins register tagged with [`super::hlc::HlcTimestamp`].

use super::hlc::HlcTimestamp;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LwwRegister<T> {
    pub value: T,
    pub ts: HlcTimestamp,
}

impl<T: Clone> LwwRegister<T> {
    pub fn new(value: T, ts: HlcTimestamp) -> Self {
        Self { value, ts }
    }

    pub fn assign(&mut self, value: T, ts: HlcTimestamp) {
        if ts.dominates(&self.ts) || ts.compare(&self.ts) == std::cmp::Ordering::Equal {
            self.value = value;
            self.ts = ts;
        }
    }

    pub fn merge(&mut self, other: &Self) {
        if other.ts.dominates(&self.ts) {
            self.value = other.value.clone();
            self.ts = other.ts;
        }
    }

    pub fn read(&self) -> &T {
        &self.value
    }
}

impl<T: Clone + Default> Default for LwwRegister<T> {
    fn default() -> Self {
        Self {
            value: T::default(),
            ts: HlcTimestamp::zero(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crdt::hlc::HlcTimestamp;

    #[test]
    fn lww_keeps_latest_hlc() {
        let mut reg = LwwRegister::new(
            "old".to_string(),
            HlcTimestamp {
                wall_ms: 1,
                logical: 0,
                actor: 1,
            },
        );
        reg.merge(&LwwRegister::new(
            "new".to_string(),
            HlcTimestamp {
                wall_ms: 2,
                logical: 0,
                actor: 1,
            },
        ));
        assert_eq!(reg.value, "new");
    }
}
