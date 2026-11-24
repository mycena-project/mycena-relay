// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Hybrid Logical Clock (HLC) — wall time + logical counter + actor tie-break.
//!
//! Avoids pure wall-clock LWW errors when devices lack synchronized time (offline mesh).

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

/// Lamport-style extension of physical time (Kulkarni et al., HLC 2014).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HlcTimestamp {
    pub wall_ms: u64,
    pub logical: u32,
    /// Stable device / node id for deterministic tie-break (not wall clock).
    pub actor: u64,
}

impl HlcTimestamp {
    pub fn zero(actor: u64) -> Self {
        Self {
            wall_ms: 0,
            logical: 0,
            actor,
        }
    }

    pub fn compare(&self, other: &Self) -> Ordering {
        match self.wall_ms.cmp(&other.wall_ms) {
            Ordering::Equal => match self.logical.cmp(&other.logical) {
                Ordering::Equal => self.actor.cmp(&other.actor),
                o => o,
            },
            o => o,
        }
    }

    pub fn dominates(&self, other: &Self) -> bool {
        self.compare(other) == Ordering::Greater
    }
}

impl PartialOrd for HlcTimestamp {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.compare(other))
    }
}

/// Local HLC state; persist per device and advance on each local event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HlcClock {
    pub last: HlcTimestamp,
}

impl HlcClock {
    pub fn new(actor_id: &str) -> Self {
        Self {
            last: HlcTimestamp::zero(stable_actor_hash(actor_id)),
        }
    }

    /// Advance for a local write; `wall_ms` should come from [`crate::data::now_ms`].
    pub fn tick(&mut self, wall_ms: u64) -> HlcTimestamp {
        if wall_ms > self.last.wall_ms {
            self.last.wall_ms = wall_ms;
            self.last.logical = 0;
        } else {
            self.last.logical = self.last.logical.saturating_add(1);
        }
        self.last
    }

    /// Merge remote timestamp into local clock (on receive before comparing registers).
    pub fn observe(&mut self, remote: HlcTimestamp) {
        let wall_ms = crate::data::now_ms()
            .max(remote.wall_ms)
            .max(self.last.wall_ms);
        if remote.wall_ms > self.last.wall_ms {
            self.last.wall_ms = remote.wall_ms;
            self.last.logical = remote.logical;
        } else if remote.wall_ms == self.last.wall_ms {
            self.last.logical = self.last.logical.max(remote.logical);
        }
        if wall_ms > self.last.wall_ms {
            self.last.wall_ms = wall_ms;
            self.last.logical = 0;
        } else if wall_ms == self.last.wall_ms {
            self.last.logical = self.last.logical.saturating_add(1);
        }
        let _ = self.last.actor;
    }
}

/// Stable 64-bit actor tag from an arbitrary string (peer id, UUID, account id).
pub fn stable_actor_hash(actor_id: &str) -> u64 {
    let hash = blake3::hash(actor_id.as_bytes());
    let bytes = hash.as_bytes();
    let mut arr = [0u8; 8];
    arr.copy_from_slice(&bytes[..8]);
    u64::from_le_bytes(arr)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn hlc_tick_increments_logical_on_same_wall_ms() {
        let mut clock = HlcClock::new("device-a");
        let t1 = clock.tick(1000);
        let t2 = clock.tick(1000);
        assert_eq!(t1.wall_ms, 1000);
        assert_eq!(t2.wall_ms, 1000);
        assert!(t2.logical > t1.logical);
    }

    #[test]
    fn hlc_observe_merges_remote_ahead() {
        let mut clock = HlcClock::new("a");
        let _ = clock.tick(100);
        let remote = HlcTimestamp {
            wall_ms: 500,
            logical: 2,
            actor: 99,
        };
        clock.observe(remote);
        assert!(clock.last.wall_ms >= 500);
    }

    #[test]
    fn timestamp_order_total() {
        let a = HlcTimestamp {
            wall_ms: 1,
            logical: 0,
            actor: 1,
        };
        let b = HlcTimestamp {
            wall_ms: 1,
            logical: 1,
            actor: 0,
        };
        assert!(b.dominates(&a));
    }
}
