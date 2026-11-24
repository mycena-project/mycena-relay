// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Mycena GATT layout — matches Android [`BleMeshGatt`] (ADR 002 F1/F2).

use uuid::Uuid;

/// Primary service — also advertised for passive scan filter.
pub const SERVICE_UUID: Uuid = Uuid::from_u128(0x6e400001_b5a3_f393_e0a9_e50e24dcca9e);

/// Read-only signed invite v3 URI (`mycena://invite/v3#…`).
pub const INVITE_CHAR_UUID: Uuid = Uuid::from_u128(0x6e400002_b5a3_f393_e0a9_e50e24dcca9e);

/// Write-only mesh frames (chunked MYBL wire payloads, F2).
pub const MESH_DATA_CHAR_UUID: Uuid = Uuid::from_u128(0x6e400003_b5a3_f393_e0a9_e50e24dcca9e);

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{INVITE_CHAR_UUID, MESH_DATA_CHAR_UUID, SERVICE_UUID};

    #[test]
    fn uuids_match_android_ble_mesh_gatt() {
        assert_eq!(
            SERVICE_UUID.to_string(),
            "6e400001-b5a3-f393-e0a9-e50e24dcca9e"
        );
        assert_eq!(
            INVITE_CHAR_UUID.to_string(),
            "6e400002-b5a3-f393-e0a9-e50e24dcca9e"
        );
        assert_eq!(
            MESH_DATA_CHAR_UUID.to_string(),
            "6e400003-b5a3-f393-e0a9-e50e24dcca9e"
        );
    }
}
