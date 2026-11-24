// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Anti-spam **caution deposit**: spend utility credits to raise message priority (not speculation).

use crate::ledger::{LocalLedger, MUON_PER_MXC};
use crate::CoinError;

/// Default deposit for high-priority delivery (~0.02 MXC). Burned as spam caution.
pub const DEFAULT_PRIORITY_DEPOSIT_MUON: u64 = 20_000;

/// Burn settled credits to purchase high message priority.
pub fn deposit_for_message_priority(
    ledger: &LocalLedger,
    from_address: &str,
    amount_muon: u64,
) -> Result<(), CoinError> {
    if amount_muon == 0 {
        return Ok(());
    }
    ledger
        .burn_settled_credits(from_address, amount_muon)
        .map_err(|e| {
            CoinError::Internal(anyhow::anyhow!(
                "insufficient settled credits for priority caution: have {} need {}",
                e.have_settled,
                e.need
            ))
        })
}

/// Suggested deposit tiers (utility framing in UI).
pub fn priority_deposit_tiers() -> &'static [(u64, &'static str)] {
    &[
        (0, "Normal (free)"),
        (DEFAULT_PRIORITY_DEPOSIT_MUON, "High (~0.02 credits)"),
        (MUON_PER_MXC / 10, "Urgent (~0.1 credits)"),
    ]
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::address_from_keypair;
    use libp2p::identity::Keypair;
    use tempfile::tempdir;

    #[test]
    fn priority_deposit_burns_credits() {
        let dir = tempdir().unwrap();
        let ledger = LocalLedger::open(dir.path().to_str().unwrap()).unwrap();
        let kp = Keypair::generate_ed25519();
        let addr = address_from_keypair(&kp);
        ledger.genesis_credit(&addr, MUON_PER_MXC).unwrap();
        deposit_for_message_priority(&ledger, &addr, DEFAULT_PRIORITY_DEPOSIT_MUON).unwrap();
        let acc = ledger.account_state(&addr).unwrap();
        assert_eq!(
            acc.balance_muon,
            MUON_PER_MXC - DEFAULT_PRIORITY_DEPOSIT_MUON
        );
    }
}
