// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Best-effort voucher settlement — double-spend detected at merge, not prevented offline.

use crate::ledger::{Address, LocalLedger};
use crate::voucher::{PaymentVoucher, VoucherRedemption, VoucherRiskLevel, VoucherRiskNotice};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct StoredRedemption {
    redemption: VoucherRedemption,
    risk: VoucherRiskLevel,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct StoredVoucher {
    voucher: PaymentVoucher,
    settled: bool,
    canonical_recipient: Option<String>,
}

/// Per-node registry of voucher redemptions (merged at online relay settlement).
#[derive(Debug, Clone)]
pub struct VoucherSettlementRegistry {
    db: sled::Db,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptRedemptionResult {
    pub notice: VoucherRiskNotice,
    pub accepted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoucherSettlementOutcome {
    pub voucher_id: String,
    pub risk: VoucherRiskLevel,
    pub canonical_recipient: Option<String>,
    pub conflicting_recipients: Vec<String>,
    pub detected_at_ms: u64,
}

impl VoucherSettlementRegistry {
    pub fn open(path: &str) -> anyhow::Result<Self> {
        let db = sled::Config::new()
            .path(path)
            .flush_every_ms(Some(5000))
            .open()?;
        Ok(Self { db })
    }

    pub fn register_voucher(&self, voucher: &PaymentVoucher) -> anyhow::Result<()> {
        if !voucher.verify() {
            anyhow::bail!("invalid voucher signature");
        }
        let tree = self.voucher_tree()?;
        if tree.contains_key(voucher.voucher_id.as_bytes())? {
            return Ok(());
        }
        let stored = StoredVoucher {
            voucher: voucher.clone(),
            settled: false,
            canonical_recipient: None,
        };
        tree.insert(voucher.voucher_id.as_bytes(), bincode::serialize(&stored)?)?;
        Ok(())
    }

    pub fn get_voucher(&self, voucher_id: &str) -> anyhow::Result<Option<PaymentVoucher>> {
        let tree = self.voucher_tree()?;
        let Some(bytes) = tree.get(voucher_id.as_bytes())? else {
            return Ok(None);
        };
        let stored: StoredVoucher = bincode::deserialize(&bytes)?;
        Ok(Some(stored.voucher))
    }

    /// Offline accept — always records locally; does **not** prevent partition double-spend.
    pub fn accept_redemption_offline(
        &self,
        voucher: &PaymentVoucher,
        redemption: &VoucherRedemption,
        now_ms: u64,
    ) -> anyhow::Result<AcceptRedemptionResult> {
        if voucher.voucher_id != redemption.voucher_id {
            anyhow::bail!("voucher id mismatch");
        }
        if !voucher.verify() {
            return Ok(AcceptRedemptionResult {
                notice: VoucherRiskNotice {
                    voucher_id: voucher.voucher_id.clone(),
                    amount_muon: redemption.amount_muon,
                    risk: VoucherRiskLevel::Revoked,
                    message: "Invalid voucher signature".into(),
                },
                accepted: false,
            });
        }
        if voucher.is_expired(now_ms) {
            return Ok(AcceptRedemptionResult {
                notice: VoucherRiskNotice {
                    voucher_id: voucher.voucher_id.clone(),
                    amount_muon: redemption.amount_muon,
                    risk: VoucherRiskLevel::Expired,
                    message: "Voucher expired".into(),
                },
                accepted: false,
            });
        }
        if redemption.amount_muon > voucher.max_amount_muon {
            anyhow::bail!("redemption exceeds voucher cap");
        }
        self.register_voucher(voucher)?;
        let key = redemption_key(&redemption.voucher_id, &redemption.recipient_address);
        if self.redemption_tree()?.contains_key(key.as_bytes())? {
            return Ok(AcceptRedemptionResult {
                notice: VoucherRiskNotice::unsettled(
                    voucher.voucher_id.clone(),
                    redemption.amount_muon,
                ),
                accepted: false,
            });
        }
        let stored = StoredRedemption {
            redemption: redemption.clone(),
            risk: VoucherRiskLevel::Unsettled,
        };
        self.redemption_tree()?
            .insert(key.as_bytes(), bincode::serialize(&stored)?)?;
        Ok(AcceptRedemptionResult {
            notice: VoucherRiskNotice::unsettled(
                voucher.voucher_id.clone(),
                redemption.amount_muon,
            ),
            accepted: true,
        })
    }

    /// Gossip path: ingest redemption seen on another relay (pre-merge).
    pub fn ingest_redemption_gossip(&self, redemption: &VoucherRedemption) -> anyhow::Result<()> {
        let key = redemption_key(&redemption.voucher_id, &redemption.recipient_address);
        if self.redemption_tree()?.contains_key(key.as_bytes())? {
            return Ok(());
        }
        let stored = StoredRedemption {
            redemption: redemption.clone(),
            risk: VoucherRiskLevel::Unsettled,
        };
        self.redemption_tree()?
            .insert(key.as_bytes(), bincode::serialize(&stored)?)?;
        Ok(())
    }

    pub fn redemptions_for(&self, voucher_id: &str) -> anyhow::Result<Vec<VoucherRedemption>> {
        let prefix = format!("{voucher_id}|");
        let mut out = Vec::new();
        for item in self.redemption_tree()?.scan_prefix(prefix.as_bytes()) {
            let (_, v) = item?;
            let stored: StoredRedemption = bincode::deserialize(&v)?;
            out.push(stored.redemption);
        }
        Ok(out)
    }

    /// Merge + settle — **retroactive** double-spend detection (not a pre-spend gate).
    pub fn settle_voucher(
        &self,
        voucher_id: &str,
        now_ms: u64,
    ) -> anyhow::Result<VoucherSettlementOutcome> {
        let tree = self.voucher_tree()?;
        let Some(bytes) = tree.get(voucher_id.as_bytes())? else {
            anyhow::bail!("unknown voucher");
        };
        let mut stored: StoredVoucher = bincode::deserialize(&bytes)?;
        if stored.settled {
            return Ok(VoucherSettlementOutcome {
                voucher_id: voucher_id.to_string(),
                risk: VoucherRiskLevel::Settled,
                canonical_recipient: stored.canonical_recipient.clone(),
                conflicting_recipients: Vec::new(),
                detected_at_ms: now_ms,
            });
        }

        let redemptions = self.redemptions_for(voucher_id)?;
        let recipients: HashSet<_> = redemptions
            .iter()
            .map(|r| r.recipient_address.clone())
            .collect();

        let (risk, canonical, conflicts) = if recipients.len() <= 1 {
            (
                VoucherRiskLevel::Settled,
                redemptions.first().map(|r| r.recipient_address.clone()),
                Vec::new(),
            )
        } else {
            let mut sorted = redemptions.clone();
            sorted.sort_by_key(|r| r.redeemed_at_ms);
            let winner = sorted
                .first()
                .map(|r| r.recipient_address.clone())
                .unwrap_or_default();
            let conflicts: Vec<_> = recipients.into_iter().filter(|a| a != &winner).collect();
            (
                VoucherRiskLevel::DoubleSpendDetected,
                Some(winner),
                conflicts,
            )
        };

        for item in self
            .redemption_tree()?
            .scan_prefix(format!("{voucher_id}|").as_bytes())
        {
            let (k, v) = item?;
            let mut rec: StoredRedemption = bincode::deserialize(&v)?;
            rec.risk = if Some(&rec.redemption.recipient_address) == canonical.as_ref()
                && risk == VoucherRiskLevel::DoubleSpendDetected
            {
                VoucherRiskLevel::Settled
            } else if risk == VoucherRiskLevel::DoubleSpendDetected {
                VoucherRiskLevel::DoubleSpendDetected
            } else {
                VoucherRiskLevel::Settled
            };
            self.redemption_tree()?
                .insert(k, bincode::serialize(&rec)?)?;
        }

        stored.settled = true;
        stored.canonical_recipient = canonical.clone();
        tree.insert(voucher_id.as_bytes(), bincode::serialize(&stored)?)?;

        Ok(VoucherSettlementOutcome {
            voucher_id: voucher_id.to_string(),
            risk,
            canonical_recipient: canonical,
            conflicting_recipients: conflicts,
            detected_at_ms: now_ms,
        })
    }

    /// Merge redemption views from a peer partition (sim + relay).
    pub fn merge_redemptions_from(
        &self,
        remote: &[(VoucherRedemption, VoucherRiskLevel)],
    ) -> anyhow::Result<()> {
        for (redemption, _risk) in remote {
            self.ingest_redemption_gossip(redemption)?;
        }
        Ok(())
    }

    pub fn risk_notices_for_recipient(
        &self,
        recipient: &str,
    ) -> anyhow::Result<Vec<VoucherRiskNotice>> {
        let mut notices = Vec::new();
        for item in self.redemption_tree()?.iter() {
            let (_, v) = item?;
            let stored: StoredRedemption = bincode::deserialize(&v)?;
            if stored.redemption.recipient_address != recipient {
                continue;
            }
            let message = match stored.risk {
                VoucherRiskLevel::Unsettled => {
                    VoucherRiskNotice::unsettled(
                        stored.redemption.voucher_id.clone(),
                        stored.redemption.amount_muon,
                    )
                    .message
                }
                VoucherRiskLevel::DoubleSpendDetected => {
                    VoucherRiskNotice::double_spend(
                        stored.redemption.voucher_id.clone(),
                        stored.redemption.amount_muon,
                    )
                    .message
                }
                VoucherRiskLevel::Settled => continue,
                VoucherRiskLevel::Expired => "Voucher expired".into(),
                VoucherRiskLevel::Revoked => "Voucher revoked".into(),
            };
            notices.push(VoucherRiskNotice {
                voucher_id: stored.redemption.voucher_id,
                amount_muon: stored.redemption.amount_muon,
                risk: stored.risk,
                message,
            });
        }
        Ok(notices)
    }

    /// Credit recipient `unsettled_voucher_muon` on local ledger (non-spendable until settled).
    pub fn credit_unsettled_on_ledger(
        ledger: &LocalLedger,
        recipient: &str,
        amount_muon: u64,
    ) -> anyhow::Result<()> {
        ledger.add_unsettled_voucher_credit(recipient, amount_muon)
    }

    /// Recipient-side ledger effects (promote / clawback unsettled credit).
    pub fn apply_recipient_settlement_to_ledger(
        ledger: &LocalLedger,
        outcome: &VoucherSettlementOutcome,
        redemptions: &[VoucherRedemption],
    ) -> anyhow::Result<()> {
        match outcome.risk {
            VoucherRiskLevel::Settled => {
                if let Some(winner) = &outcome.canonical_recipient {
                    if let Some(r) = redemptions.iter().find(|x| &x.recipient_address == winner) {
                        ledger.promote_voucher_to_pending(winner, r.amount_muon)?;
                    }
                }
            }
            VoucherRiskLevel::DoubleSpendDetected => {
                for r in redemptions {
                    if Some(&r.recipient_address) == outcome.canonical_recipient.as_ref() {
                        ledger.promote_voucher_to_pending(&r.recipient_address, r.amount_muon)?;
                    } else {
                        ledger.claw_back_unsettled_voucher(&r.recipient_address, r.amount_muon)?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Full settlement: issuer debit/reserve release + recipient promote/clawback.
    pub fn apply_settlement_to_ledger(
        ledger: &LocalLedger,
        voucher: &PaymentVoucher,
        outcome: &VoucherSettlementOutcome,
        redemptions: &[VoucherRedemption],
    ) -> anyhow::Result<()> {
        match outcome.risk {
            VoucherRiskLevel::Settled => {
                if let Some(winner) = &outcome.canonical_recipient {
                    if let Some(r) = redemptions.iter().find(|x| &x.recipient_address == winner) {
                        ledger.settle_voucher_from_issuer(
                            &voucher.issuer_address,
                            r.amount_muon,
                            voucher.max_amount_muon,
                        )?;
                    } else {
                        ledger.release_voucher_reservation(
                            &voucher.issuer_address,
                            voucher.max_amount_muon,
                        )?;
                    }
                } else {
                    ledger.release_voucher_reservation(
                        &voucher.issuer_address,
                        voucher.max_amount_muon,
                    )?;
                }
            }
            VoucherRiskLevel::DoubleSpendDetected => {
                if let Some(winner) = &outcome.canonical_recipient {
                    if let Some(r) = redemptions.iter().find(|x| &x.recipient_address == winner) {
                        ledger.settle_voucher_from_issuer(
                            &voucher.issuer_address,
                            r.amount_muon,
                            voucher.max_amount_muon,
                        )?;
                    }
                }
            }
            _ => {}
        }
        Self::apply_recipient_settlement_to_ledger(ledger, outcome, redemptions)
    }

    fn voucher_tree(&self) -> anyhow::Result<sled::Tree> {
        Ok(self.db.open_tree("vouchers")?)
    }

    fn redemption_tree(&self) -> anyhow::Result<sled::Tree> {
        Ok(self.db.open_tree("voucher_redemptions")?)
    }
}

fn redemption_key(voucher_id: &str, recipient: &Address) -> String {
    format!("{voucher_id}|{recipient}")
}

/// In-memory partition view for simulation (no sled).
#[derive(Debug, Default, Clone)]
pub struct VoucherPartitionView {
    pub redemptions: Vec<VoucherRedemption>,
}

impl VoucherPartitionView {
    pub fn merge_into_registry(&self, registry: &VoucherSettlementRegistry) -> anyhow::Result<()> {
        for r in &self.redemptions {
            registry.ingest_redemption_gossip(r)?;
        }
        Ok(())
    }
}
