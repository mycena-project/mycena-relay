// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Offline-first MeshCoin ledger and mesh propagation hooks.
#![deny(clippy::unwrap_used, clippy::expect_used)]

mod address;
mod error;
mod hot_wallet;
mod ledger;
mod node;
mod payload;
mod payment;
mod refill;
mod settlement_policy;
mod spam_caution;
mod user_trust;
mod velocity;
mod views;
mod voucher;
mod voucher_settlement;

pub use address::{address_from_keypair, validate_address};
pub use error::{decode_peer_coin_payload, CoinError, CoinResult};
pub use hot_wallet::{HotWallet, HotWalletConfig, RefillResult};
pub use ledger::{
    AccountState, Address, ApplyResult, LocalLedger, Transaction, TxId, MUON_PER_MXC,
};
pub use node::{CoinNode, CoinTransport};
pub use payload::CoinPayload;
pub use payment::PaymentRequest;
pub use refill::{RefillRequest, REFILL_REQUEST_TTL_MS};
pub use settlement_policy::{
    SettlementPolicy, DEFAULT_MAX_BALANCE_MXC, DEFAULT_MAX_TX_MXC, HARD_MAX_BALANCE_MXC,
};
pub use spam_caution::{
    deposit_for_message_priority, priority_deposit_tiers, DEFAULT_PRIORITY_DEPOSIT_MUON,
};
pub use user_trust::{UserTrust, UserTrustStore, UserTrustSummary};
pub use views::{
    AccountBalanceView, InsufficientSettledFunds, PeerBalanceHint, SpendAuthorization,
    VoucherExposureView,
};
pub use voucher::{
    PaymentVoucher, VoucherRedemption, VoucherRiskLevel, VoucherRiskNotice, VOUCHER_DEFAULT_TTL_MS,
};
pub use voucher_settlement::{
    AcceptRedemptionResult, VoucherPartitionView, VoucherSettlementOutcome,
    VoucherSettlementRegistry,
};

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::hot_wallet::{HotWallet, HotWalletConfig};
    use super::ledger::{ApplyResult, LocalLedger, Transaction, MUON_PER_MXC};
    use super::payload::CoinPayload;
    use super::payment::PaymentRequest;
    use super::refill::RefillRequest;
    use super::views::{InsufficientSettledFunds, SpendAuthorization};
    use super::{address_from_keypair, validate_address, CoinNode, CoinTransport};
    use async_trait::async_trait;
    use libp2p::identity::Keypair;
    use mycena_l0_facade::lock_recover::lock_or_recover;
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use tempfile::tempdir;

    fn write_identity_store(path: &Path, kp: &Keypair) {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        let db = sled::open(path).unwrap();
        let tree = db.open_tree("identity").unwrap();
        let ed = kp.clone().try_into_ed25519().unwrap();
        tree.insert(b"ed25519_secret_key", ed.secret().as_ref())
            .unwrap();
        tree.flush().unwrap();
        db.flush().unwrap();
    }

    #[derive(Clone, Default)]
    struct NoopTransport;

    #[async_trait]
    impl CoinTransport for NoopTransport {
        async fn broadcast_coin_inner(&self, _inner: Vec<u8>) -> anyhow::Result<()> {
            Ok(())
        }

        async fn send_direct_coin_inner(
            &self,
            _to_peer: String,
            _inner: Vec<u8>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn apply_outgoing(ledger: &LocalLedger, tx: &Transaction) -> ApplyResult {
        let total = tx.amount_muon + tx.fee_muon;
        let auth = ledger
            .authorize_spend(&tx.from, total)
            .expect("authorize spend");
        ledger
            .apply_outgoing_transaction(tx, &auth)
            .expect("apply outgoing")
    }

    fn apply_incoming(ledger: &LocalLedger, tx: &Transaction) -> ApplyResult {
        ledger
            .apply_incoming_transaction(tx)
            .expect("apply incoming")
    }

    #[test]
    fn pending_balance_does_not_authorize_spend() {
        let dir = tempdir().unwrap();
        let ledger = LocalLedger::open(dir.path().to_str().unwrap()).unwrap();
        let sender = Keypair::generate_ed25519();
        let receiver = Keypair::generate_ed25519();
        let sender_addr = address_from_keypair(&sender);
        let receiver_addr = address_from_keypair(&receiver);
        ledger
            .genesis_credit(&sender_addr, 10 * MUON_PER_MXC)
            .unwrap();
        let tx = Transaction::new(
            receiver_addr.clone(),
            5 * MUON_PER_MXC,
            1000,
            0,
            None,
            &sender,
        )
        .unwrap();
        assert_eq!(apply_outgoing(&ledger, &tx), ApplyResult::Accepted);
        let receiver_view = ledger.settled_balance_view(&receiver_addr).unwrap();
        assert_eq!(receiver_view.spendable_muon(), 0);
        assert_eq!(
            ledger.authorize_spend(&receiver_addr, 1),
            Err(InsufficientSettledFunds {
                have_settled: 0,
                need: 1,
            })
        );
    }

    #[test]
    fn apply_incoming_credits_receiver_without_spend_token() {
        let dir = tempdir().unwrap();
        let ledger = LocalLedger::open(dir.path().to_str().unwrap()).unwrap();
        let sender = Keypair::generate_ed25519();
        let receiver = Keypair::generate_ed25519();
        let sender_addr = address_from_keypair(&sender);
        let receiver_addr = address_from_keypair(&receiver);
        ledger
            .genesis_credit(&sender_addr, 10 * MUON_PER_MXC)
            .unwrap();
        let tx = Transaction::new(
            receiver_addr.clone(),
            5 * MUON_PER_MXC,
            1000,
            0,
            None,
            &sender,
        )
        .unwrap();
        assert_eq!(apply_incoming(&ledger, &tx), ApplyResult::Accepted);
        let receiver_view = ledger.settled_balance_view(&receiver_addr).unwrap();
        assert_eq!(receiver_view.spendable_muon(), 0);
    }

    #[test]
    fn outgoing_requires_matching_spend_authorization() {
        let dir = tempdir().unwrap();
        let ledger = LocalLedger::open(dir.path().to_str().unwrap()).unwrap();
        let kp = Keypair::generate_ed25519();
        let alice = address_from_keypair(&kp);
        let bob = address_from_keypair(&Keypair::generate_ed25519());
        ledger.genesis_credit(&alice, MUON_PER_MXC).unwrap();
        let tx = Transaction::new(bob, 1000, 100, 0, None, &kp).unwrap();
        let wrong_auth = SpendAuthorization::new("mxc1other".into(), 1100);
        assert!(matches!(
            ledger.apply_outgoing_transaction(&tx, &wrong_auth).unwrap(),
            ApplyResult::Invalid(_)
        ));
    }

    #[tokio::test]
    async fn basic_transfer() {
        let dir = tempdir().unwrap();
        let ledger = LocalLedger::open(dir.path().to_str().unwrap()).unwrap();
        let keypair = Keypair::generate_ed25519();
        let alice = address_from_keypair(&keypair);
        let bob = address_from_keypair(&Keypair::generate_ed25519());
        assert!(validate_address(&bob));
        ledger.genesis_credit(&alice, 10 * MUON_PER_MXC).unwrap();
        let tx = Transaction::new(bob, 5 * MUON_PER_MXC, 1000, 0, None, &keypair).unwrap();
        assert_eq!(apply_outgoing(&ledger, &tx), ApplyResult::Accepted);
        let acc = ledger.account_state(&alice).unwrap();
        assert_eq!(acc.balance_muon, 5 * MUON_PER_MXC - 1000);
    }

    #[test]
    fn double_spend_rejected() {
        let dir = tempdir().unwrap();
        let ledger = LocalLedger::open(dir.path().to_str().unwrap()).unwrap();
        let keypair = Keypair::generate_ed25519();
        let alice = address_from_keypair(&keypair);
        let bob = address_from_keypair(&Keypair::generate_ed25519());
        let carol = address_from_keypair(&Keypair::generate_ed25519());
        ledger.genesis_credit(&alice, 10 * MUON_PER_MXC).unwrap();
        let tx1 = Transaction::new(bob, 8 * MUON_PER_MXC, 1000, 0, None, &keypair).unwrap();
        assert_eq!(apply_outgoing(&ledger, &tx1), ApplyResult::Accepted);
        let tx2 = Transaction::new(carol, 1000, 1000, 0, None, &keypair).unwrap();
        assert!(matches!(
            apply_outgoing(&ledger, &tx2),
            ApplyResult::InvalidNonce { .. }
        ));
    }

    #[test]
    fn witness_confirmation() {
        let dir = tempdir().unwrap();
        let ledger = LocalLedger::open(dir.path().to_str().unwrap()).unwrap();
        let keypair = Keypair::generate_ed25519();
        let alice = address_from_keypair(&keypair);
        let bob_addr = address_from_keypair(&Keypair::generate_ed25519());
        ledger.genesis_credit(&alice, 10 * MUON_PER_MXC).unwrap();
        let tx =
            Transaction::new(bob_addr.clone(), 5 * MUON_PER_MXC, 1000, 0, None, &keypair).unwrap();
        assert_eq!(apply_outgoing(&ledger, &tx), ApplyResult::Accepted);
        let bob = ledger.account_state(&bob_addr).unwrap();
        assert_eq!(bob.balance_muon, 0);
        assert_eq!(bob.pending_received, 5 * MUON_PER_MXC);
        ledger.add_witness(&tx.id, "peer1".into()).unwrap();
        ledger.add_witness(&tx.id, "peer2".into()).unwrap();
        ledger.add_witness(&tx.id, "peer3".into()).unwrap();
        let bob = ledger.account_state(&bob_addr).unwrap();
        assert_eq!(bob.balance_muon, 5 * MUON_PER_MXC);
        assert_eq!(bob.pending_received, 0);
    }

    #[test]
    fn settlement_policy_rejects_oversized_transfer() {
        let dir = tempdir().unwrap();
        let ledger = LocalLedger::open(dir.path().to_str().unwrap()).unwrap();
        let keypair = Keypair::generate_ed25519();
        let alice = address_from_keypair(&keypair);
        let bob = address_from_keypair(&Keypair::generate_ed25519());
        ledger.genesis_credit(&alice, 50 * MUON_PER_MXC).unwrap();
        let huge = 30 * MUON_PER_MXC;
        let tx = Transaction::new(bob, huge, 1000, 0, None, &keypair).unwrap();
        assert!(matches!(
            apply_outgoing(&ledger, &tx),
            ApplyResult::Invalid(_)
        ));
    }

    #[test]
    fn address_generation_and_validation() {
        let keypair = Keypair::generate_ed25519();
        let addr = address_from_keypair(&keypair);
        assert!(addr.starts_with("mxc1"));
        assert!(validate_address(&addr));
        assert!(!validate_address("mxc1invalid"));
    }

    #[test]
    fn payment_request_roundtrip() {
        let req = PaymentRequest::new("mxc1abc123".into(), 5_000_000, Some("Kaffee".into()));
        let uri = req.to_uri();
        assert!(uri.starts_with("mxcpay:mxc1abc123?amount=5000000"));
        let parsed = PaymentRequest::from_uri(&uri).unwrap();
        assert_eq!(parsed.to_address, "mxc1abc123");
        assert_eq!(parsed.amount_muon, 5_000_000);
        assert_eq!(parsed.memo.as_deref(), Some("Kaffee"));
    }

    #[test]
    fn payment_request_expires() {
        let mut req = PaymentRequest::new("mxc1abc".into(), 1000, None);
        req.expires_at_ms = Some(1);
        assert!(req.is_expired());
    }

    #[test]
    fn refill_request_sign_and_verify() {
        let hot = Keypair::generate_ed25519();
        let cold_addr = address_from_keypair(&Keypair::generate_ed25519());
        let req = RefillRequest::new_signed(&hot, cold_addr, 5_000_000, 1000).unwrap();
        assert!(req.verify());
    }

    #[test]
    fn refill_request_rejects_tampering() {
        let hot = Keypair::generate_ed25519();
        let cold_addr = address_from_keypair(&Keypair::generate_ed25519());
        let mut req = RefillRequest::new_signed(&hot, cold_addr, 5_000_000, 1000).unwrap();
        req.amount_muon += 1;
        assert!(!req.verify());
    }

    #[tokio::test]
    async fn cold_accepts_signed_refill_once() {
        let dir = tempdir().unwrap();
        let ledger_path = dir.path().join("ledger");
        let cold_id = dir.path().join("cold_identity");
        let cold_ledger = Arc::new(LocalLedger::open(ledger_path.to_str().unwrap()).unwrap());
        let cold_kp = Keypair::generate_ed25519();
        let cold_addr = address_from_keypair(&cold_kp);
        let hot_kp = Keypair::generate_ed25519();
        let hot_addr = address_from_keypair(&hot_kp);
        write_identity_store(&cold_id, &cold_kp);
        cold_ledger
            .genesis_credit(&cold_addr, 100 * MUON_PER_MXC)
            .unwrap();

        #[derive(Clone)]
        struct Capture(Arc<Mutex<Vec<Vec<u8>>>>);

        #[async_trait]
        impl CoinTransport for Capture {
            async fn broadcast_coin_inner(&self, inner: Vec<u8>) -> anyhow::Result<()> {
                // POISON-RECOVER: test capture vec; push is atomic.
                lock_or_recover(&self.0, "coin test capture").push(inner);
                Ok(())
            }

            async fn send_direct_coin_inner(
                &self,
                _to_peer: String,
                _inner: Vec<u8>,
            ) -> anyhow::Result<()> {
                Ok(())
            }
        }

        let outs = Arc::new(Mutex::new(Vec::new()));
        let cold_node = CoinNode::new(
            cold_ledger.clone(),
            Arc::new(Capture(outs.clone())),
            cold_addr.clone(),
            "cold-peer".into(),
            cold_id.to_str().unwrap().to_string(),
        );
        let req =
            RefillRequest::new_signed(&hot_kp, cold_addr.clone(), 10 * MUON_PER_MXC, 1000).unwrap();
        let payload = CoinPayload::RefillRequest(req.clone());
        cold_node
            .handle_incoming(payload.clone(), "p99")
            .await
            .unwrap();
        cold_node.handle_incoming(payload, "p99").await.unwrap();

        // POISON-RECOVER: test broadcast capture; len check is test-only assertion.
        assert_eq!(lock_or_recover(&outs, "coin test outs").len(), 1);
        let hot_acc = cold_ledger.account_state(&hot_addr).unwrap();
        assert_eq!(hot_acc.pending_received, 10 * MUON_PER_MXC);
        let spent = 10 * MUON_PER_MXC + 1000;
        let cold_acc = cold_ledger.account_state(&cold_addr).unwrap();
        assert_eq!(cold_acc.balance_muon, 100 * MUON_PER_MXC - spent);
    }

    #[test]
    fn unsettled_voucher_does_not_authorize_spend() {
        use super::voucher::{PaymentVoucher, VOUCHER_DEFAULT_TTL_MS};
        use super::voucher_settlement::VoucherSettlementRegistry;
        let dir = tempdir().unwrap();
        let ledger = LocalLedger::open(dir.path().join("ledger").to_str().unwrap()).unwrap();
        let reg =
            VoucherSettlementRegistry::open(dir.path().join("reg").to_str().unwrap()).unwrap();
        let issuer = Keypair::generate_ed25519();
        let merchant = Keypair::generate_ed25519();
        let merchant_addr = address_from_keypair(&merchant);
        let amount = 3 * MUON_PER_MXC;
        let voucher =
            PaymentVoucher::new_signed(&issuer, amount, VOUCHER_DEFAULT_TTL_MS, None).unwrap();
        let redemption = super::voucher::VoucherRedemption::new(
            &voucher,
            merchant_addr.clone(),
            amount,
            "local".into(),
            None,
        );
        let now = 1000u64;
        let r = reg
            .accept_redemption_offline(&voucher, &redemption, now)
            .unwrap();
        assert!(r.accepted);
        assert_eq!(r.notice.risk, super::voucher::VoucherRiskLevel::Unsettled);
        VoucherSettlementRegistry::credit_unsettled_on_ledger(&ledger, &merchant_addr, amount)
            .unwrap();
        let acc = ledger.account_state(&merchant_addr).unwrap();
        assert_eq!(acc.unsettled_voucher_muon, amount);
        assert_eq!(
            ledger.authorize_spend(&merchant_addr, 1),
            Err(InsufficientSettledFunds {
                have_settled: 0,
                need: 1,
            })
        );
    }

    #[test]
    fn voucher_settlement_claws_back_loser() {
        use super::voucher::{
            PaymentVoucher, VoucherRedemption, VoucherRiskLevel, VOUCHER_DEFAULT_TTL_MS,
        };
        use super::voucher_settlement::VoucherSettlementRegistry;
        let dir = tempdir().unwrap();
        let issuer = Keypair::generate_ed25519();
        let bob = address_from_keypair(&Keypair::generate_ed25519());
        let carol = address_from_keypair(&Keypair::generate_ed25519());
        let amount = MUON_PER_MXC;
        let voucher =
            PaymentVoucher::new_signed(&issuer, amount, VOUCHER_DEFAULT_TTL_MS, None).unwrap();
        let reg =
            VoucherSettlementRegistry::open(dir.path().join("reg").to_str().unwrap()).unwrap();
        reg.register_voucher(&voucher).unwrap();
        let bob_r = VoucherRedemption {
            voucher_id: voucher.voucher_id.clone(),
            issuer_address: voucher.issuer_address.clone(),
            recipient_address: bob.clone(),
            amount_muon: amount,
            redeemed_at_ms: 100,
            recorder_peer_id: "p1".into(),
            linked_tx_id: None,
        };
        let carol_r = VoucherRedemption {
            voucher_id: voucher.voucher_id.clone(),
            issuer_address: voucher.issuer_address.clone(),
            recipient_address: carol.clone(),
            amount_muon: amount,
            redeemed_at_ms: 200,
            recorder_peer_id: "p2".into(),
            linked_tx_id: None,
        };
        reg.accept_redemption_offline(&voucher, &bob_r, 100)
            .unwrap();
        reg.accept_redemption_offline(&voucher, &carol_r, 200)
            .unwrap();
        let outcome = reg.settle_voucher(&voucher.voucher_id, 300).unwrap();
        assert_eq!(outcome.risk, VoucherRiskLevel::DoubleSpendDetected);
        assert_eq!(outcome.canonical_recipient.as_deref(), Some(bob.as_str()));
    }

    #[test]
    fn voucher_reservation_blocks_spend() {
        use super::voucher::{PaymentVoucher, VOUCHER_DEFAULT_TTL_MS};
        let dir = tempdir().unwrap();
        let ledger = LocalLedger::open(dir.path().to_str().unwrap()).unwrap();
        let issuer = Keypair::generate_ed25519();
        let addr = address_from_keypair(&issuer);
        ledger.genesis_credit(&addr, 5 * MUON_PER_MXC).unwrap();
        ledger
            .reserve_voucher_funds(&addr, 4 * MUON_PER_MXC)
            .unwrap();
        assert_eq!(
            ledger.authorize_spend(&addr, 2 * MUON_PER_MXC),
            Err(InsufficientSettledFunds {
                have_settled: MUON_PER_MXC,
                need: 2 * MUON_PER_MXC,
            })
        );
        let _voucher =
            PaymentVoucher::new_signed(&issuer, 4 * MUON_PER_MXC, VOUCHER_DEFAULT_TTL_MS, None)
                .unwrap();
    }

    #[tokio::test]
    async fn coin_node_voucher_gossip_settlement_detects_double_spend() {
        use super::voucher::{VoucherRedemption, VoucherRiskLevel};
        use super::voucher_settlement::VoucherSettlementRegistry;
        let dir = tempdir().unwrap();
        let ledger_path = dir.path().join("ledger");
        let reg_path = dir.path().join("reg");
        let id_path = dir.path().join("identity");
        let ledger = Arc::new(LocalLedger::open(ledger_path.to_str().unwrap()).unwrap());
        let reg = Arc::new(VoucherSettlementRegistry::open(reg_path.to_str().unwrap()).unwrap());
        let alice_kp = Keypair::generate_ed25519();
        let alice = address_from_keypair(&alice_kp);
        write_identity_store(&id_path, &alice_kp);
        ledger.genesis_credit(&alice, 10 * MUON_PER_MXC).unwrap();
        let bob = address_from_keypair(&Keypair::generate_ed25519());
        let carol = address_from_keypair(&Keypair::generate_ed25519());
        let amount = 3 * MUON_PER_MXC;

        #[derive(Clone, Default)]
        struct Capture(Arc<Mutex<Vec<CoinPayload>>>);
        #[async_trait]
        impl CoinTransport for Capture {
            async fn broadcast_coin_inner(&self, inner: Vec<u8>) -> anyhow::Result<()> {
                let p: CoinPayload = bincode::deserialize(&inner)?;
                // POISON-RECOVER: test capture vec; push is atomic per payload.
                lock_or_recover(&self.0, "coin test capture").push(p);
                Ok(())
            }
            async fn send_direct_coin_inner(
                &self,
                _to_peer: String,
                _inner: Vec<u8>,
            ) -> anyhow::Result<()> {
                Ok(())
            }
        }

        let outs = Arc::new(Mutex::new(Vec::new()));
        let relay = CoinNode::new(
            ledger.clone(),
            Arc::new(Capture(outs.clone())),
            alice.clone(),
            "relay".into(),
            id_path.to_str().unwrap().to_string(),
        )
        .with_voucher_registry(reg.clone());

        let voucher = relay
            .issue_voucher(amount, None)
            .await
            .expect("issue voucher");
        // POISON-RECOVER: test outs scan; missed payload only fails test expectation.
        assert!(lock_or_recover(&outs, "coin test outs")
            .iter()
            .any(|p| matches!(p, CoinPayload::PaymentVoucher(_))));

        let bob_r = VoucherRedemption {
            voucher_id: voucher.voucher_id.clone(),
            issuer_address: voucher.issuer_address.clone(),
            recipient_address: bob.clone(),
            amount_muon: amount,
            redeemed_at_ms: 100,
            recorder_peer_id: "p-a".into(),
            linked_tx_id: None,
        };
        let carol_r = VoucherRedemption {
            voucher_id: voucher.voucher_id.clone(),
            issuer_address: voucher.issuer_address.clone(),
            recipient_address: carol.clone(),
            amount_muon: amount,
            redeemed_at_ms: 200,
            recorder_peer_id: "p-b".into(),
            linked_tx_id: None,
        };
        relay
            .handle_incoming(CoinPayload::VoucherRedemption(bob_r.clone()), "peer-a")
            .await
            .unwrap();
        relay
            .handle_incoming(CoinPayload::VoucherRedemption(carol_r.clone()), "peer-b")
            .await
            .unwrap();

        let outcome = relay
            .settle_voucher_and_broadcast(&voucher.voucher_id)
            .await
            .unwrap();
        assert_eq!(outcome.risk, VoucherRiskLevel::DoubleSpendDetected);
        assert_eq!(outcome.canonical_recipient.as_deref(), Some(bob.as_str()));
        // POISON-RECOVER: test settlement sync capture; false fails test only.
        assert!(lock_or_recover(&outs, "coin test outs")
            .iter()
            .any(|p| matches!(p, CoinPayload::VoucherSettlementSync { .. })));
    }

    #[tokio::test]
    async fn hot_wallet_enforce_cap_returns_excess() {
        let base = std::env::temp_dir().join(format!("mycena-hot-wallet-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let ledger_path = base.join("ledger");
        let id_path = base.join("identity");
        let ledger = Arc::new(LocalLedger::open(ledger_path.to_str().unwrap()).unwrap());
        let hot_kp = Keypair::generate_ed25519();
        let hot_addr = address_from_keypair(&hot_kp);
        let cold_addr = address_from_keypair(&Keypair::generate_ed25519());
        // Keep excess under single-transfer policy cap (25 MXC default).
        ledger.genesis_credit(&hot_addr, 55 * MUON_PER_MXC).unwrap();

        let id_store = id_path.to_str().unwrap();
        write_identity_store(Path::new(id_store), &hot_kp);

        let coin = Arc::new(CoinNode::new(
            ledger.clone(),
            Arc::new(NoopTransport),
            hot_addr.clone(),
            "local-peer".into(),
            id_store.to_string(),
        ));

        let max = 50 * MUON_PER_MXC;
        let cfg = HotWalletConfig {
            max_cache_muon: max,
            refill_threshold_muon: max * 80 / 100,
            refill_amount_muon: max,
            cold_wallet_address: Some(cold_addr.clone()),
        };
        let hw = HotWallet::new(cfg, coin.clone(), id_store.to_string());
        hw.enforce_cap().await.unwrap();

        let hot_bal = ledger.account_state(&hot_addr).unwrap().balance_muon;
        let cold_acc = ledger.account_state(&cold_addr).unwrap();
        let cold_credited = cold_acc.balance_muon + cold_acc.pending_received;
        assert!(hot_bal <= max);
        assert!(cold_credited >= 4 * MUON_PER_MXC);
    }
}
