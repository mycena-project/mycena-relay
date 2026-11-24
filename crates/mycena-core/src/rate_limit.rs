// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Fixed-window rate limiter shared by transport and ingest paths.

use std::collections::HashMap;

/// Counts events per key inside a sliding fixed window.
#[derive(Debug, Clone)]
pub struct FixedWindowRateLimiter {
    limit: u32,
    window_ms: u64,
    buckets: HashMap<String, (u32, u64)>,
}

impl FixedWindowRateLimiter {
    pub fn new(limit: u32, window_ms: u64) -> Self {
        Self {
            limit,
            window_ms,
            buckets: HashMap::new(),
        }
    }

    /// Returns `true` when the event is allowed under the limit.
    pub fn allow(&mut self, key: &str, now_ms: u64) -> bool {
        let entry = self.buckets.entry(key.to_string()).or_insert((0, now_ms));
        if now_ms.saturating_sub(entry.1) >= self.window_ms {
            *entry = (0, now_ms);
        }
        if entry.0 >= self.limit {
            return false;
        }
        entry.0 += 1;
        true
    }

    pub fn limit(&self) -> u32 {
        self.limit
    }

    pub fn window_ms(&self) -> u64 {
        self.window_ms
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn fixed_window_resets_after_window() {
        let mut rl = FixedWindowRateLimiter::new(2, 1000);
        assert!(rl.allow("k", 0));
        assert!(rl.allow("k", 500));
        assert!(!rl.allow("k", 900));
        assert!(rl.allow("k", 1000));
    }
}
