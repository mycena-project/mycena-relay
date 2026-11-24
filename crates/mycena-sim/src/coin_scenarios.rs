// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Voucher double-spend simulation — detection at merge, not prevention offline.

use libp2p::identity::Keypair;
use mycena_coin::{
    address_from_keypair, LocalLedger, PaymentVoucher, VoucherPartitionView, VoucherRedemption,
    VoucherRiskLevel, VoucherSettlementRegistry, MUON_PER_MXC, VOUCHER_DEFAULT_TTL_MS,
};
use std::path::Path;
use tempfile::TempDir;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoucherDoubleSpendReport {
    pub detected: bool,
    pub canonical_recipient: Option<String>,
    pub loser_clawed_back_muon: u64,
    pub winner_pending_muon: u64,
}

/// Classic partition double-spend: Alice issues one voucher; Bob and Carol each redeem
/// in isolated partitions; merge settlement detects conflict (earliest redemption wins).
pub fn voucher_partition_double_spend() -> VoucherDoubleSpendReport {
    let base = TempDir::new().unwrap();
    run_voucher_double_spend_at(base.path())
}

fn run_voucher_double_spend_at(base: &Path) -> VoucherDoubleSpendReport {
    let alice_kp = Keypair::generate_ed25519();
    let bob_kp = Keypair::generate_ed25519();
    let carol_kp = Keypair::generate_ed25519();
    let _alice = address_from_keypair(&alice_kp);
    let bob = address_from_keypair(&bob_kp);
    let carol = address_from_keypair(&carol_kp);

    let amount = 5 * MUON_PER_MXC;
    let voucher =
        PaymentVoucher::new_signed(&alice_kp, amount, VOUCHER_DEFAULT_TTL_MS, None).unwrap();
    assert!(voucher.verify());

    // Partition A — Bob redeems first (t=1000)
    let ledger_a = LocalLedger::open(base.join("ledger_a").to_str().unwrap()).unwrap();
    let reg_a = VoucherSettlementRegistry::open(base.join("reg_a").to_str().unwrap()).unwrap();
    let bob_redemption = VoucherRedemption {
        voucher_id: voucher.voucher_id.clone(),
        issuer_address: voucher.issuer_address.clone(),
        recipient_address: bob.clone(),
        amount_muon: amount,
        redeemed_at_ms: 1000,
        recorder_peer_id: "relay-a".into(),
        linked_tx_id: None,
    };
    let accept_bob = reg_a
        .accept_redemption_offline(&voucher, &bob_redemption, 1000)
        .unwrap();
    assert!(accept_bob.accepted);
    assert_eq!(accept_bob.notice.risk, VoucherRiskLevel::Unsettled);
    VoucherSettlementRegistry::credit_unsettled_on_ledger(&ledger_a, &bob, amount).unwrap();
    let partition_a = VoucherPartitionView {
        redemptions: vec![bob_redemption.clone()],
    };

    // Partition B — Carol redeems later (t=2000), same voucher
    let ledger_b = LocalLedger::open(base.join("ledger_b").to_str().unwrap()).unwrap();
    let reg_b = VoucherSettlementRegistry::open(base.join("reg_b").to_str().unwrap()).unwrap();
    reg_b.register_voucher(&voucher).unwrap();
    let carol_redemption = VoucherRedemption {
        voucher_id: voucher.voucher_id.clone(),
        issuer_address: voucher.issuer_address.clone(),
        recipient_address: carol.clone(),
        amount_muon: amount,
        redeemed_at_ms: 2000,
        recorder_peer_id: "relay-b".into(),
        linked_tx_id: None,
    };
    let accept_carol = reg_b
        .accept_redemption_offline(&voucher, &carol_redemption, 2000)
        .unwrap();
    assert!(accept_carol.accepted);
    VoucherSettlementRegistry::credit_unsettled_on_ledger(&ledger_b, &carol, amount).unwrap();
    let partition_b = VoucherPartitionView {
        redemptions: vec![carol_redemption.clone()],
    };

    // Both partitions believed payment succeeded offline
    assert_eq!(
        ledger_a.account_state(&bob).unwrap().unsettled_voucher_muon,
        amount
    );
    assert_eq!(
        ledger_b
            .account_state(&carol)
            .unwrap()
            .unsettled_voucher_muon,
        amount
    );

    // Network merge at online relay
    let relay_base = base.join("relay");
    std::fs::create_dir_all(&relay_base).unwrap();
    let relay_ledger = LocalLedger::open(relay_base.join("ledger").to_str().unwrap()).unwrap();
    relay_ledger
        .genesis_credit(&_alice, 10 * MUON_PER_MXC)
        .unwrap();
    relay_ledger.reserve_voucher_funds(&_alice, amount).unwrap();
    let relay_reg =
        VoucherSettlementRegistry::open(relay_base.join("registry").to_str().unwrap()).unwrap();
    relay_reg.register_voucher(&voucher).unwrap();
    partition_a.merge_into_registry(&relay_reg).unwrap();
    partition_b.merge_into_registry(&relay_reg).unwrap();

    let outcome = relay_reg.settle_voucher(&voucher.voucher_id, 3000).unwrap();
    assert_eq!(outcome.risk, VoucherRiskLevel::DoubleSpendDetected);
    assert_eq!(outcome.canonical_recipient.as_deref(), Some(bob.as_str()));
    assert!(outcome.conflicting_recipients.contains(&carol));

    let merged = relay_reg.redemptions_for(&voucher.voucher_id).unwrap();
    VoucherSettlementRegistry::apply_settlement_to_ledger(
        &relay_ledger,
        &voucher,
        &outcome,
        &merged,
    )
    .unwrap();

    // Partition ledgers sync recipient-side effects only
    VoucherSettlementRegistry::apply_recipient_settlement_to_ledger(&ledger_a, &outcome, &merged)
        .unwrap();
    VoucherSettlementRegistry::apply_recipient_settlement_to_ledger(&ledger_b, &outcome, &merged)
        .unwrap();

    let bob_after = ledger_a.account_state(&bob).unwrap();
    let carol_after = ledger_b.account_state(&carol).unwrap();
    assert_eq!(bob_after.unsettled_voucher_muon, 0);
    assert_eq!(bob_after.pending_received, amount);
    assert_eq!(carol_after.unsettled_voucher_muon, 0);
    assert_eq!(carol_after.pending_received, 0);

    let carol_notice = relay_reg.risk_notices_for_recipient(&carol).unwrap();
    assert!(carol_notice
        .iter()
        .any(|n| n.risk == VoucherRiskLevel::DoubleSpendDetected));

    VoucherDoubleSpendReport {
        detected: true,
        canonical_recipient: outcome.canonical_recipient,
        loser_clawed_back_muon: amount,
        winner_pending_muon: amount,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voucher_double_spend_detected_at_merge_not_prevented_offline() {
        let report = voucher_partition_double_spend();
        assert!(report.detected);
        assert_eq!(report.loser_clawed_back_muon, 5 * MUON_PER_MXC);
        assert_eq!(report.winner_pending_muon, 5 * MUON_PER_MXC);
    }
}
