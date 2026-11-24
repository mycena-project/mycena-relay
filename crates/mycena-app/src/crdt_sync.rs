// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Mesh propagation helpers for CRDT documents (contacts, groups, marketplace stock).

use mycena_l0_facade::crdt::{MarketplaceStock, StockQuantity};
use serde_json::{json, Value};

pub const CONTACT_CRDT_BODY: &str = "[contact:crdt]";
pub const MARKETPLACE_STOCK_SCOPE: &str = "mycena/marketplace/stock/v1";

pub fn group_meta_scope(group_id: &str) -> String {
    format!("mycena/group-meta/{group_id}")
}

pub fn stock_quantity_json(q: &StockQuantity) -> Value {
    match q {
        StockQuantity::Available(n) => json!({ "status": "available", "qty": n }),
        StockQuantity::Conflict(c) => json!({
            "status": "conflict",
            "qty": 0,
            "oversold_by": c.oversold_by,
            "merged_value": c.merged_value,
        }),
    }
}

pub fn marketplace_stock_json(stock: &MarketplaceStock) -> Value {
    let mut out = stock_quantity_json(&stock.quantity());
    if let Some(obj) = out.as_object_mut() {
        obj.insert("listing_id".into(), json!(stock.listing_id));
    }
    out
}
