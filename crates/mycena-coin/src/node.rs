// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
use crate::address::address_from_keypair;
use crate::ledger::{ApplyResult, LocalLedger, Transaction};
use crate::payload::CoinPayload;
use crate::settlement_policy::SettlementPolicy;
use crate::views::{
    InsufficientSettledFunds, PeerBalanceHint, SpendAuthorization, VoucherExposureView,
};
use crate::voucher::{PaymentVoucher, VoucherRedemption, VOUCHER_DEFAULT_TTL_MS};
use crate::voucher_settlement::{VoucherSettlementOutcome, VoucherSettlementRegistry};
use async_trait::async_trait;
use mycena_l0_facade::consistency::{
    EventuallyConsistentModel, SettlementRequiredModel, OP_COIN_TRANSFER,
};
use std::sync::Arc;
use tracing::{info, warn};

/// Broadcast / direct-send of coin frames (`AppMessage` with [`CoinPayload`] bytes) is implemented by the host (e.g. FFI).
#[async_trait]
pub trait CoinTransport: Send + Sync {
    async fn broadcast_coin_inner(&self, coin_inner: Vec<u8>) -> anyhow::Result<()>;
    async fn send_direct_coin_inner(
        &self,
        to_peer: String,
        coin_inner: Vec<u8>,
    ) -> anyhow::Result<()>;

    /// Voucher gossip — hosts should filter to voucher-capable peers.
    async fn broadcast_coin_voucher_inner(&self, coin_inner: Vec<u8>) -> anyhow::Result<()> {
        self.broadcast_coin_inner(coin_inner).await
    }

    /// Whether a peer negotiated payment voucher v1 (default: permissive for tests).
    async fn peer_payment_voucher_v1_allowed(&self, _peer: &str) -> bool {
        true
    }
}

#[derive(Clone)]
pub struct CoinNode {
    ledger: Arc<LocalLedger>,
    transport: Arc<dyn CoinTransport>,
    local_address: String,
    local_peer_id: String,
    /// Path to sled identity store for this node's coin keypair (cold signs outgoing refills).
    coin_identity_path: String,
    voucher_registry: Option<Arc<VoucherSettlementRegistry>>,
    /// Respond to peer balance probes on coin gossip (SD-002). Off by default.
    respond_to_balance_queries: bool,
}

impl std::fmt::Debug for CoinNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoinNode")
            .field("local_address", &self.local_address)
            .field("local_peer_id", &self.local_peer_id)
            .finish_non_exhaustive()
    }
}

impl CoinNode {
    pub fn new(
        ledger: Arc<LocalLedger>,
        transport: Arc<dyn CoinTransport>,
        local_address: String,
        local_peer_id: String,
        coin_identity_path: String,
    ) -> Self {
        Self {
            ledger,
            transport,
            local_address,
            local_peer_id,
            coin_identity_path,
            voucher_registry: None,
            respond_to_balance_queries: false,
        }
    }

    /// Opt in to answering [`CoinPayload::BalanceQuery`] (leaks balance metadata to peers).
    pub fn with_balance_query_responses(mut self, enabled: bool) -> Self {
        self.respond_to_balance_queries = enabled;
        self
    }

    pub fn with_voucher_registry(mut self, registry: Arc<VoucherSettlementRegistry>) -> Self {
        self.voucher_registry = Some(registry);
        self
    }

    pub fn voucher_registry(&self) -> Option<&Arc<VoucherSettlementRegistry>> {
        self.voucher_registry.as_ref()
    }

    pub async fn broadcast_coin_payload(&self, payload: CoinPayload) -> anyhow::Result<()> {
        let inner = bincode::serialize(&payload)?;
        if payload.requires_voucher_v1_capability() {
            self.transport.broadcast_coin_voucher_inner(inner).await
        } else {
            self.transport.broadcast_coin_inner(inner).await
        }
    }

    pub fn local_address(&self) -> &str {
        &self.local_address
    }

    pub fn coin_identity_path(&self) -> &str {
        &self.coin_identity_path
    }

    pub fn settlement_policy(&self) -> SettlementPolicy {
        self.ledger.policy().clone()
    }

    /// Authorize spending settled local balance ([`OP_COIN_TRANSFER`]).
    pub fn authorize_local_spend(
        &self,
        total_muon: u64,
    ) -> Result<SpendAuthorization, InsufficientSettledFunds> {
        self.ledger.authorize_spend(&self.local_address, total_muon)
    }

