// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Maps libp2p peer id ↔ BLE device id after F1 handoff (Android `BleMeshLinkRegistry` parity).

use mycena_l0_facade::lock_or_recover;
use std::collections::HashMap;
use std::sync::Mutex;

pub struct LinkRegistry {
    peer_to_device: Mutex<HashMap<String, String>>,
    device_to_peer: Mutex<HashMap<String, String>>,
}

impl Default for LinkRegistry {
    fn default() -> Self {
        Self {
            peer_to_device: Mutex::new(HashMap::new()),
            device_to_peer: Mutex::new(HashMap::new()),
        }
    }
}

impl LinkRegistry {
    pub fn register(&self, peer_id: &str, device_id: &str) {
        let device_id = device_id.to_string();
        let peer_id = peer_id.to_string();
        // POISON-RECOVER: bidirectional map; insert replaces prior mapping atomically per hash map.
        lock_or_recover!(self.peer_to_device, "link registry peer_to_device")
            .insert(peer_id.clone(), device_id.clone());
        // POISON-RECOVER: reverse map insert; atomic per HashMap, paired with peer_to_device above.
        lock_or_recover!(self.device_to_peer, "link registry device_to_peer")
            .insert(device_id, peer_id);
    }

    pub fn peer_for_device(&self, device_id: &str) -> Option<String> {
        // POISON-RECOVER: read-only lookup; stale miss only drops one BLE route attempt.
        lock_or_recover!(self.device_to_peer, "link registry device_to_peer")
            .get(device_id)
            .cloned()
    }

    pub fn device_for_peer(&self, peer_id: &str) -> Option<String> {
        // POISON-RECOVER: read-only lookup; stale miss only drops one BLE route attempt.
        lock_or_recover!(self.peer_to_device, "link registry peer_to_device")
            .get(peer_id)
            .cloned()
    }

    #[allow(dead_code)] // Used when desktop BLE drain enumerates linked peers.
    pub fn all_peers(&self) -> Vec<String> {
        // POISON-RECOVER: enumerates keys; incomplete list is safe for drain retry.
        lock_or_recover!(self.peer_to_device, "link registry peer_to_device")
            .keys()
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn link_registry_survives_mutex_poison() {
        let reg = LinkRegistry::default();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            lock_or_recover!(reg.peer_to_device, "link registry peer_to_device")
                .insert("poison-peer".into(), "deadbeef".into());
            panic!("deliberate link registry poison");
        }));
        reg.register("12D3KooWAfterPoisonTestPeerId001", "aabbccddeeff");
        assert_eq!(
            reg.device_for_peer("12D3KooWAfterPoisonTestPeerId001")
                .as_deref(),
            Some("aabbccddeeff")
        );
    }
}
