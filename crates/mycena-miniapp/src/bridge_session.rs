// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Per-app bridge session tokens (Cell C2).

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use mycena_l0_facade::data::now_ms;
use mycena_l0_facade::lock_or_recover;
use uuid::Uuid;

const SESSION_TTL_MS: u64 = 3_600_000;

#[derive(Debug, Clone)]
struct Session {
    token: String,
    issued_at_ms: u64,
}

static SESSIONS: LazyLock<Mutex<HashMap<String, Session>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Issue a new session token for an app instance (invalidates prior token for that app).
pub fn issue_session(app_id: &str) -> String {
    let token = Uuid::new_v4().to_string();
    let now = now_ms();
    // POISON-RECOVER: session table insert; worst case re-issue on next app launch.
    lock_or_recover!(SESSIONS, "bridge session").insert(
        app_id.to_string(),
        Session {
            token: token.clone(),
            issued_at_ms: now,
        },
    );
    token
}

pub fn revoke_session(app_id: &str) {
    // POISON-RECOVER: remove is idempotent; missing entry is already revoked.
    lock_or_recover!(SESSIONS, "bridge session").remove(app_id);
}

pub fn validate_session(app_id: &str, token: &str) -> Result<(), String> {
    let now = now_ms();
    // POISON-RECOVER: read-only validation; stale deny only blocks one bridge call.
    let guard = lock_or_recover!(SESSIONS, "bridge session");
    let Some(sess) = guard.get(app_id) else {
        return Err("bridge session expired or not issued".into());
    };
    if sess.token != token {
        return Err("invalid bridge session token".into());
    }
    if now.saturating_sub(sess.issued_at_ms) > SESSION_TTL_MS {
        return Err("bridge session expired".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn session_issue_and_validate() {
        let tok = issue_session("com.test.app");
        assert!(validate_session("com.test.app", &tok).is_ok());
        assert!(validate_session("com.test.app", "wrong").is_err());
        revoke_session("com.test.app");
        assert!(validate_session("com.test.app", &tok).is_err());
    }

    #[test]
    fn bridge_sessions_survive_mutex_poison() {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut guard = lock_or_recover!(SESSIONS, "bridge session");
            guard.insert(
                "poison-seed".into(),
                Session {
                    token: "x".into(),
                    issued_at_ms: 0,
                },
            );
            panic!("deliberate bridge session poison");
        }));
        let tok = issue_session("com.test.after-poison");
        assert!(validate_session("com.test.after-poison", &tok).is_ok());
    }
}