    pub async fn submit_transfer_from_identity_path(
        &self,
        identity_path: &str,
        to: String,
        amount_muon: u64,
        fee_muon: u64,
        memo: Option<String>,
    ) -> anyhow::Result<()> {
        let kp = mycena_l1_facade::load_or_create_keypair(identity_path)?;
        if address_from_keypair(&kp) != self.local_address {
            anyhow::bail!("identity does not match coin address");
        }
        let total = amount_muon
            .checked_add(fee_muon)
            .ok_or_else(|| anyhow::anyhow!("amount overflow"))?;
        let auth = self.authorize_local_spend(total).map_err(|e| {
            anyhow::anyhow!(
                "insufficient settled balance: have {} need {} ({})",
                e.have_settled,
                e.need,
                OP_COIN_TRANSFER.name
            )
        })?;
        let nonce = self.ledger.account_state(&self.local_address)?.next_nonce;
        let tx = Transaction::new(to, amount_muon, fee_muon, nonce, memo, &kp)?;
        self.send_transaction(tx, auth).await
    }

    pub async fn send_transaction(
        &self,
        tx: Transaction,
        auth: SpendAuthorization,
    ) -> anyhow::Result<()> {
        if tx.from != self.local_address {
            anyhow::bail!("transaction from address does not match local wallet");
        }
        match self.ledger.apply_outgoing_transaction(&tx, &auth)? {
            ApplyResult::Accepted => {
                info!("TX {} accepted locally, broadcasting", tx.id);
                let inner = bincode::serialize(&CoinPayload::Transaction(tx))?;
                self.transport.broadcast_coin_inner(inner).await?;
                Ok(())
            }
            result => Err(anyhow::anyhow!("TX rejected: {result:?}")),
        }
    }

    pub async fn handle_incoming(
        &self,
        payload: CoinPayload,
        from_peer: &str,
    ) -> anyhow::Result<()> {
        if payload.requires_voucher_v1_capability()
            && !self
                .transport
                .peer_payment_voucher_v1_allowed(from_peer)
                .await
        {
            warn!("dropping voucher gossip from {from_peer}: payment voucher v1 not negotiated");
            return Ok(());
        }
        match payload {
            CoinPayload::Transaction(tx) => match self.ledger.apply_incoming_transaction(&tx)? {
                ApplyResult::Accepted => {
                    info!("TX {} received and accepted", tx.id);
                    let _ = self
                        .ledger
                        .add_witness(&tx.id, self.local_peer_id.clone())?;
                    let witness = CoinPayload::Witness {
                        tx_id: tx.id.clone(),
                        peer_id: self.local_peer_id.clone(),
                    };
                    let inner = bincode::serialize(&witness)?;
                    self.transport.broadcast_coin_inner(inner).await?;
                }
                ApplyResult::Duplicate => {
                    if self
                        .ledger
                        .add_witness(&tx.id, self.local_peer_id.clone())?
                    {
                        let witness = CoinPayload::Witness {
                            tx_id: tx.id.clone(),
                            peer_id: self.local_peer_id.clone(),
                        };
                        let inner = bincode::serialize(&witness)?;
                        self.transport.broadcast_coin_inner(inner).await?;
                    }
                }
                result => {
                    warn!("TX from {from_peer} rejected: {result:?}");
                }
            },
            CoinPayload::Witness { tx_id, peer_id } => {
                let _ = self.ledger.add_witness(&tx_id, peer_id)?;
            }
            CoinPayload::BalanceQuery { address } => {
                if !self.respond_to_balance_queries {
                    return Ok(());
                }
                let acc = self.ledger.account_state(&address)?;
                let view = self.ledger.eventual_balance_view(&address)?;
                let confirmed = view.confirmed_muon();
                let pending = view.pending_muon();
                let unsettled = acc.unsettled_voucher_muon;
                let _hint = PeerBalanceHint::with_voucher_exposure(
                    view.address.clone(),
                    confirmed,
                    pending,
                    unsettled,
                );
                let response = CoinPayload::BalanceResponse {
                    address: view.address,
                    balance_muon: confirmed,
                    pending_muon: pending,
                    unsettled_voucher_muon: unsettled,
                };
                let inner = bincode::serialize(&response)?;
                self.transport
                    .send_direct_coin_inner(from_peer.to_string(), inner)
                    .await?;
            }
            CoinPayload::BalanceResponse { .. } => {}
            CoinPayload::PaymentVoucher(voucher) => {
                if let Some(reg) = &self.voucher_registry {
                    if voucher.verify() {
                        let _ = reg.register_voucher(&voucher);
                    }
                }
            }
            CoinPayload::VoucherRedemption(redemption) => {
                self.handle_voucher_redemption(&redemption, None).await?;
            }
            CoinPayload::VoucherSettlementSync {
                voucher_id,
                redemptions,
            } => {
                self.merge_voucher_redemptions(&voucher_id, &redemptions)
                    .await?;
            }
            CoinPayload::RefillRequest(req) => {
                if req.cold_address != self.local_address {
                    return Ok(());
                }
                if mycena_l0_facade::data::now_ms() > req.expires_at_ms {
                    warn!("refill request expired from {from_peer}");
                    return Ok(());
                }
                if !req.verify() {
                    warn!("invalid refill request from {from_peer}");
                    return Ok(());
                }
                if self.ledger.refill_already_applied(&req.request_id)? {
                    return Ok(());
                }
                match self
                    .submit_transfer_from_identity_path(
                        &self.coin_identity_path,
                        req.hot_address.clone(),
                        req.amount_muon,
                        req.fee_muon,
                        Some("refill".into()),
                    )
                    .await
                {
                    Ok(()) => {
                        if let Err(e) = self.ledger.mark_refill_applied(&req.request_id) {
                            warn!(
                                "failed to record refill request_id {}: {e:#}",
                                req.request_id
                            );
                        }
                    }
                    Err(e) => {
                        warn!("refill transfer failed: {e:#}");
                    }
                }
            }
        }
        Ok(())
    }

