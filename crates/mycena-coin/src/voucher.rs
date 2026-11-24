// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Signed offline payment vouchers — bounded amount + expiry, unsettled until relay settlement.

use crate::address::{address_from_public_key, validate_address};
use libp2p::identity::{Keypair, PublicKey};
use serde::{Deserialize, Serialize};

/// Default voucher lifetime (24h).
pub const VOUCHER_DEFAULT_TTL_MS: u64 = 24 * 60 * 60 * 1000;

/// Risk label shown to recipients until global settlement completes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoucherRiskLevel {
    /// Accepted locally; not yet checked against merged redemption ledger.
    Unsettled,
    /// Relay settlement found no conflicting redemption.
    Settled,
    /// Same voucher redeemed to multiple recipients — detected at merge, not prevented upfront.
    DoubleSpendDetected,
    Expired,
    Revoked,
}

impl VoucherRiskLevel {
    pub fn is_spendable(&self) -> bool {
        matches!(self, Self::Settled)
    }
}

/// Human-readable exposure for merchant UI (ADR: honesty requirement).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoucherRiskNotice {
    pub voucher_id: String,
    pub amount_muon: u64,
    pub risk: VoucherRiskLevel,
    /// e.g. "Unsettled payment — risk 5.00 MXC until settlement"
    pub message: String,
}

impl VoucherRiskNotice {
    pub fn unsettled(voucher_id: String, amount_muon: u64) -> Self {
        let mxc = amount_muon as f64 / crate::ledger::MUON_PER_MXC as f64;
        Self {
            voucher_id,
            amount_muon,
            risk: VoucherRiskLevel::Unsettled,
            message: format!(
                "Unsettled voucher payment — risk {mxc:.2} MXC until online settlement"
            ),
        }
    }

    pub fn double_spend(voucher_id: String, amount_muon: u64) -> Self {
        let mxc = amount_muon as f64 / crate::ledger::MUON_PER_MXC as f64;
        Self {
            voucher_id,
            amount_muon,
            risk: VoucherRiskLevel::DoubleSpendDetected,
            message: format!(
                "Double-spend detected at settlement — {mxc:.2} MXC may be clawed back"
            ),
        }
    }
}

/// Issuer-signed promise to pay up to `max_amount_muon` before `expires_at_ms`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PaymentVoucher {
    pub voucher_id: String,
    pub issuer_address: String,
    pub max_amount_muon: u64,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    pub memo: Option<String>,
    pub issuer_public_key: Vec<u8>,
    pub signature: Vec<u8>,
}

impl PaymentVoucher {
    pub fn new_signed(
        issuer: &Keypair,
        max_amount_muon: u64,
        ttl_ms: u64,
        memo: Option<String>,
    ) -> anyhow::Result<Self> {
        let issuer_address = crate::address::address_from_keypair(issuer);
        let issuer_public_key = issuer.public().encode_protobuf();
        let issued_at_ms = mycena_l0_facade::data::now_ms();
        let expires_at_ms = issued_at_ms.saturating_add(ttl_ms);
        let voucher_id = blake3::hash(
            &[
                issuer_address.as_bytes(),
                &max_amount_muon.to_le_bytes(),
                &issued_at_ms.to_le_bytes(),
            ]
            .concat(),
        )
        .to_hex()
        .to_string();
        let mut voucher = Self {
            voucher_id,
            issuer_address,
            max_amount_muon,
            issued_at_ms,
            expires_at_ms,
            memo,
            issuer_public_key,
            signature: Vec::new(),
        };
        let bytes = voucher.bytes_for_signing();
        voucher.signature = issuer
            .sign(&bytes)
            .map_err(|e| anyhow::anyhow!("voucher sign failed: {e}"))?;
        Ok(voucher)
    }

    pub fn verify(&self) -> bool {
        if !validate_address(&self.issuer_address) || self.max_amount_muon == 0 {
            return false;
        }
        if self.expires_at_ms < self.issued_at_ms {
            return false;
        }
        let Ok(pk) = PublicKey::try_decode_protobuf(&self.issuer_public_key) else {
            return false;
        };
        if address_from_public_key(&pk) != self.issuer_address {
            return false;
        }
        pk.verify(&self.bytes_for_signing(), &self.signature)
    }

    pub fn is_expired(&self, now_ms: u64) -> bool {
        now_ms >= self.expires_at_ms
    }

    fn bytes_for_signing(&self) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(self.voucher_id.as_bytes());
        b.extend_from_slice(self.issuer_address.as_bytes());
        b.extend_from_slice(&self.max_amount_muon.to_le_bytes());
        b.extend_from_slice(&self.issued_at_ms.to_le_bytes());
        b.extend_from_slice(&self.expires_at_ms.to_le_bytes());
        b.extend_from_slice(&self.issuer_public_key);
        if let Some(memo) = &self.memo {
            b.extend_from_slice(memo.as_bytes());
        }
        b
    }
}

/// Local redemption claim — may conflict with other partitions until merge settlement.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VoucherRedemption {
    pub voucher_id: String,
    pub issuer_address: String,
    pub recipient_address: String,
    pub amount_muon: u64,
    pub redeemed_at_ms: u64,
    /// Relay / witness that recorded this redemption (for merge gossip).
    pub recorder_peer_id: String,
    /// Optional linked mesh transaction id when issuer also broadcast a TX.
    pub linked_tx_id: Option<String>,
}

impl VoucherRedemption {
    pub fn new(
        voucher: &PaymentVoucher,
        recipient_address: String,
        amount_muon: u64,
        recorder_peer_id: String,
        linked_tx_id: Option<String>,
    ) -> Self {
        Self {
            voucher_id: voucher.voucher_id.clone(),
            issuer_address: voucher.issuer_address.clone(),
            recipient_address,
            amount_muon,
            redeemed_at_ms: mycena_l0_facade::data::now_ms(),
            recorder_peer_id,
            linked_tx_id,
        }
    }
}
