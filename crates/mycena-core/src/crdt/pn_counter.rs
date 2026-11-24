// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Positive-Negative counter (PN-Counter) for inventory — detects oversell conflicts.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PnCounter {
    positive: HashMap<String, u64>,
    negative: HashMap<String, u64>,
}

impl PnCounter {
    pub fn increment(&mut self, actor: &str, delta: u64) {
        if delta == 0 {
            return;
        }
        *self.positive.entry(actor.to_string()).or_default() += delta;
    }

    pub fn decrement(&mut self, actor: &str, delta: u64) {
        if delta == 0 {
            return;
        }
        *self.negative.entry(actor.to_string()).or_default() += delta;
    }

    pub fn merge(&mut self, other: &Self) {
        for (actor, v) in &other.positive {
            let entry = self.positive.entry(actor.clone()).or_default();
            *entry = (*entry).max(*v);
        }
        for (actor, v) in &other.negative {
            let entry = self.negative.entry(actor.clone()).or_default();
            *entry = (*entry).max(*v);
        }
    }

    pub fn value(&self) -> i64 {
        let pos: u64 = self.positive.values().sum();
        let neg: u64 = self.negative.values().sum();
        pos as i64 - neg as i64
    }

    pub fn positive_by_actor(&self) -> &HashMap<String, u64> {
        &self.positive
    }

    pub fn negative_by_actor(&self) -> &HashMap<String, u64> {
        &self.negative
    }
}

/// Result of merging marketplace stock — never silently clamps oversell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StockQuantity {
    /// Converged non-negative on-hand count.
    Available(u64),
    /// PN-Counter merged below zero — requires human resolver.
    Conflict(StockConflict),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StockConflict {
    pub merged_value: i64,
    pub oversold_by: u64,
    pub positive_by_actor: HashMap<String, u64>,
    pub negative_by_actor: HashMap<String, u64>,
}

impl StockQuantity {
    pub fn from_counter(counter: &PnCounter) -> Self {
        let merged = counter.value();
        if merged >= 0 {
            Self::Available(merged as u64)
        } else {
            Self::Conflict(StockConflict {
                merged_value: merged,
                oversold_by: (-merged) as u64,
                positive_by_actor: counter.positive.clone(),
                negative_by_actor: counter.negative.clone(),
            })
        }
    }
}

/// Explicit resolver outcome — only path to clear a conflict without silent LWW.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StockResolution {
    /// Operator sets authoritative on-hand count; re-bases PN-Counter at `actor`.
    SetOnHand {
        quantity: u64,
        actor: String,
        at_ms: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplaceStock {
    pub listing_id: String,
    counter: PnCounter,
    #[serde(default)]
    pub pending_conflict: Option<StockConflict>,
}

impl MarketplaceStock {
    pub fn new(listing_id: String, initial_qty: u64, actor: &str) -> Self {
        let mut counter = PnCounter::default();
        if initial_qty > 0 {
            counter.increment(actor, initial_qty);
        }
        Self {
            listing_id,
            counter,
            pending_conflict: None,
        }
    }

    pub fn counter(&self) -> &PnCounter {
        &self.counter
    }

    pub fn record_restock(&mut self, actor: &str, qty: u64) {
        self.counter.increment(actor, qty);
        self.refresh_state();
    }

    pub fn record_sale(&mut self, actor: &str, qty: u64) {
        self.counter.decrement(actor, qty);
        self.refresh_state();
    }

    pub fn merge(&mut self, other: &Self) {
        if self.listing_id != other.listing_id {
            return;
        }
        self.counter.merge(&other.counter);
        self.refresh_state();
    }

    pub fn quantity(&self) -> StockQuantity {
        StockQuantity::from_counter(&self.counter)
    }

    pub fn apply_resolution(&mut self, resolution: StockResolution) {
        match resolution {
            StockResolution::SetOnHand {
                quantity,
                actor,
                at_ms: _,
            } => {
                self.counter = PnCounter::default();
                if quantity > 0 {
                    self.counter.increment(&actor, quantity);
                }
                self.pending_conflict = None;
            }
        }
    }

    fn refresh_state(&mut self) {
        match self.quantity() {
            StockQuantity::Available(_) => self.pending_conflict = None,
            StockQuantity::Conflict(c) => self.pending_conflict = Some(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pn_counter_detects_oversell() {
        // One physical unit: seller-a holds authoritative restock; seller-b sells offline
        // without having seen the decrement — merge must surface conflict, not clamp to 0.
        let mut seller_a = MarketplaceStock::new("item-1".into(), 1, "seller-a");
        seller_a.record_sale("seller-a", 1);
        let mut seller_b = MarketplaceStock::new("item-1".into(), 0, "seller-b");
        seller_b.record_sale("seller-b", 1);
        seller_a.merge(&seller_b);
        match seller_a.quantity() {
            StockQuantity::Conflict(c) => assert_eq!(c.oversold_by, 1),
            StockQuantity::Available(_) => panic!("expected conflict"),
        }
    }

    #[test]
    fn resolution_clears_conflict() {
        let mut a = MarketplaceStock::new("x".into(), 0, "s");
        a.record_sale("s", 1);
        let mut b = MarketplaceStock::new("x".into(), 0, "t");
        b.record_sale("t", 1);
        a.merge(&b);
        a.apply_resolution(StockResolution::SetOnHand {
            quantity: 0,
            actor: "operator".into(),
            at_ms: 1,
        });
        assert!(matches!(a.quantity(), StockQuantity::Available(0)));
    }
}