    /// Issue a signed voucher, reserve issuer funds locally, register + gossip.
    pub async fn issue_voucher_from_identity_path(
        &self,
        identity_path: &str,
        max_amount_muon: u64,
        ttl_ms: u64,
        memo: Option<String>,
    ) -> anyhow::Result<PaymentVoucher> {
        let kp = mycena_l1_facade::load_or_create_keypair(identity_path)?;
        if address_from_keypair(&kp) != self.local_address {
            anyhow::bail!("identity does not match coin address");
        }
        let reg = self
            .voucher_registry
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("voucher settlement not enabled on this node"))?;
        self.ledger
            .reserve_voucher_funds(&self.local_address, max_amount_muon)?;
        let voucher = match PaymentVoucher::new_signed(&kp, max_amount_muon, ttl_ms, memo) {
            Ok(v) => v,
            Err(e) => {
                let _ = self
                    .ledger
                    .release_voucher_reservation(&self.local_address, max_amount_muon);
                return Err(e);
            }
        };
        if let Err(e) = reg.register_voucher(&voucher) {
            let _ = self
                .ledger
                .release_voucher_reservation(&self.local_address, max_amount_muon);
            return Err(e);
        }
        self.broadcast_coin_payload(CoinPayload::PaymentVoucher(voucher.clone()))
            .await?;
        Ok(voucher)
    }

    pub async fn issue_voucher(
        &self,
        max_amount_muon: u64,
        memo: Option<String>,
    ) -> anyhow::Result<PaymentVoucher> {
        self.issue_voucher_from_identity_path(
            &self.coin_identity_path,
            max_amount_muon,
            VOUCHER_DEFAULT_TTL_MS,
            memo,
        )
        .await
    }

    /// Accept a voucher redemption locally (unsettled credit; does not prevent partition double-spend).
    pub async fn redeem_voucher(
        &self,
        voucher: &PaymentVoucher,
        amount_muon: u64,
    ) -> anyhow::Result<()> {
        let redemption = VoucherRedemption::new(
            voucher,
            self.local_address.clone(),
            amount_muon,
            self.local_peer_id.clone(),
            None,
        );
        self.handle_voucher_redemption(&redemption, Some(voucher))
            .await?;
        let inner = bincode::serialize(&CoinPayload::VoucherRedemption(redemption))?;
        self.transport.broadcast_coin_voucher_inner(inner).await
    }

    /// Merge redemption gossip, settle when possible, broadcast sync frame.
    pub async fn settle_voucher_and_broadcast(
        &self,
        voucher_id: &str,
    ) -> anyhow::Result<VoucherSettlementOutcome> {
        let reg = self
            .voucher_registry
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("voucher settlement not enabled"))?;
        let voucher = reg
            .get_voucher(voucher_id)?
            .ok_or_else(|| anyhow::anyhow!("unknown voucher {voucher_id}"))?;
        let now = mycena_l0_facade::data::now_ms();
        let outcome = reg.settle_voucher(voucher_id, now)?;
        let merged = reg.redemptions_for(voucher_id)?;
        VoucherSettlementRegistry::apply_settlement_to_ledger(
            &self.ledger,
            &voucher,
            &outcome,
            &merged,
        )?;
        let sync = CoinPayload::VoucherSettlementSync {
            voucher_id: voucher_id.to_string(),
            redemptions: merged,
        };
        self.broadcast_coin_payload(sync).await?;
        Ok(outcome)
    }

    /// Recipient-facing unsettled exposure + risk notices for UI.
    pub fn voucher_exposure(&self) -> anyhow::Result<VoucherExposureView> {
        let acc = self.ledger.account_state(&self.local_address)?;
        let notices = if let Some(reg) = &self.voucher_registry {
            reg.risk_notices_for_recipient(&self.local_address)?
        } else {
            Vec::new()
        };
        Ok(VoucherExposureView::from_account_and_notices(&acc, notices))
    }

    async fn merge_voucher_redemptions(
        &self,
        voucher_id: &str,
        redemptions: &[VoucherRedemption],
    ) -> anyhow::Result<()> {
        let Some(reg) = &self.voucher_registry else {
            return Ok(());
        };
        for r in redemptions {
            let _ = reg.ingest_redemption_gossip(r);
        }
        let now = mycena_l0_facade::data::now_ms();
        let outcome = reg.settle_voucher(voucher_id, now)?;
        let merged = reg.redemptions_for(voucher_id)?;
        let voucher = reg
            .get_voucher(voucher_id)?
            .ok_or_else(|| anyhow::anyhow!("unknown voucher {voucher_id}"))?;
        VoucherSettlementRegistry::apply_settlement_to_ledger(
            &self.ledger,
            &voucher,
            &outcome,
            &merged,
        )?;
        Ok(())
    }

    async fn handle_voucher_redemption(
        &self,
        redemption: &VoucherRedemption,
        voucher: Option<&PaymentVoucher>,
    ) -> anyhow::Result<()> {
        let Some(reg) = &self.voucher_registry else {
            return Ok(());
        };
        let now = mycena_l0_facade::data::now_ms();
        let voucher = match voucher {
            Some(v) => v.clone(),
            None => reg
                .get_voucher(&redemption.voucher_id)?
                .ok_or_else(|| anyhow::anyhow!("unknown voucher {}", redemption.voucher_id))?,
        };
        let result = reg.accept_redemption_offline(&voucher, redemption, now)?;
        if result.accepted && redemption.recipient_address == self.local_address {
            VoucherSettlementRegistry::credit_unsettled_on_ledger(
                &self.ledger,
                &redemption.recipient_address,
                redemption.amount_muon,
            )?;
            info!(
                "voucher {} accepted unsettled for {} muon",
                redemption.voucher_id, redemption.amount_muon
            );
        }
        Ok(())
    }

    pub fn balance_eventual(
        &self,
    ) -> anyhow::Result<crate::views::AccountBalanceView<EventuallyConsistentModel>> {
        self.ledger.eventual_balance_view(&self.local_address)
    }

    pub fn balance_settled(
        &self,
    ) -> anyhow::Result<crate::views::AccountBalanceView<SettlementRequiredModel>> {
        self.ledger.settled_balance_view(&self.local_address)
    }

    pub fn balance(&self) -> anyhow::Result<(u64, u64)> {
        let view = self.balance_eventual()?;
        Ok((view.confirmed_muon(), view.pending_muon()))
    }

    pub fn recent_transactions(&self, limit: usize) -> anyhow::Result<Vec<Transaction>> {
        self.ledger.recent_transactions(limit)
    }

    /// Testnet helper: credit this node once if it has no funds.
    pub fn request_faucet_coins(&self) -> anyhow::Result<()> {
        let acc = self.ledger.account_state(&self.local_address)?;
        if acc.balance_muon == 0 && acc.pending_received == 0 {
            self.ledger
                .genesis_credit(&self.local_address, 10 * crate::ledger::MUON_PER_MXC)?;
        }
        Ok(())
    }

    /// Burn settled credits as anti-spam caution for message priority (not transferable).
    pub fn burn_priority_deposit(&self, amount_muon: u64) -> Result<(), crate::CoinError> {
        crate::spam_caution::deposit_for_message_priority(
            &self.ledger,
            &self.local_address,
            amount_muon,
        )
    }
}
